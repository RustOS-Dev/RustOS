//! Physical frame allocator.
//!
//! A bitmap over every 4 KiB frame up to the highest usable physical address.
//! Supports single frames, physically contiguous runs (with alignment and an
//! upper address limit, for DMA engines that only take 32-bit addresses), and
//! freeing. Frames below 1 MiB are withheld from general allocation so the SMP
//! trampoline and legacy firmware structures stay available.

use bootloader_api::info::{MemoryRegionKind, MemoryRegions};

pub const FRAME_SIZE: u64 = 4096;
const LOW_MEMORY_LIMIT: u64 = 0x10_0000;

pub struct FrameAllocator {
    bitmap: &'static mut [u64],
    frames: usize,
    free: usize,
    total_usable: usize,
    /// Search hint: index of a word that probably has a free bit.
    hint: usize,
    /// Low (<1 MiB) frames handed out by `alloc_low`.
    low_claimed: [u64; 4],
}

impl FrameAllocator {
    /// Build the allocator from the bootloader memory map.
    ///
    /// # Safety
    /// `phys_offset` must be the virtual base of the complete physical memory
    /// mapping and every `Usable` region must really be unused.
    pub unsafe fn new(regions: &MemoryRegions, phys_offset: u64) -> Self {
        let max_addr = regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .map(|r| r.end)
            .max()
            .unwrap_or(0);
        let frames = (max_addr / FRAME_SIZE) as usize;
        let words = frames.div_ceil(64);
        let bitmap_bytes = (words * 8) as u64;
        let bitmap_frames = bitmap_bytes.div_ceil(FRAME_SIZE);

        // Place the bitmap in the first usable region above 1 MiB that fits.
        let bitmap_phys = regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .map(|r| (r.start.max(LOW_MEMORY_LIMIT), r.end))
            .find(|(s, e)| {
                let s = s.next_multiple_of(FRAME_SIZE);
                *e > s && e - s >= bitmap_frames * FRAME_SIZE
            })
            .map(|(s, _)| s.next_multiple_of(FRAME_SIZE))
            .expect("no room for the frame bitmap");

        let bitmap = unsafe {
            core::slice::from_raw_parts_mut((phys_offset + bitmap_phys) as *mut u64, words)
        };
        // Everything starts allocated; usable ranges are then released.
        bitmap.fill(u64::MAX);

        let mut fa = FrameAllocator {
            bitmap,
            frames,
            free: 0,
            total_usable: 0,
            hint: 0,
            low_claimed: [0; 4],
        };
        for r in regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
        {
            let start = r.start.next_multiple_of(FRAME_SIZE).max(LOW_MEMORY_LIMIT);
            let end = r.end & !(FRAME_SIZE - 1);
            let mut addr = start;
            while addr < end {
                fa.clear((addr / FRAME_SIZE) as usize);
                fa.free += 1;
                fa.total_usable += 1;
                addr += FRAME_SIZE;
            }
        }
        for i in 0..bitmap_frames {
            let idx = (bitmap_phys / FRAME_SIZE + i) as usize;
            if !fa.is_set(idx) {
                fa.set(idx);
                fa.free -= 1;
            }
        }
        fa
    }

    fn is_set(&self, idx: usize) -> bool {
        self.bitmap[idx / 64] & (1 << (idx % 64)) != 0
    }
    fn set(&mut self, idx: usize) {
        self.bitmap[idx / 64] |= 1 << (idx % 64);
    }
    fn clear(&mut self, idx: usize) {
        self.bitmap[idx / 64] &= !(1 << (idx % 64));
    }

    /// Allocate one frame, returning its physical address.
    pub fn alloc(&mut self) -> Option<u64> {
        let words = self.bitmap.len();
        for n in 0..words {
            let w = (self.hint + n) % words;
            let word = self.bitmap[w];
            if word != u64::MAX {
                let bit = (!word).trailing_zeros() as usize;
                let idx = w * 64 + bit;
                if idx >= self.frames {
                    continue;
                }
                self.set(idx);
                self.free -= 1;
                self.hint = w;
                return Some(idx as u64 * FRAME_SIZE);
            }
        }
        None
    }

    /// Allocate `count` physically contiguous frames whose start is aligned to
    /// `align` bytes and whose end lies below `limit` (exclusive).
    pub fn alloc_contiguous(&mut self, count: usize, align: u64, limit: u64) -> Option<u64> {
        if count == 0 {
            return None;
        }
        if count == 1 && align <= FRAME_SIZE && limit == u64::MAX {
            return self.alloc();
        }
        let step = (align.max(FRAME_SIZE) / FRAME_SIZE) as usize;
        let max_frame = ((limit / FRAME_SIZE) as usize).min(self.frames);
        let mut start = (LOW_MEMORY_LIMIT / FRAME_SIZE) as usize;
        start = start.next_multiple_of(step);
        while start + count <= max_frame {
            match (start..start + count).find(|&i| self.is_set(i)) {
                None => {
                    for i in start..start + count {
                        self.set(i);
                    }
                    self.free -= count;
                    return Some(start as u64 * FRAME_SIZE);
                }
                Some(used) => start = (used + 1).next_multiple_of(step),
            }
        }
        None
    }

    /// Allocate one frame below 1 MiB (for real-mode trampolines).
    pub fn alloc_low(&mut self, regions: &MemoryRegions) -> Option<u64> {
        for r in regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
        {
            let mut addr = r.start.next_multiple_of(FRAME_SIZE).max(0x1000);
            while addr + FRAME_SIZE <= r.end.min(LOW_MEMORY_LIMIT) {
                let idx = (addr / FRAME_SIZE) as usize;
                if self.low_claimed[idx / 64] & (1 << (idx % 64)) == 0 {
                    self.low_claimed[idx / 64] |= 1 << (idx % 64);
                    return Some(addr);
                }
                addr += FRAME_SIZE;
            }
        }
        None
    }

    /// Return a frame to the allocator.
    pub fn free(&mut self, phys: u64) {
        let idx = (phys / FRAME_SIZE) as usize;
        if idx < self.frames && self.is_set(idx) {
            self.clear(idx);
            self.free += 1;
        }
    }

    /// Return `count` contiguous frames starting at `phys`.
    pub fn free_contiguous(&mut self, phys: u64, count: usize) {
        for i in 0..count as u64 {
            self.free(phys + i * FRAME_SIZE);
        }
    }

    pub fn free_frames(&self) -> usize {
        self.free
    }

    pub fn total_frames(&self) -> usize {
        self.total_usable
    }

    /// One past the highest frame number the allocator manages.
    pub fn frame_limit(&self) -> usize {
        self.frames
    }
}
