//! USB HID: keyboards, mice, absolute pointers (tablets, touch screens)
//! and game controllers.
//!
//! The report descriptor is fetched and parsed (`usb_desc::hid`), and the
//! device is driven in report protocol: keyboards (including NKRO bitmaps
//! and media keys) produce PS/2 set-1 scancodes for the console decoder;
//! pointers feed `/dev/input/mice` and `/dev/input/event0`; game pads and
//! joysticks feed `/dev/input/js0` and `/dev/input/event0` (see
//! `drivers::input`). Boot-protocol keyboards and mice whose descriptor
//! cannot be parsed fall back to the boot protocol.

use super::UsbDevice;
use crate::drivers::input::{self, EV_ABS, EV_KEY, EV_REL};
use crate::drivers::mouse::{self, MouseEvent};
use crate::mm::dma::DmaBuffer;
use alloc::sync::Arc;
use alloc::vec::Vec;
use usb_desc::hid::{self, Decoded, ReportDescriptor, usage};
use usb_desc::{CLASS_HID, Interface, KeyboardState, MouseReport, TransferType};

const GET_DESCRIPTOR: u8 = 0x06;
const SET_IDLE: u8 = 0x0A;
const SET_PROTOCOL: u8 = 0x0B;
const DT_REPORT: u16 = 0x22;
const REPEAT_DELAY_MS: u64 = 500;
const REPEAT_RATE_MS: u64 = 33;

const GD: u16 = hid::PAGE_DESKTOP;

enum Mode {
    BootKeyboard,
    BootMouse,
    Report(ReportDescriptor),
}

pub fn probe(dev: &Arc<UsbDevice>, iface: &Interface) -> bool {
    if iface.class != CLASS_HID {
        return false;
    }
    let Some(ep) = iface.find_endpoint(TransferType::Interrupt, true) else {
        return false;
    };
    let boot = iface.subclass == 1 && matches!(iface.protocol, 1 | 2);
    let ifn = iface.number as u16;
    let parsed = dev
        .control_in(0x01, GET_DESCRIPTOR, DT_REPORT << 8, ifn, 1024)
        .ok()
        .and_then(|d| match ReportDescriptor::parse(&d) {
            Ok(r) => Some(r),
            Err(e) => {
                crate::println!("[usb] {}: bad HID report descriptor ({:?})", dev.name(), e);
                None
            }
        })
        .filter(|r| r.fields.iter().any(|f| !f.is_constant()));
    let mode = match parsed {
        Some(r) => Mode::Report(r),
        None if boot && iface.protocol == 1 => Mode::BootKeyboard,
        None if boot => Mode::BootMouse,
        None => return false,
    };
    if dev.configure_endpoints(&[ep]).is_err() {
        return false;
    }
    if boot {
        let proto = if matches!(mode, Mode::Report(_)) {
            1
        } else {
            0
        };
        let _ = dev.control_out(0x21, SET_PROTOCOL, proto, ifn, &[]);
    }
    let keyboard = match &mode {
        Mode::BootKeyboard => true,
        Mode::BootMouse => false,
        Mode::Report(r) => r.has_application(usage(GD, hid::USAGE_KEYBOARD)),
    };
    // Keyboards resend their report every 32 ms while keys are held so we
    // can implement typematic repeat; other devices report only changes.
    let idle = if keyboard { 8 } else { 0 };
    let _ = dev.control_out(0x21, SET_IDLE, idle << 8, ifn, &[]);
    let what = match &mode {
        Mode::BootKeyboard => alloc::string::String::from("boot keyboard"),
        Mode::BootMouse => alloc::string::String::from("boot mouse"),
        Mode::Report(r) => describe(r),
    };
    crate::println!("[usb] {}: HID {}", dev.name(), what);
    let d = dev.clone();
    crate::sched::spawn(&alloc::format!("usb-hid{}", dev.slot), move || {
        let Some(buf) = DmaBuffer::new(64.max(ep.packet_size() as usize)) else {
            return;
        };
        let len = (ep.packet_size() as usize).clamp(3, buf.len());
        match mode {
            Mode::BootKeyboard => keyboard_loop(&d, &ep, &buf, len),
            Mode::BootMouse => mouse_loop(&d, &ep, &buf, len),
            Mode::Report(r) => report_loop(&d, &ep, &buf, len, r),
        }
    });
    true
}

