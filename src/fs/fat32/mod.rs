//! FAT32 read-only filesystem driver.
//!
//! Supports:
//!  - FAT32 BPB (BIOS Parameter Block) parsing
//!  - Directory traversal (8.3 short names + LFN long file names)
//!  - Cluster chain following via the FAT
//!  - File and directory content reading
//!
//! Entry point: `Fat32Fs::new(block_dev)` where `block_dev` implements
//! `crate::usb::BlockDevice`.

use crate::usb::BlockDevice;
use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};

// ---------------------------------------------------------------------------
// BPB (BIOS Parameter Block) — first 512 bytes of the volume
// ---------------------------------------------------------------------------

#[repr(C, packed)]
#[allow(dead_code)]
struct Bpb {
    jump_boot: [u8; 3],
    oem_name: [u8; 8],
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    num_fats: u8,
    root_entry_count: u16, // 0 for FAT32
    total_sectors_16: u16, // 0 for FAT32
    media: u8,
    fat_size_16: u16, // 0 for FAT32
    sectors_per_track: u16,
    num_heads: u16,
    hidden_sectors: u32,
    total_sectors_32: u32,
    // FAT32 extended BPB
    fat_size_32: u32,
    ext_flags: u16,
    fs_version: u16,
    root_cluster: u32,
    fs_info: u16,
    backup_boot_sec: u16,
    _reserved: [u8; 12],
    drive_number: u8,
    _reserved2: u8,
    boot_signature: u8,
    volume_id: u32,
    volume_label: [u8; 11],
    fs_type: [u8; 8],
}

// ---------------------------------------------------------------------------
// Directory Entry (32 bytes)
// ---------------------------------------------------------------------------

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct DirEntry32 {
    name: [u8; 11],
    attributes: u8,
    _nt_res: u8,
    _crt_time_tenth: u8,
    _crt_time: u16,
    _crt_date: u16,
    _lst_acc_date: u16,
    fst_clus_hi: u16,
    _wrt_time: u16,
    _wrt_date: u16,
    fst_clus_lo: u16,
    file_size: u32,
}

const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
#[allow(dead_code)]
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LONG_NAME: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

// ---------------------------------------------------------------------------
// LFN (Long File Name) entry
// ---------------------------------------------------------------------------

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct LfnEntry {
    order: u8,
    name1: [u16; 5],
    attributes: u8,
    lfn_type: u8,
    checksum: u8,
    name2: [u16; 6],
    fst_clus: u16,
    name3: [u16; 2],
}

// ---------------------------------------------------------------------------
// Public directory entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FatDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u32,
    pub cluster: u32,
}

// ---------------------------------------------------------------------------
// FAT32 Filesystem
// ---------------------------------------------------------------------------

pub struct Fat32Fs {
    dev: Box<dyn BlockDevice>,
    bytes_per_sector: u32,
    secs_per_cluster: u32,
    num_fats: u32,
    fat_size_sectors: u32,
    fat_start_sector: u64,
    data_start_sector: u64,
    root_cluster: u32,
}

impl Fat32Fs {
    /// Mount a FAT32 filesystem from a block device.
    /// Reads sector 0 and parses the BPB.  Returns `None` if not FAT32.
    pub fn new(mut dev: Box<dyn BlockDevice>) -> Option<Self> {
        let boot = dev.read_sectors(0, 1)?;
        if boot.len() < 512 {
            return None;
        }
        let bpb = unsafe { &*(boot.as_ptr() as *const Bpb) };

        let bps = u16::from_le(bpb.bytes_per_sector) as u32;
        let spc = bpb.sectors_per_cluster as u32;
        let rsvd = u16::from_le(bpb.reserved_sectors) as u64;
        let nfat = bpb.num_fats as u64;
        let fat_sz = u32::from_le(bpb.fat_size_32) as u64;
        let root_clus = u32::from_le(bpb.root_cluster);
        let fat_start = rsvd;
        let data_start = rsvd + nfat * fat_sz;

        // Validate FAT32 signature
        if &bpb.fs_type[..5] != b"FAT32" {
            // Fallback: check if FAT size 16 is 0 (FAT32 indicator)
            if u16::from_le(bpb.fat_size_16) != 0 {
                return None;
            }
        }

        Some(Fat32Fs {
            dev,
            bytes_per_sector: bps,
            secs_per_cluster: spc,
            num_fats: nfat as u32,
            fat_size_sectors: fat_sz as u32,
            fat_start_sector: fat_start,
            data_start_sector: data_start,
            root_cluster: root_clus,
        })
    }

