//! Sound: PCM cards and the OSS interface.
//!
//! A card driver (Intel HD Audio, virtio-sound, USB audio) implements
//! [`Card`]: a playback stream and optionally a capture stream in one
//! fixed format (interleaved 16-bit at the card's rate), written or read
//! in blocking chunks paced by the hardware.
//!
//! On top, each card gets `/dev/dspN` and `/dev/mixerN` (card 0 also as
//! `/dev/dsp` and `/dev/mixer`) with the OSS API: any number of programs
//! can play at once in any rate, channel count and sample format; a
//! mixing thread per card converts, resamples and sums them with the
//! master and PCM volumes. Reading `/dev/dsp` records from the capture
//! stream, converted to the reader's format.

pub mod hda;
pub mod virtio_snd;

use crate::errno::*;
use crate::process::uaccess;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use audio::pcm::{self, Format, Resampler};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

/// A sound card's hardware streams.
pub trait Card: Send + Sync {
    fn name(&self) -> String;
    /// Playback format: (rate, channels).
    fn play_format(&self) -> (u32, usize);
    fn start_playback(&self) -> KResult<()>;
    fn stop_playback(&self);
    /// Queue interleaved samples; blocks until the hardware has room.
    fn write(&self, s: &[i16]) -> KResult<()>;
    /// Frames queued and not yet played.
    fn delay(&self) -> usize;
    /// Capture format, if the card can record.
    fn capture_format(&self) -> Option<(u32, usize)> {
        None
    }
    fn start_capture(&self) -> KResult<()> {
        Err(ENODEV)
    }
    fn stop_capture(&self) {}
    /// Recorded samples (blocks until some are available).
    fn read(&self, _out: &mut Vec<i16>) -> KResult<()> {
        Err(ENODEV)
    }
    /// Hardware output volume (0..=100 per side), if it has one.
    fn set_hw_volume(&self, _left: u32, _right: u32) {}
}

/// 10 ms mixing periods.
const PERIOD_MS: u32 = 10;
/// Client buffering: 250 ms.
const CLIENT_MS: usize = 250;

struct Params {
    rate: u32,
    channels: usize,
    format: Format,
    resampler: Resampler,
}

/// One open /dev/dsp: what it plays (and records).
struct Client {
    params: Mutex<Params>,
    /// Converted to the card format (interleaved), waiting to be mixed.
    fifo: Mutex<VecDeque<i32>>,
    /// Recorded, in the client's format (bytes).
    rec: Mutex<VecDeque<u8>>,
    rec_resampler: Mutex<Option<Resampler>>,
    playing: bool,
    recording: bool,
    written: AtomicUsize,
}

pub struct SoundDev {
    pub index: usize,
    pub card: Arc<dyn Card>,
    clients: Mutex<Vec<Weak<Client>>>,
    /// Wakes the mixer (data queued) and writers (room freed).
    wq: WaitQueue,
    running: AtomicBool,
    capturing: AtomicBool,
    /// OSS volumes: left | right << 8, 0..=100.
    volume: AtomicU32,
    pcm_volume: AtomicU32,
}

static CARDS: Mutex<Vec<Arc<SoundDev>>> = Mutex::new(Vec::new());

/// Register a card: /dev/dspN, /dev/mixerN (and /dev/dsp, /dev/mixer for
/// the first).
pub fn register(card: Arc<dyn Card>) -> Arc<SoundDev> {
    let dev = {
        let mut cards = CARDS.lock();
        let d = Arc::new(SoundDev {
            index: cards.len(),
            card,
            clients: Mutex::new(Vec::new()),
            wq: WaitQueue::new(),
            running: AtomicBool::new(false),
            capturing: AtomicBool::new(false),
            volume: AtomicU32::new(80 | 80 << 8),
            pcm_volume: AtomicU32::new(100 | 100 << 8),
        });
        cards.push(d.clone());
        d
    };
    let i = dev.index;
    use crate::vfs::FileType::CharDevice;
    crate::vfs::devfs::register(
        &alloc::format!("dsp{}", i),
        CharDevice,
        (14 << 8) | (3 + 16 * i as u64),
        Arc::new(DspNode(dev.clone())),
    );
    crate::vfs::devfs::register(
        &alloc::format!("mixer{}", i),
        CharDevice,
        (14 << 8) | (16 * i as u64),
        Arc::new(MixerNode(dev.clone())),
    );
    if i == 0 {
        crate::vfs::devfs::register(
            "dsp",
            CharDevice,
            (14 << 8) | 3,
            Arc::new(DspNode(dev.clone())),
        );
        crate::vfs::devfs::register(
            "mixer",
            CharDevice,
            14 << 8,
            Arc::new(MixerNode(dev.clone())),
        );
    }
    let v = dev.volume.load(Ordering::Relaxed);
    dev.card.set_hw_volume(v & 0xFF, v >> 8);
    crate::println!("[sound] card {}: {} (/dev/dsp{})", i, dev.card.name(), i);
    let d = dev.clone();
    crate::sched::spawn(&alloc::format!("snd-mix{}", i), move || d.mix_loop());
    dev
}

