//! CPU-time accounting and the load average.
//!
//! Every timer tick (`time::HZ` per second on each CPU) is charged to one
//! class on that CPU — user, nice, system, idle, irq or softirq — and,
//! for user and system ticks, to the interrupted thread (user mode or
//! kernel mode, from the privilege level of the interrupted code).
//! Interrupt handlers and timer callbacks run with interrupts disabled, so
//! the tick never samples them; instead their time is measured with the
//! TSC and paid for by charging whole ticks to irq/softirq once a tick's
//! worth has accumulated (like Linux's IRQ_TIME_ACCOUNTING). The classes
//! of a CPU therefore always add up to its ticks.
//!
//! A process's CPU time is the sum of its live threads' plus what its
//! exited threads left behind; reaped children add theirs (including
//! their own reaped children) to the parent's `cutime`/`cstime`.
//!
//! The load average follows Linux's `calc_load`: every 5 s CPU 0 samples
//! the runnable threads plus user threads in uninterruptible waits and
//! folds them into three fixed-point exponential averages.

use super::{RUN_QUEUES, Thread};
use crate::arch::x86_64::cpu;
use crate::time::HZ;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Clock ticks per second in `/proc` and `times()` (Linux `USER_HZ`).
pub const USER_HZ: u64 = 100;

/// Kernel ticks to `USER_HZ` clock ticks.
pub fn to_clock_t(ticks: u64) -> u64 {
    ticks * USER_HZ / HZ
}

/// Kernel ticks to nanoseconds.
pub fn to_ns(ticks: u64) -> u64 {
    ticks * (1_000_000_000 / HZ)
}

/// Per-CPU time classes, in the order of a `/proc/stat` `cpu` line.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Class {
    User = 0,
    Nice = 1,
    System = 2,
    Idle = 3,
    Iowait = 4,
    Irq = 5,
    Softirq = 6,
    Steal = 7,
    Guest = 8,
    GuestNice = 9,
}

pub const NCLASS: usize = 10;

static CPU_STAT: [[AtomicU64; NCLASS]; cpu::MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; NCLASS] }; cpu::MAX_CPUS];

/// TSC cycles spent in interrupt handlers / timer callbacks per CPU, and
/// how much of that has been charged as ticks. Only the owning CPU writes
/// them (with interrupts off).
static IRQ_TSC: [AtomicU64; cpu::MAX_CPUS] = [const { AtomicU64::new(0) }; cpu::MAX_CPUS];
static IRQ_CHARGED: [AtomicU64; cpu::MAX_CPUS] = [const { AtomicU64::new(0) }; cpu::MAX_CPUS];
static SOFTIRQ_TSC: [AtomicU64; cpu::MAX_CPUS] = [const { AtomicU64::new(0) }; cpu::MAX_CPUS];
static SOFTIRQ_CHARGED: [AtomicU64; cpu::MAX_CPUS] = [const { AtomicU64::new(0) }; cpu::MAX_CPUS];

/// User threads sleeping in uninterruptible waits (Linux's `D` state).
static NR_UNINTERRUPTIBLE: AtomicUsize = AtomicUsize::new(0);

fn cpu_index() -> usize {
    if cpu::is_initialized() {
        (cpu::this().cpu_id as usize).min(cpu::MAX_CPUS - 1)
    } else {
        0
    }
}

/// Add TSC cycles spent in a hardware interrupt handler on this CPU.
pub fn add_irq_tsc(cycles: u64) {
    let c = cpu_index();
    IRQ_TSC[c].store(
        IRQ_TSC[c].load(Ordering::Relaxed).wrapping_add(cycles),
        Ordering::Relaxed,
    );
}

/// Move TSC cycles of the timer interrupt from irq to softirq time (timer
/// callbacks; they were measured as part of the interrupt).
pub fn move_irq_to_softirq(cycles: u64) {
    let c = cpu_index();
    IRQ_TSC[c].store(
        IRQ_TSC[c].load(Ordering::Relaxed).wrapping_sub(cycles),
        Ordering::Relaxed,
    );
    SOFTIRQ_TSC[c].store(
        SOFTIRQ_TSC[c].load(Ordering::Relaxed).wrapping_add(cycles),
        Ordering::Relaxed,
    );
}

