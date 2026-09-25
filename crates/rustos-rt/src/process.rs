//! Processes: fork/exec/wait, pipes, descriptors, process groups.

use crate::Result;
use crate::sys::{self, cstr, nr};
use alloc::string::String;
use alloc::vec::Vec;

pub fn exit(code: i32) -> ! {
    crate::io::flush();
    loop {
        sys::syscall(nr::EXIT_GROUP, &[code as usize]);
    }
}

pub fn getpid() -> i32 {
    sys::syscall(nr::GETPID, &[]) as i32
}

pub fn getppid() -> i32 {
    sys::syscall(nr::GETPPID, &[]) as i32
}

pub fn fork() -> Result<i32> {
    crate::io::flush();
    sys::check(sys::syscall(nr::FORK, &[])).map(|v| v as i32)
}

fn cstr_array(v: &[String]) -> (Vec<Vec<u8>>, Vec<usize>) {
    let owned: Vec<Vec<u8>> = v.iter().map(|s| cstr(s)).collect();
    let mut ptrs: Vec<usize> = owned.iter().map(|s| s.as_ptr() as usize).collect();
    ptrs.push(0);
    (owned, ptrs)
}

/// Replace the current program. Only returns on failure.
pub fn execve(path: &str, argv: &[String], envp: &[String]) -> crate::Error {
    let p = cstr(path);
    let (_a, ap) = cstr_array(argv);
    let (_e, ep) = cstr_array(envp);
    let r = sys::syscall(
        nr::EXECVE,
        &[
            p.as_ptr() as usize,
            ap.as_ptr() as usize,
            ep.as_ptr() as usize,
        ],
    );
    crate::Error((-r) as i32)
}

/// Search `$PATH` for an executable.
pub fn find_in_path(cmd: &str) -> Option<String> {
    if cmd.contains('/') {
        return crate::fs::exists(cmd).then(|| String::from(cmd));
    }
    let path = crate::env::var("PATH").unwrap_or_else(|| String::from("/bin:/sbin:/usr/bin"));
    for dir in path.split(':') {
        let full = crate::fs::join(if dir.is_empty() { "." } else { dir }, cmd);
        if crate::fs::metadata(&full).is_ok_and(|m| m.is_file()) {
            return Some(full);
        }
    }
    None
}

pub const WNOHANG: i32 = 1;
pub const WUNTRACED: i32 = 2;
pub const WCONTINUED: i32 = 8;

/// Wait for a child. Returns (pid, raw status).
pub fn waitpid(pid: i32, options: i32) -> Result<(i32, i32)> {
    let mut status = 0i32;
    let r = sys::check(sys::syscall(
        nr::WAIT4,
        &[
            pid as usize,
            &mut status as *mut i32 as usize,
            options as usize,
            0,
        ],
    ))?;
    Ok((r as i32, status))
}

pub fn wifexited(s: i32) -> bool {
    s & 0x7f == 0
}
pub fn wexitstatus(s: i32) -> i32 {
    (s >> 8) & 0xff
}
pub fn wifsignaled(s: i32) -> bool {
    ((s & 0x7f) + 1) as i8 >= 2
}
pub fn wtermsig(s: i32) -> i32 {
    s & 0x7f
}
pub fn wifstopped(s: i32) -> bool {
    s & 0xff == 0x7f
}
pub fn wstopsig(s: i32) -> i32 {
    (s >> 8) & 0xff
}

/// Shell-style exit code for a wait status.
pub fn status_code(s: i32) -> i32 {
    if wifexited(s) {
        wexitstatus(s)
    } else if wifstopped(s) {
        128 + wstopsig(s)
    } else {
        128 + wtermsig(s)
    }
}

pub fn pipe() -> Result<(i32, i32)> {
    let mut fds = [0i32; 2];
    sys::check(sys::syscall(nr::PIPE2, &[fds.as_mut_ptr() as usize, 0]))?;
    Ok((fds[0], fds[1]))
}

pub fn dup(fd: i32) -> Result<i32> {
    sys::check(sys::syscall(nr::DUP, &[fd as usize])).map(|v| v as i32)
}

pub fn dup2(old: i32, new: i32) -> Result<i32> {
    sys::check(sys::syscall(nr::DUP2, &[old as usize, new as usize])).map(|v| v as i32)
}

pub fn close(fd: i32) {
    sys::syscall(nr::CLOSE, &[fd as usize]);
}

