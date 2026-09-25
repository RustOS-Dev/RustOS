pub mod mass_storage;
pub mod xhci;

use alloc::boxed::Box;
use spin::Mutex;

// ---------------------------------------------------------------------------
// BlockDevice trait
// ---------------------------------------------------------------------------

/// A block device that can read 512-byte sectors.
pub trait BlockDevice: Send {
    fn read_sectors(&mut self, lba: u64, count: u16) -> Option<alloc::vec::Vec<u8>>;
    fn write_sectors(&mut self, lba: u64, data: &[u8]) -> Option<()>;
    fn sector_count(&self) -> u64;
}

// ---------------------------------------------------------------------------
// Global XHCI controller
// ---------------------------------------------------------------------------

/// The global XHCI host controller.  Initialised once in `kernel_main` and
/// kept alive for the lifetime of the kernel so that hot-plug rescans and
/// ongoing FAT32 I/O can reach it.
pub static USB_XHCI: Mutex<Option<xhci::Xhci>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// Index-based block device wrapper
// ---------------------------------------------------------------------------

/// A `BlockDevice` backed by a specific device index in `USB_XHCI`.
///
/// Storing only the index means many `XhciBlockDevice`s can coexist and each
/// transparently delegates to the shared, global XHCI controller.
pub struct XhciBlockDevice {
    pub dev_idx: usize,
}

impl BlockDevice for XhciBlockDevice {
    fn read_sectors(&mut self, lba: u64, count: u16) -> Option<alloc::vec::Vec<u8>> {
        USB_XHCI
            .lock()
            .as_mut()?
            .read_sectors_dev(self.dev_idx, lba, count)
    }

    fn write_sectors(&mut self, lba: u64, data: &[u8]) -> Option<()> {
        USB_XHCI
            .lock()
            .as_mut()?
            .write_sectors_dev(self.dev_idx, lba, data)
    }

    fn sector_count(&self) -> u64 {
        USB_XHCI
            .lock()
            .as_ref()
            .and_then(|x| x.devices.get(self.dev_idx))
            .map(|d| d.block_count)
            .unwrap_or(0)
    }
}

/// A `BlockDevice` view into a partition of another block device.
pub struct PartitionBlockDevice {
    inner: Box<dyn BlockDevice>,
    start_lba: u64,
    sector_count: u64,
}

impl PartitionBlockDevice {
    pub fn new(inner: Box<dyn BlockDevice>, start_lba: u64, sector_count: u64) -> Self {
        Self {
            inner,
            start_lba,
            sector_count,
        }
    }
}

impl BlockDevice for PartitionBlockDevice {
    fn read_sectors(&mut self, lba: u64, count: u16) -> Option<alloc::vec::Vec<u8>> {
        let end = lba.checked_add(count as u64)?;
        if end > self.sector_count {
            return None;
        }
        self.inner
            .read_sectors(self.start_lba.checked_add(lba)?, count)
    }

    fn write_sectors(&mut self, lba: u64, data: &[u8]) -> Option<()> {
        if !data.len().is_multiple_of(512) {
            return None;
        }
        let count = (data.len() / 512) as u64;
        let end = lba.checked_add(count)?;
        if end > self.sector_count {
            return None;
        }
        self.inner
            .write_sectors(self.start_lba.checked_add(lba)?, data)
    }

    fn sector_count(&self) -> u64 {
        self.sector_count
    }
}

#[derive(Clone, Copy)]
pub struct PartitionInfo {
    pub start_lba: u64,
    pub sector_count: u64,
    /// True when this partition carries the EFI System Partition type GUID.
    pub is_efi: bool,
}

/// Records the device index and start LBA of the partition mounted as `/`.
/// `start_lba == 0` indicates the whole device (no partition table) was used.
static ROOT_PARTITION: Mutex<Option<(usize, u64)>> = Mutex::new(None);

fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    if off.checked_add(4)? > buf.len() {
        return None;
    }
    Some(u32::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
    ]))
}

fn read_u64_le(buf: &[u8], off: usize) -> Option<u64> {
    if off.checked_add(8)? > buf.len() {
        return None;
    }
    Some(u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ]))
}

/// Public wrapper around `gpt_partitions_for_device` for use in shell mount command.
pub fn gpt_partitions_for_device_pub(dev_idx: usize) -> alloc::vec::Vec<PartitionInfo> {
    gpt_partitions_for_device(dev_idx)
}

