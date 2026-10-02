//! Input event devices: the Linux evdev and joystick interfaces.
//!
//! Every input device (PS/2 keyboard and mouse, USB HID keyboards, mice,
//! tablets, touch screens, game pads) registers an [`InputDev`] and gets
//! its own `/dev/input/eventN` (N >= 1) delivering `struct input_event`
//! records (24 bytes: timeval, type, code, value): keys and buttons
//! (EV_KEY), relative motion and wheels (EV_REL), absolute axes (EV_ABS),
//! each report closed by EV_SYN. `/dev/input/event0` merges all devices.
//! The EVIOCG* ioctls describe a device (name, id, capability bitmaps,
//! key and LED state, axis ranges); EVIOCGRAB takes a device exclusively;
//! EV_LED events written to a keyboard set its lock LEDs.
//! `/dev/input/js0` speaks the joystick API (8-byte `struct js_event`, and
//! the JSIOCG* ioctls) for game pads and joysticks. Keyboards also feed
//! the console, and pointers `/dev/input/mice` (`drivers::mouse`).

use crate::errno::*;
use crate::process::uaccess;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const EV_ABS: u16 = 3;
pub const EV_MSC: u16 = 4;
pub const EV_LED: u16 = 0x11;
pub const EV_REP: u16 = 0x14;

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
pub const BTN_TOUCH: u16 = 0x14A;

pub const LED_NUML: u16 = 0;
pub const LED_CAPSL: u16 = 1;
pub const LED_SCROLLL: u16 = 2;

pub const BUS_I8042: u16 = 0x11;
pub const BUS_USB: u16 = 0x03;
pub const BUS_BLUETOOTH: u16 = 0x05;
pub const BUS_VIRTUAL: u16 = 0x06;

const INPUT_PROP_POINTER: u32 = 0;
const INPUT_PROP_DIRECT: u32 = 1;
const KEY_MAX: usize = 0x2FF;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AbsInfo {
    pub value: i32,
    pub min: i32,
    pub max: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub res: i32,
}

/// What a device is and what it can report.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub name: String,
    pub phys: String,
    pub uniq: String,
    /// bustype, vendor, product, version.
    pub id: [u16; 4],
    pub keys: Vec<u16>,
    pub rel: Vec<u16>,
    pub abs: Vec<(u16, AbsInfo)>,
    pub leds: Vec<u16>,
    pub props: u32,
    /// Game pad / joystick (also on /dev/input/js0): axes and buttons.
    pub game: Option<(u8, u8)>,
}

impl Info {
    pub fn new(name: &str, phys: &str, id: [u16; 4]) -> Info {
        Info {
            name: String::from(name),
            phys: String::from(phys),
            id,
            ..Default::default()
        }
    }

    /// A keyboard: every key code, and the three lock LEDs.
    pub fn keyboard(mut self) -> Info {
        self.keys.extend(1..=248);
        self.leds = alloc::vec![LED_NUML, LED_CAPSL, LED_SCROLLL];
        self
    }

    /// A relative pointer with `buttons` buttons and optional wheels.
    pub fn mouse(mut self, buttons: u16, wheels: bool) -> Info {
        self.keys.extend((0..buttons).map(|b| BTN_MOUSE + b));
        self.rel = alloc::vec![REL_X, REL_Y];
        if wheels {
            self.rel.extend([REL_WHEEL, REL_HWHEEL]);
        }
        self.props |= 1 << INPUT_PROP_POINTER;
        self
    }

    fn ev_bits(&self) -> u32 {
        let mut ev = 1 << EV_SYN;
        if !self.keys.is_empty() {
            ev |= 1 << EV_KEY;
        }
        if !self.rel.is_empty() {
            ev |= 1 << EV_REL;
        }
        if !self.abs.is_empty() {
            ev |= 1 << EV_ABS;
        }
        if !self.leds.is_empty() {
            ev |= (1 << EV_LED) | (1 << EV_REP);
        }
        ev
    }
}

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
    /// EVIOCSCLOCKID: monotonic timestamps instead of wall-clock time.
    monotonic: AtomicBool,
}

/// Leaves a keyboard's lock LEDs (bit 0 num, 1 caps, 2 scroll).
pub type LedSink = Box<dyn Fn(u8) + Send + Sync>;

