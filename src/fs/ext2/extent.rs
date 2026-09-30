//! ext4 extent trees: lookup, allocation (extending an extent or inserting
//! one, splitting nodes and growing the tree), conversion of uninitialized
//! (preallocated) extents on write, and truncation.
//!
//! The root node lives in the inode's `i_block` (4 entries); other nodes
//! fill a block (with a checksum tail when metadata_csum is on). Index
//! entries always carry the first logical block of their child.

use super::*;

const EXT_MAGIC: u16 = 0xF30A;
/// Longest initialized / uninitialized extent.
const INIT_MAX: u32 = 32768;
const UNINIT_MAX: u32 = 32767;

#[derive(Clone, Copy, Debug)]
pub(super) struct Extent {
    lblk: u32,
    len: u32,
    pblk: u64,
    uninit: bool,
}

fn entries(n: &[u8]) -> usize {
    u16le(n, 2) as usize
}
fn max_entries(n: &[u8]) -> usize {
    u16le(n, 4) as usize
}
fn depth(n: &[u8]) -> u16 {
    u16le(n, 6)
}
fn set_entries(n: &mut [u8], v: usize) {
    put16(n, 2, v as u16);
}

fn header(n: &mut [u8], count: usize, max: usize, depth: u16) {
    put16(n, 0, EXT_MAGIC);
    put16(n, 2, count as u16);
    put16(n, 4, max as u16);
    put16(n, 6, depth);
    put32(n, 8, 0);
}

fn read_ext(n: &[u8], i: usize) -> Extent {
    let o = 12 + 12 * i;
    let raw = u16le(n, o + 4) as u32;
    let (len, uninit) = if raw > INIT_MAX {
        (raw - INIT_MAX, true)
    } else {
        (raw, false)
    };
    Extent {
        lblk: u32le(n, o),
        len,
        pblk: (u16le(n, o + 6) as u64) << 32 | u32le(n, o + 8) as u64,
        uninit,
    }
}

fn write_ext(n: &mut [u8], i: usize, e: Extent) {
    let o = 12 + 12 * i;
    put32(n, o, e.lblk);
    put16(
        n,
        o + 4,
        if e.uninit { e.len + INIT_MAX } else { e.len } as u16,
    );
    put16(n, o + 6, (e.pblk >> 32) as u16);
    put32(n, o + 8, e.pblk as u32);
}

fn read_idx(n: &[u8], i: usize) -> (u32, u64) {
    let o = 12 + 12 * i;
    (
        u32le(n, o),
        u32le(n, o + 4) as u64 | (u16le(n, o + 8) as u64) << 32,
    )
}

fn write_idx(n: &mut [u8], i: usize, key: u32, child: u64) {
    let o = 12 + 12 * i;
    put32(n, o, key);
    put32(n, o + 4, child as u32);
    put16(n, o + 8, (child >> 32) as u16);
    put16(n, o + 10, 0);
}

/// First logical block covered by a node.
fn first_key(n: &[u8]) -> u32 {
    if entries(n) == 0 {
        0
    } else if depth(n) == 0 {
        read_ext(n, 0).lblk
    } else {
        read_idx(n, 0).0
    }
}

/// Insert a 12-byte entry slot at `pos` (shifting the rest right).
fn open_slot(n: &mut [u8], pos: usize) {
    let c = entries(n);
    let (from, to) = (12 + 12 * pos, 12 + 12 * c);
    n.copy_within(from..to, from + 12);
    set_entries(n, c + 1);
}

fn remove_slot(n: &mut [u8], pos: usize) {
    let c = entries(n);
    let (from, to) = (12 + 12 * (pos + 1), 12 + 12 * c);
    n.copy_within(from..to, from - 12);
    n[12 + 12 * (c - 1)..12 + 12 * c].fill(0);
    set_entries(n, c - 1);
}

