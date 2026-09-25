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
    drop(q);
    WQ.wake_all();
    crate::vfs::notify_poll();
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

/// /dev/input/mice: PS/2-protocol 3-byte packets from any mouse.
pub struct MouseDev;

impl crate::vfs::FileLike for MouseDev {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> crate::errno::KResult<usize> {
        let mut n = 0;
        loop {
            while n + 3 <= buf.len() {
                let Some(ev) = pop() else { break };
                let dx = ev.dx.clamp(-255, 255);
                let dy = (-ev.dy).clamp(-255, 255);
                let mut flags = 0x08 | (ev.buttons & 7);
                if dx < 0 {
                    flags |= 0x10;
                }
                if dy < 0 {
                    flags |= 0x20;
                }
                buf[n] = flags;
                buf[n + 1] = dx as u8;
                buf[n + 2] = dy as u8;
                n += 3;
            }
            if n > 0 || nonblock {
                return if n > 0 {
                    Ok(n)
                } else {
                    Err(crate::errno::EAGAIN)
                };
            }
            WQ.wait_timeout(100, || !EVENTS.lock().is_empty());
            if crate::process::signal::has_pending() {
                return Err(crate::errno::EINTR);
            }
        }
    }
    fn write(&self, b: &[u8], _nb: bool) -> crate::errno::KResult<usize> {
        Ok(b.len())
    }
    fn poll(&self) -> u16 {
        if EVENTS.lock().is_empty() {
            0
        } else {
            crate::vfs::POLLIN
        }
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

static WQ: crate::sched::WaitQueue = crate::sched::WaitQueue::new();
