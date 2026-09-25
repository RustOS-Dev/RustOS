//! PS/2 keyboard (and mouse) on the i8042 controller, routed through the
//! I/O APIC.

use crate::arch::x86_64::{apic, idt};
use x86_64::instructions::port::Port;

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;

pub fn init() {
    crate::task::keyboard::init();
    // Drain anything the firmware left behind.
    for _ in 0..32 {
        if unsafe { Port::<u8>::new(STATUS).read() } & 1 == 0 {
            break;
        }
        let _ = unsafe { Port::<u8>::new(DATA).read() };
    }
    if let Some(v) = idt::alloc_vector(keyboard_irq) {
        apic::route_isa_irq(1, v);
    }
    if let Some(v) = idt::alloc_vector(mouse_irq) {
        apic::route_isa_irq(12, v);
    }
}

fn keyboard_irq(_f: &mut idt::TrapFrame) {
    let status = unsafe { Port::<u8>::new(STATUS).read() };
    if status & 1 != 0 {
        let byte = unsafe { Port::<u8>::new(DATA).read() };
        if status & 0x20 == 0 {
            crate::task::keyboard::add_scancode(byte);
        } else {
            crate::drivers::mouse::ps2_byte(byte);
        }
    }
    apic::eoi();
}

fn mouse_irq(_f: &mut idt::TrapFrame) {
    let status = unsafe { Port::<u8>::new(STATUS).read() };
    if status & 1 != 0 {
        let byte = unsafe { Port::<u8>::new(DATA).read() };
        crate::drivers::mouse::ps2_byte(byte);
    }
    apic::eoi();
}
