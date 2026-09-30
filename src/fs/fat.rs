//! FAT12/FAT16/FAT32 with VFAT long file names (read and write).
//!
//! Files are identified by the location of their directory entry; live
//! inodes are tracked so renames and size changes stay consistent for open
//! files. All metadata writes go through the block cache.

use crate::block::cache::CachedDevice;
use crate::errno::*;
use crate::sched::mutex::Mutex;
use crate::vfs::{DirEntry, FileSystem, FileType, Inode, Metadata, StatFs};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

const ATTR_RO: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIR: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = 0x0F;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Fat12,
    Fat16,
    Fat32,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct FatFs {
    dev: Arc<CachedDevice>,
    pub kind: Kind,
    bps: u64,
    cluster_size: u64,
    fat_start: u64,
    fat_size: u64,
    fat_count: u64,
    root_dir_start: u64,
    root_dir_entries: u64,
    root_cluster: u32,
    data_start: u64,
    clusters: u32,
    fsinfo_sector: u64,
    pub label: String,
    id: usize,
    alloc: Mutex<AllocState>,
    /// Serialises directory modifications.
    dir_lock: Mutex<()>,
    inodes: crate::sync::Mutex<BTreeMap<(u32, u32), Weak<FatInode>>>,
    me: spin::Once<Weak<FatFs>>,
}

struct AllocState {
    hint: u32,
    free: Option<u32>,
}

/// Where a directory entry lives: (directory first cluster, byte offset of
/// the short entry). Cluster 0 means the FAT12/16 fixed root directory.
type Loc = (u32, u32);

/// (name, short entry position, first slot position, raw short entry)
type RawEntry = (String, u32, u32, [u8; 32]);

pub struct FatInode {
    fs: Arc<FatFs>,
    is_dir: bool,
    /// None for the root directory.
    loc: crate::sync::Mutex<Option<Loc>>,
    state: crate::sync::Mutex<InodeState>,
}

struct InodeState {
    first: u32,
    size: u32,
    attr: u8,
    mtime: u64,
    ctime: u64,
    atime: u64,
    /// Cached cluster chain.
    chain: Option<Vec<u32>>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn dos_to_unix(date: u16, time: u16) -> u64 {
    if date == 0 {
        return 0;
    }
    let y = 1980 + (date >> 9) as u32;
    let m = ((date >> 5) & 0xF).clamp(1, 12) as u32;
    let d = (date & 0x1F).max(1) as u32;
    let hh = (time >> 11) as u32;
    let mm = ((time >> 5) & 0x3F) as u32;
    let ss = ((time & 0x1F) * 2) as u32;
    crate::time::unix_from_civil(y, m, d, hh, mm, ss)
}

fn unix_to_dos(t: u64) -> (u16, u16) {
    let (y, m, d, hh, mm, ss) = crate::time::civil_from_unix(t);
    if y < 1980 {
        return (0x21, 0);
    }
    let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
    let time = ((hh as u16) << 11) | ((mm as u16) << 5) | (ss as u16 / 2);
    (date, time)
}

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for &c in short.iter() {
        sum = sum.rotate_right(1).wrapping_add(c);
    }
    sum
}

fn short_name_str(raw: &[u8]) -> String {
    let base = core::str::from_utf8(&raw[0..8]).unwrap_or("").trim_end();
    let ext = core::str::from_utf8(&raw[8..11]).unwrap_or("").trim_end();
    let mut s = String::from(base);
    if !ext.is_empty() {
        s.push('.');
        s.push_str(ext);
    }
    s
}

fn valid_short_char(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || "$%'-_@~`!(){}^#&".contains(c)
}

/// The exact 8.3 form of `name` if it has one (no LFN needed).
fn exact_short(name: &str) -> Option<[u8; 11]> {
    if name == "." || name == ".." {
        let mut s = [b' '; 11];
        s[0] = b'.';
        if name == ".." {
            s[1] = b'.';
        }
        return Some(s);
    }
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 || base.contains('.') {
        return None;
    }
    if !base.chars().chain(ext.chars()).all(valid_short_char) {
        return None;
    }
    let mut s = [b' '; 11];
    s[..base.len()].copy_from_slice(base.as_bytes());
    s[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Some(s)
}

fn valid_long_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name
            .chars()
            .any(|c| "\"*/:<>?\\|".contains(c) || (c as u32) < 0x20)
}

// ---------------------------------------------------------------------------
// Filesystem
// ---------------------------------------------------------------------------

