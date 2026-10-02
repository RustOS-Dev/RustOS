//! Block devices.
//!
//! Drivers (AHCI, NVMe, virtio-blk, USB mass storage) register whole disks
//! with [`register_disk`]. The block layer scans the partition table,
//! registers partitions, exposes everything under /dev (`sda`, `sda1`,
//! `nvme0n1p2`, `vda`, ...) and in /proc/partitions, and puts a write-back
//! block cache in front of each device for filesystems.

pub mod cache;
pub mod partition;

use crate::errno::*;
use crate::sync::Mutex;
use crate::vfs::{self, FileLike, FileType, Metadata};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};

pub trait BlockDevice: Send + Sync {
    /// Logical sector size in bytes (512 or 4096).
    fn sector_size(&self) -> usize;
    fn sector_count(&self) -> u64;
    /// Read whole sectors starting at `lba` into `buf` (a multiple of the
    /// sector size).
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> KResult<()>;
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> KResult<()>;
    fn flush(&self) -> KResult<()> {
        Ok(())
    }
    fn read_only(&self) -> bool {
        false
    }
    fn model(&self) -> String {
        String::from("disk")
    }
    fn size_bytes(&self) -> u64 {
        self.sector_count() * self.sector_size() as u64
    }
    /// Called when the device disappears (hot-unplug).
    fn removed(&self) -> bool {
        false
    }
}

/// A range of sectors of another device.
pub struct PartitionDev {
    pub parent: Arc<dyn BlockDevice>,
    pub start: u64,
    pub count: u64,
}

