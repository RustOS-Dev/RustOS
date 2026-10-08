//! Scheduling, time and timer services for the LinuxKPI C glue
//! (src/linuxkpi/c/sched.c, time.c), mapped onto `crate::sched`.
//!
//! Linux's sleep pattern (set the task state, check the condition, call
//! `schedule()`) maps onto RustOS's `prepare_block`/`schedule` with the
//! `wakeup_pending` flag: a `wake_up_process()` that lands before the
//! sleeper reaches `schedule()` makes that `schedule()` return at once.
//!
//! Linux timers and tasklets run in a "softirq" kernel thread, not in the
//! RustOS timer interrupt, so their callbacks may take ordinary locks.

use crate::sched::{self, TimerTarget, WaitQueue};
use crate::sync::Mutex;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use core::ffi::{CStr, c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::instructions::interrupts::without_interrupts;

/// `cond_resched()`: give up the CPU if the scheduler asked for it.
/// Returns 1 if this thread was rescheduled.
#[unsafe(no_mangle)]
pub extern "C" fn __cond_resched() -> c_int {
    let pc = crate::arch::x86_64::cpu::this();
    if sched::is_running()
        && pc.need_resched.load(Ordering::SeqCst) != 0
        && pc.preempt_count.load(Ordering::SeqCst) == 0
    {
        sched::yield_now();
        1
    } else {
        0
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_nanos() -> u64 {
    crate::time::nanos()
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_delay_ns(ns: u64) {
    let end = crate::time::nanos() + ns;
    while crate::time::nanos() < end {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_thread_id() -> u64 {
    sched::try_current().map(|t| t.tid).unwrap_or(0)
}

/// Before the scheduler runs (and for code with no thread) everything
/// shares this slot.
static EARLY_TASK: core::sync::atomic::AtomicPtr<c_void> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_task_slot() -> *mut *mut c_void {
    match sched::try_current() {
        // The Arc keeps the thread alive while it runs, so the slot is
        // valid for the caller (always the thread itself).
        Some(t) => t.linux_task.as_ptr(),
        None => EARLY_TASK.as_ptr(),
    }
}

/// Sleep until woken (`rustos_kpi_wake`) or until `deadline_ns` (0: no
/// timeout). Returns at once if a wake-up already arrived. Holds a strong
/// reference to the thread while it is blocked: RustOS's thread list is
/// weak, and nothing else owns a thread sleeping in Linux code.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_sleep(deadline_ns: u64, site: *const c_void) {
    if !sched::is_running() {
        return;
    }
    let me = sched::current();
    me.wchan.store(site as u64, Ordering::Relaxed);
    if !sched::prepare_block() {
        return;
    }
    if deadline_ns != 0 {
        without_interrupts(|| sched::arm_timeout(&me, deadline_ns));
    }
    sched::schedule();
    drop(me);
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_wake(tid: u64) {
    if let Some(t) = sched::find_thread(tid) {
        sched::wake(&t);
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_yield() {
    sched::yield_now();
}

struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}

/// Strong references to threads LinuxKPI spawned. RustOS keeps only weak
/// references to threads in general (owners such as wait queues hold the
/// strong ones), but a thread sleeping in Linux's `schedule()` has no
/// other owner: without this its stack would be freed and reused.
static THREADS: Mutex<BTreeMap<u64, Arc<sched::Thread>>> = Mutex::new(BTreeMap::new());

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_spawn(
    f: extern "C" fn(*mut c_void),
    arg: *mut c_void,
    name: *const c_char,
) -> u64 {
    let name = if name.is_null() {
        String::from("linux")
    } else {
        String::from(unsafe { CStr::from_ptr(name) }.to_string_lossy())
    };
    let arg = SendPtr(arg);
    // Hold the registry lock across spawn so the thread cannot finish and
    // unregister before it is registered.
    without_interrupts(|| {
        let mut threads = THREADS.lock();
        let t = sched::spawn(&name, move || {
            let arg = arg;
            f(arg.0);
            if let Some(me) = sched::try_current() {
                without_interrupts(|| THREADS.lock().remove(&me.tid));
            }
        });
        let tid = t.tid;
        threads.insert(tid, t);
        tid
    })
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_cpu_id() -> u32 {
    crate::arch::x86_64::cpu::this().cpu_id
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_cpu_count() -> u32 {
    crate::arch::x86_64::cpu::cpu_count()
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_set_cpu_offset(cpu: u32, off: u64) {
    if let Some(pc) = crate::arch::x86_64::cpu::cpu(cpu) {
        pc.linux_percpu_offset.store(off, Ordering::SeqCst);
    }
}

// --------------------------------------------------------------- timers

/// `fn(arg, handle)`: the handle lets the C side ignore a firing that was
/// cancelled or superseded after it was queued.
type Callback = extern "C" fn(*mut c_void, u64);

struct KpiTimer {
    id: u64,
    deadline_ns: u64,
    f: Callback,
    arg: usize,
    cancelled: AtomicBool,
}

impl TimerTarget for KpiTimer {
    fn fire(self: Arc<Self>, _now_ns: u64) {
        // Interrupt context: hand the callback to the softirq thread.
        if !self.cancelled.load(Ordering::SeqCst) {
            SOFTIRQ_QUEUE.lock().push_back(self);
            raise();
        }
    }
}

static NEXT_TIMER: AtomicU64 = AtomicU64::new(1);
static TIMERS: Mutex<BTreeMap<u64, Arc<KpiTimer>>> = Mutex::new(BTreeMap::new());
static SOFTIRQ_QUEUE: Mutex<VecDeque<Arc<KpiTimer>>> = Mutex::new(VecDeque::new());
static SOFTIRQ_PENDING: AtomicBool = AtomicBool::new(false);
static SOFTIRQ_WQ: WaitQueue = WaitQueue::new();

/// Run `f(arg, handle)` in the softirq thread once `deadline_ns` has passed.
/// Returns a handle for `rustos_kpi_timer_cancel`.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_timer_start(deadline_ns: u64, f: Callback, arg: *mut c_void) -> u64 {
    let id = NEXT_TIMER.fetch_add(1, Ordering::Relaxed);
    let t = Arc::new(KpiTimer {
        id,
        deadline_ns,
        f,
        arg: arg as usize,
        cancelled: AtomicBool::new(false),
    });
    without_interrupts(|| TIMERS.lock().insert(id, t.clone()));
    sched::add_timer(deadline_ns, t);
    id
}

/// Cancel a timer. Returns 1 if its callback had not started yet.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_timer_cancel(handle: u64) -> c_int {
    match without_interrupts(|| TIMERS.lock().remove(&handle)) {
        Some(t) => {
            t.cancelled.store(true, Ordering::SeqCst);
            1
        }
        None => 0,
    }
}

/// LinuxKPI deferred-work state for the sysrq dump: timers waiting (and
/// how many are overdue, which would mean a lost RustOS timer), callbacks
/// queued for the softirq thread, and work items queued per workqueue.
pub fn dump_state() {
    let now = crate::time::nanos();
    let (n, overdue, worst) = match TIMERS.try_lock() {
        Some(t) => {
            let late: alloc::vec::Vec<u64> = t
                .values()
                .filter(|k| k.deadline_ns + 100_000_000 < now)
                .map(|k| (now - k.deadline_ns) / 1_000_000)
                .collect();
            (t.len(), late.len(), late.iter().copied().max().unwrap_or(0))
        }
        None => (usize::MAX, 0, 0),
    };
    let queued = SOFTIRQ_QUEUE.try_lock().map_or(usize::MAX, |q| q.len());
    crate::println!(
        "[sysrq] linuxkpi timers {} (overdue {}, worst {} ms), softirq queue {}, pending {}",
        n,
        overdue,
        worst,
        queued,
        SOFTIRQ_PENDING.load(Ordering::SeqCst)
    );
    unsafe { kpi_wq_dump() };
}

fn raise() {
    SOFTIRQ_PENDING.store(true, Ordering::SeqCst);
    SOFTIRQ_WQ.wake_all();
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_softirq_raise() {
    raise();
}

unsafe extern "C" {
    /// src/linuxkpi/c/softirq.c: tasklets, NAPI and other deferred work.
    fn kpi_softirq_run();
    /// src/linuxkpi/c/time.c: queued work per workqueue, for the sysrq dump.
    fn kpi_wq_dump();
}

fn softirq_thread() {
    loop {
        SOFTIRQ_WQ.wait_until(|| SOFTIRQ_PENDING.load(Ordering::SeqCst));
        SOFTIRQ_PENDING.store(false, Ordering::SeqCst);
        while let Some(t) = without_interrupts(|| SOFTIRQ_QUEUE.lock().pop_front()) {
            // Only run timers that were not cancelled meanwhile.
            if without_interrupts(|| TIMERS.lock().remove(&t.id)).is_some()
                && !t.cancelled.load(Ordering::SeqCst)
            {
                (t.f)(t.arg as *mut c_void, t.id);
            }
        }
        unsafe { kpi_softirq_run() };
    }
}

pub fn start_softirq() {
    sched::spawn("linux-softirq", softirq_thread);
}

// ------------------------------------------------------------------ RCU

/// Wait for an RCU grace period: until every other CPU has passed a
/// quiescent state (`PerCpu::rcu_qs`: a context switch, or a timer tick
/// that found preemption enabled). LinuxKPI's readers disable preemption
/// (src/linuxkpi/c/rcu.c), so a reader that started before this call has
/// finished once its CPU's counter moves. The calling CPU is quiescent
/// already: the caller may sleep, so it is not inside a reader.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_rcu_synchronize() {
    use crate::arch::x86_64::cpu;
    let me = cpu::this().cpu_id;
    let n = cpu::cpu_count();
    let mut seen = [None::<u64>; cpu::MAX_CPUS];
    for id in 0..n {
        if id != me {
            seen[id as usize] = cpu::cpu(id).map(|c| c.rcu_qs.load(Ordering::SeqCst));
        }
    }
    loop {
        let pending = (0..n).any(|id| {
            seen[id as usize]
                .is_some_and(|s| cpu::cpu(id).is_some_and(|c| c.rcu_qs.load(Ordering::SeqCst) == s))
        });
        if !pending {
            return;
        }
        if sched::is_running() {
            // One tick (4 ms at 250 Hz) moves every busy CPU's counter.
            sched::sleep_until(crate::time::nanos() + 1_000_000);
        } else {
            core::hint::spin_loop();
        }
    }
}

/// Wall-clock time in nanoseconds since the Unix epoch.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_realtime_ns() -> u64 {
    crate::time::realtime_nanos()
}

/// The calling process's user id (0 in kernel threads).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_current_uid() -> u32 {
    crate::process::current().map_or(0, |p| p.uid.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------- kernel FPU

/// FXSAVE area of the user thread whose registers a CPU's kernel FPU
/// section displaced.
#[repr(C, align(16))]
struct FxArea([u8; 512]);

static FPU_AREAS: [crate::sync::Mutex<FxArea>; crate::arch::x86_64::cpu::MAX_CPUS] =
    [const { crate::sync::Mutex::new(FxArea([0; 512])) }; crate::arch::x86_64::cpu::MAX_CPUS];
/// Per CPU: 0 idle, 1 in a kernel FPU section, 2 in one that saved user state.
static FPU_STATE: [core::sync::atomic::AtomicU8; crate::arch::x86_64::cpu::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU8::new(0) }; crate::arch::x86_64::cpu::MAX_CPUS];

/// kernel_fpu_begin_mask(): SSE (and x87 with KFPU_387) for code built
/// with FPU flags (amdgpu's display core). RustOS's kernel otherwise never
/// touches the FPU, whose registers hold the current user thread's state
/// (saved only at context switch): save them, and keep this CPU's thread
/// from being switched out until kernel_fpu_end().
#[unsafe(no_mangle)]
extern "C" fn kernel_fpu_begin_mask(mask: u32) {
    const KFPU_387: u32 = 1;
    core::mem::forget(sched::PreemptGuard::new());
    let id = crate::arch::x86_64::cpu::this().cpu_id as usize;
    let user = sched::try_current().is_some_and(|t| t.is_user.load(Ordering::Relaxed));
    let prev = FPU_STATE[id].swap(if user { 2 } else { 1 }, Ordering::SeqCst);
    debug_assert_eq!(prev, 0, "nested kernel_fpu_begin");
    if user {
        let area = FPU_AREAS[id].lock();
        unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) area.0.as_ptr(), options(nostack)) };
    }
    unsafe {
        if mask & KFPU_387 != 0 {
            core::arch::asm!("fninit", options(nostack, nomem));
        }
        let mxcsr: u32 = 0x1f80;
        core::arch::asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack, readonly));
    }
}

#[unsafe(no_mangle)]
extern "C" fn kernel_fpu_end() {
    let id = crate::arch::x86_64::cpu::this().cpu_id as usize;
    if FPU_STATE[id].swap(0, Ordering::SeqCst) == 2 {
        let area = FPU_AREAS[id].lock();
        unsafe { core::arch::asm!("fxrstor64 [{}]", in(reg) area.0.as_ptr(), options(nostack)) };
    }
    drop(sched::PreemptGuard);
}

/// The FPU may be used here: not in interrupt context, not nested.
#[unsafe(no_mangle)]
extern "C" fn irq_fpu_usable() -> bool {
    let c = crate::arch::x86_64::cpu::this();
    c.preempt_count.load(Ordering::SeqCst) < crate::arch::x86_64::cpu::HARDIRQ_OFFSET
        && FPU_STATE[c.cpu_id as usize].load(Ordering::SeqCst) == 0
}
