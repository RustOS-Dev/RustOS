//! 802.11 MAC frames: management frame encode/decode and 802.11 <-> 802.3
//! data conversion.

use crate::Mac;
use crate::ie;
use alloc::vec::Vec;

pub const TYPE_MGMT: u8 = 0;
pub const TYPE_CTRL: u8 = 1;
pub const TYPE_DATA: u8 = 2;

pub const ST_ASSOC_REQ: u8 = 0;
pub const ST_ASSOC_RESP: u8 = 1;
pub const ST_REASSOC_RESP: u8 = 3;
pub const ST_PROBE_REQ: u8 = 4;
pub const ST_PROBE_RESP: u8 = 5;
pub const ST_BEACON: u8 = 8;
pub const ST_DISASSOC: u8 = 10;
pub const ST_AUTH: u8 = 11;
pub const ST_DEAUTH: u8 = 12;
pub const ST_ACTION: u8 = 13;
pub const ST_QOS_DATA: u8 = 8;

pub const FC_TO_DS: u16 = 1 << 8;
pub const FC_FROM_DS: u16 = 1 << 9;
pub const FC_PROTECTED: u16 = 1 << 14;

pub const AUTH_OPEN: u16 = 0;
pub const AUTH_SAE: u16 = 3;

pub const ETHERTYPE_EAPOL: u16 = 0x888E;

pub const STATUS_SUCCESS: u16 = 0;
pub const STATUS_ANTI_CLOGGING: u16 = 76;
pub const STATUS_UNSUPPORTED_GROUP: u16 = 77;
pub const STATUS_SAE_HASH_TO_ELEMENT: u16 = 126;

pub fn fc(ty: u8, subtype: u8) -> u16 {
    ((ty as u16) << 2) | ((subtype as u16) << 4)
}

/// Parsed 802.11 header fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub fc: u16,
    pub addr1: Mac,
    pub addr2: Mac,
    pub addr3: Mac,
    pub seq: u16,
    /// Header length (24, +2 for QoS, +6 for 4-address frames).
    pub len: usize,
}

impl Header {
    pub fn ty(&self) -> u8 {
        ((self.fc >> 2) & 3) as u8
    }
    pub fn subtype(&self) -> u8 {
        ((self.fc >> 4) & 0xF) as u8
    }
    pub fn protected(&self) -> bool {
        self.fc & FC_PROTECTED != 0
    }

    pub fn parse(f: &[u8]) -> Option<Header> {
        if f.len() < 24 {
            return None;
        }
        let fc = u16::from_le_bytes([f[0], f[1]]);
        let mac = |o: usize| -> Mac { f[o..o + 6].try_into().unwrap() };
        let mut len = 24;
        let ty = ((fc >> 2) & 3) as u8;
        if ty == TYPE_DATA {
            if fc & (FC_TO_DS | FC_FROM_DS) == (FC_TO_DS | FC_FROM_DS) {
                len += 6;
            }
            if (fc >> 4) & 0x8 != 0 {
                len += 2; // QoS control
            }
        }
        if f.len() < len {
            return None;
        }
        Some(Header {
            fc,
            addr1: mac(4),
            addr2: mac(10),
            addr3: mac(16),
            seq: u16::from_le_bytes([f[22], f[23]]) >> 4,
            len,
        })
    }
}

fn mgmt_header(subtype: u8, da: Mac, sa: Mac, bssid: Mac) -> Vec<u8> {
    let mut f = Vec::with_capacity(128);
    f.extend_from_slice(&fc(TYPE_MGMT, subtype).to_le_bytes());
    f.extend_from_slice(&[0, 0]); // duration (filled by hardware)
    f.extend_from_slice(&da);
    f.extend_from_slice(&sa);
    f.extend_from_slice(&bssid);
    f.extend_from_slice(&[0, 0]); // sequence (assigned by hardware)
    f
}

/// Beacon or probe response contents.
#[derive(Clone, Debug)]
pub struct BssInfo {
    pub bssid: Mac,
    pub ssid: Vec<u8>,
    pub capability: u16,
    pub beacon_interval: u16,
    pub channel: Option<u8>,
    pub ies: Vec<u8>,
}

