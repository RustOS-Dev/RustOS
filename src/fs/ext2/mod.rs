//! ext2, ext3 and ext4, read/write.
//!
//! Supported for writing: extents (allocation, uninitialized extents,
//! truncation), 64-bit block numbers, flex_bg, meta_bg, uninitialized block
//! groups, metadata checksums (crc32c) and the older group-descriptor
//! checksums, hashed directory indexes (htree, including large_dir and
//! casefold), inline data, bigalloc, quotas (user, group, project),
//! ea_inode attribute values, and the jbd2 journal (replayed at mount,
//! including fast commits; data=ordered, writeback or journal, see
//! `journal.rs`). Filesystems with other features (encrypt, verity, an
//! external journal, ...) are mounted read-only; the kernel log lists the
//! features that caused it.
//!
//! All metadata (superblock, group descriptors, bitmaps, inode tables,
//! directory, extent and indirect blocks) is read and written through
//! [`Ext2Fs::mread`]/[`Ext2Fs::mwrite`]/[`Ext2Fs::modify`], which route it
//! through the running journal transaction; file data goes to the device
//! directly.

mod extent;
mod fastcommit;
mod htree;
mod inline;
mod journal;
mod quota;
mod xattr;

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
use ext4_core::csum;

const MAGIC: u16 = 0xEF53;
const ROOT_INO: u32 = 2;

// Feature flags.
const COMPAT_HAS_JOURNAL: u32 = 0x4;
const COMPAT_DIR_INDEX: u32 = 0x20;
const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_RECOVER: u32 = 0x4;
const INCOMPAT_META_BG: u32 = 0x10;
const INCOMPAT_EXTENTS: u32 = 0x40;
const INCOMPAT_64BIT: u32 = 0x80;
const INCOMPAT_FLEX_BG: u32 = 0x200;
const INCOMPAT_EA_INODE: u32 = 0x400;
const INCOMPAT_CSUM_SEED: u32 = 0x2000;
const INCOMPAT_LARGEDIR: u32 = 0x4000;
const INCOMPAT_INLINE_DATA: u32 = 0x8000;
const INCOMPAT_ENCRYPT: u32 = 0x10000;
const INCOMPAT_CASEFOLD: u32 = 0x20000;
/// Features we can read.
const INCOMPAT_READ: u32 = INCOMPAT_FILETYPE
    | INCOMPAT_RECOVER
    | INCOMPAT_META_BG
    | INCOMPAT_EXTENTS
    | INCOMPAT_64BIT
    | INCOMPAT_FLEX_BG
    | INCOMPAT_EA_INODE
    | INCOMPAT_CSUM_SEED
    | INCOMPAT_LARGEDIR
    | INCOMPAT_INLINE_DATA
    | INCOMPAT_CASEFOLD
    | INCOMPAT_ENCRYPT;
/// Features we can keep consistent while writing.
/// encrypt mounts read-only: unencrypted files read normally, encrypted
/// names and contents are not decrypted.
const INCOMPAT_WRITE: u32 = INCOMPAT_READ & !INCOMPAT_ENCRYPT;
const RO_COMPAT_SPARSE: u32 = 0x1;
const RO_COMPAT_LARGE_FILE: u32 = 0x2;
const RO_COMPAT_HUGE_FILE: u32 = 0x8;
const RO_COMPAT_GDT_CSUM: u32 = 0x10;
const RO_COMPAT_DIR_NLINK: u32 = 0x20;
const RO_COMPAT_EXTRA_ISIZE: u32 = 0x40;
const RO_COMPAT_QUOTA: u32 = 0x100;
const RO_COMPAT_BIGALLOC: u32 = 0x200;
const RO_COMPAT_PROJECT: u32 = 0x2000;
const RO_COMPAT_METADATA_CSUM: u32 = 0x400;
const RO_COMPAT_WRITE: u32 = RO_COMPAT_SPARSE
    | RO_COMPAT_LARGE_FILE
    | RO_COMPAT_HUGE_FILE
    | RO_COMPAT_GDT_CSUM
    | RO_COMPAT_DIR_NLINK
    | RO_COMPAT_EXTRA_ISIZE
    | RO_COMPAT_METADATA_CSUM
    | RO_COMPAT_BIGALLOC
    | RO_COMPAT_QUOTA
    | RO_COMPAT_PROJECT;

// Superblock offsets.
const SB_FREE_BLOCKS: usize = 0x0C;
const SB_FREE_INODES: usize = 0x10;
const SB_STATE: usize = 0x3A;
const SB_INCOMPAT: usize = 0x60;
const SB_JOURNAL_INUM: usize = 0xE0;
const SB_LAST_ORPHAN: usize = 0xE8;
const SB_HASH_SEED: usize = 0xEC;
const SB_FREE_BLOCKS_HI: usize = 0x158;
const SB_FLAGS: usize = 0x160;

// Group descriptor flags.
const BG_INODE_UNINIT: u16 = 0x1;
const BG_BLOCK_UNINIT: u16 = 0x2;

const FL_INDEX: u32 = 0x1000;
const FL_HUGE_FILE: u32 = 0x40000;
const FL_EXTENTS: u32 = 0x80000;
const FL_INLINE: u32 = 0x1000_0000;
const FL_CASEFOLD: u32 = 0x4000_0000;

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
    flags: u16,
    itable_unused: u32,
    block_bitmap_csum: u32,
    inode_bitmap_csum: u32,
}

pub struct Ext2Fs {
    dev: Arc<CachedDevice>,
    block_size: u64,
    blocks_count: u64,
    inodes_count: u32,
    blocks_per_group: u32,
    /// bigalloc: log2 of blocks per cluster (0 without bigalloc). Block
    /// bitmaps and group free counts are in clusters.
    cbits: u32,
    clusters_per_group: u32,
    inodes_per_group: u32,
    inode_size: u64,
    first_data_block: u32,
    desc_size: u64,
    first_ino: u32,
    /// Blocks after each superblock copy that hold group descriptors
    /// (including reserved ones for resizing; with meta_bg, only those
    /// before `first_meta_bg`).
    gdt_blocks: u64,
    layout: DescLayout,
    filetype: bool,
    read_only: bool,
    /// New files get extent trees.
    extents: bool,
    /// metadata_csum: crc32c checksums on all metadata.
    csum: bool,
    /// gdt_csum (uninit_bg): crc16 group descriptor checksums.
    gdt_csum: bool,
    sparse_super: bool,
    is64: bool,
    largedir: bool,
    /// dir_index: linear directories become indexed when they outgrow
    /// one block (hashed with `def_hash_version`).
    dir_index: bool,
    def_hash_version: u8,
    /// casefold with the UTF-8 encoding: directories with FL_CASEFOLD
    /// match names case-insensitively.
    casefold: bool,
    seed: u32,
    uuid: [u8; 16],
    hash_seed: [u32; 4],
    hash_unsigned: bool,
    pub label: String,
    pub kind: &'static str,
    id: usize,
    journal: spin::Once<journal::Journal>,
    quota: spin::Once<crate::sync::Mutex<quota::Quotas>>,
    /// data=journal: file data is logged like metadata.
    data_journal: core::sync::atomic::AtomicBool,
    /// Free counts changed since the superblock was last written.
    counts_dirty: core::sync::atomic::AtomicBool,
    /// Serialises read-modify-write of metadata blocks.
    rmw: Mutex<()>,
    /// Serialises edits of the orphan list.
    orphans: Mutex<()>,
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
    /// `i_blocks` as stored (512-byte units unless FL_HUGE_FILE).
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
    /// A casefolded directory (names compare case-insensitively).
    folded: bool,
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

    fn generation(&self) -> u32 {
        u32le(&self.raw, csum::INODE_GENERATION)
    }

    /// Extended-attribute block (`i_file_acl`).
    fn xattr_block(&self) -> u64 {
        u32le(&self.raw, 104) as u64 | (u16le(&self.raw, 118) as u64) << 32
    }

    fn set_xattr_block(&mut self, b: u64) {
        put32(&mut self.raw, 104, b as u32);
        put16(&mut self.raw, 118, (b >> 32) as u16);
    }

    /// The 60-byte `i_block` area as bytes (extent root, inline data or
    /// a fast symlink target).
    fn block_bytes(&self) -> [u8; 60] {
        let mut b = [0u8; 60];
        for (i, v) in self.block.iter().enumerate() {
            put32(&mut b, i * 4, *v);
        }
        b
    }

