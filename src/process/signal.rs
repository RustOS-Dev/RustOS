//! POSIX signals: actions, masks, delivery on return to user mode, and
//! job-control stop/continue.

use super::{Process, current, uaccess};
use crate::arch::x86_64::{gdt, idt::TrapFrame};
use crate::errno::*;
use crate::sched::{self, Thread, WaitQueue};
use crate::sync::Mutex;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGILL: u32 = 4;
pub const SIGTRAP: u32 = 5;
pub const SIGABRT: u32 = 6;
pub const SIGBUS: u32 = 7;
pub const SIGFPE: u32 = 8;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;
pub const SIGCHLD: u32 = 17;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;
pub const SIGTTIN: u32 = 21;
pub const SIGTTOU: u32 = 22;
pub const SIGURG: u32 = 23;
pub const SIGWINCH: u32 = 28;
pub const NSIG: usize = 65;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

pub const SA_NOCLDSTOP: u64 = 1;
pub const SA_SIGINFO: u64 = 4;
pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;

/// Kernel `sigaction` layout (as used by the raw rt_sigaction syscall).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

pub struct SignalState {
    pub actions: Mutex<[SigAction; NSIG]>,
    pub pending: AtomicU64,
    /// Stopped threads wait here for SIGCONT.
    pub cont_wq: WaitQueue,
}

impl SignalState {
    pub fn new() -> SignalState {
        SignalState {
            actions: Mutex::new([SigAction::default(); NSIG]),
            pending: AtomicU64::new(0),
            cont_wq: WaitQueue::new(),
        }
    }

    pub fn inherit_from(&self, other: &SignalState) {
        *self.actions.lock() = *other.actions.lock();
    }

    /// Handlers revert to default on exec; ignored signals stay ignored.
    pub fn reset_on_exec(&self) {
        for a in self.actions.lock().iter_mut() {
            if a.handler != SIG_IGN {
                *a = SigAction::default();
            }
        }
    }
}

impl Default for SignalState {
    fn default() -> Self {
        Self::new()
    }
}

fn bit(sig: u32) -> u64 {
    1u64 << (sig - 1)
}

pub fn signal_name(sig: u32) -> &'static str {
    match sig {
        SIGHUP => "SIGHUP",
        SIGINT => "SIGINT",
        SIGQUIT => "SIGQUIT",
        SIGILL => "SIGILL",
        SIGTRAP => "SIGTRAP",
        SIGABRT => "SIGABRT",
        SIGBUS => "SIGBUS",
        SIGFPE => "SIGFPE",
        SIGKILL => "SIGKILL",
        SIGUSR1 => "SIGUSR1",
        SIGSEGV => "SIGSEGV",
        SIGUSR2 => "SIGUSR2",
        SIGPIPE => "SIGPIPE",
        SIGALRM => "SIGALRM",
        SIGTERM => "SIGTERM",
        SIGCHLD => "SIGCHLD",
        SIGCONT => "SIGCONT",
        SIGSTOP => "SIGSTOP",
        SIGTSTP => "SIGTSTP",
        SIGTTIN => "SIGTTIN",
        SIGTTOU => "SIGTTOU",
        SIGWINCH => "SIGWINCH",
        _ => "signal",
    }
}

enum Default_ {
    Terminate,
    Core,
    Ignore,
    Stop,
    Continue,
}

fn default_action(sig: u32) -> Default_ {
    match sig {
        SIGCHLD | SIGURG | SIGWINCH => Default_::Ignore,
        SIGCONT => Default_::Continue,
        SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => Default_::Stop,
        SIGQUIT | SIGILL | SIGTRAP | SIGABRT | SIGBUS | SIGFPE | SIGSEGV => Default_::Core,
        _ => Default_::Terminate,
    }
}

/// Interrupt a thread blocked in the kernel so it notices a signal.
pub fn kill_thread(t: &Arc<Thread>) {
    t.interrupted.store(true, Ordering::SeqCst);
    sched::wake(t);
}

