//! play, rec, beep, mixer: the OSS sound devices (/dev/dsp, /dev/mixer).

use crate::err;
use audio::clip;
use audio::pcm::{self, Format};
use audio::wav;
use rustos_rt::fs;
use rustos_rt::prelude::*;
use rustos_rt::sys::{check, syscall};

const SYS_IOCTL: usize = 16;
const SNDCTL_DSP_SYNC: usize = 0x5001;
const SNDCTL_DSP_SPEED: usize = 0xC004_5002;
const SNDCTL_DSP_SETFMT: usize = 0xC004_5005;
const SNDCTL_DSP_CHANNELS: usize = 0xC004_5006;
const MIXER_READ_VOLUME: usize = 0x8004_4D00;
const MIXER_WRITE_VOLUME: usize = 0xC004_4D00;
const MIXER_READ_PCM: usize = 0x8004_4D04;
const MIXER_WRITE_PCM: usize = 0xC004_4D04;

fn ioctl_int(f: &fs::File, cmd: usize, v: i32) -> rustos_rt::Result<i32> {
    let mut x = v;
    check(syscall(
        SYS_IOCTL,
        &[f.fd() as usize, cmd, &mut x as *mut i32 as usize],
    ))?;
    Ok(x)
}

/// Open a DSP device for playback (or recording) in the given format.
fn open_dsp(
    dev: &str,
    write: bool,
    rate: u32,
    ch: u16,
    fmt: Format,
) -> rustos_rt::Result<fs::File> {
    let flags = if write { fs::O_WRONLY } else { fs::O_RDONLY };
    let f = fs::File::open_with(dev, flags, 0)?;
    ioctl_int(&f, SNDCTL_DSP_SETFMT, fmt.afmt() as i32)?;
    ioctl_int(&f, SNDCTL_DSP_CHANNELS, ch as i32)?;
    ioctl_int(&f, SNDCTL_DSP_SPEED, rate as i32)?;
    Ok(f)
}

fn write_all(f: &fs::File, mut b: &[u8]) -> rustos_rt::Result<()> {
    while !b.is_empty() {
        let n = f.write(b)?;
        b = &b[n..];
    }
    Ok(())
}

/// play [-d DEVICE] FILE...: WAV or MP3 files.
pub fn play(args: &[String]) -> i32 {
    let mut dev = String::from("/dev/dsp");
    let mut files = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-d" if i + 1 < args.len() => {
                dev = args[i + 1].clone();
                i += 1;
            }
            f => files.push(f.to_string()),
        }
        i += 1;
    }
    if files.is_empty() {
        eprintln!("usage: play [-d DEVICE] FILE.wav|FILE.mp3...");
        return 2;
    }
    for f in &files {
        let data = match fs::read(f) {
            Ok(d) => d,
            Err(e) => return err("play", f, e),
        };
        // PCM WAV plays as stored; anything else (MP3) is decoded first.
        let (rate, ch, fmt, body, len): (u32, u16, Format, Vec<u8>, usize) =
            match wav::parse(&data) {
                Some(w) => {
                    let body = &data[w.data_off..w.data_off + w.data_len];
                    let b = if w.format == Format::S24 {
                        wav::unpack24(body)
                    } else {
                        body.to_vec()
                    };
                    (w.rate, w.channels, w.format, b, w.data_len)
                }
                None => match clip::decode(&data) {
                    Some(c) => {
                        let mut b = Vec::with_capacity(c.samples.len() * 2);
                        Format::S16.encode(&c.samples, &mut b);
                        let n = b.len();
                        (c.rate, c.channels as u16, Format::S16, b, n)
                    }
                    None => {
                        eprintln!("play: {}: not a WAV or MP3 file", f);
                        return 1;
                    }
                },
            };
        let out = match open_dsp(&dev, true, rate, ch, fmt) {
            Ok(o) => o,
            Err(e) => return err("play", &dev, e),
        };
        println!("{}: {} Hz, {} ch, {} bytes", f, rate, ch, len);
        if let Err(e) = write_all(&out, &body) {
            return err("play", &dev, e);
        }
        let _ = ioctl_int(&out, SNDCTL_DSP_SYNC, 0);
    }
    0
}

