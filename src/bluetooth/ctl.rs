//! `/dev/bluetooth`: write a command line, then read its output until end
//! of file. A command runs in a kernel thread; lines starting with "? "
//! are questions (a passkey, yes/no), answered by writing a line to the
//! same file while the command runs. Used by the `bt` tool.
//!
//! Commands: `status`, `power on|off`, `scan [SECONDS]`, `devices`,
//! `pair ADDR`, `connect ADDR`, `disconnect ADDR`, `remove ADDR`, `list`,
//! `attach PORT`.

use super::{Hci, keys};
use crate::errno::*;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use crate::vfs::{FileLike, POLLIN, POLLOUT};
use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use bt::hci::{self, Event, op};
use bt::{Addr, AddrType};
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

/// One running command: its output and the user's answers.
pub struct Session {
    out: Mutex<VecDeque<u8>>,
    input: Mutex<VecDeque<String>>,
    done: AtomicBool,
    wq: WaitQueue,
}

impl Session {
    fn new() -> Arc<Session> {
        Arc::new(Session {
            out: Mutex::new(VecDeque::new()),
            input: Mutex::new(VecDeque::new()),
            done: AtomicBool::new(false),
            wq: WaitQueue::new(),
        })
    }

    pub fn say(&self, line: &str) {
        let mut o = self.out.lock();
        o.extend(line.bytes());
        o.push_back(b'\n');
        drop(o);
        self.wq.wake_all();
        crate::vfs::notify_poll();
    }

    /// Ask the user; None after two minutes without an answer.
    pub fn ask(&self, q: &str) -> Option<String> {
        self.input.lock().clear();
        self.say(&alloc::format!("? {}", q));
        let deadline = crate::time::Deadline::after_ms(120_000);
        loop {
            if let Some(l) = self.input.lock().pop_front() {
                return Some(l);
            }
            if deadline.expired() {
                return None;
            }
            self.wq.wait_timeout(200, || !self.input.lock().is_empty());
        }
    }

    fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
        self.wq.wake_all();
        crate::vfs::notify_poll();
    }
}

struct Node;

struct BtFile {
    session: Mutex<Option<Arc<Session>>>,
}

static REGISTERED: AtomicBool = AtomicBool::new(false);

/// Create /dev/bluetooth (once).
pub fn register_node() {
    if !REGISTERED.swap(true, Ordering::SeqCst) {
        crate::vfs::devfs::register(
            "bluetooth",
            crate::vfs::FileType::CharDevice,
            (10 << 8) | 223,
            Arc::new(Node),
        );
    }
}