/// Set or clear FD_CLOEXEC.
pub fn set_cloexec(fd: i32, on: bool) {
    sys::syscall(nr::FCNTL, &[fd as usize, 2, on as usize]);
}

pub fn kill(pid: i32, sig: i32) -> Result<()> {
    sys::check(sys::syscall(nr::KILL, &[pid as usize, sig as usize])).map(|_| ())
}

pub fn setpgid(pid: i32, pgid: i32) -> Result<()> {
    sys::check(sys::syscall(nr::SETPGID, &[pid as usize, pgid as usize])).map(|_| ())
}

pub fn getpgrp() -> i32 {
    sys::syscall(nr::GETPGRP, &[]) as i32
}

pub fn getpgid(pid: i32) -> i32 {
    sys::syscall(nr::GETPGID, &[pid as usize]) as i32
}

pub fn setsid() -> Result<i32> {
    sys::check(sys::syscall(nr::SETSID, &[])).map(|v| v as i32)
}

pub fn umask(mask: u32) -> u32 {
    sys::syscall(nr::UMASK, &[mask as usize]) as u32
}

/// Spawn `argv` with optional stdin/stdout replacements; returns the pid.
pub fn spawn(argv: &[String], stdin: Option<i32>, stdout: Option<i32>) -> Result<i32> {
    let path = find_in_path(&argv[0]).ok_or(crate::Error(2))?;
    let env = crate::env::environ();
    let pid = fork()?;
    if pid == 0 {
        crate::io::discard_buffered();
        if let Some(fd) = stdin {
            let _ = dup2(fd, 0);
        }
        if let Some(fd) = stdout {
            let _ = dup2(fd, 1);
        }
        let e = execve(&path, argv, &env);
        crate::eprintln!("{}: {}", argv[0], e);
        exit(127);
    }
    Ok(pid)
}

/// Run a command and wait; returns its exit code.
pub fn run(argv: &[&str]) -> Result<i32> {
    let v: Vec<String> = argv.iter().map(|s| String::from(*s)).collect();
    let pid = spawn(&v, None, None)?;
    let (_, st) = waitpid(pid, 0)?;
    Ok(status_code(st))
}

/// Run a command and capture its stdout.
pub fn output(argv: &[&str]) -> Result<(i32, Vec<u8>)> {
    let v: Vec<String> = argv.iter().map(|s| String::from(*s)).collect();
    let (r, w) = pipe()?;
    let pid = spawn(&v, None, Some(w))?;
    close(w);
    let f = crate::fs::File::from_raw(r);
    let out = f.read_to_end()?;
    let (_, st) = waitpid(pid, 0)?;
    Ok((status_code(st), out))
}

#[derive(Debug, Clone, Default)]
pub struct Uname {
    pub sysname: String,
    pub nodename: String,
    pub release: String,
    pub version: String,
    pub machine: String,
}

pub fn uname() -> Uname {
    let mut raw = [[0u8; 65]; 6];
    sys::syscall(nr::UNAME, &[raw.as_mut_ptr() as usize]);
    let s = |i: usize| {
        let end = raw[i].iter().position(|&b| b == 0).unwrap_or(65);
        String::from_utf8_lossy(&raw[i][..end]).into_owned()
    };
    Uname {
        sysname: s(0),
        nodename: s(1),
        release: s(2),
        version: s(3),
        machine: s(4),
    }
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct SysInfo {
    pub uptime: i64,
    pub loads: [u64; 3],
    pub totalram: u64,
    pub freeram: u64,
    pub sharedram: u64,
    pub bufferram: u64,
    pub totalswap: u64,
    pub freeswap: u64,
    pub procs: u16,
    pub _pad: [u8; 6],
    pub totalhigh: u64,
    pub freehigh: u64,
    pub mem_unit: u32,
    pub _f: [u8; 4],
}

pub fn sysinfo() -> SysInfo {
    let mut s = SysInfo::default();
    sys::syscall(nr::SYSINFO, &[&mut s as *mut _ as usize]);
    s
}

pub fn reboot(cmd: u32) -> Result<()> {
    crate::fs::sync();
    sys::check(sys::syscall(
        nr::REBOOT,
        &[0xfee1_dead, 672274793, cmd as usize],
    ))
    .map(|_| ())
}

pub const REBOOT_RESTART: u32 = 0x0123_4567;
pub const REBOOT_POWER_OFF: u32 = 0x4321_FEDC;

pub fn getrandom(buf: &mut [u8]) {
    sys::syscall(nr::GETRANDOM, &[buf.as_mut_ptr() as usize, buf.len(), 0]);
}
