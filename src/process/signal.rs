//! POSIX signals: actions, per-thread masks, pending sets with `siginfo`,
//! Linux x86-64 signal frames (`rt_sigframe`), alternate signal stacks and
//! job-control stop/continue.
//!
//! As on Linux, the disposition table is process-wide while each thread has
//! its own blocked mask, its own pending set (signals sent with
//! `tkill`/`tgkill` and synchronous faults) and its own alternate stack.
//! Process-directed signals (`kill`, terminal signals, timers, SIGCHLD) wait
//! in the process's shared pending set and are taken by whichever thread
//! that does not block them returns to user mode first; one such thread is
//! woken when the signal is sent.

use super::{Process, current, uaccess};
use crate::arch::x86_64::{gdt, idt::TrapFrame};
use crate::errno::*;
use crate::sched::{self, Thread, WaitQueue};
use crate::sync::Mutex;
use alloc::sync::Arc;
use core::mem::{offset_of, size_of};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;

// si_code values (asm-generic/siginfo.h).
pub const SI_USER: i32 = 0;
pub const SI_KERNEL: i32 = 0x80;
pub const SI_TKILL: i32 = -6;
pub const ILL_ILLOPN: i32 = 2;
pub const FPE_INTDIV: i32 = 1;
pub const FPE_FLTDIV: i32 = 3;
pub const FPE_FLTOVF: i32 = 4;
pub const FPE_FLTUND: i32 = 5;
pub const FPE_FLTRES: i32 = 6;
pub const FPE_FLTINV: i32 = 7;
pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;
pub const BUS_ADRALN: i32 = 1;
pub const TRAP_TRACE: i32 = 2;
pub const CLD_EXITED: i32 = 1;
pub const CLD_KILLED: i32 = 2;
pub const CLD_DUMPED: i32 = 3;
pub const CLD_STOPPED: i32 = 5;

// sigaltstack flags.
pub const SS_ONSTACK: i32 = 1;
pub const SS_DISABLE: i32 = 2;
pub const SS_AUTODISARM: i32 = i32::MIN; // 1 << 31
pub const MINSIGSTKSZ: u64 = 2048;

// ucontext uc_flags (arch/x86/include/uapi/asm/ucontext.h).
const UC_SIGCONTEXT_SS: u64 = 2;
const UC_STRICT_RESTORE_SS: u64 = 4;

/// Signals that can be neither caught, blocked nor ignored.
const UNBLOCKABLE: u64 = (1 << (SIGKILL - 1)) | (1 << (SIGSTOP - 1));
/// Synchronous (fault) signals are dequeued before the others.
const SYNCHRONOUS: u64 = (1 << (SIGSEGV - 1))
    | (1 << (SIGBUS - 1))
    | (1 << (SIGILL - 1))
    | (1 << (SIGTRAP - 1))
    | (1 << (SIGFPE - 1));
const STOP_SIGNALS: u64 =
    (1 << (SIGSTOP - 1)) | (1 << (SIGTSTP - 1)) | (1 << (SIGTTIN - 1)) | (1 << (SIGTTOU - 1));

/// Kernel `sigaction` layout (as used by the raw rt_sigaction syscall).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

// ---------------------------------------------------------------------------
// siginfo
// ---------------------------------------------------------------------------

/// What is remembered about a pending signal, enough to fill `siginfo_t`
/// (standard signals do not queue: one record per signal number).
#[derive(Clone, Copy, Default, Debug)]
pub struct SigInfo {
    pub code: i32,
    /// SIGCHLD: exit code or signal number.
    pub status: i32,
    pub pid: u32,
    pub uid: u32,
    /// Faults: the faulting address (`si_addr`, and `cr2` for page faults).
    pub addr: u64,
    /// SIGCHLD: CPU time of the child in clock ticks.
    pub utime: u64,
    pub stime: u64,
}

impl SigInfo {
    const EMPTY: SigInfo = SigInfo {
        code: 0,
        status: 0,
        pid: 0,
        uid: 0,
        addr: 0,
        utime: 0,
        stime: 0,
    };

    /// Sent by the kernel (terminal, timers, job control).
    pub fn kernel() -> SigInfo {
        SigInfo {
            code: SI_KERNEL,
            ..SigInfo::EMPTY
        }
    }

    /// Sent by process `p` (`kill`: SI_USER, `tkill`/`tgkill`: SI_TKILL).
    pub fn from_process(code: i32, p: &Process) -> SigInfo {
        SigInfo {
            code,
            pid: p.pid,
            uid: p.uid.load(Ordering::SeqCst),
            ..SigInfo::EMPTY
        }
    }

