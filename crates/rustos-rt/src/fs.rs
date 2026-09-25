//! Files and directories.

use crate::Result;
use crate::sys::{self, cstr, nr};
use alloc::string::String;
use alloc::vec::Vec;

pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_NOFOLLOW: u32 = 0o400000;
pub const O_CLOEXEC: u32 = 0o2000000;

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFSOCK: u32 = 0o140000;

/// An open file descriptor, closed on drop.
pub struct File {
    fd: i32,
}

impl File {
    pub fn open(path: &str) -> Result<File> {
        File::open_with(path, O_RDONLY, 0)
    }

    pub fn create(path: &str) -> Result<File> {
        File::open_with(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
    }

    pub fn open_with(path: &str, flags: u32, mode: u32) -> Result<File> {
        let p = cstr(path);
        let fd = sys::check(sys::syscall(
            nr::OPEN,
            &[
                p.as_ptr() as usize,
                (flags | O_CLOEXEC) as usize,
                mode as usize,
            ],
        ))?;
        Ok(File { fd: fd as i32 })
    }

    pub fn from_raw(fd: i32) -> File {
        File { fd }
    }

    pub fn fd(&self) -> i32 {
        self.fd
    }

    /// Give up ownership of the descriptor.
    pub fn into_raw(self) -> i32 {
        let fd = self.fd;
        core::mem::forget(self);
        fd
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        crate::io::read(self.fd, buf)
    }

    pub fn write(&self, buf: &[u8]) -> Result<usize> {
        crate::io::write(self.fd, buf)
    }

    pub fn write_all(&self, buf: &[u8]) -> Result<()> {
        crate::io::write_all(self.fd, buf)
    }

    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize> {
        sys::check(sys::syscall(
            nr::PREAD64,
            &[
                self.fd as usize,
                buf.as_mut_ptr() as usize,
                buf.len(),
                off as usize,
            ],
        ))
    }

    pub fn write_at(&self, off: u64, buf: &[u8]) -> Result<usize> {
        sys::check(sys::syscall(
            nr::PWRITE64,
            &[
                self.fd as usize,
                buf.as_ptr() as usize,
                buf.len(),
                off as usize,
            ],
        ))
    }

    pub fn seek(&self, off: i64, whence: u32) -> Result<u64> {
        sys::check(sys::syscall(
            nr::LSEEK,
            &[self.fd as usize, off as usize, whence as usize],
        ))
        .map(|v| v as u64)
    }

    pub fn metadata(&self) -> Result<Metadata> {
        let mut st = RawStat::default();
        sys::check(sys::syscall(
            nr::FSTAT,
            &[self.fd as usize, &mut st as *mut _ as usize],
        ))?;
        Ok(Metadata::from(st))
    }

    pub fn read_to_end(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = alloc::vec![0u8; 64 * 1024];
        loop {
            match self.read(&mut buf) {
                Ok(0) => return Ok(out),
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(crate::Error(4)) => {}
                Err(e) => return Err(e),
            }
        }
    }

    pub fn sync(&self) -> Result<()> {
        sys::check(sys::syscall(nr::FSYNC, &[self.fd as usize])).map(|_| ())
    }

    pub fn set_len(&self, len: u64) -> Result<()> {
        sys::check(sys::syscall(
            nr::FTRUNCATE,
            &[self.fd as usize, len as usize],
        ))
        .map(|_| ())
    }

    pub fn ioctl(&self, cmd: u64, arg: usize) -> Result<usize> {
        sys::check(sys::syscall(
            nr::IOCTL,
            &[self.fd as usize, cmd as usize, arg],
        ))
    }
}

impl crate::io::Read for File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        crate::io::read(self.fd, buf)
    }
}

impl crate::io::Write for File {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        crate::io::write(self.fd, buf)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        sys::syscall(nr::CLOSE, &[self.fd as usize]);
    }
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct RawStat {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub _pad0: u32,
    pub rdev: u64,
    pub size: i64,
    pub blksize: i64,
    pub blocks: i64,
    pub atime: i64,
    pub atime_nsec: i64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
    pub _reserved: [i64; 3],
}

#[derive(Debug, Clone, Copy)]
pub struct Metadata {
    pub mode: u32,
    pub size: u64,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
    pub ino: u64,
    pub dev: u64,
    pub rdev: u64,
    pub blocks: u64,
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
}

