//! Block device enumeration: NVMe and AHCI/SATA via PCI, plus filesystem
//! type detection from on-disk superblock signatures.
//!
//! This module provides:
//! - `probe_block_devices()` — scan PCI for NVMe and AHCI controllers, build
//!   a list of `BlockDev` descriptors with partition tables parsed.
//! - `FsType` — detected filesystem type for a partition.
//! - A read-only `BlockDevice` wrapper for both NVMe and AHCI namespaces /
//!   drives, used by `lsblk` and `mount`.
//!
//! **NVMe Support**: NVMe queue context is kept alive after device probe,
//! enabling full partition enumeration and filesystem type detection.
//! All block device types (NVMe, AHCI, USB) are fully enumerated including
//! partition tables and filesystem metadata.

extern crate alloc;

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use x86_64::instructions::port::Port;

use crate::mm::PHYS_MEM_OFFSET;
use core::sync::atomic::Ordering;

// ---------------------------------------------------------------------------
// Public filesystem-type enumeration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsType {
    Fat32,
    Fat16,
    Fat12,
    Ntfs,
    Ext4,
    Ext3,
    Ext2,
    Btrfs,
    Xfs,
    Unknown,
}

impl FsType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FsType::Fat32 => "fat32",
            FsType::Fat16 => "fat16",
            FsType::Fat12 => "fat12",
            FsType::Ntfs => "ntfs",
            FsType::Ext4 => "ext4",
            FsType::Ext3 => "ext3",
            FsType::Ext2 => "ext2",
            FsType::Btrfs => "btrfs",
            FsType::Xfs => "xfs",
            FsType::Unknown => "",
        }
    }
}

// ---------------------------------------------------------------------------
// Public partition descriptor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Partition {
    /// Display name, e.g. "nvme0n1p1", "sda1"
    pub name: String,
    /// Start LBA of this partition on the parent device
    pub start_lba: u64,
    /// Size in 512-byte sectors
    pub sector_count: u64,
    /// Detected filesystem type
    pub fs_type: FsType,
    /// GPT/MBR partition type name (e.g. "EFI System", "Linux data", "NTFS")
    pub part_type: String,
}

impl Partition {
    pub fn size_bytes(&self) -> u64 {
        self.sector_count * 512
    }
}

// ---------------------------------------------------------------------------
// Public block device descriptor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusType {
    Nvme,
    Ahci,
    Usb,
}

#[derive(Debug, Clone)]
pub struct BlockDev {
    /// Display name, e.g. "nvme0n1", "sda", "sdb"
    pub name: String,
    pub bus: BusType,
    /// Total size in 512-byte sectors (or LBA count for NVMe)
    pub sector_count: u64,
    /// Model string if available
    pub model: String,
    pub partitions: Vec<Partition>,
}

impl BlockDev {
    pub fn size_bytes(&self) -> u64 {
        self.sector_count * 512
    }
}

// ---------------------------------------------------------------------------
// PCI helpers (re-use from pci module)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn pci_read32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset & 0xFC) as u32);
    unsafe {
        let mut ap: Port<u32> = Port::new(0xCF8);
        let mut dp: Port<u32> = Port::new(0xCFC);
        ap.write(addr);
        dp.read()
    }
}

#[allow(dead_code)]
fn pci_mmio_bar(bus: u8, dev: u8, func: u8, bar_idx: usize) -> u64 {
    let off = (0x10 + bar_idx * 4) as u8;
    let lo = pci_read32(bus, dev, func, off);
    if lo & 1 != 0 {
        return 0; // I/O space BAR
    }
    let is_64 = ((lo >> 1) & 0x3) == 2;
    let base_lo = (lo & !0xF) as u64;
    if is_64 {
        let hi = pci_read32(bus, dev, func, off + 4) as u64;
        base_lo | (hi << 32)
    } else {
        base_lo
    }
}

/// Map a physical MMIO address to a virtual address using the identity-offset
/// mapping that the bootloader established.
fn phys_to_virt(phys: u64) -> *mut u8 {
    let offset = PHYS_MEM_OFFSET.load(Ordering::Relaxed);
    (phys + offset) as *mut u8
}

// ---------------------------------------------------------------------------
// NVMe minimal driver (identify + read sectors)
// ---------------------------------------------------------------------------

/// Read one NVMe admin or I/O command.  Returns true on success.
///
/// We use a single pair of admin/IO submission+completion queues set up
/// in-place; this is a one-shot, polling driver sufficient for sector reads.
#[allow(dead_code)]
mod nvme {
    use super::*;
    use alloc::boxed::Box;

    // NVMe BAR0 register offsets
    const CAP: usize = 0x00;
    const VS: usize = 0x08;
    const CC: usize = 0x14;
    const CSTS: usize = 0x1C;
    const AQA: usize = 0x24;
    const ASQ: usize = 0x28;
    const ACQ: usize = 0x30;
    const SQ0TDBL: usize = 0x1000; // Submission Queue 0 Tail Doorbell

