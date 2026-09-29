//! HID report descriptors: parse the item stream into input fields and
//! decode reports with them (keyboards including NKRO bitmaps, consumer
//! "media" keys, mice, absolute pointers such as tablets and touch screens,
//! game pads and joysticks).

use alloc::vec::Vec;

// Usage pages.
pub const PAGE_DESKTOP: u16 = 0x01;
pub const PAGE_KEYBOARD: u16 = 0x07;
pub const PAGE_BUTTON: u16 = 0x09;
pub const PAGE_CONSUMER: u16 = 0x0C;
pub const PAGE_DIGITIZER: u16 = 0x0D;

// Generic desktop usages.
pub const USAGE_POINTER: u16 = 0x01;
pub const USAGE_MOUSE: u16 = 0x02;
pub const USAGE_JOYSTICK: u16 = 0x04;
pub const USAGE_GAMEPAD: u16 = 0x05;
pub const USAGE_KEYBOARD: u16 = 0x06;
pub const USAGE_X: u16 = 0x30;
pub const USAGE_Y: u16 = 0x31;
pub const USAGE_Z: u16 = 0x32;
pub const USAGE_RX: u16 = 0x33;
pub const USAGE_RY: u16 = 0x34;
pub const USAGE_RZ: u16 = 0x35;
pub const USAGE_WHEEL: u16 = 0x38;
pub const USAGE_HAT: u16 = 0x39;
/// Consumer page: horizontal scroll.
pub const USAGE_AC_PAN: u16 = 0x238;

/// Full usage: page << 16 | id.
pub const fn usage(page: u16, id: u16) -> u32 {
    (page as u32) << 16 | id as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Truncated,
    Unbalanced,
    TooLarge,
}

/// One Input main item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub report_id: u8,
    /// Bit offset in the report (after the report ID byte, if any).
    pub offset: u32,
    pub size: u32,
    pub count: u32,
    /// Data flags: bit 0 constant, bit 1 variable, bit 2 relative.
    pub flags: u32,
    pub logical_min: i32,
    pub logical_max: i32,
    /// Explicit usages (full, with page).
    pub usages: Vec<u32>,
    /// Usage range (full), when given as minimum/maximum.
    pub usage_range: Option<(u32, u32)>,
    /// Usage of the enclosing top-level application collection.
    pub application: u32,
}

impl Field {
    pub fn is_constant(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn is_variable(&self) -> bool {
        self.flags & 2 != 0
    }
    pub fn is_relative(&self) -> bool {
        self.flags & 4 != 0
    }

    /// Usage of element `i` of a variable field.
    pub fn usage_of(&self, i: u32) -> Option<u32> {
        if let Some((lo, hi)) = self.usage_range {
            let u = lo + i;
            return (u <= hi).then_some(u);
        }
        self.usages.get(i as usize).or(self.usages.last()).copied()
    }

    /// Usage selected by array value `v`.
    fn array_usage(&self, v: i32) -> Option<u32> {
        if v < self.logical_min || v > self.logical_max {
            return None;
        }
        let idx = (v - self.logical_min) as u32;
        match self.usage_range {
            Some((lo, hi)) => (lo + idx <= hi).then_some(lo + idx),
            None => self.usages.get(idx as usize).copied(),
        }
    }
}

/// A parsed report descriptor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportDescriptor {
    pub fields: Vec<Field>,
    /// Reports start with a report ID byte.
    pub has_ids: bool,
    /// Top-level application collections, in order.
    pub applications: Vec<u32>,
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u16,
    logical_min: i32,
    logical_max: i32,
    size: u32,
    count: u32,
    report_id: u8,
}

fn sdata(d: &[u8]) -> i32 {
    match d.len() {
        1 => d[0] as i8 as i32,
        2 => i16::from_le_bytes([d[0], d[1]]) as i32,
        4 => i32::from_le_bytes([d[0], d[1], d[2], d[3]]),
        _ => 0,
    }
}

fn udata(d: &[u8]) -> u32 {
    d.iter().rev().fold(0, |a, &b| a << 8 | b as u32)
}