    fn set_block_bytes(&mut self, b: &[u8]) {
        for i in 0..15 {
            self.block[i] = u32le(b, i * 4);
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

/// Names of the feature bits in `bits` (for the read-only mount message).
fn feature_names(incompat: u32, ro_compat: u32) -> String {
    let mut s = String::new();
    let mut add = |n: &str| {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(n);
    };
    for (bit, name) in [
        (0x1, "compression"),
        (0x8, "journal_dev"),
        (0x10, "meta_bg"),
        (0x100, "mmp"),
        (0x1000, "dirdata"),
        (0x8000, "inline_data"),
        (0x10000, "encrypt"),
        (0x20000, "casefold"),
    ] {
        if incompat & bit != 0 {
            add(name);
        }
    }
    for (bit, name) in [
        (0x4, "btree_dir"),
        (0x80, "has_snapshot"),
        (0x100, "quota"),
        (0x200, "bigalloc"),
        (0x800, "replica"),
        (0x1000, "read-only"),
        (0x2000, "project"),
        (0x4000, "shared_blocks"),
        (0x8000, "verity"),
        (0x10000, "orphan_present"),
    ] {
        if ro_compat & bit != 0 {
            add(name);
        }
    }
    if s.is_empty() {
        s = alloc::format!("incompat {:#x} ro_compat {:#x}", incompat, ro_compat);
    }
    s
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
    } else if compat & COMPAT_HAS_JOURNAL != 0 {
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

/// Whether group `g` holds a superblock backup (and, without meta_bg, a
/// copy of the descriptor table).
fn has_super(sparse_super: bool, g: u64) -> bool {
    if !sparse_super || g <= 1 {
        return true;
    }
    [3u64, 5, 7].iter().any(|&p| {
        let mut x = p;
        while x < g {
            x *= p;
        }
        x == g
    })
}

/// Where the group descriptors live. Without meta_bg they follow the
/// superblock in one table. With meta_bg, the blocks before
/// `first_meta_bg` still do; after that, each meta-group (the groups whose
/// descriptors share one block) keeps its descriptor block in its first
/// group, with backups in the second and last.
#[derive(Clone, Copy)]
struct DescLayout {
    block_size: u64,
    desc_size: u64,
    first_data_block: u64,
    blocks_per_group: u64,
    meta_bg: bool,
    first_meta_bg: u64,
    sparse_super: bool,
}

impl DescLayout {
    fn per_block(&self) -> u64 {
        self.block_size / self.desc_size
    }

    /// Block holding group `g`'s descriptor, and its byte offset there.
    fn locate(&self, g: u64) -> (u64, usize) {
        let dpb = self.per_block();
        let i = g / dpb;
        let off = ((g % dpb) * self.desc_size) as usize;
        if !self.meta_bg || i < self.first_meta_bg {
            return (self.first_data_block + 1 + i, off);
        }
        let first = i * dpb;
        let sb = has_super(self.sparse_super, first) as u64;
        (
            self.first_data_block + first * self.blocks_per_group + sb,
            off,
        )
    }

    /// Descriptor blocks group `g` holds (meta_bg groups only; the first,
    /// second and last group of each meta-group).
    fn meta_desc_blocks(&self, g: u64) -> u64 {
        let dpb = self.per_block();
        let first = g / dpb * dpb;
        (g == first || g == first + 1 || g == first + dpb - 1) as u64
    }

    /// Read all `ngroups` descriptors through `read(block, buf)`.
    fn read(
        &self,
        ngroups: usize,
        mut read: impl FnMut(u64, &mut [u8]) -> KResult<()>,
    ) -> KResult<Vec<u8>> {
        let bs = self.block_size as usize;
        let dpb = self.per_block() as usize;
        let mut raw = vec![0u8; ngroups.div_ceil(dpb) * bs];
        for (i, chunk) in raw.chunks_mut(bs).enumerate() {
            read(self.locate((i * dpb) as u64).0, chunk)?;
        }
        raw.truncate(ngroups * self.desc_size as usize);
        Ok(raw)
    }
}

/// Parse the group descriptor table.
fn parse_groups(raw: &[u8], ngroups: usize, desc_size: usize, is64: bool) -> Vec<Group> {
    let mut groups = Vec::with_capacity(ngroups);
    for g in 0..ngroups {
        let d = &raw[g * desc_size..(g + 1) * desc_size];
        let big = is64 && desc_size >= 64;
        let hi = |o: usize| if big { u32le(d, o) as u64 } else { 0 };
        let hi16 = |o: usize| if big { u16le(d, o) as u32 } else { 0 };
        groups.push(Group {
            block_bitmap: u32le(d, 0) as u64 | hi(0x20) << 32,
            inode_bitmap: u32le(d, 4) as u64 | hi(0x24) << 32,
            inode_table: u32le(d, 8) as u64 | hi(0x28) << 32,
            free_blocks: u16le(d, 0x0C) as u32 | hi16(0x2C) << 16,
            free_inodes: u16le(d, 0x0E) as u32 | hi16(0x2E) << 16,
            used_dirs: u16le(d, 0x10) as u32 | hi16(0x30) << 16,
            flags: u16le(d, 0x12),
            block_bitmap_csum: u16le(d, 0x18) as u32 | hi16(0x38) << 16,
            inode_bitmap_csum: u16le(d, 0x1A) as u32 | hi16(0x3A) << 16,
            itable_unused: u16le(d, 0x1C) as u32 | hi16(0x32) << 16,
        });
    }
    groups
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
        if incompat & !INCOMPAT_READ != 0 {
            crate::println!(
                "[ext4] unsupported incompatible features: {}",
                feature_names(incompat & !INCOMPAT_READ, 0)
            );
            return Err(EINVAL);
        }
        let journal_inum = u32le(&sb, SB_JOURNAL_INUM);
        let has_journal = compat & COMPAT_HAS_JOURNAL != 0;
        let bad_incompat = incompat & !INCOMPAT_WRITE;
        let bad_ro = ro_compat & !RO_COMPAT_WRITE;
        let mut writable_features = bad_incompat == 0 && bad_ro == 0;
        if has_journal && journal_inum == 0 {
            crate::println!("[ext4] external journal: mounting read-only");
            writable_features = false;
        }
        let dev_writable = !want_ro && !dev.read_only();
        if dev_writable && !writable_features && (bad_incompat | bad_ro) != 0 {
            crate::println!(
                "[ext4] mounting read-only: unsupported features: {}",
                feature_names(bad_incompat, bad_ro)
            );
        }
        let mut read_only = !dev_writable || !writable_features;
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
        let bigalloc = ro_compat & RO_COMPAT_BIGALLOC != 0;
        let cbits = if bigalloc {
            u32le(&sb, 0x1C).saturating_sub(log)
        } else {
            0
        };
        let clusters_per_group = if bigalloc {
            u32le(&sb, 0x24)
        } else {
            blocks_per_group
        };
        if cbits > 16 || clusters_per_group << cbits != blocks_per_group {
            return Err(EINVAL);
        }
        if bigalloc && incompat & INCOMPAT_EXTENTS == 0 {
            return Err(EINVAL);
        }
        let inodes_per_group = u32le(&sb, 40);
        if blocks_per_group == 0 || inodes_per_group == 0 || blocks_count <= first_data_block as u64
        {
            return Err(EINVAL);
        }
        let (inode_size, first_ino) = if rev >= 1 {
            (u16le(&sb, 88) as u64, u32le(&sb, 84))
        } else {
            (128, 11)
        };
        let ngroups =
            (blocks_count - first_data_block as u64).div_ceil(blocks_per_group as u64) as usize;
        let meta_bg = incompat & INCOMPAT_META_BG != 0;
        let layout = DescLayout {
            block_size,
            desc_size,
            first_data_block: first_data_block as u64,
            blocks_per_group: blocks_per_group as u64,
            meta_bg,
            first_meta_bg: if meta_bg {
                u32le(&sb, 0x104) as u64
            } else {
                u64::MAX
            },
            sparse_super: ro_compat & RO_COMPAT_SPARSE != 0,
        };
        let raw = layout.read(ngroups, |b, buf| {
            dev.read_bytes(b * block_size, buf)?;
            Ok(())
        })?;
        let groups = parse_groups(&raw, ngroups, desc_size as usize, is64);
        let label: String = core::str::from_utf8(&sb[120..136])
            .unwrap_or("")
            .trim_end_matches('\0')
            .into();
        let kind = probe(&dev).map(|(k, _)| k).unwrap_or("ext2");
        let free_blocks = groups.iter().map(|g| g.free_blocks as u64).sum();
        let free_inodes = groups.iter().map(|g| g.free_inodes).sum();
        let csum_on = ro_compat & RO_COMPAT_METADATA_CSUM != 0;
        let uuid: [u8; 16] = sb[0x68..0x78].try_into().unwrap();
        let mut hash_seed = [0u32; 4];
        for (i, h) in hash_seed.iter_mut().enumerate() {
            *h = u32le(&sb, SB_HASH_SEED + i * 4);
        }
        let gdt_blocks = if meta_bg {
            layout.first_meta_bg
        } else {
            (ngroups as u64 * desc_size).div_ceil(block_size)
        } + u16le(&sb, 0xCE) as u64;
        let fs = Arc::new(Ext2Fs {
            dev: dev.clone(),
            block_size,
            blocks_count,
            inodes_count,
            blocks_per_group,
            cbits,
            clusters_per_group,
            inodes_per_group,
            inode_size,
            first_data_block,
            desc_size,
            first_ino,
            gdt_blocks,
            layout,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
            read_only: true,
            extents: incompat & INCOMPAT_EXTENTS != 0,
            csum: csum_on,
            gdt_csum: !csum_on && ro_compat & RO_COMPAT_GDT_CSUM != 0,
            sparse_super: ro_compat & RO_COMPAT_SPARSE != 0,
            is64,
            largedir: incompat & INCOMPAT_LARGEDIR != 0,
            dir_index: compat & COMPAT_DIR_INDEX != 0,
            def_hash_version: sb[0xFC],
            casefold: incompat & INCOMPAT_CASEFOLD != 0
                && u16le(&sb, 0x27C) == ext4_core::casefold::ENCODING_UTF8,
            seed: csum::fs_seed(&sb, incompat & INCOMPAT_CSUM_SEED != 0),
            uuid,
            hash_seed,
            hash_unsigned: u32le(&sb, SB_FLAGS) & 0x2 != 0,
            label: label.clone(),
            kind,
            id: 0,
            journal: spin::Once::new(),
            quota: spin::Once::new(),
            data_journal: core::sync::atomic::AtomicBool::new(false),
            counts_dirty: core::sync::atomic::AtomicBool::new(false),
            rmw: Mutex::new(()),
            orphans: Mutex::new(()),
            meta: Mutex::new(Meta {
                groups,
                free_blocks,
                free_inodes,
            }),
            inodes: crate::sync::Mutex::new(BTreeMap::new()),
            me: spin::Once::new(),
        });
        fs.me.call_once(|| Arc::downgrade(&fs));

        // The journal: replay it (or, read-only, overlay it) when dirty.
        let mut journal = None;
        let mut replayed = false;
        if has_journal && journal_inum != 0 {
            match journal::Journal::open(&fs, journal_inum) {
                Ok(j) => {
                    if j.dirty() {
                        if dev_writable {
                            j.replay(&fs)?;
                        } else {
                            j.overlay(&fs)?;
                        }
                        replayed = true;
                    }
                    journal = Some(j);
                }
                Err(e) => {
                    crate::println!("[ext4] cannot use the journal ({:?}): read-only", e);
                    read_only = true;
                    if incompat & INCOMPAT_RECOVER != 0 {
                        crate::println!("[ext4] warning: the journal needs recovery");
                    }
                }
            }
        }
        // Rebuild the filesystem object: the replay may have changed the
        // superblock and group descriptors, and the flags above are final.
        drop(fs);
        let mut sb2 = vec![0u8; 1024];
        drop(raw);
        let raw2 = match &journal {
            Some(j) if replayed && !dev_writable => {
                // Read through the overlay.
                let mut blk = vec![0u8; block_size as usize];
                let sbb = 1024 / block_size;
                if !j.cached(sbb, &mut blk) {
                    dev.read_bytes(sbb * block_size, &mut blk)?;
                }
                let o = (1024 % block_size) as usize;
                sb2.copy_from_slice(&blk[o..o + 1024]);
                layout.read(ngroups, |b, buf| {
                    if !j.cached(b, buf) {
                        dev.read_bytes(b * block_size, buf)?;
                    }
                    Ok(())
                })?
            }
            _ => {
                dev.read_bytes(1024, &mut sb2)?;
                layout.read(ngroups, |b, buf| {
                    dev.read_bytes(b * block_size, buf)?;
                    Ok(())
                })?
            }
        };
        let groups = parse_groups(&raw2, ngroups, desc_size as usize, is64);
        let free_blocks = groups.iter().map(|g| g.free_blocks as u64).sum();
        let free_inodes = groups.iter().map(|g| g.free_inodes).sum();
        let fs = Arc::new(Ext2Fs {
            dev,
            block_size,
            blocks_count,
            inodes_count,
            blocks_per_group,
            cbits,
            clusters_per_group,
            inodes_per_group,
            inode_size,
            first_data_block,
            desc_size,
            first_ino,
            gdt_blocks,
            layout,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
            read_only,
            extents: incompat & INCOMPAT_EXTENTS != 0,
            csum: csum_on,
            gdt_csum: !csum_on && ro_compat & RO_COMPAT_GDT_CSUM != 0,
            sparse_super: ro_compat & RO_COMPAT_SPARSE != 0,
            is64,
            largedir: incompat & INCOMPAT_LARGEDIR != 0,
            dir_index: compat & COMPAT_DIR_INDEX != 0,
            def_hash_version: sb[0xFC],
            casefold: incompat & INCOMPAT_CASEFOLD != 0
                && u16le(&sb, 0x27C) == ext4_core::casefold::ENCODING_UTF8,
            seed: csum::fs_seed(&sb2, incompat & INCOMPAT_CSUM_SEED != 0),
            uuid,
            hash_seed,
            hash_unsigned: u32le(&sb2, SB_FLAGS) & 0x2 != 0,
            label,
            kind,
            id: NEXT_ID.fetch_add(1, Ordering::SeqCst) as usize | (3 << 40),
            journal: spin::Once::new(),
            quota: spin::Once::new(),
            data_journal: core::sync::atomic::AtomicBool::new(false),
            counts_dirty: core::sync::atomic::AtomicBool::new(false),
            rmw: Mutex::new(()),
            orphans: Mutex::new(()),
            meta: Mutex::new(Meta {
                groups,
                free_blocks,
                free_inodes,
            }),
            inodes: crate::sync::Mutex::new(BTreeMap::new()),
            me: spin::Once::new(),
        });
        fs.me.call_once(|| Arc::downgrade(&fs));
        if let Some(j) = journal
            && (!read_only || j.has_overlay())
        {
            fs.journal.call_once(|| j);
        }
        if !read_only {
            // Mounted: not clean, and (with a journal) needing recovery
            // until it is cleanly unmounted.
            let journaled = fs.jnl().is_some();
            fs.sb_direct(|sb| {
                put16(sb, SB_STATE, 0);
                if journaled {
                    let f = u32le(sb, SB_INCOMPAT);
                    put32(sb, SB_INCOMPAT, f | INCOMPAT_RECOVER);
                }
                put32(sb, 44, now()); // s_mtime
                let mc = u16le(sb, 52);
                put16(sb, 52, mc.wrapping_add(1));
                put32(sb, 48, now()); // s_wtime
            })?;
            fs.dev.sync()?;
            if let Some(j) = fs.jnl() {
                let tags = j.take_fc_tags();
                if !tags.is_empty() {
                    fs.fc_replay(tags)?;
                    fs.commit()?;
                    j.checkpoint(&fs)?;
                }
            }
            fs.cleanup_orphans()?;
        }
        if ro_compat & RO_COMPAT_QUOTA != 0 {
            match fs.quota_load(&sb2, ro_compat & RO_COMPAT_PROJECT != 0) {
                Ok(q) => {
                    fs.quota.call_once(|| crate::sync::Mutex::new(q));
                }
                Err(e) => crate::println!("[ext4] cannot read quota files ({:?})", e),
            }
        }
        Ok(fs)
    }

    fn arc(&self) -> Arc<Ext2Fs> {
        self.me
            .get()
            .and_then(|w| w.upgrade())
            .expect("ext2 fs dropped")
    }

    fn jnl(&self) -> Option<&journal::Journal> {
        self.journal.get()
    }

    // ------------------------------------------------------------------
    // Metadata access
    // ------------------------------------------------------------------

    /// Read metadata block `b` (the journal's copy if it has one).
    fn mread(&self, b: u64, buf: &mut [u8]) -> KResult<()> {
        if let Some(j) = self.jnl()
            && j.cached(b, buf)
        {
            return Ok(());
        }
        self.dev.read_bytes(b * self.block_size, buf)?;
        Ok(())
    }

    /// Write metadata block `b` (into the running transaction if journaled).
    fn mwrite(&self, b: u64, buf: &[u8]) -> KResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        match self.jnl() {
            Some(j) => j.log(b, buf.to_vec()),
            None => {
                self.dev.write_bytes(b * self.block_size, buf)?;
            }
        }
        Ok(())
    }

    /// Read-modify-write of metadata block `b`.
    fn modify<R>(&self, b: u64, f: impl FnOnce(&mut [u8]) -> R) -> KResult<R> {
        let _g = self.rmw.lock();
        let mut buf = vec![0u8; self.block_size as usize];
        self.mread(b, &mut buf)?;
        let r = f(&mut buf);
        self.mwrite(b, &buf)?;
        Ok(r)
    }

    /// Read-modify-write of bytes `off..off+len` of metadata block `b`
    /// (without a journal only those bytes are read and written).
    fn modify_range<R>(
        &self,
        b: u64,
        off: usize,
        len: usize,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> KResult<R> {
        if self.jnl().is_some() {
            return self.modify(b, |blk| f(&mut blk[off..off + len]));
        }
        if self.read_only {
            return Err(EROFS);
        }
        let _g = self.rmw.lock();
        let mut buf = vec![0u8; len];
        let pos = b * self.block_size + off as u64;
        self.dev.read_bytes(pos, &mut buf)?;
        let r = f(&mut buf);
        self.dev.write_bytes(pos, &buf)?;
        Ok(r)
    }

    fn read_block(&self, b: u64, buf: &mut [u8]) -> KResult<()> {
        self.mread(b, buf)
    }

    /// (block, offset) of the primary superblock.
    fn sb_pos(&self) -> (u64, usize) {
        (1024 / self.block_size, (1024 % self.block_size) as usize)
    }

    fn sb_csum(&self, sb: &mut [u8]) {
        if self.csum {
            let c = csum::superblock(sb);
            put32(sb, csum::sb::CHECKSUM, c);
        }
    }

    /// Update the superblock (journaled).
    fn sb_update(&self, f: impl FnOnce(&mut [u8])) -> KResult<()> {
        let (b, o) = self.sb_pos();
        self.modify_range(b, o, 1024, |sb| {
            f(sb);
            self.sb_csum(sb);
        })
    }

    /// Write the free counts to the superblock if they changed (done
    /// lazily: before each journal commit, on sync and at unmount).
    fn flush_counts(&self) -> KResult<()> {
        if !self.counts_dirty.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let (fb, fi) = {
            let m = self.meta.lock();
            (m.free_blocks, m.free_inodes)
        };
        let is64 = self.is64;
        let fb = fb << self.cbits;
        self.sb_update(|sb| {
            put32(sb, SB_FREE_BLOCKS, fb as u32);
            if is64 {
                put32(sb, SB_FREE_BLOCKS_HI, (fb >> 32) as u32);
            }
            put32(sb, SB_FREE_INODES, fi);
        })
    }

    /// Update the superblock on the device directly (mount and unmount,
    /// when the journal is empty).
    fn sb_direct(&self, f: impl FnOnce(&mut [u8])) -> KResult<()> {
        let mut sb = [0u8; 1024];
        self.dev.read_bytes(1024, &mut sb)?;
        f(&mut sb);
        self.sb_csum(&mut sb);
        self.dev.write_bytes(1024, &sb)?;
        Ok(())
    }

    fn iseed(&self, ino: u32, generation: u32) -> u32 {
        csum::inode_seed(self.seed, ino, generation)
    }

    fn inode_loc(&self, ino: u32) -> KResult<(u64, usize)> {
        if ino == 0 || ino > self.inodes_count {
            return Err(EIO);
        }
        let g = ((ino - 1) / self.inodes_per_group) as usize;
        let idx = ((ino - 1) % self.inodes_per_group) as u64;
        let table = self.meta.lock().groups.get(g).ok_or(EIO)?.inode_table;
        let off = idx * self.inode_size;
        Ok((
            table + off / self.block_size,
            (off % self.block_size) as usize,
        ))
    }

    fn read_raw_inode(&self, ino: u32) -> KResult<RawInode> {
        let (b, o) = self.inode_loc(ino)?;
        let mut blk = vec![0u8; self.block_size as usize];
        self.mread(b, &mut blk)?;
        let n = self.inode_size.max(128) as usize;
        Ok(RawInode::parse(&blk[o..o + n]))
    }

    fn write_raw_inode(&self, ino: u32, r: &RawInode) -> KResult<()> {
        if self.read_only {
            return Err(EROFS);
        }
        let (b, o) = self.inode_loc(ino)?;
        let mut data = r.serialize();
        if self.csum {
            csum::set_inode(self.iseed(ino, r.generation()), &mut data);
        }
        self.modify_range(b, o, data.len(), |d| d.copy_from_slice(&data))
    }

    fn inode(&self, ino: u32) -> KResult<Arc<Ext2Inode>> {
        if let Some(i) = self.inodes.lock().get(&ino).and_then(|w| w.upgrade()) {
            return Ok(i);
        }
        let raw = self.read_raw_inode(ino)?;
        let folded = self.casefold && raw.flags & FL_CASEFOLD != 0;
        let i = Arc::new(Ext2Inode {
            fs: self.arc(),
            ino,
            st: Mutex::new(raw),
            alloc_hint: AtomicU64::new(0),
            folded,
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

    // ------------------------------------------------------------------
    // Groups and allocation
    // ------------------------------------------------------------------

    /// Write group `g`'s descriptor (with its checksum) and the free
    /// counts in the superblock.
    fn flush_group(&self, meta: &Meta, g: usize) -> KResult<()> {
        let (b, o) = self.layout.locate(g as u64);
        let ds = self.desc_size as usize;
        let gr = &meta.groups[g];
        let big = self.is64 && ds >= 64;
        self.modify_range(b, o, ds, |d| {
            put16(d, 0x0C, gr.free_blocks as u16);
            put16(d, 0x0E, gr.free_inodes as u16);
            put16(d, 0x10, gr.used_dirs as u16);
            put16(d, 0x12, gr.flags);
            put16(d, 0x18, gr.block_bitmap_csum as u16);
            put16(d, 0x1A, gr.inode_bitmap_csum as u16);
            put16(d, 0x1C, gr.itable_unused as u16);
            if big {
                put16(d, 0x2C, (gr.free_blocks >> 16) as u16);
                put16(d, 0x2E, (gr.free_inodes >> 16) as u16);
                put16(d, 0x30, (gr.used_dirs >> 16) as u16);
                put16(d, 0x32, (gr.itable_unused >> 16) as u16);
                put16(d, 0x38, (gr.block_bitmap_csum >> 16) as u16);
                put16(d, 0x3A, (gr.inode_bitmap_csum >> 16) as u16);
            }
            if self.csum {
                let c = csum::group_desc(self.seed, g as u32, d);
                put16(d, 0x1E, c);
            } else if self.gdt_csum {
                let c = csum::group_desc_crc16(&self.uuid, g as u32, d);
                put16(d, 0x1E, c);
            }
        })?;
        self.counts_dirty.store(true, Ordering::SeqCst);
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

    fn group_base(&self, g: usize) -> u64 {
        self.first_data_block as u64 + g as u64 * self.blocks_per_group as u64
    }

    /// Whether group `g` holds a superblock backup (and descriptor copy).
    fn group_has_super(&self, g: usize) -> bool {
        has_super(self.sparse_super, g as u64)
    }

    /// Blocks at the start of group `g` used by the superblock copy and
    /// group descriptors (as `ext4_num_base_meta_clusters`).
    fn base_meta_blocks(&self, g: usize) -> u64 {
        let l = &self.layout;
        let sb = self.group_has_super(g) as u64;
        if !l.meta_bg || (g as u64) < l.first_meta_bg * l.per_block() {
            if sb == 1 { 1 + self.gdt_blocks } else { 0 }
        } else {
            sb + l.meta_desc_blocks(g as u64)
        }
    }

    fn itable_blocks(&self) -> u64 {
        (self.inodes_per_group as u64 * self.inode_size).div_ceil(self.block_size)
    }

    /// Contents of a BLOCK_UNINIT group's bitmap: its superblock and
    /// descriptor copies, and any group's bitmaps and inode table stored in
    /// it (flex_bg places them together), are in use.
    fn init_block_bitmap(&self, meta: &Meta, g: usize, bm: &mut [u8]) {
        bm.fill(0);
        let base = self.group_base(g);
        let nb = self.group_blocks(g, meta.groups.len()) as u64;
        let cb = self.cbits;
        let nc = self.group_clusters(g, meta.groups.len()) as u64;
        let mut set_range = |start: u64, len: u64| {
            let s = start.max(base);
            let e = (start + len).min(base + nb);
            for b in s..e.max(s) {
                let i = ((b - base) >> cb) as usize;
                bm[i / 8] |= 1 << (i % 8);
            }
        };
        set_range(base, self.base_meta_blocks(g));
        let it = self.itable_blocks();
        for h in &meta.groups {
            set_range(h.block_bitmap, 1);
            set_range(h.inode_bitmap, 1);
            set_range(h.inode_table, it);
        }
        for i in nc as usize..bm.len() * 8 {
            bm[i / 8] |= 1 << (i % 8);
        }
    }

    /// Clusters (bitmap bits) in group `g`.
    fn group_clusters(&self, g: usize, ngroups: usize) -> u32 {
        (self.group_blocks(g, ngroups) as u64).div_ceil(1 << self.cbits) as u32
    }

    /// Find and set a clear bit in group `g`'s block (`block`) or inode
    /// bitmap, scanning from `start`; initialises uninitialised bitmaps.
    fn bitmap_alloc(
        &self,
        meta: &mut Meta,
        g: usize,
        block: bool,
        limit: u32,
        start: u32,
    ) -> KResult<Option<u32>> {
        let bs = self.block_size as usize;
        let gr = &meta.groups[g];
        let (bb, flag) = if block {
            (gr.block_bitmap, BG_BLOCK_UNINIT)
        } else {
            (gr.inode_bitmap, BG_INODE_UNINIT)
        };
        let uninit = gr.flags & flag != 0 && (self.csum || self.gdt_csum);
        let init = uninit.then(|| {
            let mut bm = vec![0u8; bs];
            if block {
                self.init_block_bitmap(meta, g, &mut bm);
            } else {
                for i in self.inodes_per_group as usize..bs * 8 {
                    bm[i / 8] |= 1 << (i % 8);
                }
            }
            bm
        });
        let base = self.group_base(g);
        let jnl = self.jnl();
        let csum_len = if block {
            self.clusters_per_group as usize / 8
        } else {
            self.inodes_per_group as usize / 8
        };
        let cb = self.cbits;
        let mut bm = match init {
            Some(bm) => bm,
            None => {
                let mut bm = vec![0u8; bs];
                self.mread(bb, &mut bm)?;
                bm
            }
        };
        let mut found = None;
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
                // Blocks freed in the running transaction stay reserved
                // until it commits.
                let held = block && jnl.is_some_and(|j| j.is_freed(base + ((i as u64) << cb)));
                if !held {
                    bm[byte] |= 1 << bit;
                    found = Some(i);
                    break;
                }
            }
            k += 1;
        }
        // The caller holds the meta lock: nobody else changes bitmaps.
        match found {
            Some(i) if !uninit && jnl.is_none() => {
                let byte = (i / 8) as usize;
                let v = bm[byte];
                self.modify_range(bb, byte, 1, |x| x[0] = v)?;
            }
            _ if uninit || found.is_some() => self.mwrite(bb, &bm)?,
            _ => {}
        }
        let crc = if self.csum {
            csum::bitmap(self.seed, &bm[..csum_len.min(bm.len())])
        } else {
            0
        };
        let gr = &mut meta.groups[g];
        if uninit {
            gr.flags &= !flag;
        }
        if self.csum {
            if block {
                gr.block_bitmap_csum = crc;
            } else {
                gr.inode_bitmap_csum = crc;
            }
        }
        Ok(found)
    }

    /// Clear `count` bits from `first` in a bitmap (updating its checksum).
    fn bitmap_clear(
        &self,
        meta: &mut Meta,
        g: usize,
        block: bool,
        first: u32,
        count: u32,
    ) -> KResult<()> {
        let gr = &meta.groups[g];
        let bb = if block {
            gr.block_bitmap
        } else {
            gr.inode_bitmap
        };
        let csum_len = if block {
            self.clusters_per_group as usize / 8
        } else {
            self.inodes_per_group as usize / 8
        };
        let mut bm = vec![0u8; self.block_size as usize];
        self.mread(bb, &mut bm)?;
        for i in first..first + count {
            bm[(i / 8) as usize] &= !(1 << (i % 8));
        }
        if self.jnl().is_none() {
            let (a, e) = ((first / 8) as usize, (first + count - 1) as usize / 8 + 1);
            let part = bm[a..e].to_vec();
            self.modify_range(bb, a, e - a, |x| x.copy_from_slice(&part))?;
        } else {
            self.mwrite(bb, &bm)?;
        }
        let crc = if self.csum {
            csum::bitmap(self.seed, &bm[..csum_len.min(bm.len())])
        } else {
            0
        };
        if self.csum {
            let gr = &mut meta.groups[g];
            if block {
                gr.block_bitmap_csum = crc;
            } else {
                gr.inode_bitmap_csum = crc;
            }
        }
        Ok(())
    }

    /// Allocate a zeroed block, preferring the group of `goal`.
    fn alloc_block(&self, goal: u64) -> KResult<u64> {
        if self.read_only {
            return Err(EROFS);
        }
        let mut meta = self.meta.lock();
        let n = meta.groups.len();
        let goal = if goal >= self.blocks_count { 0 } else { goal };
        let goal_g = (goal.saturating_sub(self.first_data_block as u64)
            / self.blocks_per_group as u64) as usize
            % n.max(1);
        for k in 0..n {
            let g = (goal_g + k) % n;
            if meta.groups[g].free_blocks == 0 {
                continue;
            }
            let limit = self.group_clusters(g, n);
            let start = if k == 0 {
                ((goal.saturating_sub(self.first_data_block as u64) % self.blocks_per_group as u64)
                    >> self.cbits) as u32
            } else {
                0
            };
            if let Some(i) = self.bitmap_alloc(
                &mut meta,
                g,
                true,
                limit,
                start.min(limit.saturating_sub(1)),
            )? {
                meta.groups[g].free_blocks -= 1;
                meta.free_blocks = meta.free_blocks.saturating_sub(1);
                self.flush_group(&meta, g)?;
                drop(meta);
                let b = self.group_base(g) + ((i as u64) << self.cbits);
                self.dev
                    .write_bytes(b * self.block_size, &vec![0u8; self.block_size as usize])?;
                return Ok(b);
            }
        }
        Err(ENOSPC)
    }

    /// Free `count` blocks starting at `first` (with bigalloc: the whole
    /// clusters they lie in; callers only free clusters nothing else uses).
    fn free_blocks(&self, first: u64, count: u64) -> KResult<()> {
        let cb = self.cbits;
        let mask = (1u64 << cb) - 1;
        let mut b = first & !mask;
        let end = (first + count + mask) & !mask;
        let mut meta = self.meta.lock();
        while b < end {
            if b < self.first_data_block as u64 || b >= self.blocks_count {
                b += 1 << cb;
                continue;
            }
            let rel = b - self.first_data_block as u64;
            let g = (rel / self.blocks_per_group as u64) as usize;
            let i = ((rel % self.blocks_per_group as u64) >> cb) as u32;
            let n = ((end - b) >> cb).min((self.clusters_per_group - i) as u64) as u32;
            self.bitmap_clear(&mut meta, g, true, i, n)?;
            meta.groups[g].free_blocks += n;
            meta.free_blocks += n as u64;
            self.flush_group(&meta, g)?;
            if let Some(j) = self.jnl() {
                for x in b..b + ((n as u64) << cb) {
                    j.freed(x);
                }
            }
            b += (n as u64) << cb;
        }
        Ok(())
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
            if let Some(i) = self.bitmap_alloc(&mut meta, g, false, limit, start)? {
                if g == 0 && i < self.first_ino - 1 {
                    // Wrapped into the reserved range; undo.
                    self.bitmap_clear(&mut meta, g, false, i, 1)?;
                    continue;
                }
                let gr = &mut meta.groups[g];
                gr.free_inodes -= 1;
                if is_dir {
                    gr.used_dirs += 1;
                }
                if (self.csum || self.gdt_csum) && i >= limit - gr.itable_unused.min(limit) {
                    gr.itable_unused = limit - i - 1;
                }
                meta.free_inodes = meta.free_inodes.saturating_sub(1);
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
        self.bitmap_clear(&mut meta, g, false, i, 1)?;
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
        let mut b = vec![0u8; self.block_size as usize];
        self.mread(block as u64, &mut b)?;
        Ok(u32le(&b, idx as usize * 4))
    }

    fn write_ptr(&self, block: u32, idx: u64, v: u32) -> KResult<()> {
        self.modify_range(block as u64, idx as usize * 4, 4, |b| put32(b, 0, v))
    }

    // ------------------------------------------------------------------
    // Journal and orphans
    // ------------------------------------------------------------------

    /// Commit the running journal transaction.
    fn commit(&self) -> KResult<()> {
        if self.read_only {
            return Ok(());
        }
        {
            let _h = self.begin();
            self.flush_counts()?;
        }
        match self.jnl() {
            Some(j) => j.commit(self),
            None => Ok(()),
        }
    }

    /// Put `ino` (whose link count dropped to zero) on the orphan list, so
    /// a crash before it is released does not leak it.
    fn orphan_add(&self, ino: &Ext2Inode, st: &mut RawInode) -> KResult<()> {
        let _o = self.orphans.lock();
        let (b, o) = self.sb_pos();
        let mut blk = vec![0u8; self.block_size as usize];
        self.mread(b, &mut blk)?;
        st.dtime = u32le(&blk[o..], SB_LAST_ORPHAN);
        ino.save(st)?;
        let n = ino.ino;
        self.sb_update(|sb| put32(sb, SB_LAST_ORPHAN, n))
    }

    /// Take `ino` off the orphan list (it has been released).
    fn orphan_del(&self, ino: u32, next: u32) -> KResult<()> {
        let _o = self.orphans.lock();
        let (b, o) = self.sb_pos();
        let mut blk = vec![0u8; self.block_size as usize];
        self.mread(b, &mut blk)?;
        let mut cur = u32le(&blk[o..], SB_LAST_ORPHAN);
        if cur == ino {
            return self.sb_update(|sb| put32(sb, SB_LAST_ORPHAN, next));
        }
        let mut steps = 0;
        while cur != 0 && steps < 1_000_000 {
            let i = self.inode(cur)?;
            let mut st = i.st.lock();
            if st.dtime == ino {
                st.dtime = next;
                return i.save(&st);
            }
            cur = st.dtime;
            steps += 1;
        }
        Ok(())
    }

    /// Release the inodes on the orphan list (left by a crash).
    fn cleanup_orphans(&self) -> KResult<()> {
        let (b, o) = self.sb_pos();
        let mut blk = vec![0u8; self.block_size as usize];
        self.mread(b, &mut blk)?;
        let mut cur = u32le(&blk[o..], SB_LAST_ORPHAN);
        if cur == 0 {
            return Ok(());
        }
        let _h = self.begin();
        let mut list = Vec::new();
        while cur != 0 && cur <= self.inodes_count && list.len() < 100_000 && !list.contains(&cur) {
            list.push(cur);
            cur = self.read_raw_inode(cur)?.dtime;
        }
        self.sb_update(|sb| put32(sb, SB_LAST_ORPHAN, 0))?;
        let mut released = 0;
        for ino in list {
            let i = self.inode(ino)?;
            let dead = {
                let mut st = i.st.lock();
                st.dtime = 0;
                i.save(&st)?;
                st.links == 0 && st.mode != 0
            };
            if dead {
                i.release()?;
                released += 1;
            }
        }
        crate::println!("[ext4] released {} orphaned inode(s)", released);
        Ok(())
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
        self.quota_flush()?;
        self.commit()?;
        self.dev.sync()
    }
    fn quota(
        &self,
        op: crate::vfs::QuotaOp,
        kind: u32,
        id: u32,
    ) -> KResult<Option<crate::vfs::DiskQuota>> {
        self.quota_op(op, kind, id)
    }
    fn statfs(&self) -> StatFs {
        let m = self.meta.lock();
        StatFs {
            fs_type: 0xEF53,
            block_size: self.block_size,
            blocks: self.blocks_count,
            blocks_free: m.free_blocks << self.cbits,
            files: self.inodes_count as u64,
            files_free: m.free_inodes as u64,
            name_max: 255,
        }
    }
    fn read_only(&self) -> bool {
        self.read_only
    }
    fn unmount(&self) -> KResult<()> {
        self.quota_flush()?;
        self.clean_shutdown()
    }
}

impl Ext2Fs {
    /// Mount options: data=ordered (default), data=writeback (file data
    /// is not flushed before a commit) or data=journal (file data is
    /// logged too). Without a journal they are ignored.
    pub fn set_options(&self, opts: &[&str]) -> KResult<()> {
        for o in opts {
            match *o {
                "data=journal" | "data=writeback" | "data=ordered" => {
                    let Some(j) = self.jnl() else { continue };
                    j.writeback.store(*o == "data=writeback", Ordering::SeqCst);
                    self.data_journal
                        .store(*o == "data=journal", Ordering::SeqCst);
                }
                "ro" | "rw" | "defaults" | "noatime" | "relatime" | "sync" | "async" => {}
                _ => crate::println!("[ext4] ignoring mount option {}", o),
            }
        }
        Ok(())
    }

    /// Whether writes to this file's data go through the journal
    /// (data=journal, or the file's journal-data flag).
    fn journals_data(&self, st: &RawInode) -> bool {
        const FL_JOURNAL_DATA: u32 = 0x4000;
        self.jnl().is_some()
            && (self.data_journal.load(Ordering::Relaxed) || st.flags & FL_JOURNAL_DATA != 0)
    }

    /// Commit, checkpoint everything and mark the filesystem clean.
    fn clean_shutdown(&self) -> KResult<()> {
        if self.read_only {
            return Ok(());
        }
        self.commit()?;
        if let Some(j) = self.jnl() {
            j.checkpoint(self)?;
        }
        self.dev.sync()?;
        // Clean: the journal is empty and everything is on disk.
        self.sb_direct(|sb| {
            put16(sb, SB_STATE, 1);
            let f = u32le(sb, SB_INCOMPAT);
            put32(sb, SB_INCOMPAT, f & !INCOMPAT_RECOVER);
            put32(sb, 48, now());
        })?;
        self.dev.sync()
    }
}

impl Drop for Ext2Fs {
    fn drop(&mut self) {
        let _ = self.clean_shutdown();
    }
}

// ---------------------------------------------------------------------------
// Inodes
// ---------------------------------------------------------------------------

/// Directory leaf helpers shared by linear and indexed directories. `end`
/// is where entries stop (before the checksum tail).
fn leaf_insert(buf: &mut [u8], end: usize, name: &str, ino: u32, tcode: u8) -> bool {
    leaf_insert_raw(buf, end, name.as_bytes(), ino, tcode)
}

fn leaf_insert_raw(buf: &mut [u8], end: usize, name: &[u8], ino: u32, tcode: u8) -> bool {
    let need = (8 + name.len()).next_multiple_of(4);
    let write_rec = |buf: &mut [u8], o: usize, rec: usize| {
        put32(buf, o, ino);
        put16(buf, o + 4, rec as u16);
        buf[o + 6] = name.len() as u8;
        buf[o + 7] = tcode;
        buf[o + 8..o + 8 + name.len()].copy_from_slice(name);
    };
    let mut o = 0;
    while o + 8 <= end {
        let eino = u32le(buf, o);
        let rec = u16le(buf, o + 4) as usize;
        if rec < 8 || o + rec > end {
            return false;
        }
        let used = if eino == 0 {
            0
        } else {
            (8 + buf[o + 6] as usize).next_multiple_of(4)
        };
        if rec >= used + need {
            if eino == 0 {
                write_rec(buf, o, rec);
            } else {
                put16(buf, o + 4, used as u16);
                write_rec(buf, o + used, rec - used);
            }
            return true;
        }
        o += rec;
    }
    false
}

/// A leaf block holding only an empty record spanning it.
fn leaf_empty(buf: &mut [u8], end: usize) {
    buf.fill(0);
    put16(buf, 4, end as u16);
}

impl Ext2Inode {
    fn kind(&self) -> FileType {
        self.st.lock().kind()
    }

    fn save(&self, st: &RawInode) -> KResult<()> {
        self.fs.write_raw_inode(self.ino, st)
    }

    fn iseed(&self, st: &RawInode) -> u32 {
        self.fs.iseed(self.ino, st.generation())
    }

    /// Adjust `i_blocks` by `n` filesystem blocks.
    fn add_blocks(&self, st: &mut RawInode, n: i64) {
        let per = if st.flags & FL_HUGE_FILE != 0 {
            1
        } else {
            (self.fs.block_size / 512) as i64
        };
        st.blocks512 = (st.blocks512 as i64 + n * per).max(0) as u64;
        self.fs
            .quota_charge(self.ino, st, n * self.fs.block_size as i64, 0);
    }

    /// Where the next allocation for this file should start.
    fn goal(&self) -> u64 {
        match self.alloc_hint.load(Ordering::Relaxed) {
            0 => {
                let g = (self.ino - 1) / self.fs.inodes_per_group;
                self.fs.group_base(g as usize)
            }
            h => h + 1,
        }
    }

    /// Allocate a block for this file (data or mapping metadata).
    fn alloc_for(&self, st: &mut RawInode, goal: u64) -> KResult<u64> {
        self.fs
            .quota_check(self.ino, st, self.fs.block_size << self.fs.cbits, 0)?;
        let b = self.fs.alloc_block(goal)?;
        self.alloc_hint.store(b, Ordering::Relaxed);
        self.add_blocks(st, 1 << self.fs.cbits);
        Ok(b)
    }

    /// Physical block for logical block `l`; allocates when `create`.
    fn bmap(&self, st: &mut RawInode, l: u64, create: bool) -> KResult<Option<u64>> {
        if st.flags & FL_EXTENTS != 0 {
            return self.ext_map(st, l, create);
        }
        if st.flags & FL_INLINE != 0 {
            if !create {
                return Ok(None);
            }
            self.uninline(st)?;
            return self.bmap(st, l, create);
        }
        let fs = &self.fs;
        let p = fs.ptrs_per_block();
        let alloc32 = |me: &Self, st: &mut RawInode| -> KResult<u32> {
            let goal = match me.alloc_hint.load(Ordering::Relaxed) {
                0 if st.block[0] != 0 => st.block[0] as u64,
                _ => me.goal(),
            };
            let b = me.alloc_for(st, goal)?;
            u32::try_from(b).map_err(|_| ENOSPC)
        };
        if l < 12 {
            if st.block[l as usize] == 0 {
                if !create {
                    return Ok(None);
                }
                st.block[l as usize] = alloc32(self, st)?;
            }
            return Ok(Some(st.block[l as usize] as u64));
        }
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
            st.block[slot] = alloc32(self, st)?;
        }
        let mut cur = st.block[slot];
        for &idx in &path {
            let mut next = fs.read_ptr(cur, idx)?;
            if next == 0 {
                if !create {
                    return Ok(None);
                }
                next = alloc32(self, st)?;
                fs.write_ptr(cur, idx, next)?;
            }
            cur = next;
        }
        Ok(Some(cur as u64))
    }

    /// Read file data. `direct` reads data blocks around the block cache
    /// (the page cache holds them instead).
    fn read_data(
        &self,
        st: &mut RawInode,
        off: u64,
        buf: &mut [u8],
        direct: bool,
    ) -> KResult<usize> {
        if off >= st.size {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(st.size - off) as usize;
        if st.flags & FL_INLINE != 0 {
            // Inline data: i_block, then the system.data attribute.
            let inline = inline::inline_bytes(st);
            let end = (off as usize + len).min(inline.len());
            let n = end.saturating_sub(off as usize);
            if n > 0 {
                buf[..n].copy_from_slice(&inline[off as usize..end]);
            }
            buf[n..len].fill(0);
            return Ok(len);
        }
        let bs = self.fs.block_size;
        let mut done = 0;
        while done < len {
            let pos = off + done as u64;
            let l = pos / bs;
            let within = pos % bs;
            let mut n = ((bs - within) as usize).min(len - done);
            match self.bmap(st, l, false)? {
                Some(pb) => {
                    // Extend over physically contiguous blocks.
                    let mut next = l + 1;
                    while done + n < len && self.bmap(st, next, false)? == Some(pb + (next - l)) {
                        n = (n + bs as usize).min(len - done);
                        next += 1;
                    }
                    let dst = &mut buf[done..done + n];
                    if self.fs.journals_data(st) {
                        // Logged data: the journal may hold newer images.
                        let mut blk = vec![0u8; bs as usize];
                        let mut o = 0;
                        while o < n {
                            let p = pos + o as u64;
                            let w = (p % bs) as usize;
                            let k = (bs as usize - w).min(n - o);
                            self.fs.mread(pb + (p / bs - l), &mut blk)?;
                            dst[o..o + k].copy_from_slice(&blk[w..w + k]);
                            o += k;
                        }
                    } else if direct {
                        self.fs.dev.read_bytes_nocache(pb * bs + within, dst)?;
                    } else {
                        self.fs.dev.read_bytes(pb * bs + within, dst)?;
                    }
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
        if st.flags & FL_INLINE != 0 {
            self.uninline(st)?;
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
                if self.fs.journals_data(st) {
                    let src = &buf[done..done + n];
                    let w = within as usize;
                    self.fs
                        .modify(pb, |b| b[w..w + src.len()].copy_from_slice(src))?;
                } else {
                    self.fs
                        .dev
                        .write_bytes(pb * bs + within, &buf[done..done + n])?;
                }
                done += n;
            }
            Ok(())
        })();
        if done > 0 {
            st.size = st.size.max(off + done as u64);
            let t = now();
            st.mtime = t;
            st.ctime = t;
        }
        // Blocks may have been allocated even if nothing was written.
        self.save(st)?;
        res.map(|_| done)
    }

    /// Free blocks past `keep` logical blocks.
    fn free_from(&self, st: &mut RawInode, keep: u64) -> KResult<()> {
        if st.flags & FL_EXTENTS != 0 {
            return self.ext_truncate(st, keep);
        }
        if st.flags & FL_INLINE != 0 {
            // The inline area is simply cut off by the new size.
            return Ok(());
        }
        let fs = self.fs.clone();
        let p = fs.ptrs_per_block();
        let mut freed = 0u64;
        for i in (keep as usize).min(12)..12 {
            if st.block[i] != 0 {
                fs.free_blocks(st.block[i] as u64, 1)?;
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
            let mut ptrs = vec![0u8; fs.block_size as usize];
            fs.mread(blk as u64, &mut ptrs)?;
            for idx in 0..p {
                let first = base + idx * span;
                let child = u32le(&ptrs, idx as usize * 4);
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
                    fs.free_blocks(child as u64, 1)?;
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
                fs.free_blocks(st.block[slot] as u64, 1)?;
                st.block[slot] = 0;
                freed += 1;
            }
        }
        self.add_blocks(st, -(freed as i64));
        Ok(())
    }

    // Directory helpers ------------------------------------------------

    /// End of the entry area in a directory leaf block.
    fn dir_end(&self) -> usize {
        self.fs.block_size as usize - if self.fs.csum { csum::DIRENT_TAIL } else { 0 }
    }

    /// Write directory block `l` (physical `pb`) with its checksum.
    fn write_dir_block(&self, st: &RawInode, l: u64, pb: u64, buf: &mut [u8]) -> KResult<()> {
        if self.fs.csum {
            let seed = self.iseed(st);
            let bs = buf.len();
            if st.flags & FL_INDEX != 0 && l == 0 {
                csum::set_dx_node(seed, buf, 0x20);
            } else if st.flags & FL_INDEX != 0 && u32le(buf, 0) == 0 && u16le(buf, 4) as usize == bs
            {
                csum::set_dx_node(seed, buf, 8);
            } else {
                csum::set_dirent_tail(seed, buf);
            }
        }
        self.fs.mwrite(pb, buf)
    }

    /// All entries: (name, inode, type code, logical block, offset in block).
    fn dir_entries(&self) -> KResult<Vec<RawDirEntry>> {
        let mut st = self.st.lock();
        if st.kind() != FileType::Directory {
            return Err(ENOTDIR);
        }
        if st.flags & FL_INLINE != 0 {
            return Ok(inline::inline_dir_entries(self.ino, &st));
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
        {
            let mut st = self.st.lock();
            if st.flags & FL_INDEX != 0 {
                match self.dx_find(&mut st, name) {
                    Ok(Some(e)) => return Ok(e),
                    Ok(None) => return Err(ENOENT),
                    Err(_) => {} // damaged index: scan
                }
            }
        }
        let folded = self.folded;
        self.dir_entries()?
            .into_iter()
            .find(|(n, ..)| {
                n == name
                    || folded && ext4_core::casefold::names_equal(n.as_bytes(), name.as_bytes())
            })
            .map(|(_, i, t, l, o)| (i, t, l, o))
            .ok_or(ENOENT)
    }

    fn add_entry(&self, name: &str, ino: u32, kind: FileType) -> KResult<()> {
        if name.len() > 255 || name.is_empty() {
            return Err(ENAMETOOLONG);
        }
        let fs = self.fs.clone();
        let bs = fs.block_size as usize;
        let end = self.dir_end();
        let mut st = self.st.lock();
        self.uninline(&mut st)?;
        let tcode = if fs.filetype { ft_code(kind) } else { 0 };
        let t = now();
        st.mtime = t;
        st.ctime = t;
        if st.flags & FL_INDEX != 0 {
            self.dx_add(&mut st, name, ino, tcode)?;
            return self.save(&st);
        }
        let nblocks = st.size.div_ceil(bs as u64);
        let mut buf = vec![0u8; bs];
        for l in 0..nblocks {
            let Some(pb) = self.bmap(&mut st, l, false)? else {
                continue;
            };
            fs.read_block(pb, &mut buf)?;
            if leaf_insert(&mut buf, end, name, ino, tcode) {
                self.write_dir_block(&st, l, pb, &mut buf)?;
                return self.save(&st);
            }
        }
        // A full one-block directory becomes indexed (as Linux does).
        if nblocks == 1 && fs.dir_index && fs.def_hash_version <= 2 {
            self.dx_make_indexed(&mut st)?;
            self.dx_add(&mut st, name, ino, tcode)?;
            return self.save(&st);
        }
        // Append a new block.
        let pb = self.bmap(&mut st, nblocks, true)?.ok_or(EIO)?;
        leaf_empty(&mut buf, end);
        leaf_insert(&mut buf, end, name, ino, tcode);
        self.write_dir_block(&st, nblocks, pb, &mut buf)?;
        st.size = (nblocks + 1) * bs as u64;
        self.save(&st)
    }

    fn remove_entry(&self, l: u64, off: usize) -> KResult<()> {
        let fs = self.fs.clone();
        let bs = fs.block_size as usize;
        let mut st = self.st.lock();
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
        self.write_dir_block(&st, l, pb, &mut buf)?;
        let t = now();
        st.mtime = t;
        st.ctime = t;
        self.save(&st)
    }

    fn set_entry_ino(&self, name: &str, ino: u32) -> KResult<()> {
        {
            let mut st = self.st.lock();
            self.uninline(&mut st)?;
        }
        let (_, _, l, off) = self.find_entry(name)?;
        let mut st = self.st.lock();
        let pb = self.bmap(&mut st, l, false)?.ok_or(EIO)?;
        let mut buf = vec![0u8; self.fs.block_size as usize];
        self.fs.read_block(pb, &mut buf)?;
        put32(&mut buf, off, ino);
        self.write_dir_block(&st, l, pb, &mut buf)
    }

    fn is_empty_dir(&self) -> KResult<bool> {
        Ok(self
            .dir_entries()?
            .iter()
            .all(|(n, ..)| n == "." || n == ".."))
    }

    fn change_links(&self, delta: i32) -> KResult<()> {
        let mut st = self.st.lock();
        let old = st.links;
        st.links = (st.links as i32 + delta).clamp(0, 65000) as u16;
        st.ctime = now();
        if st.links == 0 && old != 0 {
            self.fs.orphan_add(self, &mut st)
        } else {
            self.save(&st)
        }
    }

    /// Drop an extended-attribute block reference.
    fn release_xattr(&self, st: &mut RawInode) -> KResult<()> {
        let xb = st.xattr_block();
        if xb == 0 || xb >= self.fs.blocks_count {
            return Ok(());
        }
        let (seed, use_csum) = (self.fs.seed, self.fs.csum);
        let mut refs = Vec::new();
        let free = self.fs.modify(xb, |blk| {
            let rc = u32le(blk, 4);
            if rc <= 1 {
                refs = Self::block_ea_refs(blk);
                return true;
            }
            put32(blk, 4, rc - 1);
            if use_csum {
                put32(blk, 0x10, 0);
                let c = csum::crc32c(csum::crc32c(seed, &xb.to_le_bytes()), blk);
                put32(blk, 0x10, c);
            }
            false
        })?;
        if free {
            self.fs.free_blocks(xb, 1)?;
            self.add_blocks(st, -(1 << self.fs.cbits));
        }
        st.set_xattr_block(0);
        self.put_ea_inodes(&refs)
    }

    /// Release the inode's storage once it has no links and no users.
    fn release(&self) -> KResult<()> {
        let _h = self.fs.begin();
        let mut st = self.st.lock();
        let was_dir = st.kind() == FileType::Directory;
        if !self.is_fast_symlink(&st) && st.flags & FL_INLINE == 0 {
            self.free_from(&mut st, 0)?;
        }
        self.release_xattr(&mut st)?;
        let ibody_refs = Self::ibody_ea_refs(&st);
        self.put_ea_inodes(&ibody_refs)?;
        self.fs.quota_charge(self.ino, &st, 0, -1);
        let next_orphan = st.dtime;
        st.size = 0;
        st.dtime = now();
        st.mode = 0;
        st.links = 0;
        st.block = [0; 15];
        st.flags &= !(FL_EXTENTS | FL_INDEX | FL_INLINE);
        self.save(&st)?;
        drop(st);
        self.fs.orphan_del(self.ino, next_orphan)?;
        self.fs.free_inode(self.ino, was_dir)
    }

    fn is_fast_symlink(&self, st: &RawInode) -> bool {
        let per = if st.flags & FL_HUGE_FILE != 0 {
            1
        } else {
            self.fs.block_size / 512
        };
        let ea = if st.xattr_block() != 0 { per } else { 0 };
        st.kind() == FileType::Symlink
            && st.flags & (FL_EXTENTS | FL_INLINE) == 0
            && st.size < 60
            && st.blocks512 <= ea
    }

    fn new_inode(&self, kind: FileType, mode: u32) -> KResult<Arc<Ext2Inode>> {
        let fs = &self.fs;
        let ino = fs.alloc_inode(self.ino, kind == FileType::Directory)?;
        let t = now();
        let (uid, gid) = crate::process::current()
            .map(|p| (p.uid.load(Ordering::Relaxed), p.gid.load(Ordering::Relaxed)))
            .unwrap_or((0, 0));
        let mut raw = vec![0u8; fs.inode_size.max(128) as usize];
        if fs.inode_size >= 160 {
            put16(&mut raw, csum::INODE_EXTRA_ISIZE, 32);
            put32(&mut raw, 0x90, t); // i_crtime
        }
        let mut g = [0u8; 4];
        crate::drivers::random::fill(&mut g);
        put32(&mut raw, csum::INODE_GENERATION, u32::from_le_bytes(g));
        let mut r = RawInode::parse(&raw);
        r.mode = (kind.mode_bits() as u16) | (mode & 0o7777) as u16;
        r.uid = uid;
        r.gid = gid;
        r.atime = t;
        r.ctime = t;
        r.mtime = t;
        r.links = 1;
        if fs.extents
            && matches!(
                kind,
                FileType::Regular | FileType::Directory | FileType::Symlink
            )
        {
            r.flags |= FL_EXTENTS;
            r.set_block_bytes(&extent::empty_root());
        }
        if self.folded && kind == FileType::Directory {
            r.flags |= FL_CASEFOLD;
        }
        // New files inherit the directory's project (project quotas).
        if r.raw.len() >= 0xA0 && self.st.lock().raw.len() >= 0xA0 {
            let prj = u32le(&self.st.lock().raw, 0x9C);
            put32(&mut r.raw, 0x9C, prj);
        }
        if let Err(e) = fs.quota_check(ino, &r, 0, 1) {
            fs.free_inode(ino, kind == FileType::Directory)?;
            return Err(e);
        }
        fs.write_raw_inode(ino, &r)?;
        fs.quota_charge(ino, &r, 0, 1);
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
        let _h = self.fs.begin();
        let mut st = self.st.lock();
        self.uninline(&mut st)
    }

    fn fallocate_inner(&self, mode: u32, off: u64, len: u64) -> KResult<()> {
        const KEEP_SIZE: u32 = 1;
        if mode & !KEEP_SIZE != 0 {
            return Err(EOPNOTSUPP);
        }
        if self.fs.read_only {
            return Err(EROFS);
        }
        let _h = self.fs.begin();
        let mut st = self.st.lock();
        match st.kind() {
            FileType::Regular => {}
            FileType::Directory => return Err(EISDIR),
            _ => return Err(ENODEV),
        }
        if st.flags & FL_INLINE != 0 {
            self.uninline(&mut st)?;
        }
        let bs = self.fs.block_size;
        let end = off.checked_add(len).ok_or(EFBIG)?;
        let res = (|| {
            for l in off / bs..end.div_ceil(bs) {
                if st.flags & FL_EXTENTS != 0 {
                    self.ext_prealloc(&mut st, l)?;
                } else {
                    self.bmap(&mut st, l, true)?;
                }
            }
            Ok(())
        })();
        if res.is_ok() && mode & KEEP_SIZE == 0 && end > st.size {
            st.size = end;
        }
        st.ctime = now();
        self.save(&st)?;
        res
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
    fn getxattr(&self, name: &str) -> KResult<Vec<u8>> {
        self.xattr_get(name)
    }

    fn listxattr(&self) -> KResult<Vec<String>> {
        self.xattr_list()
    }

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
        m.blocks = if st.flags & FL_HUGE_FILE != 0 {
            st.blocks512 * (self.fs.block_size / 512)
        } else {
            st.blocks512
        };
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
        let _h = self.fs.begin();
        if self.find_entry(name).is_ok() {
            return Err(EEXIST);
        }
        let child = self.new_inode(kind, mode)?;
        if kind == FileType::Directory {
            let bs = self.fs.block_size as usize;
            let end = child.dir_end();
            let mut buf = vec![0u8; bs];
            let tdir = if self.fs.filetype { 2 } else { 0 };
            put32(&mut buf, 0, child.ino);
            put16(&mut buf, 4, 12);
            buf[6] = 1;
            buf[7] = tdir;
            buf[8] = b'.';
            put32(&mut buf, 12, self.ino);
            put16(&mut buf, 16, (end - 12) as u16);
            buf[18] = 2;
            buf[19] = tdir;
            buf[20] = b'.';
            buf[21] = b'.';
            let mut st = child.st.lock();
            let pb = child.bmap(&mut st, 0, true)?.ok_or(EIO)?;
            child.write_dir_block(&st, 0, pb, &mut buf)?;
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
        let _h = self.fs.begin();
        if self.find_entry(name).is_ok() {
            return Err(EEXIST);
        }
        self.add_entry(name, t.ino, t.kind())?;
        t.change_links(1)
    }

    fn unlink(&self, name: &str) -> KResult<()> {
        self.dir_guard()?;
        let _h = self.fs.begin();
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
        let _h = self.fs.begin();
        let (ino, _, l, off) = self.find_entry(name)?;
        let child = self.fs.inode(ino)?;
        if child.kind() != FileType::Directory {
            return Err(ENOTDIR);
        }
        if !child.is_empty_dir()? {
            return Err(ENOTEMPTY);
        }
        self.remove_entry(l, off)?;
        {
            let mut st = child.st.lock();
            st.links = 0;
            st.ctime = now();
            self.fs.orphan_add(&child, &mut st)?;
        }
        self.change_links(-1)
    }

    fn rename(&self, old: &str, new_dir: &Arc<dyn Inode>, new: &str) -> KResult<()> {
        self.dir_guard()?;
        let nd = new_dir.as_any().downcast_ref::<Ext2Inode>().ok_or(EXDEV)?;
        if nd.fs.id != self.fs.id {
            return Err(EXDEV);
        }
        let _h = self.fs.begin();
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
        // Re-find: adding may have split the record (or, in an indexed
        // directory, moved it to another block) when both names live in
        // the same directory.
        let (_, _, l2, off2) = if nd.ino == self.ino {
            self.dir_entries()?
                .into_iter()
                .find(|(n, i, ..)| {
                    *i == ino
                        && (n == old
                            || self.folded
                                && ext4_core::casefold::names_equal(n.as_bytes(), old.as_bytes()))
                })
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
        self.read_data(&mut st, off, buf, false)
    }

    fn cacheable(&self) -> bool {
        self.st.lock().kind() == FileType::Regular
    }

    fn read_direct(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        let mut st = self.st.lock();
        if st.kind() == FileType::Directory {
            return Err(EISDIR);
        }
        self.read_data(&mut st, off, buf, true)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        if self.fs.read_only {
            return Err(EROFS);
        }
        let _h = self.fs.begin();
        let mut st = self.st.lock();
        if st.kind() == FileType::Directory {
            return Err(EISDIR);
        }
        if st.flags & FL_INLINE != 0 {
            self.uninline(&mut st)?;
        }
        self.write_data(&mut st, off, buf)
    }

    fn truncate(&self, size: u64) -> KResult<()> {
        if self.fs.read_only {
            return Err(EROFS);
        }
        let _h = self.fs.begin();
        let mut st = self.st.lock();
        if st.kind() == FileType::Directory {
            return Err(EISDIR);
        }
        if st.flags & FL_INLINE != 0 {
            self.uninline(&mut st)?;
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

    fn fallocate(&self, mode: u32, off: u64, len: u64) -> KResult<()> {
        self.fallocate_inner(mode, off, len)
    }

    fn symlink(&self, name: &str, target: &str) -> KResult<()> {
        self.dir_guard()?;
        let _h = self.fs.begin();
        if self.find_entry(name).is_ok() {
            return Err(EEXIST);
        }
        let child = self.new_inode(FileType::Symlink, 0o777)?;
        {
            let mut st = child.st.lock();
            if target.len() < 60 {
                let mut b = [0u8; 60];
                b[..target.len()].copy_from_slice(target.as_bytes());
                st.flags &= !FL_EXTENTS;
                st.set_block_bytes(&b);
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
        if self.is_fast_symlink(&st) {
            let b = st.block_bytes();
            return Ok(String::from_utf8_lossy(&b[..size]).into_owned());
        }
        let mut b = vec![0u8; size];
        self.read_data(&mut st, 0, &mut b, false)?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    fn chmod(&self, mode: u32) -> KResult<()> {
        let _h = self.fs.begin();
        let mut st = self.st.lock();
        st.mode = (st.mode & S_IFMT) | (mode & 0o7777) as u16;
        st.ctime = now();
        self.save(&st)
    }

    fn chown(&self, uid: u32, gid: u32) -> KResult<()> {
        let _h = self.fs.begin();
        let mut st = self.st.lock();
        // Move the file's usage to its new owners.
        let bytes = (st.blocks512 * 512) as i64;
        self.fs.quota_charge(self.ino, &st, -bytes, -1);
        if uid != u32::MAX {
            st.uid = uid;
        }
        if gid != u32::MAX {
            st.gid = gid;
        }
        self.fs.quota_charge(self.ino, &st, bytes, 1);
        st.ctime = now();
        self.save(&st)
    }

    fn set_times(&self, atime: Option<u64>, mtime: Option<u64>) -> KResult<()> {
        let _h = self.fs.begin();
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
        FileSystem::sync(&*self.fs)
    }

    fn fs_id(&self) -> usize {
        self.fs.id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
