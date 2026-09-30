//! Hashed directory indexes (htree): inserting into an indexed directory
//! keeps the index valid, splitting leaves and index nodes (and adding an
//! index level) as needed. Lookups follow the index to the leaf for the
//! name's hash (and the next leaves while hashes collide); a damaged index
//! falls back to a linear scan, which works because index blocks look like
//! empty entries.
//!
//! Block 0 holds `.`/`..` and the root (`dx_root_info` at 0x18, entries
//! from 0x20); interior nodes start with an empty entry spanning the block,
//! entries from 8. Entry 0's hash slot holds (limit, count); other entries
//! are (hash, logical block). With metadata_csum each index block ends in
//! an 8-byte tail and each leaf in a 12-byte one.

use super::*;
use ext4_core::hash;

const ROOT_COUNT: usize = 0x20;
const NODE_COUNT: usize = 8;

struct DxLevel {
    l: u64,
    pb: u64,
    buf: Vec<u8>,
    /// Offset of (limit, count).
    co: usize,
    /// Entry followed.
    pos: usize,
}

fn limit(b: &[u8], co: usize) -> usize {
    u16le(b, co) as usize
}
fn count(b: &[u8], co: usize) -> usize {
    u16le(b, co + 2) as usize
}
fn hash_at(b: &[u8], co: usize, i: usize) -> u32 {
    if i == 0 { 0 } else { u32le(b, co + 8 * i) }
}
fn block_at(b: &[u8], co: usize, i: usize) -> u32 {
    u32le(b, co + 8 * i + 4)
}

/// Insert (hash, block) at entry `pos` (> 0).
fn insert_at(b: &mut [u8], co: usize, pos: usize, h: u32, blk: u32) {
    let c = count(b, co);
    let from = co + 8 * pos;
    b.copy_within(from..co + 8 * c, from + 8);
    put32(b, from, h);
    put32(b, from + 4, blk);
    put16(b, co + 2, (c + 1) as u16);
}

impl Ext2Inode {
    fn dx_hash_of(&self, root: &[u8], name: &[u8]) -> u32 {
        let mut v = root[0x1C];
        if self.fs.hash_unsigned && v <= hash::TEA {
            v += 3;
        }
        hash::dx_hash(name, v, self.fs.hash_seed).0
    }

    fn dx_node_limit(&self, co: usize) -> usize {
        (self.fs.block_size as usize - co - if self.fs.csum { 8 } else { 0 }) / 8
    }

    fn dx_read(&self, st: &mut RawInode, l: u64, co: usize, pos: usize) -> KResult<DxLevel> {
        let pb = self.bmap(st, l, false)?.ok_or(EIO)?;
        let mut buf = vec![0u8; self.fs.block_size as usize];
        self.fs.mread(pb, &mut buf)?;
        Ok(DxLevel {
            l,
            pb,
            buf,
            co,
            pos,
        })
    }

    fn dx_write(&self, st: &RawInode, lv: &mut DxLevel) -> KResult<()> {
        let (l, pb) = (lv.l, lv.pb);
        self.write_dir_block(st, l, pb, &mut lv.buf)
    }

    /// Append an empty block to the directory; returns (logical, physical).
    fn dir_append(&self, st: &mut RawInode) -> KResult<(u64, u64)> {
        let bs = self.fs.block_size;
        let l = st.size.div_ceil(bs);
        let pb = self.bmap(st, l, true)?.ok_or(EIO)?;
        st.size = (l + 1) * bs;
        Ok((l, pb))
    }

    pub(super) fn dx_add(&self, st: &mut RawInode, name: &str, ino: u32, tcode: u8) -> KResult<()> {
        let end = self.dir_end();
        let max_levels = if self.fs.largedir { 3 } else { 2 };
        for _ in 0..8 {
            // Walk from the root to the leaf for this name's hash.
            let root = self.dx_read(st, 0, ROOT_COUNT, 0)?;
            if root.buf[0x1D] != 8 || u32le(&root.buf, 0x18) != 0 {
                return Err(EIO);
            }
            let levels = root.buf[0x1E] as usize;
            if levels >= max_levels {
                return Err(EIO);
            }
            let h = self.dx_hash_of(&root.buf, name.as_bytes());
            let mut path = vec![root];
            loop {
                let lv = path.last_mut().unwrap();
                let c = count(&lv.buf, lv.co);
                if c == 0 || c > limit(&lv.buf, lv.co) {
                    return Err(EIO);
                }
                let mut pos = 0;
                for i in 1..c {
                    if hash_at(&lv.buf, lv.co, i) <= h {
                        pos = i;
                    } else {
                        break;
                    }
                }
                lv.pos = pos;
                let child = block_at(&lv.buf, lv.co, pos) as u64;
                if path.len() > levels {
                    break;
                }
                let n = self.dx_read(st, child, NODE_COUNT, 0)?;
                path.push(n);
            }
            let leaf_l = {
                let lv = path.last().unwrap();
                block_at(&lv.buf, lv.co, lv.pos) as u64
            };
            let mut leaf = self.dx_read(st, leaf_l, 0, 0)?;
            if leaf_insert(&mut leaf.buf, end, name, ino, tcode) {
                return self.dx_write(st, &mut leaf);
            }
            // The leaf is full: find the deepest index level with room.
            let room = (0..path.len())
                .rev()
                .find(|&i| count(&path[i].buf, path[i].co) < limit(&path[i].buf, path[i].co));
            match room {
                Some(i) if i + 1 == path.len() => {
                    self.dx_split_leaf(st, &mut path[i], &mut leaf, end)?
                }
                Some(i) => {
                    let (upper, lower) = path.split_at_mut(i + 1);
                    self.dx_split_node(st, &mut upper[i], &mut lower[0])?
                }
                None if levels + 1 < max_levels => self.dx_add_level(st, &mut path[0])?,
                None => return Err(ENOSPC),
            }
        }
        Err(EIO)
    }

