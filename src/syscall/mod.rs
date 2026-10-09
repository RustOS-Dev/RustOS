//! System call dispatch (Linux x86_64 numbering).
//!
//! Arguments arrive in rdi, rsi, rdx, r10, r8, r9 with the number in rax,
//! through either `syscall` or `int 0x80`. The result (or `-errno`) is
//! returned in rax.

pub(crate) mod event;
pub mod fdobj;
mod fs;
mod mem;
mod misc;
mod proc_;

use crate::arch::x86_64::idt::TrapFrame;
use crate::errno::*;
use alloc::string::String;

pub type SysResult = KResult<i64>;

pub mod nr {
    pub const READ: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const OPEN: u64 = 2;
    pub const CLOSE: u64 = 3;
    pub const STAT: u64 = 4;
    pub const FSTAT: u64 = 5;
    pub const LSTAT: u64 = 6;
    pub const POLL: u64 = 7;
    pub const LSEEK: u64 = 8;
    pub const MMAP: u64 = 9;
    pub const MPROTECT: u64 = 10;
    pub const MUNMAP: u64 = 11;
    pub const BRK: u64 = 12;
    pub const RT_SIGACTION: u64 = 13;
    pub const RT_SIGPROCMASK: u64 = 14;
    pub const RT_SIGRETURN: u64 = 15;
    pub const IOCTL: u64 = 16;
    pub const PREAD64: u64 = 17;
    pub const PWRITE64: u64 = 18;
    pub const READV: u64 = 19;
    pub const WRITEV: u64 = 20;
    pub const ACCESS: u64 = 21;
    pub const PIPE: u64 = 22;
    pub const SELECT: u64 = 23;
    pub const SCHED_YIELD: u64 = 24;
    pub const MADVISE: u64 = 28;
    pub const DUP: u64 = 32;
    pub const DUP2: u64 = 33;
    pub const PAUSE: u64 = 34;
    pub const NANOSLEEP: u64 = 35;
    pub const ALARM: u64 = 37;
    pub const GETITIMER: u64 = 36;
    pub const MSYNC: u64 = 26;
    pub const MREMAP: u64 = 25;
    pub const MEMBARRIER: u64 = 324;
    pub const SETITIMER: u64 = 38;
    pub const GETPRIORITY: u64 = 140;
    pub const SETPRIORITY: u64 = 141;
    pub const SCHED_GETPARAM: u64 = 143;
    pub const SCHED_GETSCHEDULER: u64 = 145;
    pub const SCHED_GET_PRIORITY_MAX: u64 = 146;
    pub const SCHED_GET_PRIORITY_MIN: u64 = 147;
    pub const SCHED_SETAFFINITY: u64 = 203;
    pub const SCHED_GETAFFINITY: u64 = 204;
    pub const EPOLL_CREATE: u64 = 213;
    pub const EPOLL_WAIT: u64 = 232;
    pub const EPOLL_CTL: u64 = 233;
    pub const EPOLL_PWAIT: u64 = 281;
    pub const SIGNALFD: u64 = 282;
    pub const TIMERFD_CREATE: u64 = 283;
    pub const EVENTFD: u64 = 284;
    pub const TIMERFD_SETTIME: u64 = 286;
    pub const TIMERFD_GETTIME: u64 = 287;
    pub const SIGNALFD4: u64 = 289;
    pub const EVENTFD2: u64 = 290;
    pub const EPOLL_CREATE1: u64 = 291;
    pub const EPOLL_PWAIT2: u64 = 441;
    pub const GETPID: u64 = 39;
    pub const SENDFILE: u64 = 40;
    pub const SOCKET: u64 = 41;
    pub const CONNECT: u64 = 42;
    pub const ACCEPT: u64 = 43;
    pub const SENDTO: u64 = 44;
    pub const RECVFROM: u64 = 45;
    pub const SENDMSG: u64 = 46;
    pub const RECVMSG: u64 = 47;
    pub const SHUTDOWN: u64 = 48;
    pub const BIND: u64 = 49;
    pub const LISTEN: u64 = 50;
    pub const GETSOCKNAME: u64 = 51;
    pub const GETPEERNAME: u64 = 52;
    pub const SOCKETPAIR: u64 = 53;
    pub const SETSOCKOPT: u64 = 54;
    pub const GETSOCKOPT: u64 = 55;
    pub const CLONE: u64 = 56;
    pub const FORK: u64 = 57;
    pub const VFORK: u64 = 58;
    pub const EXECVE: u64 = 59;
    pub const EXIT: u64 = 60;
    pub const WAIT4: u64 = 61;
    pub const WAITID: u64 = 247;
    pub const KILL: u64 = 62;
    pub const UNAME: u64 = 63;
    pub const FCNTL: u64 = 72;
    pub const FLOCK: u64 = 73;
    pub const FSYNC: u64 = 74;
    pub const FDATASYNC: u64 = 75;
    pub const FADVISE64: u64 = 221;
    pub const TRUNCATE: u64 = 76;
    pub const FTRUNCATE: u64 = 77;
    pub const FALLOCATE: u64 = 285;
    pub const GETDENTS: u64 = 78;
    pub const GETCWD: u64 = 79;
    pub const CHDIR: u64 = 80;
    pub const FCHDIR: u64 = 81;
    pub const RENAME: u64 = 82;
    pub const MKDIR: u64 = 83;
    pub const RMDIR: u64 = 84;
    pub const CREAT: u64 = 85;
    pub const LINK: u64 = 86;
    pub const UNLINK: u64 = 87;
    pub const SYMLINK: u64 = 88;
    pub const READLINK: u64 = 89;
    pub const CHMOD: u64 = 90;
    pub const FCHMOD: u64 = 91;
    pub const CHOWN: u64 = 92;
    pub const FCHOWN: u64 = 93;
    pub const LCHOWN: u64 = 94;
    pub const UMASK: u64 = 95;
    pub const GETTIMEOFDAY: u64 = 96;
    pub const SETTIMEOFDAY: u64 = 164;
    pub const CLOCK_SETTIME: u64 = 227;
    pub const GETRLIMIT: u64 = 97;
    pub const GETRUSAGE: u64 = 98;
    pub const SYSINFO: u64 = 99;
    pub const TIMES: u64 = 100;
    pub const GETUID: u64 = 102;
    pub const SYSLOG: u64 = 103;
    pub const GETGID: u64 = 104;
    pub const SETUID: u64 = 105;
    pub const SETREUID: u64 = 113;
    pub const SETREGID: u64 = 114;
    pub const SETRESUID: u64 = 117;
    pub const GETRESUID: u64 = 118;
    pub const SETRESGID: u64 = 119;
    pub const GETRESGID: u64 = 120;
    pub const SETGID: u64 = 106;
    pub const GETEUID: u64 = 107;
    pub const GETEGID: u64 = 108;
    pub const SETPGID: u64 = 109;
    pub const GETPPID: u64 = 110;
    pub const GETPGRP: u64 = 111;
    pub const SETSID: u64 = 112;
    pub const GETGROUPS: u64 = 115;
    pub const GETPGID: u64 = 121;
    pub const GETSID: u64 = 124;
    pub const RT_SIGPENDING: u64 = 127;
    pub const RT_SIGSUSPEND: u64 = 130;
    pub const SIGALTSTACK: u64 = 131;
    pub const MKNOD: u64 = 133;
    pub const STATFS: u64 = 137;
    pub const FSTATFS: u64 = 138;
    pub const QUOTACTL: u64 = 179;
    pub const GETXATTR: u64 = 191;
    pub const LGETXATTR: u64 = 192;
    pub const FGETXATTR: u64 = 193;
    pub const LISTXATTR: u64 = 194;
    pub const LLISTXATTR: u64 = 195;
    pub const FLISTXATTR: u64 = 196;
    pub const PRCTL: u64 = 157;
    pub const ARCH_PRCTL: u64 = 158;
    pub const SYNC: u64 = 162;
    pub const MOUNT: u64 = 165;
    pub const UMOUNT2: u64 = 166;
    pub const REBOOT: u64 = 169;
    pub const SETHOSTNAME: u64 = 170;
    pub const GETTID: u64 = 186;
    pub const TKILL: u64 = 200;
    pub const TIME: u64 = 201;
    pub const FUTEX: u64 = 202;
    pub const GETDENTS64: u64 = 217;
    pub const SET_TID_ADDRESS: u64 = 218;
    pub const CLOCK_GETTIME: u64 = 228;
    pub const CLOCK_GETRES: u64 = 229;
    pub const CLOCK_NANOSLEEP: u64 = 230;
    pub const EXIT_GROUP: u64 = 231;
    pub const TGKILL: u64 = 234;
    pub const UTIMES: u64 = 235;
    pub const OPENAT: u64 = 257;
    pub const MKDIRAT: u64 = 258;
    pub const FCHOWNAT: u64 = 260;
    pub const NEWFSTATAT: u64 = 262;
    pub const UNLINKAT: u64 = 263;
    pub const RENAMEAT: u64 = 264;
    pub const LINKAT: u64 = 265;
    pub const SYMLINKAT: u64 = 266;
    pub const READLINKAT: u64 = 267;
    pub const FCHMODAT: u64 = 268;
    pub const FACCESSAT: u64 = 269;
    pub const PSELECT6: u64 = 270;
    pub const PPOLL: u64 = 271;
    pub const SET_ROBUST_LIST: u64 = 273;
    pub const UTIMENSAT: u64 = 280;
    pub const ACCEPT4: u64 = 288;
    pub const DUP3: u64 = 292;
    pub const PIPE2: u64 = 293;
    pub const PRLIMIT64: u64 = 302;
    pub const RENAMEAT2: u64 = 316;
    pub const GETRANDOM: u64 = 318;
    pub const STATX: u64 = 332;
    pub const INOTIFY_INIT: u64 = 253;
    pub const INOTIFY_ADD_WATCH: u64 = 254;
    pub const INOTIFY_RM_WATCH: u64 = 255;
    pub const INOTIFY_INIT1: u64 = 294;
    pub const MEMFD_CREATE: u64 = 319;
    pub const COPY_FILE_RANGE: u64 = 326;
    pub const PIDFD_SEND_SIGNAL: u64 = 424;
    pub const PIDFD_OPEN: u64 = 434;
    pub const CLOSE_RANGE: u64 = 436;
    pub const FACCESSAT2: u64 = 439;
}

