//! Fast-commit replay (after the regular journal replay at mount): apply
//! the logical records `ext4_core::fastcommit::scan` found, as Linux's
//! ext4_fc_replay does, then bring the bitmaps in line with the inodes
//! that changed.

use super::*;
use alloc::collections::BTreeSet;
use ext4_core::fastcommit::Tag;

impl Ext2Fs {
    /// Set `count` bits from `first` in group `g`'s block or inode
    /// bitmap; returns how many were clear.
    fn bitmap_set_bits(
        &self,
        meta: &mut Meta,
        g: usize,
        block: bool,
        first: u32,
        count: u32,
    ) -> KResult<u32> {
        let bs = self.block_size as usize;
        let gr = &meta.groups[g];
        let (bb, flag) = if block {
            (gr.block_bitmap, BG_BLOCK_UNINIT)
        } else {
            (gr.inode_bitmap, BG_INODE_UNINIT)
        };
        let uninit = gr.flags & flag != 0 && (self.csum || self.gdt_csum);
        let mut bm = vec![0u8; bs];
        if uninit {
            if block {
                self.init_block_bitmap(meta, g, &mut bm);
            } else {
                for i in self.inodes_per_group as usize..bs * 8 {
                    bm[i / 8] |= 1 << (i % 8);
                }
            }
        } else {
            self.mread(bb, &mut bm)?;
        }
        let mut n = 0;
        for i in first..first + count {
            let (byte, bit) = ((i / 8) as usize, i % 8);
            if bm[byte] & (1 << bit) == 0 {
                bm[byte] |= 1 << bit;
                n += 1;
            }
        }
        self.mwrite(bb, &bm)?;
        let csum_len = if block {
            self.clusters_per_group as usize / 8
        } else {
            self.inodes_per_group as usize / 8
        };
        let crc = csum::bitmap(self.seed, &bm[..csum_len.min(bs)]);
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
        Ok(n)
    }

