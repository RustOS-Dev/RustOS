//! Intel High Definition Audio (PCI class 04:03).
//!
//! The controller is reset and its codecs found (STATESTS); verbs go out
//! through the CORB ring and responses come back through the RIRB. For
//! each codec's audio function group the widgets are read into an
//! `audio::hda::Codec`, which picks the playback route (a plugged
//! headphone, else speaker or line out) and the capture route. The route
//! is programmed (connection selects, amplifiers unmuted, pin controls,
//! EAPD for speaker amplifiers, power D0) and a DAC and ADC get a stream:
//! a cyclic DMA buffer described by a buffer descriptor list, 48 kHz
//! 16-bit stereo. Playback writes stay ahead of the hardware position
//! (LPIB); capture reads behind it.

use super::Card;
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::pci::PciDevice;
use crate::sync::Mutex;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use audio::hda::{Codec, PINCAP_EAPD, PINCAP_HP, PINCAP_PRESENCE, Route, Widget, WidgetType};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const GCAP: u64 = 0x00;
const GCTL: u64 = 0x08;
const STATESTS: u64 = 0x0E;
const INTCTL: u64 = 0x20;
const CORBLBASE: u64 = 0x40;
const CORBUBASE: u64 = 0x44;
const CORBWP: u64 = 0x48;
const CORBRP: u64 = 0x4A;
const CORBCTL: u64 = 0x4C;
const CORBSIZE: u64 = 0x4E;
const RIRBLBASE: u64 = 0x50;
const RIRBUBASE: u64 = 0x54;
const RIRBWP: u64 = 0x58;
const RINTCNT: u64 = 0x5A;
const RIRBCTL: u64 = 0x5C;
const RIRBSTS: u64 = 0x5D;
const RIRBSIZE: u64 = 0x5E;
const SD_BASE: u64 = 0x80;

/// Stream buffer: 8 periods of 1024 frames (16-bit stereo), ~170 ms.
const PERIODS: usize = 8;
const PERIOD_BYTES: usize = 1024 * 4;
const BUF_BYTES: usize = PERIODS * PERIOD_BYTES;
/// 48 kHz, 16 bits, 2 channels.
const FMT_48K_16_2: u16 = 0x0011;

struct Regs(u64);

impl Regs {
    fn r8(&self, o: u64) -> u8 {
        unsafe { core::ptr::read_volatile((self.0 + o) as *const u8) }
    }
    fn r16(&self, o: u64) -> u16 {
        unsafe { core::ptr::read_volatile((self.0 + o) as *const u16) }
    }
    fn r32(&self, o: u64) -> u32 {
        unsafe { core::ptr::read_volatile((self.0 + o) as *const u32) }
    }
    fn w8(&self, o: u64, v: u8) {
        unsafe { core::ptr::write_volatile((self.0 + o) as *mut u8, v) }
    }
    fn w16(&self, o: u64, v: u16) {
        unsafe { core::ptr::write_volatile((self.0 + o) as *mut u16, v) }
    }
    fn w32(&self, o: u64, v: u32) {
        unsafe { core::ptr::write_volatile((self.0 + o) as *mut u32, v) }
    }
}

/// CORB/RIRB command rings.
struct Rings {
    corb: DmaBuffer,
    rirb: DmaBuffer,
    rirb_rp: u16,
}

/// One DMA stream (a stream descriptor and its cyclic buffer).
struct Stream {
    /// Stream descriptor register base.
    sd: u64,
    tag: u8,
    buf: DmaBuffer,
    bdl: DmaBuffer,
    /// Playback: next byte to write; capture: next byte to read.
    pos: usize,
    running: bool,
}

pub struct Hda {
    name: String,
    regs: Regs,
    rings: Mutex<Rings>,
    codec_addr: u8,
    out_route: Option<Route>,
    in_route: Option<Route>,
    codec: Codec,
    play: Mutex<Stream>,
    rec: Option<Mutex<Stream>>,
    out_gain: AtomicU32,
    /// Headphone presence last seen (for switching outputs).
    hp_present: AtomicBool,
}

