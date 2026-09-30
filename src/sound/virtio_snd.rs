//! virtio-sound (virtio device 25): PCM playback and capture streams set
//! up through the control queue; audio moves as period-sized buffers on
//! the TX and RX queues, each completed once the device has played (or
//! filled) it.

use super::Card;
use crate::drivers::virtio::{VirtioPci, Virtqueue};
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::pci::PciDevice;
use crate::sync::Mutex;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

const R_PCM_INFO: u32 = 0x0100;
const R_PCM_SET_PARAMS: u32 = 0x0101;
const R_PCM_PREPARE: u32 = 0x0102;
const R_PCM_RELEASE: u32 = 0x0103;
const R_PCM_START: u32 = 0x0104;
const R_PCM_STOP: u32 = 0x0105;
const S_OK: u32 = 0x8000;
const FMT_S16: u8 = 5;
const RATE_48000: u8 = 7;
const DIR_OUTPUT: u8 = 0;
const DIR_INPUT: u8 = 1;

/// Periods of 10 ms (480 frames of 16-bit stereo).
const PERIOD: usize = 480 * 4;
const SLOTS: usize = 4;

struct Queue {
    vq: Virtqueue,
    /// Per slot: xfer header (4 bytes), data, status (8 bytes).
    mem: DmaBuffer,
    free: Vec<usize>,
    slot_of: Vec<u16>,
    in_flight: usize,
}

const SLOT_BYTES: usize = 16 + PERIOD + 16;

impl Queue {
    fn new(vq: Virtqueue) -> Option<Queue> {
        let n = vq.size as usize;
        Some(Queue {
            vq,
            mem: DmaBuffer::new(SLOTS * SLOT_BYTES)?,
            free: (0..SLOTS).collect(),
            slot_of: alloc::vec![0; n],
            in_flight: 0,
        })
    }

    fn reclaim(&mut self) -> Vec<(usize, u32)> {
        let mut done = Vec::new();
        while let Some((id, len)) = self.vq.pop_used() {
            let slot = self.slot_of[id as usize] as usize;
            self.free.push(slot);
            self.in_flight -= 1;
            done.push((slot, len));
        }
        done
    }
}

pub struct VirtioSnd {
    pci: VirtioPci,
    ctl: Mutex<(Virtqueue, DmaBuffer)>,
    tx: Mutex<Queue>,
    rx: Option<Mutex<Queue>>,
    out_stream: u32,
    in_stream: Option<u32>,
}

impl VirtioSnd {
    /// Send a control request; returns the response bytes.
    fn control(&self, req: &[u8], resp_len: usize) -> KResult<Vec<u8>> {
        let mut g = self.ctl.lock();
        let (vq, mem) = &mut *g;
        mem.as_mut_slice()[..req.len()].copy_from_slice(req);
        mem.as_mut_slice()[2048..2048 + resp_len].fill(0);
        let p = mem.phys();
        vq.submit(&[
            (p, req.len() as u32, false),
            (p + 2048, resp_len as u32, true),
        ])
        .ok_or(EAGAIN)?;
        vq.notify();
        let d = crate::time::Deadline::after_ms(1000);
        while vq.pop_used().is_none() {
            if d.expired() {
                return Err(ETIMEDOUT);
            }
            core::hint::spin_loop();
        }
        let r = mem.as_slice()[2048..2048 + resp_len].to_vec();
        if u32::from_le_bytes(r[0..4].try_into().unwrap()) != S_OK {
            return Err(EIO);
        }
        Ok(r)
    }

    fn simple(&self, code: u32, stream: u32) -> KResult<()> {
        let mut req = code.to_le_bytes().to_vec();
        req.extend_from_slice(&stream.to_le_bytes());
        self.control(&req, 4).map(|_| ())
    }

    fn setup(&self, stream: u32) -> KResult<()> {
        let mut req = Vec::with_capacity(24);
        req.extend_from_slice(&R_PCM_SET_PARAMS.to_le_bytes());
        req.extend_from_slice(&stream.to_le_bytes());
        req.extend_from_slice(&((PERIOD * SLOTS) as u32).to_le_bytes());
        req.extend_from_slice(&(PERIOD as u32).to_le_bytes());
        req.extend_from_slice(&0u32.to_le_bytes()); // features
        req.extend_from_slice(&[2, FMT_S16, RATE_48000, 0]);
        self.control(&req, 4)?;
        self.simple(R_PCM_PREPARE, stream)
    }

    /// Queue one period on `q` (TX: `data`; RX: empty buffer to fill).
    fn post(&self, q: &mut Queue, stream: u32, data: Option<&[u8]>) -> bool {
        let Some(slot) = q.free.pop() else {
            return false;
        };
        let off = slot * SLOT_BYTES;
        let m = q.mem.as_mut_slice();
        m[off..off + 4].copy_from_slice(&stream.to_le_bytes());
        if let Some(d) = data {
            m[off + 16..off + 16 + d.len()].copy_from_slice(d);
            m[off + 16 + d.len()..off + 16 + PERIOD].fill(0);
        }
        let p = q.mem.phys() + off as u64;
        let bufs = [
            (p, 4, false),
            (p + 16, PERIOD as u32, data.is_none()),
            (p + 16 + PERIOD as u64, 8, true),
        ];
        match q.vq.submit(&bufs) {
            Some(id) => {
                q.slot_of[id as usize] = slot as u16;
                q.in_flight += 1;
                q.vq.notify();
                true
            }
            None => {
                q.free.push(slot);
                false
            }
        }
    }
}

impl Card for VirtioSnd {
    fn name(&self) -> String {
        String::from("virtio-sound")
    }

