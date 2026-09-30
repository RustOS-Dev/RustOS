//! RIFF WAVE files (PCM).

use crate::pcm::Format;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavInfo {
    pub rate: u32,
    pub channels: u16,
    pub format: Format,
    /// Offset and length of the sample data.
    pub data_off: usize,
    pub data_len: usize,
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Parse a WAV header (PCM 8/16/24/32-bit, or WAVE_FORMAT_EXTENSIBLE
/// with PCM). A data chunk whose length runs past the end is clipped.
pub fn parse(b: &[u8]) -> Option<WavInfo> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut o = 12;
    let mut fmt = None;
    while o + 8 <= b.len() {
        let id = &b[o..o + 4];
        let len = le32(b, o + 4) as usize;
        let body = o + 8;
        if id == b"fmt " && body + 16 <= b.len() {
            let tag = le16(b, body);
            let ch = le16(b, body + 2);
            let rate = le32(b, body + 4);
            let bits = le16(b, body + 14);
            let pcm = tag == 1 || (tag == 0xFFFE && len >= 40 && le16(b, body + 24) == 1);
            if !pcm {
                return None;
            }
            let f = match bits {
                8 => Format::U8,
                16 => Format::S16,
                24 => Format::S24,
                32 => Format::S32,
                _ => return None,
            };
            fmt = Some((rate, ch, f, bits));
        } else if id == b"data" {
            let (rate, channels, format, bits) = fmt?;
            let data_len = len.min(b.len() - body);
            if bits == 24 {
                // Packed 24-bit samples: callers convert with `unpack24`.
                return Some(WavInfo {
                    rate,
                    channels,
                    format,
                    data_off: body,
                    data_len: data_len / 3 * 3,
                });
            }
            return Some(WavInfo {
                rate,
                channels,
                format,
                data_off: body,
                data_len,
            });
        }
        o = body + len + (len & 1);
    }
    None
}

/// Packed 24-bit samples to the 32-bit container `Format::S24` uses.
pub fn unpack24(b: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(b.len() / 3 * 4);
    for c in b.chunks_exact(3) {
        v.extend_from_slice(&[c[0], c[1], c[2], if c[2] & 0x80 != 0 { 0xFF } else { 0 }]);
    }
    v
}

/// A 44-byte PCM header for `data_len` bytes (u32::MAX-ish for streams).
pub fn header(rate: u32, channels: u16, bits: u16, data_len: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    let block = channels * bits / 8;
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&data_len.saturating_add(36).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * block as u32).to_le_bytes());
    h[32..34].copy_from_slice(&block.to_le_bytes());
    h[34..36].copy_from_slice(&bits.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let mut f = header(48000, 2, 16, 400).to_vec();
        f.extend(core::iter::repeat_n(0u8, 400));
        let w = parse(&f).unwrap();
        assert_eq!((w.rate, w.channels, w.format), (48000, 2, Format::S16));
        assert_eq!((w.data_off, w.data_len), (44, 400));
        // Truncated data is clipped.
        assert_eq!(parse(&f[..100]).unwrap().data_len, 56);
        assert!(parse(b"RIFF....WAVE").is_none());
    }
}
