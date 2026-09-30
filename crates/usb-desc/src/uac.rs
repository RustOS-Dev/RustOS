//! USB Audio Class 1 and 2: the class-specific descriptors of the
//! AudioControl interface (feature units) and of AudioStreaming alternate
//! settings (PCM format, channels, sample rates).

use crate::{Interface, TransferType};
use alloc::vec::Vec;

pub const CLASS_AUDIO: u8 = 1;
pub const SUBCLASS_CONTROL: u8 = 1;
pub const SUBCLASS_STREAMING: u8 = 2;
/// Interface protocol of UAC 2.0.
pub const PROTO_UAC2: u8 = 0x20;
const CS_INTERFACE: u8 = 0x24;

/// Sample rates an alternate setting offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rates {
    Discrete(Vec<u32>),
    Continuous(u32, u32),
    /// UAC2: from the clock source (GET RANGE).
    FromClock,
}

impl Rates {
    pub fn supports(&self, r: u32) -> bool {
        match self {
            Rates::Discrete(v) => v.contains(&r),
            Rates::Continuous(lo, hi) => (*lo..=*hi).contains(&r),
            Rates::FromClock => true,
        }
    }
}

/// A PCM alternate setting of an AudioStreaming interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamFormat {
    pub interface: u8,
    pub alternate: u8,
    pub uac2: bool,
    pub channels: u8,
    /// Bytes per sample in the stream.
    pub subframe: u8,
    pub bits: u8,
    pub rates: Rates,
    /// The isochronous data endpoint.
    pub endpoint: crate::Endpoint,
    /// Terminal the stream connects to.
    pub terminal: u8,
}

/// Class-specific interface descriptors in `extra`: (subtype, body).
fn cs_descriptors(extra: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut o = 0;
    core::iter::from_fn(move || {
        while o + 2 <= extra.len() {
            let len = extra[o] as usize;
            if len < 3 || o + len > extra.len() {
                return None;
            }
            let d = &extra[o..o + len];
            o += len;
            if d[1] == CS_INTERFACE {
                return Some((d[2], &d[3..]));
            }
        }
        None
    })
}

/// The PCM format of an AudioStreaming alternate setting, if it has one.
pub fn stream_format(i: &Interface) -> Option<StreamFormat> {
    if i.class != CLASS_AUDIO || i.subclass != SUBCLASS_STREAMING || i.alternate == 0 {
        return None;
    }
    let uac2 = i.protocol == PROTO_UAC2;
    let endpoint =
        i.endpoints.iter().copied().find(|e| {
            e.transfer_type() == TransferType::Isochronous && e.attributes & 0x30 != 0x10
        })?;
    let mut terminal = 0;
    let mut pcm = false;
    let mut channels = 0;
    let mut fmt = None;
    for (sub, b) in cs_descriptors(&i.extra) {
        match sub {
            // AS_GENERAL
            1 if !uac2 && b.len() >= 4 => {
                terminal = b[0];
                pcm = u16::from_le_bytes([b[2], b[3]]) == 1;
            }
            1 if uac2 && b.len() >= 13 => {
                terminal = b[0];
                pcm = b[2] == 1 && u32::from_le_bytes([b[3], b[4], b[5], b[6]]) & 1 != 0;
                channels = b[7];
            }
            // FORMAT_TYPE (type I)
            2 if b.first() == Some(&1) => {
                if uac2 && b.len() >= 3 {
                    fmt = Some((channels, b[1], b[2], Rates::FromClock));
                } else if b.len() >= 5 {
                    let (ch, sub, bits, n) = (b[1], b[2], b[3], b[4] as usize);
                    let rate = |k: usize| {
                        b.get(5 + 3 * k..8 + 3 * k)
                            .map(|r| r[0] as u32 | (r[1] as u32) << 8 | (r[2] as u32) << 16)
                    };
                    let rates = if n == 0 {
                        Rates::Continuous(rate(0)?, rate(1)?)
                    } else {
                        Rates::Discrete((0..n).filter_map(rate).collect())
                    };
                    fmt = Some((ch, sub, bits, rates));
                }
            }
            _ => {}
        }
    }
    let (channels, subframe, bits, rates) = fmt?;
    pcm.then_some(StreamFormat {
        interface: i.number,
        alternate: i.alternate,
        uac2,
        channels,
        subframe,
        bits,
        rates,
        endpoint,
        terminal,
    })
}

/// A feature unit (volume and mute) in an AudioControl interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeatureUnit {
    pub id: u8,
    pub source: u8,
    /// Master channel controls (bit 0 mute, bit 1 volume).
    pub mute: bool,
    pub volume: bool,
}

