//! Terminals: virtual consoles and pseudo-terminals.
//!
//! Every terminal is a [`Tty`]: a POSIX line discipline (canonical
//! editing, echo, job-control signals for the foreground process group)
//! whose output goes to a sink. There are four virtual consoles on the
//! framebuffer (`/dev/tty1`..`tty4`, switched with Alt-F1..F4 or `chvt`);
//! the first is also COM1 (`/dev/console`, `/dev/ttyS0`). Keyboard input
//! goes to the visible console, serial input to the first. Pseudo-terminal
//! slaves (`/dev/pts/N`) send their output to the master side (see
//! [`pty`]). Serial ports of drivers (USB serial adapters through LinuxKPI,
//! `/dev/ttyUSB*`, `/dev/ttyACM*`) are terminals whose output goes to a
//! [`TtyDriver`].

use crate::errno::*;
use crate::process::{self, signal, uaccess};
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use crate::vfs::{self, File, FileLike, FileType, Metadata};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Once;

pub mod pty;

// termios flags
const ICRNL: u32 = 0o400;
const INLCR: u32 = 0o100;
const IGNCR: u32 = 0o200;
const OPOST: u32 = 0o1;
const ONLCR: u32 = 0o4;
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ECHOE: u32 = 0o20;
const ECHOK: u32 = 0o40;
const ECHOCTL: u32 = 0o1000;
const IEXTEN: u32 = 0o100000;

const VINTR: usize = 0;
const VQUIT: usize = 1;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VTIME: usize = 5;
const VMIN: usize = 6;
const VSUSP: usize = 10;
const VWERASE: usize = 14;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 19],
}

impl Termios {
    fn default_cooked() -> Termios {
        let mut cc = [0u8; 19];
        cc[VINTR] = 0x03;
        cc[VQUIT] = 0x1C;
        cc[VERASE] = 0x7F;
        cc[VKILL] = 0x15;
        cc[VEOF] = 0x04;
        cc[VTIME] = 0;
        cc[VMIN] = 1;
        cc[VSUSP] = 0x1A;
        cc[VWERASE] = 0x17;
        Termios {
            iflag: ICRNL,
            oflag: OPOST | ONLCR,
            cflag: 0o277, // CS8 | CREAD | B38400
            lflag: ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | IEXTEN,
            line: 0,
            cc,
        }
    }
}

const EOF_MARK: u16 = 0x100;

/// A serial port driver behind a terminal.
pub trait TtyDriver: Send + Sync {
    /// Device number for stat (major << 8 | minor).
    fn rdev(&self) -> u64;
    /// First open: start the port (fails if the device is gone).
    fn open(&self) -> KResult<()>;
    /// Last close.
    fn close(&self);
    /// Send bytes; returns how many the driver took (blocking until some).
    fn write(&self, data: &[u8]) -> KResult<usize>;
    /// The line settings changed (speed, character size, flow control).
    fn set_termios(&self, t: &Termios);
}

/// Where a terminal's output goes.
enum Sink {
    /// Virtual console `n` (0-based).
    Console(usize),
    /// The master side of a pseudo-terminal.
    Pty(alloc::sync::Weak<pty::Pty>),
    /// A serial port driver.
    Driver(Arc<dyn TtyDriver>),
}

pub struct Tty {
    me: alloc::sync::Weak<Tty>,
    sink: Sink,
    /// The other side went away (pty master closed): reads return EOF.
    hung_up: core::sync::atomic::AtomicBool,
    termios: Mutex<Termios>,
    /// Line being edited in canonical mode.
    line: Mutex<alloc::vec::Vec<u8>>,
    /// Bytes (or EOF marks) ready for readers.
    ready: Mutex<VecDeque<u16>>,
    wq: WaitQueue,
    fg_pgrp: AtomicU32,
    winsize: Mutex<[u16; 4]>,
    /// Open files (driver terminals start and stop their port with it).
    opens: AtomicU32,
}

pub const NUM_VCS: usize = 4;
static VCS: Once<[Arc<Tty>; NUM_VCS]> = Once::new();
/// Output kept per virtual console, replayed when it becomes visible.
static VC_LOG: [Mutex<VecDeque<u8>>; NUM_VCS] = [const { Mutex::new(VecDeque::new()) }; NUM_VCS];
const VC_LOG_MAX: usize = 32 * 1024;
static ACTIVE_VC: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Per virtual console display state for programs that draw on the
/// framebuffer themselves (KDSETMODE / VT_SETMODE).
struct VcDisplay {
    graphics: bool,
    /// Pixels of a graphics console while another console is shown.
    pixels: Option<alloc::vec::Vec<u8>>,
    /// VT_PROCESS mode: (pid, release signal, acquire signal).
    process: Option<(u32, u32, u32)>,
    /// KDSKBMODE: K_OFF (4) stops keyboard input to the console (a
    /// compositor reads evdev instead).
    kbmode: u32,
}

