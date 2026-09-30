//! L2CAP: frames, reassembly of ACL fragments, and the signaling channel
//! (BR/EDR basic-mode connections and LE connection parameter updates and
//! credit-based channels).

use crate::le16;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub const CID_SIGNALING: u16 = 0x0001;
pub const CID_ATT: u16 = 0x0004;
pub const CID_LE_SIGNALING: u16 = 0x0005;
pub const CID_SMP: u16 = 0x0006;
/// First dynamically allocated channel ID.
pub const CID_DYN_START: u16 = 0x0040;

pub const PSM_SDP: u16 = 0x0001;
pub const PSM_HID_CONTROL: u16 = 0x0011;
pub const PSM_HID_INTERRUPT: u16 = 0x0013;

pub const COMMAND_REJECT: u8 = 0x01;
pub const CONN_REQ: u8 = 0x02;
pub const CONN_RSP: u8 = 0x03;
pub const CONF_REQ: u8 = 0x04;
pub const CONF_RSP: u8 = 0x05;
pub const DISCONN_REQ: u8 = 0x06;
pub const DISCONN_RSP: u8 = 0x07;
pub const ECHO_REQ: u8 = 0x08;
pub const ECHO_RSP: u8 = 0x09;
pub const INFO_REQ: u8 = 0x0A;
pub const INFO_RSP: u8 = 0x0B;
pub const CONN_PARAM_UPDATE_REQ: u8 = 0x12;
pub const CONN_PARAM_UPDATE_RSP: u8 = 0x13;
pub const LE_CREDIT_CONN_REQ: u8 = 0x14;
pub const LE_CREDIT_CONN_RSP: u8 = 0x15;
pub const FLOW_CONTROL_CREDIT: u8 = 0x16;

/// Connection response results.
pub const CONN_SUCCESS: u16 = 0x0000;
pub const CONN_PENDING: u16 = 0x0001;

/// A basic L2CAP frame.
pub fn frame(cid: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + payload.len());
    v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    v.extend_from_slice(&cid.to_le_bytes());
    v.extend_from_slice(payload);
    v
}

/// Reassembles L2CAP frames from ACL fragments, per connection handle.
#[derive(Default)]
pub struct Reassembler {
    partial: BTreeMap<u16, Vec<u8>>,
}

impl Reassembler {
    /// Feed one ACL packet's data; returns a complete (cid, payload).
    pub fn feed(&mut self, handle: u16, pb: u8, data: &[u8]) -> Option<(u16, Vec<u8>)> {
        let buf = if pb == crate::hci::PB_CONT {
            let b = self.partial.get_mut(&handle)?;
            b.extend_from_slice(data);
            b
        } else {
            self.partial.insert(handle, data.to_vec());
            self.partial.get_mut(&handle)?
        };
        let len = le16(buf, 0)? as usize;
        if buf.len() < 4 + len {
            return None;
        }
        let b = self.partial.remove(&handle)?;
        Some((le16(&b, 2)?, b[4..4 + len].to_vec()))
    }

    pub fn drop_handle(&mut self, handle: u16) {
        self.partial.remove(&handle);
    }
}

/// One signaling command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signal {
    pub code: u8,
    pub id: u8,
    pub data: Vec<u8>,
}

impl Signal {
    pub fn new(code: u8, id: u8, data: Vec<u8>) -> Signal {
        Signal { code, id, data }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = alloc::vec![self.code, self.id];
        v.extend_from_slice(&(self.data.len() as u16).to_le_bytes());
        v.extend_from_slice(&self.data);
        v
    }

    pub fn u16_at(&self, o: usize) -> u16 {
        le16(&self.data, o).unwrap_or(0)
    }
}

/// The commands in a signaling channel payload (BR/EDR may pack several).
pub fn parse_signals(p: &[u8]) -> Vec<Signal> {
    let mut v = Vec::new();
    let mut o = 0;
    while o + 4 <= p.len() {
        let len = le16(p, o + 2).unwrap_or(0) as usize;
        let Some(d) = p.get(o + 4..o + 4 + len) else {
            break;
        };
        v.push(Signal::new(p[o], p[o + 1], d.to_vec()));
        o += 4 + len;
    }
    v
}

