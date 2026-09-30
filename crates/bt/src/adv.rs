//! Advertising and extended inquiry response data (AD structures).

use alloc::string::String;
use alloc::vec::Vec;

pub const AD_FLAGS: u8 = 0x01;
pub const AD_UUID16_SOME: u8 = 0x02;
pub const AD_UUID16_ALL: u8 = 0x03;
pub const AD_NAME_SHORT: u8 = 0x08;
pub const AD_NAME: u8 = 0x09;
pub const AD_APPEARANCE: u8 = 0x19;

/// What advertising (or EIR) data says about a device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdInfo {
    pub name: Option<String>,
    pub flags: u8,
    pub uuid16: Vec<u16>,
    pub appearance: Option<u16>,
}

/// Iterate `(type, value)` over AD structures.
pub fn structures(d: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut o = 0;
    core::iter::from_fn(move || {
        let len = *d.get(o)? as usize;
        if len == 0 {
            return None; // padding (EIR)
        }
        let s = d.get(o + 1..o + 1 + len)?;
        o += 1 + len;
        Some((s[0], &s[1..]))
    })
}

pub fn parse(d: &[u8]) -> AdInfo {
    let mut info = AdInfo::default();
    for (t, v) in structures(d) {
        match t {
            AD_FLAGS => info.flags = v.first().copied().unwrap_or(0),
            AD_UUID16_SOME | AD_UUID16_ALL => info
                .uuid16
                .extend(v.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]))),
            AD_NAME => info.name = Some(String::from_utf8_lossy(v).into_owned()),
            AD_NAME_SHORT if info.name.is_none() => {
                info.name = Some(String::from_utf8_lossy(v).into_owned())
            }
            AD_APPEARANCE if v.len() >= 2 => {
                info.appearance = Some(u16::from_le_bytes([v[0], v[1]]))
            }
            _ => {}
        }
    }
    info
}

impl AdInfo {
    /// Merge a scan response into the advertisement's data.
    pub fn merge(&mut self, o: AdInfo) {
        if o.name.is_some() {
            self.name = o.name;
        }
        self.flags |= o.flags;
        for u in o.uuid16 {
            if !self.uuid16.contains(&u) {
                self.uuid16.push(u);
            }
        }
        self.appearance = self.appearance.or(o.appearance);
    }
}

/// A short description of an LE appearance value (category).
pub fn appearance_name(a: u16) -> &'static str {
    match a >> 6 {
        0x00F => match a & 0x3F {
            1 => "keyboard",
            2 => "mouse",
            3 => "joystick",
            4 => "gamepad",
            _ => "HID",
        },
        0x001 => "phone",
        0x002 => "computer",
        0x003 => "watch",
        0x020 => "headset",
        _ => "",
    }
}

/// A short description of a BR/EDR class of device.
pub fn class_name(c: u32) -> &'static str {
    match (c >> 8) & 0x1F {
        1 => "computer",
        2 => "phone",
        4 => "audio",
        5 => match (c >> 6) & 3 {
            1 => "keyboard",
            2 => "mouse",
            3 => "keyboard+mouse",
            _ => "HID",
        },
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_adv() {
        let d = [
            2, 1, 6, 3, 3, 0x12, 0x18, 3, 0x19, 0xC1, 0x03, 5, 9, b'K', b'e', b'y', b's', 0, 0,
        ];
        let a = parse(&d);
        assert_eq!(a.flags, 6);
        assert_eq!(a.uuid16, [0x1812]);
        assert_eq!(a.appearance, Some(0x03C1));
        assert_eq!(appearance_name(0x03C1), "keyboard");
        assert_eq!(a.name.as_deref(), Some("Keys"));
        assert_eq!(class_name(0x002540), "keyboard");
    }
}
