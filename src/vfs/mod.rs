//! Virtual file system.
//!
//! * [`Inode`]: a file, directory, symlink or device node inside a
//!   [`FileSystem`]. Inodes offer offset-based `read_at`/`write_at`.
//! * [`FileLike`]: objects with stream semantics (pipes, sockets, TTYs,
//!   character devices) that an open file can wrap instead of an inode.
//! * [`File`]: an open file description (object + offset + status flags),
//!   shared between file descriptors after `dup`/`fork`.
//! * A mount table and path resolution that follows `..`, mount points and
//!   symbolic links.

pub mod devfs;
pub mod inotify;
pub mod memfd;
pub mod pipe;
pub mod procfs;
pub mod sysfs;
pub mod tmpfs;

use crate::errno::*;
use crate::sched::WaitQueue;
use crate::sync::{Mutex, RwLock};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    CharDevice,
    BlockDevice,
    Fifo,
    Socket,
}

impl FileType {
    /// `S_IFMT` bits.
    pub fn mode_bits(self) -> u32 {
        match self {
            FileType::Regular => 0o100000,
            FileType::Directory => 0o040000,
            FileType::Symlink => 0o120000,
            FileType::CharDevice => 0o020000,
            FileType::BlockDevice => 0o060000,
            FileType::Fifo => 0o010000,
            FileType::Socket => 0o140000,
        }
    }

