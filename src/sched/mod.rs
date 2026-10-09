//! Preemptive scheduler.
//!
//! Every thread (kernel or user) owns a kernel stack. A context switch saves
//! the callee-saved registers on the outgoing thread's stack and resumes the
//! incoming one; everything else (user registers, interrupted kernel state)
//! is already on the stacks as trap frames. The timer tick requests a
//! reschedule; the switch happens at the next trap exit or explicit
//! [`schedule`] call.
//!
//! Each CPU has its own run queue. A woken thread goes back to the CPU it
//! last ran on (or an idle CPU its affinity allows) and that CPU is kicked
//! with an IPI; a CPU whose queue is empty steals from the busiest one.
//! A thread's `on_cpu` flag stays set until its context has been fully
//! saved, so another CPU never resumes a thread whose registers are still
//! being written. Timed wake-ups and kernel timers (timerfd, itimers) live
//! in one deadline heap served by the timer tick.

pub mod mutex;
pub mod wait;

use crate::arch::x86_64::{cpu, idt::TrapFrame};
use crate::mm::KernelStack;
use crate::sync::Mutex;
use alloc::boxed::Box;
use alloc::collections::{BinaryHeap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::cmp::Reverse;
use core::sync::atomic::{
    AtomicBool, AtomicI8, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

pub use wait::WaitQueue;

pub type Tid = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Ready = 0,
    Running = 1,
    Blocked = 2,
    Dead = 3,
}

const KSTACK_SIZE: u64 = 64 * 1024;
/// Timer ticks a thread may run before being preempted.
const QUANTUM_TICKS: u32 = 3;

#[repr(C, align(16))]
pub struct FpuState(pub [u8; 512]);

pub struct Thread {
    pub tid: Tid,
    pub name: Mutex<String>,
    state: AtomicU8,
    /// Saved kernel RSP while not running.
    rsp: UnsafeCell<u64>,
    /// 1 while the thread's context is live on some CPU.
    on_cpu: AtomicU64,
    kstack: KernelStack,
    /// Owning process for user threads.
    pub process: Mutex<Option<Arc<crate::process::Process>>>,
    /// Page-table root (physical) to load when this thread runs.
    pub cr3: AtomicU64,
    pub fs_base: AtomicU64,
    pub fpu: UnsafeCell<FpuState>,
    pub is_user: AtomicBool,
    /// Wake-up time for timed sleeps (0 = none).
    /// Sequence number of the timed wakeup armed last; a timer entry only
    /// wakes the thread if it is still the current one (a woken thread's
    /// old entries become stale without anyone clearing this).
    wake_seq: AtomicU64,
    /// Pending wakeup that raced with blocking.
    wakeup_pending: AtomicBool,
    quantum: AtomicU8,
    /// Set by `kill` to make blocking syscalls return early.
    pub interrupted: AtomicBool,
    /// Absolute deadline of an interrupted nanosleep (for restart).
    pub restart_deadline: AtomicU64,
    /// The signal mask to restore on return to user mode after a syscall
    /// that waited with a temporary one (ppoll, pselect6, epoll_pwait,
    /// sigsuspend), valid while `restore_sigmask` is set.
    pub saved_sigmask: AtomicU64,
    pub restore_sigmask: AtomicBool,
    /// Blocked signals (per thread, as in Linux; pending ones are shared by
    /// the process).
    pub sigmask: AtomicU64,
    /// CPU this thread last ran on.
    last_cpu: AtomicU32,
    /// CPUs this thread may run on (bit per CPU).
    pub affinity: AtomicU64,
    /// Nice value (-20 .. 19): scales the time slice.
    pub nice: AtomicI8,
    /// User address to clear and futex-wake when the thread exits
    /// (`set_tid_address` / `CLONE_CHILD_CLEARTID`).
    pub clear_child_tid: AtomicU64,
    /// CPU time used in user and kernel mode, in timer ticks (`time::HZ`).
    pub utime: AtomicU64,
    pub stime: AtomicU64,
    /// System call in progress (u64::MAX: none) and its first argument,
    /// for state dumps.
    pub syscall: AtomicU64,
    pub syscall_arg: AtomicU64,
    /// Wait queue the thread last blocked on (state dumps).
    pub wchan: AtomicU64,
    /// LinuxKPI: this thread's Linux `task_struct` shadow (null until Linux
    /// code first asks for `current`).
    pub linux_task: core::sync::atomic::AtomicPtr<core::ffi::c_void>,
    /// `core::panic::Location` of the last state change (state dumps).
    state_site: AtomicUsize,
}

unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

impl Thread {
    pub fn state(&self) -> State {
        match self.state.load(Ordering::SeqCst) {
            0 => State::Ready,
            1 => State::Running,
            2 => State::Blocked,
            _ => State::Dead,
        }
    }

    #[track_caller]
    fn set_state(&self, s: State) {
        self.state.store(s as u8, Ordering::SeqCst);
        self.note_state_site();
    }

    /// Remember where the state last changed (state dumps).
    #[track_caller]
    fn note_state_site(&self) {
        self.state_site.store(
            core::panic::Location::caller() as *const _ as usize,
            Ordering::Relaxed,
        );
    }

    /// Where the state last changed ("file:line"), for state dumps.
    pub fn state_site(&self) -> Option<&'static core::panic::Location<'static>> {
        let p = self.state_site.load(Ordering::Relaxed) as *const core::panic::Location<'static>;
        unsafe { p.as_ref() }
    }

    pub fn kstack_top(&self) -> u64 {
        self.kstack.top()
    }

    pub fn last_cpu(&self) -> u32 {
        self.last_cpu.load(Ordering::Relaxed)
    }

    fn allowed_on(&self, cpu: u32) -> bool {
        cpu < 64 && self.affinity.load(Ordering::Relaxed) & (1 << cpu) != 0
    }

    /// Timer ticks per slice: 3 at nice 0, 1 at nice 19, 7 at nice -20.
    fn slice(&self) -> u8 {
        (3 - self.nice.load(Ordering::Relaxed) as i32 / 5).clamp(1, 7) as u8
    }
}

static NEXT_TID: AtomicU64 = AtomicU64::new(1);
/// Per-CPU run queues.
static RUN_QUEUES: [Mutex<VecDeque<Arc<Thread>>>; cpu::MAX_CPUS] =
    [const { Mutex::new(VecDeque::new()) }; cpu::MAX_CPUS];
/// Deadline heap for timed wake-ups and kernel timers.
static TIMERS: Mutex<BinaryHeap<Reverse<TimerEntry>>> = Mutex::new(BinaryHeap::new());
static TIMER_SEQ: AtomicU64 = AtomicU64::new(0);
static DEAD: Mutex<Vec<Arc<Thread>>> = Mutex::new(Vec::new());
static ALL: Mutex<Vec<alloc::sync::Weak<Thread>>> = Mutex::new(Vec::new());
static RUNNING: AtomicBool = AtomicBool::new(false);
static CONTEXT_SWITCHES: AtomicUsize = AtomicUsize::new(0);

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

pub fn context_switches() -> usize {
    CONTEXT_SWITCHES.load(Ordering::Relaxed)
}

fn irqsave<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

fn this_cpu() -> u32 {
    if cpu::is_initialized() {
        cpu::this().cpu_id
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------------

/// Something to do at a deadline. Runs in the timer interrupt: it must
/// only take interrupt-safe locks.
pub trait TimerTarget: Send + Sync {
    fn fire(self: Arc<Self>, now_ns: u64);
}

enum TimerKind {
    /// Wake a thread whose `wake_seq` is still this entry's sequence.
    Wake(Arc<Thread>),
    Call(Arc<dyn TimerTarget>),
}

struct TimerEntry {
    deadline: u64,
    seq: u64,
    kind: TimerKind,
}

impl PartialEq for TimerEntry {
    fn eq(&self, o: &Self) -> bool {
        (self.deadline, self.seq) == (o.deadline, o.seq)
    }
}
impl Eq for TimerEntry {}
impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, o: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for TimerEntry {
    fn cmp(&self, o: &Self) -> core::cmp::Ordering {
        (self.deadline, self.seq).cmp(&(o.deadline, o.seq))
    }
}

fn push_timer(deadline: u64, kind: TimerKind) {
    let seq = TIMER_SEQ.fetch_add(1, Ordering::Relaxed);
    push_timer_seq(deadline, seq, kind);
}

/// Pending timers and how far away the earliest is (ms; negative: late),
/// for state dumps.
pub fn timer_summary() -> Option<(usize, i64)> {
    let now = crate::time::nanos() as i64;
    irqsave(|| {
        TIMERS.try_lock().map(|h| {
            (
                h.len(),
                h.peek()
                    .map_or(0, |e| (e.0.deadline as i64 - now) / 1_000_000),
            )
        })
    })
}

fn push_timer_seq(deadline: u64, seq: u64, kind: TimerKind) {
    irqsave(|| {
        TIMERS.lock().push(Reverse(TimerEntry {
            deadline,
            seq,
            kind,
        }))
    });
}

/// Call `target.fire()` from the timer tick once `deadline_ns` (monotonic)
/// has passed. Targets re-arm themselves for periodic timers and ignore
/// stale firings.
pub fn add_timer(deadline_ns: u64, target: Arc<dyn TimerTarget>) {
    push_timer(deadline_ns, TimerKind::Call(target));
}

static DEFERRED: Mutex<VecDeque<Box<dyn FnOnce() + Send>>> = Mutex::new(VecDeque::new());
static DEFERRED_WQ: WaitQueue = WaitQueue::new();

/// Run `f` soon on the `kworker` thread (for work that timers and
/// interrupt handlers may not do themselves, like sending signals).
pub fn defer(f: impl FnOnce() + Send + 'static) {
    irqsave(|| DEFERRED.lock().push_back(Box::new(f)));
    DEFERRED_WQ.wake_all();
}

/// Start the deferred-work thread (after the scheduler is up).
pub fn start_worker() {
    WORKER_STARTED.store(true, Ordering::SeqCst);
    spawn("kworker", || {
        loop {
            DEFERRED_WQ.wait_until(|| irqsave(|| !DEFERRED.lock().is_empty()));
            while let Some(f) = irqsave(|| DEFERRED.lock().pop_front()) {
                f();
            }
        }
    });
}

/// Run every timer that is due.
fn run_timers(now: u64) {
    let mut due = Vec::new();
    {
        let mut h = TIMERS.lock();
        while h.peek().is_some_and(|e| e.0.deadline <= now) {
            due.push(h.pop().unwrap().0);
        }
    }
    for e in due {
        match e.kind {
            TimerKind::Wake(t) => {
                // Skip entries of waits that were re-armed since. A stale
                // entry of the current arming can at most wake the thread
                // early; waits re-check their condition.
                if t.wake_seq.load(Ordering::SeqCst) == e.seq && t.state() == State::Blocked {
                    wake(&t);
                }
            }
            TimerKind::Call(c) => c.fire(now),
        }
    }
}

// ---------------------------------------------------------------------------
// Context switch
// ---------------------------------------------------------------------------

core::arch::global_asm!(
    ".global sched_switch",
    // rdi = &mut old_rsp, rsi = new_rsp, rdx = &old.on_cpu
    "sched_switch:",
    "    push rbp",
    "    push rbx",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov [rdi], rsp",
    "    mov qword ptr [rdx], 0",
    "    mov rsp, rsi",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop rbx",
    "    pop rbp",
    "    ret",
    ".global sched_thread_trampoline",
    "sched_thread_trampoline:",
    // Stack: entry fn, arg (placed by `new_thread`).
    "    pop rax",
    "    pop rdi",
    "    call rax",
    "    ud2",
);

unsafe extern "C" {
    fn sched_switch(old_rsp: *mut u64, new_rsp: u64, old_on_cpu: *mut u64);
    fn sched_thread_trampoline();
}

extern "C" fn kernel_thread_entry(boxed: usize) -> ! {
    finish_switch();
    x86_64::instructions::interrupts::enable();
    let f: Box<Box<dyn FnOnce() + Send>> = unsafe { Box::from_raw(boxed as *mut _) };
    f();
    exit_current();
}

/// Build a thread whose first switch-in calls `entry(arg)`.
fn new_thread(name: &str, entry: u64, arg: u64) -> Arc<Thread> {
    let kstack = KernelStack::new(KSTACK_SIZE).expect("out of kernel stack space");
    let top = kstack.top();
    // Layout (growing down): [arg][entry][trampoline ret][6 callee-saved regs]
    let frame: [u64; 9] = [
        0,
        0,
        0,
        0,
        0,
        0, // r15..rbp
        sched_thread_trampoline as *const () as u64,
        entry,
        arg,
    ];
    // The trampoline pops `entry` and `arg` and then `call`s: rsp must be
    // 16-byte aligned at that call (SysV ABI), i.e. sp + 72 ≡ 0 (mod 16).
    let sp = (top & !15) - 8 * (frame.len() as u64 + 2);
    debug_assert_eq!((sp + 8 * frame.len() as u64) % 16, 0);
    unsafe {
        core::ptr::copy_nonoverlapping(frame.as_ptr(), sp as *mut u64, frame.len());
    }
    let t = Arc::new(Thread {
        tid: NEXT_TID.fetch_add(1, Ordering::SeqCst),
        name: Mutex::new(String::from(name)),
        state: AtomicU8::new(State::Ready as u8),
        rsp: UnsafeCell::new(sp),
        on_cpu: AtomicU64::new(0),
        kstack,
        process: Mutex::new(None),
        cr3: AtomicU64::new(crate::mm::KERNEL_PML4.load(Ordering::SeqCst)),
        fs_base: AtomicU64::new(0),
        fpu: UnsafeCell::new(FpuState(initial_fpu())),
        is_user: AtomicBool::new(false),
        wake_seq: AtomicU64::new(u64::MAX),
        wakeup_pending: AtomicBool::new(false),
        quantum: AtomicU8::new(QUANTUM_TICKS as u8),
        interrupted: AtomicBool::new(false),
        restart_deadline: AtomicU64::new(0),
        saved_sigmask: AtomicU64::new(0),
        restore_sigmask: AtomicBool::new(false),
        sigmask: AtomicU64::new(0),
        last_cpu: AtomicU32::new(this_cpu()),
        affinity: AtomicU64::new(u64::MAX),
        nice: AtomicI8::new(0),
        clear_child_tid: AtomicU64::new(0),
        utime: AtomicU64::new(0),
        stime: AtomicU64::new(0),
        syscall: AtomicU64::new(u64::MAX),
        syscall_arg: AtomicU64::new(0),
        wchan: AtomicU64::new(0),
        linux_task: core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()),
        state_site: AtomicUsize::new(0),
    });
    irqsave(|| ALL.lock().push(Arc::downgrade(&t)));
    t
}

fn initial_fpu() -> [u8; 512] {
    let mut a = [0u8; 512];
    // FCW = 0x037F, MXCSR = 0x1F80 (all exceptions masked).
    a[0] = 0x7F;
    a[1] = 0x03;
    a[24] = 0x80;
    a[25] = 0x1F;
    a
}

/// Spawn a kernel thread running `f`.
pub fn spawn<F: FnOnce() + Send + 'static>(name: &str, f: F) -> Arc<Thread> {
    let boxed: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(f));
    let t = new_thread(
        name,
        kernel_thread_entry as *const () as u64,
        Box::into_raw(boxed) as u64,
    );
    make_ready(t.clone());
    t
}