fn verb12(cad: u8, nid: u8, verb: u16, payload: u8) -> u32 {
    (cad as u32) << 28 | (nid as u32) << 20 | (verb as u32) << 8 | payload as u32
}

fn verb4(cad: u8, nid: u8, verb: u8, payload: u16) -> u32 {
    (cad as u32) << 28 | (nid as u32) << 20 | (verb as u32) << 16 | payload as u32
}

impl Hda {
    /// Send a verb and wait for its response.
    fn cmd(regs: &Regs, rings: &mut Rings, v: u32) -> KResult<u32> {
        let wp = (regs.r16(CORBWP) & 0xFF) as usize;
        let next = (wp + 1) % 256;
        rings.corb.write::<u32>(next * 4, v);
        core::sync::atomic::fence(Ordering::SeqCst);
        regs.w16(CORBWP, next as u16);
        let deadline = crate::time::Deadline::after_ms(100);
        loop {
            let rp = regs.r16(RIRBWP) & 0xFF;
            if rp != rings.rirb_rp {
                rings.rirb_rp = (rings.rirb_rp + 1) % 256;
                let e = rings.rirb_rp as usize * 8;
                let resp = rings.rirb.read::<u32>(e);
                let ex = rings.rirb.read::<u32>(e + 4);
                regs.w8(RIRBSTS, 5);
                if ex & 0x10 != 0 {
                    continue; // unsolicited (jack events): ignored here
                }
                return Ok(resp);
            }
            if deadline.expired() {
                return Err(ETIMEDOUT);
            }
            core::hint::spin_loop();
        }
    }

    fn verb(&self, nid: u8, verb: u16, payload: u8) -> u32 {
        let mut r = self.rings.lock();
        Self::cmd(
            &self.regs,
            &mut r,
            verb12(self.codec_addr, nid, verb, payload),
        )
        .unwrap_or(0)
    }

    fn verb_long(&self, nid: u8, verb: u8, payload: u16) -> u32 {
        let mut r = self.rings.lock();
        Self::cmd(
            &self.regs,
            &mut r,
            verb4(self.codec_addr, nid, verb, payload),
        )
        .unwrap_or(0)
    }

    /// Unmute and set the amplifiers along a route; select its inputs;
    /// enable the pin (and EAPD); power everything up.
    fn program_route(&self, r: &Route, output: bool) {
        let gain = self.out_gain.load(Ordering::Relaxed);
        for h in &r.hops {
            let Some(w) = self.codec.widget(h.nid) else {
                continue;
            };
            self.verb(h.nid, 0x705, 0); // power state D0
            if let Some(sel) = h.select
                && w.kind() != WidgetType::Mixer
            {
                self.verb(h.nid, 0x701, sel);
            }
            // Output amplifier (both sides), and the selected input of
            // mixers / input amps.
            let steps = |caps: u32| (caps >> 8) & 0x7F;
            let amp_out = if w.caps & audio::hda::WCAP_AMP_OVERRIDE != 0 {
                w.amp_out
            } else {
                self.codec.widget(self.codec.afg).map_or(0, |a| a.amp_out)
            };
            if w.caps & audio::hda::WCAP_OUT_AMP != 0 {
                let g = (steps(amp_out) * gain / 100) as u16;
                self.verb_long(h.nid, 0x3, 0x8000 | 0x3000 | g);
            }
            if w.caps & audio::hda::WCAP_IN_AMP != 0 {
                let idx = h.select.unwrap_or(0) as u16;
                let g = if output {
                    0
                } else {
                    steps(w.amp_in) as u16 * 3 / 4
                };
                self.verb_long(h.nid, 0x3, 0x4000 | 0x3000 | idx << 8 | g);
            }
        }
        let pin = r.pin;
        if let Some(w) = self.codec.widget(pin) {
            let ctl = if output {
                0x40 | if w.pin_caps & PINCAP_HP != 0 { 0x80 } else { 0 }
            } else {
                0x20 | 0x01 // input, VREF 50% for microphones
            };
            self.verb(pin, 0x707, ctl);
            if w.pin_caps & PINCAP_EAPD != 0 {
                self.verb(pin, 0x70C, 0x02);
            }
        }
    }

