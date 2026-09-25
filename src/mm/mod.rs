//! Memory management: physical frames, kernel page tables, MMIO mappings,
//! DMA buffers and the kernel heap.
//!
//! # Kernel virtual layout (upper half only; the lower half belongs to user
//! processes)
//!
//! ```text
//! 0xffff_8000_0000_0000  bootloader dynamic range: kernel image, boot stack,
//!                        boot info, framebuffer, complete physical-memory map
//! 0xffff_c000_0000_0000  kernel heap
//! 0xffff_d000_0000_0000  MMIO mappings (uncached)
//! 0xffff_e000_0000_0000  kernel thread stacks (with guard pages)
//! ```

pub mod dma;
pub mod frame;

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator as X86FrameAllocator, Mapper, OffsetPageTable, Page, PageTable,
        PageTableFlags, PhysFrame, Size4KiB, Translate, mapper::MapToError,
    },
};

pub use frame::FRAME_SIZE;

pub const DYNAMIC_RANGE_START: u64 = 0xffff_8000_0000_0000;
pub const DYNAMIC_RANGE_END: u64 = 0xffff_bfff_ffff_f000;
pub const HEAP_START: u64 = 0xffff_c000_0000_0000;
pub const MMIO_START: u64 = 0xffff_d000_0000_0000;
pub const MMIO_END: u64 = 0xffff_dfff_ffff_f000;
pub const KSTACK_START: u64 = 0xffff_e000_0000_0000;
pub const KSTACK_END: u64 = 0xffff_efff_ffff_f000;
/// First address that is not user space.
pub const USER_END: u64 = 0x0000_8000_0000_0000;

pub static PHYS_MEM_OFFSET: AtomicU64 = AtomicU64::new(0);
pub static KERNEL_MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);
pub static FRAMES: Mutex<Option<frame::FrameAllocator>> = Mutex::new(None);
/// Physical address of the kernel PML4 (the template every address space copies).
pub static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);

static MMIO_NEXT: AtomicU64 = AtomicU64::new(MMIO_START);
static KSTACK_NEXT: AtomicU64 = AtomicU64::new(KSTACK_START);
static BOOT_REGIONS: AtomicU64 = AtomicU64::new(0);

/// Run `f` with the global frame allocator, interrupts disabled.
pub fn with_frames<R>(f: impl FnOnce(&mut frame::FrameAllocator) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut g = FRAMES.lock();
        f(g.as_mut().expect("frame allocator not initialised"))
    })
}

/// Run `f` with the kernel page-table mapper, interrupts disabled.
pub fn with_mapper<R>(f: impl FnOnce(&mut OffsetPageTable<'static>) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut g = KERNEL_MAPPER.lock();
        f(g.as_mut().expect("kernel mapper not initialised"))
    })
}

/// Frame source for `x86_64` mapper calls, backed by the global allocator.
pub struct GlobalFrames;

unsafe impl X86FrameAllocator<Size4KiB> for GlobalFrames {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        // Called with KERNEL_MAPPER held; FRAMES is a different lock.
        let phys = with_frames(|f| f.alloc())?;
        zero_frame(phys);
        Some(PhysFrame::containing_address(PhysAddr::new(phys)))
    }
}

/// Initialise the frame allocator, the kernel mapper and the heap.
///
/// # Safety
/// Must be called exactly once, early in boot, with the bootloader's info.
pub unsafe fn init(boot_info: &'static bootloader_api::BootInfo) {
    let offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("physical memory mapping not configured");
    PHYS_MEM_OFFSET.store(offset, Ordering::SeqCst);
    BOOT_REGIONS.store(
        &boot_info.memory_regions as *const _ as u64,
        Ordering::SeqCst,
    );

    let fa = unsafe { frame::FrameAllocator::new(&boot_info.memory_regions, offset) };
    *FRAMES.lock() = Some(fa);

    let (pml4_frame, _) = Cr3::read();
    KERNEL_PML4.store(pml4_frame.start_address().as_u64(), Ordering::SeqCst);
    let pml4: &'static mut PageTable =
        unsafe { &mut *((offset + pml4_frame.start_address().as_u64()) as *mut PageTable) };

    // Pre-populate every upper-half PML4 slot so address spaces created later
    // (which copy the upper half) see kernel mappings added after they were
    // created.
    for i in 256..512 {
        if pml4[i].is_unused() {
            let phys = with_frames(|f| f.alloc()).expect("out of memory for PML4 slots");
            zero_frame(phys);
            pml4[i].set_addr(
                PhysAddr::new(phys),
                PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            );
        }
    }

    let mapper = unsafe { OffsetPageTable::new(pml4, VirtAddr::new(offset)) };
    *KERNEL_MAPPER.lock() = Some(mapper);

    crate::allocator::init_heap().expect("heap initialisation failed");
}

