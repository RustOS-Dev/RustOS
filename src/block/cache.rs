//! Write-back block cache.
//!
//! One cache per physical disk, keyed by absolute 4 KiB block; partitions
//! are windows into their disk's cache so both views stay coherent. Misses
//! read ahead up to 128 KiB; dirty blocks are written back (coalesced into
//! runs) on sync, on eviction, and by the periodic flusher.

use super::BlockDevice;
use crate::errno::*;
use crate::sched::mutex::Mutex;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

const BLOCK: usize = 4096;
const READ_AHEAD_BLOCKS: u64 = 32;

struct Entry {
    data: Vec<u8>,
    dirty: bool,
    used: u64,
}

struct Cache {
    dev: Arc<dyn BlockDevice>,
    blocks: BTreeMap<u64, Entry>,
    capacity: usize,
    clock: u64,
}

impl Cache {
    fn spb(&self) -> u64 {
        (BLOCK / self.dev.sector_size()).max(1) as u64
    }

    fn total_blocks(&self) -> u64 {
        (self.dev.sector_count()).div_ceil(self.spb())
    }

    /// Read `n` blocks starting at `blk` from the device (clipped to the end).
    fn fetch(&mut self, blk: u64, n: u64) -> KResult<()> {
        let n = n.min(self.total_blocks() - blk);
        let spb = self.spb();
        let ss = self.dev.sector_size() as u64;
        let sectors = (n * spb).min(self.dev.sector_count() - blk * spb);
        let mut buf = alloc::vec![0u8; (n as usize) * BLOCK];
        self.dev
            .read_sectors(blk * spb, &mut buf[..(sectors * ss) as usize])?;
        self.make_room(n as usize)?;
        for i in 0..n {
            if self.blocks.contains_key(&(blk + i)) {
                continue;
            }
            self.clock += 1;
            let data = buf[(i as usize) * BLOCK..(i as usize + 1) * BLOCK].to_vec();
            self.blocks.insert(
                blk + i,
                Entry {
                    data,
                    dirty: false,
                    used: self.clock,
                },
            );
        }
        Ok(())
    }

    fn make_room(&mut self, need: usize) -> KResult<()> {
        while self.blocks.len() + need > self.capacity && !self.blocks.is_empty() {
            let victim = self
                .blocks
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| *k)
                .unwrap();
            if self.blocks[&victim].dirty {
                self.write_back(Some(victim))?;
            }
            self.blocks.remove(&victim);
        }
        Ok(())
    }

    fn get(&mut self, blk: u64) -> KResult<&mut Entry> {
        if !self.blocks.contains_key(&blk) {
            // Read ahead over the following missing blocks.
            let mut n = 1;
            while n < READ_AHEAD_BLOCKS
                && blk + n < self.total_blocks()
                && !self.blocks.contains_key(&(blk + n))
            {
                n += 1;
            }
            self.fetch(blk, n)?;
        }
        self.clock += 1;
        let c = self.clock;
        let e = self.blocks.get_mut(&blk).ok_or(EIO)?;
        e.used = c;
        Ok(e)
    }

    /// Write dirty blocks (all, or the run containing `only`).
    fn write_back(&mut self, only: Option<u64>) -> KResult<()> {
        let spb = self.spb();
        let ss = self.dev.sector_size() as u64;
        let total_sectors = self.dev.sector_count();
        let mut dirty: Vec<u64> = self
            .blocks
            .iter()
            .filter(|(_, e)| e.dirty)
            .map(|(k, _)| *k)
            .collect();
        if let Some(b) = only {
            dirty.retain(|&k| k == b);
        }
        let mut i = 0;
        while i < dirty.len() {
            let start = dirty[i];
            let mut end = i + 1;
            while end < dirty.len() && dirty[end] == dirty[end - 1] + 1 && end - i < 64 {
                end += 1;
            }
            let n = (end - i) as u64;
            let mut buf = Vec::with_capacity(n as usize * BLOCK);
            for k in start..start + n {
                buf.extend_from_slice(&self.blocks[&k].data);
            }
            let sectors = (n * spb).min(total_sectors - start * spb);
            self.dev
                .write_sectors(start * spb, &buf[..(sectors * ss) as usize])?;
            for k in start..start + n {
                if let Some(e) = self.blocks.get_mut(&k) {
                    e.dirty = false;
                }
            }
            i = end;
        }
        Ok(())
    }
}

