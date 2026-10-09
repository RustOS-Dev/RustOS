//! Pseudo-terminals: `/dev/ptmx` and `/dev/pts/N`.
//!
//! Opening `/dev/ptmx` creates a pair and returns the master. The slave is
//! an ordinary [`Tty`] whose output lands in the master's read buffer;
//! bytes written to the master go through the slave's line discipline.
//! The slave starts locked (`TIOCSPTLCK`, cleared by `unlockpt`); closing
//! the master hangs the slave up, closing every slave makes master reads
//! fail with EIO (after the buffered output).

use super::{Sink, Tty};
use crate::errno::*;
use crate::process::uaccess;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use crate::vfs::{self, FileLike, FileType, Metadata};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const TIOCGPTN: u64 = 0x8004_5430;
const TIOCSPTLCK: u64 = 0x4004_5431;
const TIOCGPTLCK: u64 = 0x8004_5439;
/// Open the slave from the master (Linux 4.13+; rustix's openpty uses it).
const TIOCGPTPEER: u64 = 0x5441;
const MAX_BUFFER: usize = 64 * 1024;

pub struct Pty {
    index: u32,
    slave: Arc<Tty>,
    /// Slave output waiting for the master to read it.
    to_master: Mutex<VecDeque<u8>>,
    wq: WaitQueue,
    locked: AtomicBool,
    master_open: AtomicBool,
    /// Open slave descriptions (files).
    slave_opens: AtomicU32,
    /// A slave was opened at least once (EIO on master reads only then).
    slave_seen: AtomicBool,
}

static PTYS: Mutex<BTreeMap<u32, Arc<Pty>>> = Mutex::new(BTreeMap::new());

impl Pty {
    pub fn index(&self) -> u32 {
        self.index
    }

    pub(super) fn slave_output(&self, bytes: &[u8]) {
        if !self.master_open.load(Ordering::SeqCst) {
            return;
        }
        let mut q = self.to_master.lock();
        let room = MAX_BUFFER.saturating_sub(q.len());
        q.extend(&bytes[..bytes.len().min(room)]);
        drop(q);
        self.wq.wake_all();
    }

    pub(super) fn slave_opened(&self) -> KResult<()> {
        if self.locked.load(Ordering::SeqCst) || !self.master_open.load(Ordering::SeqCst) {
            return Err(EIO);
        }
        self.slave_opens.fetch_add(1, Ordering::SeqCst);
        self.slave_seen.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub(super) fn slave_closed(&self) {
        if self.slave_opens.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.wq.wake_all();
            self.maybe_release();
        }
    }

    fn slaves_gone(&self) -> bool {
        self.slave_seen.load(Ordering::SeqCst) && self.slave_opens.load(Ordering::SeqCst) == 0
    }

    /// Forget the pair once both sides are closed.
    fn maybe_release(&self) {
        if !self.master_open.load(Ordering::SeqCst) && self.slave_opens.load(Ordering::SeqCst) == 0
        {
            PTYS.lock().remove(&self.index);
            vfs::devfs::unregister(&format!("pts/{}", self.index));
        }
    }
}

/// The master side (one per `/dev/ptmx` open).
pub struct Master(Arc<Pty>);

