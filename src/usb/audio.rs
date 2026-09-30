//! USB Audio Class 1 and 2 playback (speakers, headsets, DACs).
//!
//! An AudioStreaming interface's alternate settings describe PCM formats
//! (`usb_desc::uac`); we pick 16-bit stereo (else mono) at 48 kHz (else
//! 44.1 kHz), select that alternate setting, set the sampling frequency
//! (UAC1: endpoint control; UAC2: clock source) and stream it as one
//! isochronous packet per service interval, keeping about 16 ms queued.
//! The feature unit is unmuted at 0 dB; the sound core's mixer applies
//! the OSS volumes in software.

use super::UsbDevice;
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::sched::WaitQueue;
use crate::sound::Card;
use crate::sync::Mutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use usb_desc::Interface;
use usb_desc::uac::{self, StreamFormat};

const SET_INTERFACE: u8 = 0x0B;
const SET_CUR: u8 = 0x01;
/// Packets in flight (1 ms each at full speed).
const INFLIGHT: usize = 16;

pub struct UsbAudio {
    dev: Weak<UsbDevice>,
    name: String,
    fmt: StreamFormat,
    rate: u32,
    /// Interleaved samples (card format) waiting to be streamed.
    fifo: Mutex<VecDeque<i16>>,
    running: AtomicBool,
    /// The streaming thread is alive.
    active: AtomicBool,
    stopped: WaitQueue,
}

impl UsbAudio {
    /// Bytes per frame on the wire.
    fn frame_bytes(&self) -> usize {
        self.fmt.channels as usize * self.fmt.subframe as usize
    }

    /// Frames per packet: rate / packets per second, with the remainder
    /// spread over the packets (44.1 kHz: 44 frames, every tenth 45).
    fn frames_for(&self, n: u64) -> usize {
        let pps = self.packets_per_second();
        let base = self.rate as u64 * n / pps;
        let next = self.rate as u64 * (n + 1) / pps;
        (next - base) as usize
    }

    fn packets_per_second(&self) -> u64 {
        let dev = self.dev.upgrade();
        let high = dev.is_some_and(|d| d.speed >= super::Speed::High);
        let interval = self.fmt.endpoint.interval.max(1) as u32;
        if high {
            // 2^(bInterval-1) microframes.
            8000 >> (interval - 1).min(3)
        } else {
            1000
        }
    }

    /// Encode `frames` frames from the FIFO (silence when it runs dry)
    /// into `out`.
    fn fill(&self, out: &mut [u8], frames: usize) {
        let mut f = self.fifo.lock();
        let sub = self.fmt.subframe as usize;
        for i in 0..frames * self.fmt.channels as usize {
            let s = f.pop_front().unwrap_or(0);
            let o = &mut out[i * sub..(i + 1) * sub];
            match sub {
                2 => o.copy_from_slice(&s.to_le_bytes()),
                3 => o.copy_from_slice(&((s as i32) << 8).to_le_bytes()[..3]),
                4 => o.copy_from_slice(&((s as i32) << 16).to_le_bytes()),
                _ => o[0] = ((s >> 8) as u8) ^ 0x80,
            }
        }
    }

    /// The streaming thread: keep INFLIGHT packets queued, refilling each
    /// slot as its packet completes.
    fn stream(self: Arc<UsbAudio>, dev: Arc<UsbDevice>) {
        let ep = self.fmt.endpoint;
        let max = ep.packet_size() as usize;
        let fb = self.frame_bytes();
        let Some(mut buf) = DmaBuffer::new(INFLIGHT * max) else {
            self.running.store(false, Ordering::SeqCst);
            self.active.store(false, Ordering::SeqCst);
            return;
        };
        let mut n = 0u64;
        let mut queue: VecDeque<crate::usb::xhci::Td> = VecDeque::new();
        let mut slot = 0;
        'run: while self.running.load(Ordering::Relaxed) && !dev.is_gone() {
            while queue.len() < INFLIGHT {
                let frames = self.frames_for(n).min(max / fb);
                n += 1;
                self.fill(&mut buf.as_mut_slice()[slot * max..], frames);
                match dev.submit_isoch(&ep, &buf, slot * max, frames * fb) {
                    Ok(td) => queue.push_back(td),
                    Err(e) => {
                        crate::println!("[usb] {}: audio: stream error {:?}", dev.name(), e);
                        break 'run;
                    }
                }
                slot = (slot + 1) % INFLIGHT;
            }
            if let Some(td) = queue.pop_front()
                && dev.wait(&td, Some(1000)).is_err()
                && dev.is_gone()
            {
                break;
            }
        }
        for td in queue {
            if dev.wait(&td, Some(100)).is_err() {
                dev.cancel(&td);
            }
        }
        self.running.store(false, Ordering::SeqCst);
        self.active.store(false, Ordering::SeqCst);
        self.stopped.wake_all();
    }
}

impl Card for UsbAudio {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn play_format(&self) -> (u32, usize) {
        (self.rate, self.fmt.channels as usize)
    }

    fn start_playback(&self) -> KResult<()> {
        let dev = self.dev.upgrade().ok_or(ENODEV)?;
        let me = find(&dev).ok_or(ENODEV)?;
        // A stream still winding down from the last stop finishes first.
        self.stopped
            .wait_timeout(500, || !self.active.load(Ordering::SeqCst));
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.active.store(true, Ordering::SeqCst);
        crate::sched::spawn("usb-audio", move || me.stream(dev));
        Ok(())
    }

    fn stop_playback(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.stopped
            .wait_timeout(500, || !self.active.load(Ordering::SeqCst));
        self.fifo.lock().clear();
    }

