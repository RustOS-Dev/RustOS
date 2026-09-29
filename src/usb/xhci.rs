//! xHCI host controller driver.
//!
//! One command ring, one event ring (interrupter 0, MSI-X/MSI/INTx with a
//! polling fallback) and one transfer ring per endpoint. Callers submit a
//! transfer descriptor and block until its completion event arrives; events
//! are drained by whichever thread is waiting (or the controller's
//! background thread, which also handles root-port hotplug).

use super::{Speed, UsbDevice};
use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::pci::PciDevice;
use crate::sched::WaitQueue;
use crate::sched::mutex::Mutex as SleepMutex;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
use crate::sync::Mutex;

// Capability registers.
const CAPLENGTH: u64 = 0x00;
const HCSPARAMS1: u64 = 0x04;
const HCSPARAMS2: u64 = 0x08;
const HCCPARAMS1: u64 = 0x10;
const DBOFF: u64 = 0x14;
const RTSOFF: u64 = 0x18;

// Operational registers.
const USBCMD: u64 = 0x00;
const USBSTS: u64 = 0x04;
const CRCR: u64 = 0x18;
const DCBAAP: u64 = 0x30;
const CONFIG: u64 = 0x38;
const PORTS: u64 = 0x400;

const CMD_RS: u32 = 1 << 0;
const CMD_HCRST: u32 = 1 << 1;
const CMD_INTE: u32 = 1 << 2;
const STS_HCH: u32 = 1 << 0;
const STS_EINT: u32 = 1 << 3;
const STS_PCD: u32 = 1 << 4;
const STS_CNR: u32 = 1 << 11;

// PORTSC bits.
pub const PORT_CCS: u32 = 1 << 0;
pub const PORT_PED: u32 = 1 << 1;
pub const PORT_PR: u32 = 1 << 4;
pub const PORT_PP: u32 = 1 << 9;
const PORT_CHANGE_BITS: u32 = 0x7F << 17; // CSC PEC WRC OCC PRC PLC CEC
const PORT_PRC: u32 = 1 << 21;
const PORT_WRC: u32 = 1 << 19;
const PORT_WPR: u32 = 1 << 31;

// TRB types.
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_EP: u32 = 12;
const TRB_EVALUATE_CTX: u32 = 13;
const TRB_RESET_EP: u32 = 14;
const TRB_STOP_EP: u32 = 15;
const TRB_SET_TR_DEQUEUE: u32 = 16;
const EV_TRANSFER: u32 = 32;
const EV_COMMAND: u32 = 33;
const EV_PORT: u32 = 34;

const TRB_CYCLE: u32 = 1 << 0;
const TRB_TC: u32 = 1 << 1;
const TRB_ISP: u32 = 1 << 2;
const TRB_CH: u32 = 1 << 4;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;
const TRB_DIR_IN: u32 = 1 << 16;

pub const CC_SUCCESS: u8 = 1;
pub const CC_STALL: u8 = 6;
pub const CC_SHORT: u8 = 13;

const RING_TRBS: usize = 256;

fn r32(a: u64) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}
fn w32(a: u64, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}
fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
}

/// A producer ring (command or transfer) of 16-byte TRBs ending in a link.
struct Ring {
    mem: DmaBuffer,
    enq: usize,
    cycle: bool,
}

impl Ring {
    fn new() -> Option<Ring> {
        Some(Ring {
            mem: DmaBuffer::new(RING_TRBS * 16)?,
            enq: 0,
            cycle: true,
        })
    }

    fn phys(&self) -> u64 {
        self.mem.phys()
    }

    /// Dequeue pointer value for Set TR Dequeue (with DCS).
    fn enqueue_ptr(&self) -> u64 {
        (self.phys() + (self.enq * 16) as u64) | self.cycle as u64
    }

    fn push(&mut self, param: u64, status: u32, control: u32) -> u64 {
        let off = self.enq * 16;
        self.mem.write::<u64>(off, param);
        self.mem.write::<u32>(off + 8, status);
        fence(Ordering::SeqCst);
        self.mem
            .write::<u32>(off + 12, (control & !TRB_CYCLE) | self.cycle as u32);
        let addr = self.phys() + off as u64;
        self.enq += 1;
        if self.enq == RING_TRBS - 1 {
            let loff = self.enq * 16;
            self.mem.write::<u64>(loff, self.phys());
            self.mem.write::<u32>(loff + 8, 0);
            fence(Ordering::SeqCst);
            self.mem.write::<u32>(
                loff + 12,
                (TRB_LINK << 10) | TRB_TC | (control & TRB_CH) | self.cycle as u32,
            );
            self.cycle = !self.cycle;
            self.enq = 0;
        }
        addr
    }
}

