//! Disk filesystems and the mount dispatcher.

pub mod ext2;
pub mod fat;

use crate::block::cache::CachedDevice;
use crate::errno::*;
use alloc::string::String;
use alloc::sync::Arc;

/// Mount `source` at `target` as filesystem type `fstype` ("auto" probes).
/// A `,ro` suffix on the type (e.g. "ext2,ro") requests a read-only mount.
pub fn mount_by_type(fstype: &str, source: &str, target: &str) -> KResult<()> {
    let (fstype, ro) = match fstype.strip_suffix(",ro") {
        Some(t) => (t, true),
        None => (fstype, false),
    };
    match fstype {
        "tmpfs" | "ramfs" => crate::vfs::mount(target, crate::vfs::tmpfs::TmpFs::new(), "tmpfs"),
        "proc" => crate::vfs::mount(target, crate::vfs::procfs::ProcFs::new(), "proc"),
        "devtmpfs" | "devfs" => {
            crate::vfs::mount(target, crate::vfs::devfs::DevFs::new(), "devtmpfs")
        }
        _ => {
            let fs = probe_block_fs(fstype, source, ro)?;
            crate::vfs::mount(target, fs, source)
        }
    }
}

/// Identify the filesystem on a device: "vfat", "ext2", "ext3" or "ext4".
pub fn detect(dev: &CachedDevice) -> Option<&'static str> {
    if let Some((kind, _)) = ext2::probe(dev) {
        return Some(kind);
    }
    let mut bs = [0u8; 512];
    dev.read_bytes(0, &mut bs).ok()?;
    if bs[510] == 0x55 && bs[511] == 0xAA {
        let bps = u16::from_le_bytes([bs[11], bs[12]]);
        let spc = bs[13];
        let fats = bs[16];
        if matches!(bps, 512 | 1024 | 2048 | 4096)
            && spc != 0
            && spc.is_power_of_two()
            && (1..=4).contains(&fats)
        {
            return Some("vfat");
        }
    }
    None
}

/// The volume label of the filesystem on `dev`, if any.
pub fn volume_label(dev: &CachedDevice) -> Option<String> {
    match detect(dev)? {
        "vfat" => {
            let d = crate::block::find(&dev.name)?;
            let fs = fat::FatFs::open(d.dev.clone()).ok()?;
            Some(fs.label.clone()).filter(|l| !l.is_empty() && l != "NO NAME")
        }
        _ => ext2::probe(dev).map(|(_, l)| l).filter(|l| !l.is_empty()),
    }
}

fn probe_block_fs(
    fstype: &str,
    source: &str,
    ro: bool,
) -> KResult<Arc<dyn crate::vfs::FileSystem>> {
    let disk = crate::block::find(source).ok_or(ENODEV)?;
    let dev = disk.dev.clone();
    let kind = match fstype {
        "auto" => detect(&dev).ok_or(EINVAL)?,
        "vfat" | "fat" | "fat32" | "fat16" | "fat12" | "msdos" => "vfat",
        "ext2" | "ext3" | "ext4" => fstype,
        _ => return Err(ENODEV),
    };
    match kind {
        "vfat" => {
            if ro {
                return Err(EINVAL);
            }
            Ok(fat::FatFs::open(dev)?)
        }
        _ => Ok(ext2::Ext2Fs::open(dev, ro)?),
    }
}