/// Create a thread that starts by returning to user mode through `frame`
/// (used by fork, clone and the initial exec). Not yet runnable.
pub fn new_user_thread(name: &str, frame: &TrapFrame, cr3: u64) -> Arc<Thread> {
    let t = new_thread(name, user_thread_entry as *const () as u64, 0);
    // Place a copy of the trap frame at the top of the kernel stack and pass
    // its address as the argument.
    let top = t.kstack.top();
    let fsz = core::mem::size_of::<TrapFrame>() as u64;
    let fptr = (top - fsz - 16) & !0xF;
    unsafe {
        core::ptr::write(fptr as *mut TrapFrame, *frame);
        // Rebuild the switch frame below the trap frame.
        let frame_words: [u64; 9] = [
            0,
            0,
            0,
            0,
            0,
            0,
            sched_thread_trampoline as *const () as u64,
            user_thread_entry as *const () as u64,
            fptr,
        ];
        let sp = fptr - 8 * (frame_words.len() as u64 + 1);
        core::ptr::copy_nonoverlapping(frame_words.as_ptr(), sp as *mut u64, frame_words.len());
        *t.rsp.get() = sp;
    }
    t.cr3.store(cr3, Ordering::SeqCst);
    t.is_user.store(true, Ordering::SeqCst);
    t
}

