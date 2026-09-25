//! virtio 1.x over PCI (modern interface) with split virtqueues.

use crate::mm::dma::DmaBuffer;
use crate::pci::PciDevice;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

pub const STATUS_ACK: u8 = 1;
pub const STATUS_DRIVER: u8 = 2;
pub const STATUS_DRIVER_OK: u8 = 4;
pub const STATUS_FEATURES_OK: u8 = 8;
pub const STATUS_FAILED: u8 = 128;

pub const F_VERSION_1: u64 = 1 << 32;

// Common configuration offsets.
const DEVICE_FEATURE_SELECT: u64 = 0x00;
const DEVICE_FEATURE: u64 = 0x04;
const DRIVER_FEATURE_SELECT: u64 = 0x08;
const DRIVER_FEATURE: u64 = 0x0C;
const MSIX_CONFIG: u64 = 0x10;
const DEVICE_STATUS: u64 = 0x14;
const QUEUE_SELECT: u64 = 0x16;
const QUEUE_SIZE: u64 = 0x18;
const QUEUE_MSIX_VECTOR: u64 = 0x1A;
const QUEUE_ENABLE: u64 = 0x1C;
const QUEUE_NOTIFY_OFF: u64 = 0x1E;
const QUEUE_DESC: u64 = 0x20;
const QUEUE_DRIVER: u64 = 0x28;
const QUEUE_DEVICE: u64 = 0x30;

pub const DESC_F_NEXT: u16 = 1;
pub const DESC_F_WRITE: u16 = 2;

pub struct VirtioPci {
    common: u64,
    notify: u64,
    notify_mul: u32,
    isr: u64,
    pub device: u64,
    pub pci: PciDevice,
}

fn r8(a: u64) -> u8 {
    unsafe { core::ptr::read_volatile(a as *const u8) }
}
fn r16(a: u64) -> u16 {
    unsafe { core::ptr::read_volatile(a as *const u16) }
}
fn r32(a: u64) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}
fn w8(a: u64, v: u8) {
    unsafe { core::ptr::write_volatile(a as *mut u8, v) }
}
fn w16(a: u64, v: u16) {
    unsafe { core::ptr::write_volatile(a as *mut u16, v) }
}
fn w32(a: u64, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}
fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
}

impl VirtioPci {
    /// Locate the virtio capabilities and map their BARs.
    pub fn new(pci: &PciDevice) -> Option<VirtioPci> {
        pci.enable();
        let mut common = None;
        let mut notify = None;
        let mut isr = None;
        let mut device = None;
        let mut mapped: Vec<(usize, u64)> = Vec::new();
        let mut bar_virt = |bar: usize| -> Option<u64> {
            if let Some((_, v)) = mapped.iter().find(|(b, _)| *b == bar) {
                return Some(*v);
            }
            let v = pci.map_bar(bar)?;
            mapped.push((bar, v));
            Some(v)
        };
        for (id, off) in pci.capabilities() {
            if id != crate::pci::CAP_VENDOR {
                continue;
            }
            let cfg_type = pci.read8(off + 3);
            let bar = pci.read8(off + 4) as usize;
            let offset = pci.read32(off + 8) as u64;
            let Some(base) = bar_virt(bar) else { continue };
            let addr = base + offset;
            match cfg_type {
                1 => common = Some(addr),
                2 => notify = Some((addr, pci.read32(off + 16))),
                3 => isr = Some(addr),
                4 => device = Some(addr),
                _ => {}
            }
        }
        let (notify, notify_mul) = notify?;
        Some(VirtioPci {
            common: common?,
            notify,
            notify_mul,
            isr: isr?,
            device: device.unwrap_or(0),
            pci: pci.clone(),
        })
    }

    pub fn status(&self) -> u8 {
        r8(self.common + DEVICE_STATUS)
    }

    pub fn set_status(&self, s: u8) {
        w8(self.common + DEVICE_STATUS, s);
    }

    pub fn reset(&self) {
        self.set_status(0);
        crate::time::wait_until(100, || self.status() == 0);
    }

    pub fn device_features(&self) -> u64 {
        w32(self.common + DEVICE_FEATURE_SELECT, 0);
        let lo = r32(self.common + DEVICE_FEATURE) as u64;
        w32(self.common + DEVICE_FEATURE_SELECT, 1);
        let hi = r32(self.common + DEVICE_FEATURE) as u64;
        lo | (hi << 32)
    }

    /// Standard init up to FEATURES_OK. Returns the negotiated features.
    pub fn negotiate(&self, wanted: u64) -> Option<u64> {
        self.reset();
        self.set_status(STATUS_ACK);
        self.set_status(STATUS_ACK | STATUS_DRIVER);
        let offered = self.device_features();
        if offered & F_VERSION_1 == 0 {
            return None;
        }
        let f = offered & (wanted | F_VERSION_1);
        w32(self.common + DRIVER_FEATURE_SELECT, 0);
        w32(self.common + DRIVER_FEATURE, f as u32);
        w32(self.common + DRIVER_FEATURE_SELECT, 1);
        w32(self.common + DRIVER_FEATURE, (f >> 32) as u32);
        self.set_status(STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK);
        if self.status() & STATUS_FEATURES_OK == 0 {
            self.set_status(STATUS_FAILED);
            return None;
        }
        Some(f)
    }

    pub fn driver_ok(&self) {
        self.set_status(self.status() | STATUS_DRIVER_OK);
    }