    /// Mark `count` blocks from `first` in use (their clusters with
    /// bigalloc), adjusting the free counts.
    fn mark_blocks_used(&self, first: u64, count: u64) -> KResult<()> {
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
            let set = self.bitmap_set_bits(&mut meta, g, true, i, n)?;
            meta.groups[g].free_blocks = meta.groups[g].free_blocks.saturating_sub(set);
            meta.free_blocks = meta.free_blocks.saturating_sub(set as u64);
            self.flush_group(&meta, g)?;
            b += (n as u64) << cb;
        }
        Ok(())
    }

    /// Mark inode `ino` in use.
    fn mark_inode_used(&self, ino: u32, is_dir: bool) -> KResult<()> {
        let g = ((ino - 1) / self.inodes_per_group) as usize;
        let i = (ino - 1) % self.inodes_per_group;
        let mut meta = self.meta.lock();
        if self.bitmap_set_bits(&mut meta, g, false, i, 1)? == 1 {
            let gr = &mut meta.groups[g];
            gr.free_inodes = gr.free_inodes.saturating_sub(1);
            if is_dir {
                gr.used_dirs += 1;
            }
            // Keep the uninitialised tail of the inode table beyond it.
            gr.itable_unused = gr.itable_unused.min(self.inodes_per_group - i - 1);
            meta.free_inodes = meta.free_inodes.saturating_sub(1);
        }
        self.flush_group(&meta, g)
    }

    /// Replay fast-commit records.
    pub(super) fn fc_replay(&self, tags: Vec<Tag>) -> KResult<()> {
        let _h = self.begin();
        // Blocks the records map are in use from the start, so tree
        // blocks allocated while replaying never land on them.
        for t in &tags {
            if let Tag::AddRange { pblk, len, .. } = t {
                self.mark_blocks_used(*pblk, *len as u64)?;
            }
        }
        let mut touched = BTreeSet::new();
        let n = tags.len();
        for t in tags {
            match t {
                Tag::AddRange {
                    ino,
                    lblk,
                    len,
                    pblk,
                    uninit,
                } => {
                    let i = self.inode(ino)?;
                    let mut st = i.st.lock();
                    if st.flags & FL_EXTENTS == 0 {
                        continue;
                    }
                    i.ext_punch(&mut st, lblk, lblk.saturating_add(len))?;
                    i.ext_map_range(&mut st, lblk, len, pblk, uninit)?;
                    i.save(&st)?;
                    touched.insert(ino);
                }
                Tag::DelRange { ino, lblk, len } => {
                    let i = self.inode(ino)?;
                    let mut st = i.st.lock();
                    if st.flags & FL_EXTENTS != 0 {
                        i.ext_punch(&mut st, lblk, lblk.saturating_add(len))?;
                        i.save(&st)?;
                    }
                    touched.insert(ino);
                }
                Tag::Inode { ino, raw } => {
                    self.fc_inode(ino, &raw)?;
                    touched.insert(ino);
                }
                Tag::Create { parent, ino, name } | Tag::Link { parent, ino, name } => {
                    let name = String::from_utf8_lossy(&name).into_owned();
                    let child = self.inode(ino)?;
                    let kind = child.kind();
                    if kind == FileType::Directory {
                        self.mark_inode_used(ino, true)?;
                    } else {
                        self.mark_inode_used(ino, false)?;
                    }
                    let dir = self.inode(parent)?;
                    if dir.find_entry(&name).is_err() {
                        dir.add_entry(&name, ino, kind)?;
                    }
                }
                Tag::Unlink { parent, ino, name } => {
                    let name = String::from_utf8_lossy(&name).into_owned();
                    let dir = self.inode(parent)?;
                    if let Ok((i, _, l, off)) = dir.find_entry(&name)
                        && i == ino
                    {
                        dir.remove_entry(l, off)?;
                    }
                }
            }
        }
        // Everything the changed inodes map is in use; i_blocks follows.
        for ino in touched {
            let i = self.inode(ino)?;
            let mut st = i.st.lock();
            if st.mode == 0 || st.links == 0 || st.flags & FL_EXTENTS == 0 {
                continue;
            }
            let ranges = i.ext_blocks(&st)?;
            let mut blocks = 0u64;
            for (b, c) in &ranges {
                self.mark_blocks_used(*b, *c)?;
                blocks += c;
            }
            let per = if st.flags & FL_HUGE_FILE != 0 {
                1
            } else {
                self.block_size / 512
            };
            let xb = (st.xattr_block() != 0) as u64;
            st.blocks512 = (blocks + xb) * per;
            i.save(&st)?;
            self.mark_inode_used(ino, st.kind() == FileType::Directory)?;
        }
        crate::println!("[ext4] fast commit: replayed {} records", n);
        Ok(())
    }

    /// Replace inode `ino` with its logged image, keeping the on-disk
    /// block map for extent files (the replayed ranges built it).
    fn fc_inode(&self, ino: u32, raw: &[u8]) -> KResult<()> {
        let cur = self.read_raw_inode(ino)?;
        let mut new = cur.raw.clone();
        let n = raw.len().min(new.len());
        // Up to i_block, and from i_generation (100) on.
        new[..40].copy_from_slice(&raw[..40.min(n)]);
        if n > 100 {
            new[100..n].copy_from_slice(&raw[100..n]);
        }
        let mut r = RawInode::parse(&new);
        if r.flags & FL_EXTENTS != 0 {
            let root = cur.block_bytes();
            if u16le(&root, 0) == 0xF30A {
                r.set_block_bytes(&root);
            } else {
                r.set_block_bytes(&extent::empty_root());
            }
        } else if n >= 100 {
            r.set_block_bytes(&raw[40..100]);
        }
        self.write_raw_inode(ino, &r)?;
        self.inodes.lock().remove(&ino);
        Ok(())
    }
}