/// A registered input device (`/dev/input/event{idx}`).
pub struct InputDev {
    pub idx: usize,
    info: Mutex<Info>,
    clients: Mutex<Vec<Weak<Client<Event>>>>,
    grab: Mutex<Option<Weak<Client<Event>>>>,
    keys: Mutex<[u64; (KEY_MAX + 1) / 64]>,
    leds: AtomicU8,
    led_sink: Mutex<Option<Arc<LedSink>>>,
    /// set-1 scancode prefix state for [`InputDev::scancode`].
    prefix: AtomicU8,
}

static EV_CLIENTS: Mutex<Vec<Weak<Client<Event>>>> = Mutex::new(Vec::new());
static JS_CLIENTS: Mutex<Vec<Weak<Client<JsEvent>>>> = Mutex::new(Vec::new());
static DEVICES: Mutex<Vec<Arc<InputDev>>> = Mutex::new(Vec::new());
static WQ: WaitQueue = WaitQueue::new();
/// Lock LED state of the console (applied to every keyboard).
static CONSOLE_LEDS: AtomicU32 = AtomicU32::new(u32::MAX);

fn irqless<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

fn push_to<T: Copy>(clients: &Mutex<Vec<Weak<Client<T>>>>, e: T) {
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

/// Register a device: it appears as the lowest free /dev/input/eventN.
pub fn register(info: Info) -> Arc<InputDev> {
    let dev = irqless(|| {
        let mut devs = DEVICES.lock();
        let idx = (1..).find(|i| !devs.iter().any(|d| d.idx == *i)).unwrap();
        let d = Arc::new(InputDev {
            idx,
            info: Mutex::new(info),
            clients: Mutex::new(Vec::new()),
            grab: Mutex::new(None),
            keys: Mutex::new([0; (KEY_MAX + 1) / 64]),
            leds: AtomicU8::new(0),
            led_sink: Mutex::new(None),
            prefix: AtomicU8::new(0),
        });
        devs.push(d.clone());
        d
    });
    crate::vfs::devfs::register(
        &alloc::format!("input/event{}", dev.idx),
        crate::vfs::FileType::CharDevice,
        (13 << 8) | (64 + dev.idx as u64),
        Arc::new(EventNode(Some(dev.clone()))),
    );
    dev
}

/// Remove a device (unplugged); its open files see no more events.
pub fn unregister(dev: &Arc<InputDev>) {
    crate::vfs::devfs::unregister(&alloc::format!("input/event{}", dev.idx));
    irqless(|| DEVICES.lock().retain(|d| !Arc::ptr_eq(d, dev)));
}

impl InputDev {
    /// Queue one event (call [`InputDev::sync`] after a report's events).
    pub fn emit(&self, kind: u16, code: u16, value: i32) {
        if kind == EV_KEY && (code as usize) <= KEY_MAX {
            irqless(|| {
                let mut k = self.keys.lock();
                let (w, b) = (code as usize / 64, code % 64);
                if value != 0 {
                    k[w] |= 1 << b;
                } else {
                    k[w] &= !(1 << b);
                }
            });
        }
        let e = Event {
            time_ns: crate::time::realtime_nanos(),
            kind,
            code,
            value,
        };
        let grab = irqless(|| self.grab.lock().as_ref().and_then(|w| w.upgrade()));
        match grab {
            Some(c) => irqless(|| {
                let mut q = c.queue.lock();
                if q.len() < QUEUE_MAX {
                    q.push_back(e);
                }
            }),
            None => {
                push_to(&self.clients, e);
                push_to(&EV_CLIENTS, e);
            }
        }
    }

    /// End of one report: SYN_REPORT, and wake readers.
    pub fn sync(&self) {
        self.emit(EV_SYN, 0, 0);
        WQ.wake_all();
    }

    /// Feed a PS/2 set-1 scancode byte (as keyboards give the console):
    /// key events with Linux key codes.
    pub fn scancode(&self, b: u8) {
        match b {
            0xE0 | 0xE1 => {
                self.prefix.store(b, Ordering::Relaxed);
                return;
            }
            _ => {}
        }
        let prefix = self.prefix.swap(0, Ordering::Relaxed);
        if prefix == 0xE1 {
            return; // Pause: not reported
        }
        let (code, down) = ((b & 0x7F) as u16, b & 0x80 == 0);
        let key = if prefix == 0xE0 {
            match extended_key(code) {
                Some(k) => k,
                None => return,
            }
        } else {
            code
        };
        if key == 0 {
            return;
        }
        let held = irqless(|| self.keys.lock()[key as usize / 64] & (1 << (key % 64)) != 0);
        // Linux reports autorepeat as value 2.
        self.emit(
            EV_KEY,
            key,
            if !down {
                0
            } else if held {
                2
            } else {
                1
            },
        );
        self.sync();
    }

    pub fn set_led_sink(&self, f: LedSink) {
        irqless(|| *self.led_sink.lock() = Some(Arc::new(f)));
        let c = CONSOLE_LEDS.load(Ordering::Relaxed);
        if c != u32::MAX {
            self.set_leds(c as u8);
        }
    }

    /// Set the lock LEDs (bit 0 num, 1 caps, 2 scroll).
    pub fn set_leds(&self, bits: u8) {
        let old = self.leds.swap(bits & 7, Ordering::Relaxed);
        if let Some(sink) = irqless(|| self.led_sink.lock().clone()) {
            sink(bits & 7);
        }
        if old != bits & 7 {
            for l in 0..3u16 {
                if (old ^ bits) & (1 << l) != 0 {
                    self.emit(EV_LED, l, ((bits >> l) & 1) as i32);
                }
            }
            self.sync();
        }
    }

    pub fn info(&self) -> Info {
        irqless(|| self.info.lock().clone())
    }
}

/// Key codes for 0xE0-prefixed set-1 scancodes.
fn extended_key(c: u16) -> Option<u16> {
    Some(match c {
        0x1C => 96,                 // KPENTER
        0x1D => 97,                 // RIGHTCTRL
        0x35 => 98,                 // KPSLASH
        0x37 => 99,                 // SYSRQ
        0x38 => 100,                // RIGHTALT
        0x47 => 102,                // HOME
        0x48 => 103,                // UP
        0x49 => 104,                // PAGEUP
        0x4B => 105,                // LEFT
        0x4D => 106,                // RIGHT
        0x4F => 107,                // END
        0x50 => 108,                // DOWN
        0x51 => 109,                // PAGEDOWN
        0x52 => 110,                // INSERT
        0x53 => 111,                // DELETE
        0x5B => 125,                // LEFTMETA
        0x5C => 126,                // RIGHTMETA
        0x5D => 127,                // COMPOSE
        0x5E => 116,                // POWER
        0x20 => 113,                // MUTE
        0x2E => 114,                // VOLUMEDOWN
        0x30 => 115,                // VOLUMEUP
        0x22 => 164,                // PLAYPAUSE
        0x24 => 166,                // STOPCD
        0x19 => 163,                // NEXTSONG
        0x10 => 165,                // PREVIOUSSONG
        0x2A | 0x36 => return None, // fake shifts
        _ => return None,
    })
}

/// PS/2 set-1 scancode bytes for Linux key `code` (make, or break with
/// `down` false), as the console keyboard takes them; None for keys it
/// has no code for.
pub fn keycode_scancodes(code: u16, down: bool) -> Option<([u8; 2], usize)> {
    let brk = if down { 0 } else { 0x80 };
    let plain = match code {
        1..=0x53 | 0x56..=0x58 => Some(code as u8),
        _ => None,
    };
    if let Some(sc) = plain {
        return Some(([sc | brk, 0], 1));
    }
    (0x01u16..0x80)
        .find(|&sc| extended_key(sc) == Some(code))
        .map(|sc| ([0xE0, sc as u8 | brk], 2))
}

/// The console's lock keys changed: update every keyboard's LEDs.
pub fn console_leds(bits: u8) {
    if CONSOLE_LEDS.swap(bits as u32, Ordering::Relaxed) == bits as u32 {
        return;
    }
    let devs: Vec<Arc<InputDev>> = irqless(|| DEVICES.lock().clone());
    for d in devs {
        if !irqless(|| d.info.lock().leds.is_empty()) {
            d.set_leds(bits);
        }
    }
}

/// Queue one event on /dev/input/event0 only (devices without a node).
pub fn emit(kind: u16, code: u16, value: i32) {
    push_to(
        &EV_CLIENTS,
        Event {
            time_ns: crate::time::realtime_nanos(),
            kind,
            code,
            value,
        },
    );
}

/// End of one report on /dev/input/event0.
pub fn sync() {
    emit(EV_SYN, 0, 0);
    WQ.wake_all();
}

/// Joystick event: `button` (true) or axis `number` with `value`.
pub fn js_emit(button: bool, number: u8, value: i16) {
    let time_ms = crate::time::millis() as u32;
    push_to(
        &JS_CLIENTS,
        JsEvent {
            time_ms,
            value,
            kind: if button { 1 } else { 2 },
            number,
        },
    );
    WQ.wake_all();
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
            monotonic: AtomicBool::new(false),
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
        enc: impl Fn(&T, &mut [u8], bool),
    ) -> KResult<usize> {
        if buf.len() < size {
            return Err(EINVAL);
        }
        let mono = self.monotonic.load(Ordering::Relaxed);
        loop {
            let mut n = 0;
            while n + size <= buf.len() {
                let Some(e) = irqless(|| self.queue.lock().pop_front()) else {
                    break;
                };
                enc(&e, &mut buf[n..n + size], mono);
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

fn encode_event(e: &Event, b: &mut [u8], mono: bool) {
    let t = if mono {
        // Shift wall-clock stamps onto the monotonic clock.
        let off = crate::time::realtime_nanos().saturating_sub(crate::time::nanos());
        e.time_ns.saturating_sub(off)
    } else {
        e.time_ns
    };
    b[0..8].copy_from_slice(&((t / 1_000_000_000) as i64).to_le_bytes());
    b[8..16].copy_from_slice(&((t % 1_000_000_000 / 1000) as i64).to_le_bytes());
    b[16..18].copy_from_slice(&e.kind.to_le_bytes());
    b[18..20].copy_from_slice(&e.code.to_le_bytes());
    b[20..24].copy_from_slice(&e.value.to_le_bytes());
}

fn encode_js(e: &JsEvent, b: &mut [u8], _mono: bool) {
    b[0..4].copy_from_slice(&e.time_ms.to_le_bytes());
    b[4..6].copy_from_slice(&e.value.to_le_bytes());
    b[6] = e.kind;
    b[7] = e.number;
}

/// A bitmap of `bits`, `len` bytes long.
fn bitmap(bits: impl Iterator<Item = u16>, len: usize) -> Vec<u8> {
    let mut v = alloc::vec![0u8; len];
    for b in bits {
        if (b as usize) / 8 < len {
            v[b as usize / 8] |= 1 << (b % 8);
        }
    }
    v
}

/// Copy `data` out (at most `size` bytes); returns the count, as Linux.
fn out(arg: u64, size: usize, data: &[u8]) -> KResult<i64> {
    let n = data.len().min(size);
    uaccess::copy_to_user(arg, &data[..n])?;
    Ok(n as i64)
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// The merged description event0 gives: every device's capabilities.
fn merged_info() -> Info {
    let mut i = Info::new("RustOS input (all devices)", "", [BUS_VIRTUAL, 0, 0, 1]);
    for d in irqless(|| DEVICES.lock().clone()) {
        let x = d.info();
        i.keys.extend(x.keys);
        i.rel.extend(x.rel);
        i.abs.extend(x.abs);
        i.leds.extend(x.leds);
        i.props |= x.props;
    }
    i.keys.sort_unstable();
    i.keys.dedup();
    i.rel.sort_unstable();
    i.rel.dedup();
    i.abs.sort_by_key(|a| a.0);
    i.abs.dedup_by_key(|a| a.0);
    i.leds.sort_unstable();
    i.leds.dedup();
    i
}

/// An open evdev file: a device's (or, for event0, every device's)
/// events.
struct EventFile {
    dev: Option<Arc<InputDev>>,
    client: Arc<Client<Event>>,
}

impl EventFile {
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        let size = ((cmd >> 16) & 0x3FFF) as usize;
        let dir = (cmd >> 30) & 3;
        if (cmd >> 8) & 0xFF != b'E' as u64 {
            return Err(ENOTTY);
        }
        let nr = (cmd & 0xFF) as u16;
        let info = match &self.dev {
            Some(d) => d.info(),
            None => merged_info(),
        };
        let keys_down: Vec<u16> = match &self.dev {
            Some(d) => {
                let k = irqless(|| *d.keys.lock());
                (0..=KEY_MAX as u16)
                    .filter(|&c| k[c as usize / 64] & (1 << (c % 64)) != 0)
                    .collect()
            }
            None => Vec::new(),
        };
        let leds = self
            .dev
            .as_ref()
            .map_or(0, |d| d.leds.load(Ordering::Relaxed));
        match nr {
            0x01 => out(arg, 4, &0x0001_0001i32.to_le_bytes()),
            0x02 => {
                let mut b = [0u8; 8];
                for (i, v) in info.id.iter().enumerate() {
                    b[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                }
                out(arg, 8, &b)
            }
            0x03 => {
                let mut b = [0u8; 8];
                b[0..4].copy_from_slice(&500u32.to_le_bytes());
                b[4..8].copy_from_slice(&33u32.to_le_bytes());
                out(arg, 8, &b)
            }
            0x06 => out(arg, size, &cstr(&info.name)),
            0x07 => out(arg, size, &cstr(&info.phys)),
            0x08 => out(arg, size, &cstr(&info.uniq)),
            0x09 => out(
                arg,
                size,
                &bitmap((0..32).filter(|b| info.props & (1 << b) != 0), 4),
            ),
            0x18 => out(arg, size, &bitmap(keys_down.into_iter(), KEY_MAX / 8 + 1)),
            0x19 => out(
                arg,
                size,
                &bitmap((0..3).filter(|l| leds & (1 << l) != 0), 1),
            ),
            0x1A | 0x1B => out(arg, size, &[0u8; 8]),
            0x20..=0x3F => {
                let ev = nr - 0x20;
                let bm = match ev {
                    0 => bitmap((0..32).filter(|b| info.ev_bits() & (1 << b) != 0), 4),
                    EV_KEY => bitmap(info.keys.iter().copied(), KEY_MAX / 8 + 1),
                    EV_REL => bitmap(info.rel.iter().copied(), 2),
                    EV_ABS => bitmap(info.abs.iter().map(|a| a.0), 8),
                    EV_LED => bitmap(info.leds.iter().copied(), 2),
                    EV_REP if !info.leds.is_empty() => bitmap([0u16, 1].into_iter(), 1),
                    _ => Vec::new(),
                };
                out(arg, size, &bm)
            }
            0x40..=0x7F if dir == 2 => {
                let a = info
                    .abs
                    .iter()
                    .find(|a| a.0 == nr - 0x40)
                    .map(|a| a.1)
                    .ok_or(EINVAL)?;
                let mut b = [0u8; 24];
                for (i, v) in [a.value, a.min, a.max, a.fuzz, a.flat, a.res]
                    .iter()
                    .enumerate()
                {
                    b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                out(arg, 24, &b)
            }
            0xC0..=0xFF if dir == 1 => {
                let d = self.dev.as_ref().ok_or(EINVAL)?;
                let mut b = [0u8; 24];
                uaccess::copy_from_user(&mut b[..size.min(24)], arg)?;
                let g = |i: usize| i32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
                let code = nr - 0xC0;
                irqless(|| {
                    let mut inf = d.info.lock();
                    match inf.abs.iter_mut().find(|a| a.0 == code) {
                        Some(a) => {
                            a.1 = AbsInfo {
                                value: g(0),
                                min: g(1),
                                max: g(2),
                                fuzz: g(3),
                                flat: g(4),
                                res: g(5),
                            };
                            Ok(0)
                        }
                        None => Err(EINVAL),
                    }
                })
            }
            0x90 => {
                let d = self.dev.as_ref().ok_or(EINVAL)?;
                irqless(|| {
                    let mut g = d.grab.lock();
                    let mine = g
                        .as_ref()
                        .and_then(|w| w.upgrade())
                        .map(|c| Arc::ptr_eq(&c, &self.client));
                    if arg != 0 {
                        match mine {
                            Some(false) => Err(EBUSY),
                            _ => {
                                *g = Some(Arc::downgrade(&self.client));
                                Ok(0)
                            }
                        }
                    } else if mine == Some(true) {
                        *g = None;
                        Ok(0)
                    } else {
                        Err(EINVAL)
                    }
                })
            }
            0x91 => Ok(0), // EVIOCREVOKE
            0xA0 => {
                let mut b = [0u8; 4];
                uaccess::copy_from_user(&mut b, arg)?;
                match i32::from_le_bytes(b) {
                    0 => self.client.monotonic.store(false, Ordering::Relaxed),
                    1 | 7 => self.client.monotonic.store(true, Ordering::Relaxed),
                    _ => return Err(EINVAL),
                }
                Ok(0)
            }
            _ => Err(EINVAL),
        }
    }
}

impl crate::vfs::FileLike for EventFile {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        self.client.read(buf, nonblock, 24, encode_event)
    }
    /// EV_LED events set a keyboard's lock LEDs; others are ignored.
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        if let Some(d) = &self.dev {
            let mut leds = d.leds.load(Ordering::Relaxed);
            let mut changed = false;
            for e in b.chunks_exact(24) {
                let kind = u16::from_le_bytes([e[16], e[17]]);
                let code = u16::from_le_bytes([e[18], e[19]]);
                let value = i32::from_le_bytes(e[20..24].try_into().unwrap());
                if kind == EV_LED && code < 3 {
                    if value != 0 {
                        leds |= 1 << code;
                    } else {
                        leds &= !(1 << code);
                    }
                    changed = true;
                }
            }
            if changed {
                d.set_leds(leds);
            }
        }
        Ok(b.len())
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        EventFile::ioctl(self, cmd, arg)
    }
    fn wait_queue(&self) -> &WaitQueue {
        &WQ
    }
    fn poll(&self) -> u16 {
        if self.client.is_empty() {
            0
        } else {
            crate::vfs::POLLIN
        }
    }
    fn close(&self) {
        if let Some(d) = &self.dev {
            irqless(|| {
                let mut g = d.grab.lock();
                if g.as_ref()
                    .and_then(|w| w.upgrade())
                    .is_some_and(|c| Arc::ptr_eq(&c, &self.client))
                {
                    *g = None;
                }
            });
        }
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

struct JsFile(Arc<Client<JsEvent>>);

impl crate::vfs::FileLike for JsFile {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        self.0.read(buf, nonblock, 8, encode_js)
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    /// JSIOCGVERSION, JSIOCGAXES, JSIOCGBUTTONS, JSIOCGNAME for the first
    /// game device.
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        if (cmd >> 8) & 0xFF != b'j' as u64 {
            return Err(ENOTTY);
        }
        let size = ((cmd >> 16) & 0x3FFF) as usize;
        let game = irqless(|| DEVICES.lock().clone())
            .into_iter()
            .map(|d| d.info())
            .find(|i| i.game.is_some());
        let (axes, buttons) = game.as_ref().and_then(|g| g.game).unwrap_or((0, 0));
        match cmd & 0xFF {
            0x01 => out(arg, 4, &0x0002_0100u32.to_le_bytes()),
            0x11 => out(arg, 1, &[axes]),
            0x12 => out(arg, 1, &[buttons]),
            0x13 => out(
                arg,
                size,
                &cstr(game.as_ref().map_or("RustOS joystick", |g| &g.name)),
            ),
            _ => Err(EINVAL),
        }
    }
    fn wait_queue(&self) -> &WaitQueue {
        &WQ
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

/// A /dev/input/event* node: each open gets its own event queue.
struct EventNode(Option<Arc<InputDev>>);

impl crate::vfs::FileLike for EventNode {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    fn open_instance(&self, _flags: u32) -> KResult<Option<Arc<dyn crate::vfs::FileLike>>> {
        let client = match &self.0 {
            Some(d) => Client::new(&d.clients),
            None => Client::new(&EV_CLIENTS),
        };
        Ok(Some(Arc::new(EventFile {
            dev: self.0.clone(),
            client,
        })))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// /dev/input/event0: all devices merged.
pub fn event0() -> Arc<dyn crate::vfs::FileLike> {
    Arc::new(EventNode(None))
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

/// Info for a HID device from its report descriptor.
pub fn touch_props(direct: bool) -> u32 {
    if direct {
        1 << INPUT_PROP_DIRECT
    } else {
        1 << INPUT_PROP_POINTER
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
