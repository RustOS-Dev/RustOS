//! Memory-management syscalls.

use super::SysResult;
use crate::errno::*;
use crate::mm::FRAME_SIZE;
use crate::process::{
    self,
    vm::{self, Area, Backing, MAP_ANONYMOUS, MAP_FIXED, MAP_SHARED, PROT_READ, PROT_WRITE},
};

pub fn mmap(addr: u64, len: u64, prot: u32, flags: u32, fd: i32, off: u64) -> SysResult {
    if len == 0 || !off.is_multiple_of(FRAME_SIZE) {
        return Err(EINVAL);
    }
    let p = process::current().ok_or(ESRCH)?;
    let len = len.next_multiple_of(FRAME_SIZE);
    let file = if flags & MAP_ANONYMOUS == 0 {
        Some(p.files.lock().get(fd)?)
    } else {
        None
    };
    let vm = p.vm().ok_or(EFAULT)?;
    let mut space = vm.lock();
    let start = if flags & MAP_FIXED != 0 {
        if !addr.is_multiple_of(FRAME_SIZE) || addr + len > vm::MMAP_TOP {
            return Err(EINVAL);
        }
        space.unmap_range(addr, addr + len);
        addr
    } else if addr != 0
        && addr.is_multiple_of(FRAME_SIZE)
        && addr + len <= vm::MMAP_TOP
        && space.find_area(addr).is_none()
        && space
            .areas
            .values()
            .all(|a| a.end <= addr || a.start >= addr + len)
    {
        addr
    } else {
        space.find_free(len).ok_or(ENOMEM)?
    };

    // Device memory (e.g. the framebuffer) is mapped directly.
    if let Some(f) = &file
        && let Some(s) = f.stream()
        && let Some((phys, size)) = crate::drivers::mmap_phys(s.as_ref())
    {
        if off + len > size.next_multiple_of(FRAME_SIZE) {
            return Err(EINVAL);
        }
        space.add_area(Area {
            start,
            end: start + len,
            prot,
            flags: flags | MAP_SHARED,
            backing: Backing::Phys { base: phys + off },
            name: "[device]",
        })?;
        return Ok(start as i64);
    }

    space.add_area(Area {
        start,
        end: start + len,
        prot,
        flags,
        backing: Backing::Anon,
        name: if file.is_some() { "[file]" } else { "[anon]" },
    })?;
    // File mappings are populated eagerly (private copy).
    if let Some(f) = file {
        let mut buf = alloc::vec![0u8; len as usize];
        let n = f.pread(off, &mut buf)?;
        if n > 0 {
            if prot & PROT_WRITE == 0 {
                space.protect(start, start + len, prot | PROT_WRITE | PROT_READ)?;
                space.write_bytes(start, &buf[..n])?;
                space.protect(start, start + len, prot)?;
            } else {
                space.write_bytes(start, &buf[..n])?;
            }
        }
    }
    Ok(start as i64)
}

pub fn munmap(addr: u64, len: u64) -> SysResult {
    if !addr.is_multiple_of(FRAME_SIZE) || len == 0 {
        return Err(EINVAL);
    }
    let p = process::current().ok_or(ESRCH)?;
    let vm = p.vm().ok_or(EFAULT)?;
    vm.lock()
        .unmap_range(addr, addr + len.next_multiple_of(FRAME_SIZE));
    Ok(0)
}

pub fn mprotect(addr: u64, len: u64, prot: u32) -> SysResult {
    if !addr.is_multiple_of(FRAME_SIZE) {
        return Err(EINVAL);
    }
    let p = process::current().ok_or(ESRCH)?;
    let vm = p.vm().ok_or(EFAULT)?;
    vm.lock()
        .protect(addr, addr + len.next_multiple_of(FRAME_SIZE), prot)?;
    Ok(0)
}

pub fn brk(new: u64) -> SysResult {
    let p = process::current().ok_or(ESRCH)?;
    let vm = p.vm().ok_or(EFAULT)?;
    let mut space = vm.lock();
    if new == 0 || new < space.brk_start {
        return Ok(space.brk as i64);
    }
    let old_end = space
        .brk
        .next_multiple_of(FRAME_SIZE)
        .max(space.brk_start + FRAME_SIZE);
    let new_end = new
        .next_multiple_of(FRAME_SIZE)
        .max(space.brk_start + FRAME_SIZE);
    if new_end > old_end {
        // Grow the heap area (fail if something is in the way).
        let blocked = space
            .areas
            .values()
            .any(|a| a.start >= old_end && a.start < new_end);
        if blocked || new_end > vm::MMAP_TOP {
            return Ok(space.brk as i64);
        }
        let start = space.brk_start;
        if let Some(a) = space.areas.get_mut(&start) {
            a.end = new_end;
        }
    } else if new_end < old_end {
        space.unmap_range(new_end, old_end);
        let start = space.brk_start;
        if !space.areas.contains_key(&start) {
            space.add_area(Area {
                start,
                end: new_end,
                prot: PROT_READ | PROT_WRITE,
                flags: vm::MAP_PRIVATE,
                backing: Backing::Anon,
                name: "[heap]",
            })?;
        }
    }
    space.brk = new;
    Ok(new as i64)
}
