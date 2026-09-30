//! Pipes and FIFOs.

use super::*;
use alloc::collections::VecDeque;

const PIPE_CAPACITY: usize = 64 * 1024;

pub struct PipeInner {
    buf: Mutex<VecDeque<u8>>,
    readers: AtomicU32,
    writers: AtomicU32,
    /// Readers, writers and pollers wait here (shared by both directions
    /// of a socketpair).
    wq: Arc<WaitQueue>,
}

impl PipeInner {
    fn new(wq: Arc<WaitQueue>) -> Arc<PipeInner> {
        Arc::new(PipeInner {
            buf: Mutex::new(VecDeque::new()),
            readers: AtomicU32::new(0),
            writers: AtomicU32::new(0),
            wq,
        })
    }

    fn notify(&self) {
        self.wq.wake_all();
    }
}

pub struct PipeEnd {
    inner: Arc<PipeInner>,
    write: bool,
}

impl PipeEnd {
    fn new(inner: &Arc<PipeInner>, write: bool) -> Arc<PipeEnd> {
        if write {
            inner.writers.fetch_add(1, Ordering::SeqCst);
        } else {
            inner.readers.fetch_add(1, Ordering::SeqCst);
        }
        Arc::new(PipeEnd {
            inner: inner.clone(),
            write,
        })
    }
}

/// Create an anonymous pipe: (read end, write end).
pub fn pipe() -> (Arc<dyn FileLike>, Arc<dyn FileLike>) {
    pipe_on(Arc::new(WaitQueue::new()))
}

/// A pipe whose wake-ups go to `wq` (a socketpair shares one queue between
/// its two pipes).
pub fn pipe_on(wq: Arc<WaitQueue>) -> (Arc<dyn FileLike>, Arc<dyn FileLike>) {
    let inner = PipeInner::new(wq);
    (PipeEnd::new(&inner, false), PipeEnd::new(&inner, true))
}

impl FileLike for PipeEnd {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        if self.write {
            return Err(EBADF);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let p = &self.inner;
        loop {
            {
                let mut q = p.buf.lock();
                if !q.is_empty() {
                    let n = buf.len().min(q.len());
                    let (a, b) = q.as_slices();
                    let k = n.min(a.len());
                    buf[..k].copy_from_slice(&a[..k]);
                    buf[k..n].copy_from_slice(&b[..n - k]);
                    q.drain(..n);
                    drop(q);
                    p.notify();
                    return Ok(n);
                }
            }
            if p.writers.load(Ordering::SeqCst) == 0 {
                return Ok(0);
            }
            if nonblock {
                return Err(EAGAIN);
            }
            let ok = p.wq.wait_interruptible(|| {
                !p.buf.lock().is_empty() || p.writers.load(Ordering::SeqCst) == 0
            });
            if !ok && crate::process::signal::has_pending() {
                return Err(EINTR);
            }
        }
    }

    fn write(&self, buf: &[u8], nonblock: bool) -> KResult<usize> {
        if !self.write {
            return Err(EBADF);
        }
        let p = &self.inner;
        let mut done = 0;
        while done < buf.len() {
            if p.readers.load(Ordering::SeqCst) == 0 {
                crate::process::signal::send_to_current(crate::process::signal::SIGPIPE);
                return if done > 0 { Ok(done) } else { Err(EPIPE) };
            }
            {
                let mut q = p.buf.lock();
                let space = PIPE_CAPACITY - q.len();
                if space > 0 {
                    let n = space.min(buf.len() - done);
                    q.extend(&buf[done..done + n]);
                    done += n;
                    drop(q);
                    p.notify();
                    continue;
                }
            }
            if nonblock {
                return if done > 0 { Ok(done) } else { Err(EAGAIN) };
            }
            let ok = p.wq.wait_interruptible(|| {
                p.buf.lock().len() < PIPE_CAPACITY || p.readers.load(Ordering::SeqCst) == 0
            });
            if !ok && crate::process::signal::has_pending() {
                return if done > 0 { Ok(done) } else { Err(EINTR) };
            }
        }
        Ok(done)
    }

    fn poll(&self) -> u16 {
        let p = &self.inner;
        let len = p.buf.lock().len();
        if self.write {
            let mut r = 0;
            if len < PIPE_CAPACITY {
                r |= POLLOUT;
            }
            if p.readers.load(Ordering::SeqCst) == 0 {
                r |= POLLERR;
            }
            r
        } else {
            let mut r = 0;
            if len > 0 {
                r |= POLLIN;
            }
            if p.writers.load(Ordering::SeqCst) == 0 {
                r |= POLLHUP;
            }
            r
        }
    }

    fn stat(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::Fifo, 0o600);
        m.size = self.inner.buf.lock().len() as u64;
        Ok(m)
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        const FIONREAD: u64 = 0x541B;
        if cmd == FIONREAD {
            let n = self.inner.buf.lock().len() as i32;
            crate::process::uaccess::write_user(arg, &n)?;
            return Ok(0);
        }
        Err(ENOTTY)
    }

    fn wait_queue(&self) -> &WaitQueue {
        &self.inner.wq
    }

    fn close(&self) {
        if self.write {
            self.inner.writers.fetch_sub(1, Ordering::SeqCst);
        } else {
            self.inner.readers.fetch_sub(1, Ordering::SeqCst);
        }
        self.inner.notify();
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A named pipe's shared state; each `open` creates a new end.
pub struct FifoHub {
    inner: Arc<PipeInner>,
}

impl FifoHub {
    pub fn open_end(&self, flags: u32) -> Arc<dyn FileLike> {
        let write = flags & O_ACCMODE != O_RDONLY;
        PipeEnd::new(&self.inner, write)
    }
}

impl FileLike for FifoHub {
    fn read(&self, _buf: &mut [u8], _nb: bool) -> KResult<usize> {
        Err(EBADF)
    }
    fn write(&self, _buf: &[u8], _nb: bool) -> KResult<usize> {
        Err(EBADF)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct Pipe;

impl Pipe {
    pub fn new_fifo() -> Arc<dyn FileLike> {
        Arc::new(FifoHub {
            inner: PipeInner::new(Arc::new(WaitQueue::new())),
        })
    }
}