    /// A hardware fault at `addr`.
    pub fn fault(code: i32, addr: u64) -> SigInfo {
        SigInfo {
            code,
            addr,
            ..SigInfo::EMPTY
        }
    }

    /// SIGCHLD for a state change of `child`.
    pub fn child(code: i32, status: i32, child: &Process) -> SigInfo {
        let (u, s) = child.cpu_times();
        let (cu, cs) = child.children_cpu_times();
        SigInfo {
            code,
            status,
            pid: child.pid,
            uid: child.uid.load(Ordering::SeqCst),
            addr: 0,
            utime: crate::sched::cputime::to_clock_t(u + cu),
            stime: crate::sched::cputime::to_clock_t(s + cs),
        }
    }

    /// The user-visible `siginfo_t` for signal `sig`.
    pub fn to_user(&self, sig: u32) -> UserSigInfo {
        let mut u = UserSigInfo {
            signo: sig as i32,
            errno: 0,
            code: self.code,
            _pad: 0,
            fields: [0; 14],
        };
        // The union member follows from the signal and its code, as in
        // the kernel's siginfo_layout().
        let kernel_code = self.code > 0 && self.code != SI_KERNEL;
        if kernel_code && matches!(sig, SIGSEGV | SIGBUS | SIGILL | SIGFPE | SIGTRAP) {
            u.fields[0] = self.addr; // si_addr
        } else if kernel_code && sig == SIGCHLD {
            u.fields[0] = self.pid as u64 | (self.uid as u64) << 32;
            u.fields[1] = self.status as u32 as u64; // si_status
            u.fields[2] = self.utime;
            u.fields[3] = self.stime;
        } else {
            u.fields[0] = self.pid as u64 | (self.uid as u64) << 32;
        }
        u
    }
}

/// `siginfo_t` as user space sees it (128 bytes; the union starts at 16).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UserSigInfo {
    pub signo: i32,
    pub errno: i32,
    pub code: i32,
    _pad: i32,
    pub fields: [u64; 14],
}

// ---------------------------------------------------------------------------
// Linux x86-64 signal frame (arch/x86/include/uapi/asm/sigcontext.h,
// asm/ucontext.h, arch/x86/include/asm/sigframe.h)
// ---------------------------------------------------------------------------

/// `stack_t`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct StackT {
    pub ss_sp: u64,
    pub ss_flags: i32,
    _pad: i32,
    pub ss_size: u64,
}

/// `struct sigcontext` (64-bit), the kernel's view of `mcontext_t`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct SigContext {
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rsp: u64,
    pub rip: u64,
    pub eflags: u64,
    pub cs: u16,
    pub gs: u16,
    pub fs: u16,
    pub ss: u16,
    pub err: u64,
    pub trapno: u64,
    pub oldmask: u64,
    pub cr2: u64,
    /// User pointer to the 512-byte FXSAVE area (`struct _fpstate_64`).
    pub fpstate: u64,
    pub reserved1: [u64; 8],
}

/// The kernel's `struct ucontext` (the 8-byte kernel `sigset_t` at the end).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct UContext {
    pub uc_flags: u64,
    pub uc_link: u64,
    pub uc_stack: StackT,
    pub uc_mcontext: SigContext,
    pub uc_sigmask: u64,
}

/// `struct rt_sigframe`: the handler's return address (`sa_restorer`) at
/// its stack pointer, then the ucontext and the siginfo. The FPU state
/// lies above it, 64-byte aligned.
#[repr(C)]
#[derive(Clone, Copy)]
struct RtSigFrame {
    pretcode: u64,
    uc: UContext,
    info: UserSigInfo,
}

const FPSTATE_SIZE: u64 = 512;

const _: () = {
    assert!(size_of::<StackT>() == 24);
    assert!(offset_of!(StackT, ss_flags) == 8);
    assert!(offset_of!(StackT, ss_size) == 16);
    assert!(size_of::<SigContext>() == 256);
    assert!(offset_of!(SigContext, rdi) == 64);
    assert!(offset_of!(SigContext, rax) == 104);
    assert!(offset_of!(SigContext, rsp) == 120);
    assert!(offset_of!(SigContext, rip) == 128);
    assert!(offset_of!(SigContext, eflags) == 136);
    assert!(offset_of!(SigContext, cs) == 144);
    assert!(offset_of!(SigContext, ss) == 150);
    assert!(offset_of!(SigContext, err) == 152);
    assert!(offset_of!(SigContext, trapno) == 160);
    assert!(offset_of!(SigContext, oldmask) == 168);
    assert!(offset_of!(SigContext, cr2) == 176);
    assert!(offset_of!(SigContext, fpstate) == 184);
    assert!(size_of::<UContext>() == 304);
    assert!(offset_of!(UContext, uc_stack) == 16);
    assert!(offset_of!(UContext, uc_mcontext) == 40);
    assert!(offset_of!(UContext, uc_sigmask) == 296);
    assert!(size_of::<UserSigInfo>() == 128);
    assert!(offset_of!(RtSigFrame, uc) == 8);
    assert!(offset_of!(RtSigFrame, info) == 312);
    assert!(size_of::<RtSigFrame>() == 440);
};

