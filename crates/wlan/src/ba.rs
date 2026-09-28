//! Block Ack (802.11n aggregation): ADDBA/DELBA action frames, sequence
//! number arithmetic, the receive reorder buffer and replay counters.

use crate::Mac;
use crate::frame;
use alloc::vec::Vec;

pub const CATEGORY_BLOCK_ACK: u8 = 3;
pub const ACTION_ADDBA_REQ: u8 = 0;
pub const ACTION_ADDBA_RESP: u8 = 1;
pub const ACTION_DELBA: u8 = 2;

pub const STATUS_SUCCESS: u16 = 0;
pub const STATUS_DECLINED: u16 = 37;
pub const REASON_UNSPECIFIED: u16 = 1;
pub const REASON_END_BA: u16 = 37;

/// Sequence numbers are 12 bits.
pub const SN_MASK: u16 = 0xFFF;

pub fn sn_add(a: u16, b: u16) -> u16 {
    a.wrapping_add(b) & SN_MASK
}
pub fn sn_inc(a: u16) -> u16 {
    sn_add(a, 1)
}
pub fn sn_sub(a: u16, b: u16) -> u16 {
    a.wrapping_sub(b) & SN_MASK
}
/// `a` is before `b` (modulo 4096, half-window comparison).
pub fn sn_less(a: u16, b: u16) -> bool {
    sn_sub(a, b) > 2048
}

/// Block Ack Parameter Set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub amsdu: bool,
    pub immediate: bool,
    pub tid: u8,
    pub buf_size: u16,
}

impl Params {
    pub fn encode(&self) -> u16 {
        (self.amsdu as u16)
            | ((self.immediate as u16) << 1)
            | ((self.tid as u16 & 0xF) << 2)
            | ((self.buf_size & 0x3FF) << 6)
    }
    pub fn decode(v: u16) -> Params {
        Params {
            amsdu: v & 1 != 0,
            immediate: v & 2 != 0,
            tid: ((v >> 2) & 0xF) as u8,
            buf_size: v >> 6,
        }
    }
}

/// A Block Ack action frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    AddbaRequest {
        token: u8,
        params: Params,
        timeout: u16,
        ssn: u16,
    },
    AddbaResponse {
        token: u8,
        status: u16,
        params: Params,
        timeout: u16,
    },
    Delba {
        tid: u8,
        initiator: bool,
        reason: u16,
    },
}

fn action_frame(sa: Mac, bssid: Mac, body: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(24 + body.len());
    f.extend_from_slice(&frame::fc(frame::TYPE_MGMT, frame::ST_ACTION).to_le_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&bssid);
    f.extend_from_slice(&sa);
    f.extend_from_slice(&bssid);
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(body);
    f
}

impl Action {
    /// Encode as a management action frame from `sa` to the AP.
    pub fn frame(&self, sa: Mac, bssid: Mac) -> Vec<u8> {
        let mut b = alloc::vec![CATEGORY_BLOCK_ACK];
        match *self {
            Action::AddbaRequest {
                token,
                params,
                timeout,
                ssn,
            } => {
                b.extend_from_slice(&[ACTION_ADDBA_REQ, token]);
                b.extend_from_slice(&params.encode().to_le_bytes());
                b.extend_from_slice(&timeout.to_le_bytes());
                b.extend_from_slice(&((ssn & SN_MASK) << 4).to_le_bytes());
            }
            Action::AddbaResponse {
                token,
                status,
                params,
                timeout,
            } => {
                b.extend_from_slice(&[ACTION_ADDBA_RESP, token]);
                b.extend_from_slice(&status.to_le_bytes());
                b.extend_from_slice(&params.encode().to_le_bytes());
                b.extend_from_slice(&timeout.to_le_bytes());
            }
            Action::Delba {
                tid,
                initiator,
                reason,
            } => {
                b.push(ACTION_DELBA);
                let p = ((initiator as u16) << 11) | ((tid as u16 & 0xF) << 12);
                b.extend_from_slice(&p.to_le_bytes());
                b.extend_from_slice(&reason.to_le_bytes());
            }
        }
        action_frame(sa, bssid, &b)
    }

    /// Parse the body of an action frame (after the 24-byte header).
    pub fn parse(body: &[u8]) -> Option<Action> {
        if body.first() != Some(&CATEGORY_BLOCK_ACK) {
            return None;
        }
        let le = |o: usize| body.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
        match *body.get(1)? {
            ACTION_ADDBA_REQ => Some(Action::AddbaRequest {
                token: *body.get(2)?,
                params: Params::decode(le(3)?),
                timeout: le(5)?,
                ssn: le(7)? >> 4,
            }),
            ACTION_ADDBA_RESP => Some(Action::AddbaResponse {
                token: *body.get(2)?,
                status: le(3)?,
                params: Params::decode(le(5)?),
                timeout: le(7)?,
            }),
            ACTION_DELBA => {
                let p = le(2)?;
                Some(Action::Delba {
                    tid: (p >> 12) as u8,
                    initiator: p & (1 << 11) != 0,
                    reason: le(4)?,
                })
            }
            _ => None,
        }
    }
}