    // -----------------------------------------------------------------------
    // Cluster <-> sector conversion
    // -----------------------------------------------------------------------

    fn cluster_to_sector(&self, cluster: u32) -> u64 {
        self.data_start_sector + (cluster as u64 - 2) * self.secs_per_cluster as u64
    }

    fn sectors_per_cluster(&self) -> u16 {
        self.secs_per_cluster as u16
    }

    fn bytes_per_cluster(&self) -> usize {
        (self.bytes_per_sector * self.secs_per_cluster) as usize
    }

    // -----------------------------------------------------------------------
    // FAT chain traversal
    // -----------------------------------------------------------------------

    fn next_cluster(&mut self, cluster: u32) -> Option<u32> {
        self.read_fat_entry(cluster)
            .filter(|&entry| entry < 0x0FFF_FFF8)
    }

    fn read_fat_entry(&mut self, cluster: u32) -> Option<u32> {
        // Each FAT32 entry is 4 bytes
        let fat_entries_per_sector = self.bytes_per_sector / 4;
        let sector_off = cluster / fat_entries_per_sector;
        let entry_off = (cluster % fat_entries_per_sector) as usize;

        let sector = self.fat_start_sector + sector_off as u64;
        let data = self.dev.read_sectors(sector, 1)?;

        Some(
            u32::from_le_bytes([
                data[entry_off * 4],
                data[entry_off * 4 + 1],
                data[entry_off * 4 + 2],
                data[entry_off * 4 + 3],
            ]) & 0x0FFF_FFFF,
        )
    }

    fn write_fat_entry(&mut self, cluster: u32, value: u32) -> Option<()> {
        let fat_entries_per_sector = self.bytes_per_sector / 4;
        let sector_off = cluster / fat_entries_per_sector;
        let entry_off = (cluster % fat_entries_per_sector) as usize;
        let bytes = (value & 0x0FFF_FFFF).to_le_bytes();

        for fat_idx in 0..self.num_fats {
            let sector = self.fat_start_sector
                + fat_idx as u64 * self.fat_size_sectors as u64
                + sector_off as u64;
            let mut data = self.dev.read_sectors(sector, 1)?;
            data[entry_off * 4..entry_off * 4 + 4].copy_from_slice(&bytes);
            self.dev.write_sectors(sector, &data)?;
        }
        Some(())
    }

    fn cluster_chain(&mut self, start: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut cur = start;
        loop {
            if !(2..0x0FFF_FFF8).contains(&cur) {
                break;
            }
            chain.push(cur);
            match self.next_cluster(cur) {
                Some(next) if next >= 2 => cur = next,
                _ => break,
            }
            if chain.len() > 65536 {
                break;
            } // safety limit
        }
        chain
    }

    // -----------------------------------------------------------------------
    // Read all sectors of a cluster chain into a byte buffer
    // -----------------------------------------------------------------------

    fn read_chain(&mut self, start_cluster: u32) -> Vec<u8> {
        if start_cluster < 2 {
            return Vec::new();
        }
        let chain = self.cluster_chain(start_cluster);
        let mut buf = Vec::new();
        for cluster in chain {
            let sector = self.cluster_to_sector(cluster);
            if let Some(data) = self.dev.read_sectors(sector, self.sectors_per_cluster()) {
                buf.extend_from_slice(&data);
            }
        }
        buf
    }

    fn write_cluster(&mut self, cluster: u32, data: &[u8]) -> Option<()> {
        if data.len() != self.bytes_per_cluster() {
            return None;
        }
        self.dev
            .write_sectors(self.cluster_to_sector(cluster), data)
    }