const K_UNICODE: u32 = 3;
const K_OFF: u32 = 4;

/// A switch waiting for the VT_PROCESS owner of the visible console to
/// allow it (VT_RELDISP); usize::MAX if none.
static PENDING_SWITCH: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(usize::MAX);
/// Woken when the visible console changes (VT_WAITACTIVE).
static VT_WQ: WaitQueue = WaitQueue::new();

/// Whether keyboard input should reach the visible console's TTY.
fn keyboard_to_tty() -> bool {
    VC_DISPLAY[active_index()].lock().kbmode != K_OFF
}

static VC_DISPLAY: [Mutex<VcDisplay>; NUM_VCS] = [const {
    Mutex::new(VcDisplay {
        graphics: false,
        pixels: None,
        process: None,
        kbmode: K_UNICODE,
    })
}; NUM_VCS];

fn vc_signal(pid: u32, sig: u32) {
    if sig != 0
        && let Some(p) = crate::process::find(pid as _)
    {
        signal::send(&p, sig);
    }
}

impl Tty {
    fn new(sink: Sink, rows: u16, cols: u16) -> Arc<Tty> {
        Arc::new_cyclic(|me| Tty {
            me: me.clone(),
            sink,
            hung_up: core::sync::atomic::AtomicBool::new(false),
            termios: Mutex::new(Termios::default_cooked()),
            line: Mutex::new(alloc::vec::Vec::new()),
            ready: Mutex::new(VecDeque::new()),
            wq: WaitQueue::new(),
            fg_pgrp: AtomicU32::new(0),
            winsize: Mutex::new([rows, cols, 0, 0]),
            opens: AtomicU32::new(0),
        })
    }

    /// A terminal on a serial port driver, in raw mode as serial devices
    /// start out (`cfmakeraw` settings with the driver's line settings).
    pub fn new_driver(driver: Arc<dyn TtyDriver>, cflag: u32) -> Arc<Tty> {
        let tty = Tty::new(Sink::Driver(driver), 24, 80);
        {
            let mut t = tty.termios.lock();
            t.iflag = 0;
            t.oflag = 0;
            t.lflag = 0;
            t.cflag = cflag;
        }
        tty
    }

    fn arc(&self) -> Arc<Tty> {
        self.me.upgrade().expect("tty dropped")
    }

    /// The other side went away (pty master closed, USB adapter
    /// unplugged): wake readers (they see EOF) and send SIGHUP to the
    /// foreground group.
    pub fn hang_up(&self) {
        self.hung_up.store(true, Ordering::SeqCst);
        self.signal_fg(signal::SIGHUP);
        self.notify();
    }
}

/// The virtual consoles.
pub fn vcs() -> &'static [Arc<Tty>; NUM_VCS] {
    VCS.call_once(|| {
        let (cols, rows) = crate::drivers::framebuffer::console_size().unwrap_or((80, 25));
        core::array::from_fn(|i| Tty::new(Sink::Console(i), rows as u16, cols as u16))
    })
}

/// The first virtual console (also the serial console).
pub fn console() -> Arc<Tty> {
    vcs()[0].clone()
}

/// The visible virtual console.
pub fn active() -> Arc<Tty> {
    vcs()[ACTIVE_VC.load(Ordering::SeqCst)].clone()
}

pub fn active_index() -> usize {
    ACTIVE_VC.load(Ordering::SeqCst)
}

/// Show virtual console `n` (0-based): clear the screen and replay its
/// recent output.
///
/// If the visible console is in VT_PROCESS mode, its owner gets its
/// release signal and the switch waits until it allows it with
/// VT_RELDISP(1), as in Linux: a compositor first stops drawing and drops
/// DRM master.
pub fn switch_vc(n: usize) {
    if n >= NUM_VCS || n == active_index() {
        return;
    }
    let release = VC_DISPLAY[active_index()].lock().process;
    if let Some((pid, rel, _)) = release
        && rel != 0
        && crate::process::find(pid as _).is_some()
    {
        if PENDING_SWITCH.swap(n, Ordering::SeqCst) == usize::MAX {
            vc_signal(pid, rel);
        }
        return;
    }
    complete_switch(n);
}

/// Show console `n` now.
fn complete_switch(n: usize) {
    PENDING_SWITCH.store(usize::MAX, Ordering::SeqCst);
    let old = ACTIVE_VC.swap(n, Ordering::SeqCst);
    if n == old {
        return;
    }
    VT_WQ.wake_all();
    // Leaving a graphics console: keep its pixels.
    let graphics = VC_DISPLAY[old].lock().graphics;
    if graphics {
        let px = crate::drivers::framebuffer::save_pixels();
        VC_DISPLAY[old].lock().pixels = px;
    }
    let (graphics, acquire, pixels) = {
        let mut d = VC_DISPLAY[n].lock();
        (d.graphics, d.process, d.pixels.take())
    };
    if let Some((pid, _, acq)) = acquire {
        vc_signal(pid, acq);
    }
    if graphics {
        crate::drivers::framebuffer::set_graphics(true);
        if let Some(px) = pixels {
            crate::drivers::framebuffer::restore_pixels(&px);
        }
        return;
    }
    crate::drivers::framebuffer::set_graphics(false);
    replay_vc(n);
}

