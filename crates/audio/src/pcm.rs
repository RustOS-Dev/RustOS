//! PCM samples: formats, conversion to and from the mixing format
//! (interleaved i32 in 16-bit range), linear resampling, mixing and a
//! sine tone generator.

use alloc::vec::Vec;

/// Sample formats (the OSS AFMT_* values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    U8,
    S16,
    /// 24-bit in the low bytes of a 32-bit little-endian word.
    S24,
    S32,
}

pub const AFMT_U8: u32 = 0x08;
pub const AFMT_S16_LE: u32 = 0x10;
pub const AFMT_S32_LE: u32 = 0x1000;
pub const AFMT_S24_LE: u32 = 0x8000;

impl Format {
    pub fn from_afmt(v: u32) -> Option<Format> {
        Some(match v {
            AFMT_U8 => Format::U8,
            AFMT_S16_LE => Format::S16,
            AFMT_S24_LE => Format::S24,
            AFMT_S32_LE => Format::S32,
            _ => return None,
        })
    }

    pub fn afmt(self) -> u32 {
        match self {
            Format::U8 => AFMT_U8,
            Format::S16 => AFMT_S16_LE,
            Format::S24 => AFMT_S24_LE,
            Format::S32 => AFMT_S32_LE,
        }
    }

    pub fn bytes(self) -> usize {
        match self {
            Format::U8 => 1,
            Format::S16 => 2,
            Format::S24 | Format::S32 => 4,
        }
    }

    /// Decode samples to 16-bit range values.
    pub fn decode(self, b: &[u8], out: &mut Vec<i32>) {
        match self {
            Format::U8 => out.extend(b.iter().map(|&x| (x as i32 - 128) << 8)),
            Format::S16 => out.extend(
                b.chunks_exact(2)
                    .map(|c| i16::from_le_bytes([c[0], c[1]]) as i32),
            ),
            Format::S24 => out.extend(b.chunks_exact(4).map(|c| {
                let v = i32::from_le_bytes([c[0], c[1], c[2], 0]) << 8 >> 8;
                v >> 8
            })),
            Format::S32 => out.extend(
                b.chunks_exact(4)
                    .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> 16),
            ),
        }
    }

    /// Encode 16-bit range values.
    pub fn encode(self, s: &[i32], out: &mut Vec<u8>) {
        for &v in s {
            let v = v.clamp(-32768, 32767);
            match self {
                Format::U8 => out.push(((v >> 8) + 128) as u8),
                Format::S16 => out.extend_from_slice(&(v as i16).to_le_bytes()),
                Format::S24 => out.extend_from_slice(&(v << 8).to_le_bytes()),
                Format::S32 => out.extend_from_slice(&(v << 16).to_le_bytes()),
            }
        }
    }
}

/// Channel count conversion of interleaved frames.
pub fn remix(s: &[i32], from: usize, to: usize) -> Vec<i32> {
    if from == to {
        return s.to_vec();
    }
    let frames = s.len() / from.max(1);
    let mut out = Vec::with_capacity(frames * to);
    for f in s.chunks_exact(from) {
        for c in 0..to {
            out.push(match (from, to) {
                (1, _) => f[0],
                (_, 1) => f.iter().sum::<i32>() / from as i32,
                _ => f[c.min(from - 1)],
            });
        }
    }
    out
}

/// Linear-interpolation resampler for interleaved frames.
#[derive(Clone, Debug)]
pub struct Resampler {
    from: u32,
    to: u32,
    ch: usize,
    /// Position of the next output frame, in input frames << 16.
    pos: u64,
    /// The last input frame of the previous call.
    last: Vec<i32>,
}

impl Resampler {
    pub fn new(from: u32, to: u32, ch: usize) -> Resampler {
        Resampler {
            from,
            to,
            ch,
            pos: 0,
            last: alloc::vec![0; ch],
        }
    }

    pub fn process(&mut self, input: &[i32]) -> Vec<i32> {
        let ch = self.ch;
        if self.from == self.to {
            return input.to_vec();
        }
        let n = input.len() / ch;
        let step = ((self.from as u64) << 16) / self.to as u64;
        let mut out = Vec::with_capacity(n * self.to as usize / self.from as usize + ch);
        // Frame -1 is the previous call's last frame.
        let frame = |i: i64, c: usize| -> i32 {
            if i < 0 {
                self.last[c]
            } else {
                input[i as usize * ch + c]
            }
        };
        loop {
            let i = (self.pos >> 16) as i64 - 1;
            if i + 1 >= n as i64 {
                break;
            }
            let frac = (self.pos & 0xFFFF) as i64;
            for c in 0..ch {
                let a = frame(i, c) as i64;
                let b = frame(i + 1, c) as i64;
                out.push((a + (b - a) * frac / 65536) as i32);
            }
            self.pos += step;
        }
        self.pos -= (n as u64) << 16;
        if n > 0 {
            self.last.copy_from_slice(&input[(n - 1) * ch..n * ch]);
        }
        out
    }
}