    /// Read (and acknowledge) the ISR status.
    pub fn isr(&self) -> u8 {
        r8(self.isr)
    }

    pub fn cfg_r8(&self, off: u64) -> u8 {
        r8(self.device + off)
    }
    pub fn cfg_r16(&self, off: u64) -> u16 {
        r16(self.device + off)
    }
    pub fn cfg_r32(&self, off: u64) -> u32 {
        r32(self.device + off)
    }
    pub fn cfg_r64(&self, off: u64) -> u64 {
        self.cfg_r32(off) as u64 | ((self.cfg_r32(off + 4) as u64) << 32)
    }

    pub fn set_config_msix(&self, v: u16) {
        w16(self.common + MSIX_CONFIG, v);
    }

    /// Set up virtqueue `idx` with at most `max` entries.
    pub fn setup_queue(&self, idx: u16, max: u16, msix: Option<u16>) -> Option<Virtqueue> {
        w16(self.common + QUEUE_SELECT, idx);
        let dev_max = r16(self.common + QUEUE_SIZE);
        if dev_max == 0 {
            return None;
        }
        let size = dev_max.min(max).next_power_of_two().min(dev_max);
        w16(self.common + QUEUE_SIZE, size);
        let q = Virtqueue::new(size)?;
        w64(self.common + QUEUE_DESC, q.mem.phys());
        w64(
            self.common + QUEUE_DRIVER,
            q.mem.phys() + q.avail_off as u64,
        );
        w64(self.common + QUEUE_DEVICE, q.mem.phys() + q.used_off as u64);
        if let Some(v) = msix {
            w16(self.common + QUEUE_MSIX_VECTOR, v);
        }
        let notify_off = r16(self.common + QUEUE_NOTIFY_OFF) as u64;
        w16(self.common + QUEUE_ENABLE, 1);
        let mut q = q;
        q.notify_addr = self.notify + notify_off * self.notify_mul as u64;
        q.index = idx;
        Some(q)
    }
}

/// A split virtqueue in one DMA allocation.
pub struct Virtqueue {
    mem: DmaBuffer,
    pub size: u16,
    avail_off: usize,
    used_off: usize,
    free: Vec<u16>,
    avail_idx: u16,
    last_used: u16,
    notify_addr: u64,
    pub index: u16,
}

unsafe impl Send for Virtqueue {}

impl Virtqueue {
    fn new(size: u16) -> Option<Virtqueue> {
        let n = size as usize;
        let desc = 16 * n;
        let avail = 6 + 2 * n;
        let used_off = (desc + avail).next_multiple_of(4096);
        let used = 6 + 8 * n;
        let mem = DmaBuffer::new(used_off + used)?;
        Some(Virtqueue {
            mem,
            size,
            avail_off: desc,
            used_off,
            free: (0..size).rev().collect(),
            avail_idx: 0,
            last_used: 0,
            notify_addr: 0,
            index: 0,
        })
    }

    pub fn free_count(&self) -> usize {
        self.free.len()
    }

    /// Post a chain of buffers: (phys, len, device-writable). Returns the
    /// head descriptor id.
    pub fn submit(&mut self, bufs: &[(u64, u32, bool)]) -> Option<u16> {
        if bufs.len() > self.free.len() || bufs.is_empty() {
            return None;
        }
        let ids: Vec<u16> = (0..bufs.len()).map(|_| self.free.pop().unwrap()).collect();
        for (i, (phys, len, writable)) in bufs.iter().enumerate() {
            let d = ids[i] as usize * 16;
            let mut flags = if *writable { DESC_F_WRITE } else { 0 };
            let next = if i + 1 < bufs.len() {
                flags |= DESC_F_NEXT;
                ids[i + 1]
            } else {
                0
            };
            self.mem.write::<u64>(d, *phys);
            self.mem.write::<u32>(d + 8, *len);
            self.mem.write::<u16>(d + 12, flags);
            self.mem.write::<u16>(d + 14, next);
        }
        let slot = self.avail_off + 4 + (self.avail_idx % self.size) as usize * 2;
        self.mem.write::<u16>(slot, ids[0]);
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.mem.write::<u16>(self.avail_off + 2, self.avail_idx);
        fence(Ordering::SeqCst);
        Some(ids[0])
    }

    pub fn notify(&self) {
        w16(self.notify_addr, self.index);
    }

    /// Take one completed chain: (head id, bytes written by the device).
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        fence(Ordering::SeqCst);
        let used_idx = self.mem.read::<u16>(self.used_off + 2);
        if used_idx == self.last_used {
            return None;
        }
        let e = self.used_off + 4 + (self.last_used % self.size) as usize * 8;
        let id = self.mem.read::<u32>(e) as u16;
        let len = self.mem.read::<u32>(e + 4);
        self.last_used = self.last_used.wrapping_add(1);
        // Return the chain's descriptors to the free list.
        let mut d = id;
        loop {
            self.free.push(d);
            let flags = self.mem.read::<u16>(d as usize * 16 + 12);
            if flags & DESC_F_NEXT == 0 {
                break;
            }
            d = self.mem.read::<u16>(d as usize * 16 + 14);
        }
        Some((id, len))
    }

    pub fn has_used(&self) -> bool {
        fence(Ordering::SeqCst);
        self.mem.read::<u16>(self.used_off + 2) != self.last_used
    }
}