impl FatFs {
    /// Probe `dev` for a FAT filesystem.
    pub fn open(dev: Arc<CachedDevice>) -> KResult<Arc<FatFs>> {
        let mut bs = [0u8; 512];
        dev.read_bytes(0, &mut bs)?;
        if bs[510] != 0x55 || bs[511] != 0xAA {
            return Err(EINVAL);
        }
        let bps = u16le(&bs, 11) as u64;
        let spc = bs[13] as u64;
        let reserved = u16le(&bs, 14) as u64;
        let fat_count = bs[16] as u64;
        let root_entries = u16le(&bs, 17) as u64;
        let total16 = u16le(&bs, 19) as u64;
        let fat16_size = u16le(&bs, 22) as u64;
        let total32 = u32le(&bs, 32) as u64;
        if !matches!(bps, 512 | 1024 | 2048 | 4096)
            || spc == 0
            || !spc.is_power_of_two()
            || fat_count == 0
            || reserved == 0
        {
            return Err(EINVAL);
        }
        let fat_size = if fat16_size != 0 {
            fat16_size
        } else {
            u32le(&bs, 36) as u64
        };
        let total = if total16 != 0 { total16 } else { total32 };
        let root_dir_sectors = (root_entries * 32).div_ceil(bps);
        let data_start = reserved + fat_count * fat_size + root_dir_sectors;
        if fat_size == 0 || total <= data_start {
            return Err(EINVAL);
        }
        let clusters = ((total - data_start) / spc) as u32;
        let kind = if clusters < 4085 {
            Kind::Fat12
        } else if clusters < 65525 {
            Kind::Fat16
        } else {
            Kind::Fat32
        };
        let (label_off, root_cluster, fsinfo) = if kind == Kind::Fat32 {
            (71, u32le(&bs, 44), u16le(&bs, 48) as u64)
        } else {
            (43, 0, 0)
        };
        let label = core::str::from_utf8(&bs[label_off..label_off + 11])
            .unwrap_or("")
            .trim()
            .to_string();
        let fs = Arc::new(FatFs {
            dev,
            kind,
            bps,
            cluster_size: bps * spc,
            fat_start: reserved * bps,
            fat_size: fat_size * bps,
            fat_count,
            root_dir_start: (reserved + fat_count * fat_size) * bps,
            root_dir_entries: root_entries,
            root_cluster,
            data_start: data_start * bps,
            clusters,
            fsinfo_sector: fsinfo,
            label,
            id: NEXT_ID.fetch_add(1, Ordering::SeqCst) as usize | (2 << 40),
            alloc: Mutex::new(AllocState {
                hint: 2,
                free: None,
            }),
            dir_lock: Mutex::new(()),
            inodes: crate::sync::Mutex::new(BTreeMap::new()),
            me: spin::Once::new(),
        });
        fs.me.call_once(|| Arc::downgrade(&fs));
        // The root cluster must be allocated (some formatters leave it 0).
        if kind == Kind::Fat32 && fs.fat_get(root_cluster)? == 0 {
            fs.fat_set(root_cluster, 0x0FFF_FFFF)?;
        }
        Ok(fs)
    }

    fn arc(&self) -> Arc<FatFs> {
        self.me
            .get()
            .and_then(|w| w.upgrade())
            .expect("fat fs dropped")
    }

    fn eoc(&self) -> u32 {
        match self.kind {
            Kind::Fat12 => 0xFFF,
            Kind::Fat16 => 0xFFFF,
            Kind::Fat32 => 0x0FFF_FFFF,
        }
    }

    fn is_eoc(&self, v: u32) -> bool {
        match self.kind {
            Kind::Fat12 => v >= 0xFF8,
            Kind::Fat16 => v >= 0xFFF8,
            Kind::Fat32 => v >= 0x0FFF_FFF8,
        }
    }

    fn cluster_off(&self, c: u32) -> u64 {
        self.data_start + (c as u64 - 2) * self.cluster_size
    }

    fn fat_get(&self, c: u32) -> KResult<u32> {
        match self.kind {
            Kind::Fat32 => {
                let mut b = [0u8; 4];
                self.dev.read_bytes(self.fat_start + c as u64 * 4, &mut b)?;
                Ok(u32::from_le_bytes(b) & 0x0FFF_FFFF)
            }
            Kind::Fat16 => {
                let mut b = [0u8; 2];
                self.dev.read_bytes(self.fat_start + c as u64 * 2, &mut b)?;
                Ok(u16::from_le_bytes(b) as u32)
            }
            Kind::Fat12 => {
                let off = self.fat_start + (c as u64 * 3) / 2;
                let mut b = [0u8; 2];
                self.dev.read_bytes(off, &mut b)?;
                let v = u16::from_le_bytes(b);
                Ok(if c & 1 == 0 { v & 0xFFF } else { v >> 4 } as u32)
            }
        }
    }

