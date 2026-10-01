//! Memory services for the LinuxKPI C glue (src/linuxkpi/c/mm.c).
//!
//! Linux code computes physical addresses with `__pa()`/`virt_to_page()`,
//! which only work for addresses in the direct map of physical memory. So
//! everything Linux allocates comes from physical frames addressed through
//! that map (`mm::phys_to_virt`), never from the RustOS heap. `vmalloc` and
//! the `struct page` array (`vmemmap`) get their own virtual windows above
//! the kernel stacks.

use crate::mm::{self, FRAME_SIZE};
use crate::sync::Mutex;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::structures::paging::PageTableFlags;

/// `struct page` array for every frame (Linux `vmemmap_base`).
pub const VMEMMAP_BASE: u64 = 0xffff_f000_0000_0000;
/// Linux `vmalloc` window.
pub const VMALLOC_START: u64 = 0xffff_f800_0000_0000;
pub const VMALLOC_END: u64 = 0xffff_fe00_0000_0000;

static VMALLOC_NEXT: AtomicU64 = AtomicU64::new(VMALLOC_START);
/// Live vmalloc areas: start → size.
static VMALLOC_AREAS: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());

fn rw() -> PageTableFlags {
    PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_page_offset() -> u64 {
    mm::PHYS_MEM_OFFSET.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_max_pfn() -> u64 {
    mm::with_frames(|f| f.frame_limit() as u64)
}

/// Map zeroed frames at a fixed kernel address (vmemmap). 0 on success.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_map_zeroed(virt: u64, size: u64) -> u64 {
    match mm::map_kernel_pages(virt, size, rw()) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

/// `count` contiguous frames aligned to `align` bytes, optionally below
/// 4 GiB. Returns the physical address, or 0.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_alloc_frames(count: u64, align: u64, below_4g: i32) -> u64 {
    let limit = if below_4g != 0 { 1 << 32 } else { u64::MAX };
    mm::with_frames(|f| f.alloc_contiguous(count as usize, align.max(FRAME_SIZE), limit))
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_free_frames(phys: u64, count: u64) {
    if count == 1 {
        mm::with_frames(|f| f.free(phys));
    } else {
        mm::with_frames(|f| f.free_contiguous(phys, count as usize));
    }
}

/// Virtually contiguous, physically scattered zeroed memory.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_vmalloc(size: u64) -> *mut u8 {
    let size = size.max(1).next_multiple_of(FRAME_SIZE);
    // One unmapped guard page after each area.
    let virt = VMALLOC_NEXT.fetch_add(size + FRAME_SIZE, Ordering::SeqCst);
    if virt + size > VMALLOC_END || mm::map_kernel_pages(virt, size, rw()).is_err() {
        return core::ptr::null_mut();
    }
    VMALLOC_AREAS.lock().insert(virt, size);
    virt as *mut u8
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_vfree(addr: *const u8) {
    if addr.is_null() {
        return;
    }
    let virt = addr as u64;
    if let Some(size) = VMALLOC_AREAS.lock().remove(&virt) {
        mm::unmap_kernel_pages(virt, size);
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_is_vmalloc(addr: *const u8) -> i32 {
    let a = addr as u64;
    (VMALLOC_START..VMALLOC_END).contains(&a) as i32
}

/// Map device memory (uncached; write-combining is not supported yet and
/// falls back to uncached).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_ioremap(phys: u64, size: u64, _wc: i32) -> *mut u8 {
    mm::map_mmio(phys, size as usize) as *mut u8
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_iounmap(_addr: *mut u8) {
    // map_mmio has no unmap yet: the mapping stays (M27.2 gap).
}

/// Copy `n` bytes from user address `from`. Returns the bytes not copied
/// (all of them if the range is not mapped readable in the current process).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_copy_from_user(to: *mut u8, from: u64, n: usize) -> usize {
    let dst = unsafe { core::slice::from_raw_parts_mut(to, n) };
    match crate::process::uaccess::copy_from_user(dst, from) {
        Ok(()) => 0,
        Err(_) => n,
    }
}

/// Copy `n` bytes to user address `to`. Returns the bytes not copied.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_copy_to_user(to: u64, from: *const u8, n: usize) -> usize {
    let src = unsafe { core::slice::from_raw_parts(from, n) };
    match crate::process::uaccess::copy_to_user(to, src) {
        Ok(()) => 0,
        Err(_) => n,
    }
}
