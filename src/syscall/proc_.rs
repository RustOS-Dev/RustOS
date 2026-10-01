//! Process, signal and credential syscalls.

use super::{Ret, SysResult};
use crate::arch::x86_64::idt::TrapFrame;
use crate::errno::*;
use crate::process::{self, Process, signal, uaccess};
use alloc::sync::Arc;
use core::sync::atomic::Ordering;

fn cur() -> KResult<Arc<Process>> {
    process::current().ok_or(ESRCH)
}

pub fn fork(frame: &TrapFrame) -> SysResult {
    Ok(process::fork(frame, true)? as i64)
}

const CLONE_VM: u64 = 0x100;
const CLONE_VFORK: u64 = 0x4000;
const CLONE_SETTLS: u64 = 0x80000;
const CLONE_THREAD: u64 = 0x10000;
const CLONE_CHILD_SETTID: u64 = 0x1000000;
const CLONE_CHILD_CLEARTID: u64 = 0x200000;
const CLONE_PARENT_SETTID: u64 = 0x100000;

pub fn clone(
    frame: &TrapFrame,
    flags: u64,
    stack: u64,
    ptid: u64,
    ctid: u64,
    tls: u64,
) -> SysResult {
    let ctid_arg = ctid;
    if flags & CLONE_VM != 0 && flags & CLONE_THREAD != 0 {
        let tls = (flags & CLONE_SETTLS != 0).then_some(tls);
        let ctid = if flags & CLONE_CHILD_SETTID != 0 {
            ctid
        } else {
            0
        };
        let clear = if flags & CLONE_CHILD_CLEARTID != 0 {
            ctid_arg
        } else {
            0
        };
        let tid = process::clone_thread(frame, stack, tls, ctid, clear)?;
        if flags & CLONE_PARENT_SETTID != 0 && ptid != 0 {
            uaccess::write_user(ptid, &(tid as u32))?;
        }
        return Ok(tid as i64);
    }
    // Everything else behaves like fork (vfork included). As on Linux, a
    // child sharing the address space (CLONE_VM without CLONE_VFORK) does
    // not inherit the alternate signal stack.
    let mut f = *frame;
    if stack != 0 {
        f.rsp = stack;
    }
    let keep_altstack = flags & (CLONE_VM | CLONE_VFORK) != CLONE_VM;
    Ok(process::fork(&f, keep_altstack)? as i64)
}

pub fn execve(frame: &mut TrapFrame, path: u64, argv: u64, envp: u64) -> KResult<()> {
    let path = uaccess::read_cstr(path, 4096)?;
    let argv = uaccess::read_str_array(argv)?;
    let envp = uaccess::read_str_array(envp)?;
    process::exec(frame, &path, argv, envp)
}

pub fn exit(code: i32) -> KResult<Ret> {
    process::exit_thread(process::exit_code_status(code));
}

pub fn exit_group(code: i32) -> KResult<Ret> {
    process::exit_current(process::exit_code_status(code));
}

pub fn wait4(pid: i32, status: u64, options: u32, rusage: u64) -> SysResult {
    let (cpid, st, (u, s)) = process::wait(pid, options)?;
    if status != 0 && cpid != 0 {
        uaccess::write_user(status, &st)?;
    }
    if rusage != 0 && cpid != 0 {
        uaccess::write_user(rusage, &Rusage::from_ticks(u, s, 0))?;
    }
    Ok(cpid as i64)
}

pub fn kill(pid: i32, sig: u32) -> SysResult {
    if sig as usize >= signal::NSIG {
        return Err(EINVAL);
    }
    let me = cur()?;
    let info = signal::SigInfo::from_process(signal::SI_USER, &me);
    match pid {
        p if p > 0 => {
            let target = process::find(p as u32).ok_or(ESRCH)?;
            if target.zombie.load(Ordering::SeqCst) {
                return Err(ESRCH);
            }
            if sig != 0 {
                signal::send_info(&target, sig, info);
            }
        }
        0 => {
            if signal::send_group_info(me.pgid.load(Ordering::SeqCst), sig, info) == 0 {
                return Err(ESRCH);
            }
        }
        -1 => {
            for p in process::all() {
                if p.pid > 1 && p.pid != me.pid {
                    signal::send_info(&p, sig, info);
                }
            }
        }
        p => {
            if signal::send_group_info((-p) as u32, sig, info) == 0 {
                return Err(ESRCH);
            }
        }
    }
    Ok(0)
}

pub fn getpid() -> SysResult {
    Ok(cur()?.pid as i64)
}

