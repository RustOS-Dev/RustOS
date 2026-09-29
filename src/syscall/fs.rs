//! File and filesystem syscalls.

use super::SysResult;
use crate::errno::*;
use crate::process::{self, Process, uaccess};
use crate::vfs::{self, File, FileType, Metadata};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

pub const AT_FDCWD: i32 = -100;
pub const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
pub const AT_REMOVEDIR: u32 = 0x200;
pub const AT_EMPTY_PATH: u32 = 0x1000;

const MAX_IO: usize = 1 << 20;

fn cur() -> KResult<Arc<Process>> {
    process::current().ok_or(ESRCH)
}

fn file(fd: i32) -> KResult<Arc<File>> {
    cur()?.files.lock().get(fd)
}

/// Resolve a path argument relative to `dirfd`.
fn path_at(dirfd: i32, ptr: u64) -> KResult<String> {
    let path = uaccess::read_cstr(ptr, 4096)?;
    resolve_at(dirfd, &path)
}

fn resolve_at(dirfd: i32, path: &str) -> KResult<String> {
    if path.starts_with('/') {
        return Ok(vfs::normalize(path));
    }
    let p = cur()?;
    if dirfd == AT_FDCWD {
        return Ok(p.abs_path(path));
    }
    let f = p.files.lock().get(dirfd)?;
    Ok(vfs::absolute(&f.path, path))
}

pub fn read(fd: i32, buf: u64, len: u64) -> SysResult {
    let f = file(fd)?;
    let len = (len as usize).min(MAX_IO);
    if len == 0 {
        return Ok(0);
    }
    uaccess::copy_to_user(buf, &[])?;
    let mut tmp = alloc::vec![0u8; len];
    let n = f.read(&mut tmp)?;
    uaccess::copy_to_user(buf, &tmp[..n])?;
    Ok(n as i64)
}

pub fn write(fd: i32, buf: u64, len: u64) -> SysResult {
    let f = file(fd)?;
    let len = (len as usize).min(MAX_IO);
    let data = uaccess::read_bytes(buf, len)?;
    Ok(f.write(&data)? as i64)
}

pub fn pread(fd: i32, buf: u64, len: u64, off: u64) -> SysResult {
    let f = file(fd)?;
    let mut tmp = alloc::vec![0u8; (len as usize).min(MAX_IO)];
    let n = f.pread(off, &mut tmp)?;
    uaccess::copy_to_user(buf, &tmp[..n])?;
    Ok(n as i64)
}

pub fn pwrite(fd: i32, buf: u64, len: u64, off: u64) -> SysResult {
    let f = file(fd)?;
    let data = uaccess::read_bytes(buf, (len as usize).min(MAX_IO))?;
    Ok(f.pwrite(off, &data)? as i64)
}

fn iovecs(iov: u64, cnt: u64) -> KResult<Vec<(u64, u64)>> {
    if cnt > 1024 {
        return Err(EINVAL);
    }
    (0..cnt)
        .map(|i| {
            let base: u64 = uaccess::read_user(iov + i * 16)?;
            let len: u64 = uaccess::read_user(iov + i * 16 + 8)?;
            Ok((base, len))
        })
        .collect()
}

pub fn readv(fd: i32, iov: u64, cnt: u64) -> SysResult {
    let mut total = 0i64;
    for (base, len) in iovecs(iov, cnt)? {
        if len == 0 {
            continue;
        }
        let n = read(fd, base, len)?;
        total += n;
        if (n as u64) < len {
            break;
        }
    }
    Ok(total)
}

pub fn writev(fd: i32, iov: u64, cnt: u64) -> SysResult {
    let mut total = 0i64;
    for (base, len) in iovecs(iov, cnt)? {
        if len == 0 {
            continue;
        }
        let n = write(fd, base, len)?;
        total += n;
        if (n as u64) < len {
            break;
        }
    }
    Ok(total)
}

