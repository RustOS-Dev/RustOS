//! USB descriptor parsing and HID boot-protocol translation.
//!
//! Pure data handling shared by the kernel's USB core; host-testable.

#![no_std]

extern crate alloc;

pub mod ncm;

use alloc::vec::Vec;

pub const DT_DEVICE: u8 = 1;
pub const DT_CONFIG: u8 = 2;
pub const DT_STRING: u8 = 3;
pub const DT_INTERFACE: u8 = 4;
pub const DT_ENDPOINT: u8 = 5;
pub const DT_HID: u8 = 0x21;
pub const DT_HUB: u8 = 0x29;
pub const DT_SS_HUB: u8 = 0x2A;
pub const DT_SS_EP_COMPANION: u8 = 0x30;

pub const CLASS_HID: u8 = 3;
pub const CLASS_MASS_STORAGE: u8 = 8;
pub const CLASS_HUB: u8 = 9;
pub const CLASS_CDC: u8 = 2;
pub const CLASS_CDC_DATA: u8 = 0x0A;
pub const CLASS_WIRELESS: u8 = 0xE0;
pub const CLASS_MISC: u8 = 0xEF;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub usb_version: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub device_version: u16,
    pub manufacturer_idx: u8,
    pub product_idx: u8,
    pub serial_idx: u8,
    pub num_configs: u8,
}

