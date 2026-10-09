//! Minimal FAT formatter.
//!
//! Produces a FAT32 (or FAT16/FAT12 for small volumes) filesystem with an
//! empty root directory. Sector I/O goes through a caller-supplied closure
//! so the same code works on a host file, a kernel block device or a
//! userland `/dev` node.

#![no_std]

pub const SECTOR: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Clone, Copy, Debug)]
pub struct Options<'a> {
    /// Volume label (up to 11 ASCII characters).
    pub label: &'a str,
    /// Volume serial number.
    pub serial: u32,
    /// Force a FAT type; `None` picks by size.
    pub fat_type: Option<FatType>,
    /// Sectors before the volume (the partition's start LBA), for the BPB.
    pub hidden_sectors: u32,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            label: "NO NAME",
            serial: 0x1234_5678,
            fat_type: None,
            hidden_sectors: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub fat_type: FatType,
    pub sectors_per_cluster: u32,
    pub reserved: u32,
    pub fat_sectors: u32,
    pub root_entries: u32,
    pub clusters: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    TooSmall,
    TooLarge,
    Io(E),
}

/// Compute the geometry for a volume of `total` sectors.
pub fn layout(total: u64, forced: Option<FatType>) -> Option<Layout> {
    if total > u32::MAX as u64 || total < 64 {
        return None;
    }
    let mb = total * SECTOR as u64 / (1024 * 1024);
    let kind = forced.unwrap_or(if mb >= 512 {
        FatType::Fat32
    } else if mb >= 16 {
        FatType::Fat16
    } else {
        FatType::Fat12
    });
    let (reserved, root_entries) = match kind {
        FatType::Fat32 => (32u32, 0u32),
        _ => (1, 512),
    };
    let root_sectors = root_entries * 32 / SECTOR as u32;
    // Pick the cluster size like mkfs.fat does.
    let spc = match kind {
        FatType::Fat32 => match mb {
            0..=259 => 1,
            260..=8191 => 8,
            8192..=16383 => 16,
            16384..=32767 => 32,
            _ => 64,
        },
        FatType::Fat16 => match mb {
            0..=15 => 2,
            16..=127 => 4,
            128..=255 => 8,
            256..=511 => 16,
            512..=1023 => 32,
            _ => 64,
        },
        FatType::Fat12 => match mb {
            0..=1 => 1,
            2..=3 => 2,
            4..=7 => 4,
            _ => 8,
        },
    };
    let total = total as u32;
    let entry_bits: u64 = match kind {
        FatType::Fat12 => 12,
        FatType::Fat16 => 16,
        FatType::Fat32 => 32,
    };
    // Iterate to a fixed point: FAT size depends on the cluster count.
    let mut fat_sectors = 1u32;
    loop {
        let data = total.checked_sub(reserved + 2 * fat_sectors + root_sectors)?;
        let clusters = data / spc;
        let need_bytes = ((clusters as u64 + 2) * entry_bits).div_ceil(8);
        let need = need_bytes.div_ceil(SECTOR as u64) as u32;
        if need <= fat_sectors {
            let ok = match kind {
                FatType::Fat12 => clusters < 4085,
                FatType::Fat16 => (4085..65525).contains(&clusters),
                FatType::Fat32 => clusters >= 65525 && clusters < 0x0FFF_FFF5,
            };
            if !ok {
                return None;
            }
            return Some(Layout {
                fat_type: kind,
                sectors_per_cluster: spc,
                reserved,
                fat_sectors,
                root_entries,
                clusters,
            });
        }
        fat_sectors = need;
    }
}

fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// Format a volume of `total` sectors. `write(lba, sector)` writes one
/// 512-byte sector relative to the start of the volume.
pub fn format<E>(
    total: u64,
    opts: &Options,
    mut write: impl FnMut(u64, &[u8; SECTOR]) -> Result<(), E>,
) -> Result<Layout, Error<E>> {
    let l = layout(total, opts.fat_type).ok_or(if total < 64 {
        Error::TooSmall
    } else {
        Error::TooLarge
    })?;
    let mut w = |lba: u64, s: &[u8; SECTOR]| write(lba, s).map_err(Error::Io);
    let mut label = [b' '; 11];
    for (i, c) in opts.label.bytes().take(11).enumerate() {
        label[i] = c.to_ascii_uppercase();
    }

    let mut bs = [0u8; SECTOR];
    bs[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    bs[3..11].copy_from_slice(b"RUSTOS  ");
    put16(&mut bs, 11, SECTOR as u16);
    bs[13] = l.sectors_per_cluster as u8;
    put16(&mut bs, 14, l.reserved as u16);
    bs[16] = 2;
    put16(&mut bs, 17, l.root_entries as u16);
    if total < 65536 && l.fat_type != FatType::Fat32 {
        put16(&mut bs, 19, total as u16);
    } else {
        put32(&mut bs, 32, total as u32);
    }
    bs[21] = 0xF8;
    put16(&mut bs, 24, 63);
    put16(&mut bs, 26, 255);
    put32(&mut bs, 28, opts.hidden_sectors);
    let ebr = if l.fat_type == FatType::Fat32 {
        put32(&mut bs, 36, l.fat_sectors);
        put32(&mut bs, 44, 2); // root cluster
        put16(&mut bs, 48, 1); // FSInfo
        put16(&mut bs, 50, 6); // backup boot sector
        64
    } else {
        put16(&mut bs, 22, l.fat_sectors as u16);
        36
    };
    bs[ebr] = 0x80; // drive number
    bs[ebr + 2] = 0x29; // extended boot signature
    put32(&mut bs, ebr + 3, opts.serial);
    bs[ebr + 7..ebr + 18].copy_from_slice(&label);
    bs[ebr + 18..ebr + 26].copy_from_slice(match l.fat_type {
        FatType::Fat12 => b"FAT12   ",
        FatType::Fat16 => b"FAT16   ",
        FatType::Fat32 => b"FAT32   ",
    });
    bs[510] = 0x55;
    bs[511] = 0xAA;

    let zero = [0u8; SECTOR];
    // Clear reserved area, FATs and root directory first.
    let root_sectors = l.root_entries * 32 / SECTOR as u32;
    let meta_end = l.reserved + 2 * l.fat_sectors + root_sectors;
    for s in 0..meta_end as u64 {
        w(s, &zero)?;
    }
    w(0, &bs)?;

    if l.fat_type == FatType::Fat32 {
        let mut fsinfo = [0u8; SECTOR];
        put32(&mut fsinfo, 0, 0x4161_5252);
        put32(&mut fsinfo, 484, 0x6141_7272);
        put32(&mut fsinfo, 488, l.clusters - 1); // root uses one
        put32(&mut fsinfo, 492, 3);
        put32(&mut fsinfo, 508, 0xAA55_0000);
        w(1, &fsinfo)?;
        w(6, &bs)?;
        w(7, &fsinfo)?;
        // Zero the root directory cluster, then put the volume label in it.
        let data_start = (l.reserved + 2 * l.fat_sectors) as u64;
        for s in 0..l.sectors_per_cluster as u64 {
            w(data_start + s, &zero)?;
        }
        let mut root = [0u8; SECTOR];
        root[0..11].copy_from_slice(&label);
        root[11] = 0x08;
        w(data_start, &root)?;
    } else {
        let mut root = [0u8; SECTOR];
        root[0..11].copy_from_slice(&label);
        root[11] = 0x08;
        w((l.reserved + 2 * l.fat_sectors) as u64, &root)?;
    }

    // First FAT sector of each copy: media byte, EOC for cluster 1 (and the
    // root cluster on FAT32).
    let mut fat0 = [0u8; SECTOR];
    match l.fat_type {
        FatType::Fat12 => fat0[0..3].copy_from_slice(&[0xF8, 0xFF, 0xFF]),
        FatType::Fat16 => fat0[0..4].copy_from_slice(&[0xF8, 0xFF, 0xFF, 0xFF]),
        FatType::Fat32 => {
            put32(&mut fat0, 0, 0x0FFF_FFF8);
            put32(&mut fat0, 4, 0x0FFF_FFFF);
            put32(&mut fat0, 8, 0x0FFF_FFFF);
        }
    }
    for copy in 0..2u64 {
        w(l.reserved as u64 + copy * l.fat_sectors as u64, &fat0)?;
    }
    Ok(l)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;

    #[test]
    fn layouts() {
        let l = layout(512 * 2048, None).unwrap();
        assert_eq!(l.fat_type, FatType::Fat32);
        let l = layout(64 * 2048, None).unwrap();
        assert_eq!(l.fat_type, FatType::Fat16);
        let l = layout(4 * 2048, None).unwrap();
        assert_eq!(l.fat_type, FatType::Fat12);
        assert!(layout(10, None).is_none());
    }

    #[test]
    fn format_fat32() {
        let total = 600 * 2048u64;
        let mut img = vec![0u8; total as usize * SECTOR];
        let l = format::<()>(
            total,
            &Options {
                label: "rustos",
                ..Default::default()
            },
            |lba, s| {
                let o = lba as usize * SECTOR;
                img[o..o + SECTOR].copy_from_slice(s);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(&img[510..512], &[0x55, 0xAA]);
        assert_eq!(&img[71..82], b"RUSTOS     ");
        let fat = l.reserved as usize * SECTOR;
        assert_eq!(&img[fat + 8..fat + 12], &0x0FFF_FFFFu32.to_le_bytes());
    }
}
