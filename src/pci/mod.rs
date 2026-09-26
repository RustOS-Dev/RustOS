//! PCI / PCI Express.
//!
//! * Configuration access through ECAM (from the ACPI MCFG table, giving the
//!   full 4 KiB extended space) with a CF8/CFC port-I/O fallback.
//! * Recursive enumeration from the host bridges through PCI-to-PCI bridges.
//! * BAR decoding and sizing, command-register helpers, capability and
//!   extended-capability walks, MSI and MSI-X programming, power-state and
//!   function-level-reset helpers.

pub mod ids;

use crate::arch::x86_64::{acpi, apic, idt};
use alloc::vec::Vec;
use spin::{Mutex, Once};
use x86_64::instructions::port::Port;

// ---------------------------------------------------------------------------
// Config-space access
// ---------------------------------------------------------------------------

struct EcamMap {
    segment: u16,
    bus_start: u8,
    bus_end: u8,
    /// Virtual base of the window (bus_start maps here).
    virt: u64,
}

static ECAM: Once<Vec<EcamMap>> = Once::new();
static LEGACY_LOCK: Mutex<()> = Mutex::new(());

/// Map the ECAM windows described by MCFG. Safe to call more than once.
pub fn init_ecam() {
    ECAM.call_once(|| {
        acpi::platform()
            .ecam
            .iter()
            .map(|r| {
                let buses = r.bus_end as u64 - r.bus_start as u64 + 1;
                let virt = crate::mm::map_mmio(
                    r.base + ((r.bus_start as u64) << 20),
                    (buses << 20) as usize,
                );
                EcamMap {
                    segment: r.segment,
                    bus_start: r.bus_start,
                    bus_end: r.bus_end,
                    virt,
                }
            })
            .collect()
    });
}

fn ecam_addr(seg: u16, bus: u8, dev: u8, func: u8, off: u16) -> Option<u64> {
    let maps = ECAM.get()?;
    let m = maps
        .iter()
        .find(|m| m.segment == seg && bus >= m.bus_start && bus <= m.bus_end)?;
    Some(
        m.virt
            + (((bus - m.bus_start) as u64) << 20)
            + ((dev as u64) << 15)
            + ((func as u64) << 12)
            + (off as u64 & 0xFFC),
    )
}

pub fn config_read32(seg: u16, bus: u8, dev: u8, func: u8, off: u16) -> u32 {
    if let Some(a) = ecam_addr(seg, bus, dev, func, off) {
        return unsafe { core::ptr::read_volatile(a as *const u32) };
    }
    if seg != 0 || off >= 256 {
        return 0xFFFF_FFFF;
    }
    let addr = 0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | (off as u32 & 0xFC);
    x86_64::instructions::interrupts::without_interrupts(|| {
        let _g = LEGACY_LOCK.lock();
        unsafe {
            Port::<u32>::new(0xCF8).write(addr);
            Port::<u32>::new(0xCFC).read()
        }
    })
}

pub fn config_write32(seg: u16, bus: u8, dev: u8, func: u8, off: u16, val: u32) {
    if let Some(a) = ecam_addr(seg, bus, dev, func, off) {
        unsafe { core::ptr::write_volatile(a as *mut u32, val) };
        return;
    }
    if seg != 0 || off >= 256 {
        return;
    }
    let addr = 0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | (off as u32 & 0xFC);
    x86_64::instructions::interrupts::without_interrupts(|| {
        let _g = LEGACY_LOCK.lock();
        unsafe {
            Port::<u32>::new(0xCF8).write(addr);
            Port::<u32>::new(0xCFC).write(val);
        }
    })
}

// ---------------------------------------------------------------------------
// Device description
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    None,
    Mmio {
        addr: u64,
        size: u64,
        prefetchable: bool,
        is64: bool,
    },
    Io {
        port: u16,
        size: u32,
    },
}

