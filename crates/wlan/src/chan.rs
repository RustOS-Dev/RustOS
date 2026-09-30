//! Bands, channels and 6 GHz discovery: operating classes, preferred
//! scanning channels (PSC), short SSIDs and the Reduced Neighbor Report
//! element that 2.4/5 GHz APs use to announce co-located 6 GHz APs.

use crate::Mac;
use alloc::vec::Vec;

/// Element id of the Reduced Neighbor Report.
pub const RNR: u8 = 201;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Band {
    #[default]
    B2G,
    B5G,
    B6G,
}

impl Band {
    /// Band of a 2.4 or 5 GHz channel number (6 GHz channel numbers
    /// overlap both, so they must be known from elsewhere).
    pub fn of_legacy(ch: u8) -> Band {
        if ch <= 14 { Band::B2G } else { Band::B5G }
    }

    /// Centre frequency of channel `ch` in MHz.
    pub fn freq(self, ch: u8) -> u32 {
        match self {
            Band::B2G if ch == 14 => 2484,
            Band::B2G => 2407 + 5 * ch as u32,
            Band::B5G => 5000 + 5 * ch as u32,
            // Channel 2 is the odd one out (5935 MHz).
            Band::B6G if ch == 2 => 5935,
            Band::B6G => 5950 + 5 * ch as u32,
        }
    }

    /// Band of a global operating class (IEEE 802.11 Annex E, table E-4).
    pub fn from_op_class(oc: u8) -> Option<Band> {
        match oc {
            81..=84 => Some(Band::B2G),
            115..=130 => Some(Band::B5G),
            131..=137 => Some(Band::B6G),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Band::B2G => "2.4 GHz",
            Band::B5G => "5 GHz",
            Band::B6G => "6 GHz",
        }
    }
}

/// 20 MHz channels of the 6 GHz band (U-NII-5 to U-NII-8).
pub fn channels_6g() -> impl Iterator<Item = u8> {
    (1..=233u8).step_by(4)
}

/// Preferred scanning channels: every fourth 20 MHz channel starting at
/// 5, where APs without a 2.4/5 GHz companion must be found.
pub fn is_psc(ch: u8) -> bool {
    ch % 16 == 5
}