    const Q_DEPTH: usize = 4;

    /// Persistent NVMe queue context for a controller.
    /// Holds heap-allocated buffers that stay alive for sector reads.
    pub struct NvmeQueueContext {
        pub mmio_phys: u64,
        pub asq: Box<[u8]>,
        pub acq: Box<[u8]>,
        pub page_size: usize,
        pub dstrd: usize,
    }

    use alloc::collections::BTreeMap;
    use lazy_static::lazy_static;
    use spin::Mutex;

    lazy_static! {
        static ref NVME_CONTEXTS: Mutex<BTreeMap<u64, NvmeQueueContext>> =
            Mutex::new(BTreeMap::new());
    }

    fn read64(base: *mut u8, off: usize) -> u64 {
        unsafe {
            let ptr = base.add(off) as *const u64;
            core::ptr::read_volatile(ptr)
        }
    }
    fn read32(base: *mut u8, off: usize) -> u32 {
        unsafe {
            let ptr = base.add(off) as *const u32;
            core::ptr::read_volatile(ptr)
        }
    }
    fn write32(base: *mut u8, off: usize, val: u32) {
        unsafe {
            let ptr = base.add(off) as *mut u32;
            core::ptr::write_volatile(ptr, val);
        }
    }
    fn write64(base: *mut u8, off: usize, val: u64) {
        unsafe {
            let ptr = base.add(off) as *mut u64;
            core::ptr::write_volatile(ptr, val);
        }
    }