/// Redraw text console `n` from its output log.
fn replay_vc(n: usize) {
    let log: alloc::vec::Vec<u8> = VC_LOG[n].lock().iter().copied().collect();
    crate::drivers::console::reset_view();
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(c) = crate::drivers::framebuffer::CONSOLE.lock().as_mut() {
            c.write_bytes(b"\x1b[?1049l\x1b[0m\x1b[?25h\x1b[2J\x1b[H");
            c.write_bytes(&log);
        }
    });
}

/// An open file for the console.
pub fn console_file() -> KResult<Arc<File>> {
    Ok(File::from_stream(console(), vfs::O_RDWR, "/dev/console"))
}

pub fn set_foreground(pgid: u32) {
    console().fg_pgrp.store(pgid, Ordering::SeqCst);
}

pub fn foreground() -> u32 {
    console().fg_pgrp.load(Ordering::SeqCst)
}

impl Tty {
    /// Send output bytes to this terminal's sink.
    fn emit(&self, bytes: &[u8]) {
        match &self.sink {
            Sink::Console(n) => {
                let n = *n;
                {
                    let mut log = VC_LOG[n].lock();
                    log.extend(bytes);
                    if log.len() > VC_LOG_MAX {
                        // Drop old output up to a line boundary.
                        let excess = log.len() - VC_LOG_MAX / 2;
                        let cut = log
                            .iter()
                            .skip(excess)
                            .position(|&b| b == b'\n')
                            .map_or(excess, |p| excess + p + 1);
                        log.drain(..cut);
                    }
                }
                if n == ACTIVE_VC.load(Ordering::SeqCst) {
                    crate::drivers::console::reset_view();
                    x86_64::instructions::interrupts::without_interrupts(|| {
                        if let Some(c) = crate::drivers::framebuffer::CONSOLE.lock().as_mut() {
                            c.write_bytes(bytes);
                        }
                    });
                }
                if n == 0 {
                    crate::drivers::serial::write_raw(bytes);
                }
            }
            Sink::Pty(m) => {
                if let Some(m) = m.upgrade() {
                    m.slave_output(bytes);
                }
            }
            Sink::Driver(d) => {
                let mut rest = bytes;
                while !rest.is_empty() && !self.hung_up.load(Ordering::SeqCst) {
                    match d.write(rest) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => rest = &rest[n.min(rest.len())..],
                    }
                }
            }
        }
    }
}

impl Tty {
    fn output(&self, data: &[u8]) {
        let t = *self.termios.lock();
        if t.oflag & OPOST != 0 && t.oflag & ONLCR != 0 && data.contains(&b'\n') {
            let mut out = alloc::vec::Vec::with_capacity(data.len() + 16);
            for &b in data {
                if b == b'\n' {
                    out.push(b'\r');
                }
                out.push(b);
            }
            self.emit(&out);
        } else {
            self.emit(data);
        }
        // Answer device status queries (cursor position reports) of the
        // visible console.
        if !matches!(self.sink, Sink::Console(n) if n == ACTIVE_VC.load(Ordering::SeqCst)) {
            return;
        }
        let mut buf = [0u8; 32];
        let n = x86_64::instructions::interrupts::without_interrupts(|| {
            crate::drivers::framebuffer::CONSOLE
                .lock()
                .as_mut()
                .map_or(0, |c| c.take_replies(&mut buf))
        });
        if n > 0 {
            let mut r = self.ready.lock();
            for &b in &buf[..n] {
                r.push_back(b as u16);
            }
            drop(r);
            self.notify();
        }
    }

    fn echo(&self, b: u8) {
        let t = *self.termios.lock();
        if t.lflag & ECHO == 0 {
            return;
        }
        if b < 0x20 && b != b'\n' && b != b'\t' && t.lflag & ECHOCTL != 0 {
            self.emit(&[b'^', b + 0x40]);
        } else {
            self.output(&[b]);
        }
    }

    fn notify(&self) {
        self.wq.wake_all();
    }

    fn signal_fg(&self, sig: u32) {
        let pg = self.fg_pgrp.load(Ordering::SeqCst);
        if pg != 0 {
            signal::send_group(pg, sig);
        }
    }

