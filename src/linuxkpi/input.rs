//! Linux input devices (src/linuxkpi/c/input.c) as RustOS input devices:
//! each gets an evdev node with the Linux device's capabilities; keyboards
//! also type into the console, and get the console's lock-key LEDs.

use crate::drivers::input::{self, AbsInfo, Info, InputDev};
use crate::sync::IrqMutex;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char, c_void};
use core::sync::atomic::{AtomicU64, Ordering};

unsafe extern "C" {
    fn kpi_input_set_leds(handle: *mut c_void, bits: u32);
}

const EV_KEY: u32 = 0x01;
const EV_SYN: u32 = 0x00;
const KEY_CNT: usize = 0x300;
const REL_CNT: usize = 0x10;
const ABS_CNT: usize = 0x40;
const LED_CNT: usize = 0x10;
const KEY_A: u16 = 30;
const KEY_Z: u16 = 44;

/// `struct kpi_input_caps` in c/input.c.
#[repr(C)]
struct Caps {
    key: *const u64,
    rel: *const u64,
    abs: *const u64,
    led: *const u64,
    prop: *const u64,
    absinfo: *const LinuxAbsInfo,
    id: [u16; 4],
}

/// `struct input_absinfo`.
#[repr(C)]
#[derive(Clone, Copy)]
struct LinuxAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

fn bits(p: *const u64, n: usize) -> Vec<u16> {
    if p.is_null() {
        return Vec::new();
    }
    let words = unsafe { core::slice::from_raw_parts(p, n.div_ceil(64)) };
    (0..n)
        .filter(|&i| words[i / 64] & (1 << (i % 64)) != 0)
        .map(|i| i as u16)
        .collect()
}

struct Dev {
    dev: Arc<InputDev>,
    /// Types into the console (has letter keys).
    keyboard: bool,
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static DEVS: IrqMutex<BTreeMap<u64, Dev>> = IrqMutex::new(BTreeMap::new());

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_input_add(
    name: *const c_char,
    phys: *const c_char,
    caps: *const Caps,
    handle: *mut c_void,
) -> u64 {
    let caps = unsafe { &*caps };
    let name = String::from(unsafe { CStr::from_ptr(name) }.to_string_lossy());
    let phys = String::from(unsafe { CStr::from_ptr(phys) }.to_string_lossy());
    let mut info = Info::new(&name, &phys, caps.id);
    info.keys = bits(caps.key, KEY_CNT);
    info.rel = bits(caps.rel, REL_CNT);
    info.leds = bits(caps.led, LED_CNT);
    info.props = bits(caps.prop, 32).iter().fold(0, |a, &b| a | (1 << b));
    for code in bits(caps.abs, ABS_CNT) {
        let a = if caps.absinfo.is_null() {
            LinuxAbsInfo {
                value: 0,
                minimum: 0,
                maximum: 0,
                fuzz: 0,
                flat: 0,
                resolution: 0,
            }
        } else {
            unsafe { *caps.absinfo.add(code as usize) }
        };
        info.abs.push((
            code,
            AbsInfo {
                value: a.value,
                min: a.minimum,
                max: a.maximum,
                fuzz: a.fuzz,
                flat: a.flat,
                res: a.resolution,
            },
        ));
    }
    let keyboard = (KEY_A..=KEY_Z).all(|k| info.keys.contains(&k));
    let has_leds = !info.leds.is_empty();
    let dev = input::register(info);
    if has_leds {
        let h = handle as usize;
        dev.set_led_sink(alloc::boxed::Box::new(move |b: u8| unsafe {
            kpi_input_set_leds(h as *mut c_void, b as u32)
        }));
    }
    let rid = NEXT.fetch_add(1, Ordering::SeqCst);
    DEVS.lock().insert(rid, Dev { dev, keyboard });
    rid
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_input_remove(rid: u64) {
    if let Some(d) = DEVS.lock().remove(&rid) {
        input::unregister(&d.dev);
    }
}

/// One event (from the device's event lock, interrupts off).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_input_event(rid: u64, kind: u32, code: u32, value: i32) {
    let Some((dev, keyboard)) = DEVS.lock().get(&rid).map(|d| (d.dev.clone(), d.keyboard)) else {
        return;
    };
    if kind == EV_SYN && code == 0 {
        dev.sync();
        return;
    }
    dev.emit(kind as u16, code as u16, value);
    if keyboard
        && kind == EV_KEY
        && let Some((sc, n)) = input::keycode_scancodes(code as u16, value != 0)
    {
        for &b in &sc[..n] {
            crate::task::keyboard::add_scancode(b);
        }
    }
}