/// What the reorder buffer did with a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict<T> {
    /// Deliver the frame now (in order).
    Pass(T),
    /// Held (or dropped); deliver `released` (possibly empty) now.
    Held(Vec<T>),
}

/// Receive reorder buffer for one Block Ack session. The hardware tells
/// us for every frame its sequence number and the new "next expected"
/// sequence number (NSSN); frames are held until the NSSN moves past them
/// (the iwlwifi model: the firmware tracks the window and timeouts).
pub struct Reorder<T> {
    head: u16,
    slots: Vec<Vec<T>>,
    stored: usize,
    valid: bool,
}

impl<T> Reorder<T> {
    pub fn new(ssn: u16, size: u16) -> Reorder<T> {
        let size = size.clamp(1, 1024) as usize;
        let mut slots = Vec::with_capacity(size);
        slots.resize_with(size, Vec::new);
        Reorder {
            head: ssn & SN_MASK,
            slots,
            stored: 0,
            valid: false,
        }
    }

    pub fn head(&self) -> u16 {
        self.head
    }
    pub fn stored(&self) -> usize {
        self.stored
    }

    /// Process a received frame. `old` means the hardware saw it behind
    /// the window; `dup` a duplicate. For A-MSDU subframes `amsdu` is set
    /// and `last` marks the final subframe (NSSN is only trusted there).
    #[allow(clippy::too_many_arguments)]
    pub fn rx(
        &mut self,
        item: T,
        sn: u16,
        nssn: u16,
        old: bool,
        dup: bool,
        amsdu: bool,
        last: bool,
    ) -> Verdict<T> {
        let (sn, nssn) = (sn & SN_MASK, nssn & SN_MASK);
        if !self.valid {
            if old {
                return Verdict::Pass(item);
            }
            self.valid = true;
        }
        if dup || old {
            return Verdict::Held(Vec::new());
        }
        let whole = !amsdu || last;
        if self.stored == 0 && sn_less(sn, nssn) {
            if whole {
                self.head = nssn;
            }
            return Verdict::Pass(item);
        }
        if self.stored == 0 && sn == self.head {
            if whole {
                self.head = sn_inc(self.head);
            }
            return Verdict::Pass(item);
        }
        let n = self.slots.len();
        self.slots[sn as usize % n].push(item);
        self.stored += 1;
        if whole {
            Verdict::Held(self.release(nssn))
        } else {
            Verdict::Held(Vec::new())
        }
    }

    /// Release every held frame before `nssn` (firmware frame-release and
    /// BAR notifications) and move the window start to `nssn`.
    pub fn release(&mut self, nssn: u16) -> Vec<T> {
        let nssn = nssn & SN_MASK;
        let mut out = Vec::new();
        let n = self.slots.len();
        let mut sn = self.head;
        // Never walk more than a full window (and a stale NSSN far behind
        // the head is a no-op).
        let mut steps = 0;
        while sn_less(sn, nssn) && steps < 4096 {
            let slot = &mut self.slots[sn as usize % n];
            self.stored -= slot.len();
            out.append(slot);
            sn = sn_inc(sn);
            steps += 1;
            if self.stored == 0 && steps >= n {
                break;
            }
        }
        if sn_less(self.head, nssn) {
            self.head = nssn;
        }
        out
    }

    /// Everything still held, in sequence order (session teardown).
    pub fn flush(&mut self) -> Vec<T> {
        let n = self.slots.len();
        let mut out = Vec::new();
        for i in 0..n {
            let slot = &mut self.slots[(self.head as usize + i) % n];
            out.append(slot);
        }
        self.stored = 0;
        out
    }
}

/// CCMP/GCMP packet number from the 8-byte security header.
pub fn ccmp_pn(h: &[u8]) -> Option<u64> {
    if h.len() < 8 || h[3] & 0x20 == 0 {
        return None; // no Ext IV
    }
    Some(
        h[0] as u64
            | (h[1] as u64) << 8
            | (h[4] as u64) << 16
            | (h[5] as u64) << 24
            | (h[6] as u64) << 32
            | (h[7] as u64) << 40,
    )
}

/// Replay counters: one per TID (0-15) plus one for non-QoS and robust
/// management frames (index 16).
#[derive(Clone, Debug)]
pub struct Replay {
    last: [u64; 17],
}

