//! HID boot-protocol keyboards and mice.
//!
//! Keyboard reports are translated to PS/2 set-1 scancodes and fed to the
//! same decoder as the PS/2 keyboard; mouse reports go to the shared
//! pointer queue (/dev/input/mice).

use super::UsbDevice;
use crate::drivers::mouse::{self, MouseEvent};
use crate::mm::dma::DmaBuffer;
use alloc::sync::Arc;
use alloc::vec::Vec;
use usb_desc::{CLASS_HID, Interface, KeyboardState, MouseReport, TransferType};

const SET_IDLE: u8 = 0x0A;
const SET_PROTOCOL: u8 = 0x0B;
const REPEAT_DELAY_MS: u64 = 500;
const REPEAT_RATE_MS: u64 = 33;

pub fn probe(dev: &Arc<UsbDevice>, iface: &Interface) -> bool {
    if iface.class != CLASS_HID || iface.subclass != 1 || !matches!(iface.protocol, 1 | 2) {
        return false;
    }
    let Some(ep) = iface.find_endpoint(TransferType::Interrupt, true) else {
        return false;
    };
    if dev.configure_endpoints(&[ep]).is_err() {
        return false;
    }
    let keyboard = iface.protocol == 1;
    let _ = dev.control_out(0x21, SET_PROTOCOL, 0, iface.number as u16, &[]);
    // Keyboards resend their report every 32 ms while keys are held so we
    // can implement typematic repeat; mice report only on change.
    let idle = if keyboard { 8 } else { 0 };
    let _ = dev.control_out(0x21, SET_IDLE, idle << 8, iface.number as u16, &[]);
    crate::println!(
        "[usb] {}: HID boot {}",
        dev.name(),
        if keyboard { "keyboard" } else { "mouse" }
    );
    let d = dev.clone();
    crate::sched::spawn(&alloc::format!("usb-hid{}", dev.slot), move || {
        let Some(buf) = DmaBuffer::new(64) else {
            return;
        };
        let len = (ep.packet_size() as usize).clamp(3, 64);
        if keyboard {
            keyboard_loop(&d, &ep, &buf, len);
        } else {
            mouse_loop(&d, &ep, &buf, len);
        }
    });
    true
}

fn emit(codes: &[u8]) {
    for &c in codes {
        crate::task::keyboard::add_scancode(c);
    }
}

fn keyboard_loop(dev: &UsbDevice, ep: &usb_desc::Endpoint, buf: &DmaBuffer, len: usize) {
    let mut state = KeyboardState::default();
    let mut out = Vec::new();
    // (usage, pressed at, last repeat)
    let mut held: Option<(u8, u64, u64)> = None;
    let mut errors = 0;
    while !dev.is_gone() {
        match dev.transfer(ep, buf, len, None) {
            Ok(n) if n >= 8 => {
                errors = 0;
                let report = &buf.as_slice()[..8];
                out.clear();
                let pressed = state.update(report, &mut out);
                let now = crate::time::millis();
                if let Some(&k) = pressed.last() {
                    held = Some((k, now, now));
                } else if let Some((k, since, last)) = held {
                    if !report[2..8].contains(&k) {
                        held = None;
                    } else if now - since >= REPEAT_DELAY_MS && now - last >= REPEAT_RATE_MS {
                        state.repeat(k, &mut out);
                        held = Some((k, since, now));
                    }
                }
                emit(&out);
            }
            Ok(_) => {}
            Err(crate::errno::ENODEV) => break,
            Err(_) => {
                errors += 1;
                if errors > 50 {
                    crate::println!("[usb] {}: keyboard stopped responding", dev.name());
                    break;
                }
                crate::time::sleep_ms(20);
            }
        }
    }
    out.clear();
    state.release_all(&mut out);
    emit(&out);
}

fn mouse_loop(dev: &UsbDevice, ep: &usb_desc::Endpoint, buf: &DmaBuffer, len: usize) {
    let mut errors = 0;
    while !dev.is_gone() {
        match dev.transfer(ep, buf, len, None) {
            Ok(n) => {
                errors = 0;
                if let Some(r) = MouseReport::parse(&buf.as_slice()[..n]) {
                    mouse::push(MouseEvent {
                        dx: r.dx as i32,
                        dy: r.dy as i32,
                        wheel: r.wheel as i32,
                        buttons: r.buttons,
                    });
                }
            }
            Err(crate::errno::ENODEV) => break,
            Err(_) => {
                errors += 1;
                if errors > 50 {
                    break;
                }
                crate::time::sleep_ms(20);
            }
        }
    }
}