impl From<RawStat> for Metadata {
    fn from(s: RawStat) -> Metadata {
        Metadata {
            mode: s.mode,
            size: s.size as u64,
            nlink: s.nlink,
            uid: s.uid,
            gid: s.gid,
            ino: s.ino,
            dev: s.dev,
            rdev: s.rdev,
            blocks: s.blocks as u64,
            atime: s.atime,
            mtime: s.mtime,
            ctime: s.ctime,
        }
    }
}

impl Metadata {
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
    pub fn is_file(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }
    pub fn is_symlink(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }
    pub fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }
    /// `ls -l` style type+permission string, e.g. "drwxr-xr-x".
    pub fn mode_string(&self) -> String {
        let t = match self.mode & S_IFMT {
            S_IFDIR => 'd',
            S_IFLNK => 'l',
            S_IFCHR => 'c',
            S_IFBLK => 'b',
            S_IFIFO => 'p',
            S_IFSOCK => 's',
            _ => '-',
        };
        let mut s = String::new();
        s.push(t);
        for i in (0..3).rev() {
            let bits = (self.mode >> (i * 3)) & 7;
            s.push(if bits & 4 != 0 { 'r' } else { '-' });
            s.push(if bits & 2 != 0 { 'w' } else { '-' });
            s.push(if bits & 1 != 0 { 'x' } else { '-' });
        }
        s
    }
}

pub fn metadata(path: &str) -> Result<Metadata> {
    let p = cstr(path);
    let mut st = RawStat::default();
    sys::check(sys::syscall(
        nr::STAT,
        &[p.as_ptr() as usize, &mut st as *mut _ as usize],
    ))?;
    Ok(Metadata::from(st))
}

pub fn symlink_metadata(path: &str) -> Result<Metadata> {
    let p = cstr(path);
    let mut st = RawStat::default();
    sys::check(sys::syscall(
        nr::LSTAT,
        &[p.as_ptr() as usize, &mut st as *mut _ as usize],
    ))?;
    Ok(Metadata::from(st))
}

pub fn exists(path: &str) -> bool {
    metadata(path).is_ok()
}

pub fn is_dir(path: &str) -> bool {
    metadata(path).is_ok_and(|m| m.is_dir())
}

pub fn read(path: &str) -> Result<Vec<u8>> {
    File::open(path)?.read_to_end()
}

pub fn read_to_string(path: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&read(path)?).into_owned())
}

pub fn write(path: &str, data: &[u8]) -> Result<()> {
    File::create(path)?.write_all(data)
}

pub fn append(path: &str, data: &[u8]) -> Result<()> {
    File::open_with(path, O_WRONLY | O_CREAT | O_APPEND, 0o644)?.write_all(data)
}

fn path1(n: usize, path: &str, a: usize) -> Result<()> {
    let p = cstr(path);
    sys::check(sys::syscall(n, &[p.as_ptr() as usize, a])).map(|_| ())
}

fn path2(n: usize, a: &str, b: &str) -> Result<()> {
    let (pa, pb) = (cstr(a), cstr(b));
    sys::check(sys::syscall(
        n,
        &[pa.as_ptr() as usize, pb.as_ptr() as usize],
    ))
    .map(|_| ())
}

pub fn create_dir(path: &str) -> Result<()> {
    path1(nr::MKDIR, path, 0o777)
}

pub fn create_dir_mode(path: &str, mode: u32) -> Result<()> {
    path1(nr::MKDIR, path, mode as usize)
}