    fn max_cluster(&self) -> u32 {
        let data_sectors = self
            .dev
            .sector_count()
            .saturating_sub(self.data_start_sector);
        (data_sectors / self.secs_per_cluster as u64) as u32 + 2
    }

    fn find_free_cluster(&mut self) -> Option<u32> {
        for cluster in 2..self.max_cluster() {
            if self.read_fat_entry(cluster)? == 0 {
                return Some(cluster);
            }
        }
        None
    }

    fn allocate_chain(&mut self, count: usize) -> Option<Vec<u32>> {
        let mut clusters = Vec::new();
        for _ in 0..count {
            let cluster = self.find_free_cluster()?;
            self.write_fat_entry(cluster, 0x0FFF_FFFF)?;
            clusters.push(cluster);
        }
        for pair in clusters.windows(2) {
            self.write_fat_entry(pair[0], pair[1])?;
        }
        let zero = alloc::vec![0u8; self.bytes_per_cluster()];
        for &cluster in &clusters {
            self.write_cluster(cluster, &zero)?;
        }
        Some(clusters)
    }

    fn free_chain(&mut self, start_cluster: u32) -> Option<()> {
        if start_cluster < 2 {
            return Some(());
        }
        let chain = self.cluster_chain(start_cluster);
        for cluster in chain {
            self.write_fat_entry(cluster, 0)?;
        }
        Some(())
    }

    // -----------------------------------------------------------------------
    // Directory listing
    // -----------------------------------------------------------------------