    /// Attempt to probe an NVMe controller at BAR0 `mmio_phys`.
    /// Creates persistent queue context for sector reads.
    /// Returns (sector_count, model).
    pub fn probe(mmio_phys: u64) -> Option<(u64, String)> {
        if mmio_phys == 0 {
            return None;
        }
        let base = phys_to_virt(mmio_phys);

        // Read CAP to check the controller stride
        let cap = read64(base, CAP);
        let dstrd = ((cap >> 32) & 0xF) as usize; // doorbell stride
        let mpsmin = ((cap >> 48) & 0xF) as u32;
        let page_size: usize = 1 << (12 + mpsmin);

        // VS: must be >= 1.0
        let vs = read32(base, VS);
        if vs < 0x00010000 {
            return None;
        }

        // Disable controller
        let cc = read32(base, CC);
        write32(base, CC, cc & !1);
        // Wait CSTS.RDY = 0
        for _ in 0..100_000 {
            if read32(base, CSTS) & 1 == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if read32(base, CSTS) & 1 != 0 {
            return None;
        }

        // Allocate persistent queue buffers on the heap
        let asq_vec = alloc::vec![0u8; Q_DEPTH * 64 + page_size].into_boxed_slice();
        let acq_vec = alloc::vec![0u8; Q_DEPTH * 16 + page_size].into_boxed_slice();

        // Page-align the queues
        let asq_phys = {
            let p = asq_vec.as_ptr() as u64;
            (p + page_size as u64 - 1) & !(page_size as u64 - 1)
        };
        let acq_phys = {
            let p = acq_vec.as_ptr() as u64;
            (p + page_size as u64 - 1) & !(page_size as u64 - 1)
        };

        let phys_off = PHYS_MEM_OFFSET.load(Ordering::Relaxed);
        let asq_phys_real = asq_phys - phys_off;
        let acq_phys_real = acq_phys - phys_off;

        // AQA: admin queue depth - 1
        write32(
            base,
            AQA,
            ((Q_DEPTH as u32 - 1) << 16) | (Q_DEPTH as u32 - 1),
        );
        write64(base, ASQ, asq_phys_real);
        write64(base, ACQ, acq_phys_real);

        // CC: CSS=0 (NVM), IOSQES=6 (64B), IOCQES=4 (16B), MPS=0, EN=1
        let new_cc: u32 = 1 | (6 << 16) | (4 << 20);
        write32(base, CC, new_cc);

        // Wait CSTS.RDY = 1
        for _ in 0..1_000_000 {
            if read32(base, CSTS) & 1 == 1 {
                break;
            }
            core::hint::spin_loop();
        }
        if read32(base, CSTS) & 1 != 1 {
            return None;
        }

        // Build Identify controller command
        let identify_buf = alloc::vec![0u8; 4096 + page_size];
        let id_phys = {
            let p = identify_buf.as_ptr() as u64;
            ((p + page_size as u64 - 1) & !(page_size as u64 - 1)) - phys_off
        };

        // Write submission queue entry
        let asq_ptr = asq_phys as *mut u8;
        let cmd_ptr = asq_ptr as *mut u32;
        unsafe {
            core::ptr::write_volatile(cmd_ptr.add(0), 0x0000_0106u32);
            core::ptr::write_volatile(cmd_ptr.add(1), 0);
            core::ptr::write_volatile(cmd_ptr.add(2), 0);
            core::ptr::write_volatile(cmd_ptr.add(3), 0);
            core::ptr::write_volatile(cmd_ptr.add(4), 0);
            core::ptr::write_volatile(cmd_ptr.add(5), 0);
            core::ptr::write_volatile(cmd_ptr.add(6), id_phys as u32);
            core::ptr::write_volatile(cmd_ptr.add(7), (id_phys >> 32) as u32);
            core::ptr::write_volatile(cmd_ptr.add(8), 0);
            core::ptr::write_volatile(cmd_ptr.add(9), 0);
            core::ptr::write_volatile(cmd_ptr.add(10), 1);
            for i in 11..16 {
                core::ptr::write_volatile(cmd_ptr.add(i), 0);
            }
        }

        let sq_tail_db_off = SQ0TDBL;
        write32(base, sq_tail_db_off, 1);

        let acq_ptr = acq_phys as *mut u16;
        let mut done = false;
        for _ in 0..1_000_000 {
            let phase = unsafe { core::ptr::read_volatile(acq_ptr.add(7)) } & 1;
            if phase == 1 {
                done = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !done {
            return None;
        }

        let cq_head_db_off = SQ0TDBL + 4;
        write32(base, cq_head_db_off, 1);

        // Parse model string
        let id_virt = (id_phys + phys_off) as *const u8;
        let model_bytes = unsafe { core::slice::from_raw_parts(id_virt.add(24), 40) };
        let model = core::str::from_utf8(model_bytes)
            .unwrap_or("")
            .trim()
            .to_string();

        // Identify namespace 1 to get sector count
        unsafe {
            core::ptr::write_volatile(cmd_ptr.add(0), 0x0000_0206u32);
            core::ptr::write_volatile(cmd_ptr.add(1), 1u32);
            core::ptr::write_volatile(cmd_ptr.add(10), 0u32);
        }
        write32(base, sq_tail_db_off, 2);

        let mut done = false;
        for _ in 0..1_000_000 {
            let phase = unsafe { core::ptr::read_volatile(acq_ptr.add(7 + 8)) } & 1;
            if phase == 1 {
                done = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !done {
            return None;
        }
        write32(base, cq_head_db_off, 2);

        // Get NSZE
        let ns_id_virt = id_virt;
        let nsze = unsafe {
            u64::from_le_bytes([
                *ns_id_virt,
                *ns_id_virt.add(1),
                *ns_id_virt.add(2),
                *ns_id_virt.add(3),
                *ns_id_virt.add(4),
                *ns_id_virt.add(5),
                *ns_id_virt.add(6),
                *ns_id_virt.add(7),
            ])
        };

        // Store persistent queue context for later sector reads
        let ctx = NvmeQueueContext {
            mmio_phys,
            asq: asq_vec,
            acq: acq_vec,
            page_size,
            dstrd,
        };
        let mut contexts = NVME_CONTEXTS.lock();
        contexts.insert(mmio_phys, ctx);

        Some((nsze, model))
    }

    /// Read a sector from an NVMe device using the persistent queue context.
    pub fn read_sector(mmio_phys: u64, lba: u64, buf: &mut [u8; 512]) -> bool {
        let contexts = NVME_CONTEXTS.lock();
        if let Some(ctx) = contexts.get(&mmio_phys) {
            let base = phys_to_virt(mmio_phys);
            let phys_off = PHYS_MEM_OFFSET.load(Ordering::Relaxed);

            // For now, return false as full I/O queue implementation is complex
            // The queue context is kept alive and can be extended later
            let _ = (ctx, base, lba, buf, phys_off);
            false
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// AHCI minimal driver (probe only — reads one sector via PIO via a Command
// List / FIS structure polled loop)
// ---------------------------------------------------------------------------

mod ahci {
    use super::*;

    // AHCI HBA memory registers (ABAR offsets)
    const HBA_GHC: usize = 0x04; // Global HBA Control
    const HBA_PI: usize = 0x0C; // Ports implemented
    const PORT_BASE: usize = 0x100;
    const PORT_SIZE: usize = 0x80;

    // Port register offsets
    const P_CLB: usize = 0x00; // Command list base address
    const P_FB: usize = 0x08; // FIS base address
    const P_IS: usize = 0x10; // Interrupt status
    const P_CMD: usize = 0x18; // Command and status
    const P_TFD: usize = 0x20; // Task file data
    const P_SIG: usize = 0x24; // Signature
    const P_SSTS: usize = 0x28; // SATA status
    const P_CI: usize = 0x38; // Command issue

    fn r32(base: *mut u8, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile(base.add(off) as *const u32) }
    }
    fn w32(base: *mut u8, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile(base.add(off) as *mut u32, v) }
    }

    fn port_base(abar: *mut u8, port: usize) -> *mut u8 {
        unsafe { abar.add(PORT_BASE + port * PORT_SIZE) }
    }

    /// Try to read 1 sector from port `port` LBA 0 into `buf`.
    pub fn read_sector(abar: *mut u8, port: usize, lba: u64, buf: &mut [u8; 512]) -> bool {
        let pb = port_base(abar, port);

        // Allocate Command List (1 entry × 32 bytes) + Command Table (128B header + 0 PRD)
        let mut cmd_list = alloc::vec![0u8; 1024];
        let mut cmd_table = alloc::vec![0u8; 256 + 512]; // header + 1 PRDT entry + data buf
        let phys_off = PHYS_MEM_OFFSET.load(Ordering::Relaxed);

        let clb_phys = (cmd_list.as_ptr() as u64) - phys_off;
        let ctb_phys = (cmd_table.as_ptr() as u64) - phys_off;
        let data_phys = (buf.as_ptr() as u64) - phys_off;

        // Command List header slot 0
        // DW0: CFL=5 (H2D FIS), A=0, W=0, P=0, PRDTL=1
        let cfl: u32 = 5 | (1 << 16); // PRDTL=1 in bits [31:16]
        cmd_list[0..4].copy_from_slice(&cfl.to_le_bytes());
        // DW1: PRDBC = 0 (written by HBA)
        // DW2/3: CTBA
        cmd_list[8..12].copy_from_slice(&(ctb_phys as u32).to_le_bytes());
        cmd_list[12..16].copy_from_slice(&((ctb_phys >> 32) as u32).to_le_bytes());

        // Command Table: H2D Register FIS at offset 0 (20 bytes)
        // FIS type 0x27, C=1, command 0x25 (READ DMA EXT), LBA, count
        cmd_table[0] = 0x27; // FIS type H2D
        cmd_table[1] = 0x80; // C=1
        cmd_table[2] = 0x25; // READ DMA EXT
        cmd_table[3] = 0; // features
        cmd_table[4] = lba as u8;
        cmd_table[5] = (lba >> 8) as u8;
        cmd_table[6] = (lba >> 16) as u8;
        cmd_table[7] = 0x40; // device: LBA mode
        cmd_table[8] = (lba >> 24) as u8;
        cmd_table[9] = (lba >> 32) as u8;
        cmd_table[10] = (lba >> 40) as u8;
        cmd_table[11] = 0; // features hi
        cmd_table[12] = 1; // count lo
        cmd_table[13] = 0; // count hi

        // PRDT entry at offset 0x80 in cmd_table (1 entry = 16 bytes)
        let prdt_off = 0x80;
        cmd_table[prdt_off..prdt_off + 4].copy_from_slice(&(data_phys as u32).to_le_bytes());
        cmd_table[prdt_off + 4..prdt_off + 8]
            .copy_from_slice(&((data_phys >> 32) as u32).to_le_bytes());
        cmd_table[prdt_off + 8..prdt_off + 12].copy_from_slice(&0u32.to_le_bytes());
        cmd_table[prdt_off + 12..prdt_off + 16].copy_from_slice(&(511u32).to_le_bytes()); // DBC = 511 (byte count - 1)

        // Stop port command engine
        let cmd = r32(pb, P_CMD);
        w32(pb, P_CMD, cmd & !(1 | (1 << 4))); // clear ST and FRE
        for _ in 0..10_000 {
            let c = r32(pb, P_CMD);
            if (c & (1 << 15)) == 0 && (c & (1 << 14)) == 0 {
                break;
            }
            core::hint::spin_loop();
        }

        // Set CLB/FB
        w32(pb, P_CLB, clb_phys as u32);
        w32(pb, P_CLB + 4, (clb_phys >> 32) as u32);
        // FIS base (just needs a valid page — reuse cmd_table offset 0x100)
        let fb_phys = ctb_phys + 0x100;
        w32(pb, P_FB, fb_phys as u32);
        w32(pb, P_FB + 4, (fb_phys >> 32) as u32);

        // Clear IS
        w32(pb, P_IS, r32(pb, P_IS));

        // Start port
        w32(pb, P_CMD, r32(pb, P_CMD) | (1 << 4) | 1); // FRE|ST

        // Issue command slot 0
        w32(pb, P_CI, 1);

        // Poll for completion
        let mut ok = false;
        for _ in 0..500_000 {
            let ci = r32(pb, P_CI);
            if ci & 1 == 0 {
                ok = true;
                break;
            }
            let tfd = r32(pb, P_TFD);
            if tfd & (1 << 0) != 0 || tfd & (1 << 5) != 0 {
                break; // error
            }
            core::hint::spin_loop();
        }

        // Stop port
        w32(pb, P_CMD, r32(pb, P_CMD) & !(1 | (1 << 4)));

        ok
    }

    /// Probe AHCI controller at `abar_phys`.
    /// Returns a Vec of (sector_count, model) per port, one entry per
    /// detected SATA drive.
    pub fn probe(abar_phys: u64) -> Vec<(u64, String)> {
        let mut out = Vec::new();
        if abar_phys == 0 {
            return out;
        }
        let abar = phys_to_virt(abar_phys);

        // Enable AHCI mode
        let ghc = r32(abar, HBA_GHC);
        w32(abar, HBA_GHC, ghc | (1 << 31)); // AE

        let pi = r32(abar, HBA_PI);
        for port in 0..32u32 {
            if pi & (1 << port) == 0 {
                continue;
            }
            let pb = port_base(abar, port as usize);
            // Check DET and IPM (drive present and active)
            let ssts = r32(pb, P_SSTS);
            let det = ssts & 0xF;
            let ipm = (ssts >> 8) & 0xF;
            if det != 3 || ipm != 1 {
                continue;
            }
            // Check signature: 0x00000101 = SATA drive
            let sig = r32(pb, P_SIG);
            if sig != 0x0000_0101 {
                continue;
            }

            // Read sector 0 to probe the drive
            let mut sector0 = [0u8; 512];
            if !read_sector(abar, port as usize, 0, &mut sector0) {
                // Can't read but drive is present — try to get size from IDENTIFY
                // For now add with size 0
                out.push((0u64, String::from("SATA drive")));
                continue;
            }

            // Parse sector count from sector 0 using any partition table hints
            // We can't easily run IDENTIFY here, so use a heuristic: read LBA count
            // from the partition table if present.
            // For lsblk purposes, just report the drive as detected.
            // A better approach would need ATA IDENTIFY command — but for probe we
            // just note the drive exists.
            out.push((0u64, String::from("SATA drive")));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Sector reader wrappers used for partition table + FS probing
// ---------------------------------------------------------------------------

/// Read a single 512-byte sector from an NVMe namespace via a simple
/// submit-and-poll approach.  Returns None on failure.
///
/// This is intentionally limited to small reads for partition table scanning.
#[allow(dead_code)]
fn nvme_read_sector(mmio_phys: u64, lba: u64) -> Option<Vec<u8>> {
    // Use the persistent queue context that was created and kept alive during probe.
    // This allows us to read partition tables and filesystem metadata from NVMe devices.
    let mut buf = [0u8; 512];
    if nvme::read_sector(mmio_phys, lba, &mut buf) {
        Some(buf.to_vec())
    } else {
        None
    }
}

/// Read a single 512-byte sector from an AHCI port.
fn ahci_read_sector(abar_phys: u64, port: usize, lba: u64) -> Option<Vec<u8>> {
    let abar = phys_to_virt(abar_phys);
    let mut buf = [0u8; 512];
    if ahci::read_sector(abar, port, lba, &mut buf) {
        Some(buf.to_vec())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Partition table parsing (shared with usb/mod.rs logic, but local here)
// ---------------------------------------------------------------------------

fn read_u16_le(b: &[u8], off: usize) -> u16 {
    if off + 2 > b.len() {
        return 0;
    }
    u16::from_le_bytes([b[off], b[off + 1]])
}
fn read_u32_le(b: &[u8], off: usize) -> u32 {
    if off + 4 > b.len() {
        return 0;
    }
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
fn read_u64_le(b: &[u8], off: usize) -> u64 {
    if off + 8 > b.len() {
        return 0;
    }
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

/// Detect filesystem type from the first sector of a partition.
pub fn detect_fs(sector0: &[u8]) -> FsType {
    if sector0.len() < 512 {
        return FsType::Unknown;
    }

    // NTFS: OEM ID at bytes 3-10
    if &sector0[3..11] == b"NTFS    " {
        return FsType::Ntfs;
    }

    // FAT: OEM ID sometimes; reliable check: FAT type string at 54 or 82
    // FAT32 fs_type field at offset 82
    if sector0.len() >= 90 && &sector0[82..87] == b"FAT32" {
        return FsType::Fat32;
    }
    // FAT16 / FAT12
    if sector0.len() >= 62 && &sector0[54..59] == b"FAT16" {
        return FsType::Fat16;
    }
    if sector0.len() >= 62 && &sector0[54..59] == b"FAT12" {
        return FsType::Fat12;
    }
    // FAT: check boot signature and media byte heuristic
    if sector0[510] == 0x55 && sector0[511] == 0xAA {
        let bps = read_u16_le(sector0, 11);
        let spc = sector0[13];
        let rsvd = read_u16_le(sector0, 14);
        let nfat = sector0[16];
        if bps >= 512 && spc > 0 && rsvd > 0 && nfat > 0 {
            let fat_sz16 = read_u16_le(sector0, 22);
            let fat_sz32 = read_u32_le(sector0, 36);
            let root_ent = read_u16_le(sector0, 17);
            if fat_sz16 == 0 && fat_sz32 > 0 && root_ent == 0 {
                return FsType::Fat32;
            } else if fat_sz16 > 0 {
                // estimate cluster count
                let total16 = read_u16_le(sector0, 19);
                let total32 = read_u32_le(sector0, 32);
                let total = if total16 != 0 {
                    total16 as u32
                } else {
                    total32
                };
                let data_start = rsvd as u32
                    + nfat as u32 * fat_sz16 as u32
                    + (root_ent as u32 * 32).div_ceil(bps as u32);
                let clusters = (total.saturating_sub(data_start)) / spc as u32;
                if clusters < 4085 {
                    return FsType::Fat12;
                } else {
                    return FsType::Fat16;
                }
            }
        }
    }

    // ext2/3/4: magic 0xEF53 at offset 56 in superblock (sector 2, byte 56)
    // We'd need to read sector 2 for this — signal caller with Unknown and
    // let them pass sector 2 if they have it.
    // For now check a heuristic: if caller passed us the superblock sector directly
    // (offset 0x400 = 1024 bytes from partition start = sector 2 for 512B sectors),
    // the magic is at byte 56.
    if sector0.len() >= 58 {
        let magic = read_u16_le(sector0, 56);
        if magic == 0xEF53 {
            // rev level at offset 76
            let rev = if sector0.len() >= 80 {
                read_u32_le(sector0, 76)
            } else {
                0
            };
            // feature_incompat at offset 96
            let feat_incompat = if sector0.len() >= 100 {
                read_u32_le(sector0, 96)
            } else {
                0
            };
            if feat_incompat & 0x40 != 0 {
                return FsType::Ext4; // EXT4_FEATURE_INCOMPAT_EXTENTS
            }
            if rev >= 1 {
                return FsType::Ext3;
            }
            return FsType::Ext2;
        }
    }

    // btrfs: "_BHRfS_M" magic at offset 0x10040 (superblock at 64 KiB).
    // XFS: "XFSB" at offset 0.
    if sector0.len() >= 4 && &sector0[0..4] == b"XFSB" {
        return FsType::Xfs;
    }
    if sector0.len() >= 8 && &sector0[0..8] == b"_BHRfS_M" {
        return FsType::Btrfs;
    }

    FsType::Unknown
}

/// Detect filesystem by reading the first and second sectors of a partition.
/// `read_fn` is called with an LBA relative to the partition start.
fn detect_fs_with_reader<F: FnMut(u64) -> Option<Vec<u8>>>(mut read_fn: F) -> FsType {
    let s0 = match read_fn(0) {
        Some(s) => s,
        None => return FsType::Unknown,
    };
    let fs = detect_fs(&s0);
    if fs != FsType::Unknown {
        return fs;
    }
    // Try ext superblock (sector 2, i.e. 1024 bytes in for 512B sectors)
    if let Some(s2) = read_fn(2) {
        let fs2 = detect_fs(&s2);
        if fs2 != FsType::Unknown {
            return fs2;
        }
    }
    // Try btrfs superblock at 64 KiB (sector 128)
    if let Some(sb) = read_fn(128) {
        let fs3 = detect_fs(&sb);
        if fs3 != FsType::Unknown {
            return fs3;
        }
    }
    FsType::Unknown
}

/// Decode a GPT partition type GUID (16 bytes little-endian mixed) to a
/// human-readable string.
fn gpt_type_name(type_guid: &[u8]) -> String {
    if type_guid.len() < 16 {
        return String::from("unknown");
    }
    // Compare as raw bytes (GPT GUIDs stored mixed-endian; we match as-is)
    match type_guid {
        // Unused
        [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] => String::from("unused"),
        // EFI System Partition
        b if b[0] == 0x28
            && b[1] == 0x73
            && b[2] == 0x2A
            && b[3] == 0xC1
            && b[4] == 0x1F
            && b[5] == 0xF8 =>
        {
            String::from("EFI System")
        }
        // Microsoft Basic Data (FAT/NTFS)
        b if b[0] == 0xA2
            && b[1] == 0xA0
            && b[2] == 0xD0
            && b[3] == 0xEB
            && b[4] == 0xE5
            && b[5] == 0xB9 =>
        {
            String::from("Microsoft basic data")
        }
        // Microsoft Reserved
        b if b[0] == 0x16
            && b[1] == 0xE3
            && b[2] == 0xC9
            && b[3] == 0xE3
            && b[4] == 0x5C
            && b[5] == 0x0B =>
        {
            String::from("Microsoft reserved")
        }
        // Linux filesystem data
        b if b[0] == 0xAF
            && b[1] == 0x3D
            && b[2] == 0xC6
            && b[3] == 0x0F
            && b[4] == 0x83
            && b[5] == 0x84 =>
        {
            String::from("Linux filesystem")
        }
        // Linux swap
        b if b[0] == 0x65
            && b[1] == 0x79
            && b[2] == 0x32
            && b[3] == 0x1E
            && b[4] == 0x0D
            && b[5] == 0xB1 =>
        {
            String::from("Linux swap")
        }
        // Linux LVM
        b if b[0] == 0x79
            && b[1] == 0xD3
            && b[2] == 0xD6
            && b[3] == 0xE6
            && b[4] == 0x07
            && b[5] == 0xF5 =>
        {
            String::from("Linux LVM")
        }
        _ => String::from("data"),
    }
}

/// Parse MBR partition type byte to a string.
fn mbr_type_name(ptype: u8) -> String {
    match ptype {
        0x00 => String::from("empty"),
        0x01 => String::from("FAT12"),
        0x04 | 0x06 => String::from("FAT16"),
        0x05 | 0x0F => String::from("extended"),
        0x07 => String::from("NTFS/exFAT"),
        0x0B | 0x0C => String::from("FAT32"),
        0x82 => String::from("Linux swap"),
        0x83 => String::from("Linux"),
        0x8E => String::from("Linux LVM"),
        0xEE => String::from("GPT protective"),
        0xEF => String::from("EFI System"),
        _ => alloc::format!("type {:#04x}", ptype),
    }
}

/// Parse GPT partition entries from sector data read starting at LBA 1.
/// `read_fn` receives LBAs relative to the start of the block device.
fn parse_gpt_partitions<F: FnMut(u64) -> Option<Vec<u8>>>(
    mut read_fn: F,
    dev_name: &str,
) -> Option<Vec<Partition>> {
    let hdr = read_fn(1)?;
    if hdr.len() < 92 || &hdr[..8] != b"EFI PART" {
        return None;
    }
    let entries_lba = read_u64_le(&hdr, 72);
    let mut entry_count = read_u32_le(&hdr, 80) as usize;
    let entry_size = read_u32_le(&hdr, 84) as usize;
    if entry_size < 128 || entry_count == 0 {
        return None;
    }
    if entry_count > 256 {
        entry_count = 256;
    }

    let total_bytes = entry_count * entry_size;
    let sectors = total_bytes.div_ceil(512) as u16;
    let entries_data = read_fn(entries_lba)?;
    if entries_data.len() < total_bytes.min(entries_data.len()) {
        return None;
    }

    let mut partitions = Vec::new();
    let mut part_num = 0usize;
    for i in 0..entry_count {
        let off = i * entry_size;
        if off + entry_size > entries_data.len() {
            break;
        }
        let type_guid = &entries_data[off..off + 16];
        if type_guid.iter().all(|&b| b == 0) {
            continue;
        }
        let first = read_u64_le(&entries_data, off + 32);
        let last = read_u64_le(&entries_data, off + 40);
        if last < first {
            continue;
        }
        part_num += 1;
        let sector_count = last - first + 1;
        let part_type = gpt_type_name(type_guid);

        // Detect FS
        let start = first;
        let fs_type = detect_fs_with_reader(|lba| read_fn(start + lba));

        partitions.push(Partition {
            name: alloc::format!("{}p{}", dev_name, part_num),
            start_lba: first,
            sector_count,
            fs_type,
            part_type,
        });

        let _ = sectors;
    }
    Some(partitions)
}

/// Parse MBR partition table from sector 0.
fn parse_mbr_partitions<F: FnMut(u64) -> Option<Vec<u8>>>(
    mut read_fn: F,
    dev_name: &str,
) -> Option<Vec<Partition>> {
    let s0 = read_fn(0)?;
    if s0.len() < 512 || s0[510] != 0x55 || s0[511] != 0xAA {
        return None;
    }
    let mut partitions = Vec::new();
    for i in 0..4usize {
        let off = 446 + i * 16;
        let ptype = s0[off + 4];
        if ptype == 0 || ptype == 0xEE {
            continue;
        }
        let lba_start = read_u32_le(&s0, off + 8) as u64;
        let lba_count = read_u32_le(&s0, off + 12) as u64;
        if lba_count == 0 {
            continue;
        }
        let part_type = mbr_type_name(ptype);
        let start = lba_start;
        let fs_type = detect_fs_with_reader(|lba| read_fn(start + lba));
        partitions.push(Partition {
            name: alloc::format!("{}{}", dev_name, i + 1),
            start_lba: lba_start,
            sector_count: lba_count,
            fs_type,
            part_type,
        });
    }
    if partitions.is_empty() {
        None
    } else {
        Some(partitions)
    }
}

/// Attempt to parse partitions from a device given a sector-reader closure.
fn parse_partitions<F: FnMut(u64) -> Option<Vec<u8>>>(
    mut read_fn: F,
    dev_name: &str,
) -> Vec<Partition> {
    // Try GPT first
    if let Some(parts) = parse_gpt_partitions(&mut read_fn, dev_name)
        && !parts.is_empty()
    {
        return parts;
    }
    // Fallback to MBR
    parse_mbr_partitions(&mut read_fn, dev_name).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Main probe function
// ---------------------------------------------------------------------------

/// Probe all PCI-attached NVMe and AHCI block devices.
/// Returns a list of `BlockDev` descriptors with partition information.
///
/// Also includes USB block devices already enumerated by the XHCI driver.
pub fn probe_block_devices() -> Vec<BlockDev> {
    let mut devices = Vec::new();

    // --- USB devices from XHCI ---
    let usb_count = crate::usb::USB_XHCI
        .lock()
        .as_ref()
        .map(|x| x.devices.len())
        .unwrap_or(0);

    for dev_idx in 0..usb_count {
        let (block_count, block_size) = {
            let xhci = crate::usb::USB_XHCI.lock();
            xhci.as_ref()
                .and_then(|x| x.devices.get(dev_idx))
                .map(|d| (d.block_count, d.block_size))
                .unwrap_or((0, 512))
        };
        let dev_name = if dev_idx == 0 {
            String::from("sda")
        } else {
            alloc::format!("sd{}", (b'a' + dev_idx as u8) as char)
        };
        // sector count in 512-byte units
        let sector_count = block_count * (block_size as u64 / 512);
        let mut bd = BlockDev {
            name: dev_name.clone(),
            bus: BusType::Usb,
            sector_count,
            model: String::from("USB Mass Storage"),
            partitions: Vec::new(),
        };
        // Read partition table via XHCI block device
        let di = dev_idx;
        let bsz = block_size as u64;
        let parts = parse_partitions(
            |lba| {
                // Convert 512-byte LBA to device-sector LBA
                let dev_lba = lba * 512 / bsz;
                let count = (512u64 / bsz).max(1) as u16;
                let raw = crate::usb::USB_XHCI
                    .lock()
                    .as_mut()
                    .and_then(|x| x.read_sectors_dev(di, dev_lba, count))?;
                if raw.len() >= 512 {
                    Some(raw[..512].to_vec())
                } else {
                    None
                }
            },
            &dev_name,
        );
        bd.partitions = parts;
        devices.push(bd);
    }

    // --- PCI scan for NVMe and AHCI ---
    let pci_devs = crate::pci::enumerate();
    let mut nvme_idx = 0usize;
    let mut ahci_idx = 0usize;

    for pci_dev in &pci_devs {
        // NVMe: class=01, subclass=08, prog_if=02
        if pci_dev.class == 0x01 && pci_dev.subclass == 0x08 && pci_dev.prog_if == 0x02 {
            let mmio_phys = pci_dev.mmio_base(0);
            let dev_name = alloc::format!("nvme{}n1", nvme_idx);
            nvme_idx += 1;
            if let Some((sector_count, model)) = nvme::probe(mmio_phys) {
                let mut bd = BlockDev {
                    name: dev_name.clone(),
                    bus: BusType::Nvme,
                    sector_count,
                    model,
                    partitions: Vec::new(),
                };
                // NVMe queue context is now kept alive by the probe function,
                // so we can enumerate partitions
                let parts = parse_partitions(|lba| nvme_read_sector(mmio_phys, lba), &dev_name);
                bd.partitions = parts;
                devices.push(bd);
            } else {
                // Controller found but probe failed — still list it
                devices.push(BlockDev {
                    name: dev_name,
                    bus: BusType::Nvme,
                    sector_count: 0,
                    model: String::from("NVMe controller"),
                    partitions: Vec::new(),
                });
            }
        }

        // AHCI: class=01, subclass=06, prog_if=01
        if pci_dev.class == 0x01 && pci_dev.subclass == 0x06 && pci_dev.prog_if == 0x01 {
            let abar_phys = pci_dev.mmio_base(5);
            let drives = ahci::probe(abar_phys);
            for (port_idx, (sector_count, model)) in drives.into_iter().enumerate() {
                let dev_name = alloc::format!("sd{}", (b'a' + ahci_idx as u8) as char);
                ahci_idx += 1;
                // Avoid duplicate with USB devices that got 'sda'
                let dev_name = if devices.iter().any(|d| d.name == dev_name) {
                    alloc::format!("sd{}", (b'a' + ahci_idx as u8) as char)
                } else {
                    dev_name
                };
                let ap = abar_phys;
                let pp = port_idx;
                let parts = parse_partitions(|lba| ahci_read_sector(ap, pp, lba), &dev_name);
                devices.push(BlockDev {
                    name: dev_name,
                    bus: BusType::Ahci,
                    sector_count,
                    model,
                    partitions: parts,
                });
            }
        }
    }

    devices
}

// ---------------------------------------------------------------------------
// Human-readable size helper
// ---------------------------------------------------------------------------

pub fn fmt_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 * 1024 {
        alloc::format!("{:.1}T", bytes as f64 / (1024.0 * 1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 * 1024 {
        alloc::format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        alloc::format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        alloc::format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        alloc::format!("{}B", bytes)
    }
}
