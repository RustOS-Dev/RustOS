//! Disk quotas (the `quota` feature): per user, group and project usage
//! and limits, kept in hidden quota inodes in the "vfsv1" tree format.
//!
//! The files are read into memory at mount. Allocations, frees, inode
//! creation and deletion, and chown update the usage there; a block or
//! inode allocation that would pass a hard limit fails with EDQUOT (files
//! owned by root are never limited). `sync` (and unmount) writes the
//! files back, rebuilt from scratch.

use super::*;
use crate::vfs::{DiskQuota, QIF_BLIMITS, QIF_ILIMITS, QuotaOp};
use alloc::collections::BTreeSet;

/// Quota block size and tree layout (linux/fs/quota/quota_tree.h).
const QBLK: usize = 1024;
const TREE_DEPTH: usize = 4;
const REFS: usize = QBLK / 4;
const ENTRY: usize = 72; // struct v2r1_disk_dqblk
const DATA_HDR: usize = 16; // struct qt_disk_dqdbheader
const PER_DATA: usize = (QBLK - DATA_HDR) / ENTRY;
const MAGIC: [u32; 3] = [0xD9C0_1F11, 0xD9C0_1927, 0xD9C0_3F14];
const GRACE: u32 = 7 * 24 * 3600;

pub const USRQUOTA: usize = 0;
pub const GRPQUOTA: usize = 1;
pub const PRJQUOTA: usize = 2;