/// Post `sig` to process `p`.
pub fn send(p: &Arc<Process>, sig: u32) {
    if sig == 0 || sig as usize >= NSIG || p.zombie.load(Ordering::SeqCst) {
        return;
    }
    let action = p.signals.actions.lock()[sig as usize];
    if sig == SIGCONT || sig == SIGKILL {
        // Resume a stopped process immediately.
        if p.stopped.swap(false, Ordering::SeqCst) {
            p.continued.store(true, Ordering::SeqCst);
            if let Some(parent) = p.parent() {
                parent.child_wq.wake_all();
            }
        }
        // Discard pending stop signals.
        p.signals.pending.fetch_and(
            !(bit(SIGSTOP) | bit(SIGTSTP) | bit(SIGTTIN) | bit(SIGTTOU)),
            Ordering::SeqCst,
        );
        p.signals.cont_wq.wake_all();
    }
    if matches!(sig, SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU) {
        p.signals.pending.fetch_and(!bit(SIGCONT), Ordering::SeqCst);
    }
    // Ignored signals are dropped at send time (except KILL/STOP).
    let ignored = sig != SIGKILL
        && sig != SIGSTOP
        && (action.handler == SIG_IGN
            || (action.handler == SIG_DFL && matches!(default_action(sig), Default_::Ignore)));
    if ignored {
        return;
    }
    if sig == SIGCONT && action.handler == SIG_DFL {
        return;
    }
    p.signals.pending.fetch_or(bit(sig), Ordering::SeqCst);
    let threads = p.live_threads();
    if sig == SIGKILL {
        for t in &threads {
            kill_thread(t);
        }
    } else {
        // Like Linux, wake one thread that does not block the signal,
        // preferring the sender if it is one of them; the others' waits
        // must not see a spurious EINTR.
        let wants = |t: &Arc<Thread>| t.sigmask.load(Ordering::SeqCst) & bit(sig) == 0;
        let me = sched::current();
        if let Some(t) = threads
            .iter()
            .find(|t| t.tid == me.tid && wants(t))
            .or_else(|| threads.iter().find(|t| wants(t)))
        {
            kill_thread(t);
        }
    }
    // signalfd readers.
    SIGNAL_WQ.wake_all();
}

/// Woken whenever a signal becomes pending (signalfd readiness).
pub static SIGNAL_WQ: WaitQueue = WaitQueue::new();

/// Send to every process in a process group.
pub fn send_group(pgid: u32, sig: u32) -> usize {
    let members = super::group_members(pgid);
    for p in &members {
        send(p, sig);
    }
    members.len()
}

pub fn send_to_current(sig: u32) {
    if let Some(p) = current() {
        send(&p, sig);
    }
}

/// Deliver a synchronous fault signal even if blocked or ignored.
pub fn force_signal(p: &Arc<Process>, sig: u32) {
    {
        let mut actions = p.signals.actions.lock();
        // The faulting thread is the current one.
        let blocked = sched::current().sigmask.load(Ordering::SeqCst) & bit(sig) != 0;
        if actions[sig as usize].handler == SIG_IGN || blocked {
            actions[sig as usize] = SigAction::default();
        }
    }
    sched::current()
        .sigmask
        .fetch_and(!bit(sig), Ordering::SeqCst);
    p.signals.pending.fetch_or(bit(sig), Ordering::SeqCst);
}

/// Whether the current thread has a deliverable signal.
pub fn has_pending() -> bool {
    match current() {
        Some(p) => {
            let pend = p.signals.pending.load(Ordering::SeqCst);
            pend & !sched::current().sigmask.load(Ordering::SeqCst) != 0 || pend & bit(SIGKILL) != 0
        }
        None => false,
    }
}

