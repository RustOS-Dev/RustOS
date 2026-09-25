//! `syscall` instruction entry.
//!
//! On `syscall` the CPU loads CS/SS from STAR, puts the user RIP in RCX and
//! RFLAGS in R11, and masks RFLAGS with SFMASK (IF is cleared). The stub swaps
//! to the kernel GS, switches to the thread's kernel stack, builds a
//! [`TrapFrame`] identical to the one interrupts produce and calls the
//! dispatcher. The return path is the shared `trap_return` (`iretq`), which
//! also handles frames rewritten by execve, fork children and signals.

use super::{cpu, gdt, idt};
use core::arch::global_asm;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::structures::gdt::SegmentSelector;
use x86_64::{PrivilegeLevel, VirtAddr};

global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "    swapgs",
    "    mov gs:[{user_rsp}], rsp",
    "    mov rsp, gs:[{kernel_rsp}]",
    "    push {user_ds}",          // ss
    "    push qword ptr gs:[{user_rsp}]", // rsp
    "    push r11",                // rflags
    "    push {user_cs}",          // cs
    "    push rcx",                // rip
    "    push 0",                  // error code
    "    push 0x80",               // vector (syscall marker)
    "    push rax",
    "    push rbx",
    "    push rcx",
    "    push rdx",
    "    push rsi",
    "    push rdi",
    "    push rbp",
    "    push r8",
    "    push r9",
    "    push r10",
    "    push r11",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    "    mov rdi, rsp",
    "    cld",
    "    call {dispatch}",
    "    jmp trap_return",
    user_rsp = const cpu::PERCPU_USER_RSP,
    kernel_rsp = const cpu::PERCPU_KERNEL_RSP,
    user_ds = const gdt::USER_DS as u64,
    user_cs = const gdt::USER_CS as u64,
    dispatch = sym syscall_from_user,
);

unsafe extern "C" {
    fn syscall_entry();
}

extern "C" fn syscall_from_user(frame: &mut idt::TrapFrame) {
    crate::syscall::dispatch(frame);
    x86_64::instructions::interrupts::disable();
    crate::sched::on_trap_exit(frame);
    x86_64::instructions::interrupts::disable();
}

/// Program the syscall MSRs on the current CPU and route `int 0x80` to the
/// same dispatcher.
pub fn init() {
    unsafe {
        Efer::update(|f| {
            f.insert(EferFlags::SYSTEM_CALL_EXTENSIONS | EferFlags::NO_EXECUTE_ENABLE);
        });
    }
    Star::write(
        SegmentSelector::new(4, PrivilegeLevel::Ring3),
        SegmentSelector::new(3, PrivilegeLevel::Ring3),
        SegmentSelector::new(1, PrivilegeLevel::Ring0),
        SegmentSelector::new(2, PrivilegeLevel::Ring0),
    )
    .expect("STAR layout");
    LStar::write(VirtAddr::new(syscall_entry as *const () as u64));
    SFMask::write(
        RFlags::INTERRUPT_FLAG
            | RFlags::TRAP_FLAG
            | RFlags::DIRECTION_FLAG
            | RFlags::ALIGNMENT_CHECK
            | RFlags::NESTED_TASK,
    );
    idt::register(idt::VEC_SYSCALL, crate::syscall::dispatch);
}
