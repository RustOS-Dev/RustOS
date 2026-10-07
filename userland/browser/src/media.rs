//! `<audio>`/`new Audio()` for scripts: the browser fetches and decodes a
//! clip (WAV or MP3, `audio::clip`) and plays it from a forked child that
//! writes to /dev/dsp, so the page stays responsive. Pausing kills the
//! child; jsd keeps the playback position from its own clock.

use alloc::collections::BTreeMap;
use audio::clip::{self, Clip};
use audio::pcm::Format;
use rustos_rt::prelude::*;
use rustos_rt::sys::{check, syscall};
use rustos_rt::{fs, io, process};

const SYS_IOCTL: usize = 16;
const SNDCTL_DSP_SYNC: usize = 0x5001;
const SNDCTL_DSP_SPEED: usize = 0xC004_5002;
const SNDCTL_DSP_SETFMT: usize = 0xC004_5005;
const SNDCTL_DSP_CHANNELS: usize = 0xC004_5006;
/// Largest media file fetched.
pub const MAX_BYTES: usize = 64 << 20;

/// A page's decoded clips and the players running them.
#[derive(Default)]
pub struct Media {
    clips: Vec<Clip>,
    /// Element key (from jsd) -> player pid.
    players: BTreeMap<i64, i32>,
}

impl Media {
    /// Decode a fetched file; returns (clip id, duration in seconds).
    pub fn add(&mut self, bytes: &[u8]) -> Result<(usize, f64), &'static str> {
        if clip::sniff(bytes).is_none() {
            return Err("unsupported media type");
        }
        let c = clip::decode(bytes).ok_or("decode error")?;
        let d = c.frames() as f64 / c.rate.max(1) as f64;
        self.clips.push(c);
        Ok((self.clips.len() - 1, d))
    }

    /// Start playing clip `id` from `from` seconds for element `key`.
    pub fn play(
        &mut self,
        key: i64,
        id: usize,
        from: f64,
        volume: f64,
    ) -> Result<(), &'static str> {
        self.stop(key);
        let c = self.clips.get(id).ok_or("no such clip")?;
        let start = ((from.max(0.0) * c.rate as f64) as usize).min(c.frames()) * c.channels;
        let pid = process::fork().map_err(|_| "cannot start the player")?;
        if pid == 0 {
            io::discard_buffered();
            for fd in 3..256 {
                process::close(fd);
            }
            process::exit(play_child(c, start, volume));
        }
        self.players.insert(key, pid);
        Ok(())
    }

    pub fn stop(&mut self, key: i64) {
        if let Some(pid) = self.players.remove(&key) {
            let _ = process::kill(pid, 9);
            let _ = process::waitpid(pid, 0);
        }
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        let keys: Vec<i64> = self.players.keys().copied().collect();
        for k in keys {
            self.stop(k);
        }
    }
}

fn ioctl_int(f: &fs::File, cmd: usize, v: i32) -> bool {
    let mut x = v;
    check(syscall(
        SYS_IOCTL,
        &[f.fd() as usize, cmd, &mut x as *mut i32 as usize],
    ))
    .is_ok()
}

/// The player process: write the clip from sample `start` to /dev/dsp.
fn play_child(c: &Clip, start: usize, volume: f64) -> i32 {
    let Ok(f) = fs::File::open_with("/dev/dsp", fs::O_WRONLY, 0) else {
        return 1;
    };
    if !(ioctl_int(&f, SNDCTL_DSP_SETFMT, Format::S16.afmt() as i32)
        && ioctl_int(&f, SNDCTL_DSP_CHANNELS, c.channels as i32)
        && ioctl_int(&f, SNDCTL_DSP_SPEED, c.rate as i32))
    {
        return 1;
    }
    let gain = (volume.clamp(0.0, 1.0) * 256.0) as i32;
    let mut b = Vec::new();
    for chunk in c.samples[start..].chunks(4096) {
        let s: Vec<i32> = chunk.iter().map(|&v| v * gain / 256).collect();
        b.clear();
        Format::S16.encode(&s, &mut b);
        let mut w = &b[..];
        while !w.is_empty() {
            match f.write(w) {
                Ok(n) => w = &w[n..],
                Err(_) => return 1,
            }
        }
    }
    ioctl_int(&f, SNDCTL_DSP_SYNC, 0);
    0
}