fn gpt_partitions_for_device(dev_idx: usize) -> alloc::vec::Vec<PartitionInfo> {
    use alloc::vec::Vec;

    let mut dev = XhciBlockDevice { dev_idx };
    let hdr = match dev.read_sectors(1, 1) {
        Some(h) if h.len() >= 512 => h,
        _ => return Vec::new(),
    };
    if &hdr[..8] != b"EFI PART" {
        return Vec::new();
    }

    let entries_lba = match read_u64_le(&hdr, 72) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let mut entry_count = match read_u32_le(&hdr, 80) {
        Some(v) => v as usize,
        None => return Vec::new(),
    };
    let entry_size = match read_u32_le(&hdr, 84) {
        Some(v) => v as usize,
        None => return Vec::new(),
    };
    if entry_size < 128 {
        return Vec::new();
    }
    if entry_count > 256 {
        entry_count = 256;
    }

    let total_bytes = match entry_count.checked_mul(entry_size) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let sectors = total_bytes.div_ceil(512) as u16;
    let entries = match dev.read_sectors(entries_lba, sectors) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for i in 0..entry_count {
        let off = i * entry_size;
        if off + entry_size > entries.len() {
            break;
        }
        // Unused entry = zero type GUID (first 16 bytes all zero).
        if entries[off..off + 16].iter().all(|b| *b == 0) {
            continue;
        }
        let first = match read_u64_le(&entries, off + 32) {
            Some(v) => v,
            None => continue,
        };
        let last = match read_u64_le(&entries, off + 40) {
            Some(v) => v,
            None => continue,
        };
        if last < first {
            continue;
        }
        // EFI System Partition type GUID (mixed-endian on-disk representation):
        // C12A7328-F81F-11D2-BA4B-00A0C93EC93B
        // bytes: 28 73 2A C1 1F F8 D2 11 BA 4B 00 A0 C9 3E C9 3B
        let is_efi = entries[off..off + 6] == [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8];
        out.push(PartitionInfo {
            start_lba: first,
            sector_count: (last - first) + 1,
            is_efi,
        });
    }

    out
}