/// Entry point from both syscall paths.
pub fn dispatch(frame: &mut TrapFrame) {
    x86_64::instructions::interrupts::enable();
    let n = frame.rax;
    let (a1, a2, a3, a4, a5, a6) = (
        frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
    );
    let trace = tracing();
    if trace {
        crate::serial_println!(
            "[strace] pid {} nr {} ({:#x}, {:#x}, {:#x})",
            crate::process::current_pid(),
            n,
            a1,
            a2,
            a3
        );
    }
    // Remember the number for syscall restart (error_code is unused here).
    frame.error_code = n;
    crate::sched::note_syscall(n, a1);
    let r = handle(frame, n, [a1, a2, a3, a4, a5, a6]);
    crate::sched::note_syscall(u64::MAX, 0);
    if trace {
        crate::serial_println!(
            "[strace]   -> {:?}",
            r.as_ref().map(|v| match v {
                Ret::Value(v) => *v,
                Ret::Frame => 0,
            })
        );
    }
    match r {
        // execve and rt_sigreturn rewrite the whole frame.
        Ok(Ret::Frame) => {}
        Ok(Ret::Value(v)) => frame.rax = v as u64,
        Err(EINTR) if restartable(n) => frame.rax = (-ERESTARTSYS) as u64,
        Err(e) => frame.rax = (-(e.0 as i64)) as u64,
    }
}

