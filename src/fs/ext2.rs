//! ext2 read/write; ext3/ext4 read-only (extents, 64-bit descriptors,
//! flex_bg). Filesystems with features we cannot keep consistent while
//! writing (journal needing recovery, metadata checksums, extents, ...) are
//! mounted read-only.

use crate::block::cache::CachedDevice;
use crate::errno::*;
use crate::sched::mutex::Mutex;
use crate::vfs::{DirEntry, FileSystem, FileType, Inode, Metadata, StatFs};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

const MAGIC: u16 = 0xEF53;
const ROOT_INO: u32 = 2;

// Feature flags.
const COMPAT_DIR_INDEX: u32 = 0x20;
const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_RECOVER: u32 = 0x4;
const INCOMPAT_EXTENTS: u32 = 0x40;
const INCOMPAT_64BIT: u32 = 0x80;
const INCOMPAT_FLEX_BG: u32 = 0x200;
const INCOMPAT_SUPPORTED_RO: u32 = INCOMPAT_FILETYPE | INCOMPAT_RECOVER | INCOMPAT_EXTENTS | INCOMPAT_64BIT | INCOMPAT_FLEX_BG | 0x400 /* EA_INODE */ | 0x1000 /* LARGEDIR */ | 0x10 /* META_BG */;
const RO_COMPAT_SPARSE: u32 = 0x1;
const RO_COMPAT_LARGE_FILE: u32 = 0x2;

const FL_INDEX: u32 = 0x1000;
const FL_EXTENTS: u32 = 0x80000;
const FL_INLINE: u32 = 0x1000_0000;

const S_IFMT: u16 = 0xF000;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// (name, inode, type code, logical block, offset in block)
type RawDirEntry = (String, u32, u8, u64, usize);

struct Group {
    block_bitmap: u64,
    inode_bitmap: u64,
    inode_table: u64,
    free_blocks: u32,
    free_inodes: u32,
    used_dirs: u32,
}

pub struct Ext2Fs {
    dev: Arc<CachedDevice>,
    block_size: u64,
    blocks_count: u64,
    inodes_count: u32,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_size: u64,
    first_data_block: u32,
    desc_size: u64,
    desc_table: u64,
    first_ino: u32,
    filetype: bool,
    read_only: bool,
    pub label: String,
    pub kind: &'static str,
    id: usize,
    meta: Mutex<Meta>,
    inodes: crate::sync::Mutex<BTreeMap<u32, Weak<Ext2Inode>>>,
    me: spin::Once<Weak<Ext2Fs>>,
}

struct Meta {
    groups: Vec<Group>,
    free_blocks: u64,
    free_inodes: u32,
}

/// Parsed on-disk inode.
#[derive(Clone)]
struct RawInode {
    mode: u16,
    uid: u32,
    gid: u32,
    size: u64,
    atime: u32,
    ctime: u32,
    mtime: u32,
    dtime: u32,
    links: u16,
    blocks512: u64,
    flags: u32,
    block: [u32; 15],
    /// The whole on-disk record, so unknown fields round-trip.
    raw: Vec<u8>,
}

pub struct Ext2Inode {
    fs: Arc<Ext2Fs>,
    ino: u32,
    st: Mutex<RawInode>,
    /// Last block allocated for this file: the next allocation starts
    /// there so files grow contiguously (and allocation stays O(1)).
    alloc_hint: AtomicU64,
}

impl RawInode {
    fn parse(b: &[u8]) -> RawInode {
        let mut block = [0u32; 15];
        for (i, v) in block.iter_mut().enumerate() {
            *v = u32le(b, 40 + i * 4);
        }
        RawInode {
            mode: u16le(b, 0),
            uid: u16le(b, 2) as u32 | (u16le(b, 120) as u32) << 16,
            size: u32le(b, 4) as u64 | (u32le(b, 108) as u64) << 32,
            atime: u32le(b, 8),
            ctime: u32le(b, 12),
            mtime: u32le(b, 16),
            dtime: u32le(b, 20),
            gid: u16le(b, 24) as u32 | (u16le(b, 122) as u32) << 16,
            links: u16le(b, 26),
            blocks512: u32le(b, 28) as u64 | (u16le(b, 116) as u64) << 32,
            flags: u32le(b, 32),
            block,
            raw: b.to_vec(),
        }
    }

    fn serialize(&self) -> Vec<u8> {
        let mut b = self.raw.clone();
        put16(&mut b, 0, self.mode);
        put16(&mut b, 2, self.uid as u16);
        put32(&mut b, 4, self.size as u32);
        put32(&mut b, 8, self.atime);
        put32(&mut b, 12, self.ctime);
        put32(&mut b, 16, self.mtime);
        put32(&mut b, 20, self.dtime);
        put16(&mut b, 24, self.gid as u16);
        put16(&mut b, 26, self.links);
        put32(&mut b, 28, self.blocks512 as u32);
        put32(&mut b, 32, self.flags);
        for (i, v) in self.block.iter().enumerate() {
            put32(&mut b, 40 + i * 4, *v);
        }
        put32(&mut b, 108, (self.size >> 32) as u32);
        put16(&mut b, 116, (self.blocks512 >> 32) as u16);
        put16(&mut b, 120, (self.uid >> 16) as u16);
        put16(&mut b, 122, (self.gid >> 16) as u16);
        b
    }

    fn kind(&self) -> FileType {
        match self.mode & S_IFMT {
            0x4000 => FileType::Directory,
            0xA000 => FileType::Symlink,
            0x2000 => FileType::CharDevice,
            0x6000 => FileType::BlockDevice,
            0x1000 => FileType::Fifo,
            0xC000 => FileType::Socket,
            _ => FileType::Regular,
        }
    }
}

fn ft_code(k: FileType) -> u8 {
    match k {
        FileType::Regular => 1,
        FileType::Directory => 2,
        FileType::CharDevice => 3,
        FileType::BlockDevice => 4,
        FileType::Fifo => 5,
        FileType::Socket => 6,
        FileType::Symlink => 7,
    }
}

