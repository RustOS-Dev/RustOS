//! CPU exception handling.
//!
//! Faults raised by user code are turned into signals for the faulting
//! process (page faults first get a chance to be resolved by demand paging or
//! copy-on-write). Faults in kernel code are fatal and panic with a register
//! dump.

use super::idt::{TrapFrame, exception_name};
use x86_64::registers::control::Cr2;

pub fn handle(frame: &mut TrapFrame) {
    match frame.vector {
        3 => {
            crate::serial_println!("[trap] breakpoint at {:#x}", frame.rip);
        }
        14 => {
            let addr = Cr2::read_raw();
            // Resolve faults with interrupts on when the faulting context had
            // them on: COW and demand paging may wait for other CPUs (TLB
            // shootdown) and must not stall timer ticks.
            if frame.rflags & 0x200 != 0 {
                x86_64::instructions::interrupts::enable();
            }
            if crate::process::handle_page_fault(frame, addr) {
                return;
            }
            if frame.from_user() {
                crate::process::user_page_fault(frame, addr);
                return;
            }
            fatal(frame, Some(addr));
        }
        _ if frame.from_user() => {
            use crate::process::signal::*;
            // Signal, si_code and si_addr as Linux's trap handlers.
            let (sig, code, addr) = match frame.vector {
                0 => (SIGFPE, FPE_INTDIV, frame.rip),
                16 | 19 => (SIGFPE, fpe_code(frame.vector), frame.rip),
                6 => (SIGILL, ILL_ILLOPN, frame.rip),
                1 => (SIGTRAP, TRAP_TRACE, frame.rip),
                17 => (SIGBUS, BUS_ADRALN, 0),
                _ => (SIGSEGV, SI_KERNEL, 0),
            };
            crate::process::user_fault(frame, sig, code, addr);
        }
        2 => {
            if super::smp::NMI_DUMP.load(core::sync::atomic::Ordering::SeqCst) {
                let mut trace = alloc::string::String::new();
                let mut rbp = frame.rbp;
                for _ in 0..12 {
                    if rbp < 0xffff_8000_0000_0000 || rbp & 7 != 0 {
                        break;
                    }
                    let ret = unsafe { *((rbp + 8) as *const u64) };
                    trace.push_str(&alloc::format!(" {:#x}", ret));
                    rbp = unsafe { *(rbp as *const u64) };
                }
                let line = alloc::format!(
                    "[nmi] cpu{} rip {:#x} if={} trace{}\n",
                    super::cpu::this().cpu_id,
                    frame.rip.wrapping_sub(KERNEL_IMAGE.start),
                    frame.rflags & 0x200 != 0,
                    trace
                );
                crate::drivers::serial::write_unlocked(line.as_bytes());
                stack_scan_serial(frame.rsp);
            } else {
                crate::serial_println!("[trap] NMI received");
            }
        }
        _ => fatal(frame, None),
    }
}

/// si_code of an x87 (#MF, vector 16) or SIMD (#XM, 19) floating-point
/// exception, from the unmasked exception flags (Linux's
/// `fpu__exception_code`). The user's FPU state is still live here.
fn fpe_code(vector: u64) -> i32 {
    use crate::process::signal::*;
    let err = if vector == 19 {
        let mut mxcsr = 0u32;
        unsafe {
            core::arch::asm!("stmxcsr [{}]", in(reg) &mut mxcsr, options(nostack));
        }
        mxcsr & !(mxcsr >> 7) & 0x3f
    } else {
        let (mut sw, mut cw) = (0u16, 0u16);
        unsafe {
            core::arch::asm!("fnstsw [{}]", in(reg) &mut sw, options(nostack));
            core::arch::asm!("fnstcw [{}]", in(reg) &mut cw, options(nostack));
        }
        (sw & !cw & 0x3f) as u32
    };
    if err & 0x01 != 0 {
        FPE_FLTINV
    } else if err & 0x04 != 0 {
        FPE_FLTDIV
    } else if err & 0x08 != 0 {
        FPE_FLTOVF
    } else if err & 0x12 != 0 {
        FPE_FLTUND
    } else if err & 0x20 != 0 {
        FPE_FLTRES
    } else {
        SI_KERNEL
    }
}

/// Where the kernel image is mapped (PIE, loaded by the bootloader).
const KERNEL_IMAGE: core::ops::Range<u64> = 0xffff_8000_0000_0000..0xffff_8000_0400_0000;

/// Print stack words that look like kernel code addresses (a heuristic
/// backtrace; resolve with `addr2line -e rustos 0xOFFSET`).
fn stack_scan(rsp: u64) {
    if !(0xffff_8000_0000_0000..u64::MAX - 1024).contains(&rsp) {
        return;
    }
    crate::println!("stack scan from {:#x} (image offsets):", rsp);
    let mut shown = 0;
    for i in 0..256u64 {
        let p = rsp + i * 8;
        // Stop at the end of the page to avoid faulting on a guard page.
        if i > 0 && p & 0xFFF == 0 && shown > 0 && i > 64 {
            break;
        }
        let v = unsafe { core::ptr::read_volatile(p as *const u64) };
        if KERNEL_IMAGE.contains(&v) {
            crate::println!("  [rsp+{:#05x}] {:#x}", i * 8, v - KERNEL_IMAGE.start);
            shown += 1;
            if shown >= 24 {
                break;
            }
        }
    }
}

/// Like [`stack_scan`], on one serial line (NMI debugging: no locks).
fn stack_scan_serial(rsp: u64) {
    if !(0xffff_8000_0000_0000..u64::MAX - 4096).contains(&rsp) {
        return;
    }
    let mut out = alloc::string::String::new();
    let mut shown = 0;
    let mut p = rsp;
    // Stay within the current and the next stack page.
    let end = (rsp & !0xFFF) + 0x2000;
    while p < end && shown < 20 {
        let v = unsafe { core::ptr::read_volatile(p as *const u64) };
        if KERNEL_IMAGE.contains(&v) {
            out.push_str(&alloc::format!(" {:#x}", v - KERNEL_IMAGE.start));
            shown += 1;
        }
        p += 8;
    }
    crate::drivers::serial::write_unlocked(alloc::format!("[nmi] stack{}\n", out).as_bytes());
}

fn fatal(frame: &TrapFrame, cr2: Option<u64>) -> ! {
    stack_scan(frame.rsp);
    let cpu = if super::cpu::is_initialized() {
        super::cpu::this().cpu_id
    } else {
        0
    };
    match cr2 {
        Some(a) => panic!(
            "kernel {} (error {:#x}) at {:#x} accessing {:#x} on CPU {}\n{:#x?}",
            exception_name(frame.vector),
            frame.error_code,
            frame.rip,
            a,
            cpu,
            frame
        ),
        None => panic!(
            "kernel {} (error {:#x}) at {:#x} on CPU {}\n{:#x?}",
            exception_name(frame.vector),
            frame.error_code,
            frame.rip,
            cpu,
            frame
        ),
    }
}
