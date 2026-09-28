//! Terminals: virtual consoles and pseudo-terminals.
//!
//! Every terminal is a [`Tty`]: a POSIX line discipline (canonical
//! editing, echo, job-control signals for the foreground process group)
//! whose output goes to a sink. There are four virtual consoles on the
//! framebuffer (`/dev/tty1`..`tty4`, switched with Alt-F1..F4 or `chvt`);
//! the first is also COM1 (`/dev/console`, `/dev/ttyS0`). Keyboard input
//! goes to the visible console, serial input to the first. Pseudo-terminal
//! slaves (`/dev/pts/N`) send their output to the master side (see
//! [`pty`]).

use crate::errno::*;
use crate::process::{self, signal, uaccess};
use crate::sched::WaitQueue;
use crate::vfs::{self, File, FileLike, FileType, Metadata};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::{Mutex, Once};

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

/// Where a terminal's output goes.
enum Sink {
    /// Virtual console `n` (0-based).
    Console(usize),
    /// The master side of a pseudo-terminal.
    Pty(alloc::sync::Weak<pty::Pty>),
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
}

pub const NUM_VCS: usize = 4;
static VCS: Once<[Arc<Tty>; NUM_VCS]> = Once::new();
/// Output kept per virtual console, replayed when it becomes visible.
static VC_LOG: [Mutex<VecDeque<u8>>; NUM_VCS] = [const { Mutex::new(VecDeque::new()) }; NUM_VCS];
const VC_LOG_MAX: usize = 32 * 1024;
static ACTIVE_VC: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

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
        })
    }

    fn arc(&self) -> Arc<Tty> {
        self.me.upgrade().expect("tty dropped")
    }

    /// The master side closed: wake readers (they see EOF) and send SIGHUP
    /// to the foreground group.
    fn hang_up(&self) {
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
pub fn switch_vc(n: usize) {
    if n >= NUM_VCS || n == ACTIVE_VC.swap(n, Ordering::SeqCst) {
        return;
    }
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
        vfs::notify_poll();
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
        match cmd {
            VT_GETSTATE | VT_ACTIVATE | VT_WAITACTIVE if !matches!(self.sink, Sink::Console(_)) => {
                Err(ENOTTY)
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
            VT_WAITACTIVE => Ok(0),
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
    active().receive_bytes(bytes);
}

/// Print scheduler, process and TTY state (serial BREAK, like SysRq).
pub fn debug_dump() {
    let tty = console();
    let t = *tty.termios.lock();
    crate::println!(
        "[sysrq] tty lflag={:#o} iflag={:#o} vmin={} fg_pgrp={} ready={} line={}",
        t.lflag,
        t.iflag,
        t.cc[VMIN],
        tty.fg_pgrp.load(Ordering::SeqCst),
        tty.ready.lock().len(),
        tty.line.lock().len()
    );
    for p in crate::process::all() {
        let threads: alloc::vec::Vec<alloc::string::String> = p
            .live_threads()
            .iter()
            .map(|th| alloc::format!("{}:{:?}", th.tid, th.state()))
            .collect();
        crate::println!(
            "[sysrq] pid {} ppid {} pgid {} sid {} {}{}{} threads {:?} cmd {:?}",
            p.pid,
            p.ppid.load(Ordering::SeqCst),
            p.pgid.load(Ordering::SeqCst),
            p.sid.load(Ordering::SeqCst),
            p.name.lock(),
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
            p.cmdline.lock()
        );
    }
    for (tid, name, state, user) in crate::sched::thread_list() {
        if !user {
            crate::println!("[sysrq] kthread {} {:?} {}", tid, state, name);
        }
    }
}

/// Start the thread that moves keyboard and serial input into the TTY.
pub fn start_input_thread() {
    crate::sched::spawn("tty-input", || {
        let tty = console();
        loop {
            let seen = PENDING_INPUT.load(Ordering::SeqCst);
            crate::drivers::serial::poll_rx();
            if crate::drivers::serial::SYSRQ.swap(false, Ordering::Relaxed) {
                debug_dump();
            }
            while let Some(ev) = crate::task::keyboard::read_key() {
                use crate::task::keyboard::Key;
                match ev {
                    Key::Bytes(b, n) => active().receive_bytes(&b[..n]),
                    Key::SwitchVc(n) => switch_vc(n),
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
