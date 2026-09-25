//! Signal handling.

use crate::sys::{self, nr};

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGKILL: i32 = 9;
pub const SIGUSR1: i32 = 10;
pub const SIGSEGV: i32 = 11;
pub const SIGUSR2: i32 = 12;
pub const SIGPIPE: i32 = 13;
pub const SIGALRM: i32 = 14;
pub const SIGTERM: i32 = 15;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;
pub const SIGTSTP: i32 = 20;
pub const SIGTTIN: i32 = 21;
pub const SIGTTOU: i32 = 22;
pub const SIGWINCH: i32 = 28;

pub const SIG_DFL: usize = 0;
pub const SIG_IGN: usize = 1;

const SA_RESTORER: u64 = 0x0400_0000;

#[repr(C)]
struct SigAction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

core::arch::global_asm!(
    ".globl __rustos_sigreturn",
    "__rustos_sigreturn:",
    "    mov rax, 15",
    "    syscall",
    "    ud2",
);

unsafe extern "C" {
    fn __rustos_sigreturn();
}

fn set(sig: i32, handler: usize) -> usize {
    let act = SigAction {
        handler,
        flags: SA_RESTORER,
        restorer: __rustos_sigreturn as *const () as usize,
        mask: 0,
    };
    let mut old = SigAction {
        handler: 0,
        flags: 0,
        restorer: 0,
        mask: 0,
    };
    sys::syscall(
        nr::RT_SIGACTION,
        &[
            sig as usize,
            &act as *const _ as usize,
            &mut old as *mut _ as usize,
            8,
        ],
    );
    old.handler
}

/// Install a handler `fn(sig)`.
pub fn handle(sig: i32, f: extern "C" fn(i32)) {
    set(sig, f as usize);
}

pub fn ignore(sig: i32) {
    set(sig, SIG_IGN);
}

pub fn default(sig: i32) {
    set(sig, SIG_DFL);
}

/// Block (`how` = 0), unblock (1) or set (2) the signal mask.
pub fn mask(how: i32, set: u64) -> u64 {
    let mut old = 0u64;
    sys::syscall(
        nr::RT_SIGPROCMASK,
        &[
            how as usize,
            &set as *const u64 as usize,
            &mut old as *mut u64 as usize,
            8,
        ],
    );
    old
}

pub fn bit(sig: i32) -> u64 {
    1u64 << (sig - 1)
}

pub fn name(sig: i32) -> &'static str {
    match sig {
        SIGHUP => "Hangup",
        SIGINT => "Interrupt",
        SIGQUIT => "Quit",
        4 => "Illegal instruction",
        5 => "Trace/breakpoint trap",
        6 => "Aborted",
        7 => "Bus error",
        8 => "Floating point exception",
        SIGKILL => "Killed",
        SIGSEGV => "Segmentation fault",
        SIGPIPE => "Broken pipe",
        SIGALRM => "Alarm clock",
        SIGTERM => "Terminated",
        SIGSTOP | SIGTSTP => "Stopped",
        SIGTTIN => "Stopped (tty input)",
        SIGTTOU => "Stopped (tty output)",
        _ => "Signal",
    }
}

/// Parse "9", "KILL", "SIGKILL", "-9", ...
pub fn parse(s: &str) -> Option<i32> {
    let s = s.trim_start_matches('-');
    if let Ok(n) = s.parse() {
        return Some(n);
    }
    let s = s.strip_prefix("SIG").unwrap_or(s);
    Some(match s {
        "HUP" => SIGHUP,
        "INT" => SIGINT,
        "QUIT" => SIGQUIT,
        "KILL" => SIGKILL,
        "USR1" => SIGUSR1,
        "SEGV" => SIGSEGV,
        "USR2" => SIGUSR2,
        "PIPE" => SIGPIPE,
        "ALRM" => SIGALRM,
        "TERM" => SIGTERM,
        "CHLD" => SIGCHLD,
        "CONT" => SIGCONT,
        "STOP" => SIGSTOP,
        "TSTP" => SIGTSTP,
        _ => return None,
    })
}