/// /proc/asound-like summary.
pub fn cards() -> Vec<Arc<SoundDev>> {
    CARDS.lock().clone()
}

impl SoundDev {
    fn live_clients(&self) -> Vec<Arc<Client>> {
        let mut c = self.clients.lock();
        c.retain(|w| w.strong_count() > 0);
        c.iter().filter_map(|w| w.upgrade()).collect()
    }

    /// The mixing thread: while anyone plays, sum their queued audio in
    /// 10 ms periods and hand it to the card (which paces the loop).
    fn mix_loop(self: Arc<SoundDev>) {
        let (rate, ch) = self.card.play_format();
        let period = (rate * PERIOD_MS / 1000) as usize * ch;
        let mut idle_periods = 0;
        loop {
            let players: Vec<Arc<Client>> = self
                .live_clients()
                .into_iter()
                .filter(|c| c.playing)
                .collect();
            let any_data = players.iter().any(|c| !c.fifo.lock().is_empty());
            if !any_data {
                // Keep the stream running briefly (silence), then stop.
                if self.running.load(Ordering::Relaxed) {
                    idle_periods += 1;
                    if idle_periods > 50 || players.is_empty() && idle_periods > 5 {
                        self.card.stop_playback();
                        self.running.store(false, Ordering::Relaxed);
                        self.wq.wake_all();
                        continue;
                    }
                } else {
                    self.wq.wait_timeout(200, || {
                        self.live_clients()
                            .iter()
                            .any(|c| c.playing && !c.fifo.lock().is_empty())
                    });
                    continue;
                }
            } else {
                idle_periods = 0;
            }
            if !self.running.load(Ordering::Relaxed) {
                if let Err(e) = self.card.start_playback() {
                    crate::println!(
                        "[sound] card {}: cannot start playback: {:?}",
                        self.index,
                        e
                    );
                    crate::time::sleep_ms(500);
                    continue;
                }
                self.running.store(true, Ordering::Relaxed);
            }
            let mut mix = alloc::vec![0i32; period];
            let v = self.volume.load(Ordering::Relaxed);
            let p = self.pcm_volume.load(Ordering::Relaxed);
            let vol = ((v & 0xFF) + (v >> 8)) / 2 * ((p & 0xFF) + (p >> 8)) / 2 / 100;
            for c in &players {
                let chunk: Vec<i32> = {
                    let mut f = c.fifo.lock();
                    let n = period.min(f.len());
                    f.drain(..n).collect()
                };
                pcm::mix_into(&mut mix, &chunk, vol);
            }
            self.wq.wake_all();
            let out: Vec<i16> = mix.iter().map(|&s| s as i16).collect();
            if self.card.write(&out).is_err() {
                crate::time::sleep_ms(PERIOD_MS as u64);
            }
        }
    }

    /// The capture thread for as long as someone records.
    fn capture_loop(self: Arc<SoundDev>) {
        let Some((rate, ch)) = self.card.capture_format() else {
            return;
        };
        if self.card.start_capture().is_err() {
            self.capturing.store(false, Ordering::SeqCst);
            return;
        }
        let mut buf = Vec::new();
        loop {
            let readers: Vec<Arc<Client>> = self
                .live_clients()
                .into_iter()
                .filter(|c| c.recording)
                .collect();
            if readers.is_empty() {
                break;
            }
            buf.clear();
            if self.card.read(&mut buf).is_err() {
                break;
            }
            let s: Vec<i32> = buf.iter().map(|&x| x as i32).collect();
            for c in readers {
                let (crate_rate, cch, fmt) = {
                    let p = c.params.lock();
                    (p.rate, p.channels, p.format)
                };
                let conv = pcm::remix(&s, ch, cch);
                let mut rs = c.rec_resampler.lock();
                let r = rs.get_or_insert_with(|| Resampler::new(rate, crate_rate, cch));
                let conv = r.process(&conv);
                drop(rs);
                let mut bytes = Vec::new();
                fmt.encode(&conv, &mut bytes);
                let mut q = c.rec.lock();
                q.extend(bytes);
                let max = crate_rate as usize * cch * fmt.bytes() * 2;
                while q.len() > max {
                    q.pop_front();
                }
            }
            self.wq.wake_all();
        }
        self.card.stop_capture();
        self.capturing.store(false, Ordering::SeqCst);
    }
}