    /// Process one input byte through the line discipline.
    pub fn receive(&self, mut b: u8) {
        let t = *self.termios.lock();
        if b == b'\r' {
            if t.iflag & IGNCR != 0 {
                return;
            }
            if t.iflag & ICRNL != 0 {
                b = b'\n';
            }
        } else if b == b'\n' && t.iflag & INLCR != 0 {
            b = b'\r';
        }
        if t.lflag & ISIG != 0 {
            let sig = if b == t.cc[VINTR] {
                Some(signal::SIGINT)
            } else if b == t.cc[VQUIT] {
                Some(signal::SIGQUIT)
            } else if b == t.cc[VSUSP] {
                Some(signal::SIGTSTP)
            } else {
                None
            };
            if let Some(sig) = sig {
                self.line.lock().clear();
                self.ready.lock().clear();
                self.echo(b);
                if t.lflag & ECHO != 0 {
                    self.output(b"\n");
                }
                self.signal_fg(sig);
                self.notify();
                return;
            }
        }
        if t.lflag & ICANON == 0 {
            self.ready.lock().push_back(b as u16);
            self.echo(b);
            self.notify();
            return;
        }
        let mut line = self.line.lock();
        if b == t.cc[VERASE] || b == 0x08 {
            if line.pop().is_some() && t.lflag & ECHO != 0 {
                self.emit(b"\x08 \x08");
            }
            return;
        }
        if b == t.cc[VKILL] {
            let n = line.len();
            line.clear();
            if t.lflag & ECHO != 0 {
                for _ in 0..n {
                    self.emit(b"\x08 \x08");
                }
            }
            return;
        }
        if b == t.cc[VWERASE] {
            while line.last() == Some(&b' ') {
                line.pop();
                self.emit(b"\x08 \x08");
            }
            while line.last().is_some_and(|&c| c != b' ') {
                line.pop();
                self.emit(b"\x08 \x08");
            }
            return;
        }
        if b == t.cc[VEOF] {
            let mut r = self.ready.lock();
            if line.is_empty() {
                r.push_back(EOF_MARK);
            } else {
                r.extend(line.drain(..).map(|c| c as u16));
            }
            drop(r);
            drop(line);
            self.notify();
            return;
        }
        line.push(b);
        drop(line);
        self.echo(b);
        if b == b'\n' {
            let mut line = self.line.lock();
            self.ready.lock().extend(line.drain(..).map(|c| c as u16));
            drop(line);
            self.notify();
        }
    }

    pub fn receive_bytes(&self, bytes: &[u8]) {
        for &b in bytes {
            self.receive(b);
        }
    }

    fn check_background_read(&self) -> KResult<()> {
        let Some(p) = process::current() else {
            return Ok(());
        };
        let fg = self.fg_pgrp.load(Ordering::SeqCst);
        let pg = p.pgid.load(Ordering::SeqCst);
        if fg != 0 && pg != fg && process::group_members(fg).iter().any(|m| m.pid != p.pid) {
            signal::send_group(pg, signal::SIGTTIN);
            return Err(EINTR);
        }
        Ok(())
    }
}