    /// Mute (pin control 0) the output pins not in use.
    fn quiet_other_outputs(&self, used: u8) {
        for r in self.codec.output_routes() {
            if r.pin != used {
                self.verb(r.pin, 0x707, 0);
            }
        }
    }

    fn sd_reset(&self, s: &Stream) {
        let r = &self.regs;
        r.w8(s.sd, r.r8(s.sd) & !2); // stop
        r.w8(s.sd, 1); // reset
        let d = crate::time::Deadline::after_ms(50);
        while r.r8(s.sd) & 1 == 0 && !d.expired() {}
        r.w8(s.sd, 0);
        let d = crate::time::Deadline::after_ms(50);
        while r.r8(s.sd) & 1 != 0 && !d.expired() {}
    }

    /// Set up and start a stream on converter `nid`.
    fn start_stream(&self, s: &mut Stream, nid: u8, fill: bool) {
        self.sd_reset(s);
        let r = &self.regs;
        s.buf.as_mut_slice().fill(0);
        for i in 0..PERIODS {
            let e = i * 16;
            s.bdl
                .write::<u64>(e, s.buf.phys() + (i * PERIOD_BYTES) as u64);
            s.bdl.write::<u32>(e + 8, PERIOD_BYTES as u32);
            s.bdl.write::<u32>(e + 12, 0);
        }
        r.w32(s.sd + 0x18, s.bdl.phys() as u32);
        r.w32(s.sd + 0x1C, (s.bdl.phys() >> 32) as u32);
        r.w32(s.sd + 0x08, BUF_BYTES as u32);
        r.w16(s.sd + 0x0C, (PERIODS - 1) as u16);
        r.w16(s.sd + 0x12, FMT_48K_16_2);
        // Stream tag in CTL byte 2 (bits 23:20).
        r.w8(s.sd + 2, s.tag << 4);
        self.verb_long(nid, 0x2, FMT_48K_16_2);
        self.verb(nid, 0x706, s.tag << 4);
        // Playback starts a period ahead of the hardware.
        s.pos = if fill { PERIOD_BYTES } else { 0 };
        r.w8(s.sd, 2); // RUN
        s.running = true;
    }

    fn stop_stream(&self, s: &mut Stream) {
        let r = &self.regs;
        r.w8(s.sd, r.r8(s.sd) & !2);
        s.running = false;
    }

    fn lpib(&self, s: &Stream) -> usize {
        (self.regs.r32(s.sd + 0x04) as usize) % BUF_BYTES
    }

    /// Switch between speaker and headphones when the jack changes.
    fn check_jack(&self) {
        let Some(hp) = self
            .codec
            .output_routes()
            .into_iter()
            .find(|r| r.kind == audio::hda::PinKind::Headphone)
        else {
            return;
        };
        if self
            .codec
            .widget(hp.pin)
            .is_none_or(|w| w.pin_caps & PINCAP_PRESENCE == 0)
        {
            return;
        }
        let present = self.verb(hp.pin, 0xF09, 0) & 0x8000_0000 != 0;
        if present != self.hp_present.swap(present, Ordering::Relaxed)
            && let Some(r) = self.codec.choose_output(|n| n == hp.pin && present)
        {
            self.program_route(&r, true);
            self.quiet_other_outputs(r.pin);
            crate::println!("[hda] {}: output now {}", self.name, r.kind.name());
        }
    }
}

impl Card for Hda {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn play_format(&self) -> (u32, usize) {
        (48000, 2)
    }

    fn start_playback(&self) -> KResult<()> {
        let route = self.out_route.as_ref().ok_or(ENODEV)?;
        self.check_jack();
        let mut s = self.play.lock();
        self.start_stream(&mut s, route.converter(), true);
        Ok(())
    }

    fn stop_playback(&self) {
        let mut s = self.play.lock();
        self.stop_stream(&mut s);
    }

