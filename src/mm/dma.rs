//! DMA-capable memory.
//!
//! Buffers are physically contiguous runs of frames accessed through the
//! complete physical-memory mapping. x86 DMA is cache-coherent, so no explicit
//! flushes are needed. Buffers are zeroed on allocation and returned to the
//! frame allocator on drop.

use super::{FRAME_SIZE, phys_to_virt, with_frames};

pub struct DmaBuffer {
    phys: u64,
    frames: usize,
    len: usize,
}

unsafe impl Send for DmaBuffer {}
unsafe impl Sync for DmaBuffer {}

impl DmaBuffer {
    /// Allocate `len` bytes of zeroed, physically contiguous memory aligned to
    /// at least a page.
    pub fn new(len: usize) -> Option<DmaBuffer> {
        Self::new_constrained(len, FRAME_SIZE, u64::MAX)
    }

    /// Allocate below 4 GiB (for 32-bit-only DMA engines).
    pub fn new_32bit(len: usize) -> Option<DmaBuffer> {
        Self::new_constrained(len, FRAME_SIZE, 1 << 32)
    }

    pub fn new_constrained(len: usize, align: u64, limit: u64) -> Option<DmaBuffer> {
        let frames = (len.max(1) as u64).div_ceil(FRAME_SIZE) as usize;
        let phys = with_frames(|f| f.alloc_contiguous(frames, align, limit))?;
        unsafe {
            core::ptr::write_bytes(
                phys_to_virt(phys) as *mut u8,
                0,
                frames * FRAME_SIZE as usize,
            )
        };
        Some(DmaBuffer { phys, frames, len })
    }

    pub fn phys(&self) -> u64 {
        self.phys
    }

    pub fn virt(&self) -> u64 {
        phys_to_virt(self.phys)
    }

    pub fn as_ptr<T>(&self) -> *mut T {
        self.virt() as *mut T
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.as_ptr(), self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.as_ptr(), self.len) }
    }

    /// Volatile read of a `T` at byte `offset`.
    pub fn read<T: Copy>(&self, offset: usize) -> T {
        debug_assert!(offset + core::mem::size_of::<T>() <= self.frames * FRAME_SIZE as usize);
        unsafe { core::ptr::read_volatile((self.virt() as usize + offset) as *const T) }
    }

    /// Volatile write of a `T` at byte `offset`.
    pub fn write<T: Copy>(&self, offset: usize, val: T) {
        debug_assert!(offset + core::mem::size_of::<T>() <= self.frames * FRAME_SIZE as usize);
        unsafe { core::ptr::write_volatile((self.virt() as usize + offset) as *mut T, val) }
    }

    pub fn zero(&self) {
        unsafe {
            core::ptr::write_bytes(self.as_ptr::<u8>(), 0, self.frames * FRAME_SIZE as usize)
        };
    }
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        let (phys, frames) = (self.phys, self.frames);
        with_frames(|f| f.free_contiguous(phys, frames));
    }
}
