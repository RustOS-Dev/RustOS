//! Per-CPU GDT and TSS.
//!
//! Selector layout (fixed so `syscall`/`sysret` work):
//!
//! | index | selector | segment            |
//! |-------|----------|--------------------|
//! | 1     | 0x08     | kernel code        |
//! | 2     | 0x10     | kernel data        |
//! | 3     | 0x1b     | user data (RPL 3)  |
//! | 4     | 0x23     | user code (RPL 3)  |
//! | 5–6   | 0x28     | TSS                |
//!
//! `STAR[47:32] = 0x08` gives kernel CS 0x08 / SS 0x10 on `syscall`;
//! `STAR[63:48] = 0x10` gives user SS 0x1b / CS 0x23 on `sysretq`.

use alloc::boxed::Box;
use x86_64::PrivilegeLevel;
use x86_64::VirtAddr;
use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_DS: u16 = 0x10;
pub const USER_DS: u16 = 0x1b;
pub const USER_CS: u16 = 0x23;
pub const TSS_SEL: u16 = 0x28;

pub const IST_DOUBLE_FAULT: u8 = 0;
pub const IST_NMI: u8 = 1;
pub const IST_MCE: u8 = 2;

const IST_STACK_SIZE: usize = 16 * 1024;

/// A CPU's descriptor tables. Leaked on creation: CPUs never go away.
pub struct CpuTables {
    pub gdt: GlobalDescriptorTable,
    pub tss: TaskStateSegment,
}

fn ist_stack() -> VirtAddr {
    let stack = Box::leak(alloc::vec![0u8; IST_STACK_SIZE].into_boxed_slice());
    VirtAddr::from_ptr(stack.as_ptr()) + IST_STACK_SIZE as u64
}

/// Early boot GDT used before the heap exists (BSP only).
static mut BOOT_TSS: TaskStateSegment = TaskStateSegment::new();
static mut BOOT_GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();
static mut BOOT_DF_STACK: [u8; IST_STACK_SIZE] = [0; IST_STACK_SIZE];

fn build(gdt: &mut GlobalDescriptorTable, tss: &'static TaskStateSegment) {
    let kc = gdt.add_entry(Descriptor::kernel_code_segment());
    let kd = gdt.add_entry(Descriptor::kernel_data_segment());
    let ud = gdt.add_entry(Descriptor::user_data_segment());
    let uc = gdt.add_entry(Descriptor::user_code_segment());
    let ts = gdt.add_entry(Descriptor::tss_segment(tss));
    debug_assert_eq!(kc.0, KERNEL_CS);
    debug_assert_eq!(kd.0, KERNEL_DS);
    debug_assert_eq!(ud.0 | 3, USER_DS);
    debug_assert_eq!(uc.0 | 3, USER_CS);
    debug_assert_eq!(ts.0, TSS_SEL);
}

fn load(gdt: &'static GlobalDescriptorTable) {
    gdt.load();
    unsafe {
        CS::set_reg(SegmentSelector::new(1, PrivilegeLevel::Ring0));
        let kd = SegmentSelector::new(2, PrivilegeLevel::Ring0);
        SS::set_reg(kd);
        DS::set_reg(kd);
        ES::set_reg(kd);
        load_tss(SegmentSelector::new(5, PrivilegeLevel::Ring0));
    }
}

/// Load the static boot GDT on the BSP (no heap required).
#[allow(clippy::deref_addrof)]
pub fn init_boot() {
    // SAFETY: runs once on the BSP before any other CPU or interrupt exists.
    let tss: &'static mut TaskStateSegment = unsafe { &mut *(&raw mut BOOT_TSS) };
    tss.interrupt_stack_table[IST_DOUBLE_FAULT as usize] =
        VirtAddr::from_ptr(&raw const BOOT_DF_STACK) + IST_STACK_SIZE as u64;
    let gdt: &'static mut GlobalDescriptorTable = unsafe { &mut *(&raw mut BOOT_GDT) };
    let tss: &'static TaskStateSegment = tss;
    build(gdt, tss);
    let gdt: &'static GlobalDescriptorTable = gdt;
    load(gdt);
}

/// Allocate and load full per-CPU tables (IST stacks for #DF, NMI, #MC).
/// Returns a pointer to the TSS so the scheduler can update `rsp0`.
pub fn init_cpu() -> &'static mut TaskStateSegment {
    let tables: &'static mut CpuTables = Box::leak(Box::new(CpuTables {
        gdt: GlobalDescriptorTable::new(),
        tss: TaskStateSegment::new(),
    }));
    tables.tss.interrupt_stack_table[IST_DOUBLE_FAULT as usize] = ist_stack();
    tables.tss.interrupt_stack_table[IST_NMI as usize] = ist_stack();
    tables.tss.interrupt_stack_table[IST_MCE as usize] = ist_stack();
    // An initial ring-0 stack for traps from user mode; replaced per thread.
    tables.tss.privilege_stack_table[0] = ist_stack();
    let tss_ptr: *const TaskStateSegment = &tables.tss;
    build(&mut tables.gdt, unsafe { &*tss_ptr });
    load(unsafe { &*(&tables.gdt as *const GlobalDescriptorTable) });
    &mut tables.tss
}