impl FileLike for Tty {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.check_background_read()?;
        let t = *self.termios.lock();
        let canon = t.lflag & ICANON != 0;
        let hung = || self.hung_up.load(Ordering::SeqCst);
        let (vmin, vtime) = (t.cc[VMIN], t.cc[VTIME]);
        loop {
            {
                let mut r = self.ready.lock();
                if !r.is_empty() {
                    let mut n = 0;
                    while n < buf.len() {
                        match r.front().copied() {
                            None => break,
                            Some(EOF_MARK) => {
                                if n == 0 {
                                    r.pop_front();
                                }
                                return Ok(n);
                            }
                            Some(c) => {
                                r.pop_front();
                                buf[n] = c as u8;
                                n += 1;
                                if canon && c == b'\n' as u16 {
                                    return Ok(n);
                                }
                            }
                        }
                    }
                    return Ok(n);
                }
            }
            if hung() {
                return Ok(0);
            }
            if nonblock || (!canon && vmin == 0 && vtime == 0) {
                return if nonblock { Err(EAGAIN) } else { Ok(0) };
            }
            let me = self;
            let has = || !me.ready.lock().is_empty() || me.hung_up.load(Ordering::SeqCst);
            if !canon && vmin == 0 {
                if !self.wq.wait_timeout(vtime as u64 * 100, has) {
                    return Ok(0);
                }
                continue;
            }
            if !self.wq.wait_interruptible(has) && signal::has_pending() {
                return Err(EINTR);
            }
        }
    }

    fn write(&self, buf: &[u8], _nonblock: bool) -> KResult<usize> {
        if self.hung_up.load(Ordering::SeqCst) {
            return Err(EIO);
        }
        self.output(buf);
        Ok(buf.len())
    }

    fn wait_queue(&self) -> &WaitQueue {
        &self.wq
    }

    fn poll(&self) -> u16 {
        if self.hung_up.load(Ordering::SeqCst) {
            return vfs::POLLIN | vfs::POLLHUP;
        }
        let mut ev = vfs::POLLOUT;
        if !self.ready.lock().is_empty() {
            ev |= vfs::POLLIN;
        }
        ev
    }

    fn open_instance(&self, flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        if let Sink::Pty(m) = &self.sink {
            let m = m.upgrade().ok_or(EIO)?;
            m.slave_opened()?;
        }
        if let Sink::Driver(d) = &self.sink {
            if self.hung_up.load(Ordering::SeqCst) {
                return Err(EIO);
            }
            if self.opens.fetch_add(1, Ordering::SeqCst) == 0 {
                if let Err(e) = d.open() {
                    self.opens.fetch_sub(1, Ordering::SeqCst);
                    return Err(e);
                }
                d.set_termios(&self.termios.lock());
            }
        }
        // A session leader without a terminal acquires this one.
        if flags & vfs::O_NOCTTY == 0
            && let Some(p) = process::current()
            && p.sid.load(Ordering::SeqCst) == p.pid
            && p.ctty.lock().is_none()
        {
            *p.ctty.lock() = Some(self.arc());
        }
        Ok(None)
    }

    fn close(&self) {
        if let Sink::Pty(m) = &self.sink
            && let Some(m) = m.upgrade()
        {
            m.slave_closed();
        }
        if let Sink::Driver(d) = &self.sink
            && self.opens.fetch_sub(1, Ordering::SeqCst) == 1
        {
            d.close();
        }
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        const TCGETS: u64 = 0x5401;
        const TCSETS: u64 = 0x5402;
        const TCSETSW: u64 = 0x5403;
        const TCSETSF: u64 = 0x5404;
        const TCFLSH: u64 = 0x540B;
        const TIOCSCTTY: u64 = 0x540E;
        const TIOCGPGRP: u64 = 0x540F;
        const TIOCSPGRP: u64 = 0x5410;
        const TIOCGWINSZ: u64 = 0x5413;
        const TIOCSWINSZ: u64 = 0x5414;
        const FIONREAD: u64 = 0x541B;
        const TIOCNOTTY: u64 = 0x5422;
        const TIOCGSID: u64 = 0x5429;
        const VT_GETSTATE: u64 = 0x5603;
        const VT_ACTIVATE: u64 = 0x5606;
        const VT_WAITACTIVE: u64 = 0x5607;
        const VT_GETMODE: u64 = 0x5601;
        const VT_SETMODE: u64 = 0x5602;
        const VT_RELDISP: u64 = 0x5605;
        const KDSETMODE: u64 = 0x4B3A;
        const KDGETMODE: u64 = 0x4B3B;
        const KDGKBMODE: u64 = 0x4B44;
        const KDSKBMODE: u64 = 0x4B45;
        const KD_GRAPHICS: u64 = 1;
        const VT_PROCESS: u8 = 1;
        let vc = match self.sink {
            Sink::Console(n) => Some(n),
            _ => None,
        };
        match cmd {
            VT_GETSTATE | VT_ACTIVATE | VT_WAITACTIVE | VT_GETMODE | VT_SETMODE | VT_RELDISP
            | KDSETMODE | KDGETMODE | KDGKBMODE | KDSKBMODE
                if vc.is_none() =>
            {
                Err(ENOTTY)
            }
            KDSETMODE => {
                let n = vc.unwrap();
                let on = arg == KD_GRAPHICS;
                VC_DISPLAY[n].lock().graphics = on;
                if n == active_index() {
                    crate::drivers::framebuffer::set_graphics(on);
                    if !on {
                        replay_vc(n);
                    }
                }
                Ok(0)
            }
            KDGETMODE => {
                let mode = VC_DISPLAY[vc.unwrap()].lock().graphics as u32;
                uaccess::write_user(arg, &mode)?;
                Ok(0)
            }
            KDGKBMODE => {
                let mode = VC_DISPLAY[vc.unwrap()].lock().kbmode;
                uaccess::write_user(arg, &mode)?;
                Ok(0)
            }
            KDSKBMODE => {
                // K_RAW, K_XLATE, K_MEDIUMRAW, K_UNICODE, K_OFF. Raw
                // scancode modes deliver nothing (programs use evdev).
                if arg > K_OFF as u64 {
                    return Err(EINVAL);
                }
                VC_DISPLAY[vc.unwrap()].lock().kbmode = arg as u32;
                Ok(0)
            }
            VT_RELDISP => {
                const VT_ACKACQ: u64 = 2;
                match arg {
                    VT_ACKACQ => Ok(0),
                    0 => {
                        // The owner refuses the switch.
                        PENDING_SWITCH.store(usize::MAX, Ordering::SeqCst);
                        Ok(0)
                    }
                    _ => {
                        let n = PENDING_SWITCH.swap(usize::MAX, Ordering::SeqCst);
                        if n == usize::MAX {
                            return Err(EINVAL);
                        }
                        complete_switch(n);
                        Ok(0)
                    }
                }
            }
            VT_GETMODE => {
                // struct vt_mode { char mode, waitv; short relsig, acqsig, frsig; }
                let d = VC_DISPLAY[vc.unwrap()].lock();
                let (mode, rel, acq) = d
                    .process
                    .map_or((0, 0, 0), |(_, r, a)| (1u8, r as i16, a as i16));
                let raw: [u8; 8] = {
                    let mut b = [0u8; 8];
                    b[0] = mode;
                    b[2..4].copy_from_slice(&rel.to_ne_bytes());
                    b[4..6].copy_from_slice(&acq.to_ne_bytes());
                    b
                };
                uaccess::write_user(arg, &raw)?;
                Ok(0)
            }
            VT_SETMODE => {
                let raw: [u8; 8] = uaccess::read_user(arg)?;
                let rel = i16::from_ne_bytes([raw[2], raw[3]]) as u32;
                let acq = i16::from_ne_bytes([raw[4], raw[5]]) as u32;
                let pid = crate::process::current().map_or(0, |p| p.pid);
                VC_DISPLAY[vc.unwrap()].lock().process =
                    (raw[0] == VT_PROCESS).then_some((pid, rel, acq));
                Ok(0)
            }
            VT_GETSTATE => {
                // struct vt_stat { v_active, v_signal, v_state } (u16 each).
                let st: [u16; 3] = [
                    active_index() as u16 + 1,
                    0,
                    ((1u32 << (NUM_VCS + 1)) - 2) as u16,
                ];
                uaccess::write_user(arg, &st)?;
                Ok(0)
            }
            VT_ACTIVATE => {
                if arg == 0 || arg as usize > NUM_VCS {
                    return Err(ENXIO);
                }
                switch_vc(arg as usize - 1);
                Ok(0)
            }
            VT_WAITACTIVE => {
                if arg == 0 || arg as usize > NUM_VCS {
                    return Err(ENXIO);
                }
                let want = arg as usize - 1;
                if !VT_WQ.wait_interruptible(|| active_index() == want) {
                    return Err(EINTR);
                }
                Ok(0)
            }
            TCGETS => {
                uaccess::write_user(arg, &*self.termios.lock())?;
                Ok(0)
            }
            TCSETS | TCSETSW | TCSETSF => {
                let t: Termios = uaccess::read_user(arg)?;
                let was_canon = self.termios.lock().lflag & ICANON != 0;
                *self.termios.lock() = t;
                if cmd == TCSETSF {
                    self.ready.lock().clear();
                    self.line.lock().clear();
                }
                // Leaving canonical mode releases the partial line.
                if was_canon && t.lflag & ICANON == 0 {
                    let mut line = self.line.lock();
                    self.ready.lock().extend(line.drain(..).map(|c| c as u16));
                }
                if let Sink::Driver(d) = &self.sink {
                    d.set_termios(&t);
                }
                Ok(0)
            }
            TCFLSH => {
                if arg == 0 || arg == 2 {
                    self.ready.lock().clear();
                    self.line.lock().clear();
                }
                Ok(0)
            }
            TIOCGPGRP => {
                let pg = match self.fg_pgrp.load(Ordering::SeqCst) {
                    0 => process::current().map_or(0, |p| p.pgid.load(Ordering::SeqCst)),
                    pg => pg,
                };
                uaccess::write_user(arg, &pg)?;
                Ok(0)
            }
            TIOCSPGRP => {
                let pg: u32 = uaccess::read_user(arg)?;
                self.fg_pgrp.store(pg, Ordering::SeqCst);
                // A session leader setting the foreground group of a
                // terminal it has no claim on yet makes it the controlling one.
                if let Some(p) = process::current()
                    && p.sid.load(Ordering::SeqCst) == p.pid
                    && p.ctty.lock().is_none()
                {
                    *p.ctty.lock() = Some(self.arc());
                }
                Ok(0)
            }
            TIOCGWINSZ => {
                uaccess::write_user(arg, &*self.winsize.lock())?;
                Ok(0)
            }
            TIOCSWINSZ => {
                let w: [u16; 4] = uaccess::read_user(arg)?;
                *self.winsize.lock() = w;
                self.signal_fg(signal::SIGWINCH);
                Ok(0)
            }
            FIONREAD => {
                let n = self.ready.lock().iter().filter(|&&c| c != EOF_MARK).count() as i32;
                uaccess::write_user(arg, &n)?;
                Ok(0)
            }
            TIOCSCTTY => {
                let p = process::current().ok_or(ENOTTY)?;
                if p.sid.load(Ordering::SeqCst) != p.pid {
                    return Err(EPERM);
                }
                *p.ctty.lock() = Some(self.arc());
                self.fg_pgrp
                    .store(p.pgid.load(Ordering::SeqCst), Ordering::SeqCst);
                Ok(0)
            }
            TIOCNOTTY => {
                if let Some(p) = process::current() {
                    *p.ctty.lock() = None;
                }
                Ok(0)
            }
            TIOCGSID => {
                let sid = process::current().map_or(0, |p| p.sid.load(Ordering::SeqCst));
                uaccess::write_user(arg, &sid)?;
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    fn stat(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::CharDevice, 0o620);
        m.rdev = match &self.sink {
            Sink::Console(n) => (4 << 8) | (*n as u64 + 1),
            Sink::Pty(p) => (136 << 8) | p.upgrade().map_or(0, |p| p.index() as u64),
            Sink::Driver(d) => d.rdev(),
        };
        Ok(m)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A process exited: if its group was in the foreground and is now empty,
/// release the terminal.
pub fn process_exited(p: &Arc<process::Process>) {
    // A VT_PROCESS owner died: its console returns to text mode with
    // keyboard input (as logind resets it), and a waiting switch goes on.
    for (n, d) in VC_DISPLAY.iter().enumerate() {
        let mut d = d.lock();
        if d.process.is_some_and(|(pid, _, _)| pid == p.pid) {
            d.process = None;
            d.kbmode = K_UNICODE;
            let was_graphics = core::mem::replace(&mut d.graphics, false);
            d.pixels = None;
            drop(d);
            if n == active_index() {
                let pending = PENDING_SWITCH.swap(usize::MAX, Ordering::SeqCst);
                if pending != usize::MAX {
                    complete_switch(pending);
                } else if was_graphics {
                    crate::drivers::framebuffer::set_graphics(false);
                    replay_vc(n);
                }
            }
        }
    }
    let Some(tty) = p.ctty.lock().clone() else {
        return;
    };
    let fg = tty.fg_pgrp.load(Ordering::SeqCst);
    if fg == p.pgid.load(Ordering::SeqCst) && process::group_members(fg).is_empty() {
        tty.fg_pgrp.store(0, Ordering::SeqCst);
    }
}

pub fn process_stopped(_p: &Arc<process::Process>) {}

// ---------------------------------------------------------------------------
// Input plumbing
// ---------------------------------------------------------------------------

static INPUT_WQ: WaitQueue = WaitQueue::new();
static SERIAL_RX: Mutex<VecDeque<u8>> = Mutex::new(VecDeque::new());
static PENDING_INPUT: AtomicU32 = AtomicU32::new(0);

/// Called from interrupt handlers when keyboard or serial input arrived.
pub fn input_available() {
    PENDING_INPUT.fetch_add(1, Ordering::SeqCst);
    INPUT_WQ.wake_all();
}

/// Queue raw bytes from the serial port (interrupt context).
pub fn serial_input(b: u8) {
    SERIAL_RX.lock().push_back(b);
    input_available();
}

/// Feed terminal bytes directly (USB keyboards): to the visible console.
pub fn inject(bytes: &[u8]) {
    if keyboard_to_tty() {
        active().receive_bytes(bytes);
    }
}

/// Print scheduler, process and TTY state (serial BREAK, like SysRq).
pub fn debug_dump() {
    // Written straight to the serial port: the dump must get out even
    // while output locks are held.
    macro_rules! dprint {
        ($($arg:tt)*) => {
            crate::drivers::serial::write_unlocked(
                alloc::format!("{}\n", format_args!($($arg)*)).as_bytes(),
            )
        };
    }
    // First sample every other CPU (NMI: works with interrupts off).
    crate::arch::x86_64::smp::nmi_dump_others();
    dprint!("[sysrq] locks: {}", crate::drivers::console::lock_state());
    dprint!(
        "[sysrq] vc logs locked: {:?}",
        VC_LOG
            .iter()
            .map(|l| l.is_locked())
            .collect::<alloc::vec::Vec<_>>()
    );
    // Only try locks: the dump must work while something holds them.
    let tty = console();
    match tty.termios.try_lock() {
        Some(t) => dprint!(
            "[sysrq] tty lflag={:#o} iflag={:#o} vmin={} fg_pgrp={} ready={:?} line={:?}",
            t.lflag,
            t.iflag,
            t.cc[VMIN],
            tty.fg_pgrp.load(Ordering::SeqCst),
            tty.ready.try_lock().map(|r| r.len()),
            tty.line.try_lock().map(|l| l.len())
        ),
        None => dprint!("[sysrq] tty termios <locked>"),
    }
    for id in 0..crate::arch::x86_64::cpu::cpu_count() {
        if let Some(c) = crate::arch::x86_64::cpu::cpu(id) {
            let cur = c.current.load(Ordering::SeqCst) as *const crate::sched::Thread;
            let tid = unsafe { cur.as_ref() }.map_or(0, |t| t.tid);
            dprint!("[sysrq] cpu{} running tid {}", id, tid);
        }
    }
    dprint!("[sysrq] run queues {:?}", crate::sched::queue_lengths());
    match crate::sched::timer_summary() {
        Some((n, next)) => dprint!(
            "[sysrq] timers: {} pending, next in {} ms; uptime {} ms; ticks {}",
            n,
            next,
            crate::time::nanos() / 1_000_000,
            crate::time::ticks()
        ),
        None => dprint!("[sysrq] timers: list locked"),
    }
    let Some(procs) = crate::process::try_all() else {
        dprint!("[sysrq] process table <locked>");
        return;
    };
    for p in procs {
        let threads: alloc::vec::Vec<alloc::string::String> = match p.threads.try_lock() {
            Some(ts) => ts
                .iter()
                .filter_map(|w| w.upgrade())
                .map(|th| match th.syscall.load(Ordering::Relaxed) {
                    u64::MAX => alloc::format!("{}:{:?}", th.tid, th.state()),
                    nr => alloc::format!(
                        "{}:{:?} in syscall {}({:#x})",
                        th.tid,
                        th.state(),
                        nr,
                        th.syscall_arg.load(Ordering::Relaxed)
                    ),
                })
                .collect(),
            None => alloc::vec![alloc::string::String::from("<locked>")],
        };
        let name = p
            .name
            .try_lock()
            .map_or(alloc::string::String::from("<locked>"), |n| n.clone());
        let cmd = p
            .cmdline
            .try_lock()
            .map_or(alloc::vec![alloc::string::String::from("<locked>")], |c| {
                c.clone()
            });
        dprint!(
            "[sysrq] pid {} ppid {} pgid {} sid {} {}{}{} threads {:?} cmd {:?}",
            p.pid,
            p.ppid.load(Ordering::SeqCst),
            p.pgid.load(Ordering::SeqCst),
            p.sid.load(Ordering::SeqCst),
            name,
            if p.zombie.load(Ordering::SeqCst) {
                " zombie"
            } else {
                ""
            },
            if p.stopped.load(Ordering::SeqCst) {
                " stopped"
            } else {
                ""
            },
            threads,
            cmd
        );
    }
    for (tid, name, state, user) in crate::sched::try_thread_list() {
        // A Ready thread in no run queue never runs again: say where it
        // was made Ready.
        let lost = state == crate::sched::State::Ready && !crate::sched::is_queued(tid);
        let site = || {
            crate::sched::find_thread(tid)
                .and_then(|t| t.state_site())
                .map(|l| alloc::format!(" (state set at {}:{})", l.file(), l.line()))
                .unwrap_or_default()
        };
        if lost {
            dprint!(
                "[sysrq] {} {} Ready but in no run queue{}",
                if user { "thread" } else { "kthread" },
                tid,
                site()
            );
        } else if !user {
            dprint!("[sysrq] kthread {} {:?} {}", tid, state, name);
        }
    }
    let base = 0xffff_8000_0000_0000u64;
    let chans: alloc::vec::Vec<alloc::string::String> = crate::sched::wait_channels()
        .into_iter()
        .filter(|(_, w)| *w != 0)
        .map(|(t, w)| {
            if w >= base && w < base + 0x400_0000 {
                alloc::format!("{}@{:#x}", t, w - base)
            } else {
                alloc::format!("{}@heap:{:#x}", t, w)
            }
        })
        .collect();
    dprint!("[sysrq] wait channels (image offsets) {:?}", chans);
}

/// Start the thread that moves keyboard and serial input into the TTY.
pub fn start_input_thread() {
    crate::sched::spawn("tty-input", || {
        let tty = console();
        loop {
            let seen = PENDING_INPUT.load(Ordering::SeqCst);
            crate::drivers::serial::poll_rx();
            if crate::drivers::serial::SYSRQ.swap(false, Ordering::Relaxed) {
                crate::drivers::serial::SYSRQ_SINCE.store(0, Ordering::SeqCst);
                debug_dump();
            }
            while let Some(ev) = crate::task::keyboard::read_key() {
                use crate::task::keyboard::Key;
                match ev {
                    Key::Bytes(b, n) if keyboard_to_tty() => active().receive_bytes(&b[..n]),
                    Key::SwitchVc(n) if keyboard_to_tty() => switch_vc(n),
                    Key::Bytes(..) | Key::SwitchVc(_) => {}
                    Key::ScrollUp => crate::drivers::console::scroll_view_up(),
                    Key::ScrollDown => crate::drivers::console::scroll_view_down(),
                }
            }
            loop {
                let b = x86_64::instructions::interrupts::without_interrupts(|| {
                    SERIAL_RX.lock().pop_front()
                });
                match b {
                    Some(b) => tty.receive(b),
                    None => break,
                }
            }
            INPUT_WQ.wait_timeout(100, || PENDING_INPUT.load(Ordering::SeqCst) != seen);
        }
    });
}