    pub fn list_dir(&mut self, cluster: u32) -> Vec<FatDirEntry> {
        let data = self.read_chain(cluster);
        let mut entries = Vec::new();
        let mut lfn_buf: Vec<(u8, Vec<u16>)> = Vec::new(); // (order, chars)

        let n = data.len() / 32;
        for i in 0..n {
            let off = i * 32;
            if off + 32 > data.len() {
                break;
            }
            let raw = &data[off..off + 32];
            let first = raw[0];

            if first == 0x00 {
                break;
            } // end of directory
            if first == 0xE5 {
                lfn_buf.clear();
                continue;
            } // deleted

            let attr = raw[11];

            if attr == ATTR_LONG_NAME {
                // LFN entry
                let lfn = unsafe { &*(raw.as_ptr() as *const LfnEntry) };
                let mut chars: Vec<u16> = Vec::new();
                let n1 = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(lfn.name1)) };
                let n2 = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(lfn.name2)) };
                let n3 = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!(lfn.name3)) };
                for c in n1 {
                    if c != 0xFFFF && c != 0 {
                        chars.push(u16::from_le(c));
                    }
                }
                for c in n2 {
                    if c != 0xFFFF && c != 0 {
                        chars.push(u16::from_le(c));
                    }
                }
                for c in n3 {
                    if c != 0xFFFF && c != 0 {
                        chars.push(u16::from_le(c));
                    }
                }
                lfn_buf.push((lfn.order & 0x1F, chars));
                continue;
            }

            if attr & (ATTR_VOLUME_ID | ATTR_SYSTEM) != 0 {
                lfn_buf.clear();
                continue;
            }

            let dir_entry = unsafe { &*(raw.as_ptr() as *const DirEntry32) };
            let cluster_hi = u16::from_le(dir_entry.fst_clus_hi) as u32;
            let cluster_lo = u16::from_le(dir_entry.fst_clus_lo) as u32;
            let cluster = (cluster_hi << 16) | cluster_lo;
            let file_size = u32::from_le(dir_entry.file_size);
            let is_dir = attr & ATTR_DIRECTORY != 0;

            let name = if !lfn_buf.is_empty() {
                // Reassemble LFN: sort by order ascending, concatenate
                lfn_buf.sort_by_key(|(o, _)| *o);
                let chars: Vec<u16> = lfn_buf
                    .iter()
                    .flat_map(|(_, c)| c.iter().cloned())
                    .collect();
                String::from_utf16_lossy(&chars).to_string()
            } else {
                parse_83_name(&dir_entry.name)
            };

            lfn_buf.clear();

            if name == "." || name == ".." {
                continue;
            }

            entries.push(FatDirEntry {
                name,
                is_dir,
                size: file_size,
                cluster,
            });
        }
        entries
    }

    // -----------------------------------------------------------------------
    // Path resolution
    // -----------------------------------------------------------------------

    /// Resolve a path (e.g. "/dir/subdir/file.txt") to its directory entry.
    /// Returns `None` if not found.
    pub fn lookup(&mut self, path: &str) -> Option<FatDirEntry> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut cluster = self.root_cluster;

        for (i, part) in parts.iter().enumerate() {
            let entries = self.list_dir(cluster);
            let found = entries
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(part))?;
            if i == parts.len() - 1 {
                return Some(found);
            }
            if !found.is_dir {
                return None;
            }
            cluster = found.cluster;
        }
        // path was "/" → return a synthetic root entry
        Some(FatDirEntry {
            name: String::from("/"),
            is_dir: true,
            size: 0,
            cluster: self.root_cluster,
        })
    }

    /// List directory at `path`.
    pub fn list(&mut self, path: &str) -> Option<Vec<FatDirEntry>> {
        let cluster = if path == "/" || path.is_empty() {
            self.root_cluster
        } else {
            let e = self.lookup(path)?;
            if !e.is_dir {
                return None;
            }
            e.cluster
        };
        Some(self.list_dir(cluster))
    }

    /// Read file contents at `path`.
    pub fn read_file(&mut self, path: &str) -> Option<Vec<u8>> {
        let entry = self.lookup(path)?;
        if entry.is_dir {
            return None;
        }
        let mut data = self.read_chain(entry.cluster);
        data.truncate(entry.size as usize);
        Some(data)
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Option<()> {
        let (parent_path, file_name) = split_parent(path)?;
        let parent = if parent_path == "/" {
            FatDirEntry {
                name: String::from("/"),
                is_dir: true,
                size: 0,
                cluster: self.root_cluster,
            }
        } else {
            self.lookup(&parent_path)?
        };
        if !parent.is_dir {
            return None;
        }
        let short_name = make_short_name(&file_name)?;
        let existing = self.find_dir_entry(parent.cluster, &short_name, &file_name);
        if let Some((_, entry)) = existing.as_ref()
            && entry.is_dir
        {
            crate::serial_println!("[fat32] refusing to overwrite directory '{}'", path);
            return None;
        }

        let bytes_per_cluster = self.bytes_per_cluster();
        let clusters = if data.is_empty() {
            Vec::new()
        } else {
            self.allocate_chain(data.len().div_ceil(bytes_per_cluster))?
        };

        for (i, &cluster) in clusters.iter().enumerate() {
            let start = i * bytes_per_cluster;
            let end = usize::min(start + bytes_per_cluster, data.len());
            let mut cluster_data = alloc::vec![0u8; bytes_per_cluster];
            if start < end {
                cluster_data[..end - start].copy_from_slice(&data[start..end]);
            }
            self.write_cluster(cluster, &cluster_data)?;
        }

        let dir_index = match existing {
            Some((idx, entry)) => {
                self.free_chain(entry.cluster)?;
                idx
            }
            None => self.find_or_expand_dir(parent.cluster)?,
        };

        let first_cluster = clusters.first().copied().unwrap_or(0);
        let dir_entry = make_dir_entry(short_name, first_cluster, data.len() as u32);
        self.write_dir_entry(parent.cluster, dir_index, &dir_entry)
    }

    fn find_dir_entry(
        &mut self,
        dir_cluster: u32,
        short_name: &[u8; 11],
        name: &str,
    ) -> Option<(usize, FatDirEntry)> {
        let data = self.read_chain(dir_cluster);
        for (idx, raw) in data.as_chunks::<32>().0.iter().enumerate() {
            if raw[0] == 0x00 {
                break;
            }
            if raw[0] == 0xE5 || raw[11] == ATTR_LONG_NAME {
                continue;
            }
            let dir_entry = unsafe { &*(raw.as_ptr() as *const DirEntry32) };
            if &dir_entry.name != short_name {
                continue;
            }
            let cluster_hi = u16::from_le(dir_entry.fst_clus_hi) as u32;
            let cluster_lo = u16::from_le(dir_entry.fst_clus_lo) as u32;
            return Some((
                idx,
                FatDirEntry {
                    name: name.to_string(),
                    is_dir: raw[11] & ATTR_DIRECTORY != 0,
                    size: u32::from_le(dir_entry.file_size),
                    cluster: (cluster_hi << 16) | cluster_lo,
                },
            ));
        }
        None
    }

    /// Find a free 32-byte directory slot in the given directory cluster chain.
    /// If all existing slots are occupied, allocates a new cluster, links it into
    /// the chain, and returns the index of its first (now free) entry.
    fn find_or_expand_dir(&mut self, dir_cluster: u32) -> Option<usize> {
        let data = self.read_chain(dir_cluster);
        for (idx, raw) in data.as_chunks::<32>().0.iter().enumerate() {
            if raw[0] == 0x00 || raw[0] == 0xE5 {
                return Some(idx);
            }
        }
        // Directory cluster chain is full — allocate and link a new cluster.
        let chain = self.cluster_chain(dir_cluster);
        let last_cluster = *chain.last()?;
        let new_cluster = self.find_free_cluster()?;
        // Mark new cluster as EOF, then link last → new.
        self.write_fat_entry(new_cluster, 0x0FFF_FFFF)?;
        self.write_fat_entry(last_cluster, new_cluster)?;
        // Zero the new cluster so all entries are treated as free.
        let bpc = self.bytes_per_cluster();
        let zero = alloc::vec![0u8; bpc];
        self.write_cluster(new_cluster, &zero)?;
        // The first slot in the new cluster.
        Some(data.len() / 32)
    }

    fn write_dir_entry(
        &mut self,
        dir_cluster: u32,
        entry_index: usize,
        entry: &DirEntry32,
    ) -> Option<()> {
        let byte_offset = entry_index * 32;
        let cluster_index = byte_offset / self.bytes_per_cluster();
        let offset_in_cluster = byte_offset % self.bytes_per_cluster();
        let chain = self.cluster_chain(dir_cluster);
        let cluster = *chain.get(cluster_index)?;
        let sector = self.cluster_to_sector(cluster)
            + (offset_in_cluster / self.bytes_per_sector as usize) as u64;
        let offset_in_sector = offset_in_cluster % self.bytes_per_sector as usize;
        let mut sector_data = self.dev.read_sectors(sector, 1)?;
        sector_data[offset_in_sector..offset_in_sector + 32]
            .copy_from_slice(&dir_entry_bytes(entry));
        self.dev.write_sectors(sector, &sector_data)
    }

    /// Mark a directory entry as deleted (first byte = 0xE5) and free its cluster chain.
    fn delete_dir_entry(&mut self, dir_cluster: u32, entry_index: usize) -> Option<()> {
        let byte_offset = entry_index * 32;
        let cluster_index = byte_offset / self.bytes_per_cluster();
        let offset_in_cluster = byte_offset % self.bytes_per_cluster();
        let chain = self.cluster_chain(dir_cluster);
        let cluster = *chain.get(cluster_index)?;
        let sector = self.cluster_to_sector(cluster)
            + (offset_in_cluster / self.bytes_per_sector as usize) as u64;
        let offset_in_sector = offset_in_cluster % self.bytes_per_sector as usize;
        let mut sector_data = self.dev.read_sectors(sector, 1)?;
        sector_data[offset_in_sector] = 0xE5; // mark deleted
        self.dev.write_sectors(sector, &sector_data)
    }

    /// Create a directory at `path`.
    pub fn mkdir(&mut self, path: &str) -> Option<()> {
        let (parent_path, dir_name) = split_parent(path)?;
        let parent = if parent_path == "/" {
            FatDirEntry {
                name: String::from("/"),
                is_dir: true,
                size: 0,
                cluster: self.root_cluster,
            }
        } else {
            self.lookup(&parent_path)?
        };
        if !parent.is_dir {
            return None;
        }
        let short_name = make_short_name(&dir_name)?;
        // Don't create if already exists
        if self
            .find_dir_entry(parent.cluster, &short_name, &dir_name)
            .is_some()
        {
            return Some(());
        }
        // Allocate one cluster for the new directory
        let clusters = self.allocate_chain(1)?;
        let new_cluster = clusters[0];
        // Write . and .. entries into the new cluster
        let bpc = self.bytes_per_cluster();
        let mut dir_data = alloc::vec![0u8; bpc];
        // "." entry
        let dot = make_dir_entry_raw(*b".          ", new_cluster, 0, ATTR_DIRECTORY);
        dir_data[0..32].copy_from_slice(&dir_entry_bytes(&dot));
        // ".." entry
        let dotdot_cluster = parent.cluster;
        let dotdot = make_dir_entry_raw(*b"..         ", dotdot_cluster, 0, ATTR_DIRECTORY);
        dir_data[32..64].copy_from_slice(&dir_entry_bytes(&dotdot));
        self.write_cluster(new_cluster, &dir_data)?;
        // Add entry to parent directory
        let dir_index = self.find_or_expand_dir(parent.cluster)?;
        let entry = make_dir_entry_raw(short_name, new_cluster, 0, ATTR_DIRECTORY);
        self.write_dir_entry(parent.cluster, dir_index, &entry)
    }

    /// Remove a file or empty directory at `path`.
    pub fn remove(&mut self, path: &str) -> Option<()> {
        let (parent_path, name) = split_parent(path)?;
        let parent = if parent_path == "/" {
            FatDirEntry {
                name: String::from("/"),
                is_dir: true,
                size: 0,
                cluster: self.root_cluster,
            }
        } else {
            self.lookup(&parent_path)?
        };
        let short_name = make_short_name(&name)?;
        let (idx, entry) = self.find_dir_entry(parent.cluster, &short_name, &name)?;
        if entry.is_dir {
            // Only remove if empty
            let contents = self.list_dir(entry.cluster);
            if !contents.is_empty() {
                return None; // directory not empty
            }
        }
        if entry.cluster >= 2 {
            self.free_chain(entry.cluster)?;
        }
        self.delete_dir_entry(parent.cluster, idx)
    }

    /// Rename / move `src` to `dst`.
    pub fn rename(&mut self, src: &str, dst: &str) -> Option<()> {
        // Read source entry data
        let data = if !self.lookup(src)?.is_dir {
            self.read_file(src)?
        } else {
            // For directories: create new dir, move children (not implemented deeply —
            // simple case: just move the dir entry cluster reference).
            alloc::vec![]
        };
        let src_entry = self.lookup(src)?;
        if src_entry.is_dir {
            // Re-home the directory cluster: create entry at dst pointing to same cluster,
            // then remove old entry.
            let (dst_parent_path, dst_name) = split_parent(dst)?;
            let dst_parent = if dst_parent_path == "/" {
                FatDirEntry {
                    name: String::from("/"),
                    is_dir: true,
                    size: 0,
                    cluster: self.root_cluster,
                }
            } else {
                self.lookup(&dst_parent_path)?
            };
            let short_name = make_short_name(&dst_name)?;
            let dir_index = self.find_or_expand_dir(dst_parent.cluster)?;
            let entry = make_dir_entry_raw(short_name, src_entry.cluster, 0, ATTR_DIRECTORY);
            self.write_dir_entry(dst_parent.cluster, dir_index, &entry)?;
            // Remove old entry without freeing the cluster chain
            let (src_parent_path, src_name) = split_parent(src)?;
            let src_parent = if src_parent_path == "/" {
                FatDirEntry {
                    name: String::from("/"),
                    is_dir: true,
                    size: 0,
                    cluster: self.root_cluster,
                }
            } else {
                self.lookup(&src_parent_path)?
            };
            let src_short = make_short_name(&src_name)?;
            let (src_idx, _) = self.find_dir_entry(src_parent.cluster, &src_short, &src_name)?;
            self.delete_dir_entry(src_parent.cluster, src_idx)
        } else {
            self.write_file(dst, &data)?;
            self.remove(src)
        }
    }
}

