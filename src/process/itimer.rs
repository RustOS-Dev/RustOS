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
}

impl TimerTarget for RealTimer {
    fn fire(self: Arc<Self>, now: u64) {
        let d = self.deadline.load(Ordering::SeqCst);
        if d == 0 || now < d {
            return; // disarmed or re-armed later: a stale entry
        }
        let iv = self.interval.load(Ordering::SeqCst);
        if iv > 0 {
            let next = d + iv * ((now - d) / iv + 1);
            self.deadline.store(next, Ordering::SeqCst);
            sched::add_timer(next, self.clone());
        } else {
            self.deadline.store(0, Ordering::SeqCst);
        }
        let pid = self.pid;
        sched::defer(move || {
            if let Some(p) = crate::process::find(pid) {
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
    t.interval.store(interval_ns, Ordering::SeqCst);
    if value_ns == 0 {
        t.deadline.store(0, Ordering::SeqCst);
    } else {
        let d = crate::time::nanos() + value_ns;
        t.deadline.store(d, Ordering::SeqCst);
        sched::add_timer(d, t);
    }
    old
}

/// Forget a process's timer when it exits.
pub fn remove(pid: u32) {
    let t = x86_64::instructions::interrupts::without_interrupts(|| TIMERS.lock().remove(&pid));
    if let Some(t) = t {
        t.deadline.store(0, Ordering::SeqCst);
    }
}