fn ft_from_code(c: u8) -> FileType {
    match c {
        2 => FileType::Directory,
        3 => FileType::CharDevice,
        4 => FileType::BlockDevice,
        5 => FileType::Fifo,
        6 => FileType::Socket,
        7 => FileType::Symlink,
        _ => FileType::Regular,
    }
}

fn now() -> u32 {
    crate::time::unix_time() as u32
}

/// Superblock probe used by detection: returns (fs kind, label).
pub fn probe(dev: &CachedDevice) -> Option<(&'static str, String)> {
    let mut sb = [0u8; 1024];
    dev.read_bytes(1024, &mut sb).ok()?;
    if u16le(&sb, 56) != MAGIC {
        return None;
    }
    let compat = u32le(&sb, 92);
    let incompat = u32le(&sb, 96);
    let kind = if incompat & (INCOMPAT_EXTENTS | INCOMPAT_64BIT | INCOMPAT_FLEX_BG) != 0 {
        "ext4"
    } else if compat & 0x4 != 0 {
        "ext3"
    } else {
        "ext2"
    };
    let label = core::str::from_utf8(&sb[120..136])
        .unwrap_or("")
        .trim_end_matches('\0')
        .into();
    Some((kind, label))
}

impl Ext2Fs {
    pub fn open(dev: Arc<CachedDevice>, want_ro: bool) -> KResult<Arc<Ext2Fs>> {
        let mut sb = [0u8; 1024];
        dev.read_bytes(1024, &mut sb)?;
        if u16le(&sb, 56) != MAGIC {
            return Err(EINVAL);
        }
        let log = u32le(&sb, 24);
        if log > 6 {
            return Err(EINVAL);
        }
        let block_size = 1024u64 << log;
        let rev = u32le(&sb, 76);
        let compat = u32le(&sb, 92);
        let incompat = u32le(&sb, 96);
        let ro_compat = u32le(&sb, 100);
        if incompat & !INCOMPAT_SUPPORTED_RO != 0 {
            crate::println!(
                "[ext2] unsupported incompatible features {:#x}",
                incompat & !INCOMPAT_SUPPORTED_RO
            );
            return Err(EINVAL);
        }
        let writable_features = incompat & !INCOMPAT_FILETYPE == 0
            && ro_compat & !(RO_COMPAT_SPARSE | RO_COMPAT_LARGE_FILE) == 0;
        let read_only = want_ro || dev.read_only() || !writable_features;
        let is64 = incompat & INCOMPAT_64BIT != 0;
        let desc_size = if is64 {
            (u16le(&sb, 254) as u64).max(32)
        } else {
            32
        };
        let blocks_count = u32le(&sb, 4) as u64
            | if is64 {
                (u32le(&sb, 336) as u64) << 32
            } else {
                0
            };
        let inodes_count = u32le(&sb, 0);
        let first_data_block = u32le(&sb, 20);
        let blocks_per_group = u32le(&sb, 32);
        let inodes_per_group = u32le(&sb, 40);
        if blocks_per_group == 0 || inodes_per_group == 0 {
            return Err(EINVAL);
        }
        let (inode_size, first_ino) = if rev >= 1 {
            (u16le(&sb, 88) as u64, u32le(&sb, 84))
        } else {
            (128, 11)
        };
        let ngroups =
            (blocks_count - first_data_block as u64).div_ceil(blocks_per_group as u64) as usize;
        let desc_table = (first_data_block as u64 + 1) * block_size;
        let mut raw = vec![0u8; ngroups * desc_size as usize];
        dev.read_bytes(desc_table, &mut raw)?;
        let mut groups = Vec::with_capacity(ngroups);
        for g in 0..ngroups {
            let d = &raw[g * desc_size as usize..(g + 1) * desc_size as usize];
            let hi = |o: usize| {
                if is64 && desc_size >= 64 {
                    u32le(d, o) as u64
                } else {
                    0
                }
            };
            let hi16 = |o: usize| {
                if is64 && desc_size >= 64 {
                    u16le(d, o) as u32
                } else {
                    0
                }
            };
            groups.push(Group {
                block_bitmap: u32le(d, 0) as u64 | hi(0x20) << 32,
                inode_bitmap: u32le(d, 4) as u64 | hi(0x24) << 32,
                inode_table: u32le(d, 8) as u64 | hi(0x28) << 32,
                free_blocks: u16le(d, 12) as u32 | hi16(0x2C) << 16,
                free_inodes: u16le(d, 14) as u32 | hi16(0x2E) << 16,
                used_dirs: u16le(d, 16) as u32 | hi16(0x30) << 16,
            });
        }
        let label: String = core::str::from_utf8(&sb[120..136])
            .unwrap_or("")
            .trim_end_matches('\0')
            .into();
        let kind = probe(&dev).map(|(k, _)| k).unwrap_or("ext2");
        let free_blocks = groups.iter().map(|g| g.free_blocks as u64).sum();
        let free_inodes = groups.iter().map(|g| g.free_inodes).sum();
        let fs = Arc::new(Ext2Fs {
            dev,
            block_size,
            blocks_count,
            inodes_count,
            blocks_per_group,
            inodes_per_group,
            inode_size,
            first_data_block,
            desc_size,
            desc_table,
            first_ino,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
            read_only,
            label,
            kind,
            id: NEXT_ID.fetch_add(1, Ordering::SeqCst) as usize | (3 << 40),
            meta: Mutex::new(Meta {
                groups,
                free_blocks,
                free_inodes,
            }),
            inodes: crate::sync::Mutex::new(BTreeMap::new()),
            me: spin::Once::new(),
        });
        fs.me.call_once(|| Arc::downgrade(&fs));
        if !read_only {
            // Mark the filesystem as not cleanly unmounted while mounted.
            fs.set_state(false)?;
        }
        let _ = compat;
        Ok(fs)
    }

    fn arc(&self) -> Arc<Ext2Fs> {
        self.me
            .get()
            .and_then(|w| w.upgrade())
            .expect("ext2 fs dropped")
    }

