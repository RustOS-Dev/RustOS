//! LE links: connecting, pairing (SMP over the link), re-encrypting with
//! a stored key, and HID over GATT keyboards and mice.

use super::ctl::Session;
use super::keys::{self, BondEntry};
use super::{Conn, Hci};
use crate::drivers::input;
use crate::errno::*;
use crate::usb::hid::HidSink;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bt::gatt::{self, Bearer, Client};
use bt::hci::{self, Event, op};
use bt::smp::{self, Failure, Local, Pairing, SmpIo};
use bt::{Addr, AddrType, att, l2cap};
use core::sync::atomic::Ordering;
use usb_desc::hid::ReportDescriptor;

/// Connect to an LE device (or return the existing link).
pub fn connect(h: &Arc<Hci>, a: Addr, t: AddrType) -> KResult<Arc<Conn>> {
    if let Some(c) = h.conn_to(&a) {
        return Ok(c);
    }
    // Some controllers refuse to connect while scanning.
    h.set_scan(false, false);
    h.background.store(false, Ordering::SeqCst);
    let since = h.ev_seq();
    h.command(hci::le_create_conn(&a, t))?;
    let r = h.wait_event(since, 10_000, |e| match e {
        Event::LeConnectionComplete {
            status,
            peer,
            handle,
            ..
        } if *peer == a => Some((*status, *handle)),
        _ => None,
    });
    match r {
        Some((0, handle)) => h.conn(handle).ok_or(EIO),
        Some((st, _)) => Err(super::hci_err(st)),
        None => {
            let _ = h.command(hci::cmd(op::LE_CREATE_CONN_CANCEL, &[]));
            Err(ETIMEDOUT)
        }
    }
}

/// Encrypt the link; true once encryption is on.
pub fn encrypt(h: &Hci, c: &Conn, key: &[u8; 16], ediv: u16, rand: &[u8; 8]) -> bool {
    let since = h.ev_seq();
    if h.command(hci::le_start_encryption(c.handle, rand, ediv, key))
        .is_err()
    {
        return false;
    }
    h.wait_event(since, 5000, |e| match e {
        Event::EncryptionChange { status, handle, on } if *handle == c.handle => {
            Some(*status == 0 && *on)
        }
        Event::DisconnectionComplete { handle, .. } if *handle == c.handle => Some(false),
        _ => None,
    })
    .unwrap_or(false)
}

fn disconnect(h: &Hci, c: &Conn) {
    let _ = h.command(hci::disconnect(c.handle, 0x13));
}

struct SmpLink<'a> {
    h: &'a Arc<Hci>,
    c: &'a Arc<Conn>,
    s: Option<&'a Session>,
}

impl SmpIo for SmpLink<'_> {
    fn send(&mut self, pdu: &[u8]) {
        self.h.send_frame(self.c, l2cap::CID_SMP, pdu);
    }
    fn recv(&mut self, ms: u64) -> Option<Vec<u8>> {
        self.c.wait_for(ms, |c| c.smp.lock().pop_front())
    }
    fn random(&mut self, out: &mut [u8]) {
        crate::drivers::random::fill(out);
    }
    fn show_passkey(&mut self, p: u32) {
        let line = alloc::format!("Type {:06} on the device, then Enter", p);
        crate::println!("[bt] {}: {}", self.c.peer, line);
        if let Some(s) = self.s {
            s.say(&line);
        }
    }
    fn enter_passkey(&mut self) -> Option<u32> {
        self.s?
            .ask("Passkey shown on the device:")?
            .trim()
            .parse()
            .ok()
    }
    fn confirm(&mut self, v: u32) -> bool {
        let Some(s) = self.s else { return false };
        s.ask(&alloc::format!("Does the device show {:06}? [yes/no]", v))
            .is_some_and(|a| a.trim().eq_ignore_ascii_case("yes") || a.trim() == "y")
    }
    fn encrypt(&mut self, key: &[u8; 16], ediv: u16, rand: &[u8; 8]) -> bool {
        encrypt(self.h, self.c, key, ediv, rand)
    }
}

/// ATT requests over the link (one outstanding at a time).
struct AttLink<'a> {
    h: &'a Hci,
    c: &'a Conn,
}

impl Bearer for AttLink<'_> {
    fn request(&mut self, pdu: &[u8]) -> Result<Vec<u8>, gatt::Error> {
        self.c.att_rsp.lock().clear();
        self.h.send_frame(self.c, l2cap::CID_ATT, pdu);
        self.c
            .wait_for(30_000, |c| c.att_rsp.lock().pop_front())
            .ok_or(gatt::Error::Timeout)
    }
    fn send(&mut self, pdu: &[u8]) -> Result<(), gatt::Error> {
        self.h.send_frame(self.c, l2cap::CID_ATT, pdu);
        Ok(())
    }
}

