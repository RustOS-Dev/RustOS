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