pub fn parse_beacon(f: &[u8]) -> Option<BssInfo> {
    let h = Header::parse(f)?;
    if h.ty() != TYPE_MGMT || !matches!(h.subtype(), ST_BEACON | ST_PROBE_RESP) || f.len() < 36 {
        return None;
    }
    let body = &f[24..];
    let ies = &body[12..];
    Some(BssInfo {
        bssid: h.addr3,
        ssid: ie::find(ies, ie::SSID).unwrap_or(&[]).to_vec(),
        capability: u16::from_le_bytes([body[10], body[11]]),
        beacon_interval: u16::from_le_bytes([body[8], body[9]]),
        channel: ie::find(ies, ie::DS_PARAMS).and_then(|d| d.first().copied()),
        ies: ies.to_vec(),
    })
}

pub fn probe_request(sa: Mac, ssid: &[u8], band_2g: bool) -> Vec<u8> {
    let mut f = mgmt_header(ST_PROBE_REQ, crate::BROADCAST, sa, crate::BROADCAST);
    ie::push(&mut f, ie::SSID, ssid);
    let (r, x) = ie::rates(band_2g);
    ie::push(&mut f, ie::SUPP_RATES, &r);
    if !x.is_empty() {
        ie::push(&mut f, ie::EXT_RATES, &x);
    }
    f
}

pub fn auth(sa: Mac, bssid: Mac, algo: u16, seq: u16, status: u16, body: &[u8]) -> Vec<u8> {
    let mut f = mgmt_header(ST_AUTH, bssid, sa, bssid);
    f.extend_from_slice(&algo.to_le_bytes());
    f.extend_from_slice(&seq.to_le_bytes());
    f.extend_from_slice(&status.to_le_bytes());
    f.extend_from_slice(body);
    f
}

/// Authentication frame: (algorithm, transaction sequence, status, body).
pub fn parse_auth(f: &[u8]) -> Option<(u16, u16, u16, &[u8])> {
    let h = Header::parse(f)?;
    if h.ty() != TYPE_MGMT || h.subtype() != ST_AUTH || f.len() < 30 {
        return None;
    }
    let b = &f[24..];
    Some((
        u16::from_le_bytes([b[0], b[1]]),
        u16::from_le_bytes([b[2], b[3]]),
        u16::from_le_bytes([b[4], b[5]]),
        &b[6..],
    ))
}

/// Association request. `rsn` is the full RSN element (empty for open),
/// `extra` further elements (HT capabilities, RSNXE, ...).
pub fn assoc_request(sa: Mac, bssid: Mac, capability: u16, ssid: &[u8], band_2g: bool, rsn: &[u8], extra: &[u8]) -> Vec<u8> {
    let mut f = mgmt_header(ST_ASSOC_REQ, bssid, sa, bssid);
    f.extend_from_slice(&(capability & !0x10 | if rsn.is_empty() { 0 } else { 0x10 }).to_le_bytes());
    f.extend_from_slice(&10u16.to_le_bytes()); // listen interval
    ie::push(&mut f, ie::SSID, ssid);
    let (r, x) = ie::rates(band_2g);
    ie::push(&mut f, ie::SUPP_RATES, &r);
    if !x.is_empty() {
        ie::push(&mut f, ie::EXT_RATES, &x);
    }
    f.extend_from_slice(rsn);
    f.extend_from_slice(extra);
    f
}

/// Association response: (status, association id, elements).
pub fn parse_assoc_response(f: &[u8]) -> Option<(u16, u16, &[u8])> {
    let h = Header::parse(f)?;
    if h.ty() != TYPE_MGMT || !matches!(h.subtype(), ST_ASSOC_RESP | ST_REASSOC_RESP) || f.len() < 30 {
        return None;
    }
    let b = &f[24..];
    Some((
        u16::from_le_bytes([b[2], b[3]]),
        u16::from_le_bytes([b[4], b[5]]) & 0x3FFF,
        &b[6..],
    ))
}

pub fn deauth(sa: Mac, bssid: Mac, reason: u16) -> Vec<u8> {
    let mut f = mgmt_header(ST_DEAUTH, bssid, sa, bssid);
    f.extend_from_slice(&reason.to_le_bytes());
    f
}

/// Deauthentication or disassociation: reason code.
pub fn parse_deauth(f: &[u8]) -> Option<u16> {
    let h = Header::parse(f)?;
    if h.ty() != TYPE_MGMT || !matches!(h.subtype(), ST_DEAUTH | ST_DISASSOC) || f.len() < 26 {
        return None;
    }
    Some(u16::from_le_bytes([f[24], f[25]]))
}

const LLC_SNAP: [u8; 6] = [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00];