impl Default for Replay {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Replay {
    /// `start`: the receive sequence counter supplied with the key.
    pub fn new(start: u64) -> Replay {
        Replay { last: [start; 17] }
    }
    /// Accept `pn` for counter `idx` if it is newer (or equal, for the
    /// later subframes of one A-MSDU).
    pub fn check(&mut self, idx: usize, pn: u64, allow_same: bool) -> bool {
        let last = &mut self.last[idx.min(16)];
        if pn > *last || (allow_same && pn == *last && pn != 0) {
            *last = pn;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Mac = [2, 0, 0, 0, 0, 1];
    const AP: Mac = [2, 0, 0, 0, 0, 2];

    #[test]
    fn action_round_trip() {
        let req = Action::AddbaRequest {
            token: 7,
            params: Params {
                amsdu: false,
                immediate: true,
                tid: 5,
                buf_size: 64,
            },
            timeout: 0,
            ssn: 4000,
        };
        let f = req.frame(ME, AP);
        assert_eq!(f.len(), 24 + 9);
        assert_eq!(&f[4..10], &AP);
        assert_eq!(Action::parse(&f[24..]), Some(req));
        // Parameter set bits: policy bit 1, TID bits 2-5, size bits 6-15.
        assert_eq!(u16::from_le_bytes([f[27], f[28]]), 2 | (5 << 2) | (64 << 6));
        let resp = Action::AddbaResponse {
            token: 7,
            status: 0,
            params: Params {
                amsdu: true,
                immediate: true,
                tid: 5,
                buf_size: 256,
            },
            timeout: 0,
        };
        assert_eq!(Action::parse(&resp.frame(ME, AP)[24..]), Some(resp));
        let d = Action::Delba {
            tid: 3,
            initiator: true,
            reason: REASON_END_BA,
        };
        let f = d.frame(ME, AP);
        assert_eq!(u16::from_le_bytes([f[26], f[27]]), (1 << 11) | (3 << 12));
        assert_eq!(Action::parse(&f[24..]), Some(d));
        assert_eq!(Action::parse(&[4, 0]), None);
    }

    #[test]
    fn sn_arithmetic() {
        assert!(sn_less(4095, 0));
        assert!(!sn_less(0, 4095));
        assert!(sn_less(10, 11));
        assert_eq!(sn_inc(4095), 0);
        assert_eq!(sn_sub(2, 4094), 4);
    }

    fn pass<T: core::fmt::Debug>(v: Verdict<T>) -> T {
        match v {
            Verdict::Pass(t) => t,
            v => panic!("expected pass, got {:?}", v),
        }
    }
    fn held<T: core::fmt::Debug>(v: Verdict<T>) -> Vec<T> {
        match v {
            Verdict::Held(t) => t,
            v => panic!("expected held, got {:?}", v),
        }
    }

    #[test]
    fn reorder_in_order_and_holes() {
        let mut r = Reorder::new(100, 64);
        // In order: NSSN = sn + 1, passes straight through.
        assert_eq!(pass(r.rx(100, 100, 101, false, false, false, false)), 100);
        assert_eq!(pass(r.rx(101, 101, 102, false, false, false, false)), 101);
        // 102 lost; 103 and 104 arrive: the firmware keeps NSSN at 102.
        assert!(held(r.rx(103, 103, 102, false, false, false, false)).is_empty());
        assert!(held(r.rx(104, 104, 102, false, false, false, false)).is_empty());
        assert_eq!(r.stored(), 2);
        // Retransmitted 102 fills the hole: NSSN jumps to 105.
        assert_eq!(
            held(r.rx(102, 102, 105, false, false, false, false)),
            [102, 103, 104]
        );
        assert_eq!(r.head(), 105);
        assert_eq!(r.stored(), 0);
        // Duplicates and old frames are dropped.
        assert!(held(r.rx(90, 90, 105, true, false, false, false)).is_empty());
        assert!(held(r.rx(104, 104, 105, false, true, false, false)).is_empty());
    }

    #[test]
    fn reorder_release_and_wrap() {
        let mut r = Reorder::new(4094, 64);
        assert!(held(r.rx(1, 1, 4094, false, false, false, false)).is_empty());
        assert!(held(r.rx(0, 0, 4094, false, false, false, false)).is_empty());
        // Firmware timer: frame release up to 2 (4094 and 4095 lost).
        assert_eq!(r.release(2), [0, 1]);
        assert_eq!(r.head(), 2);
        // A stale release behind the head changes nothing.
        assert!(r.release(1).is_empty());
        assert_eq!(r.head(), 2);
        // A-MSDU: subframes held until the last one.
        assert!(held(r.rx(10, 3, 2, false, false, true, false)).is_empty());
        assert!(held(r.rx(11, 3, 2, false, false, true, false)).is_empty());
        assert_eq!(held(r.rx(12, 3, 4, false, false, true, true)), [10, 11, 12]);
        assert!(r.release(4).is_empty());
        let mut r = Reorder::new(0, 8);
        let _ = r.rx(5, 5, 0, false, false, false, false);
        assert_eq!(r.flush(), [5]);
    }

    #[test]
    fn first_frame_old_passes() {
        let mut r = Reorder::new(10, 64);
        assert_eq!(pass(r.rx(1, 5, 10, true, false, false, false)), 1);
    }

    #[test]
    fn replay() {
        let hdr = [0x01, 0x02, 0x00, 0x20, 0x03, 0x04, 0x05, 0x06];
        assert_eq!(ccmp_pn(&hdr), Some(0x0605_0403_0201));
        assert_eq!(ccmp_pn(&[0; 8]), None);
        let mut r = Replay::new(5);
        assert!(!r.check(0, 5, false));
        assert!(r.check(0, 6, false));
        assert!(!r.check(0, 6, false));
        assert!(r.check(0, 6, true));
        assert!(r.check(1, 6, false)); // counters are per TID
        assert!(!r.check(0, 3, false));
    }
}
