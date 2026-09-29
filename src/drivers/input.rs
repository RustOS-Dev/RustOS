//! Input event devices for HID pointers and game controllers.
//!
//! `/dev/input/event0` delivers Linux `struct input_event` records (24
//! bytes: timeval, type, code, value) for every pointer and game device:
//! relative motion and wheels (EV_REL), absolute axes from tablets, touch
//! screens and sticks (EV_ABS), buttons (EV_KEY), each report closed by
//! EV_SYN. `/dev/input/js0` speaks the Linux joystick API (8-byte
//! `struct js_event`) for game pads and joysticks. Keyboards keep feeding
//! the console; relative mice also feed `/dev/input/mice`
//! (`drivers::mouse`), and absolute pointers do too, as motion scaled to
//! a virtual 1920x1080 screen.

use crate::errno::*;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const EV_ABS: u16 = 3;

pub const REL_X: u16 = 0;
pub const REL_Y: u16 = 1;
pub const REL_HWHEEL: u16 = 6;
pub const REL_WHEEL: u16 = 8;

pub const ABS_X: u16 = 0;
pub const ABS_Y: u16 = 1;
pub const ABS_Z: u16 = 2;
pub const ABS_RX: u16 = 3;
pub const ABS_RY: u16 = 4;
pub const ABS_RZ: u16 = 5;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

/// First mouse button (BTN_LEFT); buttons 1..8 map to 0x110..0x117.
pub const BTN_MOUSE: u16 = 0x110;
/// First joystick button (BTN_TRIGGER).
pub const BTN_JOYSTICK: u16 = 0x120;
/// First game-pad button (BTN_SOUTH / BTN_A).
pub const BTN_GAMEPAD: u16 = 0x130;

#[derive(Clone, Copy, Debug)]
struct Event {
    time_ns: u64,
    kind: u16,
    code: u16,
    value: i32,
}

#[derive(Clone, Copy, Debug)]
struct JsEvent {
    time_ms: u32,
    value: i16,
    kind: u8,
    number: u8,
}

const QUEUE_MAX: usize = 1024;

/// One open file: its own queue (events before the open are not seen).
struct Client<T> {
    queue: Mutex<VecDeque<T>>,
}

static EV_CLIENTS: Mutex<Vec<Weak<Client<Event>>>> = Mutex::new(Vec::new());
static JS_CLIENTS: Mutex<Vec<Weak<Client<JsEvent>>>> = Mutex::new(Vec::new());
static WQ: WaitQueue = WaitQueue::new();

fn irqless<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

fn broadcast<T: Copy>(clients: &Mutex<Vec<Weak<Client<T>>>>, e: T) {
    irqless(|| {
        let mut cl = clients.lock();
        cl.retain(|w| w.strong_count() > 0);
        for c in cl.iter().filter_map(|w| w.upgrade()) {
            let mut q = c.queue.lock();
            if q.len() >= QUEUE_MAX {
                q.pop_front();
            }
            q.push_back(e);
        }
    });
}

/// Queue one event (call [`sync`] after a report's events).
pub fn emit(kind: u16, code: u16, value: i32) {
    let time_ns = crate::time::realtime_nanos();
    broadcast(
        &EV_CLIENTS,
        Event {
            time_ns,
            kind,
            code,
            value,
        },
    );
}

/// End of one report: SYN_REPORT, and wake readers.
pub fn sync() {
    emit(EV_SYN, 0, 0);
    WQ.wake_all();
    crate::vfs::notify_poll();
}

/// Joystick event: `button` (true) or axis `number` with `value`.
pub fn js_emit(button: bool, number: u8, value: i16) {
    let time_ms = crate::time::millis() as u32;
    broadcast(
        &JS_CLIENTS,
        JsEvent {
            time_ms,
            value,
            kind: if button { 1 } else { 2 },
            number,
        },
    );
    WQ.wake_all();
    crate::vfs::notify_poll();
}