pub fn create_dir_all(path: &str) -> Result<()> {
    let mut cur = String::new();
    if path.starts_with('/') {
        cur.push('/');
    }
    for c in path.split('/').filter(|c| !c.is_empty()) {
        if !cur.is_empty() && !cur.ends_with('/') {
            cur.push('/');
        }
        cur.push_str(c);
        match create_dir(&cur) {
            Ok(()) | Err(crate::Error(17)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn remove_file(path: &str) -> Result<()> {
    path1(nr::UNLINK, path, 0)
}

pub fn remove_dir(path: &str) -> Result<()> {
    path1(nr::RMDIR, path, 0)
}

pub fn remove_dir_all(path: &str) -> Result<()> {
    let m = symlink_metadata(path)?;
    if m.is_dir() {
        for e in read_dir(path)? {
            let child = join(path, &e.name);
            remove_dir_all(&child)?;
        }
        remove_dir(path)
    } else {
        remove_file(path)
    }
}

pub fn rename(from: &str, to: &str) -> Result<()> {
    path2(nr::RENAME, from, to)
}

pub fn symlink(target: &str, link: &str) -> Result<()> {
    path2(nr::SYMLINK, target, link)
}

pub fn hard_link(from: &str, to: &str) -> Result<()> {
    path2(nr::LINK, from, to)
}

pub fn read_link(path: &str) -> Result<String> {
    let p = cstr(path);
    let mut buf = alloc::vec![0u8; 4096];
    let n = sys::check(sys::syscall(
        nr::READLINK,
        &[p.as_ptr() as usize, buf.as_mut_ptr() as usize, buf.len()],
    ))?;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn set_permissions(path: &str, mode: u32) -> Result<()> {
    path1(nr::CHMOD, path, mode as usize)
}

pub fn touch(path: &str) -> Result<()> {
    if exists(path) {
        let p = cstr(path);
        sys::check(sys::syscall(
            nr::UTIMENSAT,
            &[(-100isize) as usize, p.as_ptr() as usize, 0, 0],
        ))
        .map(|_| ())
    } else {
        File::open_with(path, O_WRONLY | O_CREAT, 0o644).map(|_| ())
    }
}

pub fn copy(from: &str, to: &str) -> Result<u64> {
    let src = File::open(from)?;
    let mode = src.metadata()?.permissions();
    let dst = File::open_with(to, O_WRONLY | O_CREAT | O_TRUNC, mode)?;
    let mut buf = alloc::vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        dst.write_all(&buf[..n])?;
        total += n as u64;
    }
}

pub fn mkfifo(path: &str, mode: u32) -> Result<()> {
    let p = cstr(path);
    sys::check(sys::syscall(
        nr::MKNOD,
        &[p.as_ptr() as usize, (0o010000 | mode) as usize, 0],
    ))
    .map(|_| ())
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: u8,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.kind == 4
    }
}

/// List a directory (without "." and "..").
pub fn read_dir(path: &str) -> Result<Vec<DirEntry>> {
    let f = File::open_with(path, O_RDONLY | O_DIRECTORY, 0)?;
    let mut out = Vec::new();
    let mut buf = alloc::vec![0u8; 8192];
    loop {
        let n = sys::check(sys::syscall(
            nr::GETDENTS64,
            &[f.fd() as usize, buf.as_mut_ptr() as usize, buf.len()],
        ))?;
        if n == 0 {
            break;
        }
        let mut off = 0;
        while off < n {
            let ino = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
            let reclen = u16::from_le_bytes([buf[off + 16], buf[off + 17]]) as usize;
            let kind = buf[off + 18];
            let name_bytes = &buf[off + 19..off + reclen];
            let end = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_bytes.len());
            let name = String::from_utf8_lossy(&name_bytes[..end]).into_owned();
            if name != "." && name != ".." {
                out.push(DirEntry { name, ino, kind });
            }
            off += reclen;
        }
    }
    Ok(out)
}

/// Join two path components.
pub fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') {
        return String::from(b);
    }
    if a.ends_with('/') {
        alloc::format!("{}{}", a, b)
    } else {
        alloc::format!("{}/{}", a, b)
    }
}

pub fn basename(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    if p.is_empty() {
        return "/";
    }
    p.rsplit('/').next().unwrap_or(p)
}

pub fn dirname(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => ".",
    }
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct StatFs {
    pub f_type: i64,
    pub f_bsize: i64,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_fsid: [i32; 2],
    pub f_namelen: i64,
    pub f_frsize: i64,
    pub f_flags: i64,
    pub f_spare: [i64; 4],
}

pub fn statfs(path: &str) -> Result<StatFs> {
    let p = cstr(path);
    let mut s = StatFs::default();
    sys::check(sys::syscall(
        nr::STATFS,
        &[p.as_ptr() as usize, &mut s as *mut _ as usize],
    ))?;
    Ok(s)
}

pub fn mount(source: &str, target: &str, fstype: &str) -> Result<()> {
    let (s, t, f) = (cstr(source), cstr(target), cstr(fstype));
    sys::check(sys::syscall(
        nr::MOUNT,
        &[
            s.as_ptr() as usize,
            t.as_ptr() as usize,
            f.as_ptr() as usize,
            0,
            0,
        ],
    ))
    .map(|_| ())
}

pub fn umount(target: &str) -> Result<()> {
    path1(nr::UMOUNT2, target, 0)
}

pub fn sync() {
    sys::syscall(nr::SYNC, &[]);
}