    fn write(&self, data: &[i16]) -> KResult<()> {
        let mut bytes: Vec<u8> = Vec::with_capacity(data.len() * 2);
        for v in data {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let mut off = 0;
        while off < bytes.len() {
            let (room, pos) = {
                let s = self.play.lock();
                if !s.running {
                    return Err(EIO);
                }
                let hw = self.lpib(&s);
                // Keep a period of slack between us and the hardware.
                let used = (s.pos + BUF_BYTES - hw) % BUF_BYTES;
                (BUF_BYTES.saturating_sub(used + PERIOD_BYTES), s.pos)
            };
            if room < 4 {
                crate::time::sleep_ms(2);
                continue;
            }
            let n = room.min(bytes.len() - off) & !3;
            let first = n.min(BUF_BYTES - pos);
            let mut s = self.play.lock();
            s.buf.as_mut_slice()[pos..pos + first].copy_from_slice(&bytes[off..off + first]);
            if n > first {
                s.buf.as_mut_slice()[..n - first].copy_from_slice(&bytes[off + first..off + n]);
            }
            s.pos = (pos + n) % BUF_BYTES;
            off += n;
        }
        Ok(())
    }

    fn delay(&self) -> usize {
        let s = self.play.lock();
        if !s.running {
            return 0;
        }
        (s.pos + BUF_BYTES - self.lpib(&s)) % BUF_BYTES / 4
    }

    fn capture_format(&self) -> Option<(u32, usize)> {
        (self.in_route.is_some() && self.rec.is_some()).then_some((48000, 2))
    }

    fn start_capture(&self) -> KResult<()> {
        let route = self.in_route.as_ref().ok_or(ENODEV)?;
        let rec = self.rec.as_ref().ok_or(ENODEV)?;
        self.program_route(route, false);
        let mut s = rec.lock();
        self.start_stream(&mut s, route.converter(), false);
        Ok(())
    }

    fn stop_capture(&self) {
        if let Some(rec) = &self.rec {
            let mut s = rec.lock();
            self.stop_stream(&mut s);
        }
    }

    fn read(&self, out: &mut Vec<i16>) -> KResult<()> {
        let rec = self.rec.as_ref().ok_or(ENODEV)?;
        // A stream whose position never moves has no input behind it.
        let stall = crate::time::Deadline::after_ms(2000);
        loop {
            let mut s = rec.lock();
            if !s.running {
                return Err(EIO);
            }
            let hw = self.lpib(&s);
            let avail = ((hw + BUF_BYTES - s.pos) % BUF_BYTES) & !3;
            if avail >= PERIOD_BYTES / 4 {
                let mut p = s.pos;
                for _ in 0..avail / 2 {
                    let b = s.buf.as_slice();
                    out.push(i16::from_le_bytes([b[p], b[p + 1]]));
                    p = (p + 2) % BUF_BYTES;
                }
                s.pos = p;
                return Ok(());
            }
            drop(s);
            if stall.expired() {
                return Err(EIO);
            }
            crate::time::sleep_ms(5);
        }
    }

    fn set_hw_volume(&self, left: u32, right: u32) {
        self.out_gain.store((left + right) / 2, Ordering::Relaxed);
        if let Some(r) = &self.out_route {
            self.program_route(r, true);
        }
    }
}

/// Read a codec's audio function group into an `audio::hda::Codec`.
fn read_codec(regs: &Regs, rings: &mut Rings, cad: u8) -> Option<Codec> {
    fn param(regs: &Regs, rings: &mut Rings, cad: u8, nid: u8, p: u8) -> u32 {
        Hda::cmd(regs, rings, verb12(cad, nid, 0xF00, p)).unwrap_or(0)
    }
    let vendor = param(regs, rings, cad, 0, 0);
    let root = param(regs, rings, cad, 0, 4);
    let (start, count) = (((root >> 16) & 0xFF) as u8, (root & 0xFF) as u8);
    let mut afg = None;
    for n in start..start.saturating_add(count) {
        if param(regs, rings, cad, n, 5) & 0xFF == 1 {
            afg = Some(n);
            break;
        }
    }
    let afg = afg?;
    let nodes = param(regs, rings, cad, afg, 4);
    let (ws, wc) = (((nodes >> 16) & 0xFF) as u8, (nodes & 0xFF) as u8);
    let afg_out_amp = param(regs, rings, cad, afg, 0x12);
    let afg_in_amp = param(regs, rings, cad, afg, 0x0D);
    let mut widgets = alloc::vec![Widget {
        nid: afg,
        caps: 0x00F0_0000,
        pin_caps: 0,
        config: 0,
        conns: Vec::new(),
        amp_in: afg_in_amp,
        amp_out: afg_out_amp,
    }];
    for nid in ws..ws.saturating_add(wc) {
        let caps = param(regs, rings, cad, nid, 9);
        let kind = audio::hda::WidgetType::from_caps(caps);
        let pin_caps = if kind == WidgetType::Pin {
            param(regs, rings, cad, nid, 0x0C)
        } else {
            0
        };
        let amp_in = param(regs, rings, cad, nid, 0x0D);
        let amp_out = param(regs, rings, cad, nid, 0x12);
        let mut conns = Vec::new();
        if caps & audio::hda::WCAP_CONN_LIST != 0 {
            let len = param(regs, rings, cad, nid, 0x0E);
            let long = len & 0x80 != 0;
            let n = (len & 0x7F) as usize;
            let per = if long { 2 } else { 4 };
            let mut i = 0;
            while i < n {
                let r = Hda::cmd(regs, rings, verb12(cad, nid, 0xF02, i as u8)).unwrap_or(0);
                for k in 0..per {
                    if i + k >= n {
                        break;
                    }
                    let e = if long {
                        (r >> (16 * k)) & 0xFFFF
                    } else {
                        (r >> (8 * k)) & 0xFF
                    };
                    // Range entries (bit 7/15) expand from the previous one.
                    let range = if long { e & 0x8000 != 0 } else { e & 0x80 != 0 };
                    let e = e & if long { 0x7FFF } else { 0x7F };
                    if range && let Some(&prev) = conns.last() {
                        for x in prev + 1..=e as u8 {
                            conns.push(x);
                        }
                    } else {
                        conns.push(e as u8);
                    }
                }
                i += per;
            }
        }
        let config = if kind == WidgetType::Pin {
            Hda::cmd(regs, rings, verb12(cad, nid, 0xF1C, 0)).unwrap_or(0)
        } else {
            0
        };
        widgets.push(Widget {
            nid,
            caps,
            pin_caps,
            config,
            conns,
            amp_in,
            amp_out,
        });
    }
    Some(Codec {
        vendor,
        afg,
        widgets,
    })
}

pub fn probe(dev: &PciDevice) {
    let Some(base) = dev.map_bar(0) else {
        crate::println!("[hda] {}: no register BAR", dev.name());
        return;
    };
    dev.enable();
    dev.enable_bus_master();
    let regs = Regs(base);
    // Controller reset: CRST low, then high; wait for codecs to report.
    regs.w32(GCTL, regs.r32(GCTL) & !1);
    let d = crate::time::Deadline::after_ms(100);
    while regs.r32(GCTL) & 1 != 0 && !d.expired() {}
    crate::time::sleep_ms(1);
    regs.w32(GCTL, regs.r32(GCTL) | 1);
    let d = crate::time::Deadline::after_ms(100);
    while regs.r32(GCTL) & 1 == 0 && !d.expired() {}
    if regs.r32(GCTL) & 1 == 0 {
        crate::println!("[hda] {}: controller did not leave reset", dev.name());
        return;
    }
    // Codecs announce themselves within 521 us of leaving reset.
    let mut codecs = 0;
    let d = crate::time::Deadline::after_ms(20);
    while codecs == 0 && !d.expired() {
        crate::time::sleep_ms(1);
        codecs = regs.r16(STATESTS);
    }
    if codecs == 0 {
        crate::println!("[hda] {}: no codecs", dev.name());
        return;
    }
    regs.w32(INTCTL, 0); // polled
    // CORB and RIRB: 256 entries each.
    let (Some(corb), Some(rirb)) = (DmaBuffer::new(1024), DmaBuffer::new(2048)) else {
        return;
    };
    regs.w8(CORBCTL, 0);
    regs.w8(RIRBCTL, 0);
    regs.w32(CORBLBASE, corb.phys() as u32);
    regs.w32(CORBUBASE, (corb.phys() >> 32) as u32);
    regs.w8(CORBSIZE, 2);
    regs.w16(CORBWP, 0);
    regs.w16(CORBRP, 0x8000);
    let d = crate::time::Deadline::after_ms(10);
    while regs.r16(CORBRP) & 0x8000 == 0 && !d.expired() {}
    regs.w16(CORBRP, 0);
    regs.w32(RIRBLBASE, rirb.phys() as u32);
    regs.w32(RIRBUBASE, (rirb.phys() >> 32) as u32);
    regs.w8(RIRBSIZE, 2);
    regs.w16(RIRBWP, 0x8000);
    // Response counting: some controllers (QEMU's) stop taking commands
    // when RINTCNT responses arrive until the RIRB interrupt flag is
    // cleared, which needs the RIRB interrupt enabled (INTCTL keeps it
    // from reaching the CPU).
    regs.w16(RINTCNT, 0xFF);
    regs.w8(CORBCTL, 2);
    regs.w8(RIRBCTL, 3);
    let mut rings = Rings {
        corb,
        rirb,
        rirb_rp: 0,
    };
    let gcap = regs.r16(GCAP);
    let (iss, oss) = (((gcap >> 8) & 0xF) as u64, ((gcap >> 12) & 0xF) as u64);
    for cad in 0..15u8 {
        if codecs & (1 << cad) == 0 {
            continue;
        }
        let Some(codec) = read_codec(&regs, &mut rings, cad) else {
            crate::println!(
                "[hda] {}: codec {}: no audio function group",
                dev.name(),
                cad
            );
            continue;
        };
        let out_route = codec.choose_output(|_| false);
        let in_route = codec.input_routes().into_iter().next();
        if out_route.is_none() {
            crate::println!(
                "[hda] {}: codec {} ({:08x}): no analog output",
                dev.name(),
                cad,
                codec.vendor
            );
            continue;
        }
        let new_stream = |sd: u64, tag: u8| -> Option<Stream> {
            Some(Stream {
                sd,
                tag,
                buf: DmaBuffer::new(BUF_BYTES)?,
                bdl: DmaBuffer::new(PERIODS * 16)?,
                pos: 0,
                running: false,
            })
        };
        if oss == 0 {
            crate::println!("[hda] {}: no output streams", dev.name());
            return;
        }
        let Some(play) = new_stream(SD_BASE + iss * 0x20, 1) else {
            return;
        };
        let rec = if iss > 0 && in_route.is_some() {
            new_stream(SD_BASE, 2)
        } else {
            None
        };
        let name = alloc::format!(
            "HDA {:04x}:{:04x}",
            codec.vendor >> 16,
            codec.vendor & 0xFFFF
        );
        let hda = Arc::new(Hda {
            name,
            regs: Regs(base),
            rings: Mutex::new(rings),
            codec_addr: cad,
            out_route: out_route.clone(),
            in_route: in_route.clone(),
            codec,
            play: Mutex::new(play),
            rec: rec.map(Mutex::new),
            out_gain: AtomicU32::new(80),
            hp_present: AtomicBool::new(false),
        });
        // Power the function group, then program the routes.
        hda.verb(hda.codec.afg, 0x705, 0);
        // GPIO 0 high: many laptops switch their speaker amplifier with it.
        hda.verb(hda.codec.afg, 0x716, 1);
        hda.verb(hda.codec.afg, 0x717, 1);
        hda.verb(hda.codec.afg, 0x715, 1);
        if let Some(r) = &out_route {
            hda.program_route(r, true);
            hda.quiet_other_outputs(r.pin);
            crate::println!(
                "[hda] {}: codec {} ({:08x}): output {} (pin {:#x}, DAC {:#x}){}",
                dev.name(),
                cad,
                hda.codec.vendor,
                r.kind.name(),
                r.pin,
                r.converter(),
                in_route
                    .as_ref()
                    .map(|i| alloc::format!(
                        ", input {} (pin {:#x}, ADC {:#x})",
                        i.kind.name(),
                        i.pin,
                        i.converter()
                    ))
                    .unwrap_or_default()
            );
        }
        super::register(hda);
        // One card per controller (the first codec with an output).
        return;
    }
}