/// Charge one tick to irq/softirq if a tick's worth of handler time has
/// accumulated on CPU `c`.
fn pending_irq_class(c: usize) -> Option<Class> {
    let per_tick = crate::time::tsc_hz() / HZ;
    if per_tick == 0 {
        return None;
    }
    for (tsc, charged, class) in [
        (&IRQ_TSC, &IRQ_CHARGED, Class::Irq),
        (&SOFTIRQ_TSC, &SOFTIRQ_CHARGED, Class::Softirq),
    ] {
        let owed = tsc[c]
            .load(Ordering::Relaxed)
            .wrapping_sub(charged[c].load(Ordering::Relaxed));
        // A negative balance (a wrapping_sub ordering artefact) is not owed.
        if owed >= per_tick && owed < u64::MAX / 2 {
            charged[c].store(
                charged[c].load(Ordering::Relaxed).wrapping_add(per_tick),
                Ordering::Relaxed,
            );
            return Some(class);
        }
    }
    None
}

/// Account one timer tick on this CPU. `from_user` tells whether the tick
/// interrupted ring 3. Runs in the timer interrupt.
pub fn account_tick(from_user: bool) {
    if !cpu::is_initialized() {
        return;
    }
    let pc = cpu::this();
    let c = (pc.cpu_id as usize).min(cpu::MAX_CPUS - 1);
    let cur = pc.current.load(Ordering::SeqCst);
    let idle = pc.idle.load(Ordering::SeqCst);
    let class = if let Some(k) = pending_irq_class(c) {
        k
    } else if cur == 0 || cur == idle {
        Class::Idle
    } else {
        // The per-CPU slot holds a reference: the thread is alive.
        let t = unsafe { &*(cur as *const Thread) };
        if from_user {
            t.utime.fetch_add(1, Ordering::Relaxed);
            if t.nice.load(Ordering::Relaxed) > 0 {
                Class::Nice
            } else {
                Class::User
            }
        } else {
            t.stime.fetch_add(1, Ordering::Relaxed);
            Class::System
        }
    };
    CPU_STAT[c][class as usize].fetch_add(1, Ordering::Relaxed);
    if c == 0 {
        load_tick();
    }
}

/// Ticks per class of CPU `c`.
pub fn cpu_stat(c: usize) -> [u64; NCLASS] {
    let mut out = [0; NCLASS];
    if c < cpu::MAX_CPUS {
        for (o, v) in out.iter_mut().zip(CPU_STAT[c].iter()) {
            *o = v.load(Ordering::Relaxed);
        }
    }
    out
}

/// Idle ticks summed over all CPUs.
pub fn total_idle() -> u64 {
    (0..cpu::MAX_CPUS)
        .map(|c| CPU_STAT[c][Class::Idle as usize].load(Ordering::Relaxed))
        .sum()
}

// ---------------------------------------------------------------------------
// Threads and processes
// ---------------------------------------------------------------------------

impl Thread {
    /// (user, system) ticks of this thread.
    pub fn cpu_times(&self) -> (u64, u64) {
        (
            self.utime.load(Ordering::Relaxed),
            self.stime.load(Ordering::Relaxed),
        )
    }

    /// Hand this thread's time to its process's exited-threads total
    /// (once): called when the thread leaves its process.
    pub fn fold_cpu_times(&self, into: &crate::process::Process) {
        if !self.acct_folded.swap(true, Ordering::SeqCst) {
            let (u, s) = self.cpu_times();
            into.dead_utime.fetch_add(u, Ordering::Relaxed);
            into.dead_stime.fetch_add(s, Ordering::Relaxed);
        }
    }

