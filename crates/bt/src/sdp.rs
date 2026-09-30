//! A minimal SDP client: one ServiceSearchAttributeRequest for a service
//! class, and the data-element parsing needed to pull out the HID
//! descriptor (attribute 0x0206) of a BR/EDR HID device. SDP is
//! big-endian.

use alloc::vec::Vec;

pub const SEARCH_ATTR_REQ: u8 = 0x06;
pub const SEARCH_ATTR_RSP: u8 = 0x07;
pub const ERROR_RSP: u8 = 0x01;

pub const UUID_HID: u16 = 0x1124;
pub const ATTR_HID_DESCRIPTOR_LIST: u16 = 0x0206;

/// A ServiceSearchAttributeRequest for `uuid`, all attributes.
pub fn search_attr_req(tid: u16, uuid: u16, cont: &[u8]) -> Vec<u8> {
    let mut params = alloc::vec![0x35, 3, 0x19];
    params.extend_from_slice(&uuid.to_be_bytes());
    params.extend_from_slice(&0xFFFFu16.to_be_bytes());
    params.extend_from_slice(&[0x35, 5, 0x0A, 0x00, 0x00, 0xFF, 0xFF]);
    params.push(cont.len() as u8);
    params.extend_from_slice(cont);
    let mut v = alloc::vec![SEARCH_ATTR_REQ];
    v.extend_from_slice(&tid.to_be_bytes());
    v.extend_from_slice(&(params.len() as u16).to_be_bytes());
    v.extend_from_slice(&params);
    v
}

/// A response: (attribute list bytes, continuation state).
pub fn search_attr_rsp(p: &[u8]) -> Option<(&[u8], &[u8])> {
    if *p.first()? != SEARCH_ATTR_RSP {
        return None;
    }
    let n = u16::from_be_bytes([*p.get(5)?, *p.get(6)?]) as usize;
    let lists = p.get(7..7 + n)?;
    let cl = *p.get(7 + n)? as usize;
    Some((lists, p.get(8 + n..8 + n + cl)?))
}

/// A parsed data element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Element<'a> {
    Nil,
    Uint(u64),
    Int(i64),
    Uuid(&'a [u8]),
    Bytes(&'a [u8]),
    Bool(bool),
    Seq(Vec<Element<'a>>),
}

/// Parse one data element at the start of `b`: (element, bytes used).
pub fn element(b: &[u8]) -> Option<(Element<'_>, usize)> {
    let h = *b.first()?;
    let (ty, si) = (h >> 3, h & 7);
    let (len, hdr) = match si {
        0..=4 if ty == 0 => (0, 1),
        0..=4 => (1usize << si, 1),
        5 => (*b.get(1)? as usize, 2),
        6 => (u16::from_be_bytes([*b.get(1)?, *b.get(2)?]) as usize, 3),
        _ => (
            u32::from_be_bytes([*b.get(1)?, *b.get(2)?, *b.get(3)?, *b.get(4)?]) as usize,
            5,
        ),
    };
    let d = b.get(hdr..hdr + len)?;
    let num = || d.iter().fold(0u64, |a, &x| a << 8 | x as u64);
    let e = match ty {
        0 => Element::Nil,
        1 => Element::Uint(num()),
        2 => {
            let shift = 64 - 8 * len.min(8) as u32;
            Element::Int(((num() << shift) as i64) >> shift)
        }
        3 => Element::Uuid(d),
        4 | 8 => Element::Bytes(d),
        5 => Element::Bool(d.first().is_some_and(|&x| x != 0)),
        6 | 7 => {
            let mut v = Vec::new();
            let mut o = 0;
            while o < d.len() {
                let (e, n) = element(&d[o..])?;
                v.push(e);
                o += n;
            }
            Element::Seq(v)
        }
        _ => return None,
    };
    Some((e, hdr + len))
}

/// Find `attr` in attribute lists (a sequence of records, each a sequence
/// of id/value pairs).
pub fn find_attr<'a>(lists: &'a [u8], attr: u16) -> Option<Element<'a>> {
    let (Element::Seq(records), _) = element(lists)? else {
        return None;
    };
    for r in records {
        let Element::Seq(pairs) = r else { continue };
        for kv in pairs.chunks(2) {
            if let [Element::Uint(id), v] = kv
                && *id == attr as u64
            {
                return Some(v.clone());
            }
        }
    }
    None
}

/// The report descriptor inside a HIDDescriptorList.
pub fn hid_descriptor(lists: &[u8]) -> Option<Vec<u8>> {
    let Element::Seq(list) = find_attr(lists, ATTR_HID_DESCRIPTOR_LIST)? else {
        return None;
    };
    for d in list {
        if let Element::Seq(pair) = d
            && let [Element::Uint(0x22), Element::Bytes(b)] = pair.as_slice()
        {
            return Some(b.to_vec());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_descriptor_list() {
        let desc = [0x05, 0x01, 0x09, 0x06];
        // { { 0x0001: UUID 0x1124 }, { 0x0206: { { 0x22, desc } } } }
        let mut inner = alloc::vec![0x35, 10, 0x35, 8, 0x08, 0x22, 0x25, 4];
        inner.extend_from_slice(&desc);
        let mut rec = alloc::vec![
            0x09, 0x00, 0x01, 0x35, 3, 0x19, 0x11, 0x24, 0x09, 0x02, 0x06
        ];
        rec.extend_from_slice(&inner);
        let mut lists = alloc::vec![0x35, rec.len() as u8 + 2, 0x35, rec.len() as u8];
        lists.extend_from_slice(&rec);
        assert_eq!(hid_descriptor(&lists).unwrap(), desc);
        let req = search_attr_req(1, UUID_HID, &[]);
        assert_eq!(&req[..5], &[SEARCH_ATTR_REQ, 0, 1, 0, 15]);
        let mut rsp = alloc::vec![SEARCH_ATTR_RSP, 0, 1, 0, 0];
        rsp.extend_from_slice(&(lists.len() as u16).to_be_bytes());
        rsp.extend_from_slice(&lists);
        rsp.push(0);
        assert_eq!(search_attr_rsp(&rsp).unwrap(), (&lists[..], &[][..]));
        assert_eq!(element(&[0x11, 0xFF, 0xFE]).unwrap().0, Element::Int(-2));
    }
}