    fn set_state(&self, clean: bool) -> KResult<()> {
        let mut sb = [0u8; 1024];
        self.dev.read_bytes(1024, &mut sb)?;
        put16(&mut sb, 58, if clean { 1 } else { 0 });
        if !clean {
            put32(&mut sb, 44, now()); // s_mtime
            let mc = u16le(&sb, 52);
            put16(&mut sb, 52, mc.wrapping_add(1));
        }
        put32(&mut sb, 48, now()); // s_wtime
        self.dev.write_bytes(1024, &sb)?;
        Ok(())
    }

    fn read_block(&self, b: u64, buf: &mut [u8]) -> KResult<()> {
        self.dev.read_bytes(b * self.block_size, buf)?;
        Ok(())
    }

    fn write_block(&self, b: u64, buf: &[u8]) -> KResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        self.dev.write_bytes(b * self.block_size, buf)?;
        Ok(())
    }

    fn inode_off(&self, ino: u32) -> KResult<u64> {
        if ino == 0 || ino > self.inodes_count {
            return Err(EIO);
        }
        let g = ((ino - 1) / self.inodes_per_group) as usize;
        let idx = ((ino - 1) % self.inodes_per_group) as u64;
        let meta = self.meta.lock();
        let table = meta.groups.get(g).ok_or(EIO)?.inode_table;
        Ok(table * self.block_size + idx * self.inode_size)
    }

    fn read_raw_inode(&self, ino: u32) -> KResult<RawInode> {
        let off = self.inode_off(ino)?;
        let mut b = vec![0u8; self.inode_size.max(128) as usize];
        self.dev.read_bytes(off, &mut b)?;
        Ok(RawInode::parse(&b))
    }

    fn write_raw_inode(&self, ino: u32, r: &RawInode) -> KResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let off = self.inode_off(ino)?;
        self.dev.write_bytes(off, &r.serialize())?;
        Ok(())
    }

    fn inode(&self, ino: u32) -> KResult<Arc<Ext2Inode>> {
        if let Some(i) = self.inodes.lock().get(&ino).and_then(|w| w.upgrade()) {
            return Ok(i);
        }
        let raw = self.read_raw_inode(ino)?;
        let i = Arc::new(Ext2Inode {
            fs: self.arc(),
            ino,
            st: Mutex::new(raw),
            alloc_hint: AtomicU64::new(0),
        });
        let mut t = self.inodes.lock();
        if let Some(existing) = t.get(&ino).and_then(|w| w.upgrade()) {
            return Ok(existing);
        }
        t.insert(ino, Arc::downgrade(&i));
        if t.len() > 256 {
            t.retain(|_, w| w.strong_count() > 0);
        }
        Ok(i)
    }

    fn flush_group(&self, meta: &Meta, g: usize) -> KResult<()> {
        let off = self.desc_table + g as u64 * self.desc_size;
        let mut d = vec![0u8; self.desc_size as usize];
        self.dev.read_bytes(off, &mut d)?;
        let gr = &meta.groups[g];
        put16(&mut d, 12, gr.free_blocks as u16);
        put16(&mut d, 14, gr.free_inodes as u16);
        put16(&mut d, 16, gr.used_dirs as u16);
        self.dev.write_bytes(off, &d)?;
        let mut sb = [0u8; 20];
        self.dev.read_bytes(1024, &mut sb)?;
        put32(&mut sb, 12, meta.free_blocks as u32);
        put32(&mut sb, 16, meta.free_inodes);
        self.dev.write_bytes(1024, &sb)?;
        Ok(())
    }

    /// Find and set a clear bit in a bitmap block; returns its index.
    fn bitmap_alloc(&self, bitmap_block: u64, limit: u32, start: u32) -> KResult<Option<u32>> {
        let mut bm = vec![0u8; self.block_size as usize];
        self.read_block(bitmap_block, &mut bm)?;
        let mut k = 0;
        while k < limit {
            let i = (start + k) % limit;
            let (byte, bit) = ((i / 8) as usize, i % 8);
            // Skip fully used bytes in one step.
            if bit == 0 && bm[byte] == 0xFF && k + 8 <= limit {
                k += 8;
                continue;
            }
            if bm[byte] & (1 << bit) == 0 {
                bm[byte] |= 1 << bit;
                self.dev.write_bytes(
                    bitmap_block * self.block_size + byte as u64,
                    &bm[byte..byte + 1],
                )?;
                return Ok(Some(i));
            }
            k += 1;
        }
        Ok(None)
    }

    fn bitmap_clear(&self, bitmap_block: u64, i: u32) -> KResult<()> {
        let off = bitmap_block * self.block_size + (i / 8) as u64;
        let mut b = [0u8; 1];
        self.dev.read_bytes(off, &mut b)?;
        b[0] &= !(1 << (i % 8));
        self.dev.write_bytes(off, &b)?;
        Ok(())
    }

    fn group_blocks(&self, g: usize, ngroups: usize) -> u32 {
        if g + 1 == ngroups {
            (self.blocks_count
                - self.first_data_block as u64
                - g as u64 * self.blocks_per_group as u64) as u32
        } else {
            self.blocks_per_group
        }
    }

    /// Allocate a zeroed block, preferring the group of `goal`.
    fn alloc_block(&self, goal: u64) -> KResult<u32> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut meta = self.meta.lock();
        let n = meta.groups.len();
        let goal_g = (goal.saturating_sub(self.first_data_block as u64)
            / self.blocks_per_group as u64) as usize
            % n.max(1);
        for k in 0..n {
            let g = (goal_g + k) % n;
            if meta.groups[g].free_blocks == 0 {
                continue;
            }
            let limit = self.group_blocks(g, n);
            let start = if k == 0 {
                (goal.saturating_sub(self.first_data_block as u64) % self.blocks_per_group as u64)
                    as u32
            } else {
                0
            };
            if let Some(i) = self.bitmap_alloc(
                meta.groups[g].block_bitmap,
                limit,
                start.min(limit.saturating_sub(1)),
            )? {
                meta.groups[g].free_blocks -= 1;
                meta.free_blocks = meta.free_blocks.saturating_sub(1);
                self.flush_group(&meta, g)?;
                drop(meta);
                let b = self.first_data_block as u64
                    + g as u64 * self.blocks_per_group as u64
                    + i as u64;
                self.write_block(b, &vec![0u8; self.block_size as usize])?;
                return Ok(b as u32);
            }
        }
        Err(ENOSPC)
    }

    fn free_block(&self, b: u64) -> KResult<()> {
        if b < self.first_data_block as u64 || b >= self.blocks_count {
            return Ok(());
        }
        let rel = b - self.first_data_block as u64;
        let g = (rel / self.blocks_per_group as u64) as usize;
        let i = (rel % self.blocks_per_group as u64) as u32;
        let mut meta = self.meta.lock();
        self.bitmap_clear(meta.groups[g].block_bitmap, i)?;
        meta.groups[g].free_blocks += 1;
        meta.free_blocks += 1;
        self.flush_group(&meta, g)
    }

    fn alloc_inode(&self, dir_hint: u32, is_dir: bool) -> KResult<u32> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut meta = self.meta.lock();
        let n = meta.groups.len();
        let hint_g = ((dir_hint.max(1) - 1) / self.inodes_per_group) as usize;
        for k in 0..n {
            let g = (hint_g + k) % n;
            if meta.groups[g].free_inodes == 0 {
                continue;
            }
            let start = if g == 0 { self.first_ino - 1 } else { 0 };
            let limit = self.inodes_per_group;
            if let Some(i) = self.bitmap_alloc(meta.groups[g].inode_bitmap, limit, start)? {
                if g == 0 && i < self.first_ino - 1 {
                    // Wrapped into the reserved range; undo.
                    self.bitmap_clear(meta.groups[g].inode_bitmap, i)?;
                    continue;
                }
                meta.groups[g].free_inodes -= 1;
                meta.free_inodes = meta.free_inodes.saturating_sub(1);
                if is_dir {
                    meta.groups[g].used_dirs += 1;
                }
                self.flush_group(&meta, g)?;
                return Ok(g as u32 * self.inodes_per_group + i + 1);
            }
        }
        Err(ENOSPC)
    }

    fn free_inode(&self, ino: u32, was_dir: bool) -> KResult<()> {
        let g = ((ino - 1) / self.inodes_per_group) as usize;
        let i = (ino - 1) % self.inodes_per_group;
        let mut meta = self.meta.lock();
        self.bitmap_clear(meta.groups[g].inode_bitmap, i)?;
        meta.groups[g].free_inodes += 1;
        meta.free_inodes += 1;
        if was_dir {
            meta.groups[g].used_dirs = meta.groups[g].used_dirs.saturating_sub(1);
        }
        self.flush_group(&meta, g)
    }

    fn ptrs_per_block(&self) -> u64 {
        self.block_size / 4
    }

    fn read_ptr(&self, block: u32, idx: u64) -> KResult<u32> {
        let mut b = [0u8; 4];
        self.dev
            .read_bytes(block as u64 * self.block_size + idx * 4, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn write_ptr(&self, block: u32, idx: u64, v: u32) -> KResult<()> {
        self.dev
            .write_bytes(block as u64 * self.block_size + idx * 4, &v.to_le_bytes())?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Extent trees (read-only)
    // ------------------------------------------------------------------

    fn extent_lookup(&self, node: &[u8], lblk: u32) -> KResult<Option<u64>> {
        if u16le(node, 0) != 0xF30A {
            return Err(EIO);
        }
        let entries = u16le(node, 2) as usize;
        let depth = u16le(node, 6);
        if depth == 0 {
            for e in 0..entries {
                let o = 12 + e * 12;
                let first = u32le(node, o);
                let mut len = u16le(node, o + 4) as u32;
                if len > 32768 {
                    len -= 32768; // uninitialized extent: reads as zeros
                    if lblk >= first && lblk < first + len {
                        return Ok(None);
                    }
                    continue;
                }
                let start = (u16le(node, o + 6) as u64) << 32 | u32le(node, o + 8) as u64;
                if lblk >= first && lblk < first + len {
                    return Ok(Some(start + (lblk - first) as u64));
                }
            }
            return Ok(None);
        }
        let mut child = None;
        for e in 0..entries {
            let o = 12 + e * 12;
            if u32le(node, o) <= lblk {
                child = Some(u32le(node, o + 4) as u64 | (u16le(node, o + 8) as u64) << 32);
            } else {
                break;
            }
        }
        let Some(c) = child else { return Ok(None) };
        let mut buf = vec![0u8; self.block_size as usize];
        self.read_block(c, &mut buf)?;
        self.extent_lookup(&buf, lblk)
    }
}

impl FileSystem for Ext2Fs {
    fn root(&self) -> Arc<dyn Inode> {
        self.inode(ROOT_INO).expect("ext2: cannot read root inode")
    }
    fn name(&self) -> &'static str {
        self.kind
    }
    fn sync(&self) -> KResult<()> {
        self.dev.sync()
    }
    fn statfs(&self) -> StatFs {
        let m = self.meta.lock();
        StatFs {
            fs_type: 0xEF53,
            block_size: self.block_size,
            blocks: self.blocks_count,
            blocks_free: m.free_blocks,
            files: self.inodes_count as u64,
            files_free: m.free_inodes as u64,
            name_max: 255,
        }
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
}

impl Drop for Ext2Fs {
    fn drop(&mut self) {
        if !self.read_only {
            let _ = self.set_state(true);
            let _ = self.dev.sync();
        }
    }
}

// ---------------------------------------------------------------------------
// Inodes
// ---------------------------------------------------------------------------

impl Ext2Inode {
    fn kind(&self) -> FileType {
        self.st.lock().kind()
    }

    fn save(&self, st: &RawInode) -> KResult<()> {
        self.fs.write_raw_inode(self.ino, st)
    }

    /// Physical block for logical block `l`; allocates when `create`.
    fn bmap(&self, st: &mut RawInode, l: u64, create: bool) -> KResult<Option<u64>> {
        let fs = &self.fs;
        if st.flags & FL_EXTENTS != 0 {
            if create {
                return Err(EROFS);
            }
            let mut node = vec![0u8; 60];
            for (i, v) in st.block.iter().enumerate() {
                put32(&mut node, i * 4, *v);
            }
            return fs.extent_lookup(&node, l as u32);
        }
        let p = fs.ptrs_per_block();
        let mut new_blocks = 0u64;
        let hint = &self.alloc_hint;
        let first = st.block[0] as u64;
        let mut alloc = |fs: &Ext2Fs| -> KResult<u32> {
            new_blocks += 1;
            let goal = match hint.load(Ordering::Relaxed) {
                0 => first,
                h => h + 1,
            };
            let b = fs.alloc_block(goal)?;
            hint.store(b as u64, Ordering::Relaxed);
            Ok(b)
        };
        let result = if l < 12 {
            if st.block[l as usize] == 0 {
                if !create {
                    return Ok(None);
                }
                st.block[l as usize] = alloc(fs)?;
            }
            Some(st.block[l as usize] as u64)
        } else {
            // Path through indirect levels.
            let (slot, path): (usize, Vec<u64>) = {
                let l = l - 12;
                if l < p {
                    (12, vec![l])
                } else if l < p + p * p {
                    let l = l - p;
                    (13, vec![l / p, l % p])
                } else {
                    let l = l - p - p * p;
                    if l >= p * p * p {
                        return Err(EFBIG);
                    }
                    (14, vec![l / (p * p), (l / p) % p, l % p])
                }
            };
            if st.block[slot] == 0 {
                if !create {
                    return Ok(None);
                }
                st.block[slot] = alloc(fs)?;
            }
            let mut cur = st.block[slot];
            let mut out = None;
            for (depth, &idx) in path.iter().enumerate() {
                let mut next = fs.read_ptr(cur, idx)?;
                if next == 0 {
                    if !create {
                        return Ok(None);
                    }
                    next = alloc(fs)?;
                    fs.write_ptr(cur, idx, next)?;
                }
                if depth + 1 == path.len() {
                    out = Some(next as u64);
                }
                cur = next;
            }
            out
        };
        st.blocks512 += new_blocks * (fs.block_size / 512);
        Ok(result)
    }

    fn read_data(&self, st: &mut RawInode, off: u64, buf: &mut [u8]) -> KResult<usize> {
        if off >= st.size {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(st.size - off) as usize;
        if st.flags & FL_INLINE != 0 {
            // Inline data lives in i_block (first 60 bytes).
            let mut inline = vec![0u8; 60];
            for (i, v) in st.block.iter().enumerate() {
                put32(&mut inline, i * 4, *v);
            }
            let end = (off as usize + len).min(60);
            let n = end.saturating_sub(off as usize);
            buf[..n].copy_from_slice(&inline[off as usize..end]);
            buf[n..len].fill(0);
            return Ok(len);
        }
        let bs = self.fs.block_size;
        let mut done = 0;
        while done < len {
            let pos = off + done as u64;
            let l = pos / bs;
            let within = pos % bs;
            let n = ((bs - within) as usize).min(len - done);
            match self.bmap(st, l, false)? {
                Some(pb) => {
                    self.fs
                        .dev
                        .read_bytes(pb * bs + within, &mut buf[done..done + n])?;
                }
                None => buf[done..done + n].fill(0),
            }
            done += n;
        }
        Ok(len)
    }

    fn write_data(&self, st: &mut RawInode, off: u64, buf: &[u8]) -> KResult<usize> {
        if self.fs.read_only {
            return Err(EROFS);
        }
        let bs = self.fs.block_size;
        let mut done = 0;
        let res = (|| {
            while done < buf.len() {
                let pos = off + done as u64;
                let l = pos / bs;
                let within = pos % bs;
                let n = ((bs - within) as usize).min(buf.len() - done);
                let pb = self.bmap(st, l, true)?.ok_or(EIO)?;
                self.fs
                    .dev
                    .write_bytes(pb * bs + within, &buf[done..done + n])?;
                done += n;
            }
            Ok(())
        })();
        if done > 0 {
            st.size = st.size.max(off + done as u64);
            let t = now();
            st.mtime = t;
            st.ctime = t;
            self.save(st)?;
        }
        res.map(|_| done)
    }

    /// Free blocks past `keep` logical blocks.
    fn free_from(&self, st: &mut RawInode, keep: u64) -> KResult<()> {
        if st.flags & FL_EXTENTS != 0 {
            return Err(EROFS);
        }
        let fs = self.fs.clone();
        let p = fs.ptrs_per_block();
        let per = fs.block_size / 512;
        let mut freed = 0u64;
        for i in (keep as usize).min(12)..12 {
            if st.block[i] != 0 {
                fs.free_block(st.block[i] as u64)?;
                st.block[i] = 0;
                freed += 1;
            }
        }
        // Recursively free an indirect tree. `base` = first logical block
        // covered by `blk`, `span` = blocks covered per pointer.
        fn walk(
            fs: &Ext2Fs,
            blk: u32,
            level: u32,
            base: u64,
            keep: u64,
            freed: &mut u64,
        ) -> KResult<bool> {
            let p = fs.ptrs_per_block();
            let span = p.pow(level - 1);
            let mut all_free = true;
            for idx in 0..p {
                let first = base + idx * span;
                let child = fs.read_ptr(blk, idx)?;
                if child == 0 {
                    continue;
                }
                if first + span <= keep {
                    all_free = false;
                    continue;
                }
                let child_empty = if level == 1 {
                    true
                } else {
                    walk(fs, child, level - 1, first, keep, freed)?
                };
                if child_empty && first >= keep {
                    fs.free_block(child as u64)?;
                    fs.write_ptr(blk, idx, 0)?;
                    *freed += 1;
                } else {
                    // Partially kept subtree.
                    all_free = false;
                }
            }
            Ok(all_free)
        }
        let bases = [12u64, 12 + p, 12 + p + p * p];
        for (k, slot) in (12..15).enumerate() {
            if st.block[slot] == 0 {
                continue;
            }
            let level = k as u32 + 1;
            if walk(&fs, st.block[slot], level, bases[k], keep, &mut freed)? && bases[k] >= keep {
                fs.free_block(st.block[slot] as u64)?;
                st.block[slot] = 0;
                freed += 1;
            }
        }
        st.blocks512 = st.blocks512.saturating_sub(freed * per);
        Ok(())
    }

    // Directory helpers ------------------------------------------------

    /// All entries: (name, inode, type code, logical block, offset in block).
    fn dir_entries(&self) -> KResult<Vec<RawDirEntry>> {
        let mut st = self.st.lock();
        if st.kind() != FileType::Directory {
            return Err(ENOTDIR);
        }
        let bs = self.fs.block_size as usize;
        let nblocks = st.size.div_ceil(bs as u64);
        let mut out = Vec::new();
        let mut buf = vec![0u8; bs];
        for l in 0..nblocks {
            let Some(pb) = self.bmap(&mut st, l, false)? else {
                continue;
            };
            self.fs.read_block(pb, &mut buf)?;
            let mut o = 0;
            while o + 8 <= bs {
                let ino = u32le(&buf, o);
                let rec = u16le(&buf, o + 4) as usize;
                let nl = buf[o + 6] as usize;
                if rec < 8 || o + rec > bs {
                    break;
                }
                if ino != 0 && o + 8 + nl <= bs {
                    let name = String::from_utf8_lossy(&buf[o + 8..o + 8 + nl]).into_owned();
                    out.push((name, ino, buf[o + 7], l, o));
                }
                o += rec;
            }
        }
        Ok(out)
    }

    fn find_entry(&self, name: &str) -> KResult<(u32, u8, u64, usize)> {
        self.dir_entries()?
            .into_iter()
            .find(|(n, ..)| n == name)
            .map(|(_, i, t, l, o)| (i, t, l, o))
            .ok_or(ENOENT)
    }

    fn add_entry(&self, name: &str, ino: u32, kind: FileType) -> KResult<()> {
        if name.len() > 255 || name.is_empty() {
            return Err(ENAMETOOLONG);
        }
        let fs = self.fs.clone();
        let bs = fs.block_size as usize;
        let need = (8 + name.len()).next_multiple_of(4);
        let mut st = self.st.lock();
        // Writing entries would desynchronise an htree index.
        st.flags &= !FL_INDEX;
        let nblocks = st.size.div_ceil(bs as u64);
        let mut buf = vec![0u8; bs];
        let tcode = if fs.filetype { ft_code(kind) } else { 0 };
        let write_rec = |buf: &mut [u8], o: usize, rec: usize| {
            put32(buf, o, ino);
            put16(buf, o + 4, rec as u16);
            buf[o + 6] = name.len() as u8;
            buf[o + 7] = tcode;
            buf[o + 8..o + 8 + name.len()].copy_from_slice(name.as_bytes());
        };
        for l in 0..nblocks {
            let Some(pb) = self.bmap(&mut st, l, false)? else {
                continue;
            };
            fs.read_block(pb, &mut buf)?;
            let mut o = 0;
            while o + 8 <= bs {
                let eino = u32le(&buf, o);
                let rec = u16le(&buf, o + 4) as usize;
                if rec < 8 || o + rec > bs {
                    break;
                }
                let used = if eino == 0 {
                    0
                } else {
                    (8 + buf[o + 6] as usize).next_multiple_of(4)
                };
                if rec - used >= need {
                    if eino == 0 {
                        write_rec(&mut buf, o, rec);
                    } else {
                        put16(&mut buf, o + 4, used as u16);
                        write_rec(&mut buf, o + used, rec - used);
                    }
                    fs.write_block(pb, &buf)?;
                    let t = now();
                    st.mtime = t;
                    st.ctime = t;
                    return self.save(&st);
                }
                o += rec;
            }
        }
        // Append a new block.
        let pb = self.bmap(&mut st, nblocks, true)?.ok_or(EIO)?;
        buf.fill(0);
        write_rec(&mut buf, 0, bs);
        fs.write_block(pb, &buf)?;
        st.size = (nblocks + 1) * bs as u64;
        let t = now();
        st.mtime = t;
        st.ctime = t;
        self.save(&st)
    }

    fn remove_entry(&self, l: u64, off: usize) -> KResult<()> {
        let fs = self.fs.clone();
        let bs = fs.block_size as usize;
        let mut st = self.st.lock();
        st.flags &= !FL_INDEX;
        let pb = self.bmap(&mut st, l, false)?.ok_or(EIO)?;
        let mut buf = vec![0u8; bs];
        fs.read_block(pb, &mut buf)?;
        // Merge into the previous record, or clear the inode if first.
        let mut prev = None;
        let mut o = 0;
        while o < off {
            prev = Some(o);
            o += u16le(&buf, o + 4) as usize;
        }
        let rec = u16le(&buf, off + 4);
        match prev {
            Some(p) => {
                let prec = u16le(&buf, p + 4);
                put16(&mut buf, p + 4, prec + rec);
            }
            None => put32(&mut buf, off, 0),
        }
        fs.write_block(pb, &buf)?;
        let t = now();
        st.mtime = t;
        st.ctime = t;
        self.save(&st)
    }

    fn set_entry_ino(&self, name: &str, ino: u32) -> KResult<()> {
        let (_, _, l, off) = self.find_entry(name)?;
        let mut st = self.st.lock();
        let pb = self.bmap(&mut st, l, false)?.ok_or(EIO)?;
        drop(st);
        self.fs
            .dev
            .write_bytes(pb * self.fs.block_size + off as u64, &ino.to_le_bytes())?;
        Ok(())
    }

    fn is_empty_dir(&self) -> KResult<bool> {
        Ok(self
            .dir_entries()?
            .iter()
            .all(|(n, ..)| n == "." || n == ".."))
    }

    fn change_links(&self, delta: i32) -> KResult<()> {
        let mut st = self.st.lock();
        st.links = (st.links as i32 + delta).max(0) as u16;
        st.ctime = now();
        self.save(&st)
    }

    /// Release the inode's storage once it has no links and no users.
    fn release(&self) -> KResult<()> {
        let mut st = self.st.lock();
        let was_dir = st.kind() == FileType::Directory;
        let fast_symlink = st.kind() == FileType::Symlink && st.blocks512 == 0;
        if !fast_symlink {
            self.free_from(&mut st, 0)?;
        }
        st.size = 0;
        st.dtime = now();
        st.mode = 0;
        st.block = [0; 15];
        self.save(&st)?;
        drop(st);
        self.fs.free_inode(self.ino, was_dir)
    }

    fn new_inode(&self, kind: FileType, mode: u32) -> KResult<Arc<Ext2Inode>> {
        let fs = &self.fs;
        let ino = fs.alloc_inode(self.ino, kind == FileType::Directory)?;
        let t = now();
        let (uid, gid) = crate::process::current()
            .map(|p| (p.uid.load(Ordering::Relaxed), p.gid.load(Ordering::Relaxed)))
            .unwrap_or((0, 0));
        let mut raw = vec![0u8; fs.inode_size.max(128) as usize];
        if fs.inode_size > 128 {
            // i_extra_isize: keep only the fields we know (none).
            put16(&mut raw, 128, 0);
        }
        let mut r = RawInode::parse(&raw);
        r.mode = (kind.mode_bits() as u16) | (mode & 0o7777) as u16;
        r.uid = uid;
        r.gid = gid;
        r.atime = t;
        r.ctime = t;
        r.mtime = t;
        r.links = 1;
        fs.write_raw_inode(ino, &r)?;
        // Drop any stale cached object for a reused inode number.
        fs.inodes.lock().remove(&ino);
        fs.inode(ino)
    }

    fn dir_guard(&self) -> KResult<()> {
        if self.kind() != FileType::Directory {
            return Err(ENOTDIR);
        }
        if self.fs.read_only {
            return Err(EROFS);
        }
        Ok(())
    }
}

impl Drop for Ext2Inode {
    fn drop(&mut self) {
        let orphan = {
            let st = self.st.lock();
            st.links == 0 && st.mode != 0
        };
        if orphan && !self.fs.read_only {
            let _ = self.release();
        }
    }
}

impl Inode for Ext2Inode {
    fn metadata(&self) -> KResult<Metadata> {
        let st = self.st.lock();
        let mut m = Metadata::new(st.kind(), (st.mode & 0o7777) as u32);
        m.ino = self.ino as u64;
        m.dev = self.fs.id as u64;
        m.nlink = st.links as u32;
        m.uid = st.uid;
        m.gid = st.gid;
        m.size = st.size;
        m.blksize = self.fs.block_size as u32;
        m.blocks = st.blocks512;
        m.atime = st.atime as u64;
        m.mtime = st.mtime as u64;
        m.ctime = st.ctime as u64;
        Ok(m)
    }

    fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
        if self.kind() != FileType::Directory {
            return Err(ENOTDIR);
        }
        let (ino, ..) = self.find_entry(name)?;
        Ok(self.fs.inode(ino)?)
    }

    fn create(&self, name: &str, kind: FileType, mode: u32) -> KResult<Arc<dyn Inode>> {
        self.dir_guard()?;
        if self.find_entry(name).is_ok() {
            return Err(EEXIST);
        }
        let child = self.new_inode(kind, mode)?;
        if kind == FileType::Directory {
            let bs = self.fs.block_size as usize;
            let mut buf = vec![0u8; bs];
            let tdir = if self.fs.filetype { 2 } else { 0 };
            put32(&mut buf, 0, child.ino);
            put16(&mut buf, 4, 12);
            buf[6] = 1;
            buf[7] = tdir;
            buf[8] = b'.';
            put32(&mut buf, 12, self.ino);
            put16(&mut buf, 16, (bs - 12) as u16);
            buf[18] = 2;
            buf[19] = tdir;
            buf[20] = b'.';
            buf[21] = b'.';
            let mut st = child.st.lock();
            let pb = child.bmap(&mut st, 0, true)?.ok_or(EIO)?;
            self.fs.write_block(pb, &buf)?;
            st.size = bs as u64;
            st.links = 2;
            child.save(&st)?;
            drop(st);
            self.change_links(1)?;
        }
        if let Err(e) = self.add_entry(name, child.ino, kind) {
            child.st.lock().links = 0;
            return Err(e);
        }
        Ok(child)
    }

    fn link(&self, name: &str, target: &Arc<dyn Inode>) -> KResult<()> {
        self.dir_guard()?;
        let t = target.as_any().downcast_ref::<Ext2Inode>().ok_or(EXDEV)?;
        if t.fs.id != self.fs.id {
            return Err(EXDEV);
        }
        if t.kind() == FileType::Directory {
            return Err(EPERM);
        }
        if self.find_entry(name).is_ok() {
            return Err(EEXIST);
        }
        self.add_entry(name, t.ino, t.kind())?;
        t.change_links(1)
    }

    fn unlink(&self, name: &str) -> KResult<()> {
        self.dir_guard()?;
        let (ino, _, l, off) = self.find_entry(name)?;
        let child = self.fs.inode(ino)?;
        if child.kind() == FileType::Directory {
            return Err(EISDIR);
        }
        self.remove_entry(l, off)?;
        child.change_links(-1)
    }

    fn rmdir(&self, name: &str) -> KResult<()> {
        self.dir_guard()?;
        if name == "." || name == ".." {
            return Err(EINVAL);
        }
        let (ino, _, l, off) = self.find_entry(name)?;
        let child = self.fs.inode(ino)?;
        if child.kind() != FileType::Directory {
            return Err(ENOTDIR);
        }
        if !child.is_empty_dir()? {
            return Err(ENOTEMPTY);
        }
        self.remove_entry(l, off)?;
        child.st.lock().links = 0;
        {
            let st = child.st.lock();
            child.save(&st)?;
        }
        self.change_links(-1)
    }

    fn rename(&self, old: &str, new_dir: &Arc<dyn Inode>, new: &str) -> KResult<()> {
        self.dir_guard()?;
        let nd = new_dir.as_any().downcast_ref::<Ext2Inode>().ok_or(EXDEV)?;
        if nd.fs.id != self.fs.id {
            return Err(EXDEV);
        }
        let (ino, _, l, off) = self.find_entry(old)?;
        let child = self.fs.inode(ino)?;
        let is_dir = child.kind() == FileType::Directory;
        if let Ok((tino, ..)) = nd.find_entry(new) {
            if tino == ino {
                return Ok(());
            }
            let target = self.fs.inode(tino)?;
            let t_dir = target.kind() == FileType::Directory;
            if t_dir != is_dir {
                return Err(if t_dir { EISDIR } else { ENOTDIR });
            }
            if t_dir {
                nd.rmdir(new)?;
            } else {
                nd.unlink(new)?;
            }
        }
        nd.add_entry(new, ino, child.kind())?;
        // Re-find: adding may have split the record we are about to remove
        // when both names live in the same directory.
        let (_, _, l2, off2) = if nd.ino == self.ino {
            self.dir_entries()?
                .into_iter()
                .find(|(n, i, ..)| n == old && *i == ino)
                .map(|(_, i, t, l, o)| (i, t, l, o))
                .ok_or(EIO)?
        } else {
            (ino, 0, l, off)
        };
        self.remove_entry(l2, off2)?;
        if is_dir && nd.ino != self.ino {
            child.set_entry_ino("..", nd.ino)?;
            self.change_links(-1)?;
            nd.change_links(1)?;
        }
        let mut st = child.st.lock();
        st.ctime = now();
        child.save(&st)
    }

    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        let entries = self.dir_entries()?;
        let mut out = Vec::with_capacity(entries.len());
        for (name, ino, t, ..) in entries {
            if name == "." || name == ".." {
                continue;
            }
            let kind = if self.fs.filetype && t != 0 {
                ft_from_code(t)
            } else {
                self.fs
                    .inode(ino)
                    .map(|i| i.kind())
                    .unwrap_or(FileType::Regular)
            };
            out.push(DirEntry {
                name,
                ino: ino as u64,
                kind,
            });
        }
        Ok(out)
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        let mut st = self.st.lock();
        if st.kind() == FileType::Directory {
            return Err(EISDIR);
        }
        self.read_data(&mut st, off, buf)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        let mut st = self.st.lock();
        if st.kind() == FileType::Directory {
            return Err(EISDIR);
        }
        if st.flags & (FL_EXTENTS | FL_INLINE) != 0 {
            return Err(EROFS);
        }
        self.write_data(&mut st, off, buf)
    }

    fn truncate(&self, size: u64) -> KResult<()> {
        if self.fs.read_only {
            return Err(EROFS);
        }
        let mut st = self.st.lock();
        if st.kind() == FileType::Directory {
            return Err(EISDIR);
        }
        let bs = self.fs.block_size;
        if size < st.size {
            self.free_from(&mut st, size.div_ceil(bs))?;
            // Zero the tail of the last kept block.
            if !size.is_multiple_of(bs)
                && let Some(pb) = self.bmap(&mut st, size / bs, false)?
            {
                let z = vec![0u8; (bs - size % bs) as usize];
                self.fs.dev.write_bytes(pb * bs + size % bs, &z)?;
            }
        }
        st.size = size;
        let t = now();
        st.mtime = t;
        st.ctime = t;
        self.save(&st)
    }

    fn symlink(&self, name: &str, target: &str) -> KResult<()> {
        self.dir_guard()?;
        if self.find_entry(name).is_ok() {
            return Err(EEXIST);
        }
        let child = self.new_inode(FileType::Symlink, 0o777)?;
        {
            let mut st = child.st.lock();
            if target.len() < 60 {
                let mut b = [0u8; 60];
                b[..target.len()].copy_from_slice(target.as_bytes());
                for i in 0..15 {
                    st.block[i] = u32le(&b, i * 4);
                }
                st.size = target.len() as u64;
                child.save(&st)?;
            } else {
                child.write_data(&mut st, 0, target.as_bytes())?;
            }
        }
        if let Err(e) = self.add_entry(name, child.ino, FileType::Symlink) {
            child.st.lock().links = 0;
            return Err(e);
        }
        Ok(())
    }

    fn readlink(&self) -> KResult<String> {
        let mut st = self.st.lock();
        if st.kind() != FileType::Symlink {
            return Err(EINVAL);
        }
        let size = st.size as usize;
        if st.blocks512 == 0 && size < 60 && st.flags & FL_EXTENTS == 0 {
            let mut b = [0u8; 60];
            for (i, v) in st.block.iter().enumerate() {
                put32(&mut b, i * 4, *v);
            }
            return Ok(String::from_utf8_lossy(&b[..size]).into_owned());
        }
        let mut b = vec![0u8; size];
        self.read_data(&mut st, 0, &mut b)?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    fn chmod(&self, mode: u32) -> KResult<()> {
        let mut st = self.st.lock();
        st.mode = (st.mode & S_IFMT) | (mode & 0o7777) as u16;
        st.ctime = now();
        self.save(&st)
    }

    fn chown(&self, uid: u32, gid: u32) -> KResult<()> {
        let mut st = self.st.lock();
        if uid != u32::MAX {
            st.uid = uid;
        }
        if gid != u32::MAX {
            st.gid = gid;
        }
        st.ctime = now();
        self.save(&st)
    }

    fn set_times(&self, atime: Option<u64>, mtime: Option<u64>) -> KResult<()> {
        let mut st = self.st.lock();
        if let Some(a) = atime {
            st.atime = a as u32;
        }
        if let Some(m) = mtime {
            st.mtime = m as u32;
        }
        st.ctime = now();
        self.save(&st)
    }

    fn sync(&self) -> KResult<()> {
        self.fs.dev.sync()
    }

    fn fs_id(&self) -> usize {
        self.fs.id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

const _: u32 = COMPAT_DIR_INDEX;
