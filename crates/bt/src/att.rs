//! Attribute Protocol PDUs.

use crate::le16;
use alloc::vec::Vec;

pub const ERROR_RSP: u8 = 0x01;
pub const MTU_REQ: u8 = 0x02;
pub const MTU_RSP: u8 = 0x03;
pub const FIND_INFO_REQ: u8 = 0x04;
pub const FIND_INFO_RSP: u8 = 0x05;
pub const READ_BY_TYPE_REQ: u8 = 0x08;
pub const READ_BY_TYPE_RSP: u8 = 0x09;
pub const READ_REQ: u8 = 0x0A;
pub const READ_RSP: u8 = 0x0B;
pub const READ_BLOB_REQ: u8 = 0x0C;
pub const READ_BLOB_RSP: u8 = 0x0D;
pub const READ_BY_GROUP_REQ: u8 = 0x10;
pub const READ_BY_GROUP_RSP: u8 = 0x11;
pub const WRITE_REQ: u8 = 0x12;
pub const WRITE_RSP: u8 = 0x13;
pub const NOTIFY: u8 = 0x1B;
pub const INDICATE: u8 = 0x1D;
pub const CONFIRM: u8 = 0x1E;
pub const WRITE_CMD: u8 = 0x52;

pub const ERR_INVALID_HANDLE: u8 = 0x01;
pub const ERR_READ_NOT_PERMITTED: u8 = 0x02;
pub const ERR_REQUEST_NOT_SUPPORTED: u8 = 0x06;
pub const ERR_INSUFFICIENT_AUTHENTICATION: u8 = 0x05;
pub const ERR_ATTRIBUTE_NOT_FOUND: u8 = 0x0A;
pub const ERR_ATTRIBUTE_NOT_LONG: u8 = 0x0B;
pub const ERR_INSUFFICIENT_ENCRYPTION: u8 = 0x0F;

/// The default LE ATT MTU.
pub const DEFAULT_MTU: u16 = 23;

/// A 16- or 128-bit UUID (the 128-bit form in wire order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Uuid {
    U16(u16),
    U128([u8; 16]),
}

impl Uuid {
    pub fn from_bytes(b: &[u8]) -> Option<Uuid> {
        match b.len() {
            2 => Some(Uuid::U16(u16::from_le_bytes([b[0], b[1]]))),
            16 => {
                let a: [u8; 16] = b.try_into().ok()?;
                Some(Uuid::U128(a).shorten())
            }
            _ => None,
        }
    }

    /// A 128-bit UUID on the Bluetooth base becomes its 16-bit alias.
    fn shorten(self) -> Uuid {
        const BASE: [u8; 12] = [
            0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00,
        ];
        match self {
            Uuid::U128(a) if a[..12] == BASE && a[14..] == [0, 0] => {
                Uuid::U16(u16::from_le_bytes([a[12], a[13]]))
            }
            u => u,
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Uuid::U16(v) => v.to_le_bytes().to_vec(),
            Uuid::U128(a) => a.to_vec(),
        }
    }
}

pub fn mtu_req(mtu: u16) -> Vec<u8> {
    let mut v = alloc::vec![MTU_REQ];
    v.extend_from_slice(&mtu.to_le_bytes());
    v
}

fn range(op: u8, start: u16, end: u16, uuid: Option<Uuid>) -> Vec<u8> {
    let mut v = alloc::vec![op];
    v.extend_from_slice(&start.to_le_bytes());
    v.extend_from_slice(&end.to_le_bytes());
    if let Some(u) = uuid {
        v.extend_from_slice(&u.bytes());
    }
    v
}

pub fn read_by_group_req(start: u16, end: u16, t: Uuid) -> Vec<u8> {
    range(READ_BY_GROUP_REQ, start, end, Some(t))
}

pub fn read_by_type_req(start: u16, end: u16, t: Uuid) -> Vec<u8> {
    range(READ_BY_TYPE_REQ, start, end, Some(t))
}

pub fn find_info_req(start: u16, end: u16) -> Vec<u8> {
    range(FIND_INFO_REQ, start, end, None)
}

pub fn read_req(h: u16) -> Vec<u8> {
    let mut v = alloc::vec![READ_REQ];
    v.extend_from_slice(&h.to_le_bytes());
    v
}

pub fn read_blob_req(h: u16, off: u16) -> Vec<u8> {
    let mut v = alloc::vec![READ_BLOB_REQ];
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(&off.to_le_bytes());
    v
}

pub fn write(op: u8, h: u16, value: &[u8]) -> Vec<u8> {
    let mut v = alloc::vec![op];
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(value);
    v
}

pub fn error_rsp(req: u8, h: u16, code: u8) -> Vec<u8> {
    let mut v = alloc::vec![ERROR_RSP, req];
    v.extend_from_slice(&h.to_le_bytes());
    v.push(code);
    v
}

/// Entries of a Read By Type / Read By Group Type / Find Information
/// response: (handle, the rest of the entry).
pub fn entries(rsp: &[u8]) -> Vec<(u16, &[u8])> {
    let (len, body) = match rsp.first() {
        Some(&FIND_INFO_RSP) => {
            let f = *rsp.get(1).unwrap_or(&0);
            (if f == 1 { 4 } else { 18 }, rsp.get(2..).unwrap_or(&[]))
        }
        Some(_) => (
            *rsp.get(1).unwrap_or(&0) as usize,
            rsp.get(2..).unwrap_or(&[]),
        ),
        None => return Vec::new(),
    };
    if len < 2 {
        return Vec::new();
    }
    body.chunks_exact(len)
        .map(|e| (le16(e, 0).unwrap_or(0), &e[2..]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids() {
        let hid128 = [
            0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x12, 0x18, 0,
            0,
        ];
        assert_eq!(Uuid::from_bytes(&hid128), Some(Uuid::U16(0x1812)));
        assert_eq!(Uuid::from_bytes(&[0x4B, 0x2A]), Some(Uuid::U16(0x2A4B)));
        let rsp = [
            READ_BY_GROUP_RSP,
            6,
            1,
            0,
            5,
            0,
            0x00,
            0x18,
            6,
            0,
            9,
            0,
            0x12,
            0x18,
        ];
        let e = entries(&rsp);
        assert_eq!(e.len(), 2);
        assert_eq!(e[1], (6, &[9, 0, 0x12, 0x18][..]));
    }
}