    fn fat_set(&self, c: u32, v: u32) -> KResult<()> {
        for copy in 0..self.fat_count {
            let base = self.fat_start + copy * self.fat_size;
            match self.kind {
                Kind::Fat32 => {
                    let off = base + c as u64 * 4;
                    let mut b = [0u8; 4];
                    self.dev.read_bytes(off, &mut b)?;
                    let old = u32::from_le_bytes(b);
                    let new = (old & 0xF000_0000) | (v & 0x0FFF_FFFF);
                    self.dev.write_bytes(off, &new.to_le_bytes())?;
                }
                Kind::Fat16 => {
                    self.dev
                        .write_bytes(base + c as u64 * 2, &(v as u16).to_le_bytes())?;
                }
                Kind::Fat12 => {
                    let off = base + (c as u64 * 3) / 2;
                    let mut b = [0u8; 2];
                    self.dev.read_bytes(off, &mut b)?;
                    let old = u16::from_le_bytes(b);
                    let new = if c & 1 == 0 {
                        (old & 0xF000) | (v as u16 & 0xFFF)
                    } else {
                        (old & 0x000F) | ((v as u16) << 4)
                    };
                    self.dev.write_bytes(off, &new.to_le_bytes())?;
                }
            }
        }
        Ok(())
    }

    fn chain(&self, first: u32) -> KResult<Vec<u32>> {
        let mut out = Vec::new();
        let mut c = first;
        while c >= 2 && !self.is_eoc(c) && c < self.clusters + 2 {
            out.push(c);
            if out.len() > self.clusters as usize {
                return Err(EIO); // loop in the FAT
            }
            c = self.fat_get(c)?;
        }
        Ok(out)
    }

