//! SD/MMC cards found by Linux's MMC core (src/linuxkpi/c/mmc.c) as RustOS
//! block devices: `mmcblk0`, with partitions `mmcblk0p1`, ...

use crate::block::{self, BlockDevice, DiskKind};
use crate::errno::*;
use crate::sync::Mutex;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use core::ffi::{CStr, c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

unsafe extern "C" {
    fn kpi_mmc_rw(d: *mut c_void, lba: u64, count: u32, buf: *mut u8, write: c_int) -> c_int;
}

struct MmcDisk {
    /// The C `struct kpi_mmc_disk`.
    d: usize,
    sectors: u64,
    model: String,
    read_only: bool,
    gone: AtomicBool,
}

impl BlockDevice for MmcDisk {
    fn sector_size(&self) -> usize {
        512
    }
    fn sector_count(&self) -> u64 {
        self.sectors
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> KResult<()> {
        self.rw(lba, buf.as_mut_ptr(), buf.len(), false)
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> KResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        self.rw(lba, buf.as_ptr() as *mut u8, buf.len(), true)
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    fn model(&self) -> String {
        self.model.clone()
    }
    fn removed(&self) -> bool {
        self.gone.load(Ordering::SeqCst)
    }
}

impl MmcDisk {
    fn rw(&self, lba: u64, buf: *mut u8, len: usize, write: bool) -> KResult<()> {
        if self.gone.load(Ordering::SeqCst) {
            return Err(ENODEV);
        }
        if !len.is_multiple_of(512) || lba + (len / 512) as u64 > self.sectors {
            return Err(EINVAL);
        }
        match unsafe {
            kpi_mmc_rw(
                self.d as *mut c_void,
                lba,
                (len / 512) as u32,
                buf,
                write as c_int,
            )
        } {
            0 => Ok(()),
            e if e < 0 => Err(Errno(-e)),
            _ => Err(EIO),
        }
    }
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static INDEX: AtomicU32 = AtomicU32::new(0);
static DISKS: Mutex<BTreeMap<u64, (String, Arc<MmcDisk>)>> = Mutex::new(BTreeMap::new());

/// A card was initialised: register its disk.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_mmc_disk_add(
    d: *mut c_void,
    sectors: u64,
    model: *const c_char,
    read_only: c_int,
) -> u64 {
    let disk = Arc::new(MmcDisk {
        d: d as usize,
        sectors,
        model: String::from(unsafe { CStr::from_ptr(model) }.to_string_lossy()),
        read_only: read_only != 0,
        gone: AtomicBool::new(false),
    });
    let n = INDEX.fetch_add(1, Ordering::SeqCst);
    let name = block::register_disk(disk.clone(), DiskKind::Mmc(n), 0);
    let h = NEXT.fetch_add(1, Ordering::SeqCst);
    DISKS.lock().insert(h, (name, disk));
    h
}

/// The card went away (ejected, host removed).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_mmc_disk_remove(handle: u64) {
    if let Some((name, disk)) = DISKS.lock().remove(&handle) {
        disk.gone.store(true, Ordering::SeqCst);
        block::unregister_disk(&name);
    }
}
