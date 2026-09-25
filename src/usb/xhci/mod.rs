//! XHCI USB 3.x Host Controller driver.
//!
//! Supports:
//!  - Controller discovery via PCI (uses `crate::pci`)
//!  - MMIO register access (volatile reads/writes)
//!  - Controller reset and ring initialisation
//!  - Port detection and device enumeration
//!  - Control transfers (GET_DESCRIPTOR, SET_CONFIGURATION)
//!  - Bulk transfers (for USB Mass Storage)
//!  - Exposes a `read_sectors` / `write_sector` interface used by the FAT32 driver

pub mod ring;
pub mod trb;

use crate::mm::{dma_alloc, map_mmio as map_mmio_region};
use crate::pci::PciDevice;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use ring::{CommandRing, EventRing, TransferRing};
use trb::*;

static SCSI_TAG: AtomicU32 = AtomicU32::new(1);

// ---------------------------------------------------------------------------
// XHCI register offsets (all relative to MMIO BAR0)
// ---------------------------------------------------------------------------

// Capability registers (at mmio_base)
const CAP_CAPLENGTH: usize = 0x00;
const CAP_HCSPARAMS1: usize = 0x04;
#[allow(dead_code)]
const CAP_HCSPARAMS2: usize = 0x08;
#[allow(dead_code)]
const CAP_HCCPARAMS1: usize = 0x10;
const CAP_RTSOFF: usize = 0x18;
const CAP_DBOFF: usize = 0x1C;

// Operational register offsets (relative to op_base = mmio_base + caplength)
const OP_USBCMD: usize = 0x00;
const OP_USBSTS: usize = 0x04;
#[allow(dead_code)]
const OP_DNCTRL: usize = 0x14;
const OP_CRCR: usize = 0x18;
const OP_DCBAAP: usize = 0x30;
const OP_CONFIG: usize = 0x38;

// Port register set (relative to op_base + 0x400 + 0x10 * port_index)
const PORT_PORTSC: usize = 0x00;

// Runtime registers (relative to rt_base = mmio_base + rtsoff)
// Interrupter 0 registers (at rt_base + 0x20)
const RT_IR0_IMAN: usize = 0x20;
#[allow(dead_code)]
const RT_IR0_IMOD: usize = 0x24;
const RT_IR0_ERSTSZ: usize = 0x28;
const RT_IR0_ERSTBA: usize = 0x30;
const RT_IR0_ERDP: usize = 0x38;

// USBCMD bits
const CMD_RUN: u32 = 1 << 0;
const CMD_HCRST: u32 = 1 << 1;
#[allow(dead_code)]
const CMD_INTE: u32 = 1 << 2;

// USBSTS bits
const STS_HCH: u32 = 1 << 0; // HCHalted
const STS_CNR: u32 = 1 << 11; // Controller Not Ready

// PORTSC bits
const PORTSC_CCS: u32 = 1 << 0; // Current Connect Status
const PORTSC_PED: u32 = 1 << 1; // Port Enabled/Disabled
const PORTSC_PR: u32 = 1 << 4; // Port Reset
const PORTSC_PRC: u32 = 1 << 21; // Port Reset Change
const PORTSC_CSC: u32 = 1 << 17; // Connect Status Change

// ---------------------------------------------------------------------------
// XHCI Device Context structures (in DMA memory)
// ---------------------------------------------------------------------------

// Compile-time size assertions
macro_rules! const_assert_size {
    ($t:ty, $n:expr) => {
        const _: () = assert!(core::mem::size_of::<$t>() == $n);
    };
}

/// 32-byte Slot Context
#[repr(C, align(32))]
struct SlotCtx {
    dword: [u32; 8],
}
const_assert_size!(SlotCtx, 32);

/// 32-byte Endpoint Context
#[repr(C, align(32))]
struct EpCtx {
    dword: [u32; 8],
}
const_assert_size!(EpCtx, 32);

/// Output Device Context (32 + 31*32 = 1024 bytes = 32 * 32)
#[repr(C, align(64))]
struct DevCtx {
    slot: SlotCtx,
    ep: [EpCtx; 31],
}
const_assert_size!(DevCtx, 1024);

/// Input Control Context (32 bytes)
#[repr(C, align(32))]
struct InputCtrlCtx {
    drop_flags: u32,
    add_flags: u32,
    _rsvd: [u32; 6],
}

/// Input Context = InputCtrlCtx + DevCtx (1024 + 32 = 1056 bytes)
#[repr(C, align(64))]
struct InputCtx {
    ctrl: InputCtrlCtx,
    dev_ctx: DevCtx,
}

// ---------------------------------------------------------------------------
// Endpoint descriptor (from config descriptor)
// ---------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub struct EndpointDesc {
    pub addr: u8,       // bEndpointAddress
    pub attributes: u8, // bmAttributes
    pub max_packet: u16,
    pub interval: u8,
}