fn describe(r: &ReportDescriptor) -> alloc::string::String {
    let mut parts: Vec<&str> = Vec::new();
    for &a in &r.applications {
        let name = match ((a >> 16) as u16, a as u16) {
            (GD, hid::USAGE_KEYBOARD) => "keyboard",
            (GD, hid::USAGE_MOUSE) | (GD, hid::USAGE_POINTER) => {
                if r.fields.iter().any(|f| {
                    f.application == a
                        && !f.is_relative()
                        && f.usage_of(0) == Some(usage(GD, hid::USAGE_X))
                }) {
                    "tablet"
                } else {
                    "mouse"
                }
            }
            (GD, hid::USAGE_GAMEPAD) => "game pad",
            (GD, hid::USAGE_JOYSTICK) => "joystick",
            (hid::PAGE_CONSUMER, 1) => "media keys",
            (hid::PAGE_DIGITIZER, _) => "touch screen",
            _ => continue,
        };
        if !parts.contains(&name) {
            parts.push(name);
        }
    }
    if parts.is_empty() {
        parts.push("device");
    }
    alloc::format!("{} (report protocol)", parts.join(" + "))
}

fn emit(codes: &[u8]) {
    for &c in codes {
        crate::task::keyboard::add_scancode(c);
    }
}

/// Typematic repeat for the last key pressed.
struct Repeat {
    held: Option<(u8, u64, u64)>,
}

impl Repeat {
    fn update(&mut self, state: &KeyboardState, pressed: &[u8], out: &mut Vec<u8>) {
        let now = crate::time::millis();
        if let Some(&k) = pressed.last() {
            self.held = Some((k, now, now));
        } else if let Some((k, since, last)) = self.held {
            if !state.is_held(k) {
                self.held = None;
            } else if now - since >= REPEAT_DELAY_MS && now - last >= REPEAT_RATE_MS {
                state.repeat(k, out);
                self.held = Some((k, since, now));
            }
        }
    }
}

/// Read reports until the device goes away; `f` handles each one.
fn read_loop(
    dev: &UsbDevice,
    ep: &usb_desc::Endpoint,
    buf: &DmaBuffer,
    len: usize,
    mut f: impl FnMut(&[u8]),
) {
    let mut errors = 0;
    while !dev.is_gone() {
        match dev.transfer(ep, buf, len, None) {
            Ok(n) => {
                errors = 0;
                f(&buf.as_slice()[..n]);
            }
            Err(crate::errno::ENODEV) => break,
            Err(_) => {
                errors += 1;
                if errors > 50 {
                    crate::println!("[usb] {}: HID device stopped responding", dev.name());
                    break;
                }
                crate::time::sleep_ms(20);
            }
        }
    }
}

fn keyboard_loop(dev: &UsbDevice, ep: &usb_desc::Endpoint, buf: &DmaBuffer, len: usize) {
    let mut state = KeyboardState::default();
    let mut rep = Repeat { held: None };
    let mut out = Vec::new();
    read_loop(dev, ep, buf, len, |report| {
        if report.len() < 8 {
            return;
        }
        out.clear();
        let pressed = state.update(&report[..8], &mut out);
        rep.update(&state, &pressed, &mut out);
        emit(&out);
    });
    out.clear();
    state.release_all(&mut out);
    emit(&out);
}

fn mouse_loop(dev: &UsbDevice, ep: &usb_desc::Endpoint, buf: &DmaBuffer, len: usize) {
    read_loop(dev, ep, buf, len, |report| {
        if let Some(r) = MouseReport::parse(report) {
            mouse::push(MouseEvent {
                dx: r.dx as i32,
                dy: r.dy as i32,
                wheel: r.wheel as i32,
                buttons: r.buttons,
            });
            input::emit(EV_REL, input::REL_X, r.dx as i32);
            input::emit(EV_REL, input::REL_Y, r.dy as i32);
            input::sync();
        }
    });
}

/// Per-device state for report-protocol devices.
#[derive(Default)]
struct ReportState {
    keyboard: KeyboardState,
    repeat: Option<Repeat>,
    buttons: u32,
    /// Last absolute pointer position (virtual screen pixels).
    abs_pos: Option<(i32, i32)>,
    /// Last absolute axis values (usage, value), to report changes only.
    axes: Vec<(u32, i32)>,
}

/// Virtual screen for absolute pointers feeding /dev/input/mice.
const VSCREEN: (i32, i32) = (1920, 1080);

fn scale(v: i32, lo: i32, hi: i32, span: i32) -> i32 {
    if hi <= lo {
        return 0;
    }
    ((v.clamp(lo, hi) - lo) as i64 * (span - 1) as i64 / (hi - lo) as i64) as i32
}

fn abs_code(u: u32) -> Option<u16> {
    if (u >> 16) as u16 != GD {
        return None;
    }
    Some(match u as u16 {
        hid::USAGE_X => input::ABS_X,
        hid::USAGE_Y => input::ABS_Y,
        hid::USAGE_Z => input::ABS_Z,
        hid::USAGE_RX => input::ABS_RX,
        hid::USAGE_RY => input::ABS_RY,
        hid::USAGE_RZ => input::ABS_RZ,
        hid::USAGE_HAT => input::ABS_HAT0X,
        _ => return None,
    })
}