struct EventRing {
    mem: DmaBuffer,
    _erst: DmaBuffer,
    deq: usize,
    cycle: bool,
}

#[derive(Clone, Copy, Debug)]
struct Event {
    param: u64,
    status: u32,
    control: u32,
}

impl Event {
    fn kind(&self) -> u32 {
        (self.control >> 10) & 0x3F
    }
    fn code(&self) -> u8 {
        (self.status >> 24) as u8
    }
    fn slot(&self) -> u8 {
        (self.control >> 24) as u8
    }
}

/// Per-endpoint transfer state.
struct EpState {
    ring: Ring,
    /// Transfer events for this endpoint not yet consumed: (trb, code, residual).
    events: VecDeque<(u64, u8, u32)>,
}

struct SlotData {
    out_ctx: DmaBuffer,
    in_ctx: DmaBuffer,
}

/// Where a new device sits in the topology (for its slot context).
#[derive(Clone, Copy, Debug, Default)]
pub struct SlotInfo {
    pub speed: Speed,
    pub route: u32,
    pub root_port: u8,
    pub tt_slot: u8,
    pub tt_port: u8,
    pub mtt: bool,
}

/// Endpoint description for Configure Endpoint.
#[derive(Clone, Copy, Debug)]
pub struct EpConfig {
    pub dci: u8,
    /// xHCI endpoint type (2 bulk out, 3 int out, 6 bulk in, 7 int in, ...).
    pub ep_type: u8,
    pub max_packet: u16,
    pub max_burst: u8,
    pub mult: u8,
    pub interval: u8,
}

pub struct HubConfig {
    pub ports: u8,
    pub ttt: u8,
    pub mtt: bool,
}

/// Serialises transfers on one endpoint.
type EpLock = Arc<SleepMutex<()>>;

pub struct Xhci {
    pub index: usize,
    pub pci: PciDevice,
    op: u64,
    rt: u64,
    db: u64,
    pub max_ports: u8,
    ctx_size: usize,
    dcbaa: DmaBuffer,
    _scratch: Vec<DmaBuffer>,
    cmd: Mutex<Ring>,
    cmd_lock: SleepMutex<()>,
    cmd_done: Mutex<BTreeMap<u64, (u8, u8)>>,
    events: Mutex<EventRing>,
    eps: Mutex<BTreeMap<(u8, u8), EpState>>,
    slots: Mutex<BTreeMap<u8, SlotData>>,
    /// Serialises transfers per endpoint.
    ep_locks: Mutex<BTreeMap<(u8, u8), EpLock>>,
    pub wq: Arc<WaitQueue>,
    irq: Arc<AtomicBool>,
    port_changes: AtomicU64,
    usb3_ports: Vec<bool>,
    /// Devices directly attached to root ports.
    pub roots: Mutex<BTreeMap<u8, Arc<UsbDevice>>>,
    pub irq_mode: &'static str,
}

impl Xhci {
    fn portsc_addr(&self, port: u8) -> u64 {
        self.op + PORTS + 0x10 * (port as u64 - 1)
    }

    pub fn portsc(&self, port: u8) -> u32 {
        r32(self.portsc_addr(port))
    }

    /// Write PORTSC preserving state, optionally setting bits (RW1S) and
    /// acknowledging change bits (RW1C).
    fn port_write(&self, port: u8, set: u32, ack: u32) {
        let v = self.portsc(port);
        let keep = v & !(PORT_PED | PORT_CHANGE_BITS | PORT_PR | PORT_WPR) & !(0xF << 5);
        w32(
            self.portsc_addr(port),
            keep | set | (ack & PORT_CHANGE_BITS),
        );
    }

    pub fn is_usb3_port(&self, port: u8) -> bool {
        self.usb3_ports
            .get(port as usize - 1)
            .copied()
            .unwrap_or(false)
    }