/// An empty root (for new inodes).
pub(super) fn empty_root() -> [u8; 60] {
    let mut r = [0u8; 60];
    header(&mut r, 0, 4, 0);
    r
}

/// A lookup path and the leaf entry containing the target, if any.
type Found = (Vec<Level>, Option<(usize, Extent)>);

/// One level of a lookup path.
struct Level {
    /// Node block; 0 = the root in the inode.
    blk: u64,
    node: Vec<u8>,
    /// Index levels: the entry followed. Leaf: number of extents starting
    /// at or before the target (the insert position).
    idx: usize,
}

impl Ext2Inode {
    fn node_max(&self) -> usize {
        (self.fs.block_size as usize - 12 - if self.fs.csum { 4 } else { 0 }) / 12
    }

    fn ext_path(&self, st: &RawInode, lblk: u32) -> KResult<Vec<Level>> {
        let mut path = Vec::new();
        let mut node = st.block_bytes().to_vec();
        let mut blk = 0;
        loop {
            if u16le(&node, 0) != EXT_MAGIC || path.len() > 8 {
                return Err(EIO);
            }
            let n = entries(&node);
            if depth(&node) == 0 {
                let pos = (0..n)
                    .take_while(|&i| read_ext(&node, i).lblk <= lblk)
                    .count();
                path.push(Level {
                    blk,
                    node,
                    idx: pos,
                });
                return Ok(path);
            }
            if n == 0 {
                return Err(EIO);
            }
            let mut i = 0;
            for k in 1..n {
                if read_idx(&node, k).0 <= lblk {
                    i = k;
                } else {
                    break;
                }
            }
            let child = read_idx(&node, i).1;
            path.push(Level { blk, node, idx: i });
            node = vec![0u8; self.fs.block_size as usize];
            self.fs.mread(child, &mut node)?;
            blk = child;
        }
    }

    /// Write a node back: the root into the inode (saved by the caller),
    /// others as metadata blocks with their checksum.
    fn ext_write(&self, st: &mut RawInode, blk: u64, node: &mut [u8]) -> KResult<()> {
        if blk == 0 {
            st.set_block_bytes(&node[..60]);
            return Ok(());
        }
        if self.fs.csum {
            csum::set_extent_block(self.iseed(st), node);
        }
        self.fs.mwrite(blk, node)
    }

    /// Allocate a tree node block.
    fn ext_new_node(&self, st: &mut RawInode) -> KResult<(u64, Vec<u8>)> {
        let goal = self.goal();
        let b = self.alloc_for(st, goal)?;
        Ok((b, vec![0u8; self.fs.block_size as usize]))
    }

    /// Insert an extent that overlaps no existing one.
    fn ext_insert(&self, st: &mut RawInode, e: Extent) -> KResult<()> {
        for _ in 0..16 {
            let mut path = self.ext_path(st, e.lblk)?;
            let d = path.len();
            let leaf = &mut path[d - 1];
            if entries(&leaf.node) < max_entries(&leaf.node) {
                let pos = leaf.idx;
                open_slot(&mut leaf.node, pos);
                write_ext(&mut leaf.node, pos, e);
                let blk = leaf.blk;
                let mut node = core::mem::take(&mut leaf.node);
                self.ext_write(st, blk, &mut node)?;
                // A new first extent lowers the keys above it.
                if pos == 0 {
                    for lv in (0..d - 1).rev() {
                        let l = &mut path[lv];
                        let (key, child) = read_idx(&l.node, l.idx);
                        if key == e.lblk {
                            break;
                        }
                        write_idx(&mut l.node, l.idx, e.lblk, child);
                        let (blk, idx) = (l.blk, l.idx);
                        let mut node = core::mem::take(&mut l.node);
                        self.ext_write(st, blk, &mut node)?;
                        if idx != 0 {
                            break;
                        }
                    }
                }
                return Ok(());
            }
            // Make room: split the node below the deepest level with room,
            // or grow the tree when every level is full.
            match (0..d - 1)
                .rev()
                .find(|&i| entries(&path[i].node) < max_entries(&path[i].node))
            {
                None => self.ext_grow(st)?,
                Some(i) => self.ext_split(st, &mut path, i + 1)?,
            }
        }
        Err(EIO)
    }