impl BlockDevice for PartitionDev {
    fn sector_size(&self) -> usize {
        self.parent.sector_size()
    }
    fn sector_count(&self) -> u64 {
        self.count
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> KResult<()> {
        let n = (buf.len() / self.sector_size()) as u64;
        if lba + n > self.count {
            return Err(EIO);
        }
        self.parent.read_sectors(self.start + lba, buf)
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> KResult<()> {
        let n = (buf.len() / self.sector_size()) as u64;
        if lba + n > self.count {
            return Err(EIO);
        }
        self.parent.write_sectors(self.start + lba, buf)
    }
    fn flush(&self) -> KResult<()> {
        self.parent.flush()
    }
    fn read_only(&self) -> bool {
        self.parent.read_only()
    }
    fn model(&self) -> String {
        self.parent.model()
    }
    fn removed(&self) -> bool {
        self.parent.removed()
    }
}

pub struct Disk {
    pub name: String,
    /// Cached view used by filesystems and /dev.
    pub dev: Arc<cache::CachedDevice>,
    pub parent: Option<String>,
    pub part: Option<partition::Partition>,
}

static DISKS: Mutex<Vec<Arc<Disk>>> = Mutex::new(Vec::new());
static SD_COUNT: AtomicU32 = AtomicU32::new(0);
static VD_COUNT: AtomicU32 = AtomicU32::new(0);
static NVME_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiskKind {
    /// SATA/SCSI/USB: sda, sdb, ... (partitions sda1)
    Scsi,
    /// virtio: vda, vdb, ...
    Virtio,
    /// NVMe namespace: nvme<ctrl>n<ns> (partitions nvme0n1p1)
    Nvme(u32),
    /// SD/MMC card: mmcblk<n> (partitions mmcblk0p1)
    Mmc(u32),
}

/// Allocate an NVMe controller index.
pub fn next_nvme_index() -> u32 {
    NVME_COUNT.fetch_add(1, Ordering::SeqCst)
}

fn letters(n: u32) -> String {
    let mut n = n as i64;
    let mut s = Vec::new();
    loop {
        s.push((b'a' + (n % 26) as u8) as char);
        n = n / 26 - 1;
        if n < 0 {
            break;
        }
    }
    s.iter().rev().collect()
}

/// Register a whole disk; scans partitions. Returns the disk name.
pub fn register_disk(dev: Arc<dyn BlockDevice>, kind: DiskKind, ns: u32) -> String {
    let name = match kind {
        DiskKind::Scsi => format!("sd{}", letters(SD_COUNT.fetch_add(1, Ordering::SeqCst))),
        DiskKind::Virtio => format!("vd{}", letters(VD_COUNT.fetch_add(1, Ordering::SeqCst))),
        DiskKind::Nvme(c) => format!("nvme{}n{}", c, ns),
        DiskKind::Mmc(n) => format!("mmcblk{}", n),
    };
    let cached = cache::CachedDevice::new(dev.clone(), &name);
    let disk = Arc::new(Disk {
        name: name.clone(),
        dev: cached.clone(),
        parent: None,
        part: None,
    });
    add(disk);
    crate::println!(
        "[block] {}: {} ({} MiB, {}-byte sectors){}",
        name,
        dev.model(),
        dev.size_bytes() >> 20,
        dev.sector_size(),
        if dev.read_only() { " read-only" } else { "" }
    );
    let parts = partition::scan(cached.as_ref());
    let sep = if name.ends_with(|c: char| c.is_ascii_digit()) {
        "p"
    } else {
        ""
    };
    for p in parts {
        let pname = format!("{}{}{}", name, sep, p.index);
        let pdev: Arc<dyn BlockDevice> = Arc::new(PartitionDev {
            parent: cached.clone(),
            start: p.start_lba,
            count: p.sector_count,
        });
        let pcached =
            cache::CachedDevice::wrap_partition(pdev, &pname, cached.clone(), p.start_lba);
        crate::println!(
            "[block]   {}: {} MiB {}{}",
            pname,
            (p.sector_count * dev.sector_size() as u64) >> 20,
            p.type_name,
            if p.label.is_empty() {
                String::new()
            } else {
                format!(" \"{}\"", p.label)
            }
        );
        add(Arc::new(Disk {
            name: pname,
            dev: pcached,
            parent: Some(name.clone()),
            part: Some(p),
        }));
    }
    name
}

fn add(d: Arc<Disk>) {
    vfs::devfs::register(
        &d.name,
        FileType::BlockDevice,
        0,
        Arc::new(BlockFile { disk: d.clone() }),
    );
    DISKS.lock().push(d);
}

/// Remove a disk and its partitions (hot-unplug).
pub fn unregister_disk(name: &str) {
    let mut disks = DISKS.lock();
    let gone: Vec<Arc<Disk>> = disks
        .iter()
        .filter(|d| d.name == name || d.parent.as_deref() == Some(name))
        .cloned()
        .collect();
    disks.retain(|d| d.name != name && d.parent.as_deref() != Some(name));
    drop(disks);
    for d in gone {
        vfs::devfs::unregister(&d.name);
        // Unmount filesystems on the vanished device.
        let dev_path = format!("/dev/{}", d.name);
        for (path, _, src) in vfs::mounts() {
            if src == dev_path {
                let _ = vfs::umount(&path);
                crate::println!("[block] {} removed; unmounted {}", d.name, path);
            }
        }
    }
}

pub fn disks() -> Vec<Arc<Disk>> {
    DISKS.lock().clone()
}

pub fn find(name: &str) -> Option<Arc<Disk>> {
    let name = name.trim_start_matches("/dev/");
    DISKS.lock().iter().find(|d| d.name == name).cloned()
}

/// Flush every cache and device.
pub fn sync_all() {
    for d in disks() {
        let _ = d.dev.sync();
    }
}

/// /dev node for a disk or partition: byte-addressed access via the cache.
struct BlockFile {
    disk: Arc<Disk>,
}

impl FileLike for BlockFile {
    fn read(&self, _b: &mut [u8], _nb: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn write(&self, _b: &[u8], _nb: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Option<KResult<usize>> {
        Some(self.disk.dev.read_bytes(off, buf))
    }
    fn write_at(&self, off: u64, buf: &[u8]) -> Option<KResult<usize>> {
        Some(self.disk.dev.write_bytes(off, buf))
    }
    fn size(&self) -> Option<u64> {
        Some(self.disk.dev.size_bytes())
    }
    fn stat(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::BlockDevice, 0o660);
        m.size = self.disk.dev.size_bytes();
        Ok(m)
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        const BLKGETSIZE64: u64 = 0x8008_1272;
        const BLKSSZGET: u64 = 0x1268;
        const BLKFLSBUF: u64 = 0x1261;
        const BLKRRPART: u64 = 0x125F;
        match cmd {
            BLKGETSIZE64 => {
                crate::process::uaccess::write_user(arg, &self.disk.dev.size_bytes())?;
                Ok(0)
            }
            BLKSSZGET => {
                crate::process::uaccess::write_user(arg, &(self.disk.dev.sector_size() as u32))?;
                Ok(0)
            }
            BLKFLSBUF => {
                self.disk.dev.sync()?;
                Ok(0)
            }
            BLKRRPART => Ok(0),
            _ => Err(ENOTTY),
        }
    }
    fn close(&self) {
        let _ = self.disk.dev.sync();
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn gen_partitions() -> String {
    let mut s = String::from("major minor  #blocks  name\n");
    for d in disks() {
        s.push_str(&format!(
            "{:5} {:5} {:9} {}\n",
            if d.name.starts_with("nvme") { 259 } else { 8 },
            d.part.as_ref().map_or(0, |p| p.index),
            d.dev.size_bytes() / 1024,
            d.name
        ));
    }
    s
}

/// Start the background flusher and register /proc/partitions.
pub fn init() {
    vfs::procfs::register("partitions", gen_partitions);
    crate::sched::spawn("blk-flush", || {
        loop {
            crate::time::sleep_ms(5000);
            crate::mm::pagecache::sync_all();
            // Filesystems commit their journals.
            crate::vfs::sync_all();
            for d in disks() {
                let _ = d.dev.sync();
            }
        }
    });
}

/// Mount every partition with a recognised filesystem: the RustOS storage
/// partition at /storage, anything else at /mnt/<name>.
pub fn automount() {
    for d in disks() {
        // Skip whole disks that carry a partition table.
        if d.parent.is_none()
            && disks()
                .iter()
                .any(|p| p.parent.as_deref() == Some(d.name.as_str()))
        {
            continue;
        }
        let dev_path = format!("/dev/{}", d.name);
        if vfs::mounts().iter().any(|(_, _, src)| *src == dev_path) {
            continue;
        }
        let Some(kind) = crate::fs::detect(d.dev.as_ref()) else {
            continue;
        };
        let is_esp = d.part.as_ref().is_some_and(|p| p.is_esp);
        let is_storage = d.part.as_ref().is_some_and(|p| p.label == "rustos-storage")
            || crate::fs::volume_label(d.dev.as_ref())
                .is_some_and(|l| l.eq_ignore_ascii_case("RUSTOS"));
        let target = if is_storage && !vfs::mounts().iter().any(|(p, _, _)| p == "/storage") {
            String::from("/storage")
        } else if is_esp {
            String::from("/boot/efi")
        } else {
            format!("/mnt/{}", d.name)
        };
        let _ = vfs::mkdir_p(&target);
        match crate::fs::mount_by_type(kind, &dev_path, &target) {
            Ok(()) => crate::println!("[block] mounted {} ({}) at {}", dev_path, kind, target),
            Err(e) => crate::println!("[block] {}: cannot mount {}: {}", dev_path, kind, e),
        }
    }
}

pub fn name_list() -> Vec<String> {
    disks().iter().map(|d| d.name.to_string()).collect()
}