impl Bar {
    pub fn mmio_addr(&self) -> Option<u64> {
        match *self {
            Bar::Mmio { addr, .. } if addr != 0 => Some(addr),
            _ => None,
        }
    }
    pub fn size(&self) -> u64 {
        match *self {
            Bar::Mmio { size, .. } => size,
            Bar::Io { size, .. } => size as u64,
            Bar::None => 0,
        }
    }
    pub fn io_port(&self) -> Option<u16> {
        match *self {
            Bar::Io { port, .. } => Some(port),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PciDevice {
    pub segment: u16,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    pub header_type: u8,
    pub subsys_vendor: u16,
    pub subsys_id: u16,
    pub irq_line: u8,
    pub irq_pin: u8,
    pub bars: [Bar; 6],
}

pub const CMD_IO: u16 = 1 << 0;
pub const CMD_MEMORY: u16 = 1 << 1;
pub const CMD_BUS_MASTER: u16 = 1 << 2;
pub const CMD_INTX_DISABLE: u16 = 1 << 10;

pub const CAP_PM: u8 = 0x01;
pub const CAP_MSI: u8 = 0x05;
pub const CAP_VENDOR: u8 = 0x09;
pub const CAP_PCIE: u8 = 0x10;
pub const CAP_MSIX: u8 = 0x11;

pub const EXT_CAP_AER: u16 = 0x0001;
pub const EXT_CAP_LTR: u16 = 0x0018;
pub const EXT_CAP_L1SS: u16 = 0x001E;

impl PciDevice {
    pub fn read32(&self, off: u16) -> u32 {
        config_read32(self.segment, self.bus, self.dev, self.func, off)
    }
    pub fn write32(&self, off: u16, v: u32) {
        config_write32(self.segment, self.bus, self.dev, self.func, off, v)
    }
    pub fn read16(&self, off: u16) -> u16 {
        (self.read32(off & !3) >> ((off & 2) * 8)) as u16
    }
    pub fn read8(&self, off: u16) -> u8 {
        (self.read32(off & !3) >> ((off & 3) * 8)) as u8
    }
    pub fn write16(&self, off: u16, v: u16) {
        let shift = (off & 2) * 8;
        let old = self.read32(off & !3);
        self.write32(off & !3, (old & !(0xFFFF << shift)) | ((v as u32) << shift));
    }
    pub fn write8(&self, off: u16, v: u8) {
        let shift = (off & 3) * 8;
        let old = self.read32(off & !3);
        self.write32(off & !3, (old & !(0xFF << shift)) | ((v as u32) << shift));
    }

    pub fn command(&self) -> u16 {
        self.read16(0x04)
    }
    pub fn set_command(&self, v: u16) {
        // Write only the command half; writing 1s to status bits clears them.
        self.write32(0x04, v as u32);
    }

    /// Enable memory decoding, I/O decoding (if the device has I/O BARs) and
    /// bus mastering.
    pub fn enable(&self) {
        let mut cmd = self.command() | CMD_MEMORY | CMD_BUS_MASTER;
        if self.bars.iter().any(|b| matches!(b, Bar::Io { .. })) {
            cmd |= CMD_IO;
        }
        self.set_command(cmd);
    }

    pub fn enable_bus_master(&self) {
        self.set_command(self.command() | CMD_BUS_MASTER | CMD_MEMORY);
    }

    /// Physical MMIO base of BAR `idx` (0 when not an MMIO BAR).
    pub fn mmio_base(&self, idx: usize) -> u64 {
        self.bars.get(idx).and_then(|b| b.mmio_addr()).unwrap_or(0)
    }

    /// Map BAR `idx` uncached and return its virtual address.
    pub fn map_bar(&self, idx: usize) -> Option<u64> {
        match self.bars.get(idx)? {
            Bar::Mmio { addr, size, .. } if *addr != 0 => {
                Some(crate::mm::map_mmio(*addr, (*size).max(4096) as usize))
            }
            _ => None,
        }
    }

    /// Walk the classic capability list: yields (id, offset).
    pub fn capabilities(&self) -> Vec<(u8, u16)> {
        let mut out = Vec::new();
        if self.read16(0x06) & (1 << 4) == 0 {
            return out;
        }
        let mut ptr = (self.read8(0x34) & 0xFC) as u16;
        let mut guard = 0;
        while ptr != 0 && guard < 48 {
            let id = self.read8(ptr);
            out.push((id, ptr));
            ptr = (self.read8(ptr + 1) & 0xFC) as u16;
            guard += 1;
        }
        out
    }

    pub fn find_capability(&self, id: u8) -> Option<u16> {
        self.capabilities()
            .into_iter()
            .find(|&(c, _)| c == id)
            .map(|(_, o)| o)
    }

    /// Walk the PCIe extended capability list: yields (id, version, offset).
    pub fn ext_capabilities(&self) -> Vec<(u16, u8, u16)> {
        let mut out = Vec::new();
        if self.find_capability(CAP_PCIE).is_none() {
            return out;
        }
        let mut off = 0x100u16;
        let mut guard = 0;
        while off != 0 && guard < 64 {
            let hdr = self.read32(off);
            if hdr == 0 || hdr == 0xFFFF_FFFF {
                break;
            }
            out.push(((hdr & 0xFFFF) as u16, ((hdr >> 16) & 0xF) as u8, off));
            off = ((hdr >> 20) & 0xFFC) as u16;
            guard += 1;
        }
        out
    }

    pub fn find_ext_capability(&self, id: u16) -> Option<u16> {
        self.ext_capabilities()
            .into_iter()
            .find(|&(c, _, _)| c == id)
            .map(|(_, _, o)| o)
    }

    /// Put the function into D0 (full power) if it supports power management.
    pub fn set_power_d0(&self) {
        if let Some(pm) = self.find_capability(CAP_PM) {
            let csr = self.read16(pm + 4);
            if csr & 3 != 0 {
                self.write16(pm + 4, csr & !3);
                crate::time::delay_us(10_000);
            }
        }
    }

    /// Function-level reset via the PCIe capability, if supported.
    pub fn function_reset(&self) -> bool {
        let Some(pcie) = self.find_capability(CAP_PCIE) else {
            return false;
        };
        let devcap = self.read32(pcie + 4);
        if devcap & (1 << 28) == 0 {
            return false;
        }
        let ctl = self.read16(pcie + 8);
        self.write16(pcie + 8, ctl | (1 << 15));
        crate::time::delay_us(100_000);
        true
    }

    /// Disable ASPM L0s/L1 on the link (some devices, e.g. Intel WiFi, need
    /// L0s off for reliable DMA).
    pub fn disable_aspm(&self, l0s: bool, l1: bool) {
        if let Some(pcie) = self.find_capability(CAP_PCIE) {
            let mut lnkctl = self.read16(pcie + 0x10);
            if l0s {
                lnkctl &= !1;
            }
            if l1 {
                lnkctl &= !2;
            }
            self.write16(pcie + 0x10, lnkctl);
        }
    }

    pub fn name(&self) -> alloc::string::String {
        alloc::format!(
            "{:04x}:{:02x}:{:02x}.{}",
            self.segment,
            self.bus,
            self.dev,
            self.func
        )
    }

    // -----------------------------------------------------------------------
    // Interrupts
    // -----------------------------------------------------------------------

    fn msi_address(dest_apic: u32) -> u64 {
        0xFEE0_0000 | ((dest_apic as u64 & 0xFF) << 12)
    }

    /// Number of MSI-X table entries, if MSI-X is supported.
    pub fn msix_count(&self) -> Option<u16> {
        let cap = self.find_capability(CAP_MSIX)?;
        Some((self.read16(cap + 2) & 0x7FF) + 1)
    }

    /// Enable MSI-X with one handler per entry. Returns the vectors in entry
    /// order. INTx and MSI are disabled.
    pub fn enable_msix(
        &self,
        handlers: Vec<alloc::boxed::Box<dyn Fn() + Send + Sync>>,
    ) -> Option<Vec<u8>> {
        let cap = self.find_capability(CAP_MSIX)?;
        let count = (self.read16(cap + 2) & 0x7FF) as usize + 1;
        if handlers.is_empty() || handlers.len() > count {
            return None;
        }
        let table = self.read32(cap + 4);
        let bir = (table & 7) as usize;
        let table_off = (table & !7) as u64;
        let bar = self.mmio_base(bir);
        if bar == 0 {
            return None;
        }
        let virt = crate::mm::map_mmio(bar + table_off, count * 16);
        // Mask the function while programming.
        let ctl = self.read16(cap + 2);
        self.write16(cap + 2, ctl | (1 << 15) | (1 << 14));
        self.disable_msi();
        let dest = apic::id();
        let mut vectors = Vec::new();
        for (i, h) in handlers.into_iter().enumerate() {
            let v = idt::alloc_vector(move |_f| {
                h();
                apic::eoi();
            })?;
            let e = virt + i as u64 * 16;
            unsafe {
                let addr = Self::msi_address(dest);
                core::ptr::write_volatile(e as *mut u32, addr as u32);
                core::ptr::write_volatile((e + 4) as *mut u32, (addr >> 32) as u32);
                core::ptr::write_volatile((e + 8) as *mut u32, v as u32);
                core::ptr::write_volatile((e + 12) as *mut u32, 0); // unmask
            }
            vectors.push(v);
        }
        self.set_command(self.command() | CMD_INTX_DISABLE);
        let ctl = self.read16(cap + 2);
        self.write16(cap + 2, (ctl | (1 << 15)) & !(1 << 14));
        Some(vectors)
    }

    pub fn disable_msi(&self) {
        if let Some(cap) = self.find_capability(CAP_MSI) {
            let ctl = self.read16(cap + 2);
            self.write16(cap + 2, ctl & !1);
        }
    }

    /// Enable single-message MSI with `handler`. Returns the vector.
    pub fn enable_msi(&self, handler: alloc::boxed::Box<dyn Fn() + Send + Sync>) -> Option<u8> {
        let cap = self.find_capability(CAP_MSI)?;
        let v = idt::alloc_vector(move |_f| {
            handler();
            apic::eoi();
        })?;
        let ctl = self.read16(cap + 2);
        let is64 = ctl & (1 << 7) != 0;
        let addr = Self::msi_address(apic::id());
        self.write32(cap + 4, addr as u32);
        if is64 {
            self.write32(cap + 8, (addr >> 32) as u32);
            self.write16(cap + 12, v as u16);
        } else {
            self.write16(cap + 8, v as u16);
        }
        // One message (MME = 0), enable.
        self.write16(cap + 2, (ctl & !(7 << 4)) | 1);
        self.set_command(self.command() | CMD_INTX_DISABLE);
        Some(v)
    }

    /// Route the legacy INTx pin through the I/O APIC to `handler`.
    pub fn enable_intx(&self, handler: alloc::boxed::Box<dyn Fn() + Send + Sync>) -> Option<u8> {
        if self.irq_pin == 0 {
            return None;
        }
        let gsi = acpi::pci_irq_route(self.dev, self.func, self.irq_pin).or_else(|| {
            (self.irq_line != 0xFF && self.irq_line != 0).then_some(self.irq_line as u32)
        })?;
        let v = idt::alloc_vector(move |_f| {
            handler();
            apic::eoi();
        })?;
        // PCI INTx is level-triggered, active-low.
        apic::route_gsi(gsi, v, apic::id(), true, true);
        self.set_command(self.command() & !CMD_INTX_DISABLE);
        Some(v)
    }

    /// MSI, falling back to INTx (for devices whose MSI-X mode needs
    /// extra vector routing setup).
    pub fn enable_msi_or_intx(
        &self,
        handler: alloc::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> Option<u8> {
        let h = handler.clone();
        self.enable_msi(alloc::boxed::Box::new(move || h()))
            .or_else(|| self.enable_intx(alloc::boxed::Box::new(move || handler())))
    }

    /// Best available interrupt: MSI-X (1 vector), then MSI, then INTx.
    pub fn enable_irq(&self, handler: alloc::sync::Arc<dyn Fn() + Send + Sync>) -> Option<u8> {
        let h1 = handler.clone();
        if self.msix_count().is_some()
            && let Some(v) = self.enable_msix(alloc::vec![alloc::boxed::Box::new(move || h1())])
        {
            return v.first().copied();
        }
        let h2 = handler.clone();
        if let Some(v) = self.enable_msi(alloc::boxed::Box::new(move || h2())) {
            return Some(v);
        }
        self.enable_intx(alloc::boxed::Box::new(move || handler()))
    }
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

static DEVICES: Mutex<Vec<PciDevice>> = Mutex::new(Vec::new());
static SCANNED: Once<()> = Once::new();

fn probe_function(seg: u16, bus: u8, dev: u8, func: u8) -> Option<PciDevice> {
    let id = config_read32(seg, bus, dev, func, 0);
    if id & 0xFFFF == 0xFFFF {
        return None;
    }
    let class_reg = config_read32(seg, bus, dev, func, 0x08);
    let hdr = (config_read32(seg, bus, dev, func, 0x0C) >> 16) as u8;
    let irq = config_read32(seg, bus, dev, func, 0x3C);
    let mut d = PciDevice {
        segment: seg,
        bus,
        dev,
        func,
        vendor_id: id as u16,
        device_id: (id >> 16) as u16,
        class: (class_reg >> 24) as u8,
        subclass: (class_reg >> 16) as u8,
        prog_if: (class_reg >> 8) as u8,
        revision: class_reg as u8,
        header_type: hdr,
        subsys_vendor: 0,
        subsys_id: 0,
        irq_line: irq as u8,
        irq_pin: (irq >> 8) as u8,
        bars: [Bar::None; 6],
    };
    let nbars = match hdr & 0x7F {
        0 => {
            let ss = config_read32(seg, bus, dev, func, 0x2C);
            d.subsys_vendor = ss as u16;
            d.subsys_id = (ss >> 16) as u16;
            6
        }
        1 => 2,
        _ => 0,
    };
    size_bars(&mut d, nbars);
    Some(d)
}

fn size_bars(d: &mut PciDevice, nbars: usize) {
    let cmd = d.command();
    // Disable decoding while sizing so the probe values never hit the bus.
    d.set_command(cmd & !(CMD_IO | CMD_MEMORY));
    let mut i = 0;
    while i < nbars {
        let off = 0x10 + i as u16 * 4;
        let orig = d.read32(off);
        if orig & 1 == 1 {
            d.write32(off, 0xFFFF_FFFF);
            let mask = d.read32(off) & 0xFFFF_FFFC;
            d.write32(off, orig);
            let size = (!mask).wrapping_add(1) & 0xFFFF;
            if mask != 0 {
                d.bars[i] = Bar::Io {
                    port: (orig & 0xFFFC) as u16,
                    size,
                };
            }
            i += 1;
            continue;
        }
        let is64 = (orig >> 1) & 3 == 2;
        let prefetchable = orig & 8 != 0;
        d.write32(off, 0xFFFF_FFFF);
        let lo_mask = d.read32(off) & 0xFFFF_FFF0;
        d.write32(off, orig);
        let (addr, size) = if is64 && i + 1 < nbars {
            let orig_hi = d.read32(off + 4);
            d.write32(off + 4, 0xFFFF_FFFF);
            let hi_mask = d.read32(off + 4);
            d.write32(off + 4, orig_hi);
            let mask = ((hi_mask as u64) << 32) | lo_mask as u64;
            (
                ((orig_hi as u64) << 32) | (orig & 0xFFFF_FFF0) as u64,
                (!mask).wrapping_add(1),
            )
        } else {
            (
                (orig & 0xFFFF_FFF0) as u64,
                ((!lo_mask).wrapping_add(1)) as u64,
            )
        };
        if lo_mask != 0 {
            d.bars[i] = Bar::Mmio {
                addr,
                size,
                prefetchable,
                is64,
            };
        }
        i += if is64 { 2 } else { 1 };
    }
    d.set_command(cmd);
}

fn scan_bus(seg: u16, bus: u8, out: &mut Vec<PciDevice>, depth: u32) {
    if depth > 32 {
        return;
    }
    for dev in 0..32u8 {
        let Some(f0) = probe_function(seg, bus, dev, 0) else {
            continue;
        };
        let multi = f0.header_type & 0x80 != 0;
        let mut funcs = alloc::vec![f0];
        if multi {
            for func in 1..8u8 {
                if let Some(f) = probe_function(seg, bus, dev, func) {
                    funcs.push(f);
                }
            }
        }
        for f in funcs {
            let is_bridge = f.header_type & 0x7F == 1 && f.class == 0x06 && f.subclass == 0x04;
            let secondary = if is_bridge {
                Some((f.read32(0x18) >> 8) as u8)
            } else {
                None
            };
            out.push(f);
            if let Some(sec) = secondary
                && sec > bus
            {
                scan_bus(seg, sec, out, depth + 1);
            }
        }
    }
}

/// Scan every segment/host bridge. Cached after the first call; use
/// [`rescan`] to force another scan.
pub fn enumerate() -> Vec<PciDevice> {
    SCANNED.call_once(|| {
        *DEVICES.lock() = scan_all();
    });
    DEVICES.lock().clone()
}

pub fn rescan() -> Vec<PciDevice> {
    let v = scan_all();
    *DEVICES.lock() = v.clone();
    v
}

fn scan_all() -> Vec<PciDevice> {
    let mut out = Vec::new();
    let segments: Vec<(u16, u8)> = match ECAM.get() {
        Some(m) if !m.is_empty() => m.iter().map(|m| (m.segment, m.bus_start)).collect(),
        _ => alloc::vec![(0, 0)],
    };
    for (seg, start_bus) in segments {
        // Host bridge 00:00.0 may be multi-function: each function is the
        // root of another bus.
        match probe_function(seg, start_bus, 0, 0) {
            Some(h) if h.header_type & 0x80 != 0 => {
                for func in 0..8u8 {
                    if probe_function(seg, start_bus, 0, func).is_some() {
                        scan_bus(seg, start_bus + func, &mut out, 0);
                    }
                }
            }
            _ => scan_bus(seg, start_bus, &mut out, 0),
        }
    }
    out.sort_by_key(|d| (d.segment, d.bus, d.dev, d.func));
    out.dedup_by_key(|d| (d.segment, d.bus, d.dev, d.func));
    out
}

pub fn find_by_class(class: u8, subclass: u8, prog_if: Option<u8>) -> Vec<PciDevice> {
    enumerate()
        .into_iter()
        .filter(|d| {
            d.class == class && d.subclass == subclass && prog_if.is_none_or(|p| d.prog_if == p)
        })
        .collect()
}

pub fn find_by_id(vendor: u16, devices: &[u16]) -> Vec<PciDevice> {
    enumerate()
        .into_iter()
        .filter(|d| d.vendor_id == vendor && devices.contains(&d.device_id))
        .collect()
}

/// First xHCI controller (class 0C/03/30).
pub fn find_xhci(devices: &[PciDevice]) -> Option<&PciDevice> {
    devices
        .iter()
        .find(|d| d.class == 0x0C && d.subclass == 0x03 && d.prog_if == 0x30)
}

/// Human-readable class description.
pub fn class_name(class: u8, subclass: u8, prog_if: u8) -> &'static str {
    match (class, subclass, prog_if) {
        (0x01, 0x01, _) => "IDE controller",
        (0x01, 0x06, 0x01) => "SATA controller (AHCI)",
        (0x01, 0x06, _) => "SATA controller",
        (0x01, 0x08, 0x02) => "NVMe controller",
        (0x01, 0x00, _) => "SCSI controller",
        (0x01, _, _) => "Mass storage controller",
        (0x02, 0x00, _) => "Ethernet controller",
        (0x02, 0x80, _) => "Network controller (WiFi)",
        (0x02, _, _) => "Network controller",
        (0x03, 0x00, _) => "VGA compatible controller",
        (0x03, _, _) => "Display controller",
        (0x04, 0x03, _) => "Audio device",
        (0x04, _, _) => "Multimedia controller",
        (0x05, _, _) => "Memory controller",
        (0x06, 0x00, _) => "Host bridge",
        (0x06, 0x01, _) => "ISA bridge",
        (0x06, 0x04, _) => "PCI bridge",
        (0x06, _, _) => "Bridge",
        (0x07, _, _) => "Communication controller",
        (0x08, _, _) => "System peripheral",
        (0x0C, 0x03, 0x00) => "USB controller (UHCI)",
        (0x0C, 0x03, 0x10) => "USB controller (OHCI)",
        (0x0C, 0x03, 0x20) => "USB controller (EHCI)",
        (0x0C, 0x03, 0x30) => "USB controller (xHCI)",
        (0x0C, 0x03, _) => "USB controller",
        (0x0C, 0x05, _) => "SMBus",
        (0x0C, _, _) => "Serial bus controller",
        (0x0D, _, _) => "Wireless controller",
        _ => "Unknown device",
    }
}