    /// Move the root's entries into a new block below it.
    fn ext_grow(&self, st: &mut RawInode) -> KResult<()> {
        let root = st.block_bytes();
        let n = entries(&root);
        let (b, mut node) = self.ext_new_node(st)?;
        header(&mut node, n, self.node_max(), depth(&root));
        node[12..12 + 12 * n].copy_from_slice(&root[12..12 + 12 * n]);
        self.ext_write(st, b, &mut node)?;
        let mut r = [0u8; 60];
        header(&mut r, 1, 4, depth(&root) + 1);
        write_idx(&mut r, 0, first_key(&root), b);
        st.set_block_bytes(&r);
        Ok(())
    }

    /// Split the (full) node at `path[lvl]` in two; its parent has room.
    fn ext_split(&self, st: &mut RawInode, path: &mut [Level], lvl: usize) -> KResult<()> {
        let n = entries(&path[lvl].node);
        let half = n / 2;
        let (b, mut new) = self.ext_new_node(st)?;
        let d = depth(&path[lvl].node);
        header(&mut new, n - half, self.node_max(), d);
        new[12..12 + 12 * (n - half)].copy_from_slice(&path[lvl].node[12 + 12 * half..12 + 12 * n]);
        let key = first_key(&new);
        self.ext_write(st, b, &mut new)?;
        {
            let old = &mut path[lvl];
            old.node[12 + 12 * half..12 + 12 * n].fill(0);
            set_entries(&mut old.node, half);
            let blk = old.blk;
            let mut node = core::mem::take(&mut old.node);
            self.ext_write(st, blk, &mut node)?;
        }
        let parent = &mut path[lvl - 1];
        let pos = parent.idx + 1;
        open_slot(&mut parent.node, pos);
        write_idx(&mut parent.node, pos, key, b);
        let blk = parent.blk;
        let mut node = core::mem::take(&mut parent.node);
        self.ext_write(st, blk, &mut node)
    }

    /// Replace leaf entry `i` of the leaf at the end of `path` and write it.
    fn ext_set(
        &self,
        st: &mut RawInode,
        path: &mut [Level],
        i: usize,
        e: Option<Extent>,
    ) -> KResult<()> {
        let leaf = path.last_mut().unwrap();
        match e {
            Some(e) => write_ext(&mut leaf.node, i, e),
            None => remove_slot(&mut leaf.node, i),
        }
        let blk = leaf.blk;
        let mut node = leaf.node.clone();
        self.ext_write(st, blk, &mut node)
    }

    /// The extent containing `l`, if any, with the path to its leaf and its
    /// index there.
    fn ext_find(&self, st: &RawInode, l: u32) -> KResult<Found> {
        let path = self.ext_path(st, l)?;
        let leaf = path.last().unwrap();
        let hit = (leaf.idx > 0)
            .then(|| (leaf.idx - 1, read_ext(&leaf.node, leaf.idx - 1)))
            .filter(|(_, e)| l < e.lblk + e.len);
        Ok((path, hit))
    }

    /// Map logical block `l`; with `create`, allocate it (or convert it
    /// from uninitialized).
    pub(super) fn ext_map(&self, st: &mut RawInode, l: u64, create: bool) -> KResult<Option<u64>> {
        let l = u32::try_from(l).map_err(|_| EFBIG)?;
        let (mut path, hit) = self.ext_find(st, l)?;
        if let Some((i, e)) = hit {
            let p = e.pblk + (l - e.lblk) as u64;
            if !e.uninit {
                return Ok(Some(p));
            }
            if !create {
                return Ok(None);
            }
            self.ext_convert(st, &mut path, i, e, l)?;
            return Ok(Some(p));
        }
        if !create {
            return Ok(None);
        }
        self.ext_alloc(st, path, l, false).map(Some)
    }