    /// `d_type` value for getdents64.
    pub fn dirent_type(self) -> u8 {
        match self {
            FileType::Fifo => 1,
            FileType::CharDevice => 2,
            FileType::Directory => 4,
            FileType::BlockDevice => 6,
            FileType::Regular => 8,
            FileType::Symlink => 10,
            FileType::Socket => 12,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Metadata {
    pub dev: u64,
    pub ino: u64,
    pub kind: FileType,
    /// Permission bits (without the file-type bits).
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub blksize: u32,
    pub blocks: u64,
    pub rdev: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
}

impl Metadata {
    pub fn new(kind: FileType, mode: u32) -> Metadata {
        Metadata {
            dev: 0,
            ino: 0,
            kind,
            mode,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: 0,
            blksize: 4096,
            blocks: 0,
            rdev: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: FileType,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StatFs {
    pub fs_type: u64,
    pub block_size: u64,
    pub blocks: u64,
    pub blocks_free: u64,
    pub files: u64,
    pub files_free: u64,
    pub name_max: u64,
}

/// Usage and limits of one quota id (Linux `struct if_dqblk`; block
/// limits in 1 KiB units, space in bytes).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct DiskQuota {
    pub bhard: u64,
    pub bsoft: u64,
    pub space: u64,
    pub ihard: u64,
    pub isoft: u64,
    pub inodes: u64,
    pub btime: u64,
    pub itime: u64,
}

pub const QIF_BLIMITS: u32 = 1;
pub const QIF_ILIMITS: u32 = 4;

/// quotactl operations.
pub enum QuotaOp {
    Get,
    /// New limits and the QIF_* fields to take from them.
    Set(DiskQuota, u32),
    Sync,
}

pub trait FileSystem: Send + Sync {
    fn root(&self) -> Arc<dyn Inode>;
    fn name(&self) -> &'static str;
    fn sync(&self) -> KResult<()> {
        Ok(())
    }
    fn statfs(&self) -> StatFs {
        StatFs::default()
    }
    fn read_only(&self) -> bool {
        false
    }
    /// Called when the filesystem is unmounted: leave it clean on disk.
    fn unmount(&self) -> KResult<()> {
        self.sync()
    }
    /// quotactl for quota type `kind` (0 user, 1 group, 2 project).
    fn quota(&self, _op: QuotaOp, _kind: u32, _id: u32) -> KResult<Option<DiskQuota>> {
        Err(ENOSYS)
    }
}

/// A node in a filesystem. Unsupported operations default to sensible errors.
pub trait Inode: Send + Sync + Any {
    fn metadata(&self) -> KResult<Metadata>;

    fn lookup(&self, _name: &str) -> KResult<Arc<dyn Inode>> {
        Err(ENOTDIR)
    }
    fn create(&self, _name: &str, _kind: FileType, _mode: u32) -> KResult<Arc<dyn Inode>> {
        Err(ENOTDIR)
    }
    fn link(&self, _name: &str, _target: &Arc<dyn Inode>) -> KResult<()> {
        Err(EPERM)
    }
    fn unlink(&self, _name: &str) -> KResult<()> {
        Err(ENOTDIR)
    }
    fn rmdir(&self, _name: &str) -> KResult<()> {
        Err(ENOTDIR)
    }
    /// Rename `old` in this directory to `new` in `new_dir` (same filesystem).
    fn rename(&self, _old: &str, _new_dir: &Arc<dyn Inode>, _new: &str) -> KResult<()> {
        Err(EPERM)
    }
    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        Err(ENOTDIR)
    }
    fn read_at(&self, _off: u64, _buf: &mut [u8]) -> KResult<usize> {
        Err(EISDIR)
    }
    fn write_at(&self, _off: u64, _buf: &[u8]) -> KResult<usize> {
        Err(EISDIR)
    }
    /// Whether `read()` of this file goes through the page cache (regular
    /// files on disk filesystems).
    fn cacheable(&self) -> bool {
        false
    }
    /// Read for the page cache: like `read_at`, but may bypass lower
    /// caches since the page cache keeps the data.
    fn read_direct(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        self.read_at(off, buf)
    }
    fn truncate(&self, _size: u64) -> KResult<()> {
        Err(EINVAL)
    }
    /// Preallocate `off..off+len` (mode 0 or FALLOC_FL_KEEP_SIZE).
    fn fallocate(&self, _mode: u32, _off: u64, _len: u64) -> KResult<()> {
        Err(EOPNOTSUPP)
    }
    fn symlink(&self, _name: &str, _target: &str) -> KResult<()> {
        Err(EPERM)
    }
    fn readlink(&self) -> KResult<String> {
        Err(EINVAL)
    }
    fn chmod(&self, _mode: u32) -> KResult<()> {
        Ok(())
    }
    fn chown(&self, _uid: u32, _gid: u32) -> KResult<()> {
        Ok(())
    }
    fn set_times(&self, _atime: Option<u64>, _mtime: Option<u64>) -> KResult<()> {
        Ok(())
    }
    fn sync(&self) -> KResult<()> {
        Ok(())
    }
    /// The value of extended attribute `name` ("user.comment").
    fn getxattr(&self, _name: &str) -> KResult<Vec<u8>> {
        Err(ENODATA)
    }
    /// Names of the extended attributes.
    fn listxattr(&self) -> KResult<Vec<String>> {
        Ok(Vec::new())
    }
    /// Device and special nodes return a stream object to use instead of
    /// `read_at`/`write_at`.
    fn open(&self, _flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        Ok(None)
    }
    /// Identifies the filesystem instance (for cross-device checks).
    fn fs_id(&self) -> usize;
    fn as_any(&self) -> &dyn Any;
}

/// Readiness bits for poll/select.
pub const POLLIN: u16 = 0x001;
pub const POLLPRI: u16 = 0x002;
pub const POLLOUT: u16 = 0x004;
pub const POLLERR: u16 = 0x008;
pub const POLLHUP: u16 = 0x010;
pub const POLLNVAL: u16 = 0x020;

/// Stream-like file objects (pipes, sockets, TTYs, devices).
pub trait FileLike: Send + Sync + Any {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize>;
    fn write(&self, buf: &[u8], nonblock: bool) -> KResult<usize>;
    fn poll(&self) -> u16 {
        POLLIN | POLLOUT
    }
    /// The queue woken when `poll()` may have changed. Objects without one
    /// of their own use the global [`POLL_WQ`] (woken by `notify_poll`).
    fn wait_queue(&self) -> &WaitQueue {
        &POLL_WQ
    }
    fn ioctl(&self, _cmd: u64, _arg: u64) -> KResult<i64> {
        Err(ENOTTY)
    }
    fn stat(&self) -> KResult<Metadata> {
        Ok(Metadata::new(FileType::CharDevice, 0o666))
    }
    /// Offset-addressed access for seekable devices (e.g. block devices).
    fn read_at(&self, _off: u64, _buf: &mut [u8]) -> Option<KResult<usize>> {
        None
    }
    fn write_at(&self, _off: u64, _buf: &[u8]) -> Option<KResult<usize>> {
        None
    }
    fn size(&self) -> Option<u64> {
        None
    }
    /// Called when the last descriptor referring to the object closes.
    fn close(&self) {}
    /// Device nodes: the object an `open()` of the node returns, if not
    /// this one (e.g. /dev/ptmx creates a new pseudo-terminal, /dev/tty is
    /// the caller's controlling terminal).
    fn open_instance(&self, _flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        Ok(None)
    }
    /// Device memory mapping: how `len` bytes at file offset `off` map into
    /// a process, or `None` if the object cannot be mapped.
    fn mmap(&self, _off: u64, _len: u64, _prot: u32) -> KResult<Option<DeviceMap>> {
        Ok(None)
    }
    /// Sockets of non-IP families (AF_UNIX, AF_NETLINK, AF_PACKET).
    fn as_socket(&self) -> Option<&dyn crate::net::generic::GenericSocket> {
        None
    }
    fn as_any(&self) -> &dyn Any;
}

/// How a device's memory maps into a process (`FileLike::mmap`).
pub enum DeviceMap {
    /// Physically contiguous memory starting at `base` (for the mapped file
    /// offset), with memory type `cache`.
    Phys { base: u64, cache: crate::mm::Cache },
    /// Pages the driver supplies on fault (`MapPages::fault`).
    Pages(Arc<dyn MapPages>),
}

/// A device mapping whose pages are looked up on fault.
pub trait MapPages: Send + Sync {
    /// The physical page backing page `pgoff` of the file, and its memory
    /// type. The driver keeps the page alive while the mapping exists.
    fn fault(&self, pgoff: u64, write: bool) -> KResult<(u64, crate::mm::Cache)>;
}

// ---------------------------------------------------------------------------
// Poll wake-ups
// ---------------------------------------------------------------------------

/// Readiness queue for objects without one of their own (see
/// [`FileLike::wait_queue`]).
pub static POLL_WQ: WaitQueue = WaitQueue::new();

pub fn notify_poll() {
    POLL_WQ.wake_all();
}

// ---------------------------------------------------------------------------
// Open file descriptions
// ---------------------------------------------------------------------------

pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_ACCMODE: u32 = 3;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_NOCTTY: u32 = 0o400;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_NOFOLLOW: u32 = 0o400000;
pub const O_CLOEXEC: u32 = 0o2000000;

pub enum FileObject {
    Inode(Arc<dyn Inode>),
    Stream(Arc<dyn FileLike>),
}

pub struct File {
    pub object: FileObject,
    /// The inode the file was opened from (also set for device nodes).
    pub inode: Option<Arc<dyn Inode>>,
    pub offset: Mutex<u64>,
    pub flags: AtomicU32,
    pub path: String,
}

impl File {
    pub fn from_stream(obj: Arc<dyn FileLike>, flags: u32, path: &str) -> Arc<File> {
        Arc::new(File {
            object: FileObject::Stream(obj),
            inode: None,
            offset: Mutex::new(0),
            flags: AtomicU32::new(flags),
            path: path.to_string(),
        })
    }

    pub fn flags(&self) -> u32 {
        self.flags.load(Ordering::Relaxed)
    }

    pub fn readable(&self) -> bool {
        self.flags() & O_ACCMODE != O_WRONLY
    }

    pub fn writable(&self) -> bool {
        self.flags() & O_ACCMODE != O_RDONLY
    }

    fn nonblock(&self) -> bool {
        self.flags() & O_NONBLOCK != 0
    }

    pub fn read(&self, buf: &mut [u8]) -> KResult<usize> {
        if !self.readable() {
            return Err(EBADF);
        }
        match &self.object {
            FileObject::Inode(i) => {
                let mut off = self.offset.lock();
                let n = Self::read_inode(i, *off, buf)?;
                *off += n as u64;
                Ok(n)
            }
            FileObject::Stream(s) => {
                let off = *self.offset.lock();
                if let Some(r) = s.read_at(off, buf) {
                    let n = r?;
                    *self.offset.lock() += n as u64;
                    return Ok(n);
                }
                s.read(buf, self.nonblock())
            }
        }
    }

    pub fn write(&self, buf: &[u8]) -> KResult<usize> {
        if !self.writable() {
            return Err(EBADF);
        }
        match &self.object {
            FileObject::Inode(i) => {
                let mut off = self.offset.lock();
                if self.flags() & O_APPEND != 0 {
                    *off = i.metadata()?.size;
                }
                let n = i.write_at(*off, buf)?;
                crate::mm::pagecache::write_through(i, *off, &buf[..n]);
                *off += n as u64;
                drop(off);
                inotify::file_event(&self.path, Some(i), inotify::IN_MODIFY);
                Ok(n)
            }
            FileObject::Stream(s) => {
                let off = *self.offset.lock();
                if let Some(r) = s.write_at(off, buf) {
                    let n = r?;
                    *self.offset.lock() += n as u64;
                    return Ok(n);
                }
                s.write(buf, self.nonblock())
            }
        }
    }

    fn read_inode(i: &Arc<dyn Inode>, off: u64, buf: &mut [u8]) -> KResult<usize> {
        if i.cacheable() {
            crate::mm::pagecache::read(i, off, buf)
        } else {
            let n = i.read_at(off, buf)?;
            crate::mm::pagecache::read_overlay(i, off, buf, n);
            Ok(n)
        }
    }

    pub fn pread(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        match &self.object {
            FileObject::Inode(i) => Self::read_inode(i, off, buf),
            FileObject::Stream(s) => s.read_at(off, buf).unwrap_or(Err(ESPIPE)),
        }
    }

    pub fn pwrite(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        match &self.object {
            FileObject::Inode(i) => {
                let n = i.write_at(off, buf)?;
                crate::mm::pagecache::write_through(i, off, &buf[..n]);
                inotify::file_event(&self.path, Some(i), inotify::IN_MODIFY);
                Ok(n)
            }
            FileObject::Stream(s) => s.write_at(off, buf).unwrap_or(Err(ESPIPE)),
        }
    }

    /// `whence`: 0 = SET, 1 = CUR, 2 = END.
    pub fn seek(&self, off: i64, whence: u32) -> KResult<u64> {
        let size = match &self.object {
            FileObject::Inode(i) => i.metadata()?.size,
            FileObject::Stream(s) => match s.size() {
                Some(sz) => sz,
                None => return Err(ESPIPE),
            },
        };
        let mut cur = self.offset.lock();
        let base = match whence {
            0 => 0i64,
            1 => *cur as i64,
            2 => size as i64,
            _ => return Err(EINVAL),
        };
        let new = base.checked_add(off).ok_or(EINVAL)?;
        if new < 0 {
            return Err(EINVAL);
        }
        *cur = new as u64;
        Ok(new as u64)
    }

    pub fn stat(&self) -> KResult<Metadata> {
        if let Some(i) = &self.inode {
            let mut m = i.metadata()?;
            if let FileObject::Stream(s) = &self.object
                && let Some(sz) = s.size()
            {
                m.size = sz;
            }
            return Ok(m);
        }
        match &self.object {
            FileObject::Inode(i) => i.metadata(),
            FileObject::Stream(s) => s.stat(),
        }
    }

    pub fn poll(&self) -> u16 {
        match &self.object {
            FileObject::Inode(_) => POLLIN | POLLOUT,
            FileObject::Stream(s) => s.poll(),
        }
    }

    /// Where to wait for `poll()` to change (None: regular files are
    /// always ready).
    pub fn wait_queue(&self) -> Option<&WaitQueue> {
        match &self.object {
            FileObject::Inode(_) => None,
            FileObject::Stream(s) => Some(s.wait_queue()),
        }
    }

    pub fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        match &self.object {
            FileObject::Inode(_) => Err(ENOTTY),
            FileObject::Stream(s) => s.ioctl(cmd, arg),
        }
    }

    pub fn stream(&self) -> Option<&Arc<dyn FileLike>> {
        match &self.object {
            FileObject::Stream(s) => Some(s),
            _ => None,
        }
    }

    pub fn dir_inode(&self) -> KResult<&Arc<dyn Inode>> {
        match &self.object {
            FileObject::Inode(i) => Ok(i),
            _ => Err(ENOTDIR),
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        if let FileObject::Stream(s) = &self.object {
            s.close();
        }
        if self.inode.is_some() {
            let mask = if self.writable() {
                inotify::IN_CLOSE_WRITE
            } else {
                inotify::IN_CLOSE_NOWRITE
            };
            inotify::file_event(&self.path, self.inode.as_ref(), mask);
        }
    }
}

// ---------------------------------------------------------------------------
// Mount table and path resolution
// ---------------------------------------------------------------------------

pub struct Mount {
    pub path: String,
    pub fs: Arc<dyn FileSystem>,
    pub source: String,
    pub root: Arc<dyn Inode>,
}

static MOUNTS: RwLock<Vec<Mount>> = RwLock::new(Vec::new());

/// Mount `fs` at `path` (the directory must exist, except for "/").
pub fn mount(path: &str, fs: Arc<dyn FileSystem>, source: &str) -> KResult<()> {
    let path = normalize(path);
    if path != "/" {
        let node = lookup(&path)?;
        if node.metadata()?.kind != FileType::Directory {
            return Err(ENOTDIR);
        }
    }
    let mut m = MOUNTS.write();
    if m.iter().any(|x| x.path == path) {
        return Err(EBUSY);
    }
    let root = fs.root();
    m.push(Mount {
        path,
        fs,
        source: source.to_string(),
        root,
    });
    Ok(())
}

pub fn umount(path: &str) -> KResult<()> {
    let path = normalize(path);
    if path == "/" {
        return Err(EBUSY);
    }
    let mut m = MOUNTS.write();
    if m.iter().any(|x| {
        x.path.starts_with(&path)
            && x.path.len() > path.len()
            && x.path.as_bytes()[path.len()] == b'/'
    }) {
        return Err(EBUSY);
    }
    let idx = m.iter().position(|x| x.path == path).ok_or(EINVAL)?;
    let fs = m[idx].fs.clone();
    let _ = fs.unmount();
    m.remove(idx);
    Ok(())
}

/// (mount path, fs name, source) for every mount.
pub fn mounts() -> Vec<(String, &'static str, String)> {
    MOUNTS
        .read()
        .iter()
        .map(|m| (m.path.clone(), m.fs.name(), m.source.clone()))
        .collect()
}

/// The filesystem mounted from device `source` (or, for quotactl
/// convenience, the one containing path `source`).
pub fn fs_by_source(source: &str) -> Option<Arc<dyn FileSystem>> {
    let by_dev = MOUNTS
        .read()
        .iter()
        .find(|m| m.source == source)
        .map(|m| m.fs.clone());
    by_dev.or_else(|| mount_fs(source))
}

pub fn mount_fs(path: &str) -> Option<Arc<dyn FileSystem>> {
    let path = normalize(path);
    MOUNTS
        .read()
        .iter()
        .filter(|m| {
            path == m.path
                || path.starts_with(&(m.path.clone() + if m.path == "/" { "" } else { "/" }))
        })
        .max_by_key(|m| m.path.len())
        .map(|m| m.fs.clone())
}

/// Before power-off: leave every filesystem clean (as at unmount).
pub fn finish_all() {
    for m in MOUNTS.read().iter() {
        let _ = m.fs.unmount();
    }
}

pub fn sync_all() {
    for m in MOUNTS.read().iter() {
        let _ = m.fs.sync();
    }
}

fn mounted_root(path: &str) -> Option<Arc<dyn Inode>> {
    MOUNTS
        .read()
        .iter()
        .rev()
        .find(|m| m.path == path)
        .map(|m| m.root.clone())
}

/// Collapse `.`, `..` and duplicate slashes in an absolute path.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let mut s = String::from("/");
    s.push_str(&parts.join("/"));
    s
}

/// Join `path` onto `cwd` (if relative) and normalise lexically.
pub fn absolute(cwd: &str, path: &str) -> String {
    if path.starts_with('/') {
        normalize(path)
    } else {
        normalize(&alloc::format!("{}/{}", cwd, path))
    }
}

fn split_parent(path: &str) -> (String, String) {
    let p = normalize(path);
    match p.rfind('/') {
        Some(0) => (String::from("/"), p[1..].to_string()),
        Some(i) => (p[..i].to_string(), p[i + 1..].to_string()),
        None => (String::from("/"), p),
    }
}

/// Resolve an absolute path to (inode, canonical path).
fn resolve(path: &str, follow_last: bool, depth: u32) -> KResult<(Arc<dyn Inode>, String)> {
    if depth > 40 {
        return Err(ELOOP);
    }
    let root = mounted_root("/").ok_or(ENOENT)?;
    let mut stack: Vec<(Arc<dyn Inode>, String)> = alloc::vec![(root, String::from("/"))];
    let comps: Vec<&str> = path
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let n = comps.len();
    for (i, comp) in comps.iter().enumerate() {
        if comp.len() > 255 {
            return Err(ENAMETOOLONG);
        }
        if *comp == ".." {
            if stack.len() > 1 {
                stack.pop();
            }
            continue;
        }
        let (cur, cur_path) = stack.last().unwrap().clone();
        if cur.metadata()?.kind != FileType::Directory {
            return Err(ENOTDIR);
        }
        let child_path = if cur_path == "/" {
            alloc::format!("/{}", comp)
        } else {
            alloc::format!("{}/{}", cur_path, comp)
        };
        let child = match mounted_root(&child_path) {
            Some(r) => r,
            None => cur.lookup(comp)?,
        };
        let is_last = i + 1 == n;
        if child.metadata()?.kind == FileType::Symlink && (!is_last || follow_last) {
            let target = child.readlink()?;
            let mut rest = String::new();
            for c in &comps[i + 1..] {
                rest.push('/');
                rest.push_str(c);
            }
            let new_path = if target.starts_with('/') {
                alloc::format!("{}{}", target, rest)
            } else {
                alloc::format!("{}/{}{}", cur_path, target, rest)
            };
            return resolve(&normalize(&new_path), follow_last, depth + 1);
        }
        stack.push((child, child_path));
    }
    Ok(stack.pop().unwrap())
}

/// Look up an absolute path, following symlinks.
pub fn lookup(path: &str) -> KResult<Arc<dyn Inode>> {
    resolve(&normalize(path), true, 0).map(|(i, _)| i)
}

/// Look up without following a final symlink.
pub fn lookup_nofollow(path: &str) -> KResult<Arc<dyn Inode>> {
    resolve(&normalize(path), false, 0).map(|(i, _)| i)
}

/// Canonical path (symlinks resolved).
pub fn canonicalize(path: &str) -> KResult<String> {
    resolve(&normalize(path), true, 0).map(|(_, p)| p)
}

/// Resolve the parent directory of `path`, returning it and the final name.
pub fn lookup_parent(path: &str) -> KResult<(Arc<dyn Inode>, String)> {
    let (parent, name) = split_parent(path);
    if name.is_empty() {
        return Err(EEXIST);
    }
    let dir = lookup(&parent)?;
    if dir.metadata()?.kind != FileType::Directory {
        return Err(ENOTDIR);
    }
    Ok((dir, name))
}

/// Open an absolute path.
pub fn open(path: &str, flags: u32, mode: u32) -> KResult<Arc<File>> {
    let path = normalize(path);
    let inode = match if flags & O_NOFOLLOW != 0 {
        lookup_nofollow(&path)
    } else {
        lookup(&path)
    } {
        Ok(i) => {
            if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                return Err(EEXIST);
            }
            i
        }
        Err(ENOENT) if flags & O_CREAT != 0 => {
            let (dir, name) = lookup_parent(&path)?;
            let i = dir.create(&name, FileType::Regular, mode & 0o7777)?;
            inotify::created(&path);
            i
        }
        Err(e) => return Err(e),
    };
    let meta = inode.metadata()?;
    if flags & O_DIRECTORY != 0 && meta.kind != FileType::Directory {
        return Err(ENOTDIR);
    }
    if meta.kind == FileType::Directory && flags & O_ACCMODE != O_RDONLY {
        return Err(EISDIR);
    }
    if meta.kind == FileType::Symlink {
        return Err(ELOOP);
    }
    if flags & O_TRUNC != 0 && meta.kind == FileType::Regular && flags & O_ACCMODE != O_RDONLY {
        inode.truncate(0)?;
        inotify::file_event(&path, Some(&inode), inotify::IN_MODIFY);
    }
    inotify::file_event(&path, Some(&inode), inotify::IN_OPEN);
    let object = match inode.open(flags)? {
        Some(stream) => FileObject::Stream(stream),
        None => FileObject::Inode(inode.clone()),
    };
    Ok(Arc::new(File {
        object,
        inode: Some(inode),
        offset: Mutex::new(0),
        flags: AtomicU32::new(flags & !(O_CREAT | O_EXCL | O_TRUNC | O_NOCTTY)),
        path,
    }))
}

// ---------------------------------------------------------------------------
// Convenience operations on absolute paths (kernel use and syscalls)
// ---------------------------------------------------------------------------

pub fn read_all(path: &str) -> KResult<Vec<u8>> {
    let inode = lookup(path)?;
    let meta = inode.metadata()?;
    if meta.kind == FileType::Directory {
        return Err(EISDIR);
    }
    // Sizes of generated files (e.g. /proc) are unknown: read until EOF.
    let mut out = Vec::with_capacity(meta.size as usize);
    let mut chunk = alloc::vec![0u8; 64 * 1024];
    loop {
        let n = inode.read_at(out.len() as u64, &mut chunk)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

pub fn write_all(path: &str, data: &[u8]) -> KResult<()> {
    let f = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)?;
    let mut done = 0;
    while done < data.len() {
        let n = f.write(&data[done..])?;
        if n == 0 {
            return Err(EIO);
        }
        done += n;
    }
    Ok(())
}

/// Append `data` to a file, creating it if needed.
pub fn append(path: &str, data: &[u8]) -> KResult<()> {
    let f = open(path, O_WRONLY | O_CREAT | O_APPEND, 0o644)?;
    let mut done = 0;
    while done < data.len() {
        let n = f.write(&data[done..])?;
        if n == 0 {
            return Err(EIO);
        }
        done += n;
    }
    Ok(())
}

pub fn mkdir(path: &str, mode: u32) -> KResult<()> {
    let (dir, name) = lookup_parent(path)?;
    if dir.lookup(&name).is_ok() {
        return Err(EEXIST);
    }
    dir.create(&name, FileType::Directory, mode)?;
    inotify::created(&normalize(path));
    Ok(())
}

pub fn mkdir_p(path: &str) -> KResult<()> {
    let norm = normalize(path);
    let mut cur = String::new();
    for c in norm.split('/').filter(|c| !c.is_empty()) {
        cur.push('/');
        cur.push_str(c);
        match mkdir(&cur, 0o755) {
            Ok(()) | Err(EEXIST) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn unlink(path: &str) -> KResult<()> {
    let (dir, name) = lookup_parent(path)?;
    let node = dir.lookup(&name)?;
    if node.metadata()?.kind == FileType::Directory {
        return Err(EISDIR);
    }
    let r = inotify::removing(&normalize(path));
    dir.unlink(&name)?;
    inotify::removed(r);
    Ok(())
}

pub fn rmdir(path: &str) -> KResult<()> {
    let norm = normalize(path);
    if MOUNTS.read().iter().any(|m| m.path == norm) {
        return Err(EBUSY);
    }
    let (dir, name) = lookup_parent(path)?;
    let r = inotify::removing(&norm);
    dir.rmdir(&name)?;
    inotify::removed(r);
    Ok(())
}

pub fn rename(old: &str, new: &str) -> KResult<()> {
    let (od, on) = lookup_parent(old)?;
    let (nd, nn) = lookup_parent(new)?;
    if od.fs_id() != nd.fs_id() {
        return Err(EXDEV);
    }
    let old_norm = normalize(old);
    let new_norm = normalize(new);
    if new_norm.starts_with(&(old_norm.clone() + "/")) {
        return Err(EINVAL);
    }
    od.rename(&on, &nd, &nn)?;
    inotify::renamed(&old_norm, &new_norm);
    Ok(())
}

pub fn symlink(target: &str, linkpath: &str) -> KResult<()> {
    let (dir, name) = lookup_parent(linkpath)?;
    if dir.lookup(&name).is_ok() {
        return Err(EEXIST);
    }
    dir.symlink(&name, target)?;
    inotify::created(&normalize(linkpath));
    Ok(())
}

pub fn link(old: &str, new: &str) -> KResult<()> {
    let target = lookup_nofollow(old)?;
    let (dir, name) = lookup_parent(new)?;
    if dir.fs_id() != target.fs_id() {
        return Err(EXDEV);
    }
    dir.link(&name, &target)?;
    inotify::created(&normalize(new));
    Ok(())
}

pub fn readdir(path: &str) -> KResult<Vec<DirEntry>> {
    lookup(path)?.readdir()
}

pub fn stat(path: &str) -> KResult<Metadata> {
    lookup(path)?.metadata()
}

pub fn exists(path: &str) -> bool {
    lookup(path).is_ok()
}

pub fn is_dir(path: &str) -> bool {
    stat(path).is_ok_and(|m| m.kind == FileType::Directory)
}

/// Copy a file (possibly across filesystems).
pub fn copy_file(src: &str, dst: &str) -> KResult<()> {
    let data = read_all(src)?;
    write_all(dst, &data)
}

/// Mount a fresh tmpfs as the root filesystem plus the standard pseudo
/// filesystems.
pub fn init() {
    if mounted_root("/").is_some() {
        return;
    }
    let root = tmpfs::TmpFs::new();
    mount("/", root, "rootfs").expect("mount root");
    for d in [
        "/dev",
        "/proc",
        "/tmp",
        "/mnt",
        "/bin",
        "/etc",
        "/lib",
        "/lib/firmware",
        "/root",
        "/home",
        "/var",
        "/var/log",
        "/sys",
    ] {
        let _ = mkdir_p(d);
    }
    mount("/dev", devfs::DevFs::new(), "devfs").expect("mount devfs");
    // POSIX shared memory (shm_open) lives in a tmpfs at /dev/shm.
    mount("/dev/shm", tmpfs::TmpFs::new(), "tmpfs").expect("mount /dev/shm");
    mount("/proc", procfs::ProcFs::new(), "proc").expect("mount procfs");
    mount("/sys", sysfs::SysFs::new(), "sysfs").expect("mount sysfs");
}