extern "C" fn user_thread_entry(frame: u64) -> ! {
    finish_switch();
    // Jump into the common trap-return path with RSP at the frame.
    unsafe {
        core::arch::asm!(
            "mov rsp, {f}",
            "jmp {ret}",
            f = in(reg) frame,
            ret = in(reg) crate::arch::x86_64::idt::trap_return_addr(),
            options(noreturn)
        );
    }
}

/// Current thread (panics before the scheduler starts).
pub fn current() -> Arc<Thread> {
    try_current().expect("no current thread")
}

/// Current thread if the scheduler is running on this CPU.
pub fn try_current() -> Option<Arc<Thread>> {
    if !cpu::is_initialized() {
        return None;
    }
    // With interrupts off: a thread preempted between finding its CPU and
    // reading that CPU's current thread may resume elsewhere and would
    // read another thread (a different process, or a kernel thread).
    irqsave(|| {
        let p = cpu::this().current.load(Ordering::SeqCst) as *const Thread;
        if p.is_null() {
            return None;
        }
        unsafe {
            Arc::increment_strong_count(p);
            Some(Arc::from_raw(p))
        }
    })
}

/// Record the current thread's system call (u64::MAX: none) for state
/// dumps. Takes no reference: `exit` never returns to drop one.
pub fn note_syscall(nr: u64, arg: u64) {
    if !cpu::is_initialized() {
        return;
    }
    irqsave(|| {
        let p = cpu::this().current.load(Ordering::SeqCst) as *const Thread;
        if let Some(t) = unsafe { p.as_ref() } {
            t.syscall.store(nr, Ordering::Relaxed);
            t.syscall_arg.store(arg, Ordering::Relaxed);
        }
    });
}