pub fn getppid() -> SysResult {
    Ok(cur()?.ppid.load(Ordering::SeqCst) as i64)
}

pub fn getpgid(pid: u32) -> SysResult {
    let p = if pid == 0 {
        cur()?
    } else {
        process::find(pid).ok_or(ESRCH)?
    };
    Ok(p.pgid.load(Ordering::SeqCst) as i64)
}

pub fn setpgid(pid: u32, pgid: u32) -> SysResult {
    let me = cur()?;
    let target = if pid == 0 || pid == me.pid {
        me.clone()
    } else {
        let t = process::find(pid).ok_or(ESRCH)?;
        if t.ppid.load(Ordering::SeqCst) != me.pid {
            return Err(ESRCH);
        }
        t
    };
    if target.sid.load(Ordering::SeqCst) == target.pid {
        return Err(EPERM);
    }
    let pgid = if pgid == 0 { target.pid } else { pgid };
    target.pgid.store(pgid, Ordering::SeqCst);
    Ok(0)
}

pub fn getsid(pid: u32) -> SysResult {
    let p = if pid == 0 {
        cur()?
    } else {
        process::find(pid).ok_or(ESRCH)?
    };
    Ok(p.sid.load(Ordering::SeqCst) as i64)
}

pub fn setsid() -> SysResult {
    let p = cur()?;
    if p.pgid.load(Ordering::SeqCst) == p.pid {
        return Err(EPERM);
    }
    p.sid.store(p.pid, Ordering::SeqCst);
    p.pgid.store(p.pid, Ordering::SeqCst);
    *p.ctty.lock() = None;
    Ok(p.pid as i64)
}

pub fn umask(mask: u32) -> SysResult {
    Ok(cur()?.umask.swap(mask & 0o777, Ordering::SeqCst) as i64)
}

pub fn sigaction(sig: u32, act: u64, old: u64) -> SysResult {
    if sig == 0 || sig as usize >= signal::NSIG {
        return Err(EINVAL);
    }
    let p = cur()?;
    let prev = p.signals.actions.lock()[sig as usize];
    if act != 0 {
        if sig == signal::SIGKILL || sig == signal::SIGSTOP {
            return Err(EINVAL);
        }
        let a: signal::SigAction = uaccess::read_user(act)?;
        p.signals.actions.lock()[sig as usize] = a;
        signal::action_changed(&p, sig);
    }
    if old != 0 {
        uaccess::write_user(old, &prev)?;
    }
    Ok(0)
}

/// rt_sigprocmask: the calling thread's mask.
pub fn sigprocmask(how: u32, set: u64, old: u64) -> SysResult {
    let t = crate::sched::current();
    let prev = t.sig.blocked();
    if set != 0 {
        let s: u64 = uaccess::read_user(set)?;
        let new = match how {
            0 => prev | s,
            1 => prev & !s,
            2 => s,
            _ => return Err(EINVAL),
        };
        t.sig.set_blocked(new);
    }
    if old != 0 {
        uaccess::write_user(old, &prev)?;
    }
    Ok(0)
}

/// rt_sigpending: blocked signals pending for the thread or its process.
pub fn sigpending(set: u64) -> SysResult {
    let p = cur()?;
    let t = crate::sched::current();
    let v = signal::pending_for(&t, &p) & t.sig.blocked();
    uaccess::write_user(set, &v)?;
    Ok(0)
}

/// Sleep until a signal is deliverable to the calling thread (or its
/// process exits).
fn wait_for_signal() {
    let t = crate::sched::current();
    let Some(p) = process::current() else {
        return;
    };
    let done = || signal::has_pending() || p.zombie.load(Ordering::SeqCst);
    let wq = crate::sched::WaitQueue::new();
    while !done() {
        // A wake-up for a process signal another thread took first.
        t.interrupted.store(false, Ordering::SeqCst);
        wq.wait_interruptible(done);
    }
}

/// rt_sigsuspend: wait with a temporary mask; the old mask comes back
/// when the handler returns (it is the handler frame's uc_sigmask).
pub fn sigsuspend(mask: u64) -> SysResult {
    let m: u64 = uaccess::read_user(mask)?;
    crate::sched::current().sig.set_temporary_mask(m);
    wait_for_signal();
    Err(EINTR)
}

pub fn pause() -> SysResult {
    wait_for_signal();
    Err(EINTR)
}