    /// Allocate a block for the unmapped logical block `l`.
    fn ext_alloc(
        &self,
        st: &mut RawInode,
        mut path: Vec<Level>,
        l: u32,
        uninit: bool,
    ) -> KResult<u64> {
        let leaf = path.last().unwrap();
        let prev = (leaf.idx > 0).then(|| (leaf.idx - 1, read_ext(&leaf.node, leaf.idx - 1)));
        let goal = match prev {
            Some((_, e)) => e.pblk + (l - e.lblk) as u64,
            None => self.goal(),
        };
        let b = match self.cluster_sibling(st, l)? {
            // bigalloc: the logical cluster already has a physical one.
            Some(p) => {
                let bs = self.fs.block_size;
                self.fs.dev.write_bytes(p * bs, &vec![0u8; bs as usize])?;
                p
            }
            None => {
                let c = self.alloc_for(st, goal)?;
                let off = (l as u64) & ((1 << self.fs.cbits) - 1);
                if off != 0 {
                    let bs = self.fs.block_size;
                    self.fs
                        .dev
                        .write_bytes((c + off) * bs, &vec![0u8; bs as usize])?;
                }
                c + off
            }
        };
        let max = if uninit { UNINIT_MAX } else { INIT_MAX };
        if let Some((i, e)) = prev
            && e.uninit == uninit
            && e.lblk + e.len == l
            && e.pblk + e.len as u64 == b
            && e.len < max
        {
            self.ext_set(
                st,
                &mut path,
                i,
                Some(Extent {
                    len: e.len + 1,
                    ..e
                }),
            )?;
        } else {
            self.ext_insert(
                st,
                Extent {
                    lblk: l,
                    len: 1,
                    pblk: b,
                    uninit,
                },
            )?;
        }
        Ok(b)
    }

    /// Map `len` blocks at logical `lblk` to physical `pblk` (fast-commit
    /// replay; the range must be unmapped).
    pub(super) fn ext_map_range(
        &self,
        st: &mut RawInode,
        lblk: u32,
        len: u32,
        pblk: u64,
        uninit: bool,
    ) -> KResult<()> {
        let max = if uninit { UNINIT_MAX } else { INIT_MAX };
        let mut done = 0;
        while done < len {
            let n = (len - done).min(max);
            self.ext_insert(
                st,
                Extent {
                    lblk: lblk + done,
                    len: n,
                    pblk: pblk + done as u64,
                    uninit,
                },
            )?;
            done += n;
        }
        Ok(())
    }

    /// Physical ranges (first, count) the file uses: data and tree nodes.
    pub(super) fn ext_blocks(&self, st: &RawInode) -> KResult<Vec<(u64, u64)>> {
        let mut exts = Vec::new();
        let mut nodes = Vec::new();
        self.ext_collect(&st.block_bytes(), &mut exts, &mut nodes)?;
        let mut v: Vec<(u64, u64)> = exts.iter().map(|e| (e.pblk, e.len as u64)).collect();
        v.extend(nodes.iter().map(|&b| (b, 1)));
        Ok(v)
    }

    /// bigalloc: the physical block for `l` if another block of its
    /// logical cluster is mapped (all blocks of a logical cluster share one
    /// physical cluster, at the same offsets).
    fn cluster_sibling(&self, st: &RawInode, l: u32) -> KResult<Option<u64>> {
        let cb = self.fs.cbits;
        if cb == 0 {
            return Ok(None);
        }
        let first = l >> cb << cb;
        for c in first..first + (1 << cb) {
            if c == l {
                continue;
            }
            if let (_, Some((_, e))) = self.ext_find(st, c)? {
                let p = e.pblk + (c - e.lblk) as u64;
                return Ok(Some(p - (c - first) as u64 + (l - first) as u64));
            }
        }
        Ok(None)
    }