// ---------------------------------------------------------------------------
// 8.3 short name parsing
// ---------------------------------------------------------------------------

fn parse_83_name(raw: &[u8; 11]) -> String {
    let base: Vec<u8> = raw[..8]
        .iter()
        .copied()
        .take_while(|&b| b != b' ')
        .collect();
    let ext: Vec<u8> = raw[8..11]
        .iter()
        .copied()
        .take_while(|&b| b != b' ')
        .collect();
    let name = String::from_utf8_lossy(&base).trim().to_string();
    if ext.is_empty() {
        name
    } else {
        alloc::format!("{}.{}", name, String::from_utf8_lossy(&ext).trim())
    }
}

fn split_parent(path: &str) -> Option<(String, String)> {
    let normalized = crate::vfs::RamFs::pub_normalize(path);
    if normalized == "/" {
        return None;
    }
    let pos = normalized.rfind('/')?;
    let parent = if pos == 0 {
        String::from("/")
    } else {
        normalized[..pos].to_string()
    };
    let name = normalized[pos + 1..].to_string();
    if name.is_empty() {
        None
    } else {
        Some((parent, name))
    }
}

fn make_short_name(name: &str) -> Option<[u8; 11]> {
    let mut out = [b' '; 11];
    let mut parts = name.split('.');
    let base = parts.next()?;
    let ext = parts.next();
    if parts.next().is_some() || base.is_empty() || base.len() > 8 {
        return None;
    }
    if let Some(ext) = ext
        && ext.len() > 3
    {
        return None;
    }
    for (idx, byte) in base.bytes().enumerate() {
        out[idx] = short_name_byte(byte)?;
    }
    if let Some(ext) = ext {
        for (idx, byte) in ext.bytes().enumerate() {
            out[8 + idx] = short_name_byte(byte)?;
        }
    }
    Some(out)
}