impl EndpointDesc {
    pub fn is_bulk_in(&self) -> bool {
        (self.attributes & 0x3) == 2 && (self.addr & 0x80) != 0
    }
    pub fn is_bulk_out(&self) -> bool {
        (self.attributes & 0x3) == 2 && (self.addr & 0x80) == 0
    }
    pub fn ep_num(&self) -> u8 {
        self.addr & 0x7F
    }
}

// ---------------------------------------------------------------------------
// Connected USB storage device
// ---------------------------------------------------------------------------
pub struct UsbDevice {
    slot_id: u8,
    bulk_in_ep: u8, // endpoint number (1-15)
    bulk_out_ep: u8,
    bulk_in_ring: TransferRing,
    bulk_out_ring: TransferRing,
    ctrl_ring: TransferRing,
    pub block_count: u64,
    pub block_size: u32,
}

// ---------------------------------------------------------------------------
// XHCI Controller
// ---------------------------------------------------------------------------
pub struct Xhci {
    #[allow(dead_code)]
    mmio_base: u64,
    op_base: u64,
    rt_base: u64,
    db_base: u64,
    max_ports: u8,
    #[allow(dead_code)]
    max_slots: u8,

    cmd_ring: CommandRing,
    event_ring: EventRing,

    // DCBAA: array of 256 u64 physical pointers (one per slot, 0=scratchpad)
    dcbaa: *mut u64,
    #[allow(dead_code)]
    dcbaa_phys: u64,

    pub devices: Vec<UsbDevice>,
}

unsafe impl Send for Xhci {}

// ---------------------------------------------------------------------------
// Register access helpers
// ---------------------------------------------------------------------------

#[inline]
unsafe fn read32(base: u64, off: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base + off as u64) as *const u32) }
}

#[inline]
unsafe fn write32(base: u64, off: usize, val: u32) {
    unsafe {
        core::ptr::write_volatile((base + off as u64) as *mut u32, val);
    }
}

#[allow(dead_code)]
#[inline]
unsafe fn read64(base: u64, off: usize) -> u64 {
    unsafe { core::ptr::read_volatile((base + off as u64) as *const u64) }
}

#[inline]
unsafe fn write64(base: u64, off: usize, val: u64) {
    unsafe {
        core::ptr::write_volatile((base + off as u64) as *mut u64, val);
    }
}