// ---------------------------------------------------------------------------
// Pending sets and per-thread state
// ---------------------------------------------------------------------------

/// A pending set: a bitmap plus the siginfo of each pending signal.
pub struct SigQueue {
    bits: AtomicU64,
    info: Mutex<[SigInfo; NSIG - 1]>,
}

impl SigQueue {
    pub const fn new() -> SigQueue {
        SigQueue {
            bits: AtomicU64::new(0),
            info: Mutex::new([SigInfo::EMPTY; NSIG - 1]),
        }
    }

    pub fn pending(&self) -> u64 {
        self.bits.load(Ordering::SeqCst)
    }

    /// Mark `sig` pending; a signal already pending keeps its first info.
    pub fn post(&self, sig: u32, info: SigInfo) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut infos = self.info.lock();
            if self.bits.load(Ordering::SeqCst) & bit(sig) == 0 {
                infos[sig as usize - 1] = info;
                self.bits.fetch_or(bit(sig), Ordering::SeqCst);
            }
        })
    }

    /// Dequeue the lowest pending signal in `allowed` (faults first).
    pub fn take(&self, allowed: u64) -> Option<(u32, SigInfo)> {
        if self.pending() & allowed == 0 {
            return None;
        }
        x86_64::instructions::interrupts::without_interrupts(|| {
            let infos = self.info.lock();
            let avail = self.bits.load(Ordering::SeqCst) & allowed;
            if avail == 0 {
                return None;
            }
            let pick = if avail & SYNCHRONOUS != 0 {
                avail & SYNCHRONOUS
            } else {
                avail
            };
            let sig = pick.trailing_zeros() + 1;
            self.bits.fetch_and(!bit(sig), Ordering::SeqCst);
            Some((sig, infos[sig as usize - 1]))
        })
    }

    pub fn discard(&self, mask: u64) {
        self.bits.fetch_and(!mask, Ordering::SeqCst);
    }
}

impl Default for SigQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// A thread's alternate signal stack (`sigaltstack`).
#[derive(Clone, Copy, Debug)]
pub struct AltStack {
    pub sp: u64,
    pub size: u64,
    pub flags: i32,
}

impl AltStack {
    pub const DISABLED: AltStack = AltStack {
        sp: 0,
        size: 0,
        flags: SS_DISABLE,
    };

    /// Whether `sp` lies on the stack (it grows down from `sp + size`).
    fn contains(&self, sp: u64) -> bool {
        sp > self.sp && sp - self.sp <= self.size
    }

    /// Linux's `on_sig_stack`: an SS_AUTODISARM stack never counts as in
    /// use.
    fn on_stack(&self, sp: u64) -> bool {
        self.flags & SS_AUTODISARM == 0 && self.contains(sp)
    }

    /// Linux's `sas_ss_flags`.
    fn ss_flags(&self, sp: u64) -> i32 {
        if self.size == 0 {
            SS_DISABLE
        } else if self.on_stack(sp) {
            SS_ONSTACK
        } else {
            0
        }
    }
}

/// Signal state of one thread.
pub struct ThreadSignals {
    /// Signals directed at this thread (tkill/tgkill, faults, SIGPIPE).
    pub pending: SigQueue,
    blocked: AtomicU64,
    /// Mask to restore after a `sigsuspend` (Linux's saved_sigmask): it
    /// becomes the handler frame's `uc_sigmask`, or is restored directly
    /// when no handler runs.
    saved_mask: AtomicU64,
    has_saved: AtomicBool,
    altstack: Mutex<AltStack>,
}

impl ThreadSignals {
    pub const fn new() -> ThreadSignals {
        ThreadSignals {
            pending: SigQueue::new(),
            blocked: AtomicU64::new(0),
            saved_mask: AtomicU64::new(0),
            has_saved: AtomicBool::new(false),
            altstack: Mutex::new(AltStack::DISABLED),
        }
    }

    pub fn blocked(&self) -> u64 {
        self.blocked.load(Ordering::SeqCst)
    }

    pub fn set_blocked(&self, mask: u64) {
        self.blocked.store(mask & !UNBLOCKABLE, Ordering::SeqCst);
    }