pub(super) struct Quotas {
    /// Quota inode per type (0 = not enabled).
    pub inum: [u32; 3],
    /// (type, id) -> usage and limits.
    pub map: BTreeMap<(usize, u32), DiskQuota>,
    pub grace: [(u32, u32); 3],
    pub dirty: bool,
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32le(b, o)
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Parse a quota file: its grace times and entries.
fn parse(kind: usize, f: &[u8]) -> Option<((u32, u32), Vec<(u32, DiskQuota)>)> {
    if f.len() < 2 * QBLK || le32(f, 0) != MAGIC[kind] || le32(f, 4) != 1 {
        return None;
    }
    let grace = (le32(f, 8), le32(f, 12));
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    fn walk(
        f: &[u8],
        blk: usize,
        depth: usize,
        out: &mut Vec<(u32, DiskQuota)>,
        seen: &mut BTreeSet<usize>,
    ) {
        let Some(b) = f.get(blk * QBLK..(blk + 1) * QBLK) else {
            return;
        };
        if depth == TREE_DEPTH {
            if !seen.insert(blk) {
                return; // data blocks are shared between ids
            }
            for i in 0..PER_DATA {
                let e = &b[DATA_HDR + i * ENTRY..DATA_HDR + (i + 1) * ENTRY];
                if e.iter().all(|&x| x == 0) {
                    continue;
                }
                out.push((
                    le32(e, 0),
                    DiskQuota {
                        bhard: le64(e, 8 + 24),
                        bsoft: le64(e, 8 + 32),
                        space: le64(e, 8 + 40),
                        ihard: le64(e, 8),
                        isoft: le64(e, 8 + 8),
                        inodes: le64(e, 8 + 16),
                        btime: le64(e, 8 + 48),
                        itime: le64(e, 8 + 56),
                    },
                ));
            }
            return;
        }
        for i in 0..REFS {
            let r = le32(b, i * 4) as usize;
            if r != 0 {
                walk(f, r, depth + 1, out, seen);
            }
        }
    }
    walk(f, 1, 0, &mut out, &mut seen);
    Some((grace, out))
}

/// Build a quota file holding `entries` (sorted by id).
fn build(kind: usize, grace: (u32, u32), entries: &[(u32, DiskQuota)]) -> Vec<u8> {
    let mut blocks: Vec<[u8; QBLK]> = alloc::vec![[0u8; QBLK]; 2];
    // Data blocks first: PER_DATA entries each.
    let mut where_: Vec<(u32, usize)> = Vec::new();
    let mut free_entry = 0u32;
    for chunk in entries.chunks(PER_DATA) {
        let n = blocks.len();
        let mut b = [0u8; QBLK];
        put16(&mut b, 8, chunk.len() as u16);
        for (i, (id, q)) in chunk.iter().enumerate() {
            let e = &mut b[DATA_HDR + i * ENTRY..DATA_HDR + (i + 1) * ENTRY];
            put32(e, 0, *id);
            let vals = [
                q.ihard, q.isoft, q.inodes, q.bhard, q.bsoft, q.space, q.btime, q.itime,
            ];
            for (k, v) in vals.iter().enumerate() {
                e[8 + 8 * k..16 + 8 * k].copy_from_slice(&v.to_le_bytes());
            }
            if e.iter().all(|&x| x == 0) {
                // An all-zero entry reads as free: mark id 0 as used.
                e[64..72].copy_from_slice(&1u64.to_le_bytes());
            }
            where_.push((*id, n));
        }
        if chunk.len() < PER_DATA {
            free_entry = n as u32;
        }
        blocks.push(b);
    }
    // The tree: block 1 is the root; one path per id.
    for (id, data) in where_ {
        let mut blk = 1usize;
        for depth in 0..TREE_DEPTH {
            let idx = ((id >> ((TREE_DEPTH - depth - 1) * 8)) & 0xFF) as usize;
            if depth == TREE_DEPTH - 1 {
                put32(&mut blocks[blk], idx * 4, data as u32);
                break;
            }
            let next = u32le(&blocks[blk], idx * 4) as usize;
            blk = if next == 0 {
                let n = blocks.len();
                blocks.push([0u8; QBLK]);
                put32(&mut blocks[blk], idx * 4, n as u32);
                n
            } else {
                next
            };
        }
    }
    let nblocks = blocks.len() as u32;
    let h = &mut blocks[0];
    put32(h, 0, MAGIC[kind]);
    put32(h, 4, 1);
    put32(h, 8, grace.0);
    put32(h, 12, grace.1);
    put32(h, 16, 0); // flags
    put32(h, 20, nblocks);
    put32(h, 24, 0); // free block list
    put32(h, 28, free_entry);
    blocks.concat()
}

impl Ext2Fs {
    /// Load the quota files named in the superblock.
    pub(super) fn quota_load(&self, sb: &[u8], project: bool) -> KResult<Quotas> {
        let mut q = Quotas {
            inum: [
                u32le(sb, 0x240),
                u32le(sb, 0x244),
                if project { u32le(sb, 0x26C) } else { 0 },
            ],
            map: BTreeMap::new(),
            grace: [(GRACE, GRACE); 3],
            dirty: false,
        };
        for kind in 0..3 {
            let ino = q.inum[kind];
            if ino == 0 {
                continue;
            }
            let i = self.inode(ino)?;
            let data = {
                let mut st = i.st.lock();
                let mut v = vec![0u8; st.size.min(64 << 20) as usize];
                let n = i.read_data(&mut st, 0, &mut v, false)?;
                v.truncate(n);
                v
            };
            match parse(kind, &data) {
                Some((grace, ents)) => {
                    q.grace[kind] = grace;
                    for (id, d) in ents {
                        q.map.insert((kind, id), d);
                    }
                }
                None => {
                    crate::println!("[ext4] quota file {} is damaged: quotas off", ino);
                    q.inum[kind] = 0;
                }
            }
        }
        Ok(q)
    }

    fn is_quota_inode(&self, ino: u32) -> bool {
        self.quota
            .get()
            .is_some_and(|q| q.lock().inum.contains(&ino))
    }

    /// Whether inode `ino` is charged to quotas (as e2fsck counts: the
    /// root directory and ordinary inodes, not the quota files).
    fn quota_counted(&self, ino: u32) -> bool {
        (ino == ROOT_INO || ino >= self.first_ino) && !self.is_quota_inode(ino)
    }

    fn quota_ids(st: &RawInode, project: bool) -> [(usize, u32); 3] {
        let prj = if project && st.raw.len() >= 0xA0 {
            u32le(&st.raw, 0x9C)
        } else {
            u32::MAX
        };
        [(USRQUOTA, st.uid), (GRPQUOTA, st.gid), (PRJQUOTA, prj)]
    }