/// Short SSID: the CRC-32 of the SSID (IEEE 802.11-2020 9.4.2.170.3).
pub fn short_ssid(ssid: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in ssid {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One AP from a Reduced Neighbor Report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighbor {
    pub band: Band,
    pub op_class: u8,
    pub channel: u8,
    pub bssid: Option<Mac>,
    pub short_ssid: Option<u32>,
    /// BSS parameters (bit 1: same SSID as the reporting AP, bit 6:
    /// co-located AP), 0 if absent.
    pub params: u8,
}

pub const RNR_SAME_SSID: u8 = 1 << 1;
pub const RNR_COLOCATED: u8 = 1 << 6;

/// Parse a Reduced Neighbor Report element body. Unknown operating
/// classes and malformed trailing data are skipped.
pub fn parse_rnr(mut b: &[u8]) -> Vec<Neighbor> {
    let mut out = Vec::new();
    while b.len() >= 4 {
        let hdr = u16::from_le_bytes([b[0], b[1]]);
        let count = ((hdr >> 4) & 0xF) as usize + 1;
        let len = (hdr >> 8) as usize;
        let (op_class, channel) = (b[2], b[3]);
        b = &b[4..];
        if len == 0 || b.len() < count * len {
            break;
        }
        for i in 0..count {
            let t = &b[i * len..(i + 1) * len];
            // Field order: TBTT offset, BSSID, short SSID, BSS parameters,
            // 20 MHz PSD, MLD parameters; which ones are present follows
            // from the length (Table 9-281).
            let (has_bssid, has_short, has_params) = match len {
                1 => (false, false, false),
                2 => (false, false, true),
                5 => (false, true, false),
                6 => (false, true, true),
                7 => (true, false, false),
                8 | 9 => (true, false, true),
                11 => (true, true, false),
                _ if len >= 12 => (true, true, true),
                _ => (false, false, false),
            };
            let mut o = 1;
            let bssid = if has_bssid {
                o += 6;
                Some(t[1..7].try_into().unwrap())
            } else {
                None
            };
            let short_ssid = if has_short {
                let v = u32::from_le_bytes(t[o..o + 4].try_into().unwrap());
                o += 4;
                Some(v)
            } else {
                None
            };
            let params = if has_params { t[o] } else { 0 };
            if let Some(band) = Band::from_op_class(op_class) {
                out.push(Neighbor {
                    band,
                    op_class,
                    channel,
                    bssid,
                    short_ssid,
                    params,
                });
            }
        }
        b = &b[count * len..];
    }
    out
}

/// All neighbors reported in `ies`.
pub fn neighbors(ies: &[u8]) -> Vec<Neighbor> {
    crate::ie::iter(ies)
        .filter(|(id, _)| *id == RNR)
        .flat_map(|(_, b)| parse_rnr(b))
        .collect()
}

/// 6 GHz channels worth scanning: the PSC channels plus every channel a
/// neighbor report pointed at.
pub fn scan_list_6g(reported: &[Neighbor]) -> Vec<u8> {
    let mut v: Vec<u8> = channels_6g().filter(|&c| is_psc(c)).collect();
    for n in reported.iter().filter(|n| n.band == Band::B6G) {
        if !v.contains(&n.channel) {
            v.push(n.channel);
        }
    }
    v.sort_unstable();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies() {
        assert_eq!(Band::B2G.freq(1), 2412);
        assert_eq!(Band::B2G.freq(14), 2484);
        assert_eq!(Band::B5G.freq(36), 5180);
        assert_eq!(Band::B6G.freq(1), 5955);
        assert_eq!(Band::B6G.freq(2), 5935);
        assert_eq!(Band::B6G.freq(233), 7115);
        assert_eq!(channels_6g().count(), 59);
        let psc: Vec<u8> = channels_6g().filter(|&c| is_psc(c)).collect();
        assert_eq!(psc.len(), 15);
        assert_eq!(&psc[..3], &[5, 21, 37]);
        assert_eq!(Band::from_op_class(131), Some(Band::B6G));
        assert_eq!(Band::from_op_class(128), Some(Band::B5G));
        assert_eq!(Band::from_op_class(81), Some(Band::B2G));
    }

    #[test]
    fn short_ssid_is_crc32() {
        // CRC-32 check value.
        assert_eq!(short_ssid(b"123456789"), 0xCBF4_3926);
        assert_eq!(short_ssid(b""), 0);
    }

    #[test]
    fn rnr_parse() {
        // Two neighbor infos: a 6 GHz AP on channel 33 (op class 131)
        // with BSSID + short SSID + params (length 12, one entry), and a
        // 5 GHz one with only a TBTT offset (length 1, two entries).
        let mut b = Vec::new();
        b.extend_from_slice(&(12u16 << 8).to_le_bytes());
        b.extend_from_slice(&[131, 33]);
        b.push(0xFF);
        b.extend_from_slice(&[2, 0, 0, 0, 0, 7]);
        b.extend_from_slice(&short_ssid(b"home").to_le_bytes());
        b.push(RNR_SAME_SSID | RNR_COLOCATED);
        b.extend_from_slice(&((1u16 << 8) | (1 << 4)).to_le_bytes());
        b.extend_from_slice(&[128, 42, 10, 20]);
        let n = parse_rnr(&b);
        assert_eq!(n.len(), 3);
        assert_eq!(n[0].band, Band::B6G);
        assert_eq!(n[0].channel, 33);
        assert_eq!(n[0].bssid, Some([2, 0, 0, 0, 0, 7]));
        assert_eq!(n[0].short_ssid, Some(short_ssid(b"home")));
        assert_eq!(n[0].params, RNR_SAME_SSID | RNR_COLOCATED);
        assert_eq!((n[1].band, n[1].bssid), (Band::B5G, None));
        let mut ies = Vec::new();
        crate::ie::push(&mut ies, RNR, &b);
        let list = scan_list_6g(&neighbors(&ies));
        assert!(list.contains(&33) && list.contains(&5) && !list.contains(&42));
        // Truncated data does not panic.
        assert!(parse_rnr(&b[..10]).is_empty());
    }
}
