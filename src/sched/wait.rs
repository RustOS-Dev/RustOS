//! Wait queues: block threads until a condition becomes true.

use super::{Thread, current, is_running, schedule, wake};
use crate::sync::Mutex;
use alloc::collections::VecDeque;
use alloc::sync::Arc;

pub struct WaitQueue {
    waiters: Mutex<VecDeque<Arc<Thread>>>,
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitQueue {
    pub const fn new() -> WaitQueue {
        WaitQueue {
            waiters: Mutex::new(VecDeque::new()),
        }
    }

    /// Block until `cond` returns true.
    pub fn wait_until(&self, mut cond: impl FnMut() -> bool) {
        self.wait_until_deadline(&mut cond, None);
    }

    /// Block until `cond` is true or `ms` milliseconds pass. Returns the final
    /// value of `cond`.
    pub fn wait_timeout(&self, ms: u64, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = crate::time::nanos() + ms * 1_000_000;
        self.wait_until_deadline(&mut cond, Some(deadline))
    }

    /// Like [`wait_until`](Self::wait_until) but also returns (false) when the
    /// thread is interrupted by a signal.
    pub fn wait_interruptible(&self, mut cond: impl FnMut() -> bool) -> bool {
        let t = super::try_current();
        let mut c = || {
            cond()
                || t.as_ref()
                    .is_some_and(|t| t.interrupted.load(core::sync::atomic::Ordering::SeqCst))
        };
        self.wait_until_deadline(&mut c, None);
        cond()
    }

    fn wait_until_deadline(&self, cond: &mut dyn FnMut() -> bool, deadline: Option<u64>) -> bool {
        if !is_running() {
            loop {
                if cond() {
                    return true;
                }
                if deadline.is_some_and(|d| crate::time::nanos() >= d) {
                    return false;
                }
                core::hint::spin_loop();
            }
        }
        let t = current();
        t.wchan.store(
            self as *const _ as u64,
            core::sync::atomic::Ordering::Relaxed,
        );
        loop {
            if cond() {
                return true;
            }
            if deadline.is_some_and(|d| crate::time::nanos() >= d) {
                return cond();
            }
            x86_64::instructions::interrupts::without_interrupts(|| {
                // Blocked before visible in the queue: a waker on another
                // CPU that pops us must find us Blocked, or its wake-up
                // would be lost (it removes us from the queue either way).
                super::set_blocked(&t);
                if let Some(d) = deadline {
                    super::arm_timeout(&t, d);
                }
                self.waiters.lock().push_back(t.clone());
            });
            // Re-check after publishing ourselves to close the lost-wakeup race.
            if cond() {
                wake(&t);
            }
            schedule();
            x86_64::instructions::interrupts::without_interrupts(|| {
                self.waiters.lock().retain(|w| !Arc::ptr_eq(w, &t));
            });
        }
    }

    /// Wake every waiter. Safe to call from interrupt handlers.
    pub fn wake_all(&self) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut q = self.waiters.lock();
            while let Some(t) = q.pop_front() {
                wake(&t);
            }
        });
    }

    /// Wake one waiter.
    pub fn wake_one(&self) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            if let Some(t) = self.waiters.lock().pop_front() {
                wake(&t);
            }
        });
    }
}