pub fn current_tid() -> Tid {
    try_current().map(|t| t.tid).unwrap_or(0)
}

/// Adopt the boot flow as this CPU's first thread and create its idle thread.
pub fn init_cpu(name: &str) {
    let boot = new_thread(name, 0, 0);
    boot.set_state(State::Running);
    boot.on_cpu.store(1, Ordering::SeqCst);
    let pc = cpu::this();
    pc.current
        .store(Arc::into_raw(boot) as usize, Ordering::SeqCst);

    let idle = new_thread("idle", idle_entry as *const () as u64, 0);
    pc.idle
        .store(Arc::into_raw(idle) as usize, Ordering::SeqCst);
    enable_fpu();
    RUNNING.store(true, Ordering::SeqCst);
}

extern "C" fn idle_entry(_arg: u64) -> ! {
    finish_switch();
    loop {
        x86_64::instructions::interrupts::enable_and_hlt();
        schedule();
    }
}

fn enable_fpu() {
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
    unsafe {
        let mut cr0 = Cr0::read();
        cr0.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
        cr0.insert(Cr0Flags::MONITOR_COPROCESSOR);
        Cr0::write(cr0);
        let mut cr4 = Cr4::read();
        cr4.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
        Cr4::write(cr4);
        core::arch::asm!("fninit");
    }
}

