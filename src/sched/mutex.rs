//! A sleeping mutex for long critical sections (e.g. held across disk I/O).
//! Waiters block on a wait queue instead of spinning.

use super::WaitQueue;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

pub struct Mutex<T: ?Sized> {
    locked: AtomicBool,
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
            locked: AtomicBool::new(false),
            wq: WaitQueue::new(),
            data: UnsafeCell::new(v),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
    fn try_acquire(&self) -> bool {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        while !self.try_acquire() {
            self.wq.wait_until(|| !self.locked.load(Ordering::Relaxed));
        }
        MutexGuard { m: self }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.try_acquire().then_some(MutexGuard { m: self })
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
        self.m.locked.store(false, Ordering::Release);
        self.m.wq.wake_one();
    }
}
