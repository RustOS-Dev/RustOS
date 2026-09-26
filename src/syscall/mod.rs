//! System call dispatch (Linux x86_64 numbering).
//!
//! Arguments arrive in rdi, rsi, rdx, r10, r8, r9 with the number in rax,
//! through either `syscall` or `int 0x80`. The result (or `-errno`) is
//! returned in rax.

mod fs;
mod mem;
mod misc;
mod proc_;

use crate::arch::x86_64::idt::TrapFrame;
use crate::errno::*;

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
    pub const KILL: u64 = 62;
    pub const UNAME: u64 = 63;
    pub const FCNTL: u64 = 72;
    pub const FLOCK: u64 = 73;
    pub const FSYNC: u64 = 74;
    pub const FDATASYNC: u64 = 75;
    pub const TRUNCATE: u64 = 76;
    pub const FTRUNCATE: u64 = 77;
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
}

/// Entry point from both syscall paths.
pub fn dispatch(frame: &mut TrapFrame) {
    x86_64::instructions::interrupts::enable();
    let n = frame.rax;
    let (a1, a2, a3, a4, a5, a6) = (
        frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
    );
    if TRACE.load(core::sync::atomic::Ordering::Relaxed) {
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
    let r = handle(frame, n, [a1, a2, a3, a4, a5, a6]);
    if TRACE.load(core::sync::atomic::Ordering::Relaxed) {
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

pub enum Ret {
    Value(i64),
    Frame,
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
        FACCESSAT => v(fs::faccessat(a[0] as i32, a[1], a[2] as u32)),
        PIPE => v(fs::pipe2(a[0], 0)),
        PIPE2 => v(fs::pipe2(a[0], a[1] as u32)),
        DUP => v(fs::dup(a[0] as i32)),
        DUP2 => v(fs::dup3(a[0] as i32, a[1] as i32, 0, true)),
        DUP3 => v(fs::dup3(a[0] as i32, a[1] as i32, a[2] as u32, false)),
        FCNTL => v(fs::fcntl(a[0] as i32, a[1] as u32, a[2])),
        FLOCK => Ok(Ret::Value(0)),
        FSYNC | FDATASYNC => v(fs::fsync(a[0] as i32)),
        SYNC => {
            crate::vfs::sync_all();
            crate::block::sync_all();
            Ok(Ret::Value(0))
        }
        TRUNCATE => v(fs::truncate(a[0], a[1])),
        FTRUNCATE => v(fs::ftruncate(a[0] as i32, a[1])),
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
        POLL => v(fs::poll(a[0], a[1], a[2] as i32 as i64)),
        PPOLL => v(fs::ppoll(a[0], a[1], a[2])),
        SELECT => v(fs::select(a[0] as i32, a[1], a[2], a[3], a[4], false)),
        PSELECT6 => v(fs::select(a[0] as i32, a[1], a[2], a[3], a[4], true)),
        SENDFILE => v(fs::sendfile(a[0] as i32, a[1] as i32, a[2], a[3])),
        MOUNT => v(fs::mount(a[0], a[1], a[2], a[3])),
        UMOUNT2 => v(fs::umount(a[0])),

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
        MPROTECT => v(mem::mprotect(a[0], a[1], a[2] as u32)),
        BRK => v(mem::brk(a[0])),
        MADVISE => Ok(Ret::Value(0)),

        // Processes
        FORK | VFORK => v(proc_::fork(frame)),
        CLONE => v(proc_::clone(frame, a[0], a[1], a[2], a[3], a[4])),
        EXECVE => proc_::execve(frame, a[0], a[1], a[2]).map(|_| Ret::Frame),
        EXIT => proc_::exit(a[0] as i32),
        EXIT_GROUP => proc_::exit(a[0] as i32),
        WAIT4 => v(proc_::wait4(a[0] as i32, a[1], a[2] as u32)),
        KILL => v(proc_::kill(a[0] as i32, a[1] as u32)),
        TKILL => v(proc_::kill(a[0] as i32, a[1] as u32)),
        TGKILL => v(proc_::kill(a[0] as i32, a[2] as u32)),
        GETPID => v(proc_::getpid()),
        GETPPID => v(proc_::getppid()),
        GETTID => Ok(Ret::Value(crate::sched::current_tid() as i64)),
        GETPGRP => v(proc_::getpgid(0)),
        GETPGID => v(proc_::getpgid(a[0] as u32)),
        SETPGID => v(proc_::setpgid(a[0] as u32, a[1] as u32)),
        GETSID => v(proc_::getsid(a[0] as u32)),
        SETSID => v(proc_::setsid()),
        GETUID | GETEUID | GETGID | GETEGID => Ok(Ret::Value(0)),
        SETUID | SETGID => Ok(Ret::Value(0)),
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
        FUTEX => v(proc_::futex(a[0], a[1] as u32, a[2] as u32, a[3])),
        SCHED_YIELD => {
            crate::sched::yield_now();
            Ok(Ret::Value(0))
        }
        GETRLIMIT | PRLIMIT64 => v(proc_::getrlimit(n, a)),
        GETRUSAGE => v(proc_::getrusage(a[1])),
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
