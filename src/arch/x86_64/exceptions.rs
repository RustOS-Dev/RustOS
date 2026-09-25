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
            crate::serial_println!("[trap] NMI received");
        }
        _ => fatal(frame, None),
    }
}

fn fatal(frame: &TrapFrame, cr2: Option<u64>) -> ! {
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