/// rt_sigtimedwait: dequeue a pending signal of `set` (the thread's own
/// first, then the process's), waiting up to `timeout` (none: forever).
pub fn sigtimedwait(set: u64, info: u64, timeout: u64) -> SysResult {
    let p = cur()?;
    let t = crate::sched::current();
    let never = signal::bit(signal::SIGKILL) | signal::bit(signal::SIGSTOP);
    let mask = uaccess::read_user::<u64>(set)? & !never;
    let deadline = if timeout != 0 {
        let ts: [i64; 2] = uaccess::read_user(timeout)?;
        if ts[0] < 0 || !(0..1_000_000_000).contains(&ts[1]) {
            return Err(EINVAL);
        }
        let ns = (ts[0] as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(ts[1] as u64);
        Some(crate::time::nanos().saturating_add(ns))
    } else {
        None
    };
    let mut got = None;
    crate::sched::wait::wait_any::<Errno>(
        &[&signal::SIGNAL_WQ],
        deadline,
        || {
            got = t
                .sig
                .pending
                .take(mask)
                .or_else(|| p.signals.shared.take(mask));
            Ok(got.is_some() as usize)
        },
        signal::has_pending,
    )?;
    let Some((sig, si)) = got else {
        return Err(if signal::has_pending() { EINTR } else { EAGAIN });
    };
    if info != 0 {
        uaccess::write_user(info, &si.to_user(sig))?;
    }
    Ok(sig as i64)
}

pub fn sigaltstack(sp: u64, ss: u64, old: u64) -> SysResult {
    signal::sigaltstack(sp, ss, old)?;
    Ok(0)
}

pub fn arch_prctl(code: u64, addr: u64) -> SysResult {
    const ARCH_SET_FS: u64 = 0x1002;
    const ARCH_GET_FS: u64 = 0x1003;
    match code {
        ARCH_SET_FS => {
            if addr >= crate::mm::USER_END {
                return Err(EPERM);
            }
            x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(addr));
            crate::sched::current()
                .fs_base
                .store(addr, Ordering::SeqCst);
            Ok(0)
        }
        ARCH_GET_FS => {
            let v = x86_64::registers::model_specific::FsBase::read().as_u64();
            uaccess::write_user(addr, &v)?;
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}

/// tkill/tgkill: signal thread `tid` (of thread group `tgid` for tgkill,
/// None for tkill).
pub fn tgkill(tgid: Option<i32>, tid: i32, sig: u32) -> SysResult {
    if sig as usize >= signal::NSIG || tid <= 0 || tgid.is_some_and(|g| g <= 0) {
        return Err(EINVAL);
    }
    let t = crate::sched::find_thread(tid as u64).ok_or(ESRCH)?;
    let p = t.process.lock().clone().ok_or(ESRCH)?;
    if tgid.is_some_and(|g| g as u32 != p.pid)
        || t.state() == crate::sched::State::Dead
        || p.zombie.load(Ordering::SeqCst)
    {
        return Err(ESRCH);
    }
    if sig != 0 {
        let me = cur()?;
        let info = signal::SigInfo::from_process(signal::SI_TKILL, &me);
        signal::send_thread(&t, &p, sig, info);
    }
    Ok(0)
}

pub fn set_tid_address(addr: u64) -> SysResult {
    crate::sched::current()
        .clear_child_tid
        .store(addr, Ordering::SeqCst);
    Ok(crate::sched::current_tid() as i64)
}

// ---------------------------------------------------------------------------
// futex (wait/wake only)
// ---------------------------------------------------------------------------

pub fn getrlimit(n: u64, a: [u64; 6]) -> SysResult {
    // RLIM_INFINITY for everything, except a realistic stack and fd limit.
    let (res, out) = if n == super::nr::PRLIMIT64 {
        (a[1], a[3])
    } else {
        (a[0], a[1])
    };
    if out != 0 {
        let lim: [u64; 2] = match res {
            3 => [
                crate::process::vm::STACK_SIZE,
                crate::process::vm::STACK_SIZE,
            ],
            7 => [
                crate::process::fd::MAX_FDS as u64,
                crate::process::fd::MAX_FDS as u64,
            ],
            _ => [u64::MAX, u64::MAX],
        };
        uaccess::write_user(out, &lim)?;
    }
    Ok(0)
}

/// `struct rusage`: user and system time as timevals, then 14 longs
/// (`ru_maxrss` in KiB first; the rest stay 0).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rusage {
    utime: [i64; 2],
    stime: [i64; 2],
    maxrss: i64,
    rest: [i64; 13],
}

impl Rusage {
    fn from_ticks(utime: u64, stime: u64, maxrss_kib: u64) -> Rusage {
        let tv = |ticks: u64| {
            let ns = crate::sched::cputime::to_ns(ticks);
            [
                (ns / 1_000_000_000) as i64,
                ((ns % 1_000_000_000) / 1000) as i64,
            ]
        };
        Rusage {
            utime: tv(utime),
            stime: tv(stime),
            maxrss: maxrss_kib as i64,
            rest: [0; 13],
        }
    }
}

const RUSAGE_SELF: i32 = 0;
const RUSAGE_CHILDREN: i32 = -1;
const RUSAGE_THREAD: i32 = 1;

pub fn getrusage(who: i32, buf: u64) -> SysResult {
    let p = cur()?;
    let rss_kib = || p.vm().map_or(0, |v| v.lock().resident_pages() * 4);
    let r = match who {
        RUSAGE_SELF => {
            let (u, s) = p.cpu_times();
            Rusage::from_ticks(u, s, rss_kib())
        }
        RUSAGE_CHILDREN => {
            let (u, s) = p.children_cpu_times();
            Rusage::from_ticks(u, s, 0)
        }
        RUSAGE_THREAD => {
            let (u, s) = crate::sched::current().cpu_times();
            Rusage::from_ticks(u, s, rss_kib())
        }
        _ => return Err(EINVAL),
    };
    uaccess::write_user(buf, &r)?;
    Ok(0)
}

pub fn times(buf: u64) -> SysResult {
    use crate::sched::cputime::to_clock_t;
    let ticks = crate::time::millis() / 10;
    if buf != 0 {
        let p = cur()?;
        let (u, s) = p.cpu_times();
        let (cu, cs) = p.children_cpu_times();
        let t: [u64; 4] = [to_clock_t(u), to_clock_t(s), to_clock_t(cu), to_clock_t(cs)];
        uaccess::write_user(buf, &t)?;
    }
    Ok(ticks as i64)
}

/// Threads selected by a (which, who) pair of get/setpriority:
/// PRIO_PROCESS (0) with 0 = the caller.
fn prio_threads(
    which: u32,
    who: u32,
) -> KResult<alloc::vec::Vec<alloc::sync::Arc<crate::sched::Thread>>> {
    if which != 0 {
        return Err(EINVAL);
    }
    let p = if who == 0 {
        cur()?
    } else {
        process::find(who).ok_or(ESRCH)?
    };
    Ok(p.live_threads())
}

pub fn getpriority(which: u32, who: u32) -> SysResult {
    let ts = prio_threads(which, who)?;
    let nice = ts
        .iter()
        .map(|t| t.nice.load(Ordering::Relaxed))
        .min()
        .unwrap_or(0);
    // The raw syscall returns 20 - nice (1..40).
    Ok(20 - nice as i64)
}

pub fn setpriority(which: u32, who: u32, nice: i32) -> SysResult {
    let nice = nice.clamp(-20, 19) as i8;
    for t in prio_threads(which, who)? {
        t.nice.store(nice, Ordering::Relaxed);
    }
    Ok(0)
}

fn affinity_thread(tid: u32) -> KResult<alloc::sync::Arc<crate::sched::Thread>> {
    if tid == 0 {
        return Ok(crate::sched::current());
    }
    crate::sched::find_thread(tid as u64).ok_or(ESRCH)
}

pub fn sched_setaffinity(tid: u32, len: u64, mask: u64) -> SysResult {
    let mut bytes = [0u8; 8];
    let n = (len as usize).min(8);
    uaccess::copy_from_user(&mut bytes[..n], mask)?;
    let m = u64::from_le_bytes(bytes);
    let cpus = crate::arch::x86_64::cpu::cpu_count().min(64);
    let valid = if cpus >= 64 {
        u64::MAX
    } else {
        (1u64 << cpus) - 1
    };
    if m & valid == 0 {
        return Err(EINVAL);
    }
    crate::sched::set_affinity(&affinity_thread(tid)?, m);
    Ok(0)
}

pub fn sched_getaffinity(tid: u32, len: u64, mask: u64) -> SysResult {
    if len < 8 || !len.is_multiple_of(8) {
        return Err(EINVAL);
    }
    let t = affinity_thread(tid)?;
    let cpus = crate::arch::x86_64::cpu::cpu_count().min(64);
    let valid = if cpus >= 64 {
        u64::MAX
    } else {
        (1u64 << cpus) - 1
    };
    uaccess::write_user(mask, &(t.affinity.load(Ordering::SeqCst) & valid))?;
    Ok(8)
}