    pub fn acct_folded(&self) -> bool {
        self.acct_folded.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// Load average (Linux kernel/sched/loadavg.c)
// ---------------------------------------------------------------------------

const FSHIFT: u32 = 11;
const FIXED_1: u64 = 1 << FSHIFT;
/// 1/exp(5s/1min), 1/exp(5s/5min), 1/exp(5s/15min) in fixed point.
const EXP: [u64; 3] = [1884, 2014, 2037];
/// Sample every 5 s (plus one tick, as Linux does, to avoid aliasing with
/// periodic 5 s activity).
const LOAD_FREQ: u64 = 5 * HZ + 1;

static AVENRUN: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];
static LOAD_TICKS: AtomicU64 = AtomicU64::new(0);

fn calc_load(load: u64, exp: u64, active: u64) -> u64 {
    let mut newload = load * exp + active * (FIXED_1 - exp);
    if active >= load {
        newload += FIXED_1 - 1;
    }
    newload / FIXED_1
}

fn load_tick() {
    if LOAD_TICKS.fetch_add(1, Ordering::Relaxed) + 1 < LOAD_FREQ {
        return;
    }
    LOAD_TICKS.store(0, Ordering::Relaxed);
    let active = (nr_running() + nr_uninterruptible()) as u64 * FIXED_1;
    for (a, e) in AVENRUN.iter().zip(EXP) {
        a.store(
            calc_load(a.load(Ordering::Relaxed), e, active),
            Ordering::Relaxed,
        );
    }
}

/// Load averages over 1, 5 and 15 minutes, fixed point with `FSHIFT`
/// fraction bits.
pub fn loadavg_raw() -> [u64; 3] {
    [
        AVENRUN[0].load(Ordering::Relaxed),
        AVENRUN[1].load(Ordering::Relaxed),
        AVENRUN[2].load(Ordering::Relaxed),
    ]
}

/// Format a fixed-point load as "N.NN" (Linux LOAD_INT/LOAD_FRAC).
pub fn fmt_load(v: u64) -> alloc::string::String {
    let v = v + FIXED_1 / 200;
    alloc::format!(
        "{}.{:02}",
        v >> FSHIFT,
        ((v & (FIXED_1 - 1)) * 100) >> FSHIFT
    )
}

/// Load averages scaled for `sysinfo(2)` (`SI_LOAD_SHIFT` = 16).
pub fn loadavg_sysinfo() -> [u64; 3] {
    loadavg_raw().map(|v| v << (16 - FSHIFT))
}

/// Threads running or waiting to run, idle threads excluded.
pub fn nr_running() -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let n = cpu::cpu_count().clamp(1, cpu::MAX_CPUS as u32);
        let mut count = 0;
        for c in 0..n {
            count += RUN_QUEUES[c as usize]
                .lock()
                .iter()
                .filter(|t| t.state() == super::State::Ready)
                .count();
            if let Some(pc) = cpu::cpu(c) {
                let cur = pc.current.load(Ordering::SeqCst);
                if cur != 0 && cur != pc.idle.load(Ordering::SeqCst) {
                    count += 1;
                }
            }
        }
        count
    })
}

/// User threads sleeping uninterruptibly (disk I/O, sleeping mutexes).
pub fn nr_uninterruptible() -> usize {
    NR_UNINTERRUPTIBLE.load(Ordering::Relaxed)
}

/// Marks an uninterruptible sleep of a user thread while alive.
pub(super) struct UninterruptibleSleep(bool);

impl UninterruptibleSleep {
    pub(super) fn new(counted: bool) -> Self {
        if counted {
            NR_UNINTERRUPTIBLE.fetch_add(1, Ordering::Relaxed);
        }
        UninterruptibleSleep(counted)
    }
}

impl Drop for UninterruptibleSleep {
    fn drop(&mut self) {
        if self.0 {
            NR_UNINTERRUPTIBLE.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

#[test_case]
fn test_calc_load() {
    // A constant load of 1 converges towards 1.00.
    let mut l = 0;
    for _ in 0..1000 {
        l = calc_load(l, EXP[0], FIXED_1);
    }
    assert_eq!(fmt_load(l), "1.00");
    assert_eq!(fmt_load(0), "0.00");
    assert_eq!(to_clock_t(HZ), USER_HZ);
}