fn words(ws: &[u16]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

pub fn conn_req(id: u8, psm: u16, scid: u16) -> Signal {
    Signal::new(CONN_REQ, id, words(&[psm, scid]))
}

pub fn conn_rsp(id: u8, dcid: u16, scid: u16, result: u16) -> Signal {
    Signal::new(CONN_RSP, id, words(&[dcid, scid, result, 0]))
}

/// Configuration request with an MTU option.
pub fn conf_req(id: u8, dcid: u16, mtu: u16) -> Signal {
    let mut d = words(&[dcid, 0]);
    d.extend_from_slice(&[0x01, 2]);
    d.extend_from_slice(&mtu.to_le_bytes());
    Signal::new(CONF_REQ, id, d)
}

/// Accept a configuration request (echoing no options).
pub fn conf_rsp(id: u8, scid: u16) -> Signal {
    Signal::new(CONF_RSP, id, words(&[scid, 0, 0]))
}

pub fn disconn_req(id: u8, dcid: u16, scid: u16) -> Signal {
    Signal::new(DISCONN_REQ, id, words(&[dcid, scid]))
}

pub fn disconn_rsp(id: u8, dcid: u16, scid: u16) -> Signal {
    Signal::new(DISCONN_RSP, id, words(&[dcid, scid]))
}

/// Reply to anything we do not implement.
pub fn reject(id: u8) -> Signal {
    Signal::new(COMMAND_REJECT, id, words(&[0]))
}

/// Information response: extended features (none beyond basic) or fixed
/// channels (signaling only), else "not supported".
pub fn info_rsp(id: u8, info_type: u16) -> Signal {
    let d = match info_type {
        2 => {
            let mut d = words(&[2, 0]);
            d.extend_from_slice(&0u32.to_le_bytes());
            d
        }
        3 => {
            let mut d = words(&[3, 0]);
            d.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 0, 0]);
            d
        }
        t => words(&[t, 1]),
    };
    Signal::new(INFO_RSP, id, d)
}

/// LE connection parameter update response (0 accepted, 1 rejected).
pub fn conn_param_update_rsp(id: u8, accept: bool) -> Signal {
    Signal::new(CONN_PARAM_UPDATE_RSP, id, words(&[!accept as u16]))
}

/// LE credit-based connection request.
pub fn le_credit_conn_req(id: u8, psm: u16, scid: u16, mtu: u16, mps: u16, credits: u16) -> Signal {
    Signal::new(
        LE_CREDIT_CONN_REQ,
        id,
        words(&[psm, scid, mtu, mps, credits]),
    )
}

pub fn le_credit_conn_rsp(
    id: u8,
    dcid: u16,
    mtu: u16,
    mps: u16,
    credits: u16,
    result: u16,
) -> Signal {
    Signal::new(
        LE_CREDIT_CONN_RSP,
        id,
        words(&[dcid, mtu, mps, credits, result]),
    )
}

pub fn flow_credit(id: u8, cid: u16, credits: u16) -> Signal {
    Signal::new(FLOW_CONTROL_CREDIT, id, words(&[cid, credits]))
}

/// K-frames of an LE credit-based channel: the first carries the SDU
/// length; each is at most `mps` bytes of payload.
pub fn le_sdu_frames(dcid: u16, sdu: &[u8], mps: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut first = (sdu.len() as u16).to_le_bytes().to_vec();
    let n = sdu.len().min(mps.saturating_sub(2));
    first.extend_from_slice(&sdu[..n]);
    out.push(frame(dcid, &first));
    for c in sdu[n..].chunks(mps.max(1)) {
        out.push(frame(dcid, c));
    }
    out
}

/// Collects LE credit-based K-frames into SDUs.
#[derive(Default)]
pub struct SduReassembler {
    want: usize,
    buf: Vec<u8>,
}

impl SduReassembler {
    pub fn feed(&mut self, k: &[u8]) -> Option<Vec<u8>> {
        if self.buf.is_empty() && self.want == 0 {
            self.want = le16(k, 0)? as usize;
            self.buf.extend_from_slice(&k[2..]);
        } else {
            self.buf.extend_from_slice(k);
        }
        if self.buf.len() >= self.want {
            self.want = 0;
            return Some(core::mem::take(&mut self.buf));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hci;

    #[test]
    fn reassembly() {
        let payload: Vec<u8> = (0..100).collect();
        let f = frame(CID_ATT, &payload);
        let mut r = Reassembler::default();
        let pkts = hci::acl_packets(0x40, &f, 27, true);
        let mut got = None;
        for p in &pkts {
            let (h, pb, d) = hci::parse_acl(p).unwrap();
            got = r.feed(h, pb, d);
        }
        assert_eq!(got, Some((CID_ATT, payload)));
        // A continuation without a start is dropped.
        assert_eq!(r.feed(0x40, hci::PB_CONT, &[1, 2]), None);
    }

    #[test]
    fn signaling() {
        let a = conn_req(1, PSM_HID_CONTROL, 0x40).encode();
        let b = conf_req(2, 0x41, 672).encode();
        let mut both = a.clone();
        both.extend_from_slice(&b);
        let s = parse_signals(&both);
        assert_eq!(s.len(), 2);
        assert_eq!(
            (s[0].code, s[0].u16_at(0), s[0].u16_at(2)),
            (CONN_REQ, 0x11, 0x40)
        );
        assert_eq!(s[1].data, [0x41, 0, 0, 0, 1, 2, 0xA0, 0x02]);
        assert_eq!(info_rsp(3, 2).data.len(), 8);
    }

    #[test]
    fn le_sdus() {
        let sdu: Vec<u8> = (0..200u8).collect();
        let frames = le_sdu_frames(0x41, &sdu, 64);
        assert_eq!(frames.len(), 4);
        let mut r = SduReassembler::default();
        let mut out = None;
        for f in &frames {
            out = r.feed(&f[4..]);
        }
        assert_eq!(out.unwrap(), sdu);
    }
}