/// Scale `v` in `lo..=hi` to the joystick range -32767..=32767.
pub fn js_scale(v: i32, lo: i32, hi: i32) -> i16 {
    if hi <= lo {
        return 0;
    }
    let v = v.clamp(lo, hi) as i64;
    let (lo, hi) = (lo as i64, hi as i64);
    ((v - lo) * 65534 / (hi - lo) - 32767) as i16
}

impl<T: Copy + Send + 'static> Client<T> {
    fn new(list: &Mutex<Vec<Weak<Client<T>>>>) -> Arc<Client<T>> {
        let c = Arc::new(Client {
            queue: Mutex::new(VecDeque::new()),
        });
        irqless(|| list.lock().push(Arc::downgrade(&c)));
        c
    }

    fn is_empty(&self) -> bool {
        irqless(|| self.queue.lock().is_empty())
    }

    fn read(
        &self,
        buf: &mut [u8],
        nonblock: bool,
        size: usize,
        enc: impl Fn(&T, &mut [u8]),
    ) -> KResult<usize> {
        if buf.len() < size {
            return Err(EINVAL);
        }
        loop {
            let mut n = 0;
            while n + size <= buf.len() {
                let Some(e) = irqless(|| self.queue.lock().pop_front()) else {
                    break;
                };
                enc(&e, &mut buf[n..n + size]);
                n += size;
            }
            if n > 0 {
                return Ok(n);
            }
            if nonblock {
                return Err(EAGAIN);
            }
            WQ.wait_timeout(100, || !self.is_empty());
            if crate::process::signal::has_pending() {
                return Err(EINTR);
            }
        }
    }
}

fn encode_event(e: &Event, b: &mut [u8]) {
    b[0..8].copy_from_slice(&((e.time_ns / 1_000_000_000) as i64).to_le_bytes());
    b[8..16].copy_from_slice(&((e.time_ns % 1_000_000_000 / 1000) as i64).to_le_bytes());
    b[16..18].copy_from_slice(&e.kind.to_le_bytes());
    b[18..20].copy_from_slice(&e.code.to_le_bytes());
    b[20..24].copy_from_slice(&e.value.to_le_bytes());
}

fn encode_js(e: &JsEvent, b: &mut [u8]) {
    b[0..4].copy_from_slice(&e.time_ms.to_le_bytes());
    b[4..6].copy_from_slice(&e.value.to_le_bytes());
    b[6] = e.kind;
    b[7] = e.number;
}

macro_rules! filelike {
    ($t:ty, $size:expr, $enc:expr) => {
        impl crate::vfs::FileLike for $t {
            fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
                self.0.read(buf, nonblock, $size, $enc)
            }
            fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
                Ok(b.len())
            }
            fn poll(&self) -> u16 {
                if self.0.is_empty() {
                    0
                } else {
                    crate::vfs::POLLIN
                }
            }
            fn as_any(&self) -> &dyn core::any::Any {
                self
            }
        }
    };
}

struct EventFile(Arc<Client<Event>>);
struct JsFile(Arc<Client<JsEvent>>);
filelike!(EventFile, 24, encode_event);
filelike!(JsFile, 8, encode_js);

/// /dev/input/event0: each open gets its own event queue.
pub struct EventDev;

impl crate::vfs::FileLike for EventDev {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    fn open_instance(&self, _flags: u32) -> KResult<Option<Arc<dyn crate::vfs::FileLike>>> {
        Ok(Some(Arc::new(EventFile(Client::new(&EV_CLIENTS)))))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// /dev/input/js0: each open gets its own event queue.
pub struct JoystickDev;

impl crate::vfs::FileLike for JoystickDev {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    fn open_instance(&self, _flags: u32) -> KResult<Option<Arc<dyn crate::vfs::FileLike>>> {
        Ok(Some(Arc::new(JsFile(Client::new(&JS_CLIENTS)))))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::js_scale;

    #[test_case]
    fn joystick_scaling() {
        assert_eq!(js_scale(0, 0, 255), -32767);
        assert_eq!(js_scale(255, 0, 255), 32767);
        assert_eq!(js_scale(-1, 0, 255), -32767);
    }
}