    fn play_format(&self) -> (u32, usize) {
        (48000, 2)
    }

    fn start_playback(&self) -> KResult<()> {
        self.setup(self.out_stream)?;
        // Prime with silence so the device has something while we mix.
        {
            let mut q = self.tx.lock();
            for _ in 0..2 {
                self.post(&mut q, self.out_stream, Some(&[0; PERIOD]));
            }
        }
        self.simple(R_PCM_START, self.out_stream)
    }

    fn stop_playback(&self) {
        let _ = self.simple(R_PCM_STOP, self.out_stream);
        let _ = self.simple(R_PCM_RELEASE, self.out_stream);
        self.tx.lock().reclaim();
    }

    fn write(&self, s: &[i16]) -> KResult<()> {
        let mut bytes = Vec::with_capacity(s.len() * 2);
        for v in s {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for chunk in bytes.chunks(PERIOD) {
            loop {
                {
                    let mut q = self.tx.lock();
                    q.reclaim();
                    if self.post(&mut q, self.out_stream, Some(chunk)) {
                        break;
                    }
                }
                crate::time::sleep_ms(2);
            }
        }
        Ok(())
    }

    fn delay(&self) -> usize {
        self.tx.lock().in_flight * PERIOD / 4
    }

    fn capture_format(&self) -> Option<(u32, usize)> {
        self.in_stream.map(|_| (48000, 2))
    }

    fn start_capture(&self) -> KResult<()> {
        let stream = self.in_stream.ok_or(ENODEV)?;
        let rx = self.rx.as_ref().ok_or(ENODEV)?;
        self.setup(stream)?;
        {
            let mut q = rx.lock();
            while self.post(&mut q, stream, None) {}
        }
        self.simple(R_PCM_START, stream)
    }

    fn stop_capture(&self) {
        if let Some(s) = self.in_stream {
            let _ = self.simple(R_PCM_STOP, s);
            let _ = self.simple(R_PCM_RELEASE, s);
        }
    }

    fn read(&self, out: &mut Vec<i16>) -> KResult<()> {
        let stream = self.in_stream.ok_or(ENODEV)?;
        let rx = self.rx.as_ref().ok_or(ENODEV)?;
        let stall = crate::time::Deadline::after_ms(2000);
        loop {
            if stall.expired() {
                return Err(EIO);
            }
            {
                let mut q = rx.lock();
                let done = q.reclaim();
                if !done.is_empty() {
                    for (slot, len) in &done {
                        let off = slot * SLOT_BYTES + 16;
                        let n = (*len as usize).saturating_sub(8).min(PERIOD);
                        let b = &q.mem.as_slice()[off..off + n];
                        out.extend(b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])));
                    }
                    while self.post(&mut q, stream, None) {}
                    return Ok(());
                }
            }
            crate::time::sleep_ms(3);
        }
    }
}

pub fn probe(dev: &PciDevice) {
    let Some(v) = VirtioPci::new(dev) else { return };
    if v.negotiate(0).is_none() {
        return;
    }
    let streams = v.cfg_r32(4);
    let (Some(ctl), Some(_ev), Some(tx), rx) = (
        v.setup_queue(0, 16, None),
        v.setup_queue(1, 16, None),
        v.setup_queue(2, 64, None),
        v.setup_queue(3, 64, None),
    ) else {
        return;
    };
    let Some(ctl_mem) = DmaBuffer::new(4096) else {
        return;
    };
    let Some(txq) = Queue::new(tx) else { return };
    let rxq = rx.and_then(Queue::new);
    v.driver_ok();
    let mut snd = VirtioSnd {
        pci: v,
        ctl: Mutex::new((ctl, ctl_mem)),
        tx: Mutex::new(txq),
        rx: rxq.map(Mutex::new),
        out_stream: u32::MAX,
        in_stream: None,
    };
    // PCM_INFO for all streams: 32-byte records after the 4-byte status.
    let mut req = R_PCM_INFO.to_le_bytes().to_vec();
    req.extend_from_slice(&0u32.to_le_bytes());
    req.extend_from_slice(&streams.to_le_bytes());
    req.extend_from_slice(&32u32.to_le_bytes());
    let info = match snd.control(&req, 4 + 32 * streams.min(16) as usize) {
        Ok(r) => r,
        Err(e) => {
            crate::println!("[virtio-snd] {}: PCM_INFO failed: {:?}", dev.name(), e);
            return;
        }
    };
    for i in 0..streams.min(16) as usize {
        let r = &info[4 + i * 32..4 + (i + 1) * 32];
        let formats = u64::from_le_bytes(r[8..16].try_into().unwrap());
        let rates = u64::from_le_bytes(r[16..24].try_into().unwrap());
        let (dir, cmax) = (r[24], r[26]);
        let ok = formats & (1 << FMT_S16) != 0 && rates & (1 << RATE_48000) != 0 && cmax >= 2;
        if !ok {
            continue;
        }
        if dir == DIR_OUTPUT && snd.out_stream == u32::MAX {
            snd.out_stream = i as u32;
        } else if dir == DIR_INPUT && snd.in_stream.is_none() {
            snd.in_stream = Some(i as u32);
        }
    }
    if snd.out_stream == u32::MAX {
        crate::println!("[virtio-snd] {}: no usable output stream", dev.name());
        snd.pci.reset();
        return;
    }
    crate::println!(
        "[virtio-snd] {}: {} streams, output {}{}",
        dev.name(),
        streams,
        snd.out_stream,
        snd.in_stream
            .map(|s| alloc::format!(", input {}", s))
            .unwrap_or_default()
    );
    super::register(Arc::new(snd));
}