pub fn openat(dirfd: i32, path: u64, flags: u32, mode: u32) -> SysResult {
    let p = cur()?;
    let abs = path_at(dirfd, path)?;
    let mode = mode & !p.umask.load(Ordering::SeqCst);
    let f = vfs::open(&abs, flags, mode)?;
    // Opening a TTY without O_NOCTTY in a session leader gives it a
    // controlling terminal.
    let fd = p.files.lock().install(f, flags & vfs::O_CLOEXEC != 0)?;
    Ok(fd as i64)
}

pub fn close(fd: i32) -> SysResult {
    let f = cur()?.files.lock().close(fd)?;
    drop(f);
    Ok(0)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Stat {
    dev: u64,
    ino: u64,
    nlink: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    _pad0: u32,
    rdev: u64,
    size: i64,
    blksize: i64,
    blocks: i64,
    atime: i64,
    atime_nsec: i64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
    _reserved: [i64; 3],
}

fn to_stat(m: &Metadata) -> Stat {
    Stat {
        dev: m.dev,
        ino: m.ino,
        nlink: m.nlink as u64,
        mode: m.kind.mode_bits() | (m.mode & 0o7777),
        uid: m.uid,
        gid: m.gid,
        rdev: m.rdev,
        size: m.size as i64,
        blksize: m.blksize as i64,
        blocks: m.blocks as i64,
        atime: m.atime as i64,
        mtime: m.mtime as i64,
        ctime: m.ctime as i64,
        ..Default::default()
    }
}

pub fn fstat(fd: i32, buf: u64) -> SysResult {
    let m = file(fd)?.stat()?;
    uaccess::write_user(buf, &to_stat(&m))?;
    Ok(0)
}

fn stat_path(dirfd: i32, path: u64, flags: u32) -> KResult<Metadata> {
    let raw = uaccess::read_cstr(path, 4096)?;
    if raw.is_empty() && flags & AT_EMPTY_PATH != 0 {
        return file(dirfd)?.stat();
    }
    let abs = resolve_at(dirfd, &raw)?;
    let inode = if flags & AT_SYMLINK_NOFOLLOW != 0 {
        vfs::lookup_nofollow(&abs)?
    } else {
        vfs::lookup(&abs)?
    };
    let mut m = inode.metadata()?;
    // Device nodes report the size of the device behind them.
    if let Ok(Some(s)) = inode.open(0)
        && let Some(sz) = s.size()
    {
        m.size = sz;
    }
    Ok(m)
}

pub fn fstatat(dirfd: i32, path: u64, buf: u64, flags: u32) -> SysResult {
    let m = stat_path(dirfd, path, flags)?;
    uaccess::write_user(buf, &to_stat(&m))?;
    Ok(0)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct StatxTs {
    sec: i64,
    nsec: u32,
    _r: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Statx {
    mask: u32,
    blksize: u32,
    attributes: u64,
    nlink: u32,
    uid: u32,
    gid: u32,
    mode: u16,
    _pad: u16,
    ino: u64,
    size: u64,
    blocks: u64,
    attributes_mask: u64,
    atime: StatxTs,
    btime: StatxTs,
    ctime: StatxTs,
    mtime: StatxTs,
    rdev_major: u32,
    rdev_minor: u32,
    dev_major: u32,
    dev_minor: u32,
    _spare: [u64; 14],
}

pub fn statx(dirfd: i32, path: u64, flags: u32, buf: u64) -> SysResult {
    let m = stat_path(dirfd, path, flags)?;
    let sx = Statx {
        mask: 0x7ff,
        blksize: m.blksize,
        nlink: m.nlink,
        uid: m.uid,
        gid: m.gid,
        mode: (m.kind.mode_bits() | (m.mode & 0o7777)) as u16,
        ino: m.ino,
        size: m.size,
        blocks: m.blocks,
        atime: StatxTs {
            sec: m.atime as i64,
            ..Default::default()
        },
        btime: StatxTs {
            sec: m.ctime as i64,
            ..Default::default()
        },
        ctime: StatxTs {
            sec: m.ctime as i64,
            ..Default::default()
        },
        mtime: StatxTs {
            sec: m.mtime as i64,
            ..Default::default()
        },
        rdev_major: (m.rdev >> 8) as u32,
        rdev_minor: (m.rdev & 0xff) as u32,
        ..Default::default()
    };
    uaccess::write_user(buf, &sx)?;
    Ok(0)
}

pub fn lseek(fd: i32, off: i64, whence: u32) -> SysResult {
    Ok(file(fd)?.seek(off, whence)? as i64)
}

pub fn ioctl(fd: i32, cmd: u64, arg: u64) -> SysResult {
    const FIONBIO: u64 = 0x5421;
    const FIOCLEX: u64 = 0x5451;
    const FIONCLEX: u64 = 0x5450;
    let f = file(fd)?;
    match cmd {
        FIONBIO => {
            let v: i32 = uaccess::read_user(arg)?;
            if v != 0 {
                f.flags.fetch_or(vfs::O_NONBLOCK, Ordering::SeqCst);
            } else {
                f.flags.fetch_and(!vfs::O_NONBLOCK, Ordering::SeqCst);
            }
            Ok(0)
        }
        FIOCLEX | FIONCLEX => {
            cur()?.files.lock().set_cloexec(fd, cmd == FIOCLEX)?;
            Ok(0)
        }
        _ => f.ioctl(cmd, arg),
    }
}

pub fn faccessat(dirfd: i32, path: u64, _mode: u32) -> SysResult {
    let abs = path_at(dirfd, path)?;
    vfs::lookup(&abs)?;
    Ok(0)
}

pub fn pipe2(fds: u64, flags: u32) -> SysResult {
    let (r, w) = vfs::pipe::pipe();
    let fl = flags & vfs::O_NONBLOCK;
    let rf = File::from_stream(r, vfs::O_RDONLY | fl, "pipe:");
    let wf = File::from_stream(w, vfs::O_WRONLY | fl, "pipe:");
    let p = cur()?;
    let cloexec = flags & vfs::O_CLOEXEC != 0;
    let (a, b) = {
        let mut t = p.files.lock();
        let a = t.install(rf, cloexec)?;
        let b = match t.install(wf, cloexec) {
            Ok(b) => b,
            Err(e) => {
                let _ = t.close(a);
                return Err(e);
            }
        };
        (a, b)
    };
    uaccess::write_user(fds, &[a, b])?;
    Ok(0)
}

pub fn dup(fd: i32) -> SysResult {
    let p = cur()?;
    let mut t = p.files.lock();
    let f = t.get(fd)?;
    Ok(t.install(f, false)? as i64)
}

pub fn dup3(old: i32, new: i32, flags: u32, allow_same: bool) -> SysResult {
    let p = cur()?;
    let mut t = p.files.lock();
    let f = t.get(old)?;
    if old == new {
        return if allow_same {
            Ok(new as i64)
        } else {
            Err(EINVAL)
        };
    }
    if !(0..crate::process::fd::MAX_FDS as i32).contains(&new) {
        return Err(EBADF);
    }
    t.install_at(new as usize, f, flags & vfs::O_CLOEXEC != 0);
    Ok(new as i64)
}

pub fn fcntl(fd: i32, cmd: u32, arg: u64) -> SysResult {
    const F_DUPFD: u32 = 0;
    const F_GETFD: u32 = 1;
    const F_SETFD: u32 = 2;
    const F_GETFL: u32 = 3;
    const F_SETFL: u32 = 4;
    const F_GETLK: u32 = 5;
    const F_SETLK: u32 = 6;
    const F_SETLKW: u32 = 7;
    const F_DUPFD_CLOEXEC: u32 = 1030;
    let p = cur()?;
    let mut t = p.files.lock();
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let f = t.get(fd)?;
            Ok(t.install_from(arg as usize, f, cmd == F_DUPFD_CLOEXEC)? as i64)
        }
        F_GETFD => Ok(t.entry(fd)?.cloexec as i64),
        F_SETFD => {
            t.set_cloexec(fd, arg & 1 != 0)?;
            Ok(0)
        }
        F_GETFL => Ok(t.get(fd)?.flags() as i64),
        F_SETFL => {
            let f = t.get(fd)?;
            let keep = f.flags() & !(vfs::O_APPEND | vfs::O_NONBLOCK);
            f.flags.store(
                keep | (arg as u32 & (vfs::O_APPEND | vfs::O_NONBLOCK)),
                Ordering::SeqCst,
            );
            Ok(0)
        }
        F_GETLK | F_SETLK | F_SETLKW => {
            t.get(fd)?;
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}

pub fn fsync(fd: i32) -> SysResult {
    let f = file(fd)?;
    if let Some(i) = &f.inode {
        i.sync()?;
    }
    Ok(0)
}

pub fn truncate(path: u64, len: u64) -> SysResult {
    let abs = path_at(AT_FDCWD, path)?;
    let i = vfs::lookup(&abs)?;
    i.truncate(len)?;
    crate::mm::pagecache::truncate(&i, len);
    Ok(0)
}

pub fn ftruncate(fd: i32, len: u64) -> SysResult {
    let f = file(fd)?;
    if !f.writable() {
        return Err(EINVAL);
    }
    let i = f.inode.as_ref().ok_or(EINVAL)?;
    i.truncate(len)?;
    crate::mm::pagecache::truncate(i, len);
    Ok(0)
}

/// fallocate(2): preallocate (mode 0 or FALLOC_FL_KEEP_SIZE). Filesystems
/// without preallocation extend the file (sparse) for mode 0.
pub fn fallocate(fd: i32, mode: u32, off: u64, len: u64) -> SysResult {
    const KEEP_SIZE: u32 = 1;
    let f = file(fd)?;
    if !f.writable() {
        return Err(EBADF);
    }
    if len == 0 {
        return Err(EINVAL);
    }
    let i = f.inode.as_ref().ok_or(ENODEV)?;
    match i.fallocate(mode, off, len) {
        Err(EOPNOTSUPP) if mode == 0 => {
            let end = off.checked_add(len).ok_or(EFBIG)?;
            if end > i.metadata()?.size {
                i.truncate(end)?;
            }
            Ok(0)
        }
        Err(EOPNOTSUPP) if mode == KEEP_SIZE => Ok(0),
        r => r.map(|_| 0),
    }
}

pub fn getdents64(fd: i32, buf: u64, len: u64) -> SysResult {
    let f = file(fd)?;
    let dir = f.dir_inode()?.clone();
    let mut entries = alloc::vec![
        vfs::DirEntry {
            name: String::from("."),
            ino: 1,
            kind: FileType::Directory
        },
        vfs::DirEntry {
            name: String::from(".."),
            ino: 1,
            kind: FileType::Directory
        },
    ];
    entries.extend(dir.readdir()?);
    // Mount points that exist only in the mount table.
    let mut off = f.offset.lock();
    let mut out: Vec<u8> = Vec::new();
    let mut idx = *off as usize;
    while idx < entries.len() {
        let e = &entries[idx];
        let reclen = (19 + e.name.len() + 1).next_multiple_of(8);
        if out.len() + reclen > len as usize {
            break;
        }
        let mut rec = alloc::vec![0u8; reclen];
        rec[0..8].copy_from_slice(&e.ino.max(1).to_le_bytes());
        rec[8..16].copy_from_slice(&((idx + 1) as i64).to_le_bytes());
        rec[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
        rec[18] = e.kind.dirent_type();
        rec[19..19 + e.name.len()].copy_from_slice(e.name.as_bytes());
        out.extend_from_slice(&rec);
        idx += 1;
    }
    if out.is_empty() && idx < entries.len() {
        return Err(EINVAL);
    }
    *off = idx as u64;
    uaccess::copy_to_user(buf, &out)?;
    Ok(out.len() as i64)
}

pub fn getcwd(buf: u64, size: u64) -> SysResult {
    let cwd = cur()?.cwd.lock().clone();
    if cwd.len() + 1 > size as usize {
        return Err(ERANGE);
    }
    let mut b = cwd.into_bytes();
    b.push(0);
    uaccess::copy_to_user(buf, &b)?;
    Ok(b.len() as i64)
}

pub fn chdir(path: u64) -> SysResult {
    let p = cur()?;
    let abs = path_at(AT_FDCWD, path)?;
    let canon = vfs::canonicalize(&abs)?;
    if vfs::stat(&canon)?.kind != FileType::Directory {
        return Err(ENOTDIR);
    }
    *p.cwd.lock() = canon;
    Ok(0)
}

pub fn fchdir(fd: i32) -> SysResult {
    let f = file(fd)?;
    f.dir_inode()?;
    *cur()?.cwd.lock() = f.path.clone();
    Ok(0)
}

pub fn renameat(od: i32, op: u64, nd: i32, np: u64) -> SysResult {
    let a = path_at(od, op)?;
    let b = path_at(nd, np)?;
    vfs::rename(&a, &b)?;
    Ok(0)
}

pub fn mkdirat(dirfd: i32, path: u64, mode: u32) -> SysResult {
    let p = cur()?;
    let abs = path_at(dirfd, path)?;
    vfs::mkdir(&abs, mode & !p.umask.load(Ordering::SeqCst) & 0o7777)?;
    Ok(0)
}

pub fn unlinkat(dirfd: i32, path: u64, flags: u32) -> SysResult {
    let abs = path_at(dirfd, path)?;
    if flags & AT_REMOVEDIR != 0 {
        vfs::rmdir(&abs)?;
    } else {
        vfs::unlink(&abs)?;
    }
    Ok(0)
}

pub fn linkat(od: i32, op: u64, nd: i32, np: u64) -> SysResult {
    let a = path_at(od, op)?;
    let b = path_at(nd, np)?;
    vfs::link(&a, &b)?;
    Ok(0)
}

pub fn symlinkat(target: u64, dirfd: i32, linkpath: u64) -> SysResult {
    let t = uaccess::read_cstr(target, 4096)?;
    let l = path_at(dirfd, linkpath)?;
    vfs::symlink(&t, &l)?;
    Ok(0)
}

pub fn readlinkat(dirfd: i32, path: u64, buf: u64, size: u64) -> SysResult {
    let abs = path_at(dirfd, path)?;
    let target = vfs::lookup_nofollow(&abs)?.readlink()?;
    let n = target.len().min(size as usize);
    uaccess::copy_to_user(buf, &target.as_bytes()[..n])?;
    Ok(n as i64)
}

pub fn fchmodat(dirfd: i32, path: u64, mode: u32) -> SysResult {
    let abs = path_at(dirfd, path)?;
    vfs::lookup(&abs)?.chmod(mode)?;
    Ok(0)
}

pub fn fchmod(fd: i32, mode: u32) -> SysResult {
    file(fd)?.inode.as_ref().ok_or(EINVAL)?.chmod(mode)?;
    Ok(0)
}

pub fn fchownat(dirfd: i32, path: u64, uid: u32, gid: u32) -> SysResult {
    let abs = path_at(dirfd, path)?;
    vfs::lookup(&abs)?.chown(uid, gid)?;
    Ok(0)
}

pub fn utimensat(dirfd: i32, path: u64, times: u64) -> SysResult {
    let now = crate::time::unix_time();
    let (a, m) = if times == 0 {
        (Some(now), Some(now))
    } else {
        let ts: [(i64, i64); 2] = uaccess::read_user(times)?;
        const UTIME_NOW: i64 = (1 << 30) - 1;
        const UTIME_OMIT: i64 = (1 << 30) - 2;
        let conv = |t: (i64, i64)| match t.1 {
            UTIME_NOW => Some(now),
            UTIME_OMIT => None,
            _ => Some(t.0 as u64),
        };
        (conv(ts[0]), conv(ts[1]))
    };
    let inode = if path == 0 {
        file(dirfd)?.inode.clone().ok_or(EINVAL)?
    } else {
        vfs::lookup(&path_at(dirfd, path)?)?
    };
    inode.set_times(a, m)?;
    Ok(0)
}

pub fn mknod(path: u64, mode: u32) -> SysResult {
    let abs = path_at(AT_FDCWD, path)?;
    let kind = match mode & 0o170000 {
        0o010000 => FileType::Fifo,
        0o100000 | 0 => FileType::Regular,
        _ => return Err(EPERM),
    };
    let (dir, name) = vfs::lookup_parent(&abs)?;
    dir.create(&name, kind, mode & 0o7777)?;
    Ok(0)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct StatFsBuf {
    f_type: i64,
    f_bsize: i64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_fsid: [i32; 2],
    f_namelen: i64,
    f_frsize: i64,
    f_flags: i64,
    f_spare: [i64; 4],
}

fn write_statfs(path: &str, buf: u64) -> SysResult {
    let fs = vfs::mount_fs(path).ok_or(ENOENT)?;
    let s = fs.statfs();
    let b = StatFsBuf {
        f_type: s.fs_type as i64,
        f_bsize: s.block_size as i64,
        f_blocks: s.blocks,
        f_bfree: s.blocks_free,
        f_bavail: s.blocks_free,
        f_files: s.files,
        f_ffree: s.files_free,
        f_namelen: s.name_max as i64,
        f_frsize: s.block_size as i64,
        ..Default::default()
    };
    uaccess::write_user(buf, &b)?;
    Ok(0)
}

pub fn statfs(path: u64, buf: u64) -> SysResult {
    let abs = path_at(AT_FDCWD, path)?;
    vfs::lookup(&abs)?;
    write_statfs(&abs, buf)
}

pub fn fstatfs(fd: i32, buf: u64) -> SysResult {
    let f = file(fd)?;
    write_statfs(&f.path, buf)
}

// ---------------------------------------------------------------------------
// poll / select
// ---------------------------------------------------------------------------

pub(crate) fn wait_ready(
    timeout_ns: Option<u64>,
    mut check: impl FnMut() -> KResult<usize>,
) -> KResult<usize> {
    let deadline = timeout_ns.map(|t| crate::time::nanos() + t);
    loop {
        let epoch = vfs::poll_epoch();
        let n = check()?;
        if n > 0 {
            return Ok(n);
        }
        if deadline.is_some_and(|d| crate::time::nanos() >= d) {
            return Ok(0);
        }
        if process::signal::has_pending() {
            return Err(EINTR);
        }
        let changed = || vfs::poll_epoch() != epoch;
        match deadline {
            Some(d) => {
                let now = crate::time::nanos();
                let ms = (d.saturating_sub(now)).div_ceil(1_000_000).max(1);
                // Poll at least every 50 ms for sources that do not notify.
                vfs::POLL_WQ.wait_timeout(ms.min(50), changed);
            }
            None => {
                vfs::POLL_WQ.wait_timeout(50, changed);
            }
        }
    }
}

pub fn poll(fds: u64, nfds: u64, timeout_ms: i64) -> SysResult {
    if nfds > 4096 {
        return Err(EINVAL);
    }
    let p = cur()?;
    let mut pfds: Vec<(i32, i16, i16)> = (0..nfds)
        .map(|i| {
            let raw: [u8; 8] = uaccess::read_user(fds + i * 8)?;
            Ok((
                i32::from_le_bytes(raw[0..4].try_into().unwrap()),
                i16::from_le_bytes(raw[4..6].try_into().unwrap()),
                0,
            ))
        })
        .collect::<KResult<_>>()?;
    let timeout = if timeout_ms < 0 {
        None
    } else {
        Some(timeout_ms as u64 * 1_000_000)
    };
    let n = wait_ready(timeout, || {
        let mut ready = 0;
        for e in pfds.iter_mut() {
            e.2 = 0;
            if e.0 < 0 {
                continue;
            }
            let rev = match p.files.lock().get(e.0) {
                Ok(f) => f.poll() & (e.1 as u16 | vfs::POLLERR | vfs::POLLHUP),
                Err(_) => vfs::POLLNVAL,
            };
            if rev != 0 {
                e.2 = rev as i16;
                ready += 1;
            }
        }
        Ok(ready)
    })?;
    for (i, e) in pfds.iter().enumerate() {
        uaccess::write_user(fds + i as u64 * 8 + 6, &e.2)?;
    }
    Ok(n as i64)
}

pub fn ppoll(fds: u64, nfds: u64, ts: u64) -> SysResult {
    let timeout = if ts == 0 {
        -1
    } else {
        let t: [i64; 2] = uaccess::read_user(ts)?;
        t[0] * 1000 + t[1] / 1_000_000
    };
    poll(fds, nfds, timeout)
}

pub fn select(nfds: i32, rd: u64, wr: u64, ex: u64, tv: u64, pselect: bool) -> SysResult {
    if !(0..=1024).contains(&nfds) {
        return Err(EINVAL);
    }
    let words = (nfds as usize).div_ceil(64);
    let read_set = |a: u64| -> KResult<Vec<u64>> {
        if a == 0 {
            return Ok(alloc::vec![0; words]);
        }
        (0..words)
            .map(|i| uaccess::read_user::<u64>(a + i as u64 * 8))
            .collect()
    };
    let (rin, win, ein) = (read_set(rd)?, read_set(wr)?, read_set(ex)?);
    let timeout = if tv == 0 {
        None
    } else {
        let t: [i64; 2] = uaccess::read_user(tv)?;
        let ns = if pselect {
            t[1] as u64
        } else {
            t[1] as u64 * 1000
        };
        Some(t[0] as u64 * 1_000_000_000 + ns)
    };
    let p = cur()?;
    let (mut rout, mut wout, mut eout) = (
        alloc::vec![0u64; words],
        alloc::vec![0u64; words],
        alloc::vec![0u64; words],
    );
    let n = wait_ready(timeout, || {
        let mut ready = 0;
        for fd in 0..nfds as usize {
            let (w, b) = (fd / 64, 1u64 << (fd % 64));
            if (rin[w] | win[w] | ein[w]) & b == 0 {
                continue;
            }
            let ev = p
                .files
                .lock()
                .get(fd as i32)
                .map(|f| f.poll())
                .map_err(|_| EBADF)?;
            rout[w] &= !b;
            wout[w] &= !b;
            eout[w] &= !b;
            if rin[w] & b != 0 && ev & (vfs::POLLIN | vfs::POLLHUP | vfs::POLLERR) != 0 {
                rout[w] |= b;
                ready += 1;
            }
            if win[w] & b != 0 && ev & (vfs::POLLOUT | vfs::POLLERR) != 0 {
                wout[w] |= b;
                ready += 1;
            }
            if ein[w] & b != 0 && ev & vfs::POLLPRI != 0 {
                eout[w] |= b;
                ready += 1;
            }
        }
        Ok(ready)
    })?;
    for (a, v) in [(rd, &rout), (wr, &wout), (ex, &eout)] {
        if a != 0 {
            for (i, w) in v.iter().enumerate() {
                uaccess::write_user(a + i as u64 * 8, w)?;
            }
        }
    }
    Ok(n as i64)
}

pub fn sendfile(out_fd: i32, in_fd: i32, offset: u64, count: u64) -> SysResult {
    let src = file(in_fd)?;
    let dst = file(out_fd)?;
    let mut buf = alloc::vec![0u8; (count as usize).min(64 * 1024)];
    let n = if offset != 0 {
        let off: u64 = uaccess::read_user(offset)?;
        let n = src.pread(off, &mut buf)?;
        uaccess::write_user(offset, &(off + n as u64))?;
        n
    } else {
        src.read(&mut buf)?
    };
    Ok(dst.write(&buf[..n])? as i64)
}

pub fn mount(source: u64, target: u64, fstype: u64, _flags: u64) -> SysResult {
    let src = if source != 0 {
        uaccess::read_cstr(source, 4096)?
    } else {
        String::new()
    };
    let tgt = path_at(AT_FDCWD, target)?;
    let ty = if fstype != 0 {
        uaccess::read_cstr(fstype, 64)?
    } else {
        String::from("auto")
    };
    let src_abs = if src.starts_with('/') {
        src.clone()
    } else {
        cur()?.abs_path(&src)
    };
    crate::fs::mount_by_type(&ty, &src_abs, &tgt)?;
    Ok(0)
}

pub fn umount(target: u64) -> SysResult {
    let tgt = path_at(AT_FDCWD, target)?;
    vfs::umount(&tgt)?;
    Ok(0)
}