/// rec [-d DEVICE] [-t SECONDS] [-r RATE] [-c CHANNELS] FILE.wav
pub fn rec(args: &[String]) -> i32 {
    let mut dev = String::from("/dev/dsp");
    let (mut secs, mut rate, mut ch) = (5u32, 48000u32, 2u16);
    let mut file = None;
    let mut i = 1;
    while i < args.len() {
        let next = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "-d" => dev = next,
            "-t" => secs = next.parse().unwrap_or(secs),
            "-r" => rate = next.parse().unwrap_or(rate),
            "-c" => ch = next.parse().unwrap_or(ch),
            f => {
                file = Some(f.to_string());
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    let Some(file) = file else {
        eprintln!("usage: rec [-d DEVICE] [-t SECONDS] [-r RATE] [-c CHANNELS] FILE.wav");
        return 2;
    };
    let inp = match open_dsp(&dev, false, rate, ch, Format::S16) {
        Ok(f) => f,
        Err(e) => return err("rec", &dev, e),
    };
    let total = (rate * secs) as usize * ch as usize * 2;
    let mut data = vec![0u8; total];
    let mut got = 0;
    while got < total {
        match inp.read(&mut data[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) => return err("rec", &dev, e),
        }
    }
    data.truncate(got);
    let mut out = wav::header(rate, ch, 16, got as u32).to_vec();
    out.extend_from_slice(&data);
    match fs::write(&file, &out) {
        Ok(()) => {
            println!("{}: {} bytes recorded", file, got);
            0
        }
        Err(e) => err("rec", &file, e),
    }
}

/// beep [-f HZ] [-l MS] [-v VOLUME%] [-d DEVICE]: a sine tone.
pub fn beep(args: &[String]) -> i32 {
    let mut dev = String::from("/dev/dsp");
    let (mut freq, mut ms, mut vol) = (440u32, 200u32, 50i32);
    let mut i = 1;
    while i + 1 < args.len() {
        let v = &args[i + 1];
        match args[i].as_str() {
            "-f" => freq = v.parse().unwrap_or(freq),
            "-l" => ms = v.parse().unwrap_or(ms),
            "-v" => vol = v.parse().unwrap_or(vol),
            "-d" => dev = v.clone(),
            _ => {}
        }
        i += 2;
    }
    let rate = 48000;
    let out = match open_dsp(&dev, true, rate, 2, Format::S16) {
        Ok(o) => o,
        Err(e) => return err("beep", &dev, e),
    };
    let mut phase = 0.0;
    let frames = (rate * ms / 1000) as usize;
    let amp = 32767 * vol.clamp(0, 100) / 100;
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(4800);
        let s = pcm::tone(freq, rate, 2, amp, n, &mut phase);
        let mut b = Vec::with_capacity(s.len() * 2);
        Format::S16.encode(&s, &mut b);
        if let Err(e) = write_all(&out, &b) {
            return err("beep", &dev, e);
        }
        done += n;
    }
    let _ = ioctl_int(&out, SNDCTL_DSP_SYNC, 0);
    0
}

/// mixer [-d DEVICE] [volume|pcm [LEVEL[%]]]: show or set volumes.
pub fn mixer(args: &[String]) -> i32 {
    let mut dev = String::from("/dev/mixer");
    let mut rest = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "-d" && i + 1 < args.len() {
            dev = args[i + 1].clone();
            i += 2;
            continue;
        }
        rest.push(args[i].clone());
        i += 1;
    }
    let f = match fs::File::open(&dev) {
        Ok(f) => f,
        Err(e) => return err("mixer", &dev, e),
    };
    let show = |name: &str, cmd: usize| {
        if let Ok(v) = ioctl_int(&f, cmd, 0) {
            println!("{:<7} {}% {}%", name, v & 0xFF, (v >> 8) & 0xFF);
        }
    };
    match rest.first().map(String::as_str) {
        None => {
            show("volume", MIXER_READ_VOLUME);
            show("pcm", MIXER_READ_PCM);
            0
        }
        Some(c @ ("volume" | "vol" | "pcm")) => {
            let (rd, wr) = if c == "pcm" {
                (MIXER_READ_PCM, MIXER_WRITE_PCM)
            } else {
                (MIXER_READ_VOLUME, MIXER_WRITE_VOLUME)
            };
            if let Some(level) = rest.get(1) {
                let l: i32 = level.trim_end_matches('%').parse().unwrap_or(0);
                let l = l.clamp(0, 100);
                if let Err(e) = ioctl_int(&f, wr, l | l << 8) {
                    return err("mixer", &dev, e);
                }
            }
            show(if c == "pcm" { "pcm" } else { "volume" }, rd);
            0
        }
        Some(_) => {
            eprintln!("usage: mixer [-d DEVICE] [volume|pcm [LEVEL]]");
            2
        }
    }
}
