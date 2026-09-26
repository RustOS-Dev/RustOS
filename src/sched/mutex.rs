//! A sleeping mutex for long critical sections (e.g. held across disk I/O).
//! Waiters block on a wait queue instead of spinning, and are served in
//! arrival order (a ticket lock): a thread that unlocks and immediately
//! locks again cannot starve a waiter that was just woken.

use super::WaitQueue;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU64, Ordering};

pub struct Mutex<T: ?Sized> {
    /// Next ticket to hand out.
    next: AtomicU64,
    /// Ticket currently holding the lock.
    serving: AtomicU64,
    wq: WaitQueue,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

pub struct MutexGuard<'a, T: ?Sized> {
    m: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(v: T) -> Mutex<T> {
        Mutex {
            next: AtomicU64::new(0),
            serving: AtomicU64::new(0),
            wq: WaitQueue::new(),
            data: UnsafeCell::new(v),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let ticket = self.next.fetch_add(1, Ordering::SeqCst);
        if self.serving.load(Ordering::SeqCst) != ticket {
            self.wq
                .wait_until(|| self.serving.load(Ordering::SeqCst) == ticket);
        }
        MutexGuard { m: self }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let now = self.serving.load(Ordering::SeqCst);
        self.next
            .compare_exchange(now, now + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            .then_some(MutexGuard { m: self })
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.m.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.m.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        let next = self.m.serving.load(Ordering::SeqCst) + 1;
        self.m.serving.store(next, Ordering::SeqCst);
        // Only the holder of ticket `next` can proceed; wake everyone so
        // it is not missed (the wait queue is not ordered by ticket).
        if self.m.next.load(Ordering::SeqCst) != next {
            self.m.wq.wake_all();
        }
    }
}
