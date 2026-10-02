//! Bluetooth host stack pieces shared by the kernel driver and host tests:
//! HCI commands and events, advertising data, L2CAP (BR/EDR basic mode and
//! LE credit-based), ATT and a GATT client, the Security Manager (LE Secure
//! Connections and legacy pairing, with the Core specification's crypto
//! functions), SDP, HID over GATT and BR/EDR HIDP, and the Intel and
//! MediaTek controller firmware formats.
//!
//! Multi-byte values are kept in wire order (least significant byte
//! first), as they appear in HCI and SMP packets.

#![no_std]

extern crate alloc;

pub mod adv;
pub mod bcm;
pub mod att;
pub mod crypto;
pub mod gatt;
pub mod hci;
pub mod hid;
pub mod intel;
pub mod l2cap;
pub mod mtk;
pub mod rtl;
pub mod sdp;
pub mod smp;

use core::fmt;

/// A device address in wire order (`addr[5]` is printed first).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Addr(pub [u8; 6]);

/// LE address types (HCI encoding); BR/EDR devices use `Public`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AddrType {
    #[default]
    Public = 0,
    Random = 1,
}

impl AddrType {
    pub fn from_u8(v: u8) -> AddrType {
        // 2/3 (resolved identity addresses) keep their public/random kind.
        if v & 1 != 0 {
            AddrType::Random
        } else {
            AddrType::Public
        }
    }
}

impl Addr {
    /// Parse "AA:BB:CC:DD:EE:FF".
    pub fn parse(s: &str) -> Option<Addr> {
        let mut a = [0u8; 6];
        let mut n = 0;
        for part in s.split(':') {
            if n == 6 || part.len() != 2 {
                return None;
            }
            a[5 - n] = u8::from_str_radix(part, 16).ok()?;
            n += 1;
        }
        (n == 6).then_some(Addr(a))
    }

    pub fn from_slice(b: &[u8]) -> Option<Addr> {
        Some(Addr(b.get(..6)?.try_into().ok()?))
    }

    /// A resolvable private address (random, top bits 01).
    pub fn is_rpa(&self) -> bool {
        self.0[5] >> 6 == 0b01
    }
}

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let a = &self.0;
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            a[5], a[4], a[3], a[2], a[1], a[0]
        )
    }
}

/// Little-endian readers used by the codecs.
pub(crate) fn le16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

pub(crate) fn le32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

#[cfg(test)]
pub(crate) mod testutil {
    use alloc::vec::Vec;

    /// Hex as printed in the specification (most significant byte first).
    pub fn msb(s: &str) -> Vec<u8> {
        let s: Vec<u8> = s.bytes().filter(|c| c.is_ascii_hexdigit()).collect();
        s.chunks(2)
            .map(|c| u8::from_str_radix(core::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }

    /// The same value in wire order (least significant byte first).
    pub fn le(s: &str) -> Vec<u8> {
        let mut v = msb(s);
        v.reverse();
        v
    }

    pub fn arr<const N: usize>(v: Vec<u8>) -> [u8; N] {
        v.try_into().unwrap()
    }
}