    /// Reset a root port and wait until it is enabled. Returns the speed.
    pub fn reset_port(&self, port: u8) -> KResult<Speed> {
        if self.portsc(port) & PORT_CCS == 0 {
            return Err(ENODEV);
        }
        if self.is_usb3_port(port) {
            // USB 3 ports enable themselves after link training; fall back
            // to a warm reset if that did not happen.
            if !crate::time::wait_until(200, || self.portsc(port) & PORT_PED != 0) {
                self.port_write(port, PORT_WPR, 0);
                crate::time::wait_until(500, || self.portsc(port) & PORT_WRC != 0);
                self.port_write(port, 0, PORT_WRC | PORT_PRC);
            }
        } else {
            self.port_write(port, PORT_PR, 0);
            crate::time::wait_until(500, || self.portsc(port) & PORT_PRC != 0);
            self.port_write(port, 0, PORT_PRC);
            crate::time::sleep_ms(10); // TRSTRCY
        }
        crate::time::wait_until(200, || self.portsc(port) & PORT_PED != 0);
        let sc = self.portsc(port);
        if sc & PORT_PED == 0 {
            return Err(EIO);
        }
        Ok(Speed::from_xhci((sc >> 10) & 0xF))
    }

    /// Acknowledge all change bits on a port.
    pub fn ack_port(&self, port: u8) {
        let v = self.portsc(port);
        self.port_write(port, 0, v & PORT_CHANGE_BITS);
    }

    pub fn has_port_changes(&self) -> bool {
        self.port_changes.load(Ordering::SeqCst) != 0 || self.irq.load(Ordering::SeqCst)
    }

    /// Take the set of root ports that reported a change.
    pub fn take_port_changes(&self) -> u64 {
        self.port_changes.swap(0, Ordering::SeqCst)
    }

    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    /// Drain the event ring. Safe to call from any thread.
    pub fn process_events(&self) {
        let Some(mut er) = self.events.try_lock() else {
            return;
        };
        self.irq.store(false, Ordering::SeqCst);
        let mut any = false;
        loop {
            let off = er.deq * 16;
            let control = er.mem.read::<u32>(off + 12);
            if (control & 1 != 0) != er.cycle {
                break;
            }
            fence(Ordering::SeqCst);
            let ev = Event {
                param: er.mem.read::<u64>(off),
                status: er.mem.read::<u32>(off + 8),
                control,
            };
            er.deq += 1;
            if er.deq == RING_TRBS {
                er.deq = 0;
                er.cycle = !er.cycle;
            }
            any = true;
            self.handle_event(ev);
        }
        if any {
            let deq = er.mem.phys() + (er.deq * 16) as u64;
            w64(self.rt + 0x20 + 0x18, deq | (1 << 3));
            drop(er);
            self.wq.wake_all();
        }
    }

    fn handle_event(&self, ev: Event) {
        match ev.kind() {
            EV_COMMAND => {
                self.cmd_done
                    .lock()
                    .insert(ev.param, (ev.code(), ev.slot()));
            }
            EV_TRANSFER => {
                let dci = ((ev.control >> 16) & 0x1F) as u8;
                if let Some(ep) = self.eps.lock().get_mut(&(ev.slot(), dci)) {
                    ep.events
                        .push_back((ev.param, ev.code(), ev.status & 0xFF_FFFF));
                    if ep.events.len() > 64 {
                        ep.events.pop_front();
                    }
                }
            }
            EV_PORT => {
                let port = (ev.param >> 24) as u8;
                if port > 0 && port <= 64 {
                    self.port_changes
                        .fetch_or(1 << (port - 1), Ordering::SeqCst);
                }
            }
            _ => {}
        }
    }

