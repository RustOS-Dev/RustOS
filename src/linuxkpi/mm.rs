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
use core::sync::atomic::Ordering;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::structures::paging::PageTableFlags;

/// `struct page` array for every frame (Linux `vmemmap_base`).
pub const VMEMMAP_BASE: u64 = 0xffff_f000_0000_0000;
/// Linux `vmalloc` window.
pub const VMALLOC_START: u64 = 0xffff_f800_0000_0000;
pub const VMALLOC_END: u64 = 0xffff_fe00_0000_0000;

static VMALLOC_VA: Mutex<mm::VaAlloc> = Mutex::new(mm::VaAlloc::new(VMALLOC_START, VMALLOC_END));
/// Live vmalloc areas: start → size.
static VMALLOC_AREAS: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());
/// Live ioremap mappings: virtual address → size.
static IOREMAPS: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());

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

/// Usable memory in pages: total, and free right now.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_mem_pages(free: *mut u64) -> u64 {
    let (f, total) = mm::memory_stats();
    unsafe { free.write(f / mm::FRAME_SIZE) };
    total / mm::FRAME_SIZE
}

/// A Linux driver asked to power the machine off (`reboot` = 0) or to
/// restart it (thermal shutdown, emergency_restart).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_power(reboot: i32) -> ! {
    crate::drivers::shutdown();
    if reboot != 0 {
        crate::acpi::reboot()
    }
    crate::acpi::shutdown()
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
    let Some(virt) = without_interrupts(|| VMALLOC_VA.lock().alloc(size + FRAME_SIZE)) else {
        return core::ptr::null_mut();
    };
    if mm::map_kernel_pages(virt, size, rw()).is_err() {
        mm::unmap_kernel_pages(virt, size);
        without_interrupts(|| VMALLOC_VA.lock().free(virt, size + FRAME_SIZE));
        return core::ptr::null_mut();
    }
    without_interrupts(|| VMALLOC_AREAS.lock().insert(virt, size));
    virt as *mut u8
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_vfree(addr: *const u8) {
    if addr.is_null() {
        return;
    }
    let virt = addr as u64;
    if let Some(size) = without_interrupts(|| VMALLOC_AREAS.lock().remove(&virt)) {
        mm::unmap_kernel_pages(virt, size);
        without_interrupts(|| VMALLOC_VA.lock().free(virt, size + FRAME_SIZE));
    }
}

/// Size of the vmalloc area starting at `addr` (0 if it is not one).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_vmalloc_size(addr: *const u8) -> u64 {
    without_interrupts(|| VMALLOC_AREAS.lock().get(&(addr as u64)).copied()).unwrap_or(0)
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_is_vmalloc(addr: *const u8) -> i32 {
    let a = addr as u64;
    (VMALLOC_START..VMALLOC_END).contains(&a) as i32
}

/// Map device memory: `cache` 0 uncached, 1 write-combining, 2 write-back.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_ioremap(phys: u64, size: u64, cache: i32) -> *mut u8 {
    let cache = match cache {
        1 => mm::Cache::WriteCombining,
        2 => mm::Cache::WriteBack,
        _ => mm::Cache::Uncached,
    };
    match mm::map_mmio_cache(phys, size as usize, cache) {
        Some(v) => {
            without_interrupts(|| IOREMAPS.lock().insert(v, size));
            v as *mut u8
        }
        None => core::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_iounmap(addr: *mut u8) {
    let v = addr as u64;
    if let Some(size) = without_interrupts(|| IOREMAPS.lock().remove(&v)) {
        mm::unmap_mmio(v, size as usize);
    }
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

/// Physical address of kernel virtual address `virt` (0 if unmapped).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_virt_to_phys(virt: u64) -> u64 {
    mm::virt_to_phys(virt).unwrap_or(0)
}

/// Map `count` frames (physical addresses) at consecutive addresses.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_vmap(phys: *const u64, count: u64) -> *mut u8 {
    if phys.is_null() || count == 0 {
        return core::ptr::null_mut();
    }
    let frames = unsafe { core::slice::from_raw_parts(phys, count as usize) };
    mm::map_frames(frames).map_or(core::ptr::null_mut(), |v| v as *mut u8)
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_vunmap(virt: *const u8, count: u64) {
    if !virt.is_null() {
        mm::unmap_frames(virt as u64, count as usize);
    }
}

/// The firmware framebuffer (physical address and length), or 0.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_fb_phys(len: *mut u64) -> u64 {
    match crate::drivers::framebuffer::framebuffer_phys() {
        Some((p, l)) => {
            unsafe { *len = l as u64 };
            p
        }
        None => 0,
    }
}

/// The firmware framebuffer's geometry: size in pixels, line length in
/// bytes, bytes per pixel, blue in the low byte. 0 if there is none.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_fb_geometry(
    width: *mut u32,
    height: *mut u32,
    pitch: *mut u32,
    bpp: *mut u32,
    bgr: *mut i32,
) -> i32 {
    crate::drivers::framebuffer::framebuffer_phys()
        .and(crate::drivers::framebuffer::geometry())
        .map_or(0, |(w, h, stride, b, is_bgr)| {
            unsafe {
                *width = w as u32;
                *height = h as u32;
                *pitch = (stride * b) as u32;
                *bpp = b as u32;
                *bgr = is_bgr as i32;
            }
            1
        })
}

/// A Linux display driver took over the firmware framebuffer.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_fb_release() {
    crate::drivers::framebuffer::release();
}

/// A Linux DRM client gives the console a buffer to draw on.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_console_attach(
    ptr: *mut u8,
    len: u64,
    width: u32,
    height: u32,
    pitch: u32,
) {
    unsafe {
        crate::drivers::framebuffer::attach(
            ptr,
            len as usize,
            width as usize,
            height as usize,
            pitch as usize,
        )
    };
}

/// Pixel rows the console drew since the last call; 0 if none.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_console_damage(lo: *mut u32, hi: *mut u32) -> i32 {
    match crate::drivers::framebuffer::take_damage() {
        Some((l, h)) => {
            unsafe {
                *lo = l as u32;
                *hi = h as u32;
            }
            1
        }
        None => 0,
    }
}
