//! User address spaces.
//!
//! Each process owns a PML4 whose upper half is shared with the kernel. The
//! lower half is described by a set of [`Area`]s; pages are populated on
//! demand by the page-fault handler. `fork` shares every page copy-on-write
//! (the writable bit is cleared and `COW_BIT` set until the first write).

use crate::errno::*;
use crate::mm::{self, FRAME_SIZE, GlobalFrames};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageTable, PageTableFlags as F, PhysFrame, Size4KiB, Translate,
    mapper::TranslateResult,
};
use x86_64::{PhysAddr, VirtAddr};

pub const PROT_READ: u32 = 1;
pub const PROT_WRITE: u32 = 2;
pub const PROT_EXEC: u32 = 4;

pub const MAP_SHARED: u32 = 0x01;
pub const MAP_PRIVATE: u32 = 0x02;
pub const MAP_FIXED: u32 = 0x10;
pub const MAP_ANONYMOUS: u32 = 0x20;

/// Software bit marking a copy-on-write page.
const COW_BIT: F = F::BIT_9;

/// Flags for intermediate page tables of user mappings. Permissions are
/// enforced at the leaf, so parents are always writable and user-accessible
/// (otherwise a read-only first mapping would block later write access).
const TABLE_FLAGS: F = F::PRESENT.union(F::WRITABLE).union(F::USER_ACCESSIBLE);

pub const USER_BASE: u64 = 0x0000_0000_0040_0000;
pub const STACK_TOP: u64 = 0x0000_7fff_ffff_0000;
pub const STACK_SIZE: u64 = 8 * 1024 * 1024;
pub const MMAP_TOP: u64 = 0x0000_7000_0000_0000;

#[derive(Clone)]
pub enum Backing {
    /// Zero-filled anonymous memory.
    Anon,
    /// Direct mapping of physical memory (device memory such as /dev/fb0).
    Phys { base: u64 },
    /// File pages from the page cache; `offset` is the file offset of the
    /// area's start.
    File {
        inode: Arc<dyn crate::vfs::Inode>,
        offset: u64,
    },
    /// Anonymous shared memory (shared across fork).
    Shm {
        obj: Arc<mm::pagecache::ShmObject>,
        offset: u64,
    },
}

impl Backing {
    /// The backing of the part of an area starting `delta` bytes in.
    fn advanced(&self, delta: u64) -> Backing {
        match self {
            Backing::Anon => Backing::Anon,
            Backing::Phys { base } => Backing::Phys { base: base + delta },
            Backing::File { inode, offset } => Backing::File {
                inode: inode.clone(),
                offset: offset + delta,
            },
            Backing::Shm { obj, offset } => Backing::Shm {
                obj: obj.clone(),
                offset: offset + delta,
            },
        }
    }
}

#[derive(Clone)]
pub struct Area {
    pub start: u64,
    pub end: u64,
    pub prot: u32,
    pub flags: u32,
    pub backing: Backing,
    pub name: &'static str,
}

pub struct AddressSpace {
    pub pml4: u64,
    pub areas: BTreeMap<u64, Area>,
    pub brk_start: u64,
    pub brk: u64,
}

fn mapper_for(pml4: u64) -> OffsetPageTable<'static> {
    let table: &'static mut PageTable = unsafe { &mut *mm::phys_ptr::<PageTable>(pml4) };
    unsafe {
        OffsetPageTable::new(
            table,
            VirtAddr::new(mm::PHYS_MEM_OFFSET.load(core::sync::atomic::Ordering::Relaxed)),
        )
    }
}

fn leaf_flags(prot: u32) -> F {
    let mut f = F::PRESENT | F::USER_ACCESSIBLE;
    if prot & PROT_WRITE != 0 {
        f |= F::WRITABLE;
    }
    if prot & PROT_EXEC == 0 {
        f |= F::NO_EXECUTE;
    }
    f
}