/// Queue a runnable thread: on its last CPU if that is idle, else on an
/// idle CPU it may use, else on its last CPU; kick the chosen CPU.
fn enqueue(t: Arc<Thread>) {
    let me = this_cpu();
    let n = cpu::cpu_count().clamp(1, cpu::MAX_CPUS as u32);
    let idle = |id: u32| {
        cpu::cpu(id).is_some_and(|c| {
            c.idle.load(Ordering::SeqCst) != 0
                && c.current.load(Ordering::SeqCst) == c.idle.load(Ordering::SeqCst)
        })
    };
    let last = t.last_cpu().min(n - 1);
    let target = if t.allowed_on(last) && idle(last) {
        last
    } else if let Some(c) =
        (0..n).find(|&c| t.allowed_on(c) && idle(c) && RUN_QUEUES[c as usize].lock().is_empty())
    {
        c
    } else if t.allowed_on(last) {
        last
    } else {
        (0..n).find(|&c| t.allowed_on(c)).unwrap_or(me)
    };
    RUN_QUEUES[target as usize].lock().push_back(t);
    if target != me {
        crate::arch::x86_64::smp::kick_cpu(target);
    }
}

pub fn make_ready(t: Arc<Thread>) {
    irqsave(|| {
        t.set_state(State::Ready);
        enqueue(t);
    });
}

/// Wake a blocked thread (no-op if it is not blocked).
#[track_caller]
pub fn wake(t: &Arc<Thread>) {
    let site = core::panic::Location::caller() as *const _ as usize;
    irqsave(|| {
        if t.state
            .compare_exchange(
                State::Blocked as u8,
                State::Ready as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            t.state_site.store(site, Ordering::Relaxed);
            enqueue(t.clone());
        } else {
            t.wakeup_pending.store(true, Ordering::SeqCst);
        }
    });
}

/// Whether `tid` is in some run queue (state dumps; may miss a thread
/// being moved between queues).
pub fn is_queued(tid: Tid) -> bool {
    let n = cpu::cpu_count().clamp(1, cpu::MAX_CPUS as u32) as usize;
    irqsave(|| {
        RUN_QUEUES[..n]
            .iter()
            .any(|q| q.try_lock().is_some_and(|q| q.iter().any(|t| t.tid == tid)))
    })
}

/// Number of runnable threads queued on each CPU.
pub fn queue_lengths() -> Vec<usize> {
    let n = cpu::cpu_count().clamp(1, cpu::MAX_CPUS as u32) as usize;
    irqsave(|| RUN_QUEUES[..n].iter().map(|q| q.lock().len()).collect())
}

/// Find a thread by id.
pub fn find_thread(tid: Tid) -> Option<Arc<Thread>> {
    irqsave(|| {
        ALL.lock()
            .iter()
            .filter_map(|w| w.upgrade())
            .find(|t| t.tid == tid)
    })
}

/// Restrict `t` to the CPUs in `mask`; moves it if it runs elsewhere.
pub fn set_affinity(t: &Arc<Thread>, mask: u64) {
    let n = cpu::cpu_count().clamp(1, 64);
    let mask = mask & if n >= 64 { u64::MAX } else { (1u64 << n) - 1 };
    t.affinity.store(mask, Ordering::SeqCst);
    if Arc::ptr_eq(t, &current()) {
        if !t.allowed_on(this_cpu()) {
            schedule();
        }
    } else if t.state() == State::Running {
        crate::arch::x86_64::smp::kick_cpu(t.last_cpu());
    }
}

/// Mark the current thread blocked; the caller must then call [`schedule`].
/// Returns false if a wakeup already arrived (so the caller should not sleep).
pub fn prepare_block() -> bool {
    let t = current();
    if t.wakeup_pending.swap(false, Ordering::SeqCst) {
        return false;
    }
    t.set_state(State::Blocked);
    true
}

/// Clear a pending wakeup flag before starting a wait (call before checking
/// the wait condition).
pub fn clear_pending_wakeup() {
    if let Some(t) = try_current() {
        t.wakeup_pending.store(false, Ordering::SeqCst);
    }
}