impl ReportDescriptor {
    pub fn parse(desc: &[u8]) -> Result<ReportDescriptor, Error> {
        let mut out = ReportDescriptor::default();
        let mut g = Globals::default();
        let mut stack: Vec<Globals> = Vec::new();
        let mut usages: Vec<u32> = Vec::new();
        let mut umin: Option<u32> = None;
        let mut umax: Option<u32> = None;
        let mut depth = 0usize;
        let mut app = 0u32;
        // Next input bit offset per report ID.
        let mut offsets: Vec<(u8, u32)> = Vec::new();
        let mut i = 0;
        while i < desc.len() {
            let p = desc[i];
            if p == 0xFE {
                // Long item: skip.
                let n = *desc.get(i + 1).ok_or(Error::Truncated)? as usize;
                i += 3 + n;
                continue;
            }
            let n = [0, 1, 2, 4][(p & 3) as usize];
            let d = desc.get(i + 1..i + 1 + n).ok_or(Error::Truncated)?;
            i += 1 + n;
            let (ty, tag) = ((p >> 2) & 3, p >> 4);
            // Full usage from a local item: 4-byte data carries the page.
            let full = |d: &[u8], page: u16| {
                if d.len() == 4 {
                    udata(d)
                } else {
                    usage(page, udata(d) as u16)
                }
            };
            match (ty, tag) {
                // Main items.
                (0, 8) => {
                    // Input.
                    let flags = udata(d);
                    let off = match offsets.iter_mut().find(|(id, _)| *id == g.report_id) {
                        Some((_, o)) => o,
                        None => {
                            offsets.push((g.report_id, 0));
                            &mut offsets.last_mut().unwrap().1
                        }
                    };
                    let bits = g.size.checked_mul(g.count).ok_or(Error::TooLarge)?;
                    if bits > 8 * 4096 || g.size > 32 {
                        return Err(Error::TooLarge);
                    }
                    let logical_max = if g.logical_min >= 0 && g.logical_max < 0 {
                        // An unsigned maximum encoded with its top bit set.
                        i32::MAX
                    } else {
                        g.logical_max
                    };
                    out.fields.push(Field {
                        report_id: g.report_id,
                        offset: *off,
                        size: g.size,
                        count: g.count,
                        flags,
                        logical_min: g.logical_min,
                        logical_max,
                        usages: core::mem::take(&mut usages),
                        usage_range: match (umin, umax) {
                            (Some(a), Some(b)) if a <= b => Some((a, b)),
                            _ => None,
                        },
                        application: app,
                    });
                    *off += bits;
                    usages.clear();
                    umin = None;
                    umax = None;
                }
                (0, 9) | (0, 11) => {
                    // Output / Feature: not used for input.
                    usages.clear();
                    umin = None;
                    umax = None;
                }
                (0, 10) => {
                    // Collection.
                    if depth == 0 && udata(d) == 1 {
                        app = usages.first().copied().or(umin).unwrap_or(0);
                        out.applications.push(app);
                    }
                    depth += 1;
                    usages.clear();
                    umin = None;
                    umax = None;
                }
                (0, 12) => {
                    depth = depth.checked_sub(1).ok_or(Error::Unbalanced)?;
                }
                // Global items.
                (1, 0) => g.page = udata(d) as u16,
                (1, 1) => g.logical_min = sdata(d),
                (1, 2) => {
                    g.logical_max = sdata(d);
                    // Unsigned maximum for unsigned ranges (e.g. 0..255 as 0xFF).
                    if g.logical_min >= 0 && g.logical_max < 0 && d.len() < 4 {
                        g.logical_max = udata(d) as i32;
                    }
                }
                (1, 7) => g.size = udata(d),
                (1, 8) => {
                    g.report_id = udata(d) as u8;
                    out.has_ids = true;
                }
                (1, 9) => g.count = udata(d),
                (1, 10) => stack.push(g),
                (1, 11) => g = stack.pop().ok_or(Error::Unbalanced)?,
                // Local items.
                (2, 0) => usages.push(full(d, g.page)),
                (2, 1) => umin = Some(full(d, g.page)),
                (2, 2) => umax = Some(full(d, g.page)),
                _ => {}
            }
        }
        if depth != 0 {
            return Err(Error::Unbalanced);
        }
        Ok(out)
    }

