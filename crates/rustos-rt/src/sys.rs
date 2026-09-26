//! Raw system calls (Linux x86_64 numbering, `syscall` instruction).

#![allow(clippy::too_many_arguments)]

use core::arch::asm;

pub mod nr {
    pub const READ: usize = 0;
    pub const WRITE: usize = 1;
    pub const OPEN: usize = 2;
    pub const CLOSE: usize = 3;
    pub const STAT: usize = 4;
    pub const FSTAT: usize = 5;
    pub const LSTAT: usize = 6;
    pub const POLL: usize = 7;
    pub const SETTIMEOFDAY: usize = 164;
    pub const LSEEK: usize = 8;
    pub const MMAP: usize = 9;
    pub const MPROTECT: usize = 10;
    pub const MUNMAP: usize = 11;
    pub const BRK: usize = 12;
    pub const RT_SIGACTION: usize = 13;
    pub const RT_SIGPROCMASK: usize = 14;
    pub const RT_SIGRETURN: usize = 15;
    pub const IOCTL: usize = 16;
    pub const PREAD64: usize = 17;
    pub const PWRITE64: usize = 18;
    pub const ACCESS: usize = 21;
    pub const PIPE: usize = 22;
    pub const SCHED_YIELD: usize = 24;
    pub const DUP: usize = 32;
    pub const DUP2: usize = 33;
    pub const PAUSE: usize = 34;
    pub const NANOSLEEP: usize = 35;
    pub const GETPID: usize = 39;
    pub const SOCKET: usize = 41;
    pub const CONNECT: usize = 42;
    pub const ACCEPT: usize = 43;
    pub const SENDTO: usize = 44;
    pub const RECVFROM: usize = 45;
    pub const SHUTDOWN: usize = 48;
    pub const BIND: usize = 49;
    pub const LISTEN: usize = 50;
    pub const GETSOCKNAME: usize = 51;
    pub const GETPEERNAME: usize = 52;
    pub const SETSOCKOPT: usize = 54;
    pub const GETSOCKOPT: usize = 55;
    pub const FORK: usize = 57;
    pub const EXECVE: usize = 59;
    pub const EXIT: usize = 60;
    pub const WAIT4: usize = 61;
    pub const KILL: usize = 62;
    pub const UNAME: usize = 63;
    pub const FCNTL: usize = 72;
    pub const FSYNC: usize = 74;
    pub const FTRUNCATE: usize = 77;
    pub const GETCWD: usize = 79;
    pub const CHDIR: usize = 80;
    pub const RENAME: usize = 82;
    pub const MKDIR: usize = 83;
    pub const RMDIR: usize = 84;
    pub const LINK: usize = 86;
    pub const UNLINK: usize = 87;
    pub const SYMLINK: usize = 88;
    pub const READLINK: usize = 89;
    pub const CHMOD: usize = 90;
    pub const UMASK: usize = 95;
    pub const GETTIMEOFDAY: usize = 96;
    pub const SYSINFO: usize = 99;
    pub const SYSLOG: usize = 103;
    pub const SETPGID: usize = 109;
    pub const GETPPID: usize = 110;
    pub const GETPGRP: usize = 111;
    pub const SETSID: usize = 112;
    pub const GETPGID: usize = 121;
    pub const MKNOD: usize = 133;
    pub const STATFS: usize = 137;
    pub const ARCH_PRCTL: usize = 158;
    pub const SYNC: usize = 162;
    pub const MOUNT: usize = 165;
    pub const UMOUNT2: usize = 166;
    pub const REBOOT: usize = 169;
    pub const SETHOSTNAME: usize = 170;
    pub const GETTID: usize = 186;
    pub const GETDENTS64: usize = 217;
    pub const CLOCK_GETTIME: usize = 228;
    pub const EXIT_GROUP: usize = 231;
    pub const UTIMENSAT: usize = 280;
    pub const PIPE2: usize = 293;
    pub const GETRANDOM: usize = 318;
}

#[inline(always)]
pub unsafe fn syscall6(
    n: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
) -> isize {
    let ret: isize;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") n as isize => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            in("r9") a6,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
pub fn syscall(n: usize, a: &[usize]) -> isize {
    let g = |i: usize| a.get(i).copied().unwrap_or(0);
    unsafe { syscall6(n, g(0), g(1), g(2), g(3), g(4), g(5)) }
}

/// Convert a raw return value into a Result.
pub fn check(r: isize) -> crate::Result<usize> {
    if (-4095..0).contains(&r) {
        Err(crate::Error((-r) as i32))
    } else {
        Ok(r as usize)
    }
}

pub fn brk(addr: usize) -> usize {
    syscall(nr::BRK, &[addr]) as usize
}

/// A NUL-terminated copy of `s` for passing to the kernel.
pub fn cstr(s: &str) -> alloc::vec::Vec<u8> {
    let mut v = alloc::vec::Vec::with_capacity(s.len() + 1);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

pub fn strerror(e: i32) -> &'static str {
    match e {
        1 => "Operation not permitted",
        2 => "No such file or directory",
        3 => "No such process",
        4 => "Interrupted system call",
        5 => "I/O error",
        6 => "No such device or address",
        7 => "Argument list too long",
        8 => "Exec format error",
        9 => "Bad file descriptor",
        10 => "No child processes",
        11 => "Resource temporarily unavailable",
        12 => "Cannot allocate memory",
        13 => "Permission denied",
        14 => "Bad address",
        16 => "Device or resource busy",
        17 => "File exists",
        18 => "Invalid cross-device link",
        19 => "No such device",
        20 => "Not a directory",
        21 => "Is a directory",
        22 => "Invalid argument",
        24 => "Too many open files",
        25 => "Inappropriate ioctl for device",
        27 => "File too large",
        28 => "No space left on device",
        29 => "Illegal seek",
        30 => "Read-only file system",
        32 => "Broken pipe",
        34 => "Numerical result out of range",
        36 => "File name too long",
        38 => "Function not implemented",
        39 => "Directory not empty",
        40 => "Too many levels of symbolic links",
        61 => "No data available",
        88 => "Socket operation on non-socket",
        95 => "Operation not supported",
        97 => "Address family not supported by protocol",
        98 => "Address already in use",
        99 => "Cannot assign requested address",
        100 => "Network is down",
        101 => "Network is unreachable",
        103 => "Software caused connection abort",
        104 => "Connection reset by peer",
        106 => "Transport endpoint is already connected",
        107 => "Transport endpoint is not connected",
        110 => "Connection timed out",
        111 => "Connection refused",
        113 => "No route to host",
        114 => "Operation already in progress",
        115 => "Operation now in progress",
        _ => "Unknown error",
    }
}
