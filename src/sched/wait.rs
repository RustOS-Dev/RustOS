//! Wait queues: block threads until a condition becomes true.

use super::{Thread, current, is_running, schedule, wake};
use crate::sync::Mutex;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

pub struct WaitQueue {
    waiters: Mutex<VecDeque<Arc<Thread>>>,
    /// Callbacks run on every wake-up (poll/select waiters, epoll
    /// interests).
    hooks: Mutex<Vec<Arc<dyn WakeHook>>>,
}

/// Something to notify when a wait queue is woken. Runs with interrupts
/// off, possibly in an interrupt handler: it must not allocate or block.
pub trait WakeHook: Send + Sync {
    /// Returns false when the hook is dead and should be dropped.
    fn woken(&self) -> bool;
}

/// A thread sleeping in poll/select/epoll_wait on several queues at once.
pub struct PollWaiter {
    thread: Arc<Thread>,
    fired: AtomicBool,
}

impl WakeHook for PollWaiter {
    fn woken(&self) -> bool {
        self.fired.store(true, Ordering::SeqCst);
        wake(&self.thread);
        true
    }
}

/// Sleep until `check` returns non-zero, `deadline` passes or the thread
/// is interrupted by a signal, waking whenever one of `queues` is woken.
/// Returns the last value of `check` (0 on timeout or interruption).
pub fn wait_any<E>(
    queues: &[&WaitQueue],
    deadline: Option<u64>,
    mut check: impl FnMut() -> Result<usize, E>,
    interrupted: impl Fn() -> bool,
) -> Result<usize, E> {
    let n = check()?;
    if n > 0 || deadline.is_some_and(|d| crate::time::nanos() >= d) || !is_running() {
        return Ok(n);
    }
    let t = current();
    let w = Arc::new(PollWaiter {
        thread: t.clone(),
        fired: AtomicBool::new(false),
    });
    let hook: Arc<dyn WakeHook> = w.clone();
    for (i, q) in queues.iter().enumerate() {
        // The same queue may back several descriptors.
        if !queues[..i].iter().any(|p| core::ptr::eq(*p, *q)) {
            q.add_hook(hook.clone());
        }
    }
    t.wchan.store(
        queues.first().map_or(0, |q| *q as *const _ as u64),
        Ordering::Relaxed,
    );
    let result = loop {
        w.fired.store(false, Ordering::SeqCst);
        let n = match check() {
            Ok(n) => n,
            Err(e) => break Err(e),
        };
        if n > 0 || interrupted() {
            break Ok(n);
        }
        if deadline.is_some_and(|d| crate::time::nanos() >= d) {
            break Ok(0);
        }
        x86_64::instructions::interrupts::without_interrupts(|| {
            super::set_blocked(&t);
            if let Some(d) = deadline {
                super::arm_timeout(&t, d);
            }
        });
        // A wake-up between the check and set_blocked found us running.
        if w.fired.load(Ordering::SeqCst) {
            wake(&t);
        }
        schedule();
    };
    for (i, q) in queues.iter().enumerate() {
        if !queues[..i].iter().any(|p| core::ptr::eq(*p, *q)) {
            q.remove_hook(&hook);
        }
    }
    result
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
            hooks: Mutex::new(Vec::new()),
        }
    }

    pub fn add_hook(&self, h: Arc<dyn WakeHook>) {
        x86_64::instructions::interrupts::without_interrupts(|| self.hooks.lock().push(h));
    }

    pub fn remove_hook(&self, h: &Arc<dyn WakeHook>) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            self.hooks.lock().retain(|x| !Arc::ptr_eq(x, h))
        });
    }

    fn run_hooks(&self) {
        let mut hooks = self.hooks.lock();
        if !hooks.is_empty() {
            hooks.retain(|h| h.woken());
        }
    }

    /// Block until `cond` returns true.
    pub fn wait_until(&self, mut cond: impl FnMut() -> bool) {
        self.wait_until_deadline(&mut cond, None, false);
    }

    /// Block until `cond` is true or `ms` milliseconds pass. Returns the final
    /// value of `cond`.
    pub fn wait_timeout(&self, ms: u64, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = crate::time::nanos() + ms * 1_000_000;
        self.wait_until_deadline(&mut cond, Some(deadline), false)
    }

    /// Like [`wait_until`](Self::wait_until) but also returns (false) when the
    /// thread is interrupted by a signal.
    pub fn wait_interruptible(&self, mut cond: impl FnMut() -> bool) -> bool {
        let t = super::try_current();
        let mut c = || {
            cond()
                || t.as_ref()
                    .is_some_and(|t| t.interrupted.load(Ordering::SeqCst))
        };
        self.wait_until_deadline(&mut c, None, true);
        cond()
    }

    /// `interruptible`: the sleep ends on signals; uninterruptible sleeps
    /// of user threads count towards the load average (Linux's `D`).
    fn wait_until_deadline(
        &self,
        cond: &mut dyn FnMut() -> bool,
        deadline: Option<u64>,
        interruptible: bool,
    ) -> bool {
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
        t.wchan.store(self as *const _ as u64, Ordering::Relaxed);
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
            {
                let _d = super::cputime::UninterruptibleSleep::new(
                    !interruptible && t.is_user.load(Ordering::Relaxed),
                );
                schedule();
            }
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
            drop(q);
            self.run_hooks();
        });
    }

    /// Wake one waiter.
    pub fn wake_one(&self) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            if let Some(t) = self.waiters.lock().pop_front() {
                wake(&t);
            }
            self.run_hooks();
        });
    }
}
