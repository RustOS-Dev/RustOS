//! Spin locks for the kernel.
//!
//! Locks are often taken with interrupts disabled, so a CPU spinning on one
//! cannot take the TLB shootdown IPI. If the holder is itself waiting for a
//! shootdown (e.g. `munmap` holding an address space while another thread
//! of the process faults on it), both CPUs would stall. These locks answer
//! pending shootdowns while they spin.

/// Relax strategy that services TLB shootdown requests.
pub struct TlbRelax;

impl spin::RelaxStrategy for TlbRelax {
    #[inline(always)]
    fn relax() {
        crate::arch::x86_64::smp::poll();
        core::hint::spin_loop();
    }
}

pub type Mutex<T> = spin::mutex::Mutex<T, TlbRelax>;
pub type MutexGuard<'a, T> = spin::MutexGuard<'a, T>;
pub type RwLock<T> = spin::rwlock::RwLock<T, TlbRelax>;

/// A spin lock held with interrupts disabled, so its holder cannot be
/// preempted: for state that code running with preemption off (LinuxKPI
/// softirq and RCU sections, interrupt handlers) also locks.
pub struct IrqMutex<T> {
    inner: Mutex<T>,
}

pub struct IrqMutexGuard<'a, T> {
    guard: core::mem::ManuallyDrop<MutexGuard<'a, T>>,
    enable: bool,
}

impl<T> IrqMutex<T> {
    pub const fn new(v: T) -> IrqMutex<T> {
        IrqMutex {
            inner: Mutex::new(v),
        }
    }

    pub fn lock(&self) -> IrqMutexGuard<'_, T> {
        let enable = x86_64::instructions::interrupts::are_enabled();
        x86_64::instructions::interrupts::disable();
        IrqMutexGuard {
            guard: core::mem::ManuallyDrop::new(self.inner.lock()),
            enable,
        }
    }
}

impl<T> core::ops::Deref for IrqMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> core::ops::DerefMut for IrqMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Drop for IrqMutexGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: dropped exactly once, here.
        unsafe { core::mem::ManuallyDrop::drop(&mut self.guard) };
        if self.enable {
            x86_64::instructions::interrupts::enable();
        }
    }
}