impl AddressSpace {
    /// A new, empty address space sharing the kernel half.
    pub fn new() -> KResult<AddressSpace> {
        let pml4 = mm::with_frames(|f| f.alloc()).ok_or(ENOMEM)?;
        mm::zero_frame(pml4);
        let kernel = mm::KERNEL_PML4.load(core::sync::atomic::Ordering::SeqCst);
        unsafe {
            let src = &*mm::phys_ptr::<PageTable>(kernel);
            let dst = &mut *mm::phys_ptr::<PageTable>(pml4);
            for i in 256..512 {
                dst[i] = src[i].clone();
            }
        }
        Ok(AddressSpace {
            pml4,
            areas: BTreeMap::new(),
            brk_start: 0,
            brk: 0,
        })
    }

    pub fn find_area(&self, addr: u64) -> Option<&Area> {
        self.areas
            .range(..=addr)
            .next_back()
            .map(|(_, a)| a)
            .filter(|a| addr < a.end)
    }

    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.areas.values().any(|a| a.start < end && start < a.end)
    }

    /// Add an area. Fails if it overlaps an existing one.
    pub fn add_area(&mut self, area: Area) -> KResult<()> {
        if area.start >= area.end
            || area.end > mm::USER_END
            || !area.start.is_multiple_of(FRAME_SIZE)
        {
            return Err(EINVAL);
        }
        if self.overlaps(area.start, area.end) {
            return Err(EEXIST);
        }
        self.areas.insert(area.start, area);
        Ok(())
    }

    /// Find a free gap of `len` bytes below `MMAP_TOP`.
    pub fn find_free(&self, len: u64) -> Option<u64> {
        let mut top = MMAP_TOP;
        for a in self.areas.values().rev() {
            if a.start >= MMAP_TOP {
                continue;
            }
            if a.end <= top && top - a.end >= len {
                return Some(top - len);
            }
            top = top.min(a.start);
        }
        (top >= len + USER_BASE).then(|| top - len)
    }

    /// Remove `[start, end)` from all areas, splitting them and unmapping
    /// pages.
    pub fn unmap_range(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let keys: alloc::vec::Vec<u64> = self
            .areas
            .values()
            .filter(|a| a.start < end && start < a.end)
            .map(|a| a.start)
            .collect();
        for k in keys {
            let a = self.areas.remove(&k).unwrap();
            if a.start < start {
                let mut left = a.clone();
                left.end = start;
                self.areas.insert(left.start, left);
            }
            if a.end > end {
                let mut right = a.clone();
                right.start = end;
                right.backing = a.backing.advanced(end - a.start);
                self.areas.insert(right.start, right);
            }
            let is_phys = matches!(a.backing, Backing::Phys { .. });
            self.unmap_pages(a.start.max(start), a.end.min(end), !is_phys);
        }
    }

    fn unmap_pages(&mut self, start: u64, end: u64, release: bool) {
        let mut m = mapper_for(self.pml4);
        let mut addr = start;
        while addr < end {
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
            if let Ok((frame, flush)) = m.unmap(page) {
                flush.flush();
                if release {
                    mm::frame_release(frame.start_address().as_u64());
                }
            }
            addr += FRAME_SIZE;
        }
        crate::arch::x86_64::smp::tlb_shootdown(self.pml4);
    }

    /// Change protection on `[start, end)`.
    pub fn protect(&mut self, start: u64, end: u64, prot: u32) -> KResult<()> {
        if start >= end {
            return Ok(()); // an empty middle piece would replace the right one
        }
        // Split areas at the boundaries, then update.
        let keys: alloc::vec::Vec<u64> = self
            .areas
            .values()
            .filter(|a| a.start < end && start < a.end)
            .map(|a| a.start)
            .collect();
        if keys.is_empty() {
            return Err(ENOMEM);
        }
        for k in keys {
            let a = self.areas.remove(&k).unwrap();
            let mid_start = a.start.max(start);
            let mid_end = a.end.min(end);
            if a.start < mid_start {
                let mut l = a.clone();
                l.end = mid_start;
                self.areas.insert(l.start, l);
            }
            if a.end > mid_end {
                let mut r = a.clone();
                r.start = mid_end;
                r.backing = a.backing.advanced(mid_end - a.start);
                self.areas.insert(r.start, r);
            }
            let mut m = a.clone();
            m.backing = a.backing.advanced(mid_start - a.start);
            m.start = mid_start;
            m.end = mid_end;
            m.prot = prot;
            self.areas.insert(m.start, m);
            // Update present pages (keep COW pages read-only).
            let mut mapper = mapper_for(self.pml4);
            let mut addr = mid_start;
            while addr < mid_end {
                let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
                if let TranslateResult::Mapped { flags, .. } =
                    mapper.translate(page.start_address())
                {
                    let mut nf = leaf_flags(prot);
                    if flags.contains(COW_BIT) {
                        nf.remove(F::WRITABLE);
                        nf |= COW_BIT;
                    }
                    if let Ok(f) = unsafe { mapper.update_flags(page, nf) } {
                        f.flush();
                    }
                }
                addr += FRAME_SIZE;
            }
        }
        crate::arch::x86_64::smp::tlb_shootdown(self.pml4);
        Ok(())
    }

    /// Map one page for `addr` according to its area. Returns false if the
    /// access is not allowed.
    pub fn handle_fault(&mut self, addr: u64, write: bool, exec: bool) -> bool {
        let r = self.handle_fault_inner(addr, write, exec);
        if let Err(why) = r {
            if self.find_area(addr).is_some() {
                let (free, total) = mm::memory_stats();
                crate::serial_println!(
                    "[vm] fault at {:#x} (write {}) not resolved: {} ({} of {} MiB free)",
                    addr,
                    write,
                    why,
                    free >> 20,
                    total >> 20
                );
            }
            return false;
        }
        true
    }

    fn handle_fault_inner(
        &mut self,
        addr: u64,
        write: bool,
        exec: bool,
    ) -> Result<(), &'static str> {
        let Some(area) = self.find_area(addr).cloned() else {
            return Err("no area");
        };
        if write && area.prot & PROT_WRITE == 0 {
            return Err("write to read-only area");
        }
        if exec && area.prot & PROT_EXEC == 0 {
            return Err("exec of non-exec area");
        }
        if area.prot == 0 {
            return Err("PROT_NONE");
        }
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(addr));
        let mut m = mapper_for(self.pml4);
        match m.translate(page.start_address()) {
            TranslateResult::Mapped { frame, flags, .. } => {
                if write && flags.contains(COW_BIT) {
                    let old = frame.start_address().as_u64();
                    let new_flags = (flags | F::WRITABLE) - COW_BIT;
                    if mm::frame_is_shared(old) {
                        let Some(new) = mm::with_frames(|f| f.alloc()) else {
                            return Err("out of memory (copy-on-write)");
                        };
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                mm::phys_ptr::<u8>(old),
                                mm::phys_ptr::<u8>(new),
                                FRAME_SIZE as usize,
                            );
                        }
                        if let Ok((_, f)) = m.unmap(page) {
                            f.flush();
                        }
                        mm::frame_release(old);
                        let frame = PhysFrame::containing_address(PhysAddr::new(new));
                        match unsafe {
                            m.map_to_with_table_flags(
                                page,
                                frame,
                                new_flags,
                                TABLE_FLAGS,
                                &mut GlobalFrames,
                            )
                        } {
                            Ok(f) => f.flush(),
                            Err(_) => return Err("remap after copy failed"),
                        }
                        // Other threads may still cache the old frame.
                        crate::arch::x86_64::smp::tlb_shootdown(self.pml4);
                    } else if let Ok(f) = unsafe { m.update_flags(page, new_flags) } {
                        f.flush();
                    }
                    Ok(())
                } else if !write || flags.contains(F::WRITABLE) {
                    // Present and permitted: spurious (e.g. another thread fixed it).
                    Ok(())
                } else {
                    Err("mapped read-only without copy-on-write")
                }
            }
            _ => {
                let va = page.start_address().as_u64();
                let shared = area.flags & MAP_SHARED != 0;
                let mut flags = leaf_flags(area.prot);
                // (frame, owned by this mapping alone, device memory)
                let (phys, fresh, device) = match &area.backing {
                    Backing::Anon => match mm::with_frames(|f| f.alloc()) {
                        Some(p) => {
                            mm::zero_frame(p);
                            (p, true, false)
                        }
                        None => return Err("out of memory"),
                    },
                    Backing::Phys { base } => (base + (va - area.start), false, true),
                    Backing::File { inode, offset } => {
                        let idx = (offset + (va - area.start)) / FRAME_SIZE;
                        let Ok(p) = mm::pagecache::get_page(inode, idx) else {
                            return Err("file page read failed");
                        };
                        if shared && area.prot & PROT_WRITE != 0 {
                            mm::pagecache::mark_dirty(inode, idx);
                        } else if area.prot & PROT_WRITE != 0 {
                            // Private: the first write copies the page.
                            flags.remove(F::WRITABLE);
                            flags |= COW_BIT;
                        }
                        (p, false, false)
                    }
                    Backing::Shm { obj, offset } => {
                        let Ok(p) = obj.page((offset + (va - area.start)) / FRAME_SIZE) else {
                            return Err("out of memory (shared)");
                        };
                        (p, false, false)
                    }
                };
                if device {
                    flags |= F::NO_CACHE | F::WRITE_THROUGH;
                } else if !fresh {
                    mm::frame_share(phys);
                }
                let frame = PhysFrame::containing_address(PhysAddr::new(phys));
                match unsafe {
                    m.map_to_with_table_flags(page, frame, flags, TABLE_FLAGS, &mut GlobalFrames)
                } {
                    Ok(f) => {
                        f.flush();
                        Ok(())
                    }
                    Err(e) => {
                        if fresh {
                            mm::with_frames(|f| f.free(phys));
                        } else if !device {
                            mm::frame_release(phys);
                        }
                        Err(match e {
                            x86_64::structures::paging::mapper::MapToError::FrameAllocationFailed => {
                                "out of memory (page table)"
                            }
                            x86_64::structures::paging::mapper::MapToError::PageAlreadyMapped(_) => {
                                "already mapped"
                            }
                            _ => "huge page in the way",
                        })
                    }
                }
            }
        }
    }

    /// mremap(2): resize the mapping at `old` (one area, anonymous or
    /// file) to `new_len`, moving it when `may_move` and it cannot grow in
    /// place. Pages move with their contents.
    pub fn mremap(&mut self, old: u64, old_len: u64, new_len: u64, may_move: bool) -> KResult<u64> {
        let area = self.find_area(old).cloned().ok_or(EFAULT)?;
        if old != area.start
            || old + old_len > area.end
            || matches!(area.backing, Backing::Phys { .. })
        {
            return Err(EINVAL);
        }
        if new_len <= old_len {
            self.unmap_range(old + new_len, old + old_len);
            return Ok(old);
        }
        let grow_end = old + new_len;
        let free_after = grow_end <= MMAP_TOP.max(area.end) && !self.overlaps(area.end, grow_end);
        if old_len == area.end - area.start && free_after {
            self.areas.get_mut(&old).unwrap().end = grow_end;
            return Ok(old);
        }
        if !may_move {
            return Err(ENOMEM);
        }
        let new = self.find_free(new_len).ok_or(ENOMEM)?;
        let mut moved = area.clone();
        moved.start = new;
        moved.end = new + new_len;
        // Move the page-table entries of the old range.
        let mut m = mapper_for(self.pml4);
        let mut off = 0;
        while off < old_len {
            let src = Page::<Size4KiB>::containing_address(VirtAddr::new(old + off));
            if let TranslateResult::Mapped { frame, flags, .. } = m.translate(src.start_address())
                && let Ok((_, f)) = m.unmap(src)
            {
                f.flush();
                let dst = Page::<Size4KiB>::containing_address(VirtAddr::new(new + off));
                let frame = PhysFrame::containing_address(frame.start_address());
                unsafe {
                    m.map_to_with_table_flags(dst, frame, flags, TABLE_FLAGS, &mut GlobalFrames)
                        .map_err(|_| ENOMEM)?
                        .flush();
                }
            }
            off += FRAME_SIZE;
        }
        crate::arch::x86_64::smp::tlb_shootdown(self.pml4);
        // What remains of the old area (beyond old_len) is unmapped.
        self.areas.remove(&old);
        if area.end > old + old_len {
            let mut rest = area.clone();
            rest.start = old + old_len;
            rest.backing = area.backing.advanced(old_len);
            self.areas.insert(rest.start, rest);
            self.unmap_range(old + old_len, area.end);
        }
        self.areas.insert(new, moved);
        Ok(new)
    }

    /// Files mapped shared in `[start, end)` (to write back).
    pub fn shared_files(
        &self,
        start: u64,
        end: u64,
    ) -> alloc::vec::Vec<Arc<dyn crate::vfs::Inode>> {
        self.areas
            .values()
            .filter(|a| a.start < end && start < a.end && a.flags & MAP_SHARED != 0)
            .filter_map(|a| match &a.backing {
                Backing::File { inode, .. } => Some(inode.clone()),
                _ => None,
            })
            .collect()
    }

    /// Ensure every page in `[addr, addr+len)` is mapped with the required
    /// access, faulting pages in as needed. Used before kernel accesses to
    /// user memory.
    pub fn ensure(&mut self, addr: u64, len: u64, write: bool) -> KResult<()> {
        if len == 0 {
            return Ok(());
        }
        let end = addr.checked_add(len).ok_or(EFAULT)?;
        if end > mm::USER_END {
            return Err(EFAULT);
        }
        let mut page = addr & !(FRAME_SIZE - 1);
        while page < end {
            let area = self.find_area(page).ok_or(EFAULT)?;
            if area.prot & PROT_READ == 0 && area.prot & PROT_WRITE == 0 {
                return Err(EFAULT);
            }
            if write && area.prot & PROT_WRITE == 0 {
                return Err(EFAULT);
            }
            let m = mapper_for(self.pml4);
            let needs = match m.translate(VirtAddr::new(page)) {
                TranslateResult::Mapped { flags, .. } => write && !flags.contains(F::WRITABLE),
                _ => true,
            };
            if needs && !self.handle_fault(page, write, false) {
                return Err(EFAULT);
            }
            page += FRAME_SIZE;
        }
        Ok(())
    }

    /// Copy `data` into this address space at `addr` (need not be current).
    pub fn write_bytes(&mut self, addr: u64, data: &[u8]) -> KResult<()> {
        self.ensure(addr, data.len() as u64, true)?;
        let m = mapper_for(self.pml4);
        let mut done = 0usize;
        while done < data.len() {
            let va = addr + done as u64;
            let phys = m.translate_addr(VirtAddr::new(va)).ok_or(EFAULT)?.as_u64();
            let n = ((FRAME_SIZE - (va % FRAME_SIZE)) as usize).min(data.len() - done);
            unsafe {
                core::ptr::copy_nonoverlapping(data[done..].as_ptr(), mm::phys_ptr::<u8>(phys), n);
            }
            done += n;
        }
        Ok(())
    }

    /// Duplicate for fork: private pages become copy-on-write in both.
    pub fn fork(&mut self) -> KResult<AddressSpace> {
        let mut child = AddressSpace::new()?;
        child.areas = self.areas.clone();
        child.brk_start = self.brk_start;
        child.brk = self.brk;
        let parent_pml4 = unsafe { &mut *mm::phys_ptr::<PageTable>(self.pml4) };
        let mut cm = mapper_for(child.pml4);
        for (i4, e4) in parent_pml4.iter_mut().enumerate().take(256) {
            if !e4.flags().contains(F::PRESENT) {
                continue;
            }
            let t3 = unsafe { &mut *mm::phys_ptr::<PageTable>(e4.addr().as_u64()) };
            for (i3, e3) in t3.iter_mut().enumerate() {
                if !e3.flags().contains(F::PRESENT) || e3.flags().contains(F::HUGE_PAGE) {
                    continue;
                }
                let t2 = unsafe { &mut *mm::phys_ptr::<PageTable>(e3.addr().as_u64()) };
                for (i2, e2) in t2.iter_mut().enumerate() {
                    if !e2.flags().contains(F::PRESENT) || e2.flags().contains(F::HUGE_PAGE) {
                        continue;
                    }
                    let t1 = unsafe { &mut *mm::phys_ptr::<PageTable>(e2.addr().as_u64()) };
                    for (i1, e1) in t1.iter_mut().enumerate() {
                        let flags = e1.flags();
                        if !flags.contains(F::PRESENT) {
                            continue;
                        }
                        let va = ((i4 as u64) << 39)
                            | ((i3 as u64) << 30)
                            | ((i2 as u64) << 21)
                            | ((i1 as u64) << 12);
                        let phys = e1.addr().as_u64();
                        let area = self.find_area(va);
                        let shared_or_device = area.is_none_or(|a| {
                            a.flags & MAP_SHARED != 0 || matches!(a.backing, Backing::Phys { .. })
                        });
                        let mut cflags = flags;
                        if !shared_or_device && flags.contains(F::WRITABLE) {
                            cflags = (flags - F::WRITABLE) | COW_BIT;
                            e1.set_flags(cflags);
                        }
                        if !matches!(area.map(|a| &a.backing), Some(Backing::Phys { .. })) {
                            mm::frame_share(phys);
                        }
                        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(va));
                        let frame = PhysFrame::containing_address(PhysAddr::new(phys));
                        unsafe {
                            cm.map_to_with_table_flags(
                                page,
                                frame,
                                cflags,
                                TABLE_FLAGS,
                                &mut GlobalFrames,
                            )
                            .map_err(|_| ENOMEM)?
                            .ignore();
                        }
                    }
                }
            }
        }
        // The parent lost write access to shared pages: flush its TLB
        // (on every CPU running one of its threads).
        x86_64::instructions::tlb::flush_all();
        crate::arch::x86_64::smp::tlb_shootdown(self.pml4);
        Ok(child)
    }

    /// Release every user page and page table (the kernel half is shared and
    /// untouched).
    fn teardown(&mut self) {
        let pml4 = unsafe { &mut *mm::phys_ptr::<PageTable>(self.pml4) };
        for e4 in pml4.iter_mut().take(256) {
            if !e4.flags().contains(F::PRESENT) {
                continue;
            }
            let t3p = e4.addr().as_u64();
            let t3 = unsafe { &mut *mm::phys_ptr::<PageTable>(t3p) };
            for e3 in t3.iter_mut() {
                if !e3.flags().contains(F::PRESENT) || e3.flags().contains(F::HUGE_PAGE) {
                    continue;
                }
                let t2p = e3.addr().as_u64();
                let t2 = unsafe { &mut *mm::phys_ptr::<PageTable>(t2p) };
                for e2 in t2.iter_mut() {
                    if !e2.flags().contains(F::PRESENT) || e2.flags().contains(F::HUGE_PAGE) {
                        continue;
                    }
                    let t1p = e2.addr().as_u64();
                    let t1 = unsafe { &mut *mm::phys_ptr::<PageTable>(t1p) };
                    for e1 in t1.iter_mut() {
                        if e1.flags().contains(F::PRESENT) {
                            let va_phys = e1.addr().as_u64();
                            // Device mappings are not owned.
                            if !e1.flags().contains(F::NO_CACHE) {
                                mm::frame_release(va_phys);
                            }
                        }
                    }
                    mm::with_frames(|f| f.free(t1p));
                }
                mm::with_frames(|f| f.free(t2p));
            }
            mm::with_frames(|f| f.free(t3p));
            e4.set_unused();
        }
        self.areas.clear();
    }

    /// Resident user pages (for /proc).
    pub fn resident_pages(&self) -> u64 {
        let m = mapper_for(self.pml4);
        let mut n = 0;
        for a in self.areas.values() {
            let mut addr = a.start;
            while addr < a.end {
                if m.translate_addr(VirtAddr::new(addr)).is_some() {
                    n += 1;
                }
                addr += FRAME_SIZE;
            }
        }
        n
    }

    pub fn virtual_size(&self) -> u64 {
        self.areas.values().map(|a| a.end - a.start).sum()
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // Never tear down the page table we are running on.
        let (cur, _) = x86_64::registers::control::Cr3::read();
        if cur.start_address().as_u64() == self.pml4 {
            unsafe {
                x86_64::registers::control::Cr3::write(
                    PhysFrame::containing_address(PhysAddr::new(
                        mm::KERNEL_PML4.load(core::sync::atomic::Ordering::SeqCst),
                    )),
                    x86_64::registers::control::Cr3Flags::empty(),
                );
            }
        }
        self.teardown();
        mm::with_frames(|f| f.free(self.pml4));
    }
}

/// Shared handle used by processes (threads of one process share it).
pub type Vm = Arc<crate::sync::Mutex<AddressSpace>>;