/// Wait with `mask` as the signal mask for the rest of this syscall (Linux's
/// set_restore_sigmask): the caller's mask comes back on return to user
/// mode, or after the handler of a signal delivered on that return.
pub fn set_temp_mask(mask: u64) {
    let t = sched::current();
    let old = t
        .sigmask
        .swap(mask & !(bit(SIGKILL) | bit(SIGSTOP)), Ordering::SeqCst);
    if !t.restore_sigmask.swap(true, Ordering::SeqCst) {
        t.saved_sigmask.store(old, Ordering::SeqCst);
    }
}

/// The mask a syscall should restore, clearing the pending restore.
fn take_saved_mask() -> Option<u64> {
    let t = sched::current();
    if t.restore_sigmask.swap(false, Ordering::SeqCst) {
        Some(t.saved_sigmask.load(Ordering::SeqCst))
    } else {
        None
    }
}

/// Saved state pushed on the user stack for a handler.
#[repr(C)]
#[derive(Clone, Copy)]
struct SigFrame {
    restorer: u64,
    sig: u64,
    old_mask: u64,
    frame: TrapFrame,
    fpu: [u8; 512],
}

/// Called on every return to user mode.
pub fn deliver_pending(frame: &mut TrapFrame) {
    // Decide inside a scope so no Arc is alive if the process must exit.
    let exit_status = deliver_inner(frame);
    if let Some(status) = exit_status {
        super::exit_current(status);
    }
}

/// Resolve a pending ERESTARTSYS: re-execute the syscall (`restart`) or
/// report EINTR.
fn finish_restart(frame: &mut TrapFrame, restart: bool) {
    let code = (-crate::syscall::ERESTARTSYS) as u64;
    if frame.rax == code {
        if restart {
            frame.rax = frame.error_code;
            frame.rip -= 2; // both `syscall` and `int 0x80` are two bytes
        } else {
            frame.rax = (-(crate::errno::EINTR.0 as i64)) as u64;
            sched::current().restart_deadline.store(0, Ordering::SeqCst);
        }
    }
}

fn deliver_inner(frame: &mut TrapFrame) -> Option<i32> {
    let r = deliver_signals(frame);
    // No handler ran: restart any interrupted syscall.
    if r.is_none() {
        finish_restart(frame, true);
    }
    // No handler took the saved mask: restore it now.
    if let Some(m) = take_saved_mask() {
        sched::current().sigmask.store(m, Ordering::SeqCst);
    }
    r
}

fn deliver_signals(frame: &mut TrapFrame) -> Option<i32> {
    let p = current()?;
    if p.zombie.load(Ordering::SeqCst) {
        return Some(p.exit_status.lock().unwrap_or(0));
    }
    sched::current().interrupted.store(false, Ordering::SeqCst);
    loop {
        let pending = p.signals.pending.load(Ordering::SeqCst);
        let blocked = sched::current().sigmask.load(Ordering::SeqCst);
        let deliverable = pending & !(blocked & !(bit(SIGKILL) | bit(SIGSTOP)));
        if deliverable == 0 {
            return None;
        }
        let sig = deliverable.trailing_zeros() + 1;
        p.signals.pending.fetch_and(!bit(sig), Ordering::SeqCst);
        let action = p.signals.actions.lock()[sig as usize];
        if sig == SIGKILL || sig == SIGSTOP || action.handler == SIG_DFL {
            match default_action(sig) {
                Default_::Ignore | Default_::Continue => continue,
                Default_::Stop => {
                    stop_current(&p, sig);
                    continue;
                }
                Default_::Terminate => return Some(sig as i32),
                Default_::Core => return Some(sig as i32 | 0x80),
            }
        }
        if action.handler == SIG_IGN {
            continue;
        }
        finish_restart(frame, action.flags & SA_RESTART != 0);
        if setup_frame(frame, sig, &action).is_err() {
            return Some(SIGSEGV as i32);
        }
        let mut actions = p.signals.actions.lock();
        if action.flags & SA_RESETHAND != 0 {
            actions[sig as usize] = SigAction::default();
        }
        return None;
    }
}