/// Parse MBR partition entries for a USB block device.
/// Returns FAT32 (type 0x0B / 0x0C) partitions only.
fn mbr_partitions_for_device(dev_idx: usize) -> alloc::vec::Vec<PartitionInfo> {
    use alloc::vec::Vec;

    let mut dev = XhciBlockDevice { dev_idx };
    let sector0 = match dev.read_sectors(0, 1) {
        Some(s) if s.len() >= 512 => s,
        _ => return Vec::new(),
    };
    // MBR signature
    if sector0[510] != 0x55 || sector0[511] != 0xAA {
        return Vec::new();
    }
    // Reject GPT protective MBR (first partition type 0xEE)
    if sector0[446 + 4] == 0xEE {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..4usize {
        let base = 446 + i * 16;
        let part_type = sector0[base + 4];
        // FAT32 with CHS (0x0B) or FAT32 with LBA (0x0C)
        if part_type != 0x0B && part_type != 0x0C {
            continue;
        }
        let start_lba = match read_u32_le(&sector0, base + 8) {
            Some(v) if v > 0 => v as u64,
            _ => continue,
        };
        let sector_count = match read_u32_le(&sector0, base + 12) {
            Some(v) if v > 0 => v as u64,
            _ => continue,
        };
        out.push(PartitionInfo {
            start_lba,
            sector_count,
            is_efi: false,
        });
    }
    out
}

/// Mount a FAT32 partition from USB device 0 as the root filesystem (`/`).
///
/// Scan order:
/// 1. All non-EFI GPT partitions in order — try each as FAT32.
/// 2. MBR FAT32 partitions (type 0x0B/0x0C) — if no GPT found.
/// 3. Whole device at LBA 0 — for raw FAT32 volumes with no partition table.
///
/// Stores the winning (dev_idx, start_lba) in `ROOT_PARTITION` so that
/// `mount_storage_devices` can skip it and avoid double-mounting.
pub fn mount_boot_storage_root() -> bool {
    let try_fat32 = |partitions: alloc::vec::Vec<PartitionInfo>| -> Option<(PartitionInfo, crate::fs::fat32::Fat32Fs)> {
        for part in partitions {
            if part.is_efi {
                continue;
            }
            let block_dev: Box<dyn BlockDevice> = Box::new(PartitionBlockDevice::new(
                Box::new(XhciBlockDevice { dev_idx: 0 }),
                part.start_lba,
                part.sector_count,
            ));
            if let Some(fat32) = crate::fs::fat32::Fat32Fs::new(block_dev) {
                return Some((part, fat32));
            }
        }
        None
    };

    // 1. Try GPT partitions.
    let gpt = gpt_partitions_for_device(0);
    let mut result = if !gpt.is_empty() {
        try_fat32(gpt)
    } else {
        None
    };

    // 2. Try MBR partitions if GPT yielded nothing.
    if result.is_none() {
        result = try_fat32(mbr_partitions_for_device(0));
    }

    if let Some((part, fat32)) = result {
        let mut vfs = crate::vfs::VFS.lock();
        if let Some(vfs) = vfs.as_mut() {
            vfs.set_root(
                Box::new(crate::vfs::Fat32Mount(fat32)),
                "fat32 RUSTOS_ROOT persistent",
            );
            *ROOT_PARTITION.lock() = Some((0, part.start_lba));
            crate::println!(
                "[usb] mounted device0 lba{} FAT32 as root '/'",
                part.start_lba
            );
            return true;
        }
    }

    // 3. Try whole device (raw FAT32, no partition table).
    let block_dev: Box<dyn BlockDevice> = Box::new(XhciBlockDevice { dev_idx: 0 });
    if let Some(fat32) = crate::fs::fat32::Fat32Fs::new(block_dev) {
        let mut vfs = crate::vfs::VFS.lock();
        if let Some(vfs) = vfs.as_mut() {
            vfs.set_root(
                Box::new(crate::vfs::Fat32Mount(fat32)),
                "fat32 RUSTOS_ROOT persistent",
            );
            *ROOT_PARTITION.lock() = Some((0, 0));
            crate::println!("[usb] mounted device0 whole-device FAT32 as root '/'");
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Mount helpers (called from kernel_main and from the `usbscan` shell command)
// ---------------------------------------------------------------------------

/// Try to mount USB devices starting from `from_idx` as FAT32 volumes.
///
/// Device 0 → `/usb`, device 1 → `/usb1`, device 2 → `/usb2`, …
///
/// Safe to call while `USB_XHCI` is **not** locked; the function locks it
/// briefly only to read the device count, then releases it before doing any
/// FAT32 I/O (which re-acquires the lock per-sector).
pub fn mount_storage_devices(from_idx: usize) {
    let device_count = USB_XHCI
        .lock()
        .as_ref()
        .map(|x| x.devices.len())
        .unwrap_or(0);

    let root_part = *ROOT_PARTITION.lock();

    for dev_idx in from_idx..device_count {
        let mount_path = if dev_idx == 0 {
            alloc::string::String::from("/usb")
        } else {
            alloc::format!("/usb{}", dev_idx)
        };

        let mut mounted = false;

        // Try the whole device as a FAT32 volume (for raw, partition-less drives).
        // Skip if this device+offset is already the root partition.
        let skip_whole = root_part == Some((dev_idx, 0));
        if !skip_whole
            && let Some(fat32) =
                crate::fs::fat32::Fat32Fs::new(Box::new(XhciBlockDevice { dev_idx }))
        {
            let mut vfs = crate::vfs::VFS.lock();
            if let Some(vfs) = vfs.as_mut() {
                vfs.mount(&mount_path, Box::new(crate::vfs::Fat32Mount(fat32)));
                crate::println!("[usb] FAT32 volume mounted at {}", mount_path);
                mounted = true;
            }
        }

        // Try GPT partitions — skip EFI partitions and the root partition.
        if !mounted {
            for part in gpt_partitions_for_device(dev_idx) {
                if part.is_efi {
                    continue;
                }
                if root_part == Some((dev_idx, part.start_lba)) {
                    continue;
                }
                let pdev: Box<dyn BlockDevice> = Box::new(PartitionBlockDevice::new(
                    Box::new(XhciBlockDevice { dev_idx }),
                    part.start_lba,
                    part.sector_count,
                ));
                if let Some(fat32) = crate::fs::fat32::Fat32Fs::new(pdev) {
                    let mut vfs = crate::vfs::VFS.lock();
                    if let Some(vfs) = vfs.as_mut() {
                        vfs.mount(&mount_path, Box::new(crate::vfs::Fat32Mount(fat32)));
                        crate::println!(
                            "[usb] FAT32 partition lba{} mounted at {}",
                            part.start_lba,
                            mount_path
                        );
                        mounted = true;
                        break;
                    }
                }
            }
        }

        // Try MBR partitions if GPT found nothing.
        if !mounted {
            for part in mbr_partitions_for_device(dev_idx) {
                if root_part == Some((dev_idx, part.start_lba)) {
                    continue;
                }
                let pdev: Box<dyn BlockDevice> = Box::new(PartitionBlockDevice::new(
                    Box::new(XhciBlockDevice { dev_idx }),
                    part.start_lba,
                    part.sector_count,
                ));
                if let Some(fat32) = crate::fs::fat32::Fat32Fs::new(pdev) {
                    let mut vfs = crate::vfs::VFS.lock();
                    if let Some(vfs) = vfs.as_mut() {
                        vfs.mount(&mount_path, Box::new(crate::vfs::Fat32Mount(fat32)));
                        crate::println!(
                            "[usb] FAT32 MBR partition lba{} mounted at {}",
                            part.start_lba,
                            mount_path
                        );
                        mounted = true;
                        break;
                    }
                }
            }
        }

        if !mounted {
            crate::serial_println!("[usb] device {} has no FAT32 volume", dev_idx);
        }
    }
}

/// Scan all XHCI ports for newly connected devices, enumerate them, and
/// mount any FAT32 volumes found.
///
/// Returns the number of new storage devices that were successfully mounted.
/// Designed to be called from the `usbscan` shell command.
pub fn scan_and_mount() -> usize {
    // Phase 1: scan ports inside the XHCI lock, collect new device count.
    let before = USB_XHCI
        .lock()
        .as_ref()
        .map(|x| x.devices.len())
        .unwrap_or(0);
    {
        let mut xhci = USB_XHCI.lock();
        if let Some(x) = xhci.as_mut() {
            x.scan_new_ports();
        }
    } // USB_XHCI lock released here

    // Phase 2: mount any newly discovered devices (acquires USB_XHCI briefly
    // per-sector, never while VFS is held).
    let after = USB_XHCI
        .lock()
        .as_ref()
        .map(|x| x.devices.len())
        .unwrap_or(0);
    if after > before {
        mount_storage_devices(before);
    }
    after - before
}