fn report_loop(
    dev: &UsbDevice,
    ep: &usb_desc::Endpoint,
    buf: &DmaBuffer,
    len: usize,
    r: ReportDescriptor,
) {
    let mut st = ReportState::default();
    let game = r.has_application(usage(GD, hid::USAGE_GAMEPAD))
        || r.has_application(usage(GD, hid::USAGE_JOYSTICK));
    let btn_base = if r.has_application(usage(GD, hid::USAGE_GAMEPAD)) {
        input::BTN_GAMEPAD
    } else if game {
        input::BTN_JOYSTICK
    } else {
        input::BTN_MOUSE
    };
    let mut out = Vec::new();
    read_loop(dev, ep, buf, len, |report| {
        let d = r.decode(report);
        out.clear();
        handle(&r, &d, &mut st, &mut out, game, btn_base);
        emit(&out);
    });
    out.clear();
    st.keyboard.release_all(&mut out);
    emit(&out);
}

fn handle(
    r: &ReportDescriptor,
    d: &Decoded,
    st: &mut ReportState,
    out: &mut Vec<u8>,
    game: bool,
    btn_base: u16,
) {
    // Which applications this report belongs to decides what it drives.
    let apps: Vec<u32> = r
        .fields
        .iter()
        .filter(|f| f.report_id == d.report_id)
        .map(|f| f.application)
        .collect();
    let has = |u: u32| apps.contains(&u);
    if has(usage(GD, hid::USAGE_KEYBOARD)) {
        let pressed = st.keyboard.update_usages(&d.keys, out);
        st.repeat
            .get_or_insert(Repeat { held: None })
            .update(&st.keyboard, &pressed, out);
    }
    if has(usage(hid::PAGE_CONSUMER, 1)) {
        st.keyboard.update_consumer(&d.consumer, out);
    }
    let pointer = has(usage(GD, hid::USAGE_MOUSE))
        || has(usage(GD, hid::USAGE_POINTER))
        || apps.iter().any(|a| (a >> 16) as u16 == hid::PAGE_DIGITIZER);
    if !pointer && !game {
        return;
    }
    let mut any = false;
    // Buttons.
    let changed = st.buttons ^ d.buttons;
    for b in 0..32 {
        if changed & (1 << b) != 0 {
            let down = d.buttons & (1 << b) != 0;
            input::emit(EV_KEY, btn_base + b as u16, down as i32);
            if game {
                input::js_emit(true, b as u8, down as i16);
            }
            any = true;
        }
    }
    st.buttons = d.buttons;
    // Relative axes.
    let dx = d.rel_of(usage(GD, hid::USAGE_X));
    let dy = d.rel_of(usage(GD, hid::USAGE_Y));
    let wheel = d.rel_of(usage(GD, hid::USAGE_WHEEL));
    let pan = d.rel_of(usage(hid::PAGE_CONSUMER, hid::USAGE_AC_PAN));
    for (code, v) in [
        (input::REL_X, dx),
        (input::REL_Y, dy),
        (input::REL_WHEEL, wheel),
        (input::REL_HWHEEL, pan),
    ] {
        if v != 0 {
            input::emit(EV_REL, code, v);
            any = true;
        }
    }
    // Absolute axes.
    for (i, &(u, v, lo, hi)) in d.abs.iter().enumerate() {
        let Some(code) = abs_code(u) else { continue };
        let prev = st.axes.iter_mut().find(|(pu, _)| *pu == u);
        match prev {
            Some((_, pv)) if *pv == v => continue,
            Some((_, pv)) => *pv = v,
            None => st.axes.push((u, v)),
        }
        input::emit(EV_ABS, code, v);
        if game {
            input::js_emit(false, i as u8, input::js_scale(v, lo, hi));
        }
        any = true;
    }
    if any {
        input::sync();
    }
    if pointer {
        // Absolute pointers move the /dev/input/mice pointer too.
        let (mut mdx, mut mdy) = (dx, dy);
        if let (Some((x, xl, xh)), Some((y, yl, yh))) = (
            d.abs_of(usage(GD, hid::USAGE_X)),
            d.abs_of(usage(GD, hid::USAGE_Y)),
        ) {
            let p = (scale(x, xl, xh, VSCREEN.0), scale(y, yl, yh, VSCREEN.1));
            if let Some((ox, oy)) = st.abs_pos {
                mdx += p.0 - ox;
                mdy += p.1 - oy;
            }
            st.abs_pos = Some(p);
        }
        if mdx != 0 || mdy != 0 || wheel != 0 || changed != 0 {
            mouse::push(MouseEvent {
                dx: mdx,
                dy: mdy,
                wheel,
                buttons: (d.buttons & 7) as u8,
            });
        }
    }
}