    fn count_free(&self) -> KResult<u32> {
        let mut n = 0;
        for c in 2..self.clusters + 2 {
            if self.fat_get(c)? == 0 {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Allocate a zeroed cluster, linking it after `prev` (0 = none).
    fn alloc_cluster(&self, prev: u32) -> KResult<u32> {
        let mut st = self.alloc.lock();
        let total = self.clusters;
        let start = st.hint.clamp(2, total + 1);
        let mut c = start;
        let mut found = None;
        for _ in 0..total {
            if self.fat_get(c)? == 0 {
                found = Some(c);
                break;
            }
            c += 1;
            if c >= total + 2 {
                c = 2;
            }
        }
        let c = found.ok_or(ENOSPC)?;
        self.fat_set(c, self.eoc())?;
        if prev >= 2 {
            self.fat_set(prev, c)?;
        }
        st.hint = c + 1;
        if let Some(f) = st.free.as_mut() {
            *f = f.saturating_sub(1);
        }
        drop(st);
        let zeros = alloc::vec![0u8; self.cluster_size as usize];
        self.dev.write_bytes(self.cluster_off(c), &zeros)?;
        Ok(c)
    }

    fn free_chain(&self, first: u32) -> KResult<()> {
        let chain = self.chain(first)?;
        let mut st = self.alloc.lock();
        for &c in &chain {
            self.fat_set(c, 0)?;
        }
        if let Some(f) = st.free.as_mut() {
            *f += chain.len() as u32;
        }
        if let Some(&c) = chain.first() {
            st.hint = st.hint.min(c);
        }
        Ok(())
    }

    fn update_fsinfo(&self) -> KResult<()> {
        if self.kind != Kind::Fat32 || self.fsinfo_sector == 0 {
            return Ok(());
        }
        let st = self.alloc.lock();
        let off = self.fsinfo_sector * self.bps;
        let mut b = [0u8; 512];
        self.dev.read_bytes(off, &mut b)?;
        if u32le(&b, 0) != 0x4161_5252 || u32le(&b, 484) != 0x6141_7272 {
            return Ok(());
        }
        if let Some(f) = st.free {
            b[488..492].copy_from_slice(&f.to_le_bytes());
        }
        b[492..496].copy_from_slice(&st.hint.to_le_bytes());
        self.dev.write_bytes(off, &b)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Directories
    // ------------------------------------------------------------------

    /// Byte ranges making up a directory: (disk offset, length).
    fn dir_extents(&self, first: u32) -> KResult<Vec<(u64, u64)>> {
        if first == 0 {
            return Ok(alloc::vec![(
                self.root_dir_start,
                self.root_dir_entries * 32
            )]);
        }
        Ok(self
            .chain(first)?
            .into_iter()
            .map(|c| (self.cluster_off(c), self.cluster_size))
            .collect())
    }

    fn dir_bytes(&self, first: u32) -> KResult<Vec<u8>> {
        let mut out = Vec::new();
        for (off, len) in self.dir_extents(first)? {
            let mut b = alloc::vec![0u8; len as usize];
            self.dev.read_bytes(off, &mut b)?;
            out.extend_from_slice(&b);
        }
        Ok(out)
    }

    /// Disk offset of byte `pos` within a directory.
    fn dir_pos_to_disk(&self, first: u32, pos: u32) -> KResult<u64> {
        let mut p = pos as u64;
        for (off, len) in self.dir_extents(first)? {
            if p < len {
                return Ok(off + p);
            }
            p -= len;
        }
        Err(EIO)
    }

    fn write_entry(&self, dir: u32, pos: u32, e: &[u8; 32]) -> KResult<()> {
        let off = self.dir_pos_to_disk(dir, pos)?;
        self.dev.write_bytes(off, e)?;
        Ok(())
    }

    /// Parse a directory into entries: (name, short-entry pos, first entry
    /// pos (LFN start), raw short entry).
    fn list(&self, dir: u32) -> KResult<Vec<RawEntry>> {
        let data = self.dir_bytes(dir)?;
        let mut out = Vec::new();
        let mut lfn: Vec<(u8, [u16; 13])> = Vec::new();
        let mut lfn_start = 0u32;
        let mut lfn_sum = 0u8;
        for (i, e) in data.chunks_exact(32).enumerate() {
            let pos = (i * 32) as u32;
            if e[0] == 0 {
                break;
            }
            if e[0] == 0xE5 {
                lfn.clear();
                continue;
            }
            if e[11] == ATTR_LFN {
                if e[0] & 0x40 != 0 {
                    lfn.clear();
                    lfn_start = pos;
                    lfn_sum = e[13];
                }
                let mut chars = [0u16; 13];
                let offs = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
                for (k, &o) in offs.iter().enumerate() {
                    chars[k] = u16le(e, o);
                }
                lfn.push((e[0] & 0x1F, chars));
                continue;
            }
            if e[11] & ATTR_VOLUME != 0 {
                lfn.clear();
                continue;
            }
            let mut short = [0u8; 11];
            short.copy_from_slice(&e[0..11]);
            let name = if !lfn.is_empty() && lfn_checksum(&short) == lfn_sum {
                lfn.sort_by_key(|(seq, _)| *seq);
                let units: Vec<u16> = lfn
                    .iter()
                    .flat_map(|(_, c)| c.iter().copied())
                    .take_while(|&c| c != 0 && c != 0xFFFF)
                    .collect();
                String::from_utf16_lossy(&units)
            } else {
                let mut s = short_name_str(&short);
                // NT lowercase flags.
                if e[12] & 0x08 != 0 || e[12] & 0x10 != 0 {
                    let (b, x) = s
                        .split_once('.')
                        .map_or((s.clone(), String::new()), |(b, x)| {
                            (b.to_string(), x.to_string())
                        });
                    let b = if e[12] & 0x08 != 0 {
                        b.to_lowercase()
                    } else {
                        b
                    };
                    let x = if e[12] & 0x10 != 0 {
                        x.to_lowercase()
                    } else {
                        x
                    };
                    s = if x.is_empty() {
                        b
                    } else {
                        alloc::format!("{}.{}", b, x)
                    };
                }
                if short[0] == 0x05 {
                    s.replace_range(0..1, "\u{e5}");
                }
                s
            };
            let start = if lfn.is_empty() { pos } else { lfn_start };
            let mut raw = [0u8; 32];
            raw.copy_from_slice(e);
            out.push((name, pos, start, raw));
            lfn.clear();
        }
        Ok(out)
    }

    fn find(&self, dir: u32, name: &str) -> KResult<RawEntry> {
        self.list(dir)?
            .into_iter()
            .find(|(n, ..)| n.eq_ignore_ascii_case(name))
            .ok_or(ENOENT)
    }

    fn first_cluster_of(e: &[u8; 32]) -> u32 {
        (u16le(e, 20) as u32) << 16 | u16le(e, 26) as u32
    }

    /// Find `n` consecutive free slots, growing the directory if needed.
    fn free_slots(&self, dir: u32, n: usize) -> KResult<u32> {
        loop {
            let data = self.dir_bytes(dir)?;
            let mut run = 0;
            for (i, e) in data.chunks_exact(32).enumerate() {
                if e[0] == 0 || e[0] == 0xE5 {
                    run += 1;
                    if run == n {
                        return Ok(((i + 1 - n) * 32) as u32);
                    }
                } else {
                    run = 0;
                }
            }
            if dir == 0 {
                return Err(ENOSPC); // fixed FAT12/16 root directory
            }
            let last = *self.chain(dir)?.last().ok_or(EIO)?;
            self.alloc_cluster(last)?;
        }
    }

    fn unique_short(&self, dir: u32, name: &str) -> KResult<[u8; 11]> {
        let upper: String = name
            .to_uppercase()
            .chars()
            .filter(|&c| c != ' ')
            .map(|c| {
                if valid_short_char(c) || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let (base, ext) = match upper.rsplit_once('.') {
            Some((b, e)) if !b.is_empty() => (b.replace('.', ""), e.to_string()),
            _ => (upper.replace('.', ""), String::new()),
        };
        let ext: String = ext.chars().take(3).collect();
        let existing: Vec<[u8; 11]> = self
            .list(dir)?
            .iter()
            .map(|(_, _, _, raw)| {
                let mut s = [0u8; 11];
                s.copy_from_slice(&raw[0..11]);
                s
            })
            .collect();
        for n in 1..1_000_000u32 {
            let tail = alloc::format!("~{}", n);
            let keep = 8 - tail.len();
            let b: String = base.chars().take(keep).collect();
            let full = alloc::format!("{}{}", b, tail);
            let mut s = [b' '; 11];
            s[..full.len()].copy_from_slice(full.as_bytes());
            s[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
            if !existing.contains(&s) {
                return Ok(s);
            }
        }
        Err(ENOSPC)
    }

    /// Create a directory entry. Returns the short entry position.
    fn add_entry(&self, dir: u32, name: &str, attr: u8, first: u32, size: u32) -> KResult<u32> {
        if !valid_long_name(name) {
            return Err(EINVAL);
        }
        let exact = exact_short(name);
        let needs_lfn = exact.is_none();
        let short = match exact {
            Some(s) => s,
            None => self.unique_short(dir, name)?,
        };
        let units: Vec<u16> = name.encode_utf16().collect();
        let lfn_count = if needs_lfn {
            units.len().div_ceil(13)
        } else {
            0
        };
        let pos = self.free_slots(dir, lfn_count + 1)?;
        let sum = lfn_checksum(&short);
        for k in 0..lfn_count {
            let seq = (lfn_count - k) as u8;
            let mut e = [0u8; 32];
            e[0] = seq | if k == 0 { 0x40 } else { 0 };
            e[11] = ATTR_LFN;
            e[13] = sum;
            let offs = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
            for (j, &o) in offs.iter().enumerate() {
                let idx = (seq as usize - 1) * 13 + j;
                let v: u16 = match idx.cmp(&units.len()) {
                    core::cmp::Ordering::Less => units[idx],
                    core::cmp::Ordering::Equal => 0,
                    core::cmp::Ordering::Greater => 0xFFFF,
                };
                e[o..o + 2].copy_from_slice(&v.to_le_bytes());
            }
            self.write_entry(dir, pos + (k as u32) * 32, &e)?;
        }
        let spos = pos + (lfn_count as u32) * 32;
        let now = crate::time::unix_time();
        let (d, t) = unix_to_dos(now);
        let mut e = [0u8; 32];
        e[0..11].copy_from_slice(&short);
        e[11] = attr;
        e[14..16].copy_from_slice(&t.to_le_bytes());
        e[16..18].copy_from_slice(&d.to_le_bytes());
        e[18..20].copy_from_slice(&d.to_le_bytes());
        e[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        e[22..24].copy_from_slice(&t.to_le_bytes());
        e[24..26].copy_from_slice(&d.to_le_bytes());
        e[26..28].copy_from_slice(&(first as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        self.write_entry(dir, spos, &e)?;
        Ok(spos)
    }

    /// Mark the entry (and its LFN entries) deleted.
    fn remove_entry(&self, dir: u32, start: u32, spos: u32) -> KResult<()> {
        let mut p = start;
        while p <= spos {
            let off = self.dir_pos_to_disk(dir, p)?;
            self.dev.write_bytes(off, &[0xE5])?;
            p += 32;
        }
        Ok(())
    }

    fn read_entry(&self, loc: Loc) -> KResult<[u8; 32]> {
        let mut e = [0u8; 32];
        let off = self.dir_pos_to_disk(loc.0, loc.1)?;
        self.dev.read_bytes(off, &mut e)?;
        Ok(e)
    }

    fn inode_for(&self, loc: Loc, raw: &[u8; 32]) -> Arc<FatInode> {
        let mut table = self.inodes.lock();
        if let Some(i) = table.get(&loc).and_then(|w| w.upgrade()) {
            return i;
        }
        let is_dir = raw[11] & ATTR_DIR != 0;
        let mtime = dos_to_unix(u16le(raw, 24), u16le(raw, 22));
        let ctime = dos_to_unix(u16le(raw, 16), u16le(raw, 14));
        let atime = dos_to_unix(u16le(raw, 18), 0);
        let inode = Arc::new(FatInode {
            fs: self.arc(),
            is_dir,
            loc: crate::sync::Mutex::new(Some(loc)),
            state: crate::sync::Mutex::new(InodeState {
                first: Self::first_cluster_of(raw),
                size: u32le(raw, 28),
                attr: raw[11],
                mtime,
                ctime,
                atime,
                chain: None,
            }),
        });
        table.insert(loc, Arc::downgrade(&inode));
        table.retain(|_, w| w.strong_count() > 0);
        inode
    }

    fn root_inode(&self) -> Arc<FatInode> {
        Arc::new(FatInode {
            fs: self.arc(),
            is_dir: true,
            loc: crate::sync::Mutex::new(None),
            state: crate::sync::Mutex::new(InodeState {
                first: if self.kind == Kind::Fat32 {
                    self.root_cluster
                } else {
                    0
                },
                size: 0,
                attr: ATTR_DIR,
                mtime: 0,
                ctime: 0,
                atime: 0,
                chain: None,
            }),
        })
    }
}

impl FileSystem for FatFs {
    fn root(&self) -> Arc<dyn Inode> {
        self.root_inode()
    }
    fn name(&self) -> &'static str {
        "vfat"
    }
    fn sync(&self) -> KResult<()> {
        let _ = self.update_fsinfo();
        self.dev.sync()
    }
    fn statfs(&self) -> StatFs {
        let free = {
            let known = self.alloc.lock().free;
            match known {
                Some(f) => f,
                None => {
                    let f = self.count_free().unwrap_or(0);
                    self.alloc.lock().free = Some(f);
                    f
                }
            }
        };
        StatFs {
            fs_type: 0x4d44,
            block_size: self.cluster_size,
            blocks: self.clusters as u64,
            blocks_free: free as u64,
            files: 0,
            files_free: 0,
            name_max: 255,
        }
    }
}

// ---------------------------------------------------------------------------
// Inodes
// ---------------------------------------------------------------------------

impl FatInode {
    fn dir_cluster(&self) -> u32 {
        self.state.lock().first
    }

    /// Write size/first cluster/mtime back to the directory entry.
    fn flush_entry(&self) -> KResult<()> {
        let Some(loc) = *self.loc.lock() else {
            return Ok(());
        };
        let mut e = self.fs.read_entry(loc)?;
        let st = self.state.lock();
        let (d, t) = unix_to_dos(st.mtime);
        e[11] = st.attr;
        e[20..22].copy_from_slice(&((st.first >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(st.first as u16).to_le_bytes());
        e[22..24].copy_from_slice(&t.to_le_bytes());
        e[24..26].copy_from_slice(&d.to_le_bytes());
        e[18..20].copy_from_slice(&d.to_le_bytes());
        e[28..32].copy_from_slice(&(if self.is_dir { 0 } else { st.size }).to_le_bytes());
        drop(st);
        let off = self.fs.dir_pos_to_disk(loc.0, loc.1)?;
        self.fs.dev.write_bytes(off, &e)?;
        Ok(())
    }

    fn chain(&self) -> KResult<Vec<u32>> {
        let mut st = self.state.lock();
        if st.chain.is_none() {
            let c = self.fs.chain(st.first)?;
            st.chain = Some(c);
        }
        Ok(st.chain.clone().unwrap())
    }

    /// Ensure the chain covers `bytes`, allocating clusters as needed.
    fn grow_to(&self, bytes: u64) -> KResult<Vec<u32>> {
        let cs = self.fs.cluster_size;
        let need = bytes.div_ceil(cs) as usize;
        let mut chain = self.chain()?;
        while chain.len() < need {
            let prev = chain.last().copied().unwrap_or(0);
            let c = self.fs.alloc_cluster(prev)?;
            if chain.is_empty() {
                self.state.lock().first = c;
            }
            chain.push(c);
        }
        self.state.lock().chain = Some(chain.clone());
        Ok(chain)
    }

    fn touch(&self) {
        let now = crate::time::unix_time();
        let mut st = self.state.lock();
        st.mtime = now;
        st.atime = now;
    }

    fn check_dir(&self) -> KResult<()> {
        if !self.is_dir {
            return Err(ENOTDIR);
        }
        Ok(())
    }
}

impl Inode for FatInode {
    fn metadata(&self) -> KResult<Metadata> {
        let st = self.state.lock();
        let kind = if self.is_dir {
            FileType::Directory
        } else {
            FileType::Regular
        };
        let mode = if st.attr & ATTR_RO != 0 { 0o555 } else { 0o755 };
        let mut m = Metadata::new(kind, if self.is_dir { 0o755 } else { mode & 0o777 });
        m.size = if self.is_dir {
            st.chain.as_ref().map_or(0, |c| c.len() as u64) * self.fs.cluster_size
        } else {
            st.size as u64
        };
        m.ino = match *self.loc.lock() {
            Some((d, p)) => ((d as u64) << 20) | p as u64,
            None => 1,
        };
        m.blksize = self.fs.cluster_size as u32;
        m.blocks = (st.size as u64).div_ceil(512);
        m.mtime = st.mtime;
        m.ctime = st.ctime.max(st.mtime.min(st.ctime.max(1)));
        m.atime = st.atime;
        Ok(m)
    }

    fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
        self.check_dir()?;
        let dir = self.dir_cluster();
        if name == "." {
            return Err(ENOENT);
        }
        let (_, spos, _, raw) = self.fs.find(dir, name)?;
        Ok(self.fs.inode_for((dir, spos), &raw))
    }

    fn create(&self, name: &str, kind: FileType, _mode: u32) -> KResult<Arc<dyn Inode>> {
        self.check_dir()?;
        let dir = self.dir_cluster();
        let _g = self.fs.dir_lock.lock();
        if self.fs.find(dir, name).is_ok() {
            return Err(EEXIST);
        }
        let (attr, first) = match kind {
            FileType::Regular => (ATTR_ARCHIVE, 0),
            FileType::Directory => {
                let c = self.fs.alloc_cluster(0)?;
                (ATTR_DIR, c)
            }
            _ => return Err(EPERM),
        };
        let spos = match self.fs.add_entry(dir, name, attr, first, 0) {
            Ok(p) => p,
            Err(e) => {
                if first != 0 {
                    let _ = self.fs.free_chain(first);
                }
                return Err(e);
            }
        };
        if kind == FileType::Directory {
            // "." and ".." entries.
            let parent_cluster = if dir == self.fs.root_cluster && self.fs.kind == Kind::Fat32 {
                0
            } else {
                dir
            };
            self.fs.add_entry(first, ".", ATTR_DIR, first, 0)?;
            self.fs
                .add_entry(first, "..", ATTR_DIR, parent_cluster, 0)?;
        }
        self.touch();
        let _ = self.flush_entry();
        let raw = self.fs.read_entry((dir, spos))?;
        Ok(self.fs.inode_for((dir, spos), &raw))
    }

    fn unlink(&self, name: &str) -> KResult<()> {
        self.check_dir()?;
        let dir = self.dir_cluster();
        let _g = self.fs.dir_lock.lock();
        let (_, spos, start, raw) = self.fs.find(dir, name)?;
        if raw[11] & ATTR_DIR != 0 {
            return Err(EISDIR);
        }
        self.fs.remove_entry(dir, start, spos)?;
        let first = FatFs::first_cluster_of(&raw);
        if first >= 2 {
            self.fs.free_chain(first)?;
        }
        if let Some(i) = self
            .fs
            .inodes
            .lock()
            .remove(&(dir, spos))
            .and_then(|w| w.upgrade())
        {
            *i.loc.lock() = None;
        }
        self.touch();
        Ok(())
    }

    fn rmdir(&self, name: &str) -> KResult<()> {
        self.check_dir()?;
        let dir = self.dir_cluster();
        let _g = self.fs.dir_lock.lock();
        let (_, spos, start, raw) = self.fs.find(dir, name)?;
        if raw[11] & ATTR_DIR == 0 {
            return Err(ENOTDIR);
        }
        let first = FatFs::first_cluster_of(&raw);
        if self
            .fs
            .list(first)?
            .iter()
            .any(|(n, ..)| n != "." && n != "..")
        {
            return Err(ENOTEMPTY);
        }
        self.fs.remove_entry(dir, start, spos)?;
        if first >= 2 {
            self.fs.free_chain(first)?;
        }
        self.fs.inodes.lock().remove(&(dir, spos));
        self.touch();
        Ok(())
    }

    fn rename(&self, old: &str, new_dir: &Arc<dyn Inode>, new: &str) -> KResult<()> {
        self.check_dir()?;
        let nd = new_dir.as_any().downcast_ref::<FatInode>().ok_or(EXDEV)?;
        nd.check_dir()?;
        let src_dir = self.dir_cluster();
        let dst_dir = nd.dir_cluster();
        let _g = self.fs.dir_lock.lock();
        let (_, spos, start, raw) = self.fs.find(src_dir, old)?;
        let is_dir = raw[11] & ATTR_DIR != 0;
        if let Ok((_, tpos, tstart, traw)) = self.fs.find(dst_dir, new) {
            if (src_dir, spos) == (dst_dir, tpos) {
                // Same entry (case change): rewrite it below.
            } else {
                let t_is_dir = traw[11] & ATTR_DIR != 0;
                if t_is_dir != is_dir {
                    return Err(if t_is_dir { EISDIR } else { ENOTDIR });
                }
                let tfirst = FatFs::first_cluster_of(&traw);
                if t_is_dir
                    && self
                        .fs
                        .list(tfirst)?
                        .iter()
                        .any(|(n, ..)| n != "." && n != "..")
                {
                    return Err(ENOTEMPTY);
                }
                self.fs.remove_entry(dst_dir, tstart, tpos)?;
                if tfirst >= 2 {
                    self.fs.free_chain(tfirst)?;
                }
            }
        }
        let first = FatFs::first_cluster_of(&raw);
        let size = u32le(&raw, 28);
        self.fs.remove_entry(src_dir, start, spos)?;
        let npos = self.fs.add_entry(dst_dir, new, raw[11], first, size)?;
        // Keep the original timestamps.
        let mut ne = self.fs.read_entry((dst_dir, npos))?;
        ne[13..26].copy_from_slice(&raw[13..26]);
        let off = self.fs.dir_pos_to_disk(dst_dir, npos)?;
        self.fs.dev.write_bytes(off, &ne)?;
        if is_dir && src_dir != dst_dir && first >= 2 {
            // Point ".." at the new parent.
            let parent = if dst_dir == self.fs.root_cluster && self.fs.kind == Kind::Fat32 {
                0
            } else {
                dst_dir
            };
            if let Ok((_, dpos, _, mut draw)) = self.fs.find(first, "..") {
                draw[20..22].copy_from_slice(&((parent >> 16) as u16).to_le_bytes());
                draw[26..28].copy_from_slice(&(parent as u16).to_le_bytes());
                self.fs.write_entry(first, dpos, &draw)?;
            }
        }
        let mut table = self.fs.inodes.lock();
        if let Some(w) = table.remove(&(src_dir, spos)) {
            if let Some(i) = w.upgrade() {
                *i.loc.lock() = Some((dst_dir, npos));
            }
            table.insert((dst_dir, npos), w);
        }
        drop(table);
        self.touch();
        nd.touch();
        Ok(())
    }

    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        self.check_dir()?;
        let dir = self.dir_cluster();
        Ok(self
            .fs
            .list(dir)?
            .into_iter()
            .filter(|(n, ..)| n != "." && n != "..")
            .map(|(name, spos, _, raw)| DirEntry {
                name,
                ino: ((dir as u64) << 20) | spos as u64,
                kind: if raw[11] & ATTR_DIR != 0 {
                    FileType::Directory
                } else {
                    FileType::Regular
                },
            })
            .collect())
    }

    fn cacheable(&self) -> bool {
        !self.is_dir
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        if self.is_dir {
            return Err(EISDIR);
        }
        let size = self.state.lock().size as u64;
        if off >= size {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(size - off) as usize;
        let chain = self.chain()?;
        let cs = self.fs.cluster_size;
        let mut done = 0usize;
        while done < len {
            let pos = off + done as u64;
            let idx = (pos / cs) as usize;
            let within = pos % cs;
            let c = *chain.get(idx).ok_or(EIO)?;
            // Read contiguous clusters in one go.
            let mut run = 1;
            while idx + run < chain.len()
                && chain[idx + run] == c + run as u32
                && (run as u64) * cs < (len - done) as u64 + within
            {
                run += 1;
            }
            let n = ((run as u64 * cs - within) as usize).min(len - done);
            self.fs
                .dev
                .read_bytes(self.fs.cluster_off(c) + within, &mut buf[done..done + n])?;
            done += n;
        }
        self.state.lock().atime = crate::time::unix_time();
        Ok(len)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        if self.is_dir {
            return Err(EISDIR);
        }
        let end = off.checked_add(buf.len() as u64).ok_or(EFBIG)?;
        if end > u32::MAX as u64 {
            return Err(EFBIG);
        }
        let size = self.state.lock().size as u64;
        let chain = self.grow_to(end.max(size))?;
        let cs = self.fs.cluster_size;
        // Zero-fill a gap between the old end and `off`.
        if off > size {
            let zeros = alloc::vec![0u8; (off - size) as usize];
            self.write_range(&chain, size, &zeros)?;
        }
        self.write_range(&chain, off, buf)?;
        let _ = cs;
        {
            let mut st = self.state.lock();
            st.size = st.size.max(end as u32);
        }
        self.touch();
        self.flush_entry()?;
        Ok(buf.len())
    }

    fn truncate(&self, size: u64) -> KResult<()> {
        if self.is_dir {
            return Err(EISDIR);
        }
        if size > u32::MAX as u64 {
            return Err(EFBIG);
        }
        let old = self.state.lock().size as u64;
        if size > old {
            self.write_at(old, &alloc::vec![0u8; (size - old) as usize])?;
            return Ok(());
        }
        let cs = self.fs.cluster_size;
        let keep = size.div_ceil(cs) as usize;
        let chain = self.chain()?;
        if keep < chain.len() {
            if keep == 0 {
                self.fs.free_chain(chain[0])?;
                self.state.lock().first = 0;
            } else {
                self.fs.free_chain(chain[keep])?;
                self.fs.fat_set(chain[keep - 1], self.fs.eoc())?;
            }
            self.state.lock().chain = Some(chain[..keep].to_vec());
        }
        self.state.lock().size = size as u32;
        self.touch();
        self.flush_entry()
    }

    fn chmod(&self, mode: u32) -> KResult<()> {
        let mut st = self.state.lock();
        if mode & 0o222 == 0 {
            st.attr |= ATTR_RO;
        } else {
            st.attr &= !ATTR_RO;
        }
        drop(st);
        self.flush_entry()
    }

    fn set_times(&self, atime: Option<u64>, mtime: Option<u64>) -> KResult<()> {
        {
            let mut st = self.state.lock();
            if let Some(a) = atime {
                st.atime = a;
            }
            if let Some(m) = mtime {
                st.mtime = m;
            }
        }
        self.flush_entry()
    }

    fn sync(&self) -> KResult<()> {
        self.fs.sync()
    }

    fn fs_id(&self) -> usize {
        self.fs.id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl FatInode {
    fn write_range(&self, chain: &[u32], off: u64, buf: &[u8]) -> KResult<()> {
        let cs = self.fs.cluster_size;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = off + done as u64;
            let idx = (pos / cs) as usize;
            let within = pos % cs;
            let c = *chain.get(idx).ok_or(EIO)?;
            let n = ((cs - within) as usize).min(buf.len() - done);
            self.fs
                .dev
                .write_bytes(self.fs.cluster_off(c) + within, &buf[done..done + n])?;
            done += n;
        }
        Ok(())
    }
}

#[allow(dead_code)]
const _: u8 = ATTR_HIDDEN | ATTR_SYSTEM;