/// Bootloader memory map (valid after [`init`]).
pub fn boot_regions() -> &'static bootloader_api::info::MemoryRegions {
    let p = BOOT_REGIONS.load(Ordering::SeqCst);
    assert!(p != 0, "mm not initialised");
    unsafe { &*(p as *const bootloader_api::info::MemoryRegions) }
}

/// Virtual address of physical memory through the complete physical mapping.
#[inline]
pub fn phys_to_virt(phys: u64) -> u64 {
    PHYS_MEM_OFFSET.load(Ordering::Relaxed) + phys
}

/// Pointer to physical memory through the complete physical mapping.
#[inline]
pub fn phys_ptr<T>(phys: u64) -> *mut T {
    phys_to_virt(phys) as *mut T
}

/// Translate a kernel virtual address to a physical address.
pub fn virt_to_phys(virt: u64) -> Option<u64> {
    with_mapper(|m| m.translate_addr(VirtAddr::new(virt)).map(|p| p.as_u64()))
}

pub fn zero_frame(phys: u64) {
    unsafe { core::ptr::write_bytes(phys_ptr::<u8>(phys), 0, FRAME_SIZE as usize) };
}

/// Map `size` bytes of device memory at `phys` into the uncached MMIO window
/// and return the virtual address corresponding to `phys` (offset preserved).
pub fn map_mmio(phys: u64, size: usize) -> u64 {
    let page_off = phys & (FRAME_SIZE - 1);
    let base = phys - page_off;
    let pages = (size as u64 + page_off).div_ceil(FRAME_SIZE);
    let virt = MMIO_NEXT.fetch_add((pages + 1) * FRAME_SIZE, Ordering::SeqCst);
    assert!(
        virt + pages * FRAME_SIZE < MMIO_END,
        "MMIO window exhausted"
    );
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_CACHE
        | PageTableFlags::WRITE_THROUGH
        | PageTableFlags::NO_EXECUTE;
    with_mapper(|m| {
        for i in 0..pages {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt + i * FRAME_SIZE));
            let frame = PhysFrame::containing_address(PhysAddr::new(base + i * FRAME_SIZE));
            unsafe {
                m.map_to(page, frame, flags, &mut GlobalFrames)
                    .expect("map_mmio")
                    .flush();
            }
        }
    });
    virt + page_off
}

/// Map fresh zeroed frames at `virt..virt+size` in the kernel address space.
pub fn map_kernel_pages(
    virt: u64,
    size: u64,
    flags: PageTableFlags,
) -> Result<(), MapToError<Size4KiB>> {
    let pages = size.div_ceil(FRAME_SIZE);
    with_mapper(|m| {
        for i in 0..pages {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt + i * FRAME_SIZE));
            let phys = with_frames(|f| f.alloc()).ok_or(MapToError::FrameAllocationFailed)?;
            zero_frame(phys);
            let frame = PhysFrame::containing_address(PhysAddr::new(phys));
            unsafe { m.map_to(page, frame, flags, &mut GlobalFrames)?.flush() };
        }
        Ok(())
    })
}

/// Unmap kernel pages and free their frames.
pub fn unmap_kernel_pages(virt: u64, size: u64) {
    let pages = size.div_ceil(FRAME_SIZE);
    with_mapper(|m| {
        for i in 0..pages {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt + i * FRAME_SIZE));
            if let Ok((frame, flush)) = m.unmap(page) {
                flush.flush();
                with_frames(|f| f.free(frame.start_address().as_u64()));
            }
        }
    });
}