    /// Install a temporary mask (`sigsuspend`); the current one is restored
    /// on the way back to user mode.
    pub fn set_temporary_mask(&self, mask: u64) {
        let old = self.blocked.swap(mask & !UNBLOCKABLE, Ordering::SeqCst);
        self.saved_mask.store(old, Ordering::SeqCst);
        self.has_saved.store(true, Ordering::SeqCst);
    }

    fn take_saved(&self) -> Option<u64> {
        self.has_saved
            .swap(false, Ordering::SeqCst)
            .then(|| self.saved_mask.load(Ordering::SeqCst))
    }

    fn put_saved(&self, saved: Option<u64>) {
        if let Some(m) = saved {
            self.saved_mask.store(m, Ordering::SeqCst);
            self.has_saved.store(true, Ordering::SeqCst);
        }
    }

    /// State of a new thread created by `parent`: fork and clone inherit
    /// the mask; with `keep_altstack` (fork, vfork) the alternate stack too,
    /// threads start without one.
    pub fn inherit(&self, parent: &ThreadSignals, keep_altstack: bool) {
        self.set_blocked(parent.blocked());
        if keep_altstack {
            *self.altstack.lock() = *parent.altstack.lock();
        }
    }

    /// execve: the alternate stack is gone; mask and pending signals stay.
    pub fn reset_on_exec(&self) {
        *self.altstack.lock() = AltStack::DISABLED;
        if let Some(m) = self.take_saved() {
            self.set_blocked(m);
        }
    }
}

impl Default for ThreadSignals {
    fn default() -> Self {
        Self::new()
    }
}

/// Process-wide signal state.
pub struct SignalState {
    pub actions: Mutex<[SigAction; NSIG]>,
    /// Process-directed pending signals.
    pub shared: SigQueue,
    /// Stopped threads wait here for SIGCONT.
    pub cont_wq: WaitQueue,
}

