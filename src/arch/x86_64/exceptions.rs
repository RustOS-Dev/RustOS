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
                crate::process::user_fault(frame, crate::process::signal::SIGSEGV, addr);
                return;
            }
            fatal(frame, Some(addr));
        }
        _ if frame.from_user() => {
            let sig = match frame.vector {
                0 | 16 | 19 => crate::process::signal::SIGFPE,
                6 => crate::process::signal::SIGILL,
                1 => crate::process::signal::SIGTRAP,
                17 => crate::process::signal::SIGBUS,
                _ => crate::process::signal::SIGSEGV,
            };
            crate::process::user_fault(frame, sig, 0);
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

/// Where the kernel image is mapped (PIE, loaded by the bootloader).
const KERNEL_IMAGE: core::ops::Range<u64> = 0xffff_8000_0000_0000..0xffff_8000_0400_0000;

/// Whether `va` is mapped, by walking the current page tables without
/// locks (usable from NMI and panic paths, where the mapper lock may be
/// held).
fn mapped(va: u64) -> bool {
    let (frame, _) = x86_64::registers::control::Cr3::read_raw();
    let mut table = frame.start_address().as_u64();
    for level in (0..4).rev() {
        let idx = (va >> (12 + 9 * level)) & 0x1FF;
        let entry = unsafe { *crate::mm::phys_ptr::<u64>(table + idx * 8) };
        if entry & 1 == 0 {
            return false;
        }
        // A huge page (2 MiB or 1 GiB) ends the walk.
        if (level == 1 || level == 2) && entry & 0x80 != 0 {
            return true;
        }
        table = entry & 0x000F_FFFF_FFFF_F000;
    }
    true
}

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
        // Never read an unmapped page (a guard page past the stack).
        if (i == 0 || p & 0xFFF == 0) && !mapped(p) {
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
        if (p == rsp || p & 0xFFF == 0) && !mapped(p) {
            break;
        }
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