/// Internal "restart this syscall" code (never seen by user space).
pub const ERESTARTSYS: i64 = 512;

/// Syscalls that transparently restart after a signal without a handler
/// (or with SA_RESTART).
fn restartable(n: u64) -> bool {
    use nr::*;
    matches!(
        n,
        READ | WRITE
            | READV
            | WRITEV
            | PREAD64
            | PWRITE64
            | WAIT4
            | WAITID
            | NANOSLEEP
            | CLOCK_NANOSLEEP
            | ACCEPT
            | ACCEPT4
            | CONNECT
            | RECVFROM
            | SENDTO
            | RECVMSG
            | SENDMSG
            | FUTEX
            | OPEN
            | OPENAT
            | IOCTL
            | FLOCK
    )
}

/// Log every syscall to the serial port (debugging aid).
pub static TRACE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Processes whose name is in /sys/kernel/debug/strace have their
/// syscalls logged (`echo weston-terminal > /sys/kernel/debug/strace`;
/// an empty write stops it).
static TRACE_NAME: crate::sync::Mutex<String> = crate::sync::Mutex::new(String::new());
static TRACE_SOME: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

struct StraceAttr;

impl crate::vfs::sysfs::Attr for StraceAttr {
    fn show(&self) -> KResult<alloc::vec::Vec<u8>> {
        let mut v = TRACE_NAME.lock().clone().into_bytes();
        v.push(b'\n');
        Ok(v)
    }
    fn store(&self, data: &[u8]) -> KResult<usize> {
        let name = String::from_utf8_lossy(data).trim().into();
        *TRACE_NAME.lock() = name;
        TRACE_SOME.store(
            !TRACE_NAME.lock().is_empty(),
            core::sync::atomic::Ordering::SeqCst,
        );
        Ok(data.len())
    }
    fn mode(&self) -> u32 {
        0o644
    }
}