/// Pair with (or, `pair == false`, reconnect to) an LE device and set up
/// its HID service.
pub fn pair(h: &Arc<Hci>, a: Addr, t: AddrType, s: &Session, pair: bool) -> KResult<()> {
    s.say(&alloc::format!("connecting to {}...", a));
    let c = connect(h, a, t)?;
    let bond = keys::get(&a);
    let secured = match (&bond, pair) {
        (Some(b), false) if b.ltk.is_some() => encrypt(h, &c, &b.ltk.unwrap(), b.ediv, &b.rand),
        _ => false,
    };
    let mut entry = bond.clone().unwrap_or_default();
    if !secured {
        s.say("pairing...");
        let local = Local {
            addr: h.info.lock().addr,
            addr_type: AddrType::Public,
            io_cap: smp::IO_KEYBOARD_DISPLAY,
            irk: keys::local_irk(),
        };
        let mut link = SmpLink {
            h,
            c: &c,
            s: Some(s),
        };
        let b = match Pairing::new(&mut link, &local, a, t).run() {
            Ok(b) => b,
            Err(f) => {
                disconnect(h, &c);
                s.say(&alloc::format!("pairing failed: {}", failure(f)));
                return Err(EACCES);
            }
        };
        entry = BondEntry {
            addr: a,
            addr_type: t,
            le: true,
            name: entry.name,
            ltk: Some(b.ltk),
            ediv: b.ediv,
            rand: b.rand,
            key_size: b.key_size,
            authenticated: b.authenticated,
            secure: b.secure,
            irk: b.irk,
            ..Default::default()
        };
        s.say(&alloc::format!(
            "paired ({}{})",
            if b.secure {
                "LE Secure Connections"
            } else {
                "legacy"
            },
            if b.authenticated {
                ", authenticated"
            } else {
                ", unauthenticated"
            }
        ));
    }
    if entry.name.is_empty()
        && let Some(n) = h.found.lock().get(&a).and_then(|f| f.info.name.clone())
    {
        entry.name = n;
    }
    match setup_hid(h, &c, &entry.name) {
        Ok((name, node)) => {
            if !name.is_empty() {
                entry.name = name;
            }
            keys::put(entry);
            s.say(&alloc::format!("connected: {} ({})", node, a));
            Ok(())
        }
        Err(e) => {
            // Keep the bond: the device may not be a HID device.
            keys::put(entry);
            s.say(&alloc::format!("connected, but no HID service ({:?})", e));
            Ok(())
        }
    }
}

fn failure(f: Failure) -> String {
    match f {
        Failure::Local(r) => alloc::format!("we refused (reason {:#04x})", r),
        Failure::Remote(r) => alloc::format!("the device refused (reason {:#04x})", r),
        Failure::Timeout => String::from("no answer from the device"),
        Failure::Encryption => String::from("the link would not encrypt"),
    }
}

/// A bonded device advertised: connect, encrypt and restart its HID.
pub fn reconnect(h: &Arc<Hci>, a: Addr, t: AddrType) {
    let Some(b) = keys::resolve(&a) else { return };
    let c = match connect(h, a, t) {
        Ok(c) => c,
        Err(_) => {
            h.update_background();
            return;
        }
    };
    let ok = b.ltk.is_some_and(|k| encrypt(h, &c, &k, b.ediv, &b.rand));
    if !ok {
        crate::println!("[bt] {}: {}: stored key refused; pair again", h.name(), a);
        disconnect(h, &c);
    } else if let Err(e) = setup_hid(h, &c, &b.name) {
        crate::println!("[bt] {}: {}: HID set-up failed: {:?}", h.name(), a, e);
    }
    h.update_background();
}

/// Discover HID over GATT, register an input device and start the thread
/// that feeds it. Returns (device name, /dev/input node).
fn setup_hid(h: &Arc<Hci>, c: &Arc<Conn>, name: &str) -> Result<(String, String), gatt::Error> {
    let mut link = AttLink { h, c };
    let mut cl = Client::new(&mut link);
    let _ = cl.exchange_mtu(185);
    let dev = bt::hid::discover(&mut cl)?;
    let rd = ReportDescriptor::parse(&dev.report_map).map_err(|_| gatt::Error::Protocol)?;
    let name = dev
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| String::from(name));
    let (vendor, product, version) = dev.pnp.map_or((0, 0, 0), |p| (p.1, p.2, p.3));
    let info = input::Info::new(
        if name.is_empty() {
            "Bluetooth HID"
        } else {
            &name
        },
        &alloc::format!("{}/{}", h.name(), c.peer),
        [input::BUS_BLUETOOTH, vendor, product, version],
    );
    let info = crate::usb::hid::report_info(info, &rd);
    let what = crate::usb::hid::describe(&rd);
    let ev = input::register(info);
    let node = alloc::format!("/dev/input/event{}", ev.idx);
    crate::println!("[bt] {}: {}: HID {} ({})", h.name(), c.peer, what, node);
    let mut sink = HidSink::new(rd, ev.clone());
    if sink.is_keyboard()
        && let Some((handle, id)) = dev.led_output
    {
        let (hw, cw) = (Arc::downgrade(h), Arc::downgrade(c));
        ev.set_led_sink(alloc::boxed::Box::new(move |bits| {
            if let (Some(h), Some(c)) = (hw.upgrade(), cw.upgrade()) {
                let _ = id; // the report ID is implied by the characteristic
                h.send_frame(
                    &c,
                    l2cap::CID_ATT,
                    &att::write(att::WRITE_CMD, handle, &[bits]),
                );
            }
        }));
    }
    let (h2, c2) = (h.clone(), c.clone());
    crate::sched::spawn("bt-hid", move || {
        while !c2.is_gone() {
            let batch: Vec<(u16, Vec<u8>)> = c2.att_ntf.lock().drain(..).collect();
            for (handle, v) in batch {
                if let Some(r) = dev.report(handle, &v) {
                    sink.feed(&r);
                }
            }
            sink.tick();
            c2.wq
                .wait_timeout(30, || c2.is_gone() || !c2.att_ntf.lock().is_empty());
        }
        sink.finish();
        input::unregister(&sink.ev);
        h2.update_background();
    });
    Ok((name, node))
}