/// A kernel stack with an unmapped guard page below it.
pub struct KernelStack {
    pub base: u64,
    pub size: u64,
}

impl KernelStack {
    pub fn new(size: u64) -> Option<KernelStack> {
        let size = size.next_multiple_of(FRAME_SIZE);
        let guard = FRAME_SIZE;
        let region = KSTACK_NEXT.fetch_add(size + guard, Ordering::SeqCst);
        if region + size + guard >= KSTACK_END {
            return None;
        }
        let base = region + guard;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        map_kernel_pages(base, size, flags).ok()?;
        Some(KernelStack { base, size })
    }

    pub fn top(&self) -> u64 {
        self.base + self.size
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        unmap_kernel_pages(self.base, self.size);
    }
}

/// (free, total) usable memory in bytes.
pub fn memory_stats() -> (u64, u64) {
    with_frames(|f| {
        (
            f.free_frames() as u64 * FRAME_SIZE,
            f.total_frames() as u64 * FRAME_SIZE,
        )
    })
}

/// Make sure `phys..phys+len` is reachable through the physical-memory map.
///
/// The bootloader only maps physical memory up to the end of the memory map,
/// so firmware tables or device memory above that need extra pages. Frames
/// outside usable RAM are mapped uncached.
pub fn ensure_phys_mapped(phys: u64, len: usize) {
    let start = phys & !(FRAME_SIZE - 1);
    let end = (phys + len.max(1) as u64).next_multiple_of(FRAME_SIZE);
    let offset = PHYS_MEM_OFFSET.load(Ordering::Relaxed);
    with_mapper(|m| {
        let mut addr = start;
        while addr < end {
            let virt = VirtAddr::new(offset + addr);
            if m.translate_addr(virt).is_none() {
                let page = Page::<Size4KiB>::containing_address(virt);
                let frame = PhysFrame::containing_address(PhysAddr::new(addr));
                let flags = PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::NO_CACHE
                    | PageTableFlags::NO_EXECUTE;
                if let Ok(f) = unsafe { m.map_to(page, frame, flags, &mut GlobalFrames) } {
                    f.flush();
                }
            }
            addr += FRAME_SIZE;
        }
    });
}

/// Legacy helper for drivers not yet converted to [`dma::DmaBuffer`]:
/// allocate zeroed, physically contiguous memory that is never freed.
pub fn dma_alloc(size: usize, _align: usize) -> (*mut u8, u64) {
    let frames = (size.max(1) as u64).div_ceil(FRAME_SIZE) as usize;
    let phys = with_frames(|f| f.alloc_contiguous(frames, FRAME_SIZE, u64::MAX))
        .expect("dma_alloc: out of memory");
    let virt = phys_to_virt(phys);
    unsafe { core::ptr::write_bytes(virt as *mut u8, 0, frames * FRAME_SIZE as usize) };
    (virt as *mut u8, phys)
}

// ---------------------------------------------------------------------------
// Shared-frame reference counts (copy-on-write)
// ---------------------------------------------------------------------------

/// Extra references to frames shared between address spaces. A frame not in
/// the map has exactly one owner.
static FRAME_REFS: Mutex<alloc::collections::BTreeMap<u64, u32>> =
    Mutex::new(alloc::collections::BTreeMap::new());

/// Record one more owner of `phys`.
pub fn frame_share(phys: u64) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        *FRAME_REFS.lock().entry(phys).or_insert(0) += 1;
    });
}

/// Drop one owner of `phys`, freeing the frame when it was the last.
pub fn frame_release(phys: u64) {
    let free = x86_64::instructions::interrupts::without_interrupts(|| {
        let mut refs = FRAME_REFS.lock();
        match refs.get_mut(&phys) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => {
                refs.remove(&phys);
                false
            }
            None => true,
        }
    });
    if free {
        with_frames(|f| f.free(phys));
    }
}

/// Whether `phys` has more than one owner.
pub fn frame_is_shared(phys: u64) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| FRAME_REFS.lock().contains_key(&phys))
}