    /// Wait (processing events) until `done` or the timeout. Returns `done`.
    fn wait_for(
        &self,
        timeout_ms: Option<u64>,
        mut done: impl FnMut() -> bool,
        gone: &dyn Fn() -> bool,
    ) -> bool {
        let deadline = timeout_ms.map(crate::time::Deadline::after_ms);
        loop {
            self.process_events();
            if done() {
                return true;
            }
            if gone() || deadline.as_ref().is_some_and(|d| d.expired()) {
                return false;
            }
            let irq = self.irq.clone();
            self.wq
                .wait_timeout(4, || irq.load(Ordering::SeqCst) || gone());
        }
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    fn command(&self, param: u64, control: u32) -> KResult<(u8, u8)> {
        let _g = self.cmd_lock.lock();
        let trb = self.cmd.lock().push(param, 0, control);
        w32(self.db, 0);
        let mut res = None;
        let ok = self.wait_for(
            Some(5000),
            || {
                res = self.cmd_done.lock().remove(&trb);
                res.is_some()
            },
            &|| false,
        );
        if !ok {
            crate::println!(
                "[xhci{}] command {} timed out",
                self.index,
                (control >> 10) & 0x3F
            );
            return Err(ETIMEDOUT);
        }
        Ok(res.unwrap())
    }

    fn command_ok(&self, param: u64, control: u32) -> KResult<u8> {
        let (code, slot) = self.command(param, control)?;
        if code != CC_SUCCESS {
            crate::println!(
                "[xhci{}] command {} failed: completion code {}",
                self.index,
                (control >> 10) & 0x3F,
                code
            );
            return Err(EIO);
        }
        Ok(slot)
    }

    // ------------------------------------------------------------------
    // Contexts
    // ------------------------------------------------------------------

    fn in_ctx_off(&self, idx: usize) -> usize {
        idx * self.ctx_size
    }

    /// Enable a slot and address the device on it. Returns the slot id.
    pub fn address_device(&self, info: &SlotInfo, max_packet0: u16) -> KResult<u8> {
        let slot = self.command_ok(0, TRB_ENABLE_SLOT << 10)?;
        let (Some(out_ctx), Some(in_ctx), Some(ring)) = (
            DmaBuffer::new(32 * self.ctx_size),
            DmaBuffer::new(33 * self.ctx_size),
            Ring::new(),
        ) else {
            let _ = self.command(0, (TRB_DISABLE_SLOT << 10) | (slot as u32) << 24);
            return Err(ENOMEM);
        };
        self.dcbaa.write::<u64>(slot as usize * 8, out_ctx.phys());
        // Input control: add slot + EP0.
        in_ctx.write::<u32>(4, 0b11);
        let s = self.in_ctx_off(1);
        let dw0 = (info.route & 0xF_FFFF)
            | (info.speed.xhci() << 20)
            | ((info.mtt as u32) << 25)
            | (1 << 27);
        in_ctx.write::<u32>(s, dw0);
        in_ctx.write::<u32>(s + 4, (info.root_port as u32) << 16);
        in_ctx.write::<u32>(s + 8, info.tt_slot as u32 | (info.tt_port as u32) << 8);
        let e = self.in_ctx_off(2);
        in_ctx.write::<u32>(e + 4, (3 << 1) | (4 << 3) | ((max_packet0 as u32) << 16));
        in_ctx.write::<u64>(e + 8, ring.phys() | 1);
        in_ctx.write::<u32>(e + 16, 8);
        self.eps.lock().insert(
            (slot, 1),
            EpState {
                ring,
                events: VecDeque::new(),
            },
        );
        let in_phys = in_ctx.phys();
        self.slots.lock().insert(slot, SlotData { out_ctx, in_ctx });
        if let Err(e) = self.command_ok(in_phys, (TRB_ADDRESS_DEVICE << 10) | (slot as u32) << 24) {
            self.free_slot(slot);
            return Err(e);
        }
        Ok(slot)
    }

    /// Update EP0's max packet size (after reading the device descriptor).
    pub fn set_max_packet0(&self, slot: u8, mps: u16) -> KResult<()> {
        let phys = {
            let slots = self.slots.lock();
            let sd = slots.get(&slot).ok_or(ENODEV)?;
            sd.in_ctx.zero();
            sd.in_ctx.write::<u32>(4, 0b10);
            let e = self.in_ctx_off(2);
            sd.in_ctx
                .write::<u32>(e + 4, (3 << 1) | (4 << 3) | ((mps as u32) << 16));
            sd.in_ctx.phys()
        };
        self.command_ok(phys, (TRB_EVALUATE_CTX << 10) | (slot as u32) << 24)
            .map(|_| ())
    }

    /// Add endpoints (and optionally hub information) to a slot.
    pub fn configure_endpoints(
        &self,
        slot: u8,
        eps: &[EpConfig],
        hub: Option<HubConfig>,
    ) -> KResult<()> {
        let mut rings = Vec::new();
        let phys = {
            let slots = self.slots.lock();
            let sd = slots.get(&slot).ok_or(ENODEV)?;
            let inc = &sd.in_ctx;
            inc.zero();
            // Copy the current slot context from the output context.
            let s = self.in_ctx_off(1);
            for i in 0..8 {
                inc.write::<u32>(s + i * 4, sd.out_ctx.read::<u32>(i * 4));
            }
            let mut add = 1u32;
            let mut max_dci = (sd.out_ctx.read::<u32>(0) >> 27) & 0x1F;
            for ep in eps {
                let ring = Ring::new().ok_or(ENOMEM)?;
                add |= 1 << ep.dci;
                max_dci = max_dci.max(ep.dci as u32);
                let o = self.in_ctx_off(1 + ep.dci as usize);
                let periodic = matches!(ep.ep_type, 1 | 3 | 5 | 7);
                let isoch = matches!(ep.ep_type, 1 | 5);
                let esit = ep.max_packet as u32 * (ep.max_burst as u32 + 1) * (ep.mult as u32 + 1);
                inc.write::<u32>(
                    o,
                    ((ep.mult as u32) << 8) | ((ep.interval as u32) << 16) | ((esit >> 16) << 24),
                );
                let cerr = if isoch { 0 } else { 3 };
                inc.write::<u32>(
                    o + 4,
                    (cerr << 1)
                        | ((ep.ep_type as u32) << 3)
                        | ((ep.max_burst as u32) << 8)
                        | ((ep.max_packet as u32) << 16),
                );
                inc.write::<u64>(o + 8, ring.phys() | 1);
                let avg = if periodic { esit.min(1024) } else { 3072 };
                inc.write::<u32>(o + 16, avg | ((esit & 0xFFFF) << 16));
                rings.push((ep.dci, ring));
            }
            let mut dw0 = inc.read::<u32>(s);
            dw0 = (dw0 & !(0x1F << 27)) | (max_dci << 27);
            if let Some(h) = &hub {
                dw0 |= 1 << 26;
                if h.mtt {
                    dw0 |= 1 << 25;
                }
                let dw1 = inc.read::<u32>(s + 4);
                inc.write::<u32>(s + 4, (dw1 & 0x00FF_FFFF) | (h.ports as u32) << 24);
                let dw2 = inc.read::<u32>(s + 8);
                inc.write::<u32>(s + 8, (dw2 & !(3 << 16)) | ((h.ttt as u32) << 16));
            }
            inc.write::<u32>(s, dw0);
            // Slot state lives in dword 3 of the output context; clear it
            // in the input copy.
            inc.write::<u32>(s + 12, 0);
            inc.write::<u32>(4, add);
            inc.phys()
        };
        {
            let mut map = self.eps.lock();
            for (dci, ring) in rings {
                map.insert(
                    (slot, dci),
                    EpState {
                        ring,
                        events: VecDeque::new(),
                    },
                );
            }
        }
        self.command_ok(phys, (TRB_CONFIGURE_EP << 10) | (slot as u32) << 24)
            .map(|_| ())
    }

    /// Release a slot (device detached).
    pub fn free_slot(&self, slot: u8) {
        let _ = self.command(0, (TRB_DISABLE_SLOT << 10) | (slot as u32) << 24);
        self.dcbaa.write::<u64>(slot as usize * 8, 0);
        self.eps.lock().retain(|(s, _), _| *s != slot);
        self.ep_locks.lock().retain(|(s, _), _| *s != slot);
        self.slots.lock().remove(&slot);
    }

    fn ep_lock(&self, slot: u8, dci: u8) -> Arc<SleepMutex<()>> {
        self.ep_locks
            .lock()
            .entry((slot, dci))
            .or_insert_with(|| Arc::new(SleepMutex::new(())))
            .clone()
    }

    /// Recover a halted or stuck endpoint: reset it (if halted) and move
    /// its dequeue pointer past everything queued.
    fn recover(&self, slot: u8, dci: u8, halted: bool) {
        if halted {
            let _ = self.command(
                0,
                (TRB_RESET_EP << 10) | (dci as u32) << 16 | (slot as u32) << 24,
            );
        } else {
            let _ = self.command(
                0,
                (TRB_STOP_EP << 10) | (dci as u32) << 16 | (slot as u32) << 24,
            );
        }
        let ptr = match self.eps.lock().get_mut(&(slot, dci)) {
            Some(ep) => {
                ep.events.clear();
                ep.ring.enqueue_ptr()
            }
            None => return,
        };
        let _ = self.command(
            ptr,
            (TRB_SET_TR_DEQUEUE << 10) | (dci as u32) << 16 | (slot as u32) << 24,
        );
    }

    /// Queue a TD (list of TRBs as (param, status, control)) and wait for it.
    /// Returns (completion code, bytes transferred for data TRBs).
    fn run_td(
        &self,
        dev: &UsbDevice,
        dci: u8,
        trbs: &[(u64, u32, u32)],
        timeout_ms: Option<u64>,
    ) -> KResult<usize> {
        let slot = dev.slot;
        let lock = self.ep_lock(slot, dci);
        let _g = lock.lock();
        if dev.is_gone() {
            return Err(ENODEV);
        }
        let addrs: Vec<u64> = {
            let mut map = self.eps.lock();
            let ep = map.get_mut(&(slot, dci)).ok_or(ENODEV)?;
            ep.events.clear();
            trbs.iter()
                .map(|&(p, s, c)| ep.ring.push(p, s, c))
                .collect()
        };
        w32(self.db + 4 * slot as u64, dci as u32);
        let last = *addrs.last().unwrap();
        let mut result: Option<(u8, usize)> = None;
        let mut moved = 0usize;
        let data_len = |i: usize| -> usize {
            let t = (trbs[i].2 >> 10) & 0x3F;
            if t == TRB_NORMAL || t == TRB_DATA {
                (trbs[i].1 & 0x1_FFFF) as usize
            } else {
                0
            }
        };
        let done = self.wait_for(
            timeout_ms,
            || {
                let mut map = self.eps.lock();
                let Some(ep) = map.get_mut(&(slot, dci)) else {
                    result = Some((0, 0));
                    return true;
                };
                while let Some((trb, code, residual)) = ep.events.pop_front() {
                    let Some(i) = addrs.iter().position(|&a| a == trb) else {
                        continue; // stale event from an earlier TD
                    };
                    let before: usize = (0..i).map(data_len).sum();
                    match code {
                        CC_SUCCESS => {
                            if trb == last {
                                if moved == 0 {
                                    moved = (0..trbs.len()).map(data_len).sum();
                                }
                                result = Some((code, moved));
                                return true;
                            }
                        }
                        CC_SHORT => {
                            moved = before + data_len(i).saturating_sub(residual as usize);
                            let t = (trbs[i].2 >> 10) & 0x3F;
                            // Control transfers continue with the status stage.
                            if t != TRB_DATA || trb == last {
                                result = Some((code, moved));
                                return true;
                            }
                        }
                        _ => {
                            result = Some((code, before));
                            return true;
                        }
                    }
                }
                false
            },
            &|| dev.is_gone(),
        );
        if !done {
            if dev.is_gone() {
                return Err(ENODEV);
            }
            self.recover(slot, dci, false);
            return Err(ETIMEDOUT);
        }
        let (code, n) = result.unwrap();
        match code {
            0 => Err(ENODEV),
            CC_SUCCESS | CC_SHORT => Ok(n),
            CC_STALL => {
                self.recover(slot, dci, true);
                Err(EPIPE)
            }
            c => {
                crate::println!(
                    "[xhci{}] slot {} ep {}: completion code {}",
                    self.index,
                    slot,
                    dci,
                    c
                );
                self.recover(slot, dci, true);
                Err(EIO)
            }
        }
    }

    /// Control transfer on EP0. `data` is the DMA buffer for the data stage.
    pub fn control(
        &self,
        dev: &UsbDevice,
        setup: [u8; 8],
        data: Option<(&DmaBuffer, usize)>,
        timeout_ms: u64,
    ) -> KResult<usize> {
        let dir_in = setup[0] & 0x80 != 0;
        let len = data.map_or(0, |(_, l)| l);
        let trt = if len == 0 {
            0
        } else if dir_in {
            3 << 16
        } else {
            2 << 16
        };
        let mut trbs = alloc::vec![(
            u64::from_le_bytes(setup),
            8,
            (TRB_SETUP << 10) | TRB_IDT | trt
        )];
        if let Some((buf, l)) = data
            && l > 0
        {
            let dir = if dir_in { TRB_DIR_IN | TRB_ISP } else { 0 };
            trbs.push((buf.phys(), l as u32, (TRB_DATA << 10) | dir));
        }
        let status_in = len == 0 || !dir_in;
        trbs.push((
            0,
            0,
            (TRB_STATUS << 10) | TRB_IOC | if status_in { TRB_DIR_IN } else { 0 },
        ));
        self.run_td(dev, 1, &trbs, Some(timeout_ms))
    }

    /// Bulk or interrupt transfer on endpoint `dci` using `phys..phys+len`.
    pub fn transfer(
        &self,
        dev: &UsbDevice,
        dci: u8,
        phys: u64,
        len: usize,
        timeout_ms: Option<u64>,
    ) -> KResult<usize> {
        let dir_in = dci & 1 == 1;
        let mut trbs = Vec::new();
        let mut off = 0usize;
        while off < len || (len == 0 && trbs.is_empty()) {
            let addr = phys + off as u64;
            // TRB buffers must not cross a 64 KiB boundary.
            let room = (0x1_0000 - (addr & 0xFFFF)) as usize;
            let n = room.min(len - off);
            let remaining_after = len - off - n;
            let td_size = (remaining_after.div_ceil(512)).min(31) as u32;
            let mut ctl = (TRB_NORMAL << 10) | if dir_in { TRB_ISP } else { 0 };
            ctl |= if remaining_after > 0 { TRB_CH } else { TRB_IOC };
            trbs.push((addr, n as u32 | (td_size << 17), ctl));
            off += n;
            if len == 0 {
                break;
            }
        }
        self.run_td(dev, dci, &trbs, timeout_ms)
    }
}

/// Probe an xHCI controller and start its root hub thread.
pub fn probe(dev: &PciDevice, index: usize) -> Option<Arc<Xhci>> {
    dev.set_power_d0();
    dev.enable();
    let base = dev.map_bar(0)?;
    let caplen = r32(base + CAPLENGTH) & 0xFF;
    let hcs1 = r32(base + HCSPARAMS1);
    let hcs2 = r32(base + HCSPARAMS2);
    let hcc1 = r32(base + HCCPARAMS1);
    let op = base + caplen as u64;
    let rt = base + (r32(base + RTSOFF) & !0x1F) as u64;
    let db = base + (r32(base + DBOFF) & !0x3) as u64;
    let max_slots = (hcs1 & 0xFF) as u8;
    let max_ports = (hcs1 >> 24) as u8;
    let ctx_size = if hcc1 & (1 << 2) != 0 { 64 } else { 32 };
    let name = dev.name();

    // Extended capabilities: legacy handoff and supported protocols.
    let mut usb3_ports = alloc::vec![false; max_ports as usize];
    let mut xecp = ((hcc1 >> 16) & 0xFFFF) as u64 * 4;
    while xecp != 0 {
        let a = base + xecp;
        let v = r32(a);
        match v & 0xFF {
            1 => {
                if v & (1 << 16) != 0 {
                    w32(a, v | (1 << 24));
                    if !crate::time::wait_until(1000, || r32(a) & (1 << 16) == 0) {
                        crate::println!("[xhci] {}: BIOS did not release the controller", name);
                    }
                }
                // Disable SMIs.
                w32(a + 4, r32(a + 4) & 0xE000_0000);
            }
            2 => {
                let major = v >> 24;
                let ports = r32(a + 8);
                let first = (ports & 0xFF) as usize;
                let count = ((ports >> 8) & 0xFF) as usize;
                if major >= 3 {
                    for p in first..first + count {
                        if p >= 1 && p <= usb3_ports.len() {
                            usb3_ports[p - 1] = true;
                        }
                    }
                }
            }
            _ => {}
        }
        let next = ((v >> 8) & 0xFF) as u64 * 4;
        xecp = if next == 0 { 0 } else { xecp + next };
    }

    // Stop and reset.
    w32(op + USBCMD, r32(op + USBCMD) & !CMD_RS);
    crate::time::wait_until(100, || r32(op + USBSTS) & STS_HCH != 0);
    w32(op + USBCMD, CMD_HCRST);
    if !crate::time::wait_until(1000, || {
        r32(op + USBCMD) & CMD_HCRST == 0 && r32(op + USBSTS) & STS_CNR == 0
    }) {
        crate::println!("[xhci] {}: reset timed out", name);
        return None;
    }

    w32(op + CONFIG, max_slots as u32);
    let dcbaa = DmaBuffer::new((max_slots as usize + 1) * 8)?;
    let scratch_n = (((hcs2 >> 21) & 0x1F) << 5 | (hcs2 >> 27)) as usize;
    let mut scratch = Vec::new();
    if scratch_n > 0 {
        let arr = DmaBuffer::new(scratch_n * 8)?;
        for i in 0..scratch_n {
            let page = DmaBuffer::new(4096)?;
            arr.write::<u64>(i * 8, page.phys());
            scratch.push(page);
        }
        dcbaa.write::<u64>(0, arr.phys());
        scratch.push(arr);
    }
    w64(op + DCBAAP, dcbaa.phys());

    let cmd = Ring::new()?;
    w64(op + CRCR, cmd.phys() | 1);

    let ev_mem = DmaBuffer::new(RING_TRBS * 16)?;
    let erst = DmaBuffer::new(64)?;
    erst.write::<u64>(0, ev_mem.phys());
    erst.write::<u32>(8, RING_TRBS as u32);
    let ir0 = rt + 0x20;
    w32(ir0 + 0x08, 1); // ERSTSZ
    w64(ir0 + 0x18, ev_mem.phys()); // ERDP
    w64(ir0 + 0x10, erst.phys()); // ERSTBA
    w32(ir0 + 0x04, 160); // IMOD: 40 µs

    let wq = Arc::new(WaitQueue::new());
    let irq = Arc::new(AtomicBool::new(false));
    let (w, f) = (wq.clone(), irq.clone());
    let handler: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        w32(ir0, 0b11); // IMAN: ack IP, keep IE
        w32(op + USBSTS, STS_EINT | STS_PCD);
        f.store(true, Ordering::SeqCst);
        w.wake_all();
    });
    let irq_mode = if dev.enable_irq(handler).is_some() {
        if dev.msix_count().is_some() {
            "MSI-X"
        } else {
            "MSI/INTx"
        }
    } else {
        "polling"
    };
    w32(ir0, 0b10); // IMAN.IE

    let hc = Arc::new(Xhci {
        index,
        pci: dev.clone(),
        op,
        rt,
        db,
        max_ports,
        ctx_size,
        dcbaa,
        _scratch: scratch,
        cmd: Mutex::new(cmd),
        cmd_lock: SleepMutex::new(()),
        cmd_done: Mutex::new(BTreeMap::new()),
        events: Mutex::new(EventRing {
            mem: ev_mem,
            _erst: erst,
            deq: 0,
            cycle: true,
        }),
        eps: Mutex::new(BTreeMap::new()),
        slots: Mutex::new(BTreeMap::new()),
        ep_locks: Mutex::new(BTreeMap::new()),
        wq,
        irq,
        port_changes: AtomicU64::new(0),
        usb3_ports,
        roots: Mutex::new(BTreeMap::new()),
        irq_mode,
    });

    w32(op + USBCMD, CMD_RS | CMD_INTE);
    if !crate::time::wait_until(100, || r32(op + USBSTS) & STS_HCH == 0) {
        crate::println!("[xhci] {}: controller did not start", name);
        return None;
    }
    // Power all ports (no-op when port power switching is unsupported).
    for p in 1..=max_ports {
        if hc.portsc(p) & PORT_PP == 0 {
            hc.port_write(p, PORT_PP, 0);
        }
    }
    let n3 = hc.usb3_ports.iter().filter(|&&b| b).count();
    crate::println!(
        "[xhci{}] {}: xHCI {}.{}, {} slots, {} ports ({} USB3), {}",
        index,
        name,
        r32(base) >> 24,
        (r32(base) >> 16) & 0xFF,
        max_slots,
        max_ports,
        n3,
        irq_mode
    );
    // Every connected port counts as changed initially.
    hc.port_changes.store(
        if max_ports >= 64 {
            u64::MAX
        } else {
            (1u64 << max_ports) - 1
        },
        Ordering::SeqCst,
    );
    Some(hc)
}

/// Halt a controller before reboot so devices see a clean disconnect.
pub fn halt(hc: &Xhci) {
    w32(hc.op + USBCMD, r32(hc.op + USBCMD) & !(CMD_RS | CMD_INTE));
    crate::time::wait_until(20, || r32(hc.op + USBSTS) & STS_HCH != 0);
}

pub fn name_of(hc: &Xhci) -> String {
    alloc::format!("xhci{}", hc.index)
}