    /// Whether any top-level application has usage `u`.
    pub fn has_application(&self, u: u32) -> bool {
        self.applications.contains(&u)
    }

    /// Length in bytes of input report `id` (without the ID byte).
    pub fn report_len(&self, id: u8) -> usize {
        self.fields
            .iter()
            .filter(|f| f.report_id == id)
            .map(|f| (f.offset + f.size * f.count).div_ceil(8) as usize)
            .max()
            .unwrap_or(0)
    }

    /// Decode one input report.
    pub fn decode(&self, report: &[u8]) -> Decoded {
        let (id, data) = if self.has_ids {
            match report.split_first() {
                Some((&id, rest)) => (id, rest),
                None => (0, report),
            }
        } else {
            (0, report)
        };
        let mut out = Decoded {
            report_id: id,
            ..Decoded::default()
        };
        for f in self
            .fields
            .iter()
            .filter(|f| f.report_id == id && !f.is_constant())
        {
            let signed = f.logical_min < 0;
            for i in 0..f.count {
                let Some(v) = extract(data, f.offset + i * f.size, f.size, signed) else {
                    break;
                };
                if f.is_variable() {
                    let Some(u) = f.usage_of(i) else { continue };
                    out.add_variable(f, u, v);
                } else if let Some(u) = f.array_usage(v) {
                    out.add_selected(u);
                }
            }
        }
        out
    }
}

/// Read a `size`-bit little-endian field at bit `off`.
pub fn extract(data: &[u8], off: u32, size: u32, signed: bool) -> Option<i32> {
    if size == 0 || size > 32 || (off + size).div_ceil(8) as usize > data.len() {
        return None;
    }
    let mut v: u64 = 0;
    let first = (off / 8) as usize;
    let last = ((off + size - 1) / 8) as usize;
    for (k, &b) in data[first..=last].iter().enumerate() {
        v |= (b as u64) << (8 * k);
    }
    v >>= off % 8;
    v &= (1u64 << size) - 1;
    if signed && size < 32 && v & (1 << (size - 1)) != 0 {
        v |= !0u64 << size;
    }
    Some(v as u32 as i32)
}

/// The content of one input report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decoded {
    pub report_id: u8,
    /// Keyboard usages held (page 7, including modifiers 0xE0-0xE7).
    pub keys: Vec<u8>,
    /// Consumer-page usages held (media keys).
    pub consumer: Vec<u16>,
    /// Buttons held (page 9: bit 0 = button 1).
    pub buttons: u32,
    /// Relative axes: (usage, delta).
    pub rel: Vec<(u32, i32)>,
    /// Absolute axes: (usage, value, logical min, logical max).
    pub abs: Vec<(u32, i32, i32, i32)>,
}

impl Decoded {
    fn add_variable(&mut self, f: &Field, u: u32, v: i32) {
        let (page, id) = ((u >> 16) as u16, u as u16);
        match page {
            PAGE_KEYBOARD => {
                if v != 0 && id > 3 && id <= 0xFF {
                    self.keys.push(id as u8);
                }
            }
            PAGE_BUTTON => {
                if v != 0 && (1..=32).contains(&id) {
                    self.buttons |= 1 << (id - 1);
                }
            }
            PAGE_CONSUMER if id != USAGE_AC_PAN => {
                if v != 0 {
                    self.consumer.push(id);
                }
            }
            _ => {
                if f.is_relative() {
                    if v != 0 {
                        self.rel.push((u, v));
                    }
                } else {
                    self.abs.push((u, v, f.logical_min, f.logical_max));
                }
            }
        }
    }

    fn add_selected(&mut self, u: u32) {
        let (page, id) = ((u >> 16) as u16, u as u16);
        match page {
            PAGE_KEYBOARD if id > 3 && id <= 0xFF => self.keys.push(id as u8),
            PAGE_CONSUMER if id != 0 => self.consumer.push(id),
            PAGE_BUTTON if (1..=32).contains(&id) => self.buttons |= 1 << (id - 1),
            _ => {}
        }
    }

    /// Relative value of `usage` (0 if absent).
    pub fn rel_of(&self, usage: u32) -> i32 {
        self.rel
            .iter()
            .filter(|(u, _)| *u == usage)
            .map(|(_, v)| v)
            .sum()
    }