    /// Look `name` up through the index: Some((ino, type, logical block,
    /// offset)), None if absent, Err if the index looks damaged.
    pub(super) fn dx_find(
        &self,
        st: &mut RawInode,
        name: &str,
    ) -> KResult<Option<(u32, u8, u64, usize)>> {
        let root = self.dx_read(st, 0, ROOT_COUNT, 0)?;
        if root.buf[0x1D] != 8 || u32le(&root.buf, 0x18) != 0 {
            return Err(EIO);
        }
        let levels = root.buf[0x1E] as usize;
        if levels > 2 {
            return Err(EIO);
        }
        let h = self.dx_hash_of(&root.buf, name.as_bytes());
        // Walk down, remembering each level to find the following leaves.
        let mut path = vec![root];
        loop {
            let lv = path.last_mut().unwrap();
            let c = count(&lv.buf, lv.co);
            if c == 0 || c > limit(&lv.buf, lv.co) {
                return Err(EIO);
            }
            let mut pos = 0;
            for i in 1..c {
                if hash_at(&lv.buf, lv.co, i) <= h {
                    pos = i;
                } else {
                    break;
                }
            }
            lv.pos = pos;
            let child = block_at(&lv.buf, lv.co, pos) as u64;
            if path.len() > levels {
                break;
            }
            let n = self.dx_read(st, child, NODE_COUNT, 0)?;
            path.push(n);
        }
        let bs = self.fs.block_size as usize;
        for _ in 0..64 {
            let leaf_l = {
                let lv = path.last().unwrap();
                block_at(&lv.buf, lv.co, lv.pos) as u64
            };
            let leaf = self.dx_read(st, leaf_l, 0, 0)?;
            if let Some((ino, t, off)) = leaf_find(&leaf.buf, bs, name.as_bytes()) {
                return Ok(Some((ino, t, leaf_l, off)));
            }
            // Continue in the next leaf only if its first hash collides
            // with ours (the low bit marks a continuation).
            let lv = path.last_mut().unwrap();
            let c = count(&lv.buf, lv.co);
            if lv.pos + 1 >= c {
                return Ok(None); // (a deeper walk across nodes is not needed for 2 levels in practice)
            }
            let next = hash_at(&lv.buf, lv.co, lv.pos + 1);
            if next & !1 != h & !1 || next & 1 == 0 {
                return Ok(None);
            }
            lv.pos += 1;
        }
        Ok(None)
    }

    /// Split a full leaf by hash into itself and a new block, and index
    /// the new block in `parent` after the entry that led to the leaf.
    fn dx_split_leaf(
        &self,
        st: &mut RawInode,
        parent: &mut DxLevel,
        leaf: &mut DxLevel,
        end: usize,
    ) -> KResult<()> {
        let bs = self.fs.block_size as usize;
        let root_hash = {
            let mut b = vec![0u8; bs];
            self.fs
                .mread(self.bmap(st, 0, false)?.ok_or(EIO)?, &mut b)?;
            b
        };
        // (hash, record bytes)
        let mut ents: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut o = 0;
        while o + 8 <= end {
            let ino = u32le(&leaf.buf, o);
            let rec = u16le(&leaf.buf, o + 4) as usize;
            if rec < 8 || o + rec > end {
                return Err(EIO);
            }
            if ino != 0 {
                let nl = leaf.buf[o + 6] as usize;
                let h = self.dx_hash_of(&root_hash, &leaf.buf[o + 8..o + 8 + nl]);
                let mut r = leaf.buf[o..o + (8 + nl).next_multiple_of(4)].to_vec();
                let rl = r.len() as u16;
                put16(&mut r, 4, rl);
                ents.push((h, r));
            }
            o += rec;
        }
        if ents.len() < 2 {
            return Err(ENOSPC);
        }
        ents.sort_by_key(|(h, _)| *h);
        let mid = ents.len() / 2;
        let split = ents[mid].0;
        // Equal hashes continue in the next block.
        let cont = (ents[mid - 1].0 == split) as u32;
        let fill = |buf: &mut [u8], part: &[(u32, Vec<u8>)]| {
            leaf_empty(buf, end);
            let mut o = 0;
            for (i, (_, r)) in part.iter().enumerate() {
                buf[o..o + r.len()].copy_from_slice(r);
                let rec = if i + 1 == part.len() {
                    end - o
                } else {
                    r.len()
                };
                put16(buf, o + 4, rec as u16);
                o += r.len();
            }
        };
        let (nl, npb) = self.dir_append(st)?;
        let mut new = DxLevel {
            l: nl,
            pb: npb,
            buf: vec![0u8; bs],
            co: 0,
            pos: 0,
        };
        fill(&mut new.buf, &ents[mid..]);
        fill(&mut leaf.buf, &ents[..mid]);
        self.dx_write(st, &mut new)?;
        self.dx_write(st, leaf)?;
        let pos = parent.pos + 1;
        insert_at(&mut parent.buf, parent.co, pos, split | cont, nl as u32);
        self.dx_write(st, parent)
    }