    /// Make block `l` of uninitialized extent `e` (leaf entry `i`)
    /// initialized: zero it and split the extent around it, merging into
    /// the previous extent when they are contiguous (sequential writes into
    /// preallocated space then keep one growing extent).
    fn ext_convert(
        &self,
        st: &mut RawInode,
        path: &mut [Level],
        i: usize,
        e: Extent,
        l: u32,
    ) -> KResult<()> {
        let bs = self.fs.block_size;
        let off = l - e.lblk;
        let p = e.pblk + off as u64;
        self.fs.dev.write_bytes(p * bs, &vec![0u8; bs as usize])?;
        if off == 0 {
            if i > 0 {
                let leaf = &path.last().unwrap().node;
                let a = read_ext(leaf, i - 1);
                if !a.uninit
                    && a.lblk + a.len == l
                    && a.pblk + a.len as u64 == p
                    && a.len < INIT_MAX
                {
                    let leaf = path.last_mut().unwrap();
                    write_ext(
                        &mut leaf.node,
                        i - 1,
                        Extent {
                            len: a.len + 1,
                            ..a
                        },
                    );
                    let rest = if e.len > 1 {
                        Some(Extent {
                            lblk: l + 1,
                            len: e.len - 1,
                            pblk: p + 1,
                            uninit: true,
                        })
                    } else {
                        None
                    };
                    return self.ext_set(st, path, i, rest);
                }
            }
            let one = Extent {
                lblk: l,
                len: 1,
                pblk: p,
                uninit: false,
            };
            self.ext_set(st, path, i, Some(one))?;
            if e.len > 1 {
                self.ext_insert(
                    st,
                    Extent {
                        lblk: l + 1,
                        len: e.len - 1,
                        pblk: p + 1,
                        uninit: true,
                    },
                )?;
            }
            return Ok(());
        }
        self.ext_set(st, path, i, Some(Extent { len: off, ..e }))?;
        self.ext_insert(
            st,
            Extent {
                lblk: l,
                len: 1,
                pblk: p,
                uninit: false,
            },
        )?;
        if off + 1 < e.len {
            self.ext_insert(
                st,
                Extent {
                    lblk: l + 1,
                    len: e.len - off - 1,
                    pblk: p + 1,
                    uninit: true,
                },
            )?;
        }
        Ok(())
    }

    /// Preallocate logical block `l` as uninitialized (fallocate).
    pub(super) fn ext_prealloc(&self, st: &mut RawInode, l: u64) -> KResult<()> {
        let l = u32::try_from(l).map_err(|_| EFBIG)?;
        let (path, hit) = self.ext_find(st, l)?;
        if hit.is_none() {
            self.ext_alloc(st, path, l, true)?;
        }
        Ok(())
    }

    /// All extents (in order) and the tree's node blocks.
    fn ext_collect(
        &self,
        node: &[u8],
        exts: &mut Vec<Extent>,
        nodes: &mut Vec<u64>,
    ) -> KResult<()> {
        if u16le(node, 0) != EXT_MAGIC {
            return Err(EIO);
        }
        let n = entries(node);
        if depth(node) == 0 {
            exts.extend((0..n).map(|i| read_ext(node, i)));
            return Ok(());
        }
        let mut buf = vec![0u8; self.fs.block_size as usize];
        for i in 0..n {
            let child = read_idx(node, i).1;
            nodes.push(child);
            self.fs.mread(child, &mut buf)?;
            self.ext_collect(&buf.clone(), exts, nodes)?;
        }
        Ok(())
    }

    /// Free everything from logical block `keep` on, then rebuild the tree
    /// from the extents that remain.
    pub(super) fn ext_truncate(&self, st: &mut RawInode, keep: u64) -> KResult<()> {
        let keep = keep.min(u32::MAX as u64) as u32;
        self.ext_punch(st, keep, u32::MAX)
    }