pub struct CachedDevice {
    cache: Arc<Mutex<Cache>>,
    /// Byte offset of this view on the disk.
    base: u64,
    size: u64,
    sector_size: usize,
    raw: Arc<dyn BlockDevice>,
    pub name: String,
}

impl CachedDevice {
    pub fn new(dev: Arc<dyn BlockDevice>, name: &str) -> Arc<CachedDevice> {
        let heap = crate::allocator::heap_size() as usize;
        let capacity = (heap / 8 / BLOCK).clamp(256, 8192);
        Arc::new(CachedDevice {
            cache: Arc::new(Mutex::new(Cache {
                dev: dev.clone(),
                blocks: BTreeMap::new(),
                capacity,
                clock: 0,
            })),
            base: 0,
            size: dev.size_bytes(),
            sector_size: dev.sector_size(),
            raw: dev,
            name: String::from(name),
        })
    }

    /// A partition view sharing the parent's cache.
    pub fn wrap_partition(
        part: Arc<dyn BlockDevice>,
        name: &str,
        parent: Arc<CachedDevice>,
        start_lba: u64,
    ) -> Arc<CachedDevice> {
        Arc::new(CachedDevice {
            cache: parent.cache.clone(),
            base: parent.base + start_lba * parent.sector_size as u64,
            size: part.size_bytes(),
            sector_size: part.sector_size(),
            raw: part,
            name: String::from(name),
        })
    }

    pub fn size_bytes(&self) -> u64 {
        self.size
    }

    pub fn sector_size(&self) -> usize {
        self.sector_size
    }

    pub fn read_only(&self) -> bool {
        self.raw.read_only()
    }

    pub fn read_bytes(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        if off >= self.size {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(self.size - off) as usize;
        let mut c = self.cache.lock();
        let mut done = 0;
        while done < len {
            let abs = self.base + off + done as u64;
            let blk = abs / BLOCK as u64;
            let boff = (abs % BLOCK as u64) as usize;
            let n = (BLOCK - boff).min(len - done);
            let e = c.get(blk)?;
            buf[done..done + n].copy_from_slice(&e.data[boff..boff + n]);
            done += n;
        }
        Ok(len)
    }

    pub fn write_bytes(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        if self.read_only() {
            return Err(EROFS);
        }
        if off >= self.size {
            return Err(ENOSPC);
        }
        let len = (buf.len() as u64).min(self.size - off) as usize;
        let mut c = self.cache.lock();
        let mut done = 0;
        while done < len {
            let abs = self.base + off + done as u64;
            let blk = abs / BLOCK as u64;
            let boff = (abs % BLOCK as u64) as usize;
            let n = (BLOCK - boff).min(len - done);
            // Whole-block overwrites need not read the old contents.
            if n == BLOCK && !c.blocks.contains_key(&blk) {
                c.make_room(1)?;
                c.clock += 1;
                let used = c.clock;
                c.blocks.insert(
                    blk,
                    Entry {
                        data: buf[done..done + n].to_vec(),
                        dirty: true,
                        used,
                    },
                );
            } else {
                let e = c.get(blk)?;
                e.data[boff..boff + n].copy_from_slice(&buf[done..done + n]);
                e.dirty = true;
            }
            done += n;
        }
        Ok(len)
    }

    /// Write back dirty blocks and flush the device's write cache.
    pub fn sync(&self) -> KResult<()> {
        let mut c = self.cache.lock();
        c.write_back(None)?;
        c.dev.flush()
    }

    /// Drop all cached data (after the device changed underneath).
    pub fn invalidate(&self) {
        let mut c = self.cache.lock();
        let _ = c.write_back(None);
        c.blocks.clear();
    }
}

impl BlockDevice for CachedDevice {
    fn sector_size(&self) -> usize {
        self.sector_size
    }
    fn sector_count(&self) -> u64 {
        self.size / self.sector_size as u64
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> KResult<()> {
        let n = self.read_bytes(lba * self.sector_size as u64, buf)?;
        if n != buf.len() {
            return Err(EIO);
        }
        Ok(())
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> KResult<()> {
        let n = self.write_bytes(lba * self.sector_size as u64, buf)?;
        if n != buf.len() {
            return Err(EIO);
        }
        Ok(())
    }
    fn flush(&self) -> KResult<()> {
        self.sync()
    }
    fn read_only(&self) -> bool {
        self.raw.read_only()
    }
    fn model(&self) -> String {
        self.raw.model()
    }
    fn removed(&self) -> bool {
        self.raw.removed()
    }
}