impl FileLike for Master {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        let p = &self.0;
        loop {
            {
                let mut q = p.to_master.lock();
                if !q.is_empty() {
                    let n = buf.len().min(q.len());
                    for (i, b) in q.drain(..n).enumerate() {
                        buf[i] = b;
                    }
                    return Ok(n);
                }
            }
            if p.slaves_gone() {
                return Err(EIO);
            }
            if nonblock {
                return Err(EAGAIN);
            }
            let ready = || !p.to_master.lock().is_empty() || p.slaves_gone();
            if !p.wq.wait_interruptible(ready) && crate::process::signal::has_pending() {
                return Err(EINTR);
            }
        }
    }

    fn write(&self, buf: &[u8], _nonblock: bool) -> KResult<usize> {
        self.0.slave.receive_bytes(buf);
        Ok(buf.len())
    }

    fn wait_queue(&self) -> &WaitQueue {
        &self.0.wq
    }

    fn poll(&self) -> u16 {
        let mut ev = vfs::POLLOUT;
        if !self.0.to_master.lock().is_empty() {
            ev |= vfs::POLLIN;
        }
        if self.0.slaves_gone() {
            ev |= vfs::POLLHUP;
        }
        ev
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        match cmd {
            TIOCGPTN => {
                uaccess::write_user(arg, &self.0.index)?;
                Ok(0)
            }
            TIOCSPTLCK => {
                let v: i32 = uaccess::read_user(arg)?;
                self.0.locked.store(v != 0, Ordering::SeqCst);
                Ok(0)
            }
            TIOCGPTLCK => {
                uaccess::write_user(arg, &(self.0.locked.load(Ordering::SeqCst) as i32))?;
                Ok(0)
            }
            TIOCGPTPEER => {
                // `arg` holds the open flags (O_RDWR, O_NOCTTY, O_CLOEXEC...).
                let flags = arg as u32;
                let slave: Arc<dyn FileLike> = self.0.slave.clone();
                if let Some(other) = slave.open_instance(flags)? {
                    // Ttys open themselves; nothing else is expected here.
                    drop(other);
                }
                let path = format!("/dev/pts/{}", self.0.index);
                let file =
                    vfs::File::from_stream(slave, flags & !(vfs::O_NOCTTY | vfs::O_CLOEXEC), &path);
                let p = crate::process::current().ok_or(ESRCH)?;
                let fd = p.files.lock().install(file, flags & vfs::O_CLOEXEC != 0)?;
                Ok(fd as i64)
            }
            // Terminal settings and window size act on the slave.
            _ => self.0.slave.ioctl(cmd, arg),
        }
    }

    fn stat(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::CharDevice, 0o666);
        m.rdev = (5 << 8) | 2;
        Ok(m)
    }

    fn close(&self) {
        self.0.master_open.store(false, Ordering::SeqCst);
        self.0.slave.hang_up();
        self.0.maybe_release();
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `/dev/ptmx`: every open creates a new pseudo-terminal.
pub struct Ptmx;

impl FileLike for Ptmx {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(EIO)
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(EIO)
    }
    fn open_instance(&self, _flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        let pty = {
            let mut all = PTYS.lock();
            let index = (0..1024u32).find(|i| !all.contains_key(i)).ok_or(ENOSPC)?;
            let pty = Arc::new_cyclic(|me: &alloc::sync::Weak<Pty>| Pty {
                index,
                slave: Tty::new(Sink::Pty(me.clone()), 24, 80),
                to_master: Mutex::new(VecDeque::new()),
                wq: WaitQueue::new(),
                locked: AtomicBool::new(true),
                master_open: AtomicBool::new(true),
                slave_opens: AtomicU32::new(0),
                slave_seen: AtomicBool::new(false),
            });
            all.insert(index, pty.clone());
            pty
        };
        vfs::devfs::register(
            &format!("pts/{}", pty.index),
            FileType::CharDevice,
            (136 << 8) | pty.index as u64,
            pty.slave.clone(),
        );
        Ok(Some(Arc::new(Master(pty))))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `/dev/tty`: the caller's controlling terminal.
pub struct Ctty;

impl FileLike for Ctty {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(ENXIO)
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(ENXIO)
    }
    fn open_instance(&self, _flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        let p = crate::process::current().ok_or(ENXIO)?;
        let t = p.ctty.lock().clone().ok_or(ENXIO)?;
        Ok(Some(t))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `/dev/tty0`: the visible virtual console.
pub struct ActiveVc;

impl FileLike for ActiveVc {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(ENXIO)
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(ENXIO)
    }
    fn open_instance(&self, _flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        Ok(Some(super::active()))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
