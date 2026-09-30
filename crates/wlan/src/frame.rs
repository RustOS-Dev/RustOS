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
    /// Band the BSS was heard on (drivers override the guess from the
    /// DS Parameter Set with what the receiver reports).
    pub band: crate::chan::Band,
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
        band: ie::find(ies, ie::DS_PARAMS)
            .and_then(|d| d.first().copied())
            .map_or(crate::chan::Band::B5G, crate::chan::Band::of_legacy),
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
pub fn assoc_request(
    sa: Mac,
    bssid: Mac,
    capability: u16,
    ssid: &[u8],
    band_2g: bool,
    rsn: &[u8],
    extra: &[u8],
) -> Vec<u8> {
    let mut f = mgmt_header(ST_ASSOC_REQ, bssid, sa, bssid);
    f.extend_from_slice(
        &(capability & !0x10 | if rsn.is_empty() { 0 } else { 0x10 }).to_le_bytes(),
    );
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
    if h.ty() != TYPE_MGMT
        || !matches!(h.subtype(), ST_ASSOC_RESP | ST_REASSOC_RESP)
        || f.len() < 30
    {
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
    data_frame(eth, bssid, if qos { Some(0) } else { None }, 0)
}

/// Wrap an Ethernet frame as a data frame to the AP with sequence number
/// `seq`: QoS data for traffic identifier `tid`, or non-QoS data.
pub fn data_frame(eth: &[u8], bssid: Mac, tid: Option<u8>, seq: u16) -> Option<Vec<u8>> {
    if eth.len() < 14 {
        return None;
    }
    let (da, sa, et) = (&eth[0..6], &eth[6..12], &eth[12..14]);
    let sub = if tid.is_some() { ST_QOS_DATA } else { 0 };
    let mut f = Vec::with_capacity(eth.len() + 34);
    f.extend_from_slice(&(fc(TYPE_DATA, sub) | FC_TO_DS).to_le_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&bssid);
    f.extend_from_slice(sa);
    f.extend_from_slice(da);
    f.extend_from_slice(&((seq & 0xFFF) << 4).to_le_bytes());
    if let Some(t) = tid {
        f.extend_from_slice(&[t & 0xF, 0]); // normal ack
    }
    f.extend_from_slice(&LLC_SNAP);
    f.extend_from_slice(et);
    f.extend_from_slice(&eth[14..]);
    Some(f)
}

/// Set the sequence number of a frame (management frames).
pub fn set_seq(f: &mut [u8], seq: u16) {
    if f.len() >= 24 {
        let frag = u16::from_le_bytes([f[22], f[23]]) & 0xF;
        f[22..24].copy_from_slice(&(((seq & 0xFFF) << 4) | frag).to_le_bytes());
    }
}

/// 802.1D user priority (= TID) of an Ethernet frame from its IPv4 DSCP
/// or IPv6 traffic class (the precedence bits), 0 for everything else.
pub fn tid_for_ethernet(eth: &[u8]) -> u8 {
    if eth.len() < 16 {
        return 0;
    }
    match (eth[12], eth[13]) {
        (0x08, 0x00) => eth[15] >> 5,
        (0x86, 0xDD) => (eth[14] & 0x0F) >> 1,
        (0x88, 0x8E) => 6, // EAPOL: voice, as other stacks send it
        _ => 0,
    }
}

/// Offset of the QoS control field (QoS data frames only).
fn qos_offset(h: &Header) -> Option<usize> {
    (h.ty() == TYPE_DATA && h.subtype() & 0x8 != 0).then_some(h.len - 2)
}

/// TID of a QoS data frame.
pub fn qos_tid(f: &[u8]) -> Option<u8> {
    let h = Header::parse(f)?;
    qos_offset(&h).map(|o| f[o] & 0xF)
}

const QOS_AMSDU: u8 = 1 << 7;

/// The QoS control field says the body is an A-MSDU.
pub fn is_amsdu(f: &[u8]) -> bool {
    Header::parse(f)
        .and_then(|h| qos_offset(&h))
        .is_some_and(|o| f[o] & QOS_AMSDU != 0)
}

/// Clear the A-MSDU flag (the hardware already split the A-MSDU and each
/// subframe arrives as its own MPDU).
pub fn clear_amsdu(f: &mut [u8]) {
    if let Some(o) = Header::parse(f).and_then(|h| qos_offset(&h)) {
        f[o] &= !QOS_AMSDU;
    }
}

/// Split a received A-MSDU data frame into Ethernet frames.
pub fn amsdu_to_ethernet(f: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Some(h) = Header::parse(f) else {
        return out;
    };
    if h.protected() {
        return out;
    }
    let mut b = &f[h.len..];
    while b.len() >= 14 {
        let (da, sa) = (&b[0..6], &b[6..12]);
        let len = u16::from_be_bytes([b[12], b[13]]) as usize;
        let Some(msdu) = b.get(14..14 + len) else {
            break;
        };
        let mut eth = Vec::with_capacity(len + 14);
        eth.extend_from_slice(da);
        eth.extend_from_slice(sa);
        if msdu.len() >= 8 && msdu[..6] == LLC_SNAP {
            eth.extend_from_slice(&msdu[6..8]);
            eth.extend_from_slice(&msdu[8..]);
        } else {
            eth.extend_from_slice(&(msdu.len() as u16).to_be_bytes());
            eth.extend_from_slice(msdu);
        }
        out.push(eth);
        let used = (14 + len).next_multiple_of(4);
        if used >= b.len() {
            break;
        }
        b = &b[used..];
    }
    out
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
    if h.protected() || is_amsdu(f) {
        return None; // decrypt / split first
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
    fn qos_seq_and_amsdu() {
        let mut eth = Vec::new();
        eth.extend_from_slice(&AP);
        eth.extend_from_slice(&ME);
        eth.extend_from_slice(&[0x08, 0x00, 0x45, 0xB8]); // DSCP 46 (EF)
        eth.extend_from_slice(&[0; 20]);
        assert_eq!(tid_for_ethernet(&eth), 5);
        let f = data_frame(&eth, AP, Some(5), 0x123).unwrap();
        assert_eq!(qos_tid(&f), Some(5));
        assert_eq!(Header::parse(&f).unwrap().seq, 0x123);
        let mut ip6 = eth.clone();
        ip6[12..16].copy_from_slice(&[0x86, 0xDD, 0x6E, 0x00]); // TC 0xE0
        assert_eq!(tid_for_ethernet(&ip6), 7);
        let mut m = auth(ME, AP, 0, 1, 0, &[]);
        set_seq(&mut m, 4095);
        assert_eq!(Header::parse(&m).unwrap().seq, 4095);

        // A-MSDU with two subframes from the AP.
        let mut a = data_frame(&eth, AP, Some(0), 1).unwrap();
        a.truncate(26);
        let fcv = (u16::from_le_bytes([a[0], a[1]]) & !FC_TO_DS) | FC_FROM_DS;
        a[0..2].copy_from_slice(&fcv.to_le_bytes());
        a[24] |= 0x80;
        for (i, payload) in [&b"first"[..], &b"second!"[..]].iter().enumerate() {
            let mut msdu = LLC_SNAP.to_vec();
            msdu.extend_from_slice(&[0x08, 0x00]);
            msdu.extend_from_slice(payload);
            a.extend_from_slice(&ME);
            a.extend_from_slice(&AP);
            a.extend_from_slice(&(msdu.len() as u16).to_be_bytes());
            a.extend_from_slice(&msdu);
            if i == 0 {
                while (a.len() - 26) % 4 != 0 {
                    a.push(0);
                }
            }
        }
        assert!(is_amsdu(&a));
        assert_eq!(ethernet_from_data(&a), None);
        let e = amsdu_to_ethernet(&a);
        assert_eq!(e.len(), 2);
        assert_eq!(&e[0][14..], b"first");
        assert_eq!(&e[1][14..], b"second!");
        assert_eq!(&e[1][0..6], &ME);
        clear_amsdu(&mut a);
        assert!(!is_amsdu(&a));
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
