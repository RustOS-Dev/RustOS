//! Descriptor syscalls desktop software expects: memfd_create (with
//! seals), inotify, pidfds, close_range and copy_file_range.

use super::SysResult;
use crate::errno::*;
use crate::process::{self, signal, uaccess};
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use crate::vfs::{self, File, FileLike, FileObject, POLLIN, inotify, memfd};
use alloc::sync::{Arc, Weak};
use core::any::Any;
use core::sync::atomic::AtomicU32;

fn cur() -> KResult<Arc<process::Process>> {
    process::current().ok_or(ESRCH)
}

fn install(file: Arc<File>, cloexec: bool) -> SysResult {
    Ok(cur()?.files.lock().install(file, cloexec)? as i64)
}

// ---------------------------------------------------------------------------
// memfd
// ---------------------------------------------------------------------------

const MFD_HUGETLB: u32 = 4;
const MFD_NOEXEC_SEAL: u32 = 8;
const MFD_EXEC: u32 = 0x10;

pub fn memfd_create(name: u64, flags: u32) -> SysResult {
    let known = memfd::MFD_CLOEXEC | memfd::MFD_ALLOW_SEALING | MFD_NOEXEC_SEAL | MFD_EXEC;
    if flags & MFD_HUGETLB != 0 {
        return Err(EINVAL);
    }
    if flags & !known != 0 {
        return Err(EINVAL);
    }
    let name = uaccess::read_cstr(name, 250)?;
    // MFD_NOEXEC_SEAL implies sealing is allowed.
    let flags = if flags & MFD_NOEXEC_SEAL != 0 {
        flags | memfd::MFD_ALLOW_SEALING
    } else {
        flags
    };
    let m = memfd::Memfd::new(flags)?;
    let inode: Arc<dyn vfs::Inode> = m;
    let file = Arc::new(File {
        object: FileObject::Inode(inode.clone()),
        inode: Some(inode),
        offset: Mutex::new(0),
        flags: AtomicU32::new(vfs::O_RDWR),
        // Not an absolute path: memfds have no name in any directory.
        path: alloc::format!("memfd:{} (deleted)", name),
    });
    install(file, flags & memfd::MFD_CLOEXEC != 0)
}

/// The memfd behind `f`, if it is one.
pub fn as_memfd(f: &File) -> Option<&memfd::Memfd> {
    match &f.object {
        FileObject::Inode(i) => i.as_any().downcast_ref::<memfd::Memfd>(),
        _ => None,
    }
}

pub const F_ADD_SEALS: u32 = 1033;
pub const F_GET_SEALS: u32 = 1034;

/// fcntl(F_ADD_SEALS / F_GET_SEALS).
pub fn seals(f: &File, cmd: u32, arg: u64) -> SysResult {
    let m = as_memfd(f).ok_or(EINVAL)?;
    if cmd == F_GET_SEALS {
        return Ok(m.seals() as i64);
    }
    if !f.writable() {
        return Err(EPERM);
    }
    let target = Arc::as_ptr(m.pages()) as *const ();
    m.add_seals(arg as u32, mapped_writable(target))?;
    Ok(0)
}

/// Whether any process has a writable shared mapping of the memfd
/// frames at `target`.
fn mapped_writable(target: *const ()) -> bool {
    use crate::process::vm::{Backing, MAP_SHARED, PROT_WRITE};
    process::all().iter().any(|p| {
        p.vm().is_some_and(|vm| {
            vm.lock().areas.values().any(|a| {
                a.flags & MAP_SHARED != 0
                    && a.prot & PROT_WRITE != 0
                    && matches!(&a.backing, Backing::Shm { obj, .. }
                        if Arc::as_ptr(obj) as *const () == target)
            })
        })
    })
}

// ---------------------------------------------------------------------------
// inotify
// ---------------------------------------------------------------------------

const IN_NONBLOCK: u32 = vfs::O_NONBLOCK;
const IN_CLOEXEC: u32 = vfs::O_CLOEXEC;

