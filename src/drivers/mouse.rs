//! Pointer input: PS/2 packet decoding and a shared event queue fed by PS/2
//! and USB HID mice.

use alloc::collections::VecDeque;
use spin::Mutex;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseEvent {
    pub dx: i32,
    pub dy: i32,
    pub wheel: i32,
    pub buttons: u8,
}

static EVENTS: Mutex<VecDeque<MouseEvent>> = Mutex::new(VecDeque::new());
static PACKET: Mutex<([u8; 3], usize)> = Mutex::new(([0; 3], 0));

pub fn push(ev: MouseEvent) {
    let mut q = EVENTS.lock();
    if q.len() >= 256 {
        q.pop_front();
    }
    q.push_back(ev);
}

pub fn pop() -> Option<MouseEvent> {
    x86_64::instructions::interrupts::without_interrupts(|| EVENTS.lock().pop_front())
}

/// Feed one byte from the PS/2 auxiliary port (standard 3-byte packets).
pub fn ps2_byte(b: u8) {
    let mut p = PACKET.lock();
    let idx = p.1;
    if idx == 0 && b & 0x08 == 0 {
        return; // resynchronise on the always-one bit
    }
    p.0[idx] = b;
    p.1 += 1;
    if p.1 == 3 {
        p.1 = 0;
        let [flags, x, y] = p.0;
        let dx = x as i32 - if flags & 0x10 != 0 { 256 } else { 0 };
        let dy = y as i32 - if flags & 0x20 != 0 { 256 } else { 0 };
        push(MouseEvent {
            dx,
            dy: -dy,
            wheel: 0,
            buttons: flags & 7,
        });
    }
}
