//! PS/2 keyboard (and mouse) on the i8042 controller, routed through the
//! I/O APIC.

use crate::arch::x86_64::{apic, idt};
use x86_64::instructions::port::Port;

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;

use crate::drivers::input::{self, Info, InputDev};
use alloc::sync::Arc;

/// The evdev devices of the i8042 keyboard and mouse ports.
static KBD: spin::Once<Arc<InputDev>> = spin::Once::new();
pub static MOUSE: spin::Once<Arc<InputDev>> = spin::Once::new();

pub fn init() {
    crate::task::keyboard::init();
    KBD.call_once(|| {
        input::register(
            Info::new(
                "AT Translated Set 2 keyboard",
                "isa0060/serio0/input0",
                [input::BUS_I8042, 1, 1, 0xAB41],
            )
            .keyboard(),
        )
    });
    MOUSE.call_once(|| {
        input::register(
            Info::new(
                "PS/2 Generic Mouse",
                "isa0060/serio1/input0",
                [input::BUS_I8042, 2, 1, 0],
            )
            .mouse(3, false),
        )
    });
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
            if let Some(k) = KBD.get() {
                k.scancode(byte);
            }
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