    fn write(&self, s: &[i16]) -> KResult<()> {
        let dev = self.dev.upgrade().ok_or(ENODEV)?;
        // Keep at most ~40 ms waiting beyond what is queued on the bus.
        let limit = self.rate as usize * self.fmt.channels as usize / 25;
        loop {
            if dev.is_gone() {
                return Err(ENODEV);
            }
            if self.fifo.lock().len() + s.len() <= limit.max(s.len()) {
                break;
            }
            crate::time::sleep_ms(2);
        }
        self.fifo.lock().extend(s.iter().copied());
        Ok(())
    }

    fn delay(&self) -> usize {
        self.fifo.lock().len() / self.fmt.channels as usize
            + self.rate as usize * INFLIGHT / self.packets_per_second() as usize
    }
}

/// Cards by device (so the streaming thread can get an Arc of itself).
static CARDS: Mutex<Vec<(Weak<UsbDevice>, Arc<UsbAudio>)>> = Mutex::new(Vec::new());

fn best_format(all: &[StreamFormat]) -> Option<(StreamFormat, u32)> {
    let score = |f: &StreamFormat| {
        let ch = match f.channels {
            2 => 3,
            1 => 2,
            _ => 1,
        };
        let bits = if f.subframe == 2 { 2 } else { 1 };
        ch * 4 + bits
    };
    let mut v: Vec<&StreamFormat> = all
        .iter()
        .filter(|f| f.endpoint.address & 0x80 == 0 && f.channels <= 2 && f.subframe <= 4)
        .collect();
    v.sort_by_key(|f| core::cmp::Reverse(score(f)));
    for rate in [48000, 44100, 32000, 16000] {
        if let Some(f) = v.iter().find(|f| f.rates.supports(rate)) {
            return Some(((*f).clone(), rate));
        }
    }
    None
}

pub fn probe(dev: &Arc<UsbDevice>, iface: &Interface) -> bool {
    if iface.class != uac::CLASS_AUDIO || iface.subclass != uac::SUBCLASS_STREAMING {
        return false;
    }
    let (alts, ac): (Vec<StreamFormat>, Option<Interface>) = {
        let cfg = dev.config.lock();
        let Some(c) = cfg.as_ref() else { return false };
        (
            c.interfaces
                .iter()
                .filter(|i| i.number == iface.number)
                .filter_map(uac::stream_format)
                .collect(),
            c.interfaces
                .iter()
                .find(|i| i.class == uac::CLASS_AUDIO && i.subclass == uac::SUBCLASS_CONTROL)
                .cloned(),
        )
    };
    let Some((fmt, rate)) = best_format(&alts) else {
        return false;
    };
    if dev
        .control_out(
            0x01,
            SET_INTERFACE,
            fmt.alternate as u16,
            fmt.interface as u16,
            &[],
        )
        .is_err()
        || dev.configure_endpoints(&[fmt.endpoint]).is_err()
    {
        crate::println!("[usb] {}: audio: cannot select the stream", dev.name());
        return false;
    }
    // Sampling frequency.
    let r = rate.to_le_bytes();
    if fmt.uac2 {
        if let Some(ac) = &ac
            && let Some(&clk) = uac::clock_sources(ac).first()
        {
            let _ = dev.control_out(
                0x21,
                SET_CUR,
                0x0100,
                (clk as u16) << 8 | ac.number as u16,
                &r,
            );
        }
    } else {
        let _ = dev.control_out(0x22, SET_CUR, 0x0100, fmt.endpoint.address as u16, &r[..3]);
    }
    // Unmute the master channel at 0 dB (volume is applied in software).
    let fu = ac.as_ref().and_then(|a| {
        uac::feature_units(a)
            .into_iter()
            .next()
            .map(|f| (a.number, f))
    });
    if let Some((acn, f)) = &fu {
        let idx = (f.id as u16) << 8 | *acn as u16;
        if f.mute {
            let _ = dev.control_out(0x21, SET_CUR, 0x0100, idx, &[0]);
        }
        if f.volume {
            let _ = dev.control_out(0x21, SET_CUR, 0x0200, idx, &0i16.to_le_bytes());
        }
    }
    let name = {
        let p = dev.product.lock().clone();
        if p.is_empty() {
            String::from("USB audio")
        } else {
            p
        }
    };
    crate::println!(
        "[usb] {}: audio: {} Hz, {} ch, {}-bit, UAC{}{}",
        dev.name(),
        rate,
        fmt.channels,
        fmt.bits,
        if fmt.uac2 { 2 } else { 1 },
        if fu.is_some() { ", feature unit" } else { "" }
    );
    let card = Arc::new(UsbAudio {
        dev: Arc::downgrade(dev),
        name,
        fmt,
        rate,
        fifo: Mutex::new(VecDeque::new()),
        running: AtomicBool::new(false),
        active: AtomicBool::new(false),
        stopped: WaitQueue::new(),
    });
    CARDS.lock().push((Arc::downgrade(dev), card.clone()));
    let snd = crate::sound::register(card);
    dev.on_detach(move || {
        crate::sound::unregister(&snd);
        CARDS.lock().retain(|(d, _)| d.strong_count() > 0);
    });
    true
}

fn find(dev: &Arc<UsbDevice>) -> Option<Arc<UsbAudio>> {
    CARDS
        .lock()
        .iter()
        .find(|(d, _)| d.upgrade().is_some_and(|d| Arc::ptr_eq(&d, dev)))
        .map(|(_, c)| c.clone())
}