impl FileLike for Node {
    fn read(&self, _b: &mut [u8], _nb: bool) -> KResult<usize> {
        Ok(0)
    }
    fn write(&self, _b: &[u8], _nb: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn open_instance(&self, _flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        Ok(Some(Arc::new(BtFile {
            session: Mutex::new(None),
        })))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

impl FileLike for BtFile {
    fn write(&self, buf: &[u8], _nb: bool) -> KResult<usize> {
        let line = String::from_utf8_lossy(buf).trim().to_string();
        let mut cur = self.session.lock();
        if let Some(s) = cur.as_ref()
            && !s.done.load(Ordering::SeqCst)
        {
            // An answer to a question.
            s.input.lock().push_back(line);
            s.wq.wake_all();
            return Ok(buf.len());
        }
        let s = Session::new();
        *cur = Some(s.clone());
        crate::sched::spawn("bt-cmd", move || {
            if let Err(e) = run(&line, &s) {
                s.say(&alloc::format!("error: {}", e.message()));
            }
            s.finish();
        });
        Ok(buf.len())
    }

    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        let Some(s) = self.session.lock().clone() else {
            return Ok(0);
        };
        loop {
            {
                let mut o = s.out.lock();
                if !o.is_empty() {
                    let n = buf.len().min(o.len());
                    for (d, b) in buf.iter_mut().zip(o.drain(..n)) {
                        *d = b;
                    }
                    return Ok(n);
                }
            }
            if s.done.load(Ordering::SeqCst) {
                return Ok(0);
            }
            if nonblock {
                return Err(EAGAIN);
            }
            s.wq.wait_timeout(200, || {
                !s.out.lock().is_empty() || s.done.load(Ordering::SeqCst)
            });
            if crate::process::signal::has_pending() {
                return Err(EINTR);
            }
        }
    }

    fn poll(&self) -> u16 {
        match self.session.lock().as_ref() {
            Some(s) if s.out.lock().is_empty() && !s.done.load(Ordering::SeqCst) => POLLOUT,
            _ => POLLIN | POLLOUT,
        }
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

fn controller() -> KResult<Arc<Hci>> {
    let h = super::first().ok_or(ENODEV)?;
    if !h.info.lock().up {
        return Err(ENETDOWN);
    }
    Ok(h)
}

fn addr_arg(a: Option<&str>) -> KResult<Addr> {
    a.and_then(Addr::parse).ok_or(EINVAL)
}

fn run(line: &str, s: &Arc<Session>) -> KResult<()> {
    let mut w = line.split_whitespace();
    let cmd = w.next().unwrap_or("status");
    match cmd {
        "status" | "show" => status(s),
        "attach" => {
            let port = w.next().ok_or(EINVAL)?;
            super::h4::attach(port)?;
            s.say(&alloc::format!("attached {}", port));
            Ok(())
        }
        "power" => {
            let h = super::first().ok_or(ENODEV)?;
            match w.next() {
                Some("on") => {
                    if !h.info.lock().up {
                        h.power_on()?;
                    }
                    s.say("powered on");
                }
                Some("off") => {
                    h.power_off();
                    s.say("powered off");
                }
                _ => return Err(EINVAL),
            }
            Ok(())
        }
        "scan" => {
            let secs: u64 = w
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(5)
                .clamp(1, 60);
            scan(&controller()?, secs, s)
        }
        "devices" => {
            devices(&*controller()?, 0, s);
            Ok(())
        }
        "pair" | "connect" => {
            let h = controller()?;
            let a = addr_arg(w.next())?;
            let bond = keys::get(&a);
            if cmd == "connect" && bond.is_none() {
                s.say(&alloc::format!("{} is not paired: use bt pair", a));
                return Err(ENOENT);
            }
            let (le, t) = match h.found.lock().get(&a) {
                Some(f) => (f.le, f.addr_type),
                None => match &bond {
                    Some(b) => (b.le, b.addr_type),
                    None => {
                        s.say(&alloc::format!("{} not found: run bt scan first", a));
                        return Err(ENOENT);
                    }
                },
            };
            *h.session.lock() = Some(s.clone());
            h.connecting.lock().push(a);
            let r = if le {
                super::le::pair(&h, a, t, s, cmd == "pair")
            } else {
                super::bredr::pair(&h, a, s, cmd == "pair")
            };
            h.connecting.lock().retain(|x| *x != a);
            *h.session.lock() = None;
            h.update_background();
            r
        }
        "disconnect" => {
            let h = controller()?;
            let a = addr_arg(w.next())?;
            disconnect(&h, &a)?;
            s.say(&alloc::format!("disconnected {}", a));
            Ok(())
        }
        "remove" => {
            let h = controller()?;
            let a = addr_arg(w.next())?;
            let _ = disconnect(&h, &a);
            if !keys::remove(&a) {
                return Err(ENOENT);
            }
            h.update_background();
            s.say(&alloc::format!("removed {}", a));
            Ok(())
        }
        "list" => {
            let h = super::first();
            let bonds = keys::all();
            if bonds.is_empty() {
                s.say("no paired devices");
            }
            for b in bonds {
                let connected = h.as_ref().is_some_and(|h| h.conn_to(&b.addr).is_some());
                s.say(&alloc::format!(
                    "{}  {:<5} {:<9} {}",
                    b.addr,
                    if b.le { "le" } else { "bredr" },
                    if connected { "connected" } else { "paired" },
                    b.name
                ));
            }
            Ok(())
        }
        _ => {
            s.say("commands: status, power on|off, scan [SECONDS], devices, pair ADDR, connect ADDR, disconnect ADDR, remove ADDR, list, attach PORT");
            Err(EINVAL)
        }
    }
}

fn status(s: &Session) -> KResult<()> {
    let hs = super::controllers();
    if hs.is_empty() {
        s.say("no Bluetooth controller");
        return Ok(());
    }
    for h in hs {
        let i = h.info.lock().clone();
        s.say(&alloc::format!(
            "{}: {} via {}, Bluetooth {}, manufacturer {:#06x}, {}{}{}",
            h.name(),
            i.addr,
            h.tr.name(),
            hci::version_name(i.version),
            i.manufacturer,
            if i.up { "up" } else { "down" },
            if i.le { ", LE" } else { "" },
            if i.bredr { ", BR/EDR" } else { "" },
        ));
        for c in h.conns.lock().values() {
            s.say(&alloc::format!(
                "  connected {} ({}{})",
                c.peer,
                if c.le { "LE" } else { "BR/EDR" },
                if c.encrypted.load(Ordering::Relaxed) {
                    ", encrypted"
                } else {
                    ""
                }
            ));
        }
    }
    Ok(())
}

fn scan(h: &Arc<Hci>, secs: u64, s: &Session) -> KResult<()> {
    let start = crate::time::millis();
    let (le, bredr) = {
        let i = h.info.lock();
        (i.le, i.bredr)
    };
    h.set_scan(true, true);
    if bredr {
        let _ = h.command(hci::inquiry(secs as u32));
    }
    s.say(&alloc::format!("scanning for {} s...", secs));
    crate::time::sleep_ms(secs * 1000);
    if le {
        h.set_scan(false, false);
    }
    if bredr {
        let _ = h.command(hci::cmd(op::INQUIRY_CANCEL, &[]));
    }
    h.background.store(false, Ordering::SeqCst);
    h.update_background();
    devices(h, start, s);
    Ok(())
}

fn devices(h: &Hci, since: u64, s: &Session) {
    let f = h.found.lock();
    let mut n = 0;
    for (a, d) in f.iter().filter(|(_, d)| d.seen >= since) {
        let mut line = alloc::format!(
            "{}  {:<5} {:>4} dBm  {:<9}",
            a,
            if d.le { "le" } else { "bredr" },
            d.rssi,
            d.kind()
        );
        if let Some(n) = &d.info.name {
            let _ = write!(line, " {}", n);
        }
        if d.le && d.addr_type == AddrType::Random {
            line.push_str(" (random)");
        }
        s.say(&line);
        n += 1;
    }
    if n == 0 {
        s.say("no devices found");
    }
}

fn disconnect(h: &Hci, a: &Addr) -> KResult<()> {
    let c = h.conn_to(a).ok_or(ENOTCONN)?;
    let since = h.ev_seq();
    h.command(hci::disconnect(c.handle, 0x13))?;
    h.wait_event(since, 5000, |e| match e {
        Event::DisconnectionComplete { handle, .. } if *handle == c.handle => Some(()),
        _ => None,
    })
    .map(|_| ())
    .ok_or(ETIMEDOUT)
}