fn stop_current(p: &Arc<Process>, sig: u32) {
    p.stop_sig.store(sig as i32, Ordering::SeqCst);
    p.stop_reported.store(false, Ordering::SeqCst);
    p.stopped.store(true, Ordering::SeqCst);
    if let Some(parent) = p.parent() {
        let nocldstop = parent.signals.actions.lock()[SIGCHLD as usize].flags & SA_NOCLDSTOP != 0;
        if !nocldstop {
            send(&parent, SIGCHLD);
        }
        parent.child_wq.wake_all();
    }
    crate::tty::process_stopped(p);
    let pp = p.clone();
    p.signals.cont_wq.wait_until(|| {
        !pp.stopped.load(Ordering::SeqCst)
            || pp.signals.pending.load(Ordering::SeqCst) & bit(SIGKILL) != 0
    });
}

fn setup_frame(frame: &mut TrapFrame, sig: u32, action: &SigAction) -> KResult<()> {
    let cur_mask = sched::current().sigmask.load(Ordering::SeqCst);
    // After a temporary-mask wait the handler returns to the caller's mask.
    let old_mask = take_saved_mask().unwrap_or(cur_mask);
    let mut sf = SigFrame {
        restorer: action.restorer,
        sig: sig as u64,
        old_mask,
        frame: *frame,
        fpu: [0; 512],
    };
    let t = sched::current();
    unsafe {
        core::arch::asm!("fxsave64 [{}]", in(reg) t.fpu.get(), options(nostack));
        sf.fpu.copy_from_slice(&(*t.fpu.get()).0);
    }
    // Skip the red zone, align so that after the "return address" slot the
    // handler sees a 16-byte aligned stack minus 8, as after a call.
    let size = core::mem::size_of::<SigFrame>() as u64;
    let sp = ((frame.rsp - 128 - size) & !0xF) - 8;
    uaccess::write_user(sp, &sf)?;
    let mut mask = cur_mask | action.mask;
    if action.flags & SA_NODEFER == 0 {
        mask |= bit(sig);
    }
    sched::current()
        .sigmask
        .store(mask & !(bit(SIGKILL) | bit(SIGSTOP)), Ordering::SeqCst);
    frame.rsp = sp;
    frame.rip = action.handler;
    frame.rdi = sig as u64;
    frame.rsi = 0;
    frame.rdx = sp + 24; // pointer to the saved TrapFrame (ucontext-ish)
    frame.rax = 0;
    Ok(())
}

/// rt_sigreturn: restore the context saved by `setup_frame`.
pub fn sigreturn(frame: &mut TrapFrame) -> KResult<()> {
    current().ok_or(ESRCH)?;
    // The handler's `ret` popped the restorer address.
    let sp = frame.rsp - 8;
    let sf: SigFrame = uaccess::read_user(sp)?;
    let mut restored = sf.frame;
    // Never let user code forge kernel segments or privileged flags.
    restored.cs = gdt::USER_CS as u64;
    restored.ss = gdt::USER_DS as u64;
    restored.rflags = (restored.rflags & 0xCD5) | 0x202;
    if restored.rip >= crate::mm::USER_END {
        return Err(EFAULT);
    }
    *frame = restored;
    sched::current().sigmask.store(
        sf.old_mask & !(bit(SIGKILL) | bit(SIGSTOP)),
        Ordering::SeqCst,
    );
    let t = sched::current();
    unsafe {
        (*t.fpu.get()).0.copy_from_slice(&sf.fpu);
        // Clear reserved MXCSR bits user code may have scribbled.
        let fpu = &mut (*t.fpu.get()).0;
        let mxcsr = u32::from_le_bytes([fpu[24], fpu[25], fpu[26], fpu[27]]) & 0xFFFF;
        fpu[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        core::arch::asm!("fxrstor64 [{}]", in(reg) t.fpu.get(), options(nostack));
    }
    Ok(())
}
