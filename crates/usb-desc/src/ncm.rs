//! CDC NCM (Network Control Model) 16-bit transfer blocks.
//!
//! An NTB is an NTH16 header followed by one or more NDP16 datagram
//! pointer tables, each listing (offset, length) pairs of Ethernet frames
//! inside the block.

use alloc::vec::Vec;

pub const NTH16_SIGNATURE: u32 = 0x484D_434E; // "NCMH"
pub const NDP16_NO_CRC: u32 = 0x304D_434E; // "NCM0"
pub const NDP16_CRC: u32 = 0x314D_434E; // "NCM1"
const NTH16_LEN: usize = 12;

/// GET_NTB_PARAMETERS response (the fields the host needs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NtbParams {
    pub in_max: u32,
    pub out_max: u32,
    pub out_divisor: u16,
    pub out_remainder: u16,
    pub out_alignment: u16,
    pub out_max_datagrams: u16,
}

impl NtbParams {
    pub fn parse(b: &[u8]) -> Option<NtbParams> {
        if b.len() < 28 {
            return None;
        }
        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        Some(NtbParams {
            in_max: u32_at(4),
            out_max: u32_at(16),
            out_divisor: u16_at(20).max(1),
            out_remainder: u16_at(22),
            out_alignment: u16_at(24).max(1),
            out_max_datagrams: u16_at(26),
        })
    }
}

fn rd16(b: &[u8], o: usize) -> Option<usize> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]) as usize)
}

fn rd32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Datagrams (Ethernet frames) carried by an NTB-16.
pub fn parse_ntb16(b: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if rd32(b, 0) != Some(NTH16_SIGNATURE) || rd16(b, 4) != Some(NTH16_LEN) {
        return out;
    }
    let block_len = rd16(b, 8).map_or(b.len(), |l| if l == 0 { b.len() } else { l.min(b.len()) });
    let b = &b[..block_len];
    let mut ndp = rd16(b, 10).unwrap_or(0);
    let mut hops = 0;
    while ndp != 0 && hops < 16 {
        hops += 1;
        let Some(sig) = rd32(b, ndp) else { break };
        if sig != NDP16_NO_CRC && sig != NDP16_CRC {
            break;
        }
        let len = rd16(b, ndp + 4).unwrap_or(0);
        let next = rd16(b, ndp + 6).unwrap_or(0);
        let mut e = ndp + 8;
        while e + 4 <= ndp + len {
            let (Some(off), Some(dlen)) = (rd16(b, e), rd16(b, e + 2)) else { break };
            if off == 0 || dlen == 0 {
                break;
            }
            if let Some(d) = b.get(off..off + dlen) {
                out.push(d);
            }
            e += 4;
        }
        if next <= ndp {
            break; // NDPs must move forward
        }
        ndp = next;
    }
    out
}

/// Build an NTB-16 carrying `frames` (as many as fit in `p.out_max`).
/// Returns the block and how many frames it contains.
pub fn build_ntb16(frames: &[&[u8]], seq: u16, p: &NtbParams) -> (Vec<u8>, usize) {
    let max = (p.out_max as usize).clamp(64, 65535);
    let max_dg = if p.out_max_datagrams == 0 { usize::MAX } else { p.out_max_datagrams as usize };
    let divisor = p.out_divisor as usize;
    let rem = p.out_remainder as usize % divisor;
    let ndp_align = (p.out_alignment as usize).clamp(4, 64);
    // Lay out datagrams after a table sized for all of them.
    let mut n = frames.len().min(max_dg);
    loop {
        let ndp_off = NTH16_LEN.next_multiple_of(ndp_align);
        let ndp_len = 8 + (n + 1) * 4;
        let mut pos = ndp_off + ndp_len;
        let mut offsets = Vec::with_capacity(n);
        for f in &frames[..n] {
            // Datagram start ≡ remainder (mod divisor).
            let mut start = pos - pos % divisor + rem;
            if start < pos {
                start += divisor;
            }
            offsets.push(start);
            pos = start + f.len();
        }
        if pos > max && n > 1 {
            n -= 1;
            continue;
        }
        let mut b = alloc::vec![0u8; pos];
        b[0..4].copy_from_slice(&NTH16_SIGNATURE.to_le_bytes());
        b[4..6].copy_from_slice(&(NTH16_LEN as u16).to_le_bytes());
        b[6..8].copy_from_slice(&seq.to_le_bytes());
        b[8..10].copy_from_slice(&(pos as u16).to_le_bytes());
        b[10..12].copy_from_slice(&(ndp_off as u16).to_le_bytes());
        b[ndp_off..ndp_off + 4].copy_from_slice(&NDP16_NO_CRC.to_le_bytes());
        b[ndp_off + 4..ndp_off + 6].copy_from_slice(&(ndp_len as u16).to_le_bytes());
        for (i, (f, &off)) in frames[..n].iter().zip(&offsets).enumerate() {
            let e = ndp_off + 8 + i * 4;
            b[e..e + 2].copy_from_slice(&(off as u16).to_le_bytes());
            b[e + 2..e + 4].copy_from_slice(&(f.len() as u16).to_le_bytes());
            b[off..off + f.len()].copy_from_slice(f);
        }
        return (b, n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> NtbParams {
        NtbParams {
            in_max: 16384,
            out_max: 2048,
            out_divisor: 4,
            out_remainder: 2,
            out_alignment: 4,
            out_max_datagrams: 0,
        }
    }

    #[test]
    fn roundtrip() {
        let a = [1u8; 60];
        let b = [2u8; 100];
        let (ntb, n) = build_ntb16(&[&a, &b], 7, &params());
        assert_eq!(n, 2);
        let got = parse_ntb16(&ntb);
        assert_eq!(got, alloc::vec![&a[..], &b[..]]);
        // Datagrams honour divisor/remainder.
        let off0 = u16::from_le_bytes([ntb[12 + 8], ntb[12 + 9]]) as usize;
        assert_eq!(off0 % 4, 2);
    }

    #[test]
    fn splits_when_full() {
        let big = [3u8; 1500];
        let (ntb, n) = build_ntb16(&[&big, &big], 1, &params());
        assert_eq!(n, 1);
        assert_eq!(parse_ntb16(&ntb).len(), 1);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_ntb16(&[0u8; 64]).is_empty());
        let mut ntb = build_ntb16(&[&[9u8; 20]], 0, &params()).0;
        // Datagram pointing past the end is skipped.
        ntb[12 + 10] = 0xFF;
        assert!(parse_ntb16(&ntb).is_empty());
    }

    #[test]
    fn params_parse() {
        let mut b = [0u8; 28];
        b[4..8].copy_from_slice(&32768u32.to_le_bytes());
        b[16..20].copy_from_slice(&16384u32.to_le_bytes());
        b[20..22].copy_from_slice(&4u16.to_le_bytes());
        b[24..26].copy_from_slice(&4u16.to_le_bytes());
        let p = NtbParams::parse(&b).unwrap();
        assert_eq!((p.in_max, p.out_max, p.out_divisor, p.out_remainder), (32768, 16384, 4, 0));
    }
}