/// Register /sys/kernel/debug/strace.
pub fn init() {
    let _ = crate::vfs::sysfs::add_file("kernel/debug/strace", alloc::sync::Arc::new(StraceAttr));
}

fn tracing() -> bool {
    if TRACE.load(core::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    if !TRACE_SOME.load(core::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    let want = TRACE_NAME.lock().clone();
    crate::process::current().is_some_and(|p| {
        let n = p.name.lock();
        n.rsplit('/').next() == Some(want.as_str())
    })
}

pub enum Ret {
    Value(i64),
    Frame,
}

/// ppoll/pselect6/epoll_pwait: wait with the user's signal mask, if given.
fn wait_sigmask(ptr: u64, len: u64) -> KResult<()> {
    if ptr == 0 {
        return Ok(());
    }
    if len != 8 {
        return Err(EINVAL);
    }
    let mask: u64 = crate::process::uaccess::read_user(ptr)?;
    crate::process::signal::set_temp_mask(mask);
    Ok(())
}

fn handle(frame: &mut TrapFrame, n: u64, a: [u64; 6]) -> KResult<Ret> {
    use nr::*;
    let v = |r: SysResult| r.map(Ret::Value);
    match n {
        // Files
        READ => v(fs::read(a[0] as i32, a[1], a[2])),
        WRITE => v(fs::write(a[0] as i32, a[1], a[2])),
        OPEN => v(fs::openat(fs::AT_FDCWD, a[0], a[1] as u32, a[2] as u32)),
        OPENAT => v(fs::openat(a[0] as i32, a[1], a[2] as u32, a[3] as u32)),
        CREAT => v(fs::openat(fs::AT_FDCWD, a[0], 0o1101, a[1] as u32)),
        CLOSE => v(fs::close(a[0] as i32)),
        STAT => v(fs::fstatat(fs::AT_FDCWD, a[0], a[1], 0)),
        LSTAT => v(fs::fstatat(
            fs::AT_FDCWD,
            a[0],
            a[1],
            fs::AT_SYMLINK_NOFOLLOW,
        )),
        FSTAT => v(fs::fstat(a[0] as i32, a[1])),
        NEWFSTATAT => v(fs::fstatat(a[0] as i32, a[1], a[2], a[3] as u32)),
        STATX => v(fs::statx(a[0] as i32, a[1], a[2] as u32, a[4])),
        LSEEK => v(fs::lseek(a[0] as i32, a[1] as i64, a[2] as u32)),
        PREAD64 => v(fs::pread(a[0] as i32, a[1], a[2], a[3])),
        PWRITE64 => v(fs::pwrite(a[0] as i32, a[1], a[2], a[3])),
        READV => v(fs::readv(a[0] as i32, a[1], a[2])),
        WRITEV => v(fs::writev(a[0] as i32, a[1], a[2])),
        IOCTL => v(fs::ioctl(a[0] as i32, a[1], a[2])),
        ACCESS => v(fs::faccessat(fs::AT_FDCWD, a[0], a[1] as u32)),
        FACCESSAT | FACCESSAT2 => v(fs::faccessat(a[0] as i32, a[1], a[2] as u32)),
        PIPE => v(fs::pipe2(a[0], 0)),
        PIPE2 => v(fs::pipe2(a[0], a[1] as u32)),
        DUP => v(fs::dup(a[0] as i32)),
        DUP2 => v(fs::dup3(a[0] as i32, a[1] as i32, 0, true)),
        DUP3 => v(fs::dup3(a[0] as i32, a[1] as i32, a[2] as u32, false)),
        FCNTL => v(fs::fcntl(a[0] as i32, a[1] as u32, a[2])),
        FLOCK => Ok(Ret::Value(0)),
        // Advice only; the page cache does its own readahead.
        FADVISE64 => Ok(Ret::Value(0)),
        FSYNC | FDATASYNC => v(fs::fsync(a[0] as i32)),
        SYNC => {
            crate::mm::pagecache::sync_all();
            crate::vfs::sync_all();
            crate::block::sync_all();
            Ok(Ret::Value(0))
        }
        TRUNCATE => v(fs::truncate(a[0], a[1])),
        FTRUNCATE => v(fs::ftruncate(a[0] as i32, a[1])),
        FALLOCATE => v(fs::fallocate(a[0] as i32, a[1] as u32, a[2], a[3])),
        GETDENTS | GETDENTS64 => v(fs::getdents64(a[0] as i32, a[1], a[2])),
        GETCWD => v(fs::getcwd(a[0], a[1])),
        CHDIR => v(fs::chdir(a[0])),
        FCHDIR => v(fs::fchdir(a[0] as i32)),
        RENAME => v(fs::renameat(fs::AT_FDCWD, a[0], fs::AT_FDCWD, a[1])),
        RENAMEAT | RENAMEAT2 => v(fs::renameat(a[0] as i32, a[1], a[2] as i32, a[3])),
        MKDIR => v(fs::mkdirat(fs::AT_FDCWD, a[0], a[1] as u32)),
        MKDIRAT => v(fs::mkdirat(a[0] as i32, a[1], a[2] as u32)),
        RMDIR => v(fs::unlinkat(fs::AT_FDCWD, a[0], fs::AT_REMOVEDIR)),
        UNLINK => v(fs::unlinkat(fs::AT_FDCWD, a[0], 0)),
        UNLINKAT => v(fs::unlinkat(a[0] as i32, a[1], a[2] as u32)),
        LINK => v(fs::linkat(fs::AT_FDCWD, a[0], fs::AT_FDCWD, a[1])),
        LINKAT => v(fs::linkat(a[0] as i32, a[1], a[2] as i32, a[3])),
        SYMLINK => v(fs::symlinkat(a[0], fs::AT_FDCWD, a[1])),
        SYMLINKAT => v(fs::symlinkat(a[0], a[1] as i32, a[2])),
        READLINK => v(fs::readlinkat(fs::AT_FDCWD, a[0], a[1], a[2])),
        READLINKAT => v(fs::readlinkat(a[0] as i32, a[1], a[2], a[3])),
        CHMOD => v(fs::fchmodat(fs::AT_FDCWD, a[0], a[1] as u32)),
        FCHMODAT => v(fs::fchmodat(a[0] as i32, a[1], a[2] as u32)),
        FCHMOD => v(fs::fchmod(a[0] as i32, a[1] as u32)),
        CHOWN | LCHOWN => v(fs::fchownat(fs::AT_FDCWD, a[0], a[1] as u32, a[2] as u32)),
        FCHOWNAT => v(fs::fchownat(a[0] as i32, a[1], a[2] as u32, a[3] as u32)),
        FCHOWN => Ok(Ret::Value(0)),
        UTIMES => v(fs::utimensat(fs::AT_FDCWD, a[0], 0)),
        UTIMENSAT => v(fs::utimensat(a[0] as i32, a[1], a[2])),
        MKNOD => v(fs::mknod(a[0], a[1] as u32)),
        STATFS => v(fs::statfs(a[0], a[1])),
        FSTATFS => v(fs::fstatfs(a[0] as i32, a[1])),
        QUOTACTL => v(fs::quotactl(a[0] as u32, a[1], a[2] as u32, a[3])),
        GETXATTR => v(fs::getxattr(a[0], a[1], a[2], a[3], true)),
        LGETXATTR => v(fs::getxattr(a[0], a[1], a[2], a[3], false)),
        FGETXATTR => v(fs::fgetxattr(a[0] as i32, a[1], a[2], a[3])),
        LISTXATTR => v(fs::listxattr(a[0], a[1], a[2], true)),
        LLISTXATTR => v(fs::listxattr(a[0], a[1], a[2], false)),
        FLISTXATTR => v(fs::flistxattr(a[0] as i32, a[1], a[2])),
        POLL => v(fs::poll(a[0], a[1], a[2] as i32 as i64)),
        PPOLL => {
            wait_sigmask(a[3], a[4])?;
            v(fs::ppoll(a[0], a[1], a[2]))
        }
        SELECT => v(fs::select(a[0] as i32, a[1], a[2], a[3], a[4], false)),
        PSELECT6 => {
            // The sixth argument points at { const sigset_t *ss; size_t len; }.
            if a[5] != 0 {
                let ss: [u64; 2] = crate::process::uaccess::read_user(a[5])?;
                wait_sigmask(ss[0], ss[1])?;
            }
            v(fs::select(a[0] as i32, a[1], a[2], a[3], a[4], true))
        }
        SENDFILE => v(fs::sendfile(a[0] as i32, a[1] as i32, a[2], a[3])),
        MOUNT => v(fs::mount(a[0], a[1], a[2], a[3], a[4])),
        UMOUNT2 => v(fs::umount(a[0])),
        MEMFD_CREATE => v(fdobj::memfd_create(a[0], a[1] as u32)),
        INOTIFY_INIT => v(fdobj::inotify_init1(0)),
        INOTIFY_INIT1 => v(fdobj::inotify_init1(a[0] as u32)),
        INOTIFY_ADD_WATCH => v(fdobj::inotify_add_watch(a[0] as i32, a[1], a[2] as u32)),
        INOTIFY_RM_WATCH => v(fdobj::inotify_rm_watch(a[0] as i32, a[1] as i32)),
        CLOSE_RANGE => v(fdobj::close_range(a[0] as u32, a[1] as u32, a[2] as u32)),
        COPY_FILE_RANGE => v(fdobj::copy_file_range(
            a[0] as i32,
            a[1],
            a[2] as i32,
            a[3],
            a[4],
            a[5] as u32,
        )),
        PIDFD_OPEN => v(fdobj::pidfd_open(a[0] as i32, a[1] as u32)),
        PIDFD_SEND_SIGNAL => v(fdobj::pidfd_send_signal(
            a[0] as i32,
            a[1] as u32,
            a[2],
            a[3] as u32,
        )),

        // Memory
        MMAP => v(mem::mmap(
            a[0],
            a[1],
            a[2] as u32,
            a[3] as u32,
            a[4] as i32,
            a[5],
        )),
        MUNMAP => v(mem::munmap(a[0], a[1])),
        MSYNC => v(mem::msync(a[0], a[1], a[2] as u32)),
        MREMAP => v(mem::mremap(a[0], a[1], a[2], a[3] as u32)),
        MEMBARRIER => v(mem::membarrier(a[0] as u32)),
        MPROTECT => v(mem::mprotect(a[0], a[1], a[2] as u32)),
        BRK => v(mem::brk(a[0])),
        MADVISE => Ok(Ret::Value(0)),

        // Processes
        FORK | VFORK => v(proc_::fork(frame)),
        CLONE => v(proc_::clone(frame, a[0], a[1], a[2], a[3], a[4])),
        EXECVE => proc_::execve(frame, a[0], a[1], a[2]).map(|_| Ret::Frame),
        EXIT => proc_::exit(a[0] as i32),
        EXIT_GROUP => proc_::exit_group(a[0] as i32),
        WAIT4 => v(proc_::wait4(a[0] as i32, a[1], a[2] as u32)),
        WAITID => v(proc_::waitid(a[0] as u32, a[1], a[2], a[3] as u32, a[4])),
        KILL => v(proc_::kill(a[0] as i32, a[1] as u32)),
        TKILL => v(proc_::tkill(a[0], a[1] as u32)),
        TGKILL => v(proc_::tkill(a[1], a[2] as u32)),
        GETPID => v(proc_::getpid()),
        GETPPID => v(proc_::getppid()),
        GETTID => Ok(Ret::Value(crate::sched::current_tid() as i64)),
        GETPGRP => v(proc_::getpgid(0)),
        GETPGID => v(proc_::getpgid(a[0] as u32)),
        SETPGID => v(proc_::setpgid(a[0] as u32, a[1] as u32)),
        GETSID => v(proc_::getsid(a[0] as u32)),
        SETSID => v(proc_::setsid()),
        GETUID | GETEUID | GETGID | GETEGID => Ok(Ret::Value(0)),
        // Everything runs as root: changing ids succeeds and changes nothing.
        SETUID | SETGID | SETREUID | SETREGID | SETRESUID | SETRESGID => Ok(Ret::Value(0)),
        GETRESUID | GETRESGID => {
            for p in [a[0], a[1], a[2]] {
                crate::process::uaccess::write_user(p, &0u32)?;
            }
            Ok(Ret::Value(0))
        }
        GETGROUPS => Ok(Ret::Value(0)),
        UMASK => v(proc_::umask(a[0] as u32)),
        RT_SIGACTION => v(proc_::sigaction(a[0] as u32, a[1], a[2])),
        RT_SIGPROCMASK => v(proc_::sigprocmask(a[0] as u32, a[1], a[2])),
        RT_SIGPENDING => v(proc_::sigpending(a[0])),
        RT_SIGSUSPEND => v(proc_::sigsuspend(a[0])),
        RT_SIGRETURN => crate::process::signal::sigreturn(frame).map(|_| Ret::Frame),
        SIGALTSTACK => Ok(Ret::Value(0)),
        PAUSE => v(proc_::pause()),
        ARCH_PRCTL => v(proc_::arch_prctl(a[0], a[1])),
        PRCTL => Ok(Ret::Value(0)),
        SET_TID_ADDRESS => v(proc_::set_tid_address(a[0])),
        SET_ROBUST_LIST => Ok(Ret::Value(0)),
        FUTEX => v(crate::process::futex::futex(
            a[0],
            a[1] as u32,
            a[2] as u32,
            a[3],
            a[4],
            a[5] as u32,
        )),
        SCHED_YIELD => {
            crate::sched::yield_now();
            Ok(Ret::Value(0))
        }
        GETRLIMIT | PRLIMIT64 => v(proc_::getrlimit(n, a)),
        GETRUSAGE => v(proc_::getrusage(a[0] as i32 as i64, a[1])),
        TIMES => v(proc_::times(a[0])),

        // Time
        NANOSLEEP => v(misc::nanosleep(a[0], a[1])),
        CLOCK_NANOSLEEP => v(misc::clock_nanosleep(a[0] as u32, a[1] as u32, a[2], a[3])),
        CLOCK_GETTIME => v(misc::clock_gettime(a[0] as u32, a[1])),
        CLOCK_GETRES => v(misc::clock_getres(a[1])),
        GETTIMEOFDAY => v(misc::gettimeofday(a[0])),
        SETTIMEOFDAY => v(misc::settimeofday(a[0])),
        CLOCK_SETTIME => v(misc::clock_settime(a[0] as u32, a[1])),
        TIME => v(misc::time(a[0])),
        ALARM => v(misc::alarm(a[0])),
        GETITIMER => v(misc::getitimer(a[0], a[1])),
        SETITIMER => v(misc::setitimer(a[0], a[1], a[2])),
        GETPRIORITY => v(proc_::getpriority(a[0] as u32, a[1] as u32)),
        SETPRIORITY => v(proc_::setpriority(a[0] as u32, a[1] as u32, a[2] as i32)),
        SCHED_SETAFFINITY => v(proc_::sched_setaffinity(a[0] as u32, a[1], a[2])),
        SCHED_GETAFFINITY => v(proc_::sched_getaffinity(a[0] as u32, a[1], a[2])),
        SCHED_GETSCHEDULER | SCHED_GET_PRIORITY_MAX | SCHED_GET_PRIORITY_MIN => Ok(Ret::Value(0)),
        SCHED_GETPARAM => {
            crate::process::uaccess::write_user(a[1], &0u32)?;
            Ok(Ret::Value(0))
        }
        EPOLL_CREATE => v(if (a[0] as i32) <= 0 {
            Err(EINVAL)
        } else {
            event::epoll_create1(0)
        }),
        EPOLL_CREATE1 => v(event::epoll_create1(a[0] as u32)),
        EPOLL_CTL => v(event::epoll_ctl(
            a[0] as i32,
            a[1] as u32,
            a[2] as i32,
            a[3],
        )),
        EPOLL_WAIT => v(event::epoll_wait(
            a[0] as i32,
            a[1],
            a[2] as i32,
            a[3] as i32 as i64,
        )),
        EPOLL_PWAIT => {
            wait_sigmask(a[4], a[5])?;
            v(event::epoll_wait(
                a[0] as i32,
                a[1],
                a[2] as i32,
                a[3] as i32 as i64,
            ))
        }
        EPOLL_PWAIT2 => {
            wait_sigmask(a[4], a[5])?;
            v(event::epoll_pwait2(a[0] as i32, a[1], a[2] as i32, a[3]))
        }
        EVENTFD => v(event::eventfd2(a[0], 0)),
        EVENTFD2 => v(event::eventfd2(a[0], a[1] as u32)),
        TIMERFD_CREATE => v(event::timerfd_create(a[0], a[1] as u32)),
        TIMERFD_SETTIME => v(event::timerfd_settime(a[0] as i32, a[1] as u32, a[2], a[3])),
        TIMERFD_GETTIME => v(event::timerfd_gettime(a[0] as i32, a[1])),
        SIGNALFD => v(event::signalfd4(a[0] as i32, a[1], a[2], 0)),
        SIGNALFD4 => v(event::signalfd4(a[0] as i32, a[1], a[2], a[3] as u32)),

        // System
        UNAME => v(misc::uname(a[0])),
        SYSINFO => v(misc::sysinfo(a[0])),
        SYSLOG => v(misc::syslog(a[0] as u32, a[1], a[2])),
        REBOOT => v(misc::reboot(a[0] as u32, a[1] as u32, a[2] as u32)),
        SETHOSTNAME => v(misc::sethostname(a[0], a[1])),
        GETRANDOM => v(misc::getrandom(a[0], a[1])),

        // Sockets (implemented by the network stack).
        SOCKET | CONNECT | ACCEPT | ACCEPT4 | SENDTO | RECVFROM | SENDMSG | RECVMSG | SHUTDOWN
        | BIND | LISTEN | GETSOCKNAME | GETPEERNAME | SOCKETPAIR | SETSOCKOPT | GETSOCKOPT => {
            v(crate::net::syscalls::dispatch(n, a))
        }

        _ => {
            crate::serial_println!(
                "[syscall] pid {} unimplemented syscall {}",
                crate::process::current_pid(),
                n
            );
            Err(ENOSYS)
        }
    }
}