impl DeviceDescriptor {
    pub fn parse(b: &[u8]) -> Option<DeviceDescriptor> {
        if b.len() < 18 || b[1] != DT_DEVICE {
            return None;
        }
        Some(DeviceDescriptor {
            usb_version: u16::from_le_bytes([b[2], b[3]]),
            class: b[4],
            subclass: b[5],
            protocol: b[6],
            max_packet0: b[7],
            vendor: u16::from_le_bytes([b[8], b[9]]),
            product: u16::from_le_bytes([b[10], b[11]]),
            device_version: u16::from_le_bytes([b[12], b[13]]),
            manufacturer_idx: b[14],
            product_idx: b[15],
            serial_idx: b[16],
            num_configs: b[17],
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub address: u8,
    pub attributes: u8,
    pub max_packet: u16,
    pub interval: u8,
    /// SuperSpeed companion: max burst and attributes.
    pub max_burst: u8,
    pub ss_attributes: u8,
}

impl Endpoint {
    pub fn number(&self) -> u8 {
        self.address & 0x0F
    }
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
    pub fn transfer_type(&self) -> TransferType {
        match self.attributes & 3 {
            0 => TransferType::Control,
            1 => TransferType::Isochronous,
            2 => TransferType::Bulk,
            _ => TransferType::Interrupt,
        }
    }
    /// Max packet size without the high-bandwidth multiplier bits.
    pub fn packet_size(&self) -> u16 {
        self.max_packet & 0x7FF
    }
    /// Additional transactions per microframe (high-speed high-bandwidth).
    pub fn hs_mult(&self) -> u8 {
        ((self.max_packet >> 11) & 3) as u8
    }
    /// xHCI device context index.
    pub fn dci(&self) -> u8 {
        self.number() * 2 + self.is_in() as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub number: u8,
    pub alternate: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<Endpoint>,
    /// Class-specific descriptors following the interface (raw).
    pub extra: Vec<u8>,
}

impl Interface {
    pub fn find_endpoint(&self, tt: TransferType, dir_in: bool) -> Option<Endpoint> {
        self.endpoints
            .iter()
            .copied()
            .find(|e| e.transfer_type() == tt && e.is_in() == dir_in)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configuration {
    pub value: u8,
    pub attributes: u8,
    pub max_power_ma: u16,
    pub interfaces: Vec<Interface>,
}

impl Configuration {
    /// Parse a full configuration descriptor (with all sub-descriptors).
    pub fn parse(b: &[u8]) -> Option<Configuration> {
        if b.len() < 9 || b[1] != DT_CONFIG {
            return None;
        }
        let total = (u16::from_le_bytes([b[2], b[3]]) as usize).min(b.len());
        let mut cfg = Configuration {
            value: b[5],
            attributes: b[7],
            max_power_ma: b[8] as u16 * 2,
            interfaces: Vec::new(),
        };
        let mut o = b[0] as usize;
        while o + 2 <= total {
            let len = b[o] as usize;
            if len < 2 || o + len > total {
                break;
            }
            let d = &b[o..o + len];
            match d[1] {
                DT_INTERFACE if len >= 9 => cfg.interfaces.push(Interface {
                    number: d[2],
                    alternate: d[3],
                    class: d[5],
                    subclass: d[6],
                    protocol: d[7],
                    endpoints: Vec::new(),
                    extra: Vec::new(),
                }),
                DT_ENDPOINT if len >= 7 => {
                    if let Some(i) = cfg.interfaces.last_mut() {
                        i.endpoints.push(Endpoint {
                            address: d[2],
                            attributes: d[3],
                            max_packet: u16::from_le_bytes([d[4], d[5]]),
                            interval: d[6],
                            max_burst: 0,
                            ss_attributes: 0,
                        });
                    }
                }
                DT_SS_EP_COMPANION if len >= 6 => {
                    if let Some(e) = cfg.interfaces.last_mut().and_then(|i| i.endpoints.last_mut()) {
                        e.max_burst = d[2];
                        e.ss_attributes = d[3];
                    }
                }
                _ => {
                    if let Some(i) = cfg.interfaces.last_mut() {
                        i.extra.extend_from_slice(d);
                    }
                }
            }
            o += len;
        }
        Some(cfg)
    }

    /// Interfaces with alternate setting 0 (the ones active after
    /// SET_CONFIGURATION).
    pub fn default_interfaces(&self) -> impl Iterator<Item = &Interface> {
        self.interfaces.iter().filter(|i| i.alternate == 0)
    }
}

/// Decode a string descriptor (UTF-16LE) into `out`.
pub fn parse_string(b: &[u8], out: &mut alloc::string::String) -> bool {
    if b.len() < 2 || b[1] != DT_STRING {
        return false;
    }
    let len = (b[0] as usize).min(b.len());
    let units: Vec<u16> = b[2..len]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    out.clear();
    out.extend(char::decode_utf16(units).map(|c| c.unwrap_or('?')));
    true
}

/// Hub descriptor (USB 2 0x29 or USB 3 0x2A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HubDescriptor {
    pub ports: u8,
    pub characteristics: u16,
    /// Power-on to power-good time in ms.
    pub power_on_ms: u16,
}

impl HubDescriptor {
    pub fn parse(b: &[u8]) -> Option<HubDescriptor> {
        if b.len() < 7 || !matches!(b[1], DT_HUB | DT_SS_HUB) {
            return None;
        }
        Some(HubDescriptor {
            ports: b[2],
            characteristics: u16::from_le_bytes([b[3], b[4]]),
            power_on_ms: b[5] as u16 * 2,
        })
    }
    /// TT think time (USB 2 hubs), in xHCI TTT encoding.
    pub fn tt_think_time(&self) -> u8 {
        ((self.characteristics >> 5) & 3) as u8
    }
}

// ---------------------------------------------------------------------------
// HID boot protocol
// ---------------------------------------------------------------------------

/// Boot-protocol mouse report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseReport {
    pub buttons: u8,
    pub dx: i8,
    pub dy: i8,
    pub wheel: i8,
}

impl MouseReport {
    pub fn parse(b: &[u8]) -> Option<MouseReport> {
        if b.len() < 3 {
            return None;
        }
        Some(MouseReport {
            buttons: b[0] & 7,
            dx: b[1] as i8,
            dy: b[2] as i8,
            wheel: b.get(3).copied().unwrap_or(0) as i8,
        })
    }
}

/// A PS/2 set-1 scancode sequence for one key transition (up to 3 bytes:
/// optional 0xE0 prefix, make/break code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scancode {
    pub bytes: [u8; 2],
    pub len: usize,
}

/// Map a HID keyboard usage (page 7) to a PS/2 set-1 make code. Extended
/// keys return `(true, code)` meaning an 0xE0 prefix.
pub fn usage_to_set1(usage: u8) -> Option<(bool, u8)> {
    const MAIN: [u8; 0x66] = [
        0, 0, 0, 0, // 0x00-0x03: none / error codes
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, // a-l
        0x32, 0x31, 0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, // m-x
        0x15, 0x2C, // y z
        0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, // 1-0
        0x1C, 0x01, 0x0E, 0x0F, 0x39, // enter esc backspace tab space
        0x0C, 0x0D, 0x1A, 0x1B, 0x2B, 0x2B, 0x27, 0x28, 0x29, 0x33, 0x34, 0x35, // - = [ ] \ # ; ' ` , . /
        0x3A, // caps lock
        0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x57, 0x58, // F1-F12
        0, 0x46, 0, // printscreen (ext), scroll lock, pause
        0x52, 0x47, 0x49, 0x53, 0x4F, 0x51, // insert home pgup delete end pgdn (ext)
        0x4D, 0x4B, 0x50, 0x48, // right left down up (ext)
        0x45, // num lock
        0x35, 0x37, 0x4A, 0x4E, 0x1C, // kp / * - + enter
        0x4F, 0x50, 0x51, 0x4B, 0x4C, 0x4D, 0x47, 0x48, 0x49, 0x52, 0x53, // kp 1-9 0 .
        0x56, 0x5D, // non-US \, application (ext)
    ];
    let u = usage as usize;
    if u >= MAIN.len() || MAIN[u] == 0 {
        return None;
    }
    let ext = matches!(u, 0x49..=0x52 | 0x54 | 0x58 | 0x65);
    Some((ext, MAIN[u]))
}

/// Modifier bit (0-7) to set-1 code: LCtrl LShift LAlt LGui RCtrl RShift RAlt RGui.
pub fn modifier_to_set1(bit: u8) -> (bool, u8) {
    match bit {
        0 => (false, 0x1D),
        1 => (false, 0x2A),
        2 => (false, 0x38),
        3 => (true, 0x5B),
        4 => (true, 0x1D),
        5 => (false, 0x36),
        6 => (true, 0x38),
        _ => (true, 0x5C),
    }
}

fn push_code(out: &mut Vec<u8>, (ext, code): (bool, u8), down: bool) {
    if ext {
        out.push(0xE0);
    }
    out.push(if down { code } else { code | 0x80 });
}

/// Tracks the previous boot keyboard report and turns changes into PS/2
/// set-1 scancodes.
#[derive(Debug, Clone, Default)]
pub struct KeyboardState {
    mods: u8,
    keys: [u8; 6],
}

impl KeyboardState {
    /// Process an 8-byte boot report; appends scancodes to `out`. Returns
    /// the usages newly pressed (for typematic repeat tracking).
    pub fn update(&mut self, report: &[u8], out: &mut Vec<u8>) -> Vec<u8> {
        let mut pressed = Vec::new();
        if report.len() < 8 {
            return pressed;
        }
        // Rollover error: all keys report 0x01; ignore.
        if report[2..8].iter().all(|&k| k == 1) {
            return pressed;
        }
        let mods = report[0];
        for bit in 0..8 {
            let was = self.mods & (1 << bit) != 0;
            let now = mods & (1 << bit) != 0;
            if was != now {
                push_code(out, modifier_to_set1(bit), now);
            }
        }
        let mut keys = [0u8; 6];
        keys.copy_from_slice(&report[2..8]);
        for &k in self.keys.iter().filter(|&&k| k > 3) {
            if !keys.contains(&k) {
                if let Some(c) = usage_to_set1(k) {
                    push_code(out, c, false);
                }
            }
        }
        for &k in keys.iter().filter(|&&k| k > 3) {
            if !self.keys.contains(&k) {
                if let Some(c) = usage_to_set1(k) {
                    push_code(out, c, true);
                    pressed.push(k);
                }
            }
        }
        self.mods = mods;
        self.keys = keys;
        pressed
    }

    /// Make code for a held key (typematic repeat).
    pub fn repeat(&self, usage: u8, out: &mut Vec<u8>) -> bool {
        if !self.keys.contains(&usage) {
            return false;
        }
        match usage_to_set1(usage) {
            Some(c) => {
                push_code(out, c, true);
                true
            }
            None => false,
        }
    }

    /// Release everything (device unplugged).
    pub fn release_all(&mut self, out: &mut Vec<u8>) {
        let _ = self.update(&[0; 8], out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // QEMU usb-kbd style configuration descriptor.
    const KBD_CONFIG: [u8; 34] = [
        9, 2, 34, 0, 1, 1, 0, 0xA0, 50, // config
        9, 4, 0, 0, 1, 3, 1, 1, 0, // interface: HID boot keyboard
        9, 0x21, 0x11, 1, 0, 1, 0x22, 63, 0, // HID
        7, 5, 0x81, 3, 8, 0, 7, // EP1 IN interrupt
    ];

    #[test]
    fn parse_config() {
        let c = Configuration::parse(&KBD_CONFIG).unwrap();
        assert_eq!(c.value, 1);
        assert_eq!(c.max_power_ma, 100);
        assert_eq!(c.interfaces.len(), 1);
        let i = &c.interfaces[0];
        assert_eq!((i.class, i.subclass, i.protocol), (3, 1, 1));
        assert_eq!(i.extra.len(), 9);
        let e = i.find_endpoint(TransferType::Interrupt, true).unwrap();
        assert_eq!(e.dci(), 3);
        assert_eq!(e.packet_size(), 8);
    }

    #[test]
    fn parse_truncated_config() {
        assert!(Configuration::parse(&KBD_CONFIG[..5]).is_none());
        let c = Configuration::parse(&KBD_CONFIG[..20]).unwrap();
        assert_eq!(c.interfaces.len(), 1);
        assert!(c.interfaces[0].endpoints.is_empty());
    }

    #[test]
    fn device_descriptor() {
        let d = DeviceDescriptor::parse(&[
            18, 1, 0, 2, 0, 0, 0, 64, 0x27, 0x06, 0x01, 0x00, 0, 0, 1, 2, 3, 1,
        ])
        .unwrap();
        assert_eq!(d.vendor, 0x0627);
        assert_eq!(d.max_packet0, 64);
        assert_eq!(d.usb_version, 0x200);
    }

    #[test]
    fn strings() {
        let mut s = alloc::string::String::new();
        assert!(parse_string(&[8, 3, b'Q', 0, b'E', 0, b'M', 0], &mut s));
        assert_eq!(s, "QEM");
    }

    #[test]
    fn keyboard_transitions() {
        let mut k = KeyboardState::default();
        let mut out = vec![];
        // Press 'a'
        let p = k.update(&[0, 0, 4, 0, 0, 0, 0, 0], &mut out);
        assert_eq!(out, vec![0x1E]);
        assert_eq!(p, vec![4]);
        out.clear();
        // Shift + 'a' held
        k.update(&[2, 0, 4, 0, 0, 0, 0, 0], &mut out);
        assert_eq!(out, vec![0x2A]);
        out.clear();
        // Release all, press Up arrow
        k.update(&[0, 0, 0x52, 0, 0, 0, 0, 0], &mut out);
        assert_eq!(out, vec![0xAA, 0x9E, 0xE0, 0x48]);
        out.clear();
        assert!(k.repeat(0x52, &mut out));
        assert_eq!(out, vec![0xE0, 0x48]);
        out.clear();
        k.release_all(&mut out);
        assert_eq!(out, vec![0xE0, 0xC8]);
    }

    #[test]
    fn keymap_basics() {
        assert_eq!(usage_to_set1(0x28), Some((false, 0x1C))); // enter
        assert_eq!(usage_to_set1(0x2C), Some((false, 0x39))); // space
        assert_eq!(usage_to_set1(0x27), Some((false, 0x0B))); // 0
        assert_eq!(usage_to_set1(0x4C), Some((true, 0x53))); // delete
        assert_eq!(usage_to_set1(0x58), Some((true, 0x1C))); // kp enter
        assert_eq!(usage_to_set1(0x3A), Some((false, 0x3B))); // F1
        assert_eq!(usage_to_set1(0x45), Some((false, 0x58))); // F12
        assert_eq!(usage_to_set1(0x2D), Some((false, 0x0C))); // -
        assert_eq!(usage_to_set1(0x38), Some((false, 0x35))); // /
        assert_eq!(usage_to_set1(0x54), Some((true, 0x35))); // kp /
    }

    #[test]
    fn mouse() {
        let r = MouseReport::parse(&[1, 0xFF, 2]).unwrap();
        assert_eq!((r.buttons, r.dx, r.dy, r.wheel), (1, -1, 2, 0));
    }
}
