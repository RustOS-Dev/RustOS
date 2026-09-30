//! Whole sound files decoded to PCM: WAV (any format `wav` parses) and,
//! with the `mp3` feature, MPEG-1/2 Layer III.

use crate::pcm::Format;
use crate::wav;
use alloc::vec::Vec;

/// Decoded audio: interleaved samples in the 16-bit range.
#[derive(Clone, Debug, Default)]
pub struct Clip {
    pub rate: u32,
    pub channels: usize,
    pub samples: Vec<i32>,
}

impl Clip {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    /// Length in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.frames() as u64 * 1000 / self.rate.max(1) as u64
    }
}

/// Container kinds [`decode`] recognises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Wav,
    Mp3,
}

/// Guess the file type from its first bytes.
pub fn sniff(b: &[u8]) -> Option<Kind> {
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        Some(Kind::Wav)
    } else if b.starts_with(b"ID3") || (b.len() >= 2 && b[0] == 0xFF && b[1] & 0xE0 == 0xE0) {
        Some(Kind::Mp3)
    } else {
        None
    }
}

/// Decode a WAV or MP3 file.
pub fn decode(b: &[u8]) -> Option<Clip> {
    match sniff(b)? {
        Kind::Wav => decode_wav(b),
        Kind::Mp3 => decode_mp3(b),
    }
}

fn decode_wav(b: &[u8]) -> Option<Clip> {
    let w = wav::parse(b)?;
    let body = &b[w.data_off..w.data_off + w.data_len];
    let mut samples = Vec::new();
    if w.format == Format::S24 {
        w.format.decode(&wav::unpack24(body), &mut samples);
    } else {
        w.format.decode(body, &mut samples);
    }
    Some(Clip {
        rate: w.rate,
        channels: w.channels as usize,
        samples,
    })
}

#[cfg(feature = "mp3")]
fn decode_mp3(b: &[u8]) -> Option<Clip> {
    let mut dec = nanomp3::Decoder::new();
    let mut pcm = alloc::vec![0f32; nanomp3::MAX_SAMPLES_PER_FRAME];
    let mut clip = Clip::default();
    let mut o = 0;
    while o < b.len() {
        let (used, info) = dec.decode(&b[o..], &mut pcm);
        if used == 0 {
            break;
        }
        o += used;
        let Some(info) = info else { continue };
        let ch = info.channels.num() as usize;
        if clip.channels == 0 {
            clip.rate = info.sample_rate;
            clip.channels = ch;
        }
        let n = info.samples_produced * ch;
        if ch == clip.channels {
            clip.samples.extend(
                pcm[..n]
                    .iter()
                    .map(|&v| (v.clamp(-1.0, 1.0) * 32767.0) as i32),
            );
        } else {
            // A channel-count change mid-stream: remix to the first.
            let s: Vec<i32> = pcm[..n]
                .iter()
                .map(|&v| (v.clamp(-1.0, 1.0) * 32767.0) as i32)
                .collect();
            clip.samples
                .extend(crate::pcm::remix(&s, ch, clip.channels));
        }
    }
    (clip.channels > 0).then_some(clip)
}

#[cfg(not(feature = "mp3"))]
fn decode_mp3(_b: &[u8]) -> Option<Clip> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm;

    #[test]
    fn wav_roundtrip() {
        let mut phase = 0.0;
        let s = pcm::tone(440, 8000, 1, 10000, 800, &mut phase);
        let mut body = Vec::new();
        Format::S16.encode(&s, &mut body);
        let mut f = wav::header(8000, 1, 16, body.len() as u32).to_vec();
        f.extend_from_slice(&body);
        assert_eq!(sniff(&f), Some(Kind::Wav));
        let c = decode(&f).unwrap();
        assert_eq!((c.rate, c.channels, c.frames()), (8000, 1, 800));
        assert_eq!(c.duration_ms(), 100);
    }

    #[cfg(feature = "mp3")]
    #[test]
    fn mp3_tone() {
        let f = include_bytes!("../tests/data/tone1k.mp3");
        assert_eq!(sniff(f), Some(Kind::Mp3));
        let c = decode(f).unwrap();
        assert_eq!((c.rate, c.channels), (44100, 2));
        // 0.5 s plus the encoder's padding frames.
        assert!((22050..=26000).contains(&c.frames()), "{}", c.frames());
        let left: Vec<i32> = c.samples.iter().step_by(2).copied().collect();
        let mid = &left[4410..17640];
        let on = pcm::goertzel(mid, 44100, 1000.0);
        let off = pcm::goertzel(mid, 44100, 3000.0);
        assert!(on > off * 1000.0, "{} vs {}", on, off);
    }
}
