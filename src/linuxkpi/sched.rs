//! Scheduling entry points Linux code calls (`cond_resched`, …), mapped
//! onto `crate::sched`.

use core::ffi::c_int;
use core::sync::atomic::Ordering;

/// `cond_resched()`: give up the CPU if the scheduler asked for it.
/// Returns 1 if this thread was rescheduled.
#[unsafe(no_mangle)]
pub extern "C" fn __cond_resched() -> c_int {
    let pc = crate::arch::x86_64::cpu::this();
    if crate::sched::is_running()
        && pc.need_resched.load(Ordering::SeqCst) != 0
        && pc.preempt_count.load(Ordering::SeqCst) == 0
    {
        crate::sched::yield_now();
        1
    } else {
        0
    }
}