/// /dev/dsp node: each open is a client.
struct DspNode(Arc<SoundDev>);
/// /dev/mixer node.
struct MixerNode(Arc<SoundDev>);

struct DspFile {
    dev: Arc<SoundDev>,
    client: Arc<Client>,
}

const O_ACCMODE: u32 = 3;

impl crate::vfs::FileLike for DspNode {
    fn read(&self, _b: &mut [u8], _nb: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn write(&self, _b: &[u8], _nb: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn open_instance(&self, flags: u32) -> KResult<Option<Arc<dyn crate::vfs::FileLike>>> {
        let dev = self.0.clone();
        let (rate, ch) = dev.card.play_format();
        let mode = flags & O_ACCMODE;
        let recording = mode != 1 && dev.card.capture_format().is_some();
        let client = Arc::new(Client {
            params: Mutex::new(Params {
                rate: 8000,
                channels: 1,
                format: Format::U8,
                resampler: Resampler::new(8000, rate, ch),
            }),
            fifo: Mutex::new(VecDeque::new()),
            rec: Mutex::new(VecDeque::new()),
            rec_resampler: Mutex::new(None),
            playing: mode != 0,
            recording,
            written: AtomicUsize::new(0),
        });
        dev.clients.lock().push(Arc::downgrade(&client));
        if recording && !dev.capturing.swap(true, Ordering::SeqCst) {
            let d = dev.clone();
            crate::sched::spawn(&alloc::format!("snd-rec{}", dev.index), move || {
                d.capture_loop()
            });
        }
        Ok(Some(Arc::new(DspFile { dev, client })))
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

impl DspFile {
    fn card_frames_to_bytes(&self, frames: usize) -> usize {
        let p = self.client.params.lock();
        let (rate, _) = self.dev.card.play_format();
        frames * p.rate as usize / rate as usize * p.channels * p.format.bytes()
    }

    /// Frames of the card format the client may still queue.
    fn room(&self) -> usize {
        let (rate, ch) = self.dev.card.play_format();
        let cap = rate as usize * CLIENT_MS / 1000 * ch;
        cap.saturating_sub(self.client.fifo.lock().len()) / ch
    }

    /// SNDCTL_DSP_SYNC: wait until everything queued has been played.
    fn drain(&self) {
        let deadline = crate::time::Deadline::after_ms(5000);
        while !self.client.fifo.lock().is_empty() && !deadline.expired() {
            self.dev
                .wq
                .wait_timeout(50, || self.client.fifo.lock().is_empty());
        }
        let (rate, _) = self.dev.card.play_format();
        let ms = self.dev.card.delay() as u64 * 1000 / rate as u64;
        crate::time::sleep_ms(ms.min(1000));
    }

    fn set_params(&self, f: impl FnOnce(&mut Params)) {
        let (rate, ch) = self.dev.card.play_format();
        let mut p = self.client.params.lock();
        f(&mut p);
        p.resampler = Resampler::new(p.rate, rate, ch);
        *self.client.rec_resampler.lock() = None;
    }
}

fn get_int(arg: u64) -> KResult<i32> {
    let mut b = [0u8; 4];
    uaccess::copy_from_user(&mut b, arg)?;
    Ok(i32::from_le_bytes(b))
}

fn put_int(arg: u64, v: i32) -> KResult<i64> {
    uaccess::copy_to_user(arg, &v.to_le_bytes())?;
    Ok(0)
}

impl crate::vfs::FileLike for DspFile {
    fn write(&self, buf: &[u8], nonblock: bool) -> KResult<usize> {
        if !self.client.playing {
            return Err(EBADF);
        }
        let (_, ch) = self.dev.card.play_format();
        let (fsize, cch, fmt) = {
            let p = self.client.params.lock();
            (p.channels * p.format.bytes(), p.channels, p.format)
        };
        let usable = buf.len() / fsize * fsize;
        let mut done = 0;
        // Convert in pieces of about 10 ms so a full queue blocks early.
        while done < usable {
            let n = (usable - done).min(fsize * 512);
            loop {
                if self.room() > 0 {
                    break;
                }
                if nonblock {
                    return if done > 0 { Ok(done) } else { Err(EAGAIN) };
                }
                self.dev.wq.wait_timeout(100, || self.room() > 0);
                if crate::process::signal::has_pending() {
                    return if done > 0 { Ok(done) } else { Err(EINTR) };
                }
            }
            let mut s = Vec::with_capacity(n / fmt.bytes());
            fmt.decode(&buf[done..done + n], &mut s);
            let s = pcm::remix(&s, cch, ch);
            let s = self.client.params.lock().resampler.process(&s);
            self.client.fifo.lock().extend(s);
            self.dev.wq.wake_all();
            done += n;
        }
        self.client.written.fetch_add(done, Ordering::Relaxed);
        Ok(buf.len())
    }

    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        if !self.client.recording {
            return Err(EBADF);
        }
        let fsize = {
            let p = self.client.params.lock();
            p.channels * p.format.bytes()
        };
        loop {
            {
                let mut q = self.client.rec.lock();
                let n = (buf.len().min(q.len())) / fsize * fsize;
                if n > 0 {
                    for (d, s) in buf[..n].iter_mut().zip(q.drain(..n)) {
                        *d = s;
                    }
                    return Ok(n);
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            self.dev
                .wq
                .wait_timeout(100, || self.client.rec.lock().len() >= fsize);
            if crate::process::signal::has_pending() {
                return Err(EINTR);
            }
            if !self.dev.capturing.load(Ordering::SeqCst) {
                return Err(EIO);
            }
        }
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        let (rate, ch) = self.dev.card.play_format();
        match cmd {
            0x5000 => {
                // SNDCTL_DSP_RESET
                self.client.fifo.lock().clear();
                self.client.rec.lock().clear();
                Ok(0)
            }
            0x5001 | 0x5008 => {
                // SYNC / POST
                if cmd == 0x5001 {
                    self.drain();
                }
                Ok(0)
            }
            0xC004_5002 => {
                // SPEED
                let r = get_int(arg)?.clamp(4000, 192_000) as u32;
                self.set_params(|p| p.rate = r);
                put_int(arg, r as i32)
            }
            0xC004_5003 => {
                // STEREO
                let s = get_int(arg)? != 0;
                self.set_params(|p| p.channels = if s { 2 } else { 1 });
                put_int(arg, s as i32)
            }
            0xC004_5006 => {
                // CHANNELS
                let c = get_int(arg)?.clamp(1, 8) as usize;
                self.set_params(|p| p.channels = c);
                put_int(arg, c as i32)
            }
            0xC004_5005 => {
                // SETFMT (AFMT_QUERY = 0 asks)
                let f = get_int(arg)? as u32;
                if f != 0
                    && let Some(fmt) = Format::from_afmt(f)
                {
                    self.set_params(|p| p.format = fmt);
                }
                put_int(arg, self.client.params.lock().format.afmt() as i32)
            }
            0x8004_500B => put_int(
                arg,
                (pcm::AFMT_U8 | pcm::AFMT_S16_LE | pcm::AFMT_S24_LE | pcm::AFMT_S32_LE) as i32,
            ),
            0xC004_5004 => {
                // GETBLKSIZE: one period of the client's format.
                let b = self.card_frames_to_bytes((rate * PERIOD_MS / 1000) as usize);
                put_int(arg, b as i32)
            }
            0xC004_500A => Ok(0), // SETFRAGMENT: accepted, fixed periods
            0x8010_500C | 0x8010_500D => {
                // GETOSPACE / GETISPACE: {fragments, fragstotal, fragsize, bytes}
                let frag = self
                    .card_frames_to_bytes((rate * PERIOD_MS / 1000) as usize)
                    .max(1);
                let bytes = if cmd == 0x8010_500C {
                    self.card_frames_to_bytes(self.room())
                } else {
                    self.client.rec.lock().len()
                };
                let total = self.card_frames_to_bytes(rate as usize * CLIENT_MS / 1000) / frag;
                let mut b = [0u8; 16];
                for (i, v) in [
                    (bytes / frag) as i32,
                    total as i32,
                    frag as i32,
                    bytes as i32,
                ]
                .iter()
                .enumerate()
                {
                    b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
                uaccess::copy_to_user(arg, &b)?;
                Ok(0)
            }
            0x8004_5017 => {
                // GETODELAY
                let frames = self.client.fifo.lock().len() / ch + self.dev.card.delay();
                put_int(arg, self.card_frames_to_bytes(frames) as i32)
            }
            0x8004_500F => put_int(arg, 0x3000 | 0x100), // GETCAPS: TRIGGER | MMAP-less, DUPLEX
            0x500E => Ok(0),                             // NONBLOCK
            0x8004_5010 => put_int(arg, 3),              // GETTRIGGER
            0x4004_5010 => Ok(0),                        // SETTRIGGER
            0x800C_5012 | 0x800C_5011 => {
                // GETOPTR / GETIPTR: {bytes, blocks, ptr}
                let bytes = self.client.written.load(Ordering::Relaxed) as i32;
                let mut b = [0u8; 12];
                b[0..4].copy_from_slice(&bytes.to_le_bytes());
                uaccess::copy_to_user(arg, &b)?;
                Ok(0)
            }
            _ => mixer_ioctl(&self.dev, cmd, arg),
        }
    }

    fn poll(&self) -> u16 {
        let mut r = 0;
        if self.client.playing && self.room() > 0 {
            r |= crate::vfs::POLLOUT;
        }
        if self.client.recording && !self.client.rec.lock().is_empty() {
            r |= crate::vfs::POLLIN;
        }
        r
    }

    fn wait_queue(&self) -> &WaitQueue {
        &self.dev.wq
    }

    fn close(&self) {
        // OSS drains pending output on close.
        if self.client.playing {
            self.drain();
        }
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// OSS mixer ioctls (on /dev/mixer and /dev/dsp).
fn mixer_ioctl(dev: &SoundDev, cmd: u64, arg: u64) -> KResult<i64> {
    const DEVMASK: i32 = 1 | 1 << 4; // VOLUME | PCM
    match cmd {
        0x8004_4D00 => put_int(arg, dev.volume.load(Ordering::Relaxed) as i32),
        0x8004_4D04 => put_int(arg, dev.pcm_volume.load(Ordering::Relaxed) as i32),
        0xC004_4D00 | 0xC004_4D04 => {
            let v = get_int(arg)? as u32;
            let (l, r) = ((v & 0xFF).min(100), ((v >> 8) & 0xFF).min(100));
            let v = l | r << 8;
            if cmd == 0xC004_4D00 {
                dev.volume.store(v, Ordering::Relaxed);
                dev.card.set_hw_volume(l, r);
            } else {
                dev.pcm_volume.store(v, Ordering::Relaxed);
            }
            put_int(arg, v as i32)
        }
        0x8004_4DFE | 0x8004_4DFB => put_int(arg, DEVMASK),
        0x8004_4DFD | 0x8004_4DFF => put_int(arg, 0),
        0x8004_4DFC => put_int(arg, 0),
        0x805C_4D65 => {
            // SOUND_MIXER_INFO: id[16], name[32], modify_counter, fill[10]
            let mut b = [0u8; 92];
            b[..6].copy_from_slice(b"RustOS");
            let n = dev.card.name();
            let k = n.len().min(31);
            b[16..16 + k].copy_from_slice(&n.as_bytes()[..k]);
            uaccess::copy_to_user(arg, &b)?;
            Ok(0)
        }
        _ => Err(ENOTTY),
    }
}

impl crate::vfs::FileLike for MixerNode {
    fn read(&self, _b: &mut [u8], _nb: bool) -> KResult<usize> {
        Ok(0)
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        mixer_ioctl(&self.0, cmd, arg)
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// Probe PCI sound devices.
pub fn probe(dev: &crate::pci::PciDevice) {
    if dev.class == 0x04 && dev.subclass == 0x03 {
        hda::probe(dev);
    } else if dev.vendor_id == crate::pci::ids::VENDOR_REDHAT
        && matches!(dev.device_id, 0x1059 | 0x1019)
    {
        virtio_snd::probe(dev);
    }
}

/// /proc/asound/cards.
pub fn proc_cards() -> String {
    let mut s = String::new();
    for c in cards() {
        s.push_str(&alloc::format!(
            "{:2} [{}]: /dev/dsp{}\n",
            c.index,
            c.card.name(),
            c.index
        ));
    }
    s
}