/// Arm a timed wakeup for the current (about to block) thread.
pub(crate) fn arm_timeout(t: &Arc<Thread>, deadline_ns: u64) {
    let seq = TIMER_SEQ.fetch_add(1, Ordering::Relaxed);
    t.wake_seq.store(seq, Ordering::SeqCst);
    push_timer_seq(deadline_ns, seq, TimerKind::Wake(t.clone()));
}

pub(crate) fn set_blocked(t: &Thread) {
    t.set_state(State::Blocked);
}

/// Block the current thread until `deadline_ns` (monotonic).
pub fn sleep_until(deadline_ns: u64) {
    if !is_running() {
        while crate::time::nanos() < deadline_ns {
            core::hint::spin_loop();
        }
        return;
    }
    let t = current();
    loop {
        if crate::time::nanos() >= deadline_ns {
            return;
        }
        irqsave(|| {
            t.set_state(State::Blocked);
            arm_timeout(&t, deadline_ns);
        });
        schedule();
        if t.interrupted.load(Ordering::SeqCst) {
            return;
        }
    }
}

pub fn yield_now() {
    if is_running() {
        schedule();
    }
}

/// Terminate the current thread.
pub fn exit_current() -> ! {
    let t = current();
    // Its CPU time stays with the process (getrusage, /proc/PID/stat).
    if let Some(p) = t.process.lock().as_ref() {
        p.add_exited_thread_time(
            t.utime.load(Ordering::Relaxed),
            t.stime.load(Ordering::Relaxed),
        );
    }
    irqsave(|| {
        t.set_state(State::Dead);
        DEAD.lock().push(t.clone());
    });
    drop(t);
    schedule();
    unreachable!("dead thread rescheduled");
}

/// Ticks each CPU spent in user mode, in the kernel and idle (`/proc/stat`).
pub struct CpuTime {
    pub user: AtomicU64,
    pub system: AtomicU64,
    pub idle: AtomicU64,
}

pub static CPU_TIME: [CpuTime; cpu::MAX_CPUS] = [const {
    CpuTime {
        user: AtomicU64::new(0),
        system: AtomicU64::new(0),
        idle: AtomicU64::new(0),
    }
}; cpu::MAX_CPUS];

/// Called from the timer interrupt; `user` tells whether it interrupted
/// user mode.
pub fn timer_tick(user: bool) {
    if !is_running() {
        return;
    }
    run_timers(crate::time::nanos());
    let pc = cpu::this();
    // RCU readers run with preemption disabled: a tick that interrupted
    // preemptible code (only this interrupt's own hard-IRQ count) is a
    // quiescent state for this CPU.
    if pc.preempt_count.load(Ordering::Relaxed) & !cpu::HARDIRQ_MASK == 0 {
        pc.rcu_qs.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(feature = "linuxkpi")]
    if pc.cpu_id == 0 {
        crate::linuxkpi::tick();
    }
    let cur = pc.current.load(Ordering::SeqCst) as *const Thread;
    if cur.is_null() {
        return;
    }
    let cur = unsafe { &*cur };
    let times = &CPU_TIME[(pc.cpu_id as usize).min(cpu::MAX_CPUS - 1)];
    if pc.current.load(Ordering::SeqCst) == pc.idle.load(Ordering::SeqCst) {
        times.idle.fetch_add(1, Ordering::Relaxed);
    } else if user {
        times.user.fetch_add(1, Ordering::Relaxed);
        cur.utime.fetch_add(1, Ordering::Relaxed);
    } else {
        times.system.fetch_add(1, Ordering::Relaxed);
        cur.stime.fetch_add(1, Ordering::Relaxed);
    }
    let q = cur.quantum.load(Ordering::Relaxed);
    if q <= 1 || pc.current.load(Ordering::SeqCst) == pc.idle.load(Ordering::SeqCst) {
        pc.need_resched.store(1, Ordering::SeqCst);
    } else {
        cur.quantum.store(q - 1, Ordering::Relaxed);
    }
}