pub fn inotify_init1(flags: u32) -> SysResult {
    if flags & !(IN_NONBLOCK | IN_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    let file = File::from_stream(
        inotify::Inotify::new(),
        vfs::O_RDONLY | (flags & IN_NONBLOCK),
        "anon_inode:inotify",
    );
    install(file, flags & IN_CLOEXEC != 0)
}

fn with_inotify<R>(fd: i32, f: impl FnOnce(&inotify::Inotify) -> KResult<R>) -> KResult<R> {
    let file = cur()?.files.lock().get(fd)?;
    let FileObject::Stream(s) = &file.object else {
        return Err(EINVAL);
    };
    f(s.as_any()
        .downcast_ref::<inotify::Inotify>()
        .ok_or(EINVAL)?)
}

pub fn inotify_add_watch(fd: i32, path: u64, mask: u32) -> SysResult {
    let p = cur()?;
    let path = uaccess::read_cstr(path, 4096)?;
    let abs = if path.starts_with('/') {
        vfs::normalize(&path)
    } else {
        p.abs_path(&path)
    };
    with_inotify(fd, |i| i.add_watch(&abs, mask)).map(|wd| wd as i64)
}

pub fn inotify_rm_watch(fd: i32, wd: i32) -> SysResult {
    with_inotify(fd, |i| i.rm_watch(wd)).map(|_| 0)
}

// ---------------------------------------------------------------------------
// pidfd
// ---------------------------------------------------------------------------

/// Woken whenever a process exits (pidfds poll readable then).
pub static EXIT_WQ: WaitQueue = WaitQueue::new();

struct PidFd {
    proc_: Weak<process::Process>,
    pid: u32,
}

impl PidFd {
    fn exited(&self) -> bool {
        self.proc_
            .upgrade()
            .is_none_or(|p| p.exit_status.lock().is_some())
    }
}

impl FileLike for PidFd {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn poll(&self) -> u16 {
        if self.exited() { POLLIN } else { 0 }
    }
    fn wait_queue(&self) -> &WaitQueue {
        &EXIT_WQ
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

const PIDFD_NONBLOCK: u32 = vfs::O_NONBLOCK;

pub fn pidfd_open(pid: i32, flags: u32) -> SysResult {
    if flags & !PIDFD_NONBLOCK != 0 || pid <= 0 {
        return Err(EINVAL);
    }
    // A zombie still gets a pidfd (it polls readable at once).
    let p = process::find(pid as u32).ok_or(ESRCH)?;
    let obj = Arc::new(PidFd {
        proc_: Arc::downgrade(&p),
        pid: pid as u32,
    });
    let file = File::from_stream(
        obj,
        vfs::O_RDWR | (flags & PIDFD_NONBLOCK),
        "anon_inode:[pidfd]",
    );
    // pidfds are always close-on-exec.
    install(file, true)
}

/// The pid a pidfd refers to (waitid's P_PIDFD).
pub fn pidfd_pid(fd: i32) -> KResult<u32> {
    let file = cur()?.files.lock().get(fd)?;
    let FileObject::Stream(s) = &file.object else {
        return Err(EBADF);
    };
    let pf = s.as_any().downcast_ref::<PidFd>().ok_or(EBADF)?;
    Ok(pf.pid)
}

pub fn pidfd_send_signal(fd: i32, sig: u32, info: u64, flags: u32) -> SysResult {
    if flags != 0 || info != 0 {
        return Err(EINVAL);
    }
    if sig as usize >= signal::NSIG {
        return Err(EINVAL);
    }
    let file = cur()?.files.lock().get(fd)?;
    let FileObject::Stream(s) = &file.object else {
        return Err(EBADF);
    };
    let pf = s.as_any().downcast_ref::<PidFd>().ok_or(EBADF)?;
    let p = pf.proc_.upgrade().ok_or(ESRCH)?;
    if p.pid != pf.pid || p.exit_status.lock().is_some() {
        return Err(ESRCH);
    }
    if sig != 0 {
        signal::send(&p, sig);
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// close_range, copy_file_range
// ---------------------------------------------------------------------------

const CLOSE_RANGE_UNSHARE: u32 = 1 << 1;
const CLOSE_RANGE_CLOEXEC: u32 = 1 << 2;

pub fn close_range(first: u32, last: u32, flags: u32) -> SysResult {
    if flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 || first > last {
        return Err(EINVAL);
    }
    let p = cur()?;
    let closed = {
        let mut t = p.files.lock();
        let mut closed = alloc::vec::Vec::new();
        if let Some(top) = t.len().checked_sub(1) {
            for fd in first as usize..=(last as usize).min(top) {
                if flags & CLOSE_RANGE_CLOEXEC != 0 {
                    let _ = t.set_cloexec(fd as i32, true);
                } else if let Ok(f) = t.close(fd as i32) {
                    closed.push(f);
                }
            }
        }
        closed
    };
    // Files are released outside the table lock.
    drop(closed);
    Ok(0)
}

pub fn copy_file_range(
    fd_in: i32,
    off_in: u64,
    fd_out: i32,
    off_out: u64,
    len: u64,
    flags: u32,
) -> SysResult {
    if flags != 0 {
        return Err(EINVAL);
    }
    let (fin, fout) = {
        let p = cur()?;
        let t = p.files.lock();
        (t.get(fd_in)?, t.get(fd_out)?)
    };
    if !fin.readable() || !fout.writable() {
        return Err(EBADF);
    }
    if fout.flags() & vfs::O_APPEND != 0 {
        return Err(EBADF);
    }
    let (FileObject::Inode(_), FileObject::Inode(_)) = (&fin.object, &fout.object) else {
        return Err(EINVAL);
    };
    let mut pos_in = if off_in != 0 {
        uaccess::read_user::<u64>(off_in)?
    } else {
        *fin.offset.lock()
    };
    let mut pos_out = if off_out != 0 {
        uaccess::read_user::<u64>(off_out)?
    } else {
        *fout.offset.lock()
    };
    let mut buf = alloc::vec![0u8; 64 * 1024];
    let mut done = 0u64;
    while done < len {
        let want = ((len - done) as usize).min(buf.len());
        let n = fin.pread(pos_in, &mut buf[..want])?;
        if n == 0 {
            break;
        }
        let w = fout.pwrite(pos_out, &buf[..n])?;
        pos_in += w as u64;
        pos_out += w as u64;
        done += w as u64;
        if w < n {
            break;
        }
    }
    if off_in != 0 {
        uaccess::write_user(off_in, &pos_in)?;
    } else {
        *fin.offset.lock() = pos_in;
    }
    if off_out != 0 {
        uaccess::write_user(off_out, &pos_out)?;
    } else {
        *fout.offset.lock() = pos_out;
    }
    if done > 0 {
        inotify::file_event(&fout.path, fout.inode.as_ref(), inotify::IN_MODIFY);
    }
    Ok(done as i64)
}