    /// Charge `bytes` and `inodes` (may be negative) to the owners of `st`.
    pub(super) fn quota_charge(&self, ino: u32, st: &RawInode, bytes: i64, inodes: i64) {
        let Some(q) = self.quota.get() else { return };
        if !self.quota_counted(ino) {
            return;
        }
        let mut q = q.lock();
        for (kind, id) in Self::quota_ids(st, q.inum[PRJQUOTA] != 0) {
            if q.inum[kind] == 0 || id == u32::MAX {
                continue;
            }
            let d = q.map.entry((kind, id)).or_default();
            d.space = (d.space as i64 + bytes).max(0) as u64;
            d.inodes = (d.inodes as i64 + inodes).max(0) as u64;
            q.dirty = true;
        }
    }

    /// EDQUOT if adding `bytes`/`inodes` to the owners of `st` would pass
    /// a hard limit (root-owned files are never limited).
    pub(super) fn quota_check(
        &self,
        ino: u32,
        st: &RawInode,
        bytes: u64,
        inodes: u64,
    ) -> KResult<()> {
        let Some(q) = self.quota.get() else {
            return Ok(());
        };
        if st.uid == 0 || !self.quota_counted(ino) {
            return Ok(());
        }
        let q = q.lock();
        for (kind, id) in Self::quota_ids(st, q.inum[PRJQUOTA] != 0) {
            if q.inum[kind] == 0 || id == u32::MAX {
                continue;
            }
            if let Some(d) = q.map.get(&(kind, id))
                && ((d.bhard != 0 && d.space + bytes > d.bhard * 1024)
                    || (d.ihard != 0 && d.inodes + inodes > d.ihard))
            {
                return Err(EDQUOT);
            }
        }
        Ok(())
    }

    /// Write changed quota files back.
    pub(super) fn quota_flush(&self) -> KResult<()> {
        let Some(q) = self.quota.get() else {
            return Ok(());
        };
        let files = {
            let mut q = q.lock();
            if !q.dirty || self.read_only {
                return Ok(());
            }
            q.dirty = false;
            let mut files = Vec::new();
            for kind in 0..3 {
                if q.inum[kind] == 0 {
                    continue;
                }
                let ents: Vec<(u32, DiskQuota)> = q
                    .map
                    .iter()
                    .filter(|((k, _), d)| {
                        *k == kind
                            && (d.space | d.inodes | d.bhard | d.bsoft | d.ihard | d.isoft) != 0
                    })
                    .map(|((_, id), d)| (*id, *d))
                    .collect();
                files.push((q.inum[kind], build(kind, q.grace[kind], &ents)));
            }
            files
        };
        let _h = self.begin();
        for (ino, data) in files {
            let i = self.inode(ino)?;
            let mut st = i.st.lock();
            i.write_data(&mut st, 0, &data)?;
            if st.size > data.len() as u64 {
                let keep = (data.len() as u64).div_ceil(self.block_size);
                i.free_from(&mut st, keep)?;
            }
            st.size = data.len() as u64;
            i.save(&st)?;
        }
        Ok(())
    }

    /// quotactl on this filesystem.
    pub(super) fn quota_op(&self, op: QuotaOp, kind: u32, id: u32) -> KResult<Option<DiskQuota>> {
        let kind = kind as usize;
        let q = self.quota.get().ok_or(ESRCH)?;
        if kind > 2 || q.lock().inum[kind] == 0 {
            return Err(ESRCH);
        }
        match op {
            QuotaOp::Get => Ok(Some(
                q.lock().map.get(&(kind, id)).copied().unwrap_or_default(),
            )),
            QuotaOp::Set(new, valid) => {
                if self.read_only {
                    return Err(EROFS);
                }
                let mut q = q.lock();
                let d = q.map.entry((kind, id)).or_default();
                if valid & QIF_BLIMITS != 0 {
                    d.bhard = new.bhard;
                    d.bsoft = new.bsoft;
                }
                if valid & QIF_ILIMITS != 0 {
                    d.ihard = new.ihard;
                    d.isoft = new.isoft;
                }
                q.dirty = true;
                Ok(None)
            }
            QuotaOp::Sync => {
                self.quota_flush()?;
                Ok(None)
            }
        }
    }
}