/// Add `src` into `dst` scaled by `vol` (0..=100), saturating to 16 bits.
pub fn mix_into(dst: &mut [i32], src: &[i32], vol: u32) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = (*d + s * vol as i32 / 100).clamp(-32768, 32767);
    }
}

/// sin(2 pi x) for x in [0, 1), accurate to about 1e-4.
pub fn sin_turns(x: f32) -> f32 {
    let x = x - (x as i32) as f32;
    let (x, sign) = if x < 0.5 { (x, 1.0) } else { (x - 0.5, -1.0) };
    // Bhaskara I, refined: 16x(pi - x) / (5pi^2 - 4x(pi - x)) in half turns.
    let t = x * 2.0; // 0..1 of a half turn
    let p = t * (1.0 - t);
    sign * 16.0 * p / (5.0 - 4.0 * p)
}

/// `frames` frames of a `freq` Hz sine at `rate` Hz, `ch` channels,
/// amplitude `amp` (16-bit range); `phase` carries on across calls.
pub fn tone(freq: u32, rate: u32, ch: usize, amp: i32, frames: usize, phase: &mut f32) -> Vec<i32> {
    let mut out = Vec::with_capacity(frames * ch);
    let inc = freq as f32 / rate as f32;
    for _ in 0..frames {
        let v = (sin_turns(*phase) * amp as f32) as i32;
        out.extend(core::iter::repeat_n(v, ch));
        *phase += inc;
        if *phase >= 1.0 {
            *phase -= 1.0;
        }
    }
    out
}

/// Goertzel power of `freq` in mono samples (for tests and checks).
pub fn goertzel(s: &[i32], rate: u32, freq: f32) -> f32 {
    let w = freq / rate as f32;
    let c = 2.0 * cos_turns(w);
    let (mut s1, mut s2) = (0f32, 0f32);
    for &x in s {
        let s0 = x as f32 + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - c * s1 * s2
}

fn cos_turns(x: f32) -> f32 {
    sin_turns(x + 0.25)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_round_trip() {
        let s = alloc::vec![0, 1000, -1000, 32767, -32768];
        for f in [Format::S16, Format::S24, Format::S32] {
            let mut b = Vec::new();
            f.encode(&s, &mut b);
            assert_eq!(b.len(), s.len() * f.bytes());
            let mut d = Vec::new();
            f.decode(&b, &mut d);
            assert_eq!(d, s, "{:?}", f);
        }
        let mut b = Vec::new();
        Format::U8.encode(&[0, 32767, -32768], &mut b);
        assert_eq!(b, [128, 255, 0]);
    }

    #[test]
    fn resample_keeps_frequency() {
        // 1 kHz at 44.1 kHz to 48 kHz, in chunks.
        let mut ph = 0.0;
        let input = tone(1000, 44100, 1, 10000, 44100, &mut ph);
        let mut r = Resampler::new(44100, 48000, 1);
        let mut out = Vec::new();
        for c in input.chunks(1000) {
            out.extend(r.process(c));
        }
        assert!((out.len() as i32 - 48000).abs() < 5, "{}", out.len());
        let on = goertzel(&out, 48000, 1000.0);
        let off = goertzel(&out, 48000, 1500.0);
        assert!(on > off * 1000.0);
        // Stereo remix and back.
        let st = remix(&input[..100], 1, 2);
        assert_eq!(st.len(), 200);
        assert_eq!(remix(&st, 2, 1), input[..100].to_vec());
    }

    #[test]
    fn sine_accuracy() {
        for i in 0..100 {
            let x = i as f32 / 100.0;
            let want = libm_sin(x * 2.0 * core::f32::consts::PI);
            assert!(
                (sin_turns(x) - want).abs() < 2e-3,
                "{} {} {}",
                x,
                sin_turns(x),
                want
            );
        }
    }

    fn libm_sin(x: f32) -> f32 {
        // Taylor series around the nearest multiple of pi/2 (test only).
        let mut x = x % (2.0 * core::f32::consts::PI);
        if x > core::f32::consts::PI {
            x -= 2.0 * core::f32::consts::PI;
        }
        let mut term = x;
        let mut sum = x;
        for n in 1..12 {
            term *= -x * x / ((2 * n) as f32 * (2 * n + 1) as f32);
            sum += term;
        }
        sum
    }

    #[test]
    fn mixing_saturates() {
        let mut d = alloc::vec![30000, -30000];
        mix_into(&mut d, &[10000, -10000], 100);
        assert_eq!(d, [32767, -32768]);
        let mut d = alloc::vec![0];
        mix_into(&mut d, &[1000], 50);
        assert_eq!(d, [500]);
    }
}
