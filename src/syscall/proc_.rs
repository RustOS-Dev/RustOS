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
    Ok(process::fork(frame)? as i64)
}

const CLONE_VM: u64 = 0x100;
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
    // Everything else behaves like fork (vfork included).
    let mut f = *frame;
    if stack != 0 {
        f.rsp = stack;
    }
    Ok(process::fork(&f)? as i64)
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

pub fn wait4(pid: i32, status: u64, options: u32) -> SysResult {
    let (cpid, st) = process::wait(pid, options)?;
    if status != 0 && cpid != 0 {
        uaccess::write_user(status, &st)?;
    }
    Ok(cpid as i64)
}

pub fn kill(pid: i32, sig: u32) -> SysResult {
    if sig as usize >= signal::NSIG {
        return Err(EINVAL);
    }
    let me = cur()?;
    match pid {
        p if p > 0 => {
            let target = process::find(p as u32).ok_or(ESRCH)?;
            if target.zombie.load(Ordering::SeqCst) {
                return Err(ESRCH);
            }
            if sig != 0 {
                signal::send(&target, sig);
            }
        }
        0 => {
            if signal::send_group(me.pgid.load(Ordering::SeqCst), sig) == 0 {
                return Err(ESRCH);
            }
        }
        -1 => {
            for p in process::all() {
                if p.pid > 1 && p.pid != me.pid {
                    signal::send(&p, sig);
                }
            }
        }
        p => {
            if signal::send_group((-p) as u32, sig) == 0 {
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
        if a.handler == signal::SIG_IGN {
            p.signals
                .pending
                .fetch_and(!(1u64 << (sig - 1)), Ordering::SeqCst);
        }
    }
    if old != 0 {
        uaccess::write_user(old, &prev)?;
    }
    Ok(0)
}

pub fn sigprocmask(how: u32, set: u64, old: u64) -> SysResult {
    cur()?;
    let t = crate::sched::current();
    let prev = t.sigmask.load(Ordering::SeqCst);
    if set != 0 {
        let s: u64 = uaccess::read_user(set)?;
        let new = match how {
            0 => prev | s,
            1 => prev & !s,
            2 => s,
            _ => return Err(EINVAL),
        };
        let never = (1u64 << (signal::SIGKILL - 1)) | (1u64 << (signal::SIGSTOP - 1));
        t.sigmask.store(new & !never, Ordering::SeqCst);
    }
    if old != 0 {
        uaccess::write_user(old, &prev)?;
    }
    Ok(0)
}

pub fn sigpending(set: u64) -> SysResult {
    let p = cur()?;
    let v = p.signals.pending.load(Ordering::SeqCst)
        & crate::sched::current().sigmask.load(Ordering::SeqCst);
    uaccess::write_user(set, &v)?;
    Ok(0)
}

pub fn sigsuspend(mask: u64) -> SysResult {
    cur()?;
    let m: u64 = uaccess::read_user(mask)?;
    // The caller's mask comes back after the handler runs.
    signal::set_temp_mask(m);
    let wq = crate::sched::WaitQueue::new();
    wq.wait_interruptible(signal::has_pending);
    Err(EINTR)
}

pub fn pause() -> SysResult {
    let wq = crate::sched::WaitQueue::new();
    wq.wait_interruptible(signal::has_pending);
    Err(EINTR)
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

/// tkill/tgkill: signal the process of thread `tid` (signals are
/// process-wide here).
pub fn tkill(tid: u64, sig: u32) -> SysResult {
    if sig as usize >= signal::NSIG {
        return Err(EINVAL);
    }
    let t = crate::sched::find_thread(tid).ok_or(ESRCH)?;
    let p = t.process.lock().clone().ok_or(ESRCH)?;
    if sig != 0 {
        signal::send(&p, sig);
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

pub fn getrusage(buf: u64) -> SysResult {
    uaccess::copy_to_user(buf, &[0u8; 144])?;
    Ok(0)
}

pub fn times(buf: u64) -> SysResult {
    let ticks = crate::time::millis() / 10;
    if buf != 0 {
        let t: [u64; 4] = [ticks, 0, 0, 0];
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