/// Wrap an Ethernet frame for transmission to the AP (ToDS, QoS data,
/// best-effort TID). Protection is added by the hardware.
pub fn data_from_ethernet(eth: &[u8], bssid: Mac, qos: bool) -> Option<Vec<u8>> {
    if eth.len() < 14 {
        return None;
    }
    let (da, sa, et) = (&eth[0..6], &eth[6..12], &eth[12..14]);
    let sub = if qos { ST_QOS_DATA } else { 0 };
    let mut f = Vec::with_capacity(eth.len() + 34);
    f.extend_from_slice(&(fc(TYPE_DATA, sub) | FC_TO_DS).to_le_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&bssid);
    f.extend_from_slice(sa);
    f.extend_from_slice(da);
    f.extend_from_slice(&[0, 0]);
    if qos {
        f.extend_from_slice(&[0, 0]);
    }
    f.extend_from_slice(&LLC_SNAP);
    f.extend_from_slice(et);
    f.extend_from_slice(&eth[14..]);
    Some(f)
}

/// Unwrap a received (already decrypted) data frame into Ethernet.
pub fn ethernet_from_data(f: &[u8]) -> Option<Vec<u8>> {
    let h = Header::parse(f)?;
    if h.ty() != TYPE_DATA || h.subtype() & 0x4 != 0 {
        return None; // not data, or a null-data frame
    }
    let (da, sa) = match h.fc & (FC_TO_DS | FC_FROM_DS) {
        FC_FROM_DS => (h.addr1, h.addr3),
        0 => (h.addr1, h.addr2),
        FC_TO_DS => (h.addr3, h.addr2),
        _ => (h.addr3, f.get(24..30)?.try_into().ok()?),
    };
    let mut body = &f[h.len..];
    if h.protected() {
        return None; // caller must decrypt (or hardware must strip) first
    }
    let mut eth = Vec::with_capacity(body.len() + 14);
    eth.extend_from_slice(&da);
    eth.extend_from_slice(&sa);
    if body.len() >= 8 && body[..6] == LLC_SNAP {
        eth.extend_from_slice(&body[6..8]);
        body = &body[8..];
    } else {
        eth.extend_from_slice(&(body.len() as u16).to_be_bytes());
    }
    eth.extend_from_slice(body);
    Some(eth)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Mac = [2, 0, 0, 0, 0, 1];
    const AP: Mac = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];

    #[test]
    fn data_round_trip() {
        let mut eth = Vec::new();
        eth.extend_from_slice(&AP);
        eth.extend_from_slice(&ME);
        eth.extend_from_slice(&[0x08, 0x00]);
        eth.extend_from_slice(b"payload");
        let f = data_from_ethernet(&eth, AP, true).unwrap();
        let h = Header::parse(&f).unwrap();
        assert_eq!(h.len, 26);
        assert_eq!(h.addr1, AP);
        // As the AP would forward it back to us (FromDS).
        let mut back = f.clone();
        let fc = (u16::from_le_bytes([back[0], back[1]]) & !FC_TO_DS) | FC_FROM_DS;
        back[0..2].copy_from_slice(&fc.to_le_bytes());
        back[4..10].copy_from_slice(&ME);
        back[10..16].copy_from_slice(&AP);
        back[16..22].copy_from_slice(&AP);
        let e = ethernet_from_data(&back).unwrap();
        assert_eq!(&e[0..6], &ME);
        assert_eq!(&e[12..14], &[0x08, 0x00]);
        assert_eq!(&e[14..], b"payload");
    }

    #[test]
    fn mgmt_frames() {
        let a = auth(ME, AP, AUTH_OPEN, 1, 0, &[]);
        assert_eq!(parse_auth(&a), Some((AUTH_OPEN, 1, 0, &[][..])));
        let d = deauth(ME, AP, 3);
        assert_eq!(parse_deauth(&d), Some(3));
        let r = assoc_request(ME, AP, 0x0431, b"net", true, &[], &[]);
        let h = Header::parse(&r).unwrap();
        assert_eq!(h.subtype(), ST_ASSOC_REQ);
        assert_eq!(ie::find(&r[28..], ie::SSID), Some(&b"net"[..]));
    }

    #[test]
    fn beacon_parse() {
        let mut f = mgmt_header(ST_BEACON, crate::BROADCAST, AP, AP);
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(&100u16.to_le_bytes());
        f.extend_from_slice(&0x0411u16.to_le_bytes());
        ie::push(&mut f, ie::SSID, b"cafe");
        ie::push(&mut f, ie::DS_PARAMS, &[6]);
        let b = parse_beacon(&f).unwrap();
        assert_eq!(b.ssid, b"cafe");
        assert_eq!(b.channel, Some(6));
        assert_eq!(b.bssid, AP);
    }
}
