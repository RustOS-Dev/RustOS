//! Per-CPU data, reached through the GS base.
//!
//! In kernel mode `GS.base` points at this CPU's [`PerCpu`]; `swapgs` on
//! entry from / exit to ring 3 swaps it with the user value kept in
//! `KERNEL_GS_BASE`. The first fields are accessed from assembly at fixed
//! offsets and must not be reordered.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use x86_64::VirtAddr;
use x86_64::registers::model_specific::{GsBase, KernelGsBase};
use x86_64::structures::tss::TaskStateSegment;

#[repr(C)]
pub struct PerCpu {
    /// Offset 0: pointer to self (so `mov rax, gs:[0]` yields the block).
    pub self_ptr: u64,
    /// Offset 8: kernel stack top for the running thread (syscall entry).
    pub kernel_rsp: u64,
    /// Offset 16: scratch slot for the user RSP during syscall entry.
    pub user_rsp: u64,
    /// Offset 24: logical CPU index (0 = BSP).
    pub cpu_id: u32,
    pub lapic_id: u32,
    /// Offset 32: TSS for this CPU (rsp0 updated on every context switch).
    pub tss: *mut TaskStateSegment,
    /// Scheduler: currently running thread (opaque to arch code).
    pub current: AtomicUsize,
    /// Scheduler: this CPU's idle thread.
    pub idle: AtomicUsize,
    /// Nesting depth of preemption-disable sections.
    pub preempt_count: AtomicU32,
    /// Set when the scheduler wants to switch at the next safe point.
    pub need_resched: AtomicU32,
    /// Timer ticks handled on this CPU.
    pub ticks: core::sync::atomic::AtomicU64,
}

pub const PERCPU_KERNEL_RSP: usize = 8;
pub const PERCPU_USER_RSP: usize = 16;

pub const MAX_CPUS: usize = 64;
static CPUS: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];
static CPU_COUNT: AtomicU32 = AtomicU32::new(0);

/// Create and install this CPU's per-CPU block. Requires the heap.
pub fn init(cpu_id: u32, lapic_id: u32, tss: &'static mut TaskStateSegment) -> &'static PerCpu {
    let pc = Box::leak(Box::new(PerCpu {
        self_ptr: 0,
        kernel_rsp: tss.privilege_stack_table[0].as_u64(),
        user_rsp: 0,
        cpu_id,
        lapic_id,
        tss,
        current: AtomicUsize::new(0),
        idle: AtomicUsize::new(0),
        preempt_count: AtomicU32::new(0),
        need_resched: AtomicU32::new(0),
        ticks: core::sync::atomic::AtomicU64::new(0),
    }));
    pc.self_ptr = pc as *mut PerCpu as u64;
    GsBase::write(VirtAddr::new(pc.self_ptr));
    KernelGsBase::write(VirtAddr::new(0));
    CPUS[cpu_id as usize].store(pc.self_ptr as usize, Ordering::SeqCst);
    CPU_COUNT.fetch_max(cpu_id + 1, Ordering::SeqCst);
    pc
}

/// Whether the per-CPU block has been installed on this CPU.
pub fn is_initialized() -> bool {
    GsBase::read().as_u64() != 0
}

/// The current CPU's block. Only valid in kernel mode after [`init`].
#[inline]
pub fn this() -> &'static PerCpu {
    let p: u64;
    unsafe {
        core::arch::asm!("mov {}, gs:[0]", out(reg) p, options(nostack, readonly, preserves_flags))
    };
    unsafe { &*(p as *const PerCpu) }
}

/// Mutable access to this CPU's TSS (interrupts must be off).
pub fn set_kernel_stack(top: u64) {
    let pc = this();
    unsafe {
        (*pc.tss).privilege_stack_table[0] = VirtAddr::new(top);
        let p = pc as *const PerCpu as *mut PerCpu;
        (*p).kernel_rsp = top;
    }
}

/// Record the local APIC id once the APIC is up (the BSP's per-CPU block
/// is created before it).
pub fn set_lapic_id(id: u32) {
    unsafe { (*(this() as *const PerCpu as *mut PerCpu)).lapic_id = id };
}

pub fn cpu_count() -> u32 {
    CPU_COUNT.load(Ordering::SeqCst)
}

pub fn cpu(id: u32) -> Option<&'static PerCpu> {
    let p = CPUS.get(id as usize)?.load(Ordering::SeqCst);
    (p != 0).then(|| unsafe { &*(p as *const PerCpu) })
}