    /// Split a full index node into itself and a new node indexed in
    /// `parent`.
    fn dx_split_node(
        &self,
        st: &mut RawInode,
        parent: &mut DxLevel,
        node: &mut DxLevel,
    ) -> KResult<()> {
        let bs = self.fs.block_size as usize;
        let c = count(&node.buf, node.co);
        let half = c / 2;
        let key = hash_at(&node.buf, node.co, half);
        let (nl, npb) = self.dir_append(st)?;
        let mut new = DxLevel {
            l: nl,
            pb: npb,
            buf: vec![0u8; bs],
            co: NODE_COUNT,
            pos: 0,
        };
        put16(&mut new.buf, 4, bs as u16);
        let lim = self.dx_node_limit(NODE_COUNT);
        put16(&mut new.buf, NODE_COUNT, lim as u16);
        put16(&mut new.buf, NODE_COUNT + 2, (c - half) as u16);
        put32(
            &mut new.buf,
            NODE_COUNT + 4,
            block_at(&node.buf, node.co, half),
        );
        for i in half + 1..c {
            let j = i - half;
            put32(
                &mut new.buf,
                NODE_COUNT + 8 * j,
                hash_at(&node.buf, node.co, i),
            );
            put32(
                &mut new.buf,
                NODE_COUNT + 8 * j + 4,
                block_at(&node.buf, node.co, i),
            );
        }
        let co = node.co;
        node.buf[co + 8 * half..co + 8 * c].fill(0);
        put16(&mut node.buf, co + 2, half as u16);
        self.dx_write(st, &mut new)?;
        self.dx_write(st, node)?;
        let pos = parent.pos + 1;
        insert_at(&mut parent.buf, parent.co, pos, key, nl as u32);
        self.dx_write(st, parent)
    }

    /// Move the root's entries into a new node: one more index level.
    fn dx_add_level(&self, st: &mut RawInode, root: &mut DxLevel) -> KResult<()> {
        let bs = self.fs.block_size as usize;
        let c = count(&root.buf, ROOT_COUNT);
        let (nl, npb) = self.dir_append(st)?;
        let mut new = DxLevel {
            l: nl,
            pb: npb,
            buf: vec![0u8; bs],
            co: NODE_COUNT,
            pos: 0,
        };
        put16(&mut new.buf, 4, bs as u16);
        new.buf[NODE_COUNT..NODE_COUNT + 8 * c]
            .copy_from_slice(&root.buf[ROOT_COUNT..ROOT_COUNT + 8 * c]);
        put16(
            &mut new.buf,
            NODE_COUNT,
            self.dx_node_limit(NODE_COUNT) as u16,
        );
        self.dx_write(st, &mut new)?;
        root.buf[ROOT_COUNT + 8..ROOT_COUNT + 8 * c].fill(0);
        put16(&mut root.buf, ROOT_COUNT + 2, 1);
        put32(&mut root.buf, ROOT_COUNT + 4, nl as u32);
        root.buf[0x1E] += 1;
        self.dx_write(st, root)
    }
}

/// Find `name` in one directory leaf block: (inode, type, offset).
fn leaf_find(b: &[u8], bs: usize, name: &[u8]) -> Option<(u32, u8, usize)> {
    let mut o = 0;
    while o + 8 <= bs {
        let ino = u32le(b, o);
        let rec = u16le(b, o + 4) as usize;
        let nl = b[o + 6] as usize;
        if rec < 8 || o + rec > bs {
            return None;
        }
        if ino != 0 && nl == name.len() && o + 8 + nl <= bs && &b[o + 8..o + 8 + nl] == name {
            return Some((ino, b[o + 7], o));
        }
        o += rec;
    }
    None
}
