//! Processes (placeholder until the user-mode process model lands).

pub mod signal;

use crate::arch::x86_64::idt::TrapFrame;

pub struct Process;

/// Resolve a page fault from demand paging / copy-on-write. Returns true if
/// the fault was handled.
pub fn handle_page_fault(_frame: &mut TrapFrame, _addr: u64) -> bool {
    false
}

/// A user-mode fault that could not be resolved.
pub fn user_fault(frame: &mut TrapFrame, sig: u32, addr: u64) {
    panic!(
        "user fault sig {} at {:#x} addr {:#x}",
        sig, frame.rip, addr
    );
}
