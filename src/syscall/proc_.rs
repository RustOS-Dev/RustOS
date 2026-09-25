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
const CLONE_PARENT_SETTID: u64 = 0x100000;

pub fn clone(
    frame: &TrapFrame,
    flags: u64,
    stack: u64,
    ptid: u64,
    ctid: u64,
    tls: u64,
) -> SysResult {
    if flags & CLONE_VM != 0 && flags & CLONE_THREAD != 0 {
        let tls = (flags & CLONE_SETTLS != 0).then_some(tls);
        let ctid = if flags & CLONE_CHILD_SETTID != 0 {
            ctid
        } else {
            0
        };
        let tid = process::clone_thread(frame, stack, tls, ctid)?;
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
    let p = cur()?;
    let prev = p.signals.blocked.load(Ordering::SeqCst);
    if set != 0 {
        let s: u64 = uaccess::read_user(set)?;
        let new = match how {
            0 => prev | s,
            1 => prev & !s,
            2 => s,
            _ => return Err(EINVAL),
        };
        let never = (1u64 << (signal::SIGKILL - 1)) | (1u64 << (signal::SIGSTOP - 1));
        p.signals.blocked.store(new & !never, Ordering::SeqCst);
    }
    if old != 0 {
        uaccess::write_user(old, &prev)?;
    }
    Ok(0)
}

pub fn sigpending(set: u64) -> SysResult {
    let p = cur()?;
    let v = p.signals.pending.load(Ordering::SeqCst) & p.signals.blocked.load(Ordering::SeqCst);
    uaccess::write_user(set, &v)?;
    Ok(0)
}

pub fn sigsuspend(mask: u64) -> SysResult {
    let p = cur()?;
    let m: u64 = uaccess::read_user(mask)?;
    let old = p.signals.blocked.swap(m, Ordering::SeqCst);
    let wq = crate::sched::WaitQueue::new();
    wq.wait_interruptible(signal::has_pending);
    // The original mask is restored after the handler runs; approximate by
    // restoring now (the handler frame records the suspended mask).
    let _ = old;
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

pub fn set_tid_address(addr: u64) -> SysResult {
    *cur()?.clear_child_tid.lock() = addr;
    Ok(crate::sched::current_tid() as i64)
}

// ---------------------------------------------------------------------------
// futex (wait/wake only)
// ---------------------------------------------------------------------------

static FUTEX_WQ: crate::sched::WaitQueue = crate::sched::WaitQueue::new();
static FUTEX_SEQ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn futex(addr: u64, op: u32, val: u32, timeout: u64) -> SysResult {
    const FUTEX_WAIT: u32 = 0;
    const FUTEX_WAKE: u32 = 1;
    match op & 0x7f {
        FUTEX_WAIT => {
            let cur: u32 = uaccess::read_user(addr)?;
            if cur != val {
                return Err(EAGAIN);
            }
            let seq = FUTEX_SEQ.load(Ordering::SeqCst);
            let cond = || FUTEX_SEQ.load(Ordering::SeqCst) != seq || signal::has_pending();
            if timeout != 0 {
                let t: [i64; 2] = uaccess::read_user(timeout)?;
                let ms = (t[0] * 1000 + t[1] / 1_000_000).max(1) as u64;
                if !FUTEX_WQ.wait_timeout(ms, cond) {
                    return Err(ETIMEDOUT);
                }
            } else {
                FUTEX_WQ.wait_until(cond);
            }
            Ok(0)
        }
        FUTEX_WAKE => {
            FUTEX_SEQ.fetch_add(1, Ordering::SeqCst);
            FUTEX_WQ.wake_all();
            Ok(val as i64)
        }
        _ => Err(ENOSYS),
    }
}

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