fn short_name_byte(byte: u8) -> Option<u8> {
    match byte {
        b'a'..=b'z' => Some(byte - 32),
        b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' => Some(byte),
        _ => None,
    }
}

fn make_dir_entry(name: [u8; 11], cluster: u32, size: u32) -> DirEntry32 {
    make_dir_entry_raw(name, cluster, size, ATTR_ARCHIVE)
}

fn make_dir_entry_raw(name: [u8; 11], cluster: u32, size: u32, attributes: u8) -> DirEntry32 {
    DirEntry32 {
        name,
        attributes,
        _nt_res: 0,
        _crt_time_tenth: 0,
        _crt_time: 0,
        _crt_date: 0,
        _lst_acc_date: 0,
        fst_clus_hi: ((cluster >> 16) as u16).to_le(),
        _wrt_time: 0,
        _wrt_date: 0,
        fst_clus_lo: (cluster as u16).to_le(),
        file_size: size.to_le(),
    }
}

fn dir_entry_bytes(entry: &DirEntry32) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0..11].copy_from_slice(&entry.name);
    out[11] = entry.attributes;
    out[12] = entry._nt_res;
    out[13] = entry._crt_time_tenth;
    out[14..16].copy_from_slice(&entry._crt_time.to_le_bytes());
    out[16..18].copy_from_slice(&entry._crt_date.to_le_bytes());
    out[18..20].copy_from_slice(&entry._lst_acc_date.to_le_bytes());
    out[20..22].copy_from_slice(&entry.fst_clus_hi.to_le_bytes());
    out[22..24].copy_from_slice(&entry._wrt_time.to_le_bytes());
    out[24..26].copy_from_slice(&entry._wrt_date.to_le_bytes());
    out[26..28].copy_from_slice(&entry.fst_clus_lo.to_le_bytes());
    out[28..32].copy_from_slice(&entry.file_size.to_le_bytes());
    out
}