    /// Unmap and free logical blocks `start..end`, then rebuild the tree
    /// from the extents that remain.
    pub(super) fn ext_punch(&self, st: &mut RawInode, start: u32, end: u32) -> KResult<()> {
        let mut exts = Vec::new();
        let mut nodes = Vec::new();
        self.ext_collect(&st.block_bytes(), &mut exts, &mut nodes)?;
        if exts
            .iter()
            .all(|e| e.lblk + e.len <= start || e.lblk >= end)
        {
            return Ok(());
        }
        let mut kept = Vec::with_capacity(exts.len());
        let mut gone: Vec<(u64, u64)> = Vec::new();
        for e in exts {
            let (a, b) = (e.lblk, e.lblk + e.len);
            if b <= start || a >= end {
                kept.push(e);
                continue;
            }
            let (ca, cb) = (a.max(start), b.min(end));
            gone.push((e.pblk + (ca - a) as u64, (cb - ca) as u64));
            if a < ca {
                kept.push(Extent { len: ca - a, ..e });
            }
            if cb < b {
                kept.push(Extent {
                    lblk: cb,
                    len: b - cb,
                    pblk: e.pblk + (cb - a) as u64,
                    uninit: e.uninit,
                });
            }
        }
        let cb = self.fs.cbits;
        let mut freed = 0i64;
        if cb == 0 {
            for &(p, n) in &gone {
                self.fs.free_blocks(p, n)?;
                freed += n as i64;
            }
            for &b in &nodes {
                self.fs.free_blocks(b, 1)?;
            }
            freed += nodes.len() as i64;
        } else {
            // bigalloc: free the clusters no kept block still uses.
            let mut clusters = alloc::collections::BTreeSet::new();
            for &(p, n) in &gone {
                clusters.extend((p >> cb)..=((p + n - 1) >> cb));
            }
            for e in &kept {
                let (p, n) = (e.pblk, e.len as u64);
                for c in (p >> cb)..=((p + n - 1) >> cb) {
                    clusters.remove(&c);
                }
            }
            clusters.extend(nodes.iter().map(|b| b >> cb));
            for &c in &clusters {
                self.fs.free_blocks(c << cb, 1)?;
            }
            freed = (clusters.len() as i64) << cb;
        }
        self.add_blocks(st, -freed);
        self.ext_build(st, &kept)
    }

    /// Build a tree holding `exts` from scratch.
    fn ext_build(&self, st: &mut RawInode, exts: &[Extent]) -> KResult<()> {
        let mut root = [0u8; 60];
        if exts.len() <= 4 {
            header(&mut root, exts.len(), 4, 0);
            for (i, e) in exts.iter().enumerate() {
                write_ext(&mut root, i, *e);
            }
            st.set_block_bytes(&root);
            return Ok(());
        }
        let per = self.node_max();
        let mut level: Vec<(u32, u64)> = Vec::new();
        for chunk in exts.chunks(per) {
            let (b, mut node) = self.ext_new_node(st)?;
            header(&mut node, chunk.len(), per, 0);
            for (i, e) in chunk.iter().enumerate() {
                write_ext(&mut node, i, *e);
            }
            self.ext_write(st, b, &mut node)?;
            level.push((chunk[0].lblk, b));
        }
        let mut d = 1;
        while level.len() > 4 {
            let mut next = Vec::new();
            for chunk in level.chunks(per) {
                let (b, mut node) = self.ext_new_node(st)?;
                header(&mut node, chunk.len(), per, d);
                for (i, (k, c)) in chunk.iter().enumerate() {
                    write_idx(&mut node, i, *k, *c);
                }
                self.ext_write(st, b, &mut node)?;
                next.push((chunk[0].0, b));
            }
            level = next;
            d += 1;
        }
        header(&mut root, level.len(), 4, d);
        for (i, (k, c)) in level.iter().enumerate() {
            write_idx(&mut root, i, *k, *c);
        }
        st.set_block_bytes(&root);
        Ok(())
    }
}