fn spin_until<F: Fn() -> bool>(f: F, msg: &str) {
    for _ in 0..1_000_000 {
        if f() {
            return;
        }
        core::hint::spin_loop();
    }
    crate::serial_println!("[xhci] timeout waiting for: {}", msg);
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl Xhci {
    /// Initialise an XHCI controller found at `pci_dev`.
    pub fn init(pci_dev: &PciDevice) -> Option<Self> {
        let bar_phys = pci_dev.mmio_base(0);
        if bar_phys == 0 {
            crate::serial_println!("[xhci] BAR0 is zero");
            return None;
        }

        // Map ~64 KiB of MMIO
        let mmio_base = map_mmio_region(bar_phys, 0x1_0000);
        crate::serial_println!(
            "[xhci] MMIO base virt=0x{:x} phys=0x{:x}",
            mmio_base,
            bar_phys
        );

        let caplength = unsafe { (read32(mmio_base, CAP_CAPLENGTH) & 0xFF) as u64 };
        let op_base = mmio_base + caplength;
        let hcsparams1 = unsafe { read32(mmio_base, CAP_HCSPARAMS1) };
        let max_slots = (hcsparams1 & 0xFF) as u8;
        let max_ports = ((hcsparams1 >> 24) & 0xFF) as u8;
        let rtsoff = unsafe { read32(mmio_base, CAP_RTSOFF) & !0x1F } as u64;
        let dboff = unsafe { read32(mmio_base, CAP_DBOFF) & !0x3 } as u64;
        let rt_base = mmio_base + rtsoff;
        let db_base = mmio_base + dboff;

        crate::serial_println!("[xhci] slots={} ports={}", max_slots, max_ports);

        // --- Reset controller ---
        unsafe {
            // Halt first if running
            let cmd = read32(op_base, OP_USBCMD);
            write32(op_base, OP_USBCMD, cmd & !CMD_RUN);
            spin_until(|| read32(op_base, OP_USBSTS) & STS_HCH != 0, "halt");

            // Reset
            write32(op_base, OP_USBCMD, CMD_HCRST);
            spin_until(
                || read32(op_base, OP_USBCMD) & CMD_HCRST == 0,
                "HCRST clear",
            );
            spin_until(|| read32(op_base, OP_USBSTS) & STS_CNR == 0, "CNR clear");
        }

        // --- DCBAA (Device Context Base Address Array) ---
        let dcbaa_size = 256 * 8; // 256 slots × 8 bytes
        let (dcbaa_virt, dcbaa_phys) = dma_alloc(dcbaa_size, 64);

        // --- Command Ring ---
        let cmd_ring = CommandRing::new();

        // --- Event Ring ---
        let event_ring = EventRing::new();

        unsafe {
            // MaxSlotsEn
            write32(op_base, OP_CONFIG, max_slots as u32);

            // DCBAAP
            write64(op_base, OP_DCBAAP, dcbaa_phys);

            // CRCR (Command Ring Control Register)
            // bit 0 = Consumer Cycle State (initial = 1)
            write64(op_base, OP_CRCR, cmd_ring.phys | 1);

            // Event Ring Segment Table
            write32(rt_base, RT_IR0_ERSTSZ, 1);
            write64(rt_base, RT_IR0_ERSTBA, event_ring.erst_phys);
            write64(rt_base, RT_IR0_ERDP, event_ring.phys);

            // Clear IMAN IP bit
            let iman = read32(rt_base, RT_IR0_IMAN);
            write32(rt_base, RT_IR0_IMAN, iman | 2); // clear IP

            // Run!
            write32(op_base, OP_USBCMD, CMD_RUN);
            spin_until(|| read32(op_base, OP_USBSTS) & STS_HCH == 0, "run");
        }

        crate::serial_println!("[xhci] controller running");

        let mut ctrl = Xhci {
            mmio_base,
            op_base,
            rt_base,
            db_base,
            max_ports,
            max_slots,
            cmd_ring,
            event_ring,
            dcbaa: dcbaa_virt as *mut u64,
            dcbaa_phys,
            devices: Vec::new(),
        };

        // --- Enumerate ports ---
        ctrl.enumerate_ports();

        Some(ctrl)
    }

    // -----------------------------------------------------------------------
    // Ring doorbell
    // -----------------------------------------------------------------------

    fn ring_doorbell(&self, slot: u8, endpoint: u8) {
        // DB[0] = host controller command doorbell, DB[slot] = device doorbell
        let db_off = (slot as u64) * 4;
        unsafe {
            write32(self.db_base, db_off as usize, endpoint as u32);
        }
    }

    // -----------------------------------------------------------------------
    // Send a command TRB and wait for Command Completion Event
    // -----------------------------------------------------------------------

    fn send_command(&mut self, trb: Trb) -> Option<Trb> {
        self.cmd_ring.push(trb);
        self.ring_doorbell(0, 0);
        self.wait_event(ty::CMD_COMPLETION_EVENT)
    }

    // -----------------------------------------------------------------------
    // Wait for a specific event type (polling)
    // -----------------------------------------------------------------------

    fn wait_event(&mut self, expected_type: u32) -> Option<Trb> {
        for _ in 0..10_000_000u64 {
            if let Some(evt) = self.event_ring.pop() {
                // Update ERDP (acknowledge)
                let dp = self.event_ring.dequeue_phys();
                unsafe {
                    write64(self.rt_base, RT_IR0_ERDP, dp | (1 << 3));
                }

                if evt.trb_type() == expected_type {
                    return Some(evt);
                }
                // Discard unexpected events (e.g. port status changes during enum)
                continue;
            }
            core::hint::spin_loop();
        }
        crate::serial_println!("[xhci] wait_event timed out for type={}", expected_type);
        None
    }

    // -----------------------------------------------------------------------
    // Port enumeration
    // -----------------------------------------------------------------------

    fn enumerate_ports(&mut self) {
        let num_ports = self.max_ports;
        for port in 0..num_ports {
            let portsc =
                unsafe { read32(self.op_base, 0x400 + (port as usize) * 0x10 + PORT_PORTSC) };
            if portsc & PORTSC_CCS == 0 {
                continue;
            }
            crate::serial_println!("[xhci] port {} connected (PORTSC=0x{:x})", port, portsc);
            self.reset_port(port);
        }
    }

    fn reset_port(&mut self, port: u8) {
        let base = self.op_base;
        let port_reg = base + 0x400 + (port as u64) * 0x10 + PORT_PORTSC as u64;

        // Write PR bit to start reset
        let portsc = unsafe { core::ptr::read_volatile(port_reg as *const u32) };
        unsafe {
            core::ptr::write_volatile(
                port_reg as *mut u32,
                (portsc | PORTSC_PR) & !(PORTSC_PRC | PORTSC_CSC),
            );
        }

        // Wait for PRC (Port Reset Change) to indicate reset complete
        spin_until(
            || {
                let s = unsafe { core::ptr::read_volatile(port_reg as *const u32) };
                s & PORTSC_PRC != 0
            },
            "port reset",
        );

        // Clear PRC. On USB 2.0 ports PED is RW1CS — writing 1 *disables* the
        // port. Mask PED to 0 so we only clear PRC, leaving the freshly enabled
        // port intact. Writing 0 to any RW1CS bit is a no-op per the xHCI spec.
        let portsc = unsafe { core::ptr::read_volatile(port_reg as *const u32) };
        unsafe {
            core::ptr::write_volatile(port_reg as *mut u32, (portsc & !PORTSC_PED) | PORTSC_PRC);
        }

        // Check port is enabled
        let portsc = unsafe { core::ptr::read_volatile(port_reg as *const u32) };
        if portsc & PORTSC_PED == 0 {
            crate::serial_println!("[xhci] port {} not enabled after reset", port);
            return;
        }

        // Determine speed: bits [13:10] of PORTSC
        let speed = ((portsc >> 10) & 0xF) as u8;
        self.enable_slot(port, speed);
    }

    // -----------------------------------------------------------------------
    // Slot enable + Address Device
    // -----------------------------------------------------------------------

    fn enable_slot(&mut self, port: u8, speed: u8) {
        // Enable Slot command
        let cmd = enable_slot_cmd(true);
        let evt = match self.send_command(cmd) {
            Some(e) => e,
            None => return,
        };
        if evt.completion_code() != cc::SUCCESS {
            crate::serial_println!("[xhci] Enable Slot failed cc={}", evt.completion_code());
            return;
        }
        let slot_id = evt.slot_id();
        crate::serial_println!("[xhci] slot_id={} assigned for port {}", slot_id, port);

        // Allocate Output Device Context
        let (dev_ctx_virt, dev_ctx_phys) = dma_alloc(core::mem::size_of::<DevCtx>(), 64);
        unsafe {
            core::ptr::write_bytes(dev_ctx_virt, 0, core::mem::size_of::<DevCtx>());
        }

        // Store in DCBAA
        unsafe {
            self.dcbaa
                .add(slot_id as usize)
                .write_volatile(dev_ctx_phys);
        }

        // Allocate Input Context
        let (in_ctx_virt, in_ctx_phys) = dma_alloc(core::mem::size_of::<InputCtx>(), 64);
        unsafe {
            core::ptr::write_bytes(in_ctx_virt, 0, core::mem::size_of::<InputCtx>());
        }

        // Allocate EP0 Transfer Ring
        let ep0_ring = TransferRing::new();

        // Fill Input Control Context: A0=1, A1=1 (slot + EP0)
        unsafe {
            let in_ctx = &mut *(in_ctx_virt as *mut InputCtx);
            in_ctx.ctrl.drop_flags = 0;
            in_ctx.ctrl.add_flags = 0b11; // bit 0 = slot, bit 1 = EP0

            // Slot Context: context entries=1, root hub port number, speed
            in_ctx.dev_ctx.slot.dword[0] = (1 << 27) // context entries
                | ((speed as u32) << 20)
                | ((port as u32 + 1) << 16); // root hub port (1-indexed)

            // EP0 Context (endpoint 0 = index 1 in device context array)
            let max_pkt: u32 = match speed {
                3 => 64,      // High speed (USB 2.0)
                4 | 5 => 512, // SuperSpeed / SuperSpeedPlus (USB 3.x)
                _ => 8,       // Full / Low speed
            };
            in_ctx.dev_ctx.ep[0].dword[1] = (3 << 3)        // EP type: control
                | (max_pkt << 16) // MaxPacketSize
                | (3 << 1); // Error count
            // EP0 TR Dequeue Pointer (word 2 = low 32, word 3 = high 32) + DCS=1
            in_ctx.dev_ctx.ep[0].dword[2] = ep0_ring.phys as u32 | 1;
            in_ctx.dev_ctx.ep[0].dword[3] = (ep0_ring.phys >> 32) as u32;
        }

        // Address Device command (BSR=false)
        let cmd = address_device_cmd(in_ctx_phys, slot_id, false, true);
        let evt = match self.send_command(cmd) {
            Some(e) => e,
            None => return,
        };
        if evt.completion_code() != cc::SUCCESS {
            crate::serial_println!("[xhci] Address Device failed cc={}", evt.completion_code());
            return;
        }
        crate::serial_println!("[xhci] device addressed on slot {}", slot_id);

        // Temporary UsbDevice to hold EP0 ring for control transfers
        let mut dev = UsbDevice {
            slot_id,
            bulk_in_ep: 0,
            bulk_out_ep: 0,
            bulk_in_ring: TransferRing::new(),
            bulk_out_ring: TransferRing::new(),
            ctrl_ring: ep0_ring,
            block_count: 0,
            block_size: 512,
        };

        // Fetch device descriptor to get bMaxPacketSize0
        let (buf_virt, buf_phys) = dma_alloc(18, 64);
        if self
            .control_in(&mut dev, 0x80, 6, 0x0100, 0, 18, buf_phys)
            .is_some()
        {
            let raw_pkt = unsafe { (buf_virt as *const u8).add(7).read() } as u32;
            // For USB 3.x (SuperSpeed+), bMaxPacketSize0 is a base-2 exponent.
            let max_pkt = if speed >= 4 {
                1u32 << raw_pkt.min(10)
            } else {
                raw_pkt
            };
            crate::serial_println!("[xhci] bMaxPacketSize0={} (raw={})", max_pkt, raw_pkt);

            // Update EP0 MaxPacketSize in device context
            unsafe {
                let in_ctx = &mut *(in_ctx_virt as *mut InputCtx);
                in_ctx.ctrl.drop_flags = 0;
                in_ctx.ctrl.add_flags = 0b10; // only EP0
                let ep0 = &mut in_ctx.dev_ctx.ep[0];
                let existing = ep0.dword[1];
                ep0.dword[1] = (existing & !(0xFFFF << 16)) | (max_pkt << 16);
            }
            // Evaluate Context command (type 13)
            let mut ec = Trb::zero();
            ec.word[0] = in_ctx_phys as u32;
            ec.word[1] = (in_ctx_phys >> 32) as u32;
            ec.word[3] = (13u32 << 10) | ((slot_id as u32) << 24) | 1;
            let _ = self.send_command(ec);
        }

        // Get Configuration Descriptor: first read 9 bytes to get wTotalLength,
        // then fetch the full descriptor so multi-interface drives are not truncated.
        let (hdr_virt, hdr_phys) = dma_alloc(9, 64);
        if self
            .control_in(&mut dev, 0x80, 6, 0x0200, 0, 9, hdr_phys)
            .is_none()
        {
            crate::serial_println!("[xhci] GET_DESCRIPTOR(Config hdr) failed");
            return;
        }
        let w_total = unsafe {
            let h = hdr_virt as *const u8;
            u16::from_le_bytes([h.add(2).read(), h.add(3).read()]) as usize
        };
        let cfg_len = w_total.clamp(9, 512);
        let (cfg_virt, cfg_phys) = dma_alloc(cfg_len, 64);
        if self
            .control_in(&mut dev, 0x80, 6, 0x0200, 0, cfg_len as u16, cfg_phys)
            .is_none()
        {
            crate::serial_println!("[xhci] GET_DESCRIPTOR(Config) failed");
            return;
        }

        let cfg_slice = unsafe { core::slice::from_raw_parts(cfg_virt as *const u8, cfg_len) };

        // Parse interface + endpoint descriptors
        let mut is_msc = false;
        let mut bulk_in: Option<EndpointDesc> = None;
        let mut bulk_out: Option<EndpointDesc> = None;
        let mut cfg_value = 1u8;
        let mut i = 0usize;
        while i < cfg_slice.len() {
            let len = cfg_slice[i] as usize;
            if len < 2 {
                break;
            }
            let dtype = cfg_slice[i + 1];
            match dtype {
                0x02 => {
                    // Configuration
                    cfg_value = cfg_slice[i + 5];
                }
                0x04 => {
                    // Interface
                    let class = cfg_slice.get(i + 5).copied().unwrap_or(0);
                    let subclass = cfg_slice.get(i + 6).copied().unwrap_or(0);
                    let protocol = cfg_slice.get(i + 7).copied().unwrap_or(0);
                    is_msc = class == 0x08 && subclass == 0x06 && protocol == 0x50;
                }
                0x05 if is_msc && len >= 7 => {
                    // Endpoint
                    let ep = EndpointDesc {
                        addr: cfg_slice[i + 2],
                        attributes: cfg_slice[i + 3],
                        max_packet: u16::from_le_bytes([cfg_slice[i + 4], cfg_slice[i + 5]]),
                        interval: cfg_slice[i + 6],
                    };
                    if ep.is_bulk_in() {
                        bulk_in = Some(ep.clone());
                    }
                    if ep.is_bulk_out() {
                        bulk_out = Some(ep);
                    }
                }
                0x05 => {}
                _ => {}
            }
            i += len;
        }

        let (bulk_in, bulk_out) = match (bulk_in, bulk_out) {
            (Some(i), Some(o)) => (i, o),
            _ => {
                crate::serial_println!("[xhci] no USB MSC bulk endpoints found");
                return;
            }
        };
        crate::serial_println!(
            "[xhci] MSC EP IN=0x{:x} OUT=0x{:x}",
            bulk_in.addr,
            bulk_out.addr
        );

        // SET_CONFIGURATION
        self.control_out(&mut dev, 0x00, 9, cfg_value as u16, 0, 0, 0);

        // Allocate bulk transfer rings
        let bi_ring = TransferRing::new();
        let bo_ring = TransferRing::new();

        // Configure Endpoint command
        let (in_ctx2_virt, in_ctx2_phys) = dma_alloc(core::mem::size_of::<InputCtx>(), 64);
        unsafe {
            core::ptr::write_bytes(in_ctx2_virt, 0, core::mem::size_of::<InputCtx>());
            let in_ctx2 = &mut *(in_ctx2_virt as *mut InputCtx);
            in_ctx2.ctrl.add_flags =
                (1 << ep_dci(bulk_in.ep_num(), true)) | (1 << ep_dci(bulk_out.ep_num(), false));

            // Bulk IN endpoint context
            let in_dci = ep_dci(bulk_in.ep_num(), true) - 1;
            let ep_in = &mut in_ctx2.dev_ctx.ep[in_dci];
            ep_in.dword[1] = (6 << 3) | ((bulk_in.max_packet as u32) << 16); // type=bulk-in
            ep_in.dword[2] = bi_ring.phys as u32 | 1; // dequeue ptr low + DCS
            ep_in.dword[3] = (bi_ring.phys >> 32) as u32;
            ep_in.dword[4] = bulk_in.max_packet as u32; // Average TRB Length

            // Bulk OUT endpoint context
            let out_dci = ep_dci(bulk_out.ep_num(), false) - 1;
            let ep_out = &mut in_ctx2.dev_ctx.ep[out_dci];
            ep_out.dword[1] = (2 << 3) | ((bulk_out.max_packet as u32) << 16); // type=bulk-out
            ep_out.dword[2] = bo_ring.phys as u32 | 1;
            ep_out.dword[3] = (bo_ring.phys >> 32) as u32;
            ep_out.dword[4] = bulk_out.max_packet as u32;
        }

        let cmd = configure_ep_cmd(in_ctx2_phys, slot_id, true);
        let evt = match self.send_command(cmd) {
            Some(e) => e,
            None => return,
        };
        if evt.completion_code() != cc::SUCCESS {
            crate::serial_println!(
                "[xhci] Configure Endpoint failed cc={}",
                evt.completion_code()
            );
            return;
        }

        dev.bulk_in_ep = bulk_in.ep_num();
        dev.bulk_out_ep = bulk_out.ep_num();
        dev.bulk_in_ring = bi_ring;
        dev.bulk_out_ring = bo_ring;

        // SCSI INQUIRY — clears any UNIT ATTENTION condition that some drives
        // assert the first time they are accessed after enumeration.
        self.scsi_inquiry(&mut dev);

        // SCSI READ CAPACITY to get block count/size
        if let Some((count, bsize)) = self.read_capacity(&mut dev) {
            dev.block_count = count;
            dev.block_size = bsize;
            crate::serial_println!("[xhci] USB disk: {} blocks × {} bytes", count, bsize);
        }

        self.devices.push(dev);
    }

    // -----------------------------------------------------------------------
    // Control transfer helpers
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn control_in(
        &mut self,
        dev: &mut UsbDevice,
        bm: u8,
        breq: u8,
        wval: u16,
        widx: u16,
        wlen: u16,
        buf_phys: u64,
    ) -> Option<()> {
        dev.ctrl_ring
            .push(setup_stage_trb(bm, breq, wval, widx, wlen, 3, true));
        if wlen > 0 {
            dev.ctrl_ring
                .push(data_stage_trb(buf_phys, wlen as u32, true, true));
        }
        dev.ctrl_ring.push(status_stage_trb(false, true));

        let slot = dev.slot_id;
        self.ring_doorbell(slot, 1);
        // For control IN with data: the xHC generates a Transfer Event for the
        // Data Stage (IOC=1) and then a second one for the Status Stage (IOC=1).
        // Consume the Data Stage event first; if there was a data stage, consume
        // the Status Stage event too so it does not pollute future wait_event calls.
        let evt = self.wait_event(ty::TRANSFER_EVENT)?;
        if wlen > 0 {
            let _ = self.wait_event(ty::TRANSFER_EVENT);
        }
        if evt.completion_code() != cc::SUCCESS && evt.completion_code() != cc::SHORT_PACKET {
            return None;
        }
        Some(())
    }

    #[allow(clippy::too_many_arguments)]
    fn control_out(
        &mut self,
        dev: &mut UsbDevice,
        bm: u8,
        breq: u8,
        wval: u16,
        widx: u16,
        wlen: u16,
        _buf_phys: u64,
    ) -> Option<()> {
        dev.ctrl_ring
            .push(setup_stage_trb(bm, breq, wval, widx, wlen, 0, true));
        dev.ctrl_ring.push(status_stage_trb(true, true));

        let slot = dev.slot_id;
        self.ring_doorbell(slot, 1);
        let evt = self.wait_event(ty::TRANSFER_EVENT)?;
        if evt.completion_code() != cc::SUCCESS {
            return None;
        }
        Some(())
    }

    // -----------------------------------------------------------------------
    // Bulk transfer
    // -----------------------------------------------------------------------

    pub fn bulk_out(&mut self, dev_idx: usize, data: &[u8]) -> bool {
        let (buf_virt, buf_phys) = dma_alloc(data.len(), 64);
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), buf_virt, data.len());
        }
        let slot_id = self.devices[dev_idx].slot_id;
        let ep_dci_v = ep_dci(self.devices[dev_idx].bulk_out_ep, false) as u8;
        self.devices[dev_idx]
            .bulk_out_ring
            .push(normal_trb(buf_phys, data.len() as u32, true));
        self.ring_doorbell(slot_id, ep_dci_v);
        let evt = self.wait_event(ty::TRANSFER_EVENT);
        evt.map(|e| e.completion_code() == cc::SUCCESS)
            .unwrap_or(false)
    }

    pub fn bulk_in(&mut self, dev_idx: usize, len: usize) -> Option<alloc::vec::Vec<u8>> {
        let (buf_virt, buf_phys) = dma_alloc(len, 64);
        let slot_id = self.devices[dev_idx].slot_id;
        let ep_dci_v = ep_dci(self.devices[dev_idx].bulk_in_ep, true) as u8;
        self.devices[dev_idx]
            .bulk_in_ring
            .push(normal_trb(buf_phys, len as u32, true));
        self.ring_doorbell(slot_id, ep_dci_v);
        let evt = self.wait_event(ty::TRANSFER_EVENT)?;
        if evt.completion_code() != cc::SUCCESS && evt.completion_code() != cc::SHORT_PACKET {
            return None;
        }
        let data = unsafe { core::slice::from_raw_parts(buf_virt as *const u8, len).to_vec() };
        Some(data)
    }

    // -----------------------------------------------------------------------
    // SCSI via USB MSC BOT
    // -----------------------------------------------------------------------

    fn scsi_command(
        &mut self,
        dev_idx: usize,
        cdb: &[u8],
        data_in: Option<usize>,
    ) -> Option<alloc::vec::Vec<u8>> {
        let tag = SCSI_TAG.fetch_add(1, Ordering::Relaxed);

        let transfer_len = data_in.unwrap_or(0);

        // CBW
        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&0x43425355u32.to_le_bytes()); // dCBWSignature
        cbw[4..8].copy_from_slice(&tag.to_le_bytes()); // dCBWTag
        cbw[8..12].copy_from_slice(&(transfer_len as u32).to_le_bytes());
        cbw[12] = if data_in.is_some() { 0x80 } else { 0x00 }; // IN flag
        cbw[14] = cdb.len() as u8;
        cbw[15..15 + cdb.len()].copy_from_slice(cdb);

        if !self.bulk_out(dev_idx, &cbw) {
            return None;
        }

        // Data phase
        let result = if let Some(len) = data_in {
            self.bulk_in(dev_idx, len)
        } else {
            Some(alloc::vec::Vec::new())
        };

        // CSW
        let _ = self.bulk_in(dev_idx, 13);

        result
    }

    fn scsi_write_command(&mut self, dev_idx: usize, cdb: &[u8], data: &[u8]) -> Option<()> {
        let tag = SCSI_TAG.fetch_add(1, Ordering::Relaxed);

        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&0x43425355u32.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&(data.len() as u32).to_le_bytes());
        cbw[12] = 0x00; // OUT
        cbw[14] = cdb.len() as u8;
        cbw[15..15 + cdb.len()].copy_from_slice(cdb);

        if !self.bulk_out(dev_idx, &cbw) {
            return None;
        }
        if !data.is_empty() && !self.bulk_out(dev_idx, data) {
            return None;
        }

        let csw = self.bulk_in(dev_idx, 13)?;
        if csw.len() < 13 || &csw[0..4] != b"USBS" || csw[12] != 0 {
            return None;
        }
        Some(())
    }

    fn read_capacity(&mut self, dev: &mut UsbDevice) -> Option<(u64, u32)> {
        // We need to pass dev_idx; temporarily push, get index, do the call
        // Instead, inline the SCSI BOT directly
        let cdb = [0x25u8, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // READ CAPACITY(10)
        let (buf_virt, buf_phys) = dma_alloc(8, 64);

        let tag = 0xCAu32;
        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&0x43425355u32.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&8u32.to_le_bytes());
        cbw[12] = 0x80; // IN
        cbw[14] = cdb.len() as u8;
        cbw[15..15 + cdb.len()].copy_from_slice(&cdb);

        // Manual bulk_out / bulk_in using dev directly
        let (cbw_virt, cbw_phys) = dma_alloc(31, 64);
        unsafe {
            core::ptr::copy_nonoverlapping(cbw.as_ptr(), cbw_virt, 31);
        }
        let ep_out_dci = ep_dci(dev.bulk_out_ep, false) as u8;
        let ep_in_dci = ep_dci(dev.bulk_in_ep, true) as u8;

        dev.bulk_out_ring.push(normal_trb(cbw_phys, 31, true));
        self.ring_doorbell(dev.slot_id, ep_out_dci);
        let _ = self.wait_event(ty::TRANSFER_EVENT)?;

        dev.bulk_in_ring.push(normal_trb(buf_phys, 8, true));
        self.ring_doorbell(dev.slot_id, ep_in_dci);
        let _ = self.wait_event(ty::TRANSFER_EVENT)?;

        // CSW
        let (csw_virt, csw_phys) = dma_alloc(13, 64);
        dev.bulk_in_ring.push(normal_trb(csw_phys, 13, true));
        self.ring_doorbell(dev.slot_id, ep_in_dci);
        let _ = self.wait_event(ty::TRANSFER_EVENT)?;
        let _ = csw_virt; // suppress unused warning

        let data = unsafe { core::slice::from_raw_parts(buf_virt as *const u8, 8) };
        let last_lba = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let block_len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        Some((last_lba as u64 + 1, block_len))
    }

    /// Send a SCSI INQUIRY command directly to `dev`.
    ///
    /// This is called immediately after bulk endpoints are configured to clear
    /// any UNIT ATTENTION condition the drive may assert on first access.
    /// Failures are silently ignored — it is purely a warm-up command.
    fn scsi_inquiry(&mut self, dev: &mut UsbDevice) {
        const INQUIRY_LEN: u32 = 0x24;
        let cdb = [0x12u8, 0x00, 0x00, 0x00, INQUIRY_LEN as u8, 0x00];
        let tag = SCSI_TAG.fetch_add(1, Ordering::Relaxed);

        let mut cbw = [0u8; 31];
        cbw[0..4].copy_from_slice(&0x43425355u32.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&INQUIRY_LEN.to_le_bytes());
        cbw[12] = 0x80; // IN
        cbw[14] = cdb.len() as u8;
        cbw[15..15 + cdb.len()].copy_from_slice(&cdb);

        let (cbw_virt, cbw_phys) = dma_alloc(31, 64);
        unsafe { core::ptr::copy_nonoverlapping(cbw.as_ptr(), cbw_virt, 31) };
        let (buf_virt, buf_phys) = dma_alloc(INQUIRY_LEN as usize, 64);
        let (csw_virt, csw_phys) = dma_alloc(13, 64);
        let _ = (cbw_virt, buf_virt, csw_virt);

        let ep_out = ep_dci(dev.bulk_out_ep, false) as u8;
        let ep_in = ep_dci(dev.bulk_in_ep, true) as u8;

        dev.bulk_out_ring.push(normal_trb(cbw_phys, 31, true));
        self.ring_doorbell(dev.slot_id, ep_out);
        let _ = self.wait_event(ty::TRANSFER_EVENT);

        dev.bulk_in_ring
            .push(normal_trb(buf_phys, INQUIRY_LEN, true));
        self.ring_doorbell(dev.slot_id, ep_in);
        let _ = self.wait_event(ty::TRANSFER_EVENT);

        dev.bulk_in_ring.push(normal_trb(csw_phys, 13, true));
        self.ring_doorbell(dev.slot_id, ep_in);
        let _ = self.wait_event(ty::TRANSFER_EVENT);
    }

    // -----------------------------------------------------------------------
    // Public sector read/write (used by FAT32 driver)
    // -----------------------------------------------------------------------

    /// Read `count` 512-byte sectors starting at `lba` from USB device 0.
    pub fn read_sectors(&mut self, lba: u64, count: u16) -> Option<alloc::vec::Vec<u8>> {
        self.read_sectors_dev(0, lba, count)
    }

    /// Read `count` 512-byte sectors starting at `lba` from USB device `dev_idx`.
    pub fn read_sectors_dev(
        &mut self,
        dev_idx: usize,
        lba: u64,
        count: u16,
    ) -> Option<alloc::vec::Vec<u8>> {
        let len = count as usize * 512;
        let cdb = [
            0x28u8, // READ(10) opcode
            0,      // LUN=0
            (lba >> 24) as u8,
            (lba >> 16) as u8,
            (lba >> 8) as u8,
            lba as u8, // LBA (big-endian)
            0,         // reserved
            (count >> 8) as u8,
            count as u8, // transfer length
            0,           // control
        ];
        self.scsi_command(dev_idx, &cdb, Some(len))
    }

    pub fn write_sectors_dev(&mut self, dev_idx: usize, lba: u64, data: &[u8]) -> Option<()> {
        if !data.len().is_multiple_of(512) {
            return None;
        }
        let count = data.len() / 512;
        if count == 0 || count > u16::MAX as usize {
            return None;
        }
        let count = count as u16;
        let cdb = [
            0x2au8, // WRITE(10) opcode
            0,      // LUN=0
            (lba >> 24) as u8,
            (lba >> 16) as u8,
            (lba >> 8) as u8,
            lba as u8,
            0,
            (count >> 8) as u8,
            count as u8,
            0,
        ];
        self.scsi_write_command(dev_idx, &cdb, data)
    }

    /// Re-scan all ports and enumerate any newly connected devices.
    ///
    /// A port that has a device connected (CCS=1) but is not yet enabled (PED=0)
    /// indicates a hot-plugged device.  Returns the number of new devices added.
    pub fn scan_new_ports(&mut self) -> usize {
        let before = self.devices.len();
        for port in 0..self.max_ports {
            let portsc =
                unsafe { read32(self.op_base, 0x400 + (port as usize) * 0x10 + PORT_PORTSC) };
            // Connected but not yet enabled → newly plugged device
            if portsc & PORTSC_CCS != 0 && portsc & PORTSC_PED == 0 {
                crate::serial_println!("[xhci] hot-plug: port {} connected, enumerating", port);
                self.reset_port(port);
            }
        }
        let added = self.devices.len() - before;
        if added > 0 {
            crate::serial_println!("[xhci] scan_new_ports: {} new device(s)", added);
        }
        added
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Device Context Index for an endpoint.
/// DCI = 2 * ep_number + direction (1 = IN, 0 = OUT), endpoint 0 = DCI 1.
fn ep_dci(ep_num: u8, is_in: bool) -> usize {
    if ep_num == 0 {
        return 1;
    }
    2 * ep_num as usize + if is_in { 1 } else { 0 }
}
