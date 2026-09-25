//! Preemptive scheduler.
//!
//! Every thread (kernel or user) owns a kernel stack. A context switch saves
//! the callee-saved registers on the outgoing thread's stack and resumes the
//! incoming one; everything else (user registers, interrupted kernel state)
//! is already on the stacks as trap frames. The timer tick requests a
//! reschedule; the switch happens at the next trap exit or explicit
//! [`schedule`] call.
//!
//! One global run queue serves all CPUs. A thread's `on_cpu` flag stays set
//! until its context has been fully saved, so another CPU never resumes a
//! thread whose registers are still being written.

pub mod wait;

use crate::arch::x86_64::{cpu, idt::TrapFrame};
use crate::mm::KernelStack;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

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
    wake_at: AtomicU64,
    /// Pending wakeup that raced with blocking.
    wakeup_pending: AtomicBool,
    quantum: AtomicU8,
    /// Set by `kill` to make blocking syscalls return early.
    pub interrupted: AtomicBool,
    /// Absolute deadline of an interrupted nanosleep (for restart).
    pub restart_deadline: AtomicU64,
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

    fn set_state(&self, s: State) {
        self.state.store(s as u8, Ordering::SeqCst);
    }

    pub fn kstack_top(&self) -> u64 {
        self.kstack.top()
    }
}

static NEXT_TID: AtomicU64 = AtomicU64::new(1);
static RUN_QUEUE: Mutex<VecDeque<Arc<Thread>>> = Mutex::new(VecDeque::new());
static SLEEPERS: Mutex<Vec<Arc<Thread>>> = Mutex::new(Vec::new());
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
    let sp = top - 8 * (frame.len() as u64 + 1); // keep 16-byte alignment for call
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
        wake_at: AtomicU64::new(0),
        wakeup_pending: AtomicBool::new(false),
        quantum: AtomicU8::new(QUANTUM_TICKS as u8),
        interrupted: AtomicBool::new(false),
        restart_deadline: AtomicU64::new(0),
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
    let p = cpu::this().current.load(Ordering::SeqCst) as *const Thread;
    assert!(!p.is_null(), "no current thread");
    unsafe {
        Arc::increment_strong_count(p);
        Arc::from_raw(p)
    }
}

/// Current thread if the scheduler is running on this CPU.
pub fn try_current() -> Option<Arc<Thread>> {
    if !cpu::is_initialized() {
        return None;
    }
    let p = cpu::this().current.load(Ordering::SeqCst) as *const Thread;
    if p.is_null() {
        return None;
    }
    unsafe {
        Arc::increment_strong_count(p);
        Some(Arc::from_raw(p))
    }
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

pub fn make_ready(t: Arc<Thread>) {
    irqsave(|| {
        t.set_state(State::Ready);
        RUN_QUEUE.lock().push_back(t);
    });
}

/// Wake a blocked thread (no-op if it is not blocked).
pub fn wake(t: &Arc<Thread>) {
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
            t.wake_at.store(0, Ordering::SeqCst);
            RUN_QUEUE.lock().push_back(t.clone());
        } else {
            t.wakeup_pending.store(true, Ordering::SeqCst);
        }
    });
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
    t.wake_at.store(deadline_ns, Ordering::SeqCst);
    SLEEPERS.lock().push(t.clone());
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
            t.wake_at.store(deadline_ns, Ordering::SeqCst);
            t.set_state(State::Blocked);
            SLEEPERS.lock().push(t.clone());
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
    irqsave(|| {
        t.set_state(State::Dead);
        DEAD.lock().push(t.clone());
    });
    drop(t);
    schedule();
    unreachable!("dead thread rescheduled");
}

/// Called from the timer interrupt.
pub fn timer_tick() {
    if !is_running() {
        return;
    }
    let now = crate::time::nanos();
    {
        let mut s = SLEEPERS.lock();
        let mut i = 0;
        while i < s.len() {
            let at = s[i].wake_at.load(Ordering::SeqCst);
            if at == 0 || s[i].state() != State::Blocked {
                // Already woken by someone else.
                s.swap_remove(i);
            } else if at <= now {
                let t = s.swap_remove(i);
                wake(&t);
            } else {
                i += 1;
            }
        }
    }
    let pc = cpu::this();
    let cur = pc.current.load(Ordering::SeqCst) as *const Thread;
    if cur.is_null() {
        return;
    }
    let cur = unsafe { &*cur };
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

fn pick_next() -> Option<Arc<Thread>> {
    let mut rq = RUN_QUEUE.lock();
    let n = rq.len();
    for _ in 0..n {
        let t = rq.pop_front()?;
        match t.state() {
            State::Ready if t.on_cpu.load(Ordering::SeqCst) == 0 => return Some(t),
            State::Ready => rq.push_back(t), // still switching out elsewhere
            _ => {}                          // stale entry
        }
    }
    None
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

    let next = pick_next();
    let next_ptr: *const Thread = match next {
        Some(n) => Arc::into_raw(n),
        None => {
            if cur.state() == State::Running {
                cur.quantum.store(QUANTUM_TICKS as u8, Ordering::Relaxed);
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
            RUN_QUEUE
                .lock()
                .push_back(unsafe { Arc::from_raw(cur_ptr) });
        }
    }

    let next = unsafe { &*next_ptr };
    next.set_state(State::Running);
    next.quantum.store(QUANTUM_TICKS as u8, Ordering::Relaxed);
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
    let mut dead = DEAD.lock();
    dead.retain(|t| t.on_cpu.load(Ordering::SeqCst) != 0);
}

/// Snapshot of all live threads: (tid, name, state, is_user).
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
