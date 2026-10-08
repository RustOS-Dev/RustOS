//! ITIMER_REAL (setitimer/alarm): SIGALRM from the scheduler's timer heap.

use crate::process::signal;
use crate::sched::{self, TimerTarget};
use crate::sync::Mutex;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

pub struct RealTimer {
    pid: u32,
    /// Next expiry (monotonic ns), 0 = disarmed.
    deadline: AtomicU64,
    interval: AtomicU64,
    /// Bumped by every set(): heap entries of an earlier arming are stale.
    generation: AtomicU64,
}

/// One heap entry: the timer as armed by one set() (or periodic re-arm).
/// Without the generation, an entry left from an earlier arming that fires
/// after a re-arm to an earlier deadline would look current and start a
/// second periodic chain; X servers re-arm constantly (smart scheduler).
struct Arm {
    timer: Arc<RealTimer>,
    generation: u64,
}

impl TimerTarget for Arm {
    fn fire(self: Arc<Self>, now: u64) {
        let t = &self.timer;
        let d = t.deadline.load(Ordering::SeqCst);
        if t.generation.load(Ordering::SeqCst) != self.generation || d == 0 || now < d {
            return; // stale entry
        }
        let iv = t.interval.load(Ordering::SeqCst);
        if iv > 0 {
            let next = d + iv * ((now - d) / iv + 1);
            t.deadline.store(next, Ordering::SeqCst);
            sched::add_timer(next, self.clone());
        } else {
            t.deadline.store(0, Ordering::SeqCst);
        }
        let (timer, generation) = (t.clone(), self.generation);
        sched::defer(move || {
            // Disarmed or re-armed since: no signal from the old arming.
            if timer.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            if let Some(p) = crate::process::find(timer.pid) {
                signal::send(&p, signal::SIGALRM);
            }
        });
    }
}

static TIMERS: Mutex<BTreeMap<u32, Arc<RealTimer>>> = Mutex::new(BTreeMap::new());

fn timer(pid: u32) -> Arc<RealTimer> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        TIMERS
            .lock()
            .entry(pid)
            .or_insert_with(|| {
                Arc::new(RealTimer {
                    pid,
                    deadline: AtomicU64::new(0),
                    interval: AtomicU64::new(0),
                    generation: AtomicU64::new(0),
                })
            })
            .clone()
    })
}

/// Current (remaining ns, interval ns).
pub fn get(pid: u32) -> (u64, u64) {
    let t = timer(pid);
    let d = t.deadline.load(Ordering::SeqCst);
    let rem = if d == 0 {
        0
    } else {
        d.saturating_sub(crate::time::nanos()).max(1)
    };
    (rem, t.interval.load(Ordering::SeqCst))
}

/// Arm (value > 0) or disarm the timer; returns the previous setting.
pub fn set(pid: u32, value_ns: u64, interval_ns: u64) -> (u64, u64) {
    let old = get(pid);
    let t = timer(pid);
    let generation = t.generation.fetch_add(1, Ordering::SeqCst) + 1;
    t.interval.store(interval_ns, Ordering::SeqCst);
    if value_ns == 0 {
        t.deadline.store(0, Ordering::SeqCst);
    } else {
        let d = crate::time::nanos() + value_ns;
        t.deadline.store(d, Ordering::SeqCst);
        sched::add_timer(
            d,
            Arc::new(Arm {
                timer: t,
                generation,
            }),
        );
    }
    old
}

/// Forget a process's timer when it exits.
pub fn remove(pid: u32) {
    let t = x86_64::instructions::interrupts::without_interrupts(|| TIMERS.lock().remove(&pid));
    if let Some(t) = t {
        t.generation.fetch_add(1, Ordering::SeqCst);
        t.deadline.store(0, Ordering::SeqCst);
    }
}