    /// Absolute value of `usage`, with its logical range.
    pub fn abs_of(&self, usage: u32) -> Option<(i32, i32, i32)> {
        self.abs
            .iter()
            .find(|(u, ..)| *u == usage)
            .map(|&(_, v, lo, hi)| (v, lo, hi))
    }
}

/// PS/2 set-1 code (always 0xE0-prefixed) for a consumer-page usage.
pub fn consumer_to_set1(u: u16) -> Option<u8> {
    Some(match u {
        0xE2 => 0x20,  // mute
        0xE9 => 0x30,  // volume up
        0xEA => 0x2E,  // volume down
        0xCD => 0x22,  // play/pause
        0xB5 => 0x19,  // next track
        0xB6 => 0x10,  // previous track
        0xB7 => 0x24,  // stop
        0x183 => 0x6D, // media select
        0x18A => 0x6C, // mail
        0x192 => 0x21, // calculator
        0x194 => 0x6B, // my computer
        0x221 => 0x65, // www search
        0x223 => 0x32, // www home
        0x224 => 0x6A, // www back
        0x225 => 0x69, // www forward
        0x226 => 0x68, // www stop
        0x227 => 0x67, // www refresh
        0x22A => 0x66, // www favourites
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // QEMU usb-tablet (hw/usb/dev-hid.c).
    const TABLET: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29,
        0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05,
        0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xff, 0x7f, 0x35, 0x00,
        0x46, 0xff, 0x7f, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81,
        0x25, 0x7f, 0x35, 0x00, 0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0,
    ];

    // The HID spec's boot keyboard (appendix B.1).
    const BOOT_KBD: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25,
        0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05,
        0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91,
        0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65,
        0x81, 0x00, 0xC0,
    ];

    // A composite gaming keyboard: report 1 = NKRO bitmap, report 2 =
    // consumer keys (array), report 3 = mouse with wheel and 5 buttons.
    const COMPOSITE: &[u8] = &[
        // Report 1: keyboard, modifiers + 104-key bitmap.
        0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15,
        0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x19, 0x00, 0x29, 0x67, 0x95, 0x68,
        0x81, 0x02, 0xC0, // Report 2: consumer control, one 16-bit array slot.
        0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, 0x15, 0x00, 0x26, 0xFF, 0x03, 0x19, 0x00,
        0x2A, 0xFF, 0x03, 0x75, 0x10, 0x95, 0x01, 0x81, 0x00, 0xC0, // Report 3: mouse.
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x03, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19,
        0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01,
        0x75, 0x03, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x01, 0x80, 0x26, 0xFF,
        0x7F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x06, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08,
        0x95, 0x01, 0x81, 0x06, 0xC0, 0xC0,
    ];

    // A typical USB game pad: 2 sticks (8-bit), a hat switch, 12 buttons.
    const GAMEPAD: &[u8] = &[
        0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x04,
        0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35, 0x81, 0x02, 0x15, 0x00, 0x25, 0x07, 0x35,
        0x00, 0x46, 0x3B, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x09, 0x39, 0x81, 0x42, 0x65,
        0x00, 0x75, 0x01, 0x95, 0x0C, 0x05, 0x09, 0x19, 0x01, 0x29, 0x0C, 0x15, 0x00, 0x25, 0x01,
        0x81, 0x02, 0xC0,
    ];

    #[test]
    fn tablet() {
        let d = ReportDescriptor::parse(TABLET).unwrap();
        assert!(!d.has_ids);
        assert!(d.has_application(usage(PAGE_DESKTOP, USAGE_MOUSE)));
        assert_eq!(d.report_len(0), 6);
        // Button 1, x = 0x4000, y = 0x2000, wheel -1.
        let r = d.decode(&[0x01, 0x00, 0x40, 0x00, 0x20, 0xFF]);
        assert_eq!(r.buttons, 1);
        assert_eq!(
            r.abs_of(usage(PAGE_DESKTOP, USAGE_X)),
            Some((0x4000, 0, 0x7FFF))
        );
        assert_eq!(r.abs_of(usage(PAGE_DESKTOP, USAGE_Y)).unwrap().0, 0x2000);
        assert_eq!(r.rel_of(usage(PAGE_DESKTOP, USAGE_WHEEL)), -1);
    }

    #[test]
    fn boot_keyboard() {
        let d = ReportDescriptor::parse(BOOT_KBD).unwrap();
        assert!(d.has_application(usage(PAGE_DESKTOP, USAGE_KEYBOARD)));
        assert_eq!(d.report_len(0), 8);
        // Left shift + 'a' + 'b'.
        let r = d.decode(&[0x02, 0, 0x04, 0x05, 0, 0, 0, 0]);
        assert_eq!(r.keys, vec![0xE1, 0x04, 0x05]);
    }

    #[test]
    fn nkro_consumer_and_mouse() {
        let d = ReportDescriptor::parse(COMPOSITE).unwrap();
        assert!(d.has_ids);
        assert_eq!(d.applications.len(), 3);
        // NKRO: 8 modifier bits then usages 0..0x67, one bit each.
        let mut rep = vec![0u8; 1 + d.report_len(1)];
        rep[0] = 1;
        rep[1] = 0x01; // left ctrl
        for u in [0x04u32, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A] {
            let bit = 8 + u;
            rep[1 + (bit / 8) as usize] |= 1 << (bit % 8);
        }
        let r = d.decode(&rep);
        assert_eq!(r.keys, vec![0xE0, 4, 5, 6, 7, 8, 9, 10]);
        // Consumer: volume up.
        let r = d.decode(&[2, 0xE9, 0x00]);
        assert_eq!(r.consumer, vec![0xE9]);
        assert_eq!(consumer_to_set1(0xE9), Some(0x30));
        let r = d.decode(&[2, 0, 0]);
        assert!(r.consumer.is_empty());
        // Mouse: buttons 1+5, dx = -300, dy = 20, wheel 2.
        let dx = (-300i16).to_le_bytes();
        let r = d.decode(&[3, 0x11, dx[0], dx[1], 20, 0, 2]);
        assert_eq!(r.buttons, 0x11);
        assert_eq!(r.rel_of(usage(PAGE_DESKTOP, USAGE_X)), -300);
        assert_eq!(r.rel_of(usage(PAGE_DESKTOP, USAGE_Y)), 20);
        assert_eq!(r.rel_of(usage(PAGE_DESKTOP, USAGE_WHEEL)), 2);
    }

    #[test]
    fn gamepad() {
        let d = ReportDescriptor::parse(GAMEPAD).unwrap();
        assert!(d.has_application(usage(PAGE_DESKTOP, USAGE_GAMEPAD)));
        assert_eq!(d.report_len(0), 6);
        // Sticks 0x80,0x80,0x00,0xFF; hat 2 (right); buttons 1 and 12.
        let r = d.decode(&[0x80, 0x80, 0x00, 0xFF, 0x12, 0x80]);
        assert_eq!(r.abs_of(usage(PAGE_DESKTOP, USAGE_X)), Some((0x80, 0, 255)));
        assert_eq!(r.abs_of(usage(PAGE_DESKTOP, USAGE_RZ)).unwrap().0, 255);
        assert_eq!(r.abs_of(usage(PAGE_DESKTOP, USAGE_HAT)), Some((2, 0, 7)));
        assert_eq!(r.buttons, 1 | 1 << 11);
    }

    #[test]
    fn extract_bits() {
        assert_eq!(extract(&[0b1011_0000], 4, 4, false), Some(0b1011));
        assert_eq!(extract(&[0b1011_0000], 4, 4, true), Some(-5));
        assert_eq!(extract(&[0xFF, 0x0F], 4, 8, false), Some(0xFF));
        assert_eq!(extract(&[0x01], 4, 8, false), None);
    }

    #[test]
    fn malformed() {
        assert_eq!(
            ReportDescriptor::parse(&[0xA1, 0x01]),
            Err(Error::Unbalanced)
        );
        assert_eq!(ReportDescriptor::parse(&[0xC0]), Err(Error::Unbalanced));
        assert_eq!(
            ReportDescriptor::parse(&[0x26, 0xFF]),
            Err(Error::Truncated)
        );
    }
}