impl SignalState {
    pub fn new() -> SignalState {
        SignalState {
            actions: Mutex::new([SigAction::default(); NSIG]),
            shared: SigQueue::new(),
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

pub fn bit(sig: u32) -> u64 {
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
        SIGURG => "SIGURG",
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

/// Whether `action` discards `sig` (explicitly or by default).
fn is_ignored(sig: u32, action: &SigAction) -> bool {
    sig != SIGKILL
        && sig != SIGSTOP
        && (action.handler == SIG_IGN
            || (action.handler == SIG_DFL && matches!(default_action(sig), Default_::Ignore)))
}

/// Drop `mask` from every pending set of `p`.
fn discard_everywhere(p: &Process, mask: u64) {
    p.signals.shared.discard(mask);
    for t in p.live_threads() {
        t.sig.pending.discard(mask);
    }
}

/// `sigaction` changed the disposition of `sig`: drop it if now ignored.
pub fn action_changed(p: &Process, sig: u32) {
    let action = p.signals.actions.lock()[sig as usize];
    if is_ignored(sig, &action) {
        discard_everywhere(p, bit(sig));
    }
}

/// Interrupt a thread blocked in the kernel so it notices a signal.
pub fn kill_thread(t: &Arc<Thread>) {
    t.interrupted.store(true, Ordering::SeqCst);
    sched::wake(t);
}

/// Send-time processing common to all signals: job-control side effects,
/// and whether the signal is to be queued at all.
fn prepare(p: &Arc<Process>, sig: u32) -> bool {
    if sig == 0 || sig as usize >= NSIG || p.zombie.load(Ordering::SeqCst) {
        return false;
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
        discard_everywhere(p, STOP_SIGNALS);
        p.signals.cont_wq.wake_all();
    }
    if STOP_SIGNALS & bit(sig) != 0 {
        discard_everywhere(p, bit(SIGCONT));
    }
    // Ignored signals are dropped at send time (except KILL/STOP).
    if is_ignored(sig, &action) {
        return false;
    }
    !(sig == SIGCONT && action.handler == SIG_DFL)
}

/// Post a process-directed signal to `p` (kernel-generated: SI_KERNEL).
pub fn send(p: &Arc<Process>, sig: u32) {
    send_info(p, sig, SigInfo::kernel());
}

/// Post a process-directed signal with its siginfo.
pub fn send_info(p: &Arc<Process>, sig: u32, info: SigInfo) {
    if !prepare(p, sig) {
        return;
    }
    p.signals.shared.post(sig, info);
    let threads = p.live_threads();
    if sig == SIGKILL {
        for t in &threads {
            kill_thread(t);
        }
    } else if let Some(t) = threads
        .iter()
        .find(|t| t.state() != sched::State::Dead && t.sig.blocked() & bit(sig) == 0)
    {
        // One thread that can take it (the main thread first).
        kill_thread(t);
    }
    // signalfd readers.
    SIGNAL_WQ.wake_all();
}

/// Post a signal to thread `t` of process `p` (tkill/tgkill). SIGKILL and
/// SIGSTOP act on the whole process.
pub fn send_thread(t: &Arc<Thread>, p: &Arc<Process>, sig: u32, info: SigInfo) {
    if sig == SIGKILL || sig == SIGSTOP {
        send_info(p, sig, info);
        return;
    }
    if !prepare(p, sig) {
        return;
    }
    t.sig.pending.post(sig, info);
    if t.sig.blocked() & bit(sig) == 0 {
        kill_thread(t);
    }
    SIGNAL_WQ.wake_all();
}

/// Woken whenever a signal becomes pending (signalfd readiness).
pub static SIGNAL_WQ: WaitQueue = WaitQueue::new();

/// Send to every process in a process group.
pub fn send_group(pgid: u32, sig: u32) -> usize {
    send_group_info(pgid, sig, SigInfo::kernel())
}

pub fn send_group_info(pgid: u32, sig: u32, info: SigInfo) -> usize {
    let members = super::group_members(pgid);
    for p in &members {
        send_info(p, sig, info);
    }
    members.len()
}

/// A signal the current thread raises against itself (SIGPIPE): directed
/// at the thread, SI_USER from its own process, as Linux's `send_sig`.
pub fn send_to_current(sig: u32) {
    if let Some(p) = current() {
        let info = SigInfo::from_process(SI_USER, &p);
        send_thread(&sched::current(), &p, sig, info);
    }
}

/// Deliver a synchronous fault signal to the current thread even if
/// blocked or ignored (Linux's `force_sig_fault`).
pub fn force_fault(p: &Arc<Process>, sig: u32, info: SigInfo) {
    let t = sched::current();
    {
        let mut actions = p.signals.actions.lock();
        let blocked = t.sig.blocked() & bit(sig) != 0;
        if actions[sig as usize].handler == SIG_IGN || blocked {
            actions[sig as usize] = SigAction::default();
        }
    }
    t.sig.blocked.fetch_and(!bit(sig), Ordering::SeqCst);
    t.sig.pending.post(sig, info);
}

/// Signals pending for thread `t` of process `p` (its own and the
/// process's), deliverable or not.
pub fn pending_for(t: &Thread, p: &Process) -> u64 {
    t.sig.pending.pending() | p.signals.shared.pending()
}

/// Whether the current thread has a deliverable signal: interruptible
/// sleeps end when this turns true.
pub fn has_pending() -> bool {
    let Some(t) = sched::try_current() else {
        return false;
    };
    let Some(p) = t.process.lock().clone() else {
        return false;
    };
    // Another thread is tearing the process down (exit_group, a fatal signal) and told this
    // one to die (`do_exit`): every sleep that ends on signals must end, so the thread reaches
    // the return to user mode and exits there. Without this a thread blocked in futex or
    // epoll_wait slept on, keeping the zombie and its memory forever. (The exiting thread
    // itself is not interrupted: its own teardown still sleeps normally.)
    if p.zombie.load(Ordering::SeqCst) && t.interrupted.load(Ordering::SeqCst) {
        return true;
    }
    let shared = p.signals.shared.pending();
    (t.sig.pending.pending() | shared) & !t.sig.blocked() != 0 || shared & bit(SIGKILL) != 0
}

/// Called on every return to user mode.
pub fn deliver_pending(frame: &mut TrapFrame) {
    use x86_64::instructions::interrupts;
    // Delivery writes the user stack under the address-space lock, which
    // another thread of the process may hold while preempted: spinning
    // for it with interrupts off could stall this CPU for good. The trap
    // exit path turns interrupts off again before `iretq`.
    let were_enabled = interrupts::are_enabled();
    interrupts::enable();
    // Decide inside a scope so no Arc is alive if the process must exit.
    let exit_status = deliver_inner(frame);
    if !were_enabled {
        interrupts::disable();
    }
    if let Some(status) = exit_status {
        super::exit_current(status);
    }
}

/// Resolve a pending ERESTARTSYS: re-execute the syscall (`restart`) or
/// report EINTR.
fn finish_restart(frame: &mut TrapFrame, restart: bool) {
    let code = (-crate::syscall::ERESTARTSYS) as u64;
    // Only a frame returning from a system call can carry it.
    if frame.vector == crate::arch::x86_64::idt::VEC_SYSCALL as u64 && frame.rax == code {
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
    r
}

fn deliver_signals(frame: &mut TrapFrame) -> Option<i32> {
    let p = current()?;
    if p.zombie.load(Ordering::SeqCst) {
        return Some(p.exit_status.lock().unwrap_or(0));
    }
    let t = sched::current();
    t.interrupted.store(false, Ordering::SeqCst);
    loop {
        if p.stopped.load(Ordering::SeqCst) {
            // Another thread stopped the process: stop here too.
            wait_while_stopped(&p);
        }
        if p.signals.shared.pending() & bit(SIGKILL) != 0 {
            return Some(SIGKILL as i32);
        }
        let allowed = !t.sig.blocked() | UNBLOCKABLE;
        let Some((sig, info)) = t
            .sig
            .pending
            .take(allowed)
            .or_else(|| p.signals.shared.take(allowed))
        else {
            // No handler runs: a sigsuspend mask ends here.
            if let Some(m) = t.sig.take_saved() {
                t.sig.set_blocked(m);
            }
            return None;
        };
        let action = p.signals.actions.lock()[sig as usize];
        if sig == SIGKILL || sig == SIGSTOP || action.handler == SIG_DFL {
            match default_action(sig) {
                Default_::Ignore | Default_::Continue => continue,
                Default_::Stop => {
                    stop_current(&p, &t, sig);
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
        if setup_frame(&t, frame, sig, &info, &action).is_err() {
            // The frame could not be written (Linux's force_sigsegv): a
            // failing SIGSEGV frame is fatal, any other signal turns into
            // a SIGSEGV whose handler may still run (e.g. on its own stack).
            if sig == SIGSEGV {
                return Some(SIGSEGV as i32 | 0x80);
            }
            force_fault(&p, SIGSEGV, SigInfo::kernel());
            continue;
        }
        if action.flags & SA_RESETHAND != 0 {
            p.signals.actions.lock()[sig as usize] = SigAction::default();
        }
        return None;
    }
}

fn continued_or_killed(p: &Process) -> bool {
    !p.stopped.load(Ordering::SeqCst) || p.signals.shared.pending() & bit(SIGKILL) != 0
}

fn wait_while_stopped(p: &Arc<Process>) {
    let pp = p.clone();
    p.signals.cont_wq.wait_until(|| continued_or_killed(&pp));
}

fn stop_current(p: &Arc<Process>, me: &Arc<Thread>, sig: u32) {
    p.stop_sig.store(sig as i32, Ordering::SeqCst);
    p.stop_reported.store(false, Ordering::SeqCst);
    p.stopped.store(true, Ordering::SeqCst);
    // The other threads stop on their way back to user mode.
    for t in p.live_threads() {
        if !Arc::ptr_eq(&t, me) {
            kill_thread(&t);
        }
    }
    if let Some(parent) = p.parent() {
        let nocldstop = parent.signals.actions.lock()[SIGCHLD as usize].flags & SA_NOCLDSTOP != 0;
        if !nocldstop {
            send_info(&parent, SIGCHLD, SigInfo::child(CLD_STOPPED, sig as i32, p));
        }
        parent.child_wq.wake_all();
    }
    crate::tty::process_stopped(p);
    wait_while_stopped(p);
}

/// The CPU's initial FPU/SSE state (FCW 0x037F, MXCSR 0x1F80).
fn initial_fpu() -> [u8; 512] {
    let mut a = [0u8; 512];
    a[0] = 0x7F;
    a[1] = 0x03;
    a[24] = 0x80;
    a[25] = 0x1F;
    a
}

/// Push an `rt_sigframe` and enter the handler, as Linux's
/// `get_sigframe` + `__setup_rt_frame` + `handle_signal`.
fn setup_frame(
    t: &Arc<Thread>,
    frame: &mut TrapFrame,
    sig: u32,
    info: &SigInfo,
    action: &SigAction,
) -> KResult<()> {
    let blocked = t.sig.blocked();
    // After a sigsuspend the handler returns to the caller's mask.
    let saved = t.sig.take_saved();
    let r = write_frame(t, frame, sig, info, action, saved.unwrap_or(blocked));
    if r.is_err() {
        t.sig.put_saved(saved);
        return r;
    }
    // Committed: block the handler's mask, start it with a clean FPU.
    let mut mask = blocked | action.mask;
    if action.flags & SA_NODEFER == 0 {
        mask |= bit(sig);
    }
    t.sig.set_blocked(mask);
    // The saved copy and the live registers change together (a context
    // switch in between would save the live state over the new one).
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        (*t.fpu.get()).0 = initial_fpu();
        core::arch::asm!("fxrstor64 [{}]", in(reg) t.fpu.get(), options(nostack));
    });
    Ok(())
}

fn write_frame(
    t: &Arc<Thread>,
    frame: &mut TrapFrame,
    sig: u32,
    info: &SigInfo,
    action: &SigAction,
    old_mask: u64,
) -> KResult<()> {
    let alt = *t.sig.altstack.lock();
    // Skip the red zone, then switch to the alternate stack if asked for
    // and not already on it.
    let nested = alt.on_stack(frame.rsp);
    let mut sp = frame.rsp.wrapping_sub(128);
    let mut entering = false;
    if action.flags & SA_ONSTACK != 0 && alt.ss_flags(sp) == 0 {
        sp = alt.sp.wrapping_add(alt.size);
        entering = true;
    }
    let fpstate = sp.wrapping_sub(FPSTATE_SIZE) & !63;
    sp = (fpstate.wrapping_sub(size_of::<RtSigFrame>() as u64) & !15).wrapping_sub(8);
    if (nested || entering) && !alt.contains(sp) {
        return Err(EFAULT); // alternate stack overflow
    }
    if sp < 4096 || fpstate.saturating_add(FPSTATE_SIZE) > crate::mm::USER_END {
        return Err(EFAULT);
    }

    let (trapno, err, cr2) = if frame.vector < 32 {
        let cr2 = if frame.vector == 14 { info.addr } else { 0 };
        (frame.vector, frame.error_code, cr2)
    } else {
        (0, 0, 0)
    };
    let sf = RtSigFrame {
        pretcode: action.restorer,
        uc: UContext {
            uc_flags: UC_SIGCONTEXT_SS | UC_STRICT_RESTORE_SS,
            uc_link: 0,
            uc_stack: StackT {
                ss_sp: alt.sp,
                ss_flags: alt.flags,
                _pad: 0,
                ss_size: alt.size,
            },
            uc_mcontext: SigContext {
                r8: frame.r8,
                r9: frame.r9,
                r10: frame.r10,
                r11: frame.r11,
                r12: frame.r12,
                r13: frame.r13,
                r14: frame.r14,
                r15: frame.r15,
                rdi: frame.rdi,
                rsi: frame.rsi,
                rbp: frame.rbp,
                rbx: frame.rbx,
                rdx: frame.rdx,
                rax: frame.rax,
                rcx: frame.rcx,
                rsp: frame.rsp,
                rip: frame.rip,
                eflags: frame.rflags,
                cs: frame.cs as u16,
                gs: 0,
                fs: 0,
                ss: frame.ss as u16,
                err,
                trapno,
                oldmask: old_mask,
                cr2,
                fpstate,
                reserved1: [0; 8],
            },
            uc_sigmask: old_mask,
        },
        info: info.to_user(sig),
    };
    let mut fpu = [0u8; 512];
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        core::arch::asm!("fxsave64 [{}]", in(reg) t.fpu.get(), options(nostack));
        fpu.copy_from_slice(&(*t.fpu.get()).0);
    });
    // The software-reserved tail carries no XSAVE extension.
    fpu[464..].fill(0);
    uaccess::copy_to_user(fpstate, &fpu)?;
    uaccess::write_user(sp, &sf)?;

    if entering && alt.flags & SS_AUTODISARM != 0 {
        *t.sig.altstack.lock() = AltStack::DISABLED;
    }
    frame.rsp = sp;
    frame.rip = action.handler;
    frame.rdi = sig as u64;
    frame.rsi = sp + offset_of!(RtSigFrame, info) as u64;
    frame.rdx = sp + offset_of!(RtSigFrame, uc) as u64;
    frame.rax = 0;
    frame.cs = gdt::USER_CS as u64;
    frame.ss = gdt::USER_DS as u64;
    // DF, TF and RF cleared, as Linux.
    frame.rflags &= !(0x400 | 0x100 | 0x1_0000);
    Ok(())
}

/// rt_sigreturn: restore the context of the `rt_sigframe` below the stack
/// pointer, including any edits the handler made to it. A bad frame is
/// fatal (SIGSEGV), as on Linux.
pub fn sigreturn(frame: &mut TrapFrame) -> KResult<()> {
    let p = current().ok_or(ESRCH)?;
    let r = restore_frame(frame);
    if r.is_err() {
        crate::serial_println!(
            "[signal] pid {} ({}): bad rt_sigreturn frame at {:#x}",
            p.pid,
            p.name.lock(),
            frame.rsp
        );
        force_fault(&p, SIGSEGV, SigInfo::kernel());
    }
    r
}

fn canonical(a: u64) -> bool {
    ((a as i64) << 16 >> 16) as u64 == a
}

fn restore_frame(frame: &mut TrapFrame) -> KResult<()> {
    let t = sched::current();
    // The handler's `ret` popped `pretcode`.
    let sp = frame.rsp.wrapping_sub(8);
    let sf: RtSigFrame = uaccess::read_user(sp)?;
    let mc = sf.uc.uc_mcontext;
    if mc.rip >= crate::mm::USER_END || !canonical(mc.rsp) {
        return Err(EFAULT);
    }
    // FPU state: none means the initial state.
    let mut fpu = initial_fpu();
    if mc.fpstate != 0 {
        if !mc.fpstate.is_multiple_of(16)
            || mc.fpstate.saturating_add(FPSTATE_SIZE) > crate::mm::USER_END
        {
            return Err(EFAULT);
        }
        uaccess::copy_from_user(&mut fpu, mc.fpstate)?;
    }
    let mut r = *frame;
    r.r8 = mc.r8;
    r.r9 = mc.r9;
    r.r10 = mc.r10;
    r.r11 = mc.r11;
    r.r12 = mc.r12;
    r.r13 = mc.r13;
    r.r14 = mc.r14;
    r.r15 = mc.r15;
    r.rdi = mc.rdi;
    r.rsi = mc.rsi;
    r.rbp = mc.rbp;
    r.rbx = mc.rbx;
    r.rdx = mc.rdx;
    r.rax = mc.rax;
    r.rcx = mc.rcx;
    r.rsp = mc.rsp;
    r.rip = mc.rip;
    // Never let user code forge kernel segments or privileged flags.
    r.cs = gdt::USER_CS as u64;
    r.ss = gdt::USER_DS as u64;
    r.rflags = (mc.eflags & 0xCD5) | 0x202;
    // Not a system call return: nothing to restart.
    r.vector = u64::MAX;
    r.error_code = 0;
    *frame = r;
    t.sig.set_blocked(sf.uc.uc_sigmask);
    x86_64::instructions::interrupts::without_interrupts(|| unsafe {
        let cur = &mut (*t.fpu.get()).0;
        // Clear MXCSR bits the CPU does not support (fxrstor would fault),
        // using the MXCSR_MASK the CPU reported in the last fxsave.
        let mask = match u32::from_le_bytes([cur[28], cur[29], cur[30], cur[31]]) {
            0 => 0xFFBF,
            m => m,
        };
        let mxcsr = u32::from_le_bytes([fpu[24], fpu[25], fpu[26], fpu[27]]) & mask;
        fpu[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        fpu[28..32].copy_from_slice(&mask.to_le_bytes());
        cur.copy_from_slice(&fpu);
        core::arch::asm!("fxrstor64 [{}]", in(reg) t.fpu.get(), options(nostack));
    });
    // The alternate stack as the handler left it in uc_stack (errors, such
    // as still running on it, are ignored as on Linux).
    let _ = set_altstack(&t, &sf.uc.uc_stack, frame.rsp);
    Ok(())
}

// ---------------------------------------------------------------------------
// sigaltstack
// ---------------------------------------------------------------------------

/// Linux's `do_sigaltstack` (set part); `sp` is the user stack pointer.
fn set_altstack(t: &Thread, ss: &StackT, sp: u64) -> KResult<()> {
    let mut alt = t.sig.altstack.lock();
    if alt.on_stack(sp) {
        return Err(EPERM);
    }
    let mode = ss.ss_flags & !SS_AUTODISARM;
    if mode != SS_DISABLE && mode != SS_ONSTACK && mode != 0 {
        return Err(EINVAL);
    }
    if alt.sp == ss.ss_sp && alt.size == ss.ss_size && alt.flags == ss.ss_flags {
        return Ok(());
    }
    if mode == SS_DISABLE {
        *alt = AltStack {
            sp: 0,
            size: 0,
            flags: ss.ss_flags,
        };
    } else {
        if ss.ss_size < MINSIGSTKSZ {
            return Err(ENOMEM);
        }
        *alt = AltStack {
            sp: ss.ss_sp,
            size: ss.ss_size,
            flags: ss.ss_flags,
        };
    }
    Ok(())
}

/// sigaltstack(2) for the calling thread, whose user stack pointer is `sp`.
pub fn sigaltstack(sp: u64, ss: u64, old: u64) -> KResult<()> {
    let t = sched::current();
    let new = if ss != 0 {
        Some(uaccess::read_user::<StackT>(ss)?)
    } else {
        None
    };
    let prev = {
        let a = *t.sig.altstack.lock();
        StackT {
            ss_sp: a.sp,
            ss_flags: a.ss_flags(sp) | (a.flags & SS_AUTODISARM),
            _pad: 0,
            ss_size: a.size,
        }
    };
    if let Some(n) = new {
        set_altstack(&t, &n, sp)?;
    }
    if old != 0 {
        uaccess::write_user(old, &prev)?;
    }
    Ok(())
}