/// Safe point on every trap exit: preempt and deliver signals.
pub fn on_trap_exit(frame: &mut TrapFrame) {
    if !is_running() || !cpu::is_initialized() {
        return;
    }
    let pc = cpu::this();
    if pc.need_resched.load(Ordering::SeqCst) != 0 && pc.preempt_count.load(Ordering::SeqCst) == 0 {
        // A thread preempted between marking itself Blocked and calling
        // schedule() (e.g. while re-checking its wait condition, possibly
        // holding the lock that condition takes) has not gone to sleep
        // yet: preemption keeps it runnable. It re-checks and blocks
        // again itself; if a waker already made it Ready, nothing changes.
        if let Some(t) = try_current() {
            let _ = t.state.compare_exchange(
                State::Blocked as u8,
                State::Running as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
        }
        schedule();
    }
    if frame.from_user() {
        crate::process::signal::deliver_pending(frame);
    }
}

/// Disable preemption on this CPU until the guard drops.
pub struct PreemptGuard;

impl PreemptGuard {
    pub fn new() -> PreemptGuard {
        if cpu::is_initialized() {
            cpu::this().preempt_count.fetch_add(1, Ordering::SeqCst);
        }
        PreemptGuard
    }
}

impl Default for PreemptGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PreemptGuard {
    fn drop(&mut self) {
        if cpu::is_initialized() {
            cpu::this().preempt_count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Next thread for CPU `me`: from its own queue, else stolen from the
/// busiest other queue.
fn pick_next(me: u32) -> Option<Arc<Thread>> {
    let runnable =
        |t: &Arc<Thread>| t.state() == State::Ready && t.on_cpu.load(Ordering::SeqCst) == 0;
    {
        let mut rq = RUN_QUEUES[me as usize].lock();
        let n = rq.len();
        for _ in 0..n {
            let t = rq.pop_front()?;
            match t.state() {
                State::Ready if !t.allowed_on(me) => {
                    drop(rq);
                    enqueue(t);
                    rq = RUN_QUEUES[me as usize].lock();
                }
                State::Ready if runnable(&t) => return Some(t),
                State::Ready => rq.push_back(t), // still switching out elsewhere
                _ => {}                          // stale entry
            }
        }
    }
    let n = cpu::cpu_count().clamp(1, cpu::MAX_CPUS as u32);
    let busiest = (0..n)
        .filter(|&c| c != me)
        .map(|c| (RUN_QUEUES[c as usize].lock().len(), c))
        .max()?;
    if busiest.0 == 0 {
        return None;
    }
    let mut rq = RUN_QUEUES[busiest.1 as usize].lock();
    let i = rq.iter().rposition(|t| runnable(t) && t.allowed_on(me))?;
    rq.remove(i)
}

/// Switch to the next runnable thread (or keep running the current one).
pub fn schedule() {
    if !is_running() {
        return;
    }
    let was_enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();

    let pc = cpu::this();
    pc.need_resched.store(0, Ordering::SeqCst);
    let cur_ptr = pc.current.load(Ordering::SeqCst) as *const Thread;
    let cur = unsafe { &*cur_ptr };
    let idle_ptr = pc.idle.load(Ordering::SeqCst) as *const Thread;
    let cur_is_idle = cur_ptr == idle_ptr;

    let me = pc.cpu_id;
    let next = pick_next(me);
    let next_ptr: *const Thread = match next {
        Some(n) => Arc::into_raw(n),
        None => {
            if cur.state() == State::Running && (cur_is_idle || cur.allowed_on(me)) {
                cur.quantum.store(cur.slice(), Ordering::Relaxed);
                if was_enabled {
                    x86_64::instructions::interrupts::enable();
                }
                return;
            }
            // Nothing runnable: go idle.
            unsafe { Arc::increment_strong_count(idle_ptr) };
            idle_ptr
        }
    };
    if next_ptr == cur_ptr {
        unsafe { Arc::decrement_strong_count(next_ptr) };
        cur.set_state(State::Running);
        if was_enabled {
            x86_64::instructions::interrupts::enable();
        }
        return;
    }

    // Requeue the outgoing thread if it is still runnable.
    if cur.state() == State::Running {
        cur.set_state(State::Ready);
        if !cur_is_idle {
            unsafe { Arc::increment_strong_count(cur_ptr) };
            let t = unsafe { Arc::from_raw(cur_ptr) };
            if cur.allowed_on(me) {
                RUN_QUEUES[me as usize].lock().push_back(t);
            } else {
                enqueue(t);
            }
        }
    }

    // A context switch is a quiescent state (RCU readers cannot sleep).
    pc.rcu_qs.fetch_add(1, Ordering::Relaxed);
    let next = unsafe { &*next_ptr };
    next.set_state(State::Running);
    next.quantum.store(next.slice(), Ordering::Relaxed);
    next.last_cpu.store(me, Ordering::Relaxed);
    next.on_cpu.store(1, Ordering::SeqCst);

    // Architectural state for the incoming thread.
    cpu::set_kernel_stack(next.kstack.top());
    let new_cr3 = next.cr3.load(Ordering::SeqCst);
    let (cur_cr3, _) = x86_64::registers::control::Cr3::read_raw();
    if cur.is_user.load(Ordering::Relaxed) {
        unsafe {
            core::arch::asm!("fxsave64 [{}]", in(reg) cur.fpu.get(), options(nostack));
        }
        cur.fs_base.store(
            x86_64::registers::model_specific::FsBase::read().as_u64(),
            Ordering::Relaxed,
        );
    }
    if cur_cr3.start_address().as_u64() != new_cr3 {
        unsafe {
            x86_64::registers::control::Cr3::write_raw(
                x86_64::structures::paging::PhysFrame::containing_address(x86_64::PhysAddr::new(
                    new_cr3,
                )),
                0,
            );
        }
    }
    if next.is_user.load(Ordering::Relaxed) {
        unsafe {
            core::arch::asm!("fxrstor64 [{}]", in(reg) next.fpu.get(), options(nostack));
        }
        x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(
            next.fs_base.load(Ordering::Relaxed),
        ));
    }

    pc.current.store(next_ptr as usize, Ordering::SeqCst);
    CONTEXT_SWITCHES.fetch_add(1, Ordering::Relaxed);
    // `prev` reference held by the per-CPU slot moves to PREV for cleanup
    // after the switch completes.
    PREV[pc.cpu_id as usize].store(cur_ptr as usize, Ordering::SeqCst);
    unsafe {
        sched_switch(cur.rsp.get(), *next.rsp.get(), cur.on_cpu.as_ptr());
    }
    // Back on `cur`, possibly on a different CPU.
    finish_switch();
    if was_enabled {
        x86_64::instructions::interrupts::enable();
    }
}

static PREV: [AtomicUsize; cpu::MAX_CPUS] = [const { AtomicUsize::new(0) }; cpu::MAX_CPUS];

/// Runs on the incoming thread right after a switch: drop the per-CPU
/// reference to the previous thread and reap dead threads.
fn finish_switch() {
    let pc = cpu::this();
    let prev = PREV[pc.cpu_id as usize].swap(0, Ordering::SeqCst) as *const Thread;
    if !prev.is_null() {
        unsafe { Arc::decrement_strong_count(prev) };
    }
    // Free finished threads outside the lock: dropping a kernel stack
    // unmaps it, which waits for the other CPUs (TLB shootdown).
    let reaped: Vec<Arc<Thread>> = {
        let mut dead = DEAD.lock();
        let mut out = Vec::new();
        let mut i = 0;
        while i < dead.len() {
            if dead[i].on_cpu.load(Ordering::SeqCst) == 0 {
                out.push(dead.swap_remove(i));
            } else {
                i += 1;
            }
        }
        out
    };
    // Dropping a thread can drop its process and address space, which take
    // locks (the process table, ...) that a thread preempted on this CPU
    // may hold: never here with interrupts off. The worker does it.
    if !reaped.is_empty() {
        if WORKER_STARTED.load(Ordering::SeqCst) {
            defer(move || drop(reaped));
        } else {
            // Early boot: nothing else can hold those locks yet.
            drop(reaped);
        }
    }
}

static WORKER_STARTED: AtomicBool = AtomicBool::new(false);

impl Drop for Thread {
    fn drop(&mut self) {
        // The last thread of a process may be dropped anywhere, including
        // the scheduler and interrupt handlers (stale run-queue and timer
        // entries). Tearing the process down (process table, open files,
        // address space) needs locks that preempted code on this CPU may
        // hold, so the worker does it.
        if let Some(p) = self.process.get_mut().take()
            && WORKER_STARTED.load(Ordering::SeqCst)
        {
            defer(move || drop(p));
        }
    }
}

/// Snapshot of all live threads: (tid, name, state, is_user).
/// (tid, wait channel) of every live thread (state dumps; no locks but ALL).
pub fn wait_channels() -> Vec<(Tid, u64)> {
    irqsave(|| {
        ALL.try_lock()
            .map(|all| {
                all.iter()
                    .filter_map(|w| w.upgrade())
                    .map(|t| (t.tid, t.wchan.load(Ordering::Relaxed)))
                    .collect()
            })
            .unwrap_or_default()
    })
}

pub fn thread_list() -> Vec<(Tid, String, State, bool)> {
    irqsave(|| {
        let mut all = ALL.lock();
        all.retain(|w| w.strong_count() > 0);
        all.iter()
            .filter_map(|w| w.upgrade())
            .filter(|t| t.name.lock().as_str() != "idle")
            .map(|t| {
                (
                    t.tid,
                    t.name.lock().clone(),
                    t.state(),
                    t.is_user.load(Ordering::Relaxed),
                )
            })
            .collect()
    })
}

/// Like [`thread_list`] with try-locks only (state dumps).
pub fn try_thread_list() -> Vec<(Tid, String, State, bool)> {
    irqsave(|| {
        let Some(all) = ALL.try_lock() else {
            return Vec::new();
        };
        all.iter()
            .filter_map(|w| w.upgrade())
            .map(|t| {
                let name = t
                    .name
                    .try_lock()
                    .map_or(String::from("<locked>"), |n| n.clone());
                (t.tid, name, t.state(), t.is_user.load(Ordering::Relaxed))
            })
            .filter(|(_, n, ..)| n != "idle")
            .collect()
    })
}