/// Feature units of an AudioControl interface.
pub fn feature_units(ac: &Interface) -> Vec<FeatureUnit> {
    let uac2 = ac.protocol == PROTO_UAC2;
    cs_descriptors(&ac.extra)
        .filter(|(sub, b)| *sub == 6 && b.len() >= 3)
        .map(|(_, b)| {
            let (mute, volume) = if uac2 {
                // 4-byte bmaControls, 2 bits per control.
                let c = b
                    .get(2..6)
                    .map_or(0, |x| u32::from_le_bytes(x.try_into().unwrap()));
                (c & 3 != 0, (c >> 2) & 3 != 0)
            } else {
                let size = b[2] as usize;
                let c = b.get(3).copied().unwrap_or(0) as u32
                    | if size > 1 {
                        (b.get(4).copied().unwrap_or(0) as u32) << 8
                    } else {
                        0
                    };
                (c & 1 != 0, c & 2 != 0)
            };
            FeatureUnit {
                id: b[0],
                source: b[1],
                mute,
                volume,
            }
        })
        .collect()
}

/// UAC2 clock sources (id) of an AudioControl interface.
pub fn clock_sources(ac: &Interface) -> Vec<u8> {
    cs_descriptors(&ac.extra)
        .filter(|(sub, b)| *sub == 0x0A && !b.is_empty())
        .map(|(_, b)| b[0])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Endpoint;

    /// A UAC1 speaker like QEMU's usb-audio: 48 kHz stereo 16-bit on an
    /// adaptive isochronous OUT endpoint, feature unit 2 with mute and
    /// volume.
    #[test]
    fn uac1_speaker() {
        let ac = Interface {
            number: 0,
            alternate: 0,
            class: 1,
            subclass: 1,
            protocol: 0,
            endpoints: Vec::new(),
            extra: alloc::vec![
                9, 0x24, 1, 0, 1, 43, 0, 1, 1, // header
                12, 0x24, 2, 1, 1, 1, 0, 2, 3, 0, 0, 0, // input terminal 1 (USB stream)
                9, 0x24, 6, 2, 1, 1, 3, 0, 0, // feature unit 2 <- 1: mute|volume
                9, 0x24, 3, 3, 1, 3, 0, 2, 0, // output terminal 3 (speaker)
            ],
        };
        let fu = feature_units(&ac);
        assert_eq!(
            fu,
            [FeatureUnit {
                id: 2,
                source: 1,
                mute: true,
                volume: true
            }]
        );
        let alt = Interface {
            number: 1,
            alternate: 1,
            class: 1,
            subclass: 2,
            protocol: 0,
            endpoints: alloc::vec![Endpoint {
                address: 0x01,
                attributes: 0x09, // isochronous, adaptive
                max_packet: 192,
                interval: 1,
                max_burst: 0,
                ss_attributes: 0,
                pipe_id: 0,
            }],
            extra: alloc::vec![
                7, 0x24, 1, 1, 1, 1, 0, // AS_GENERAL: terminal 1, PCM
                11, 0x24, 2, 1, 2, 2, 16, 1, 0x80, 0xBB, 0x00, // type I, 48000
                7, 0x25, 1, 1, 0, 0, 0, // CS endpoint
            ],
        };
        let f = stream_format(&alt).unwrap();
        assert_eq!((f.channels, f.subframe, f.bits), (2, 2, 16));
        assert_eq!(f.rates, Rates::Discrete(alloc::vec![48000]));
        assert!(f.rates.supports(48000) && !f.rates.supports(44100));
        assert_eq!((f.interface, f.alternate, f.terminal), (1, 1, 1));
        // Alternate 0 (zero bandwidth) has no format.
        let zero = Interface {
            alternate: 0,
            endpoints: Vec::new(),
            ..alt.clone()
        };
        assert!(stream_format(&zero).is_none());
    }

    #[test]
    fn uac2_stream() {
        let alt = Interface {
            number: 1,
            alternate: 1,
            class: 1,
            subclass: 2,
            protocol: 0x20,
            endpoints: alloc::vec![Endpoint {
                address: 0x01,
                attributes: 0x05, // isochronous, asynchronous
                max_packet: 200,
                interval: 1,
                max_burst: 0,
                ss_attributes: 0,
                pipe_id: 0,
            }],
            extra: alloc::vec![
                16, 0x24, 1, 1, 0, 1, 1, 0, 0, 0, 2, 3, 0, 0, 0, 0, // AS_GENERAL
                6, 0x24, 2, 1, 3, 24, // type I: 3-byte subslots, 24 bits
            ],
        };
        let f = stream_format(&alt).unwrap();
        assert!(f.uac2);
        assert_eq!((f.channels, f.subframe, f.bits), (2, 3, 24));
        assert_eq!(f.rates, Rates::FromClock);
    }
}
