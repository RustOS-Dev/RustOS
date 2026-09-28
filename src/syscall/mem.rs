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

    let backing = match &file {
        Some(f) => match &f.object {
            crate::vfs::FileObject::Inode(i) => Backing::File {
                inode: i.clone(),
                offset: off,
            },
            _ => return Err(ENODEV),
        },
        None if flags & MAP_SHARED != 0 => Backing::Shm {
            obj: crate::mm::pagecache::ShmObject::new(),
            offset: 0,
        },
        None => Backing::Anon,
    };
    if let Some(f) = &file
        && (!f.readable() || (flags & MAP_SHARED != 0 && prot & PROT_WRITE != 0 && !f.writable()))
    {
        return Err(EACCES);
    }
    space.add_area(Area {
        start,
        end: start + len,
        prot,
        flags,
        backing,
        name: if file.is_some() { "[file]" } else { "[anon]" },
    })?;
    Ok(start as i64)
}

pub fn munmap(addr: u64, len: u64) -> SysResult {
    if !addr.is_multiple_of(FRAME_SIZE) || len == 0 {
        return Err(EINVAL);
    }
    let p = process::current().ok_or(ESRCH)?;
    let vm = p.vm().ok_or(EFAULT)?;
    let end = addr + len.next_multiple_of(FRAME_SIZE);
    let files = {
        let mut space = vm.lock();
        let files = space.shared_files(addr, end);
        space.unmap_range(addr, end);
        files
    };
    for f in files {
        let _ = crate::mm::pagecache::sync_inode(&f);
    }
    Ok(0)
}

/// msync(2): write back shared file mappings in the range.
pub fn msync(addr: u64, len: u64, _flags: u32) -> SysResult {
    if !addr.is_multiple_of(FRAME_SIZE) {
        return Err(EINVAL);
    }
    let p = process::current().ok_or(ESRCH)?;
    let vm = p.vm().ok_or(EFAULT)?;
    let files = vm.lock().shared_files(
        addr,
        addr + len.next_multiple_of(FRAME_SIZE).max(FRAME_SIZE),
    );
    for f in files {
        crate::mm::pagecache::sync_inode(&f)?;
    }
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
        // Grow the heap by a new area (the program may have replaced parts
        // of the old one, e.g. musl's guard page at the heap start).
        let blocked = space
            .areas
            .values()
            .any(|a| a.start < new_end && old_end < a.end);
        if blocked || new_end > vm::MMAP_TOP {
            return Ok(space.brk as i64);
        }
        space.add_area(Area {
            start: old_end,
            end: new_end,
            prot: PROT_READ | PROT_WRITE,
            flags: vm::MAP_PRIVATE,
            backing: Backing::Anon,
            name: "[heap]",
        })?;
    } else if new_end < old_end {
        space.unmap_range(new_end, old_end);
    }
    space.brk = new;
    Ok(new as i64)
}

/// mremap(2) (MREMAP_MAYMOVE; no MREMAP_FIXED).
pub fn mremap(old: u64, old_len: u64, new_len: u64, flags: u32) -> SysResult {
    const MREMAP_MAYMOVE: u32 = 1;
    if !old.is_multiple_of(FRAME_SIZE) || new_len == 0 || flags & !MREMAP_MAYMOVE != 0 {
        return Err(EINVAL);
    }
    let p = process::current().ok_or(ESRCH)?;
    let vm = p.vm().ok_or(EFAULT)?;
    let r = vm.lock().mremap(
        old,
        old_len.next_multiple_of(FRAME_SIZE),
        new_len.next_multiple_of(FRAME_SIZE),
        flags & MREMAP_MAYMOVE != 0,
    )?;
    Ok(r as i64)
}

/// membarrier(2): every command is a full barrier on every CPU running
/// this process (an IPI, like a TLB shootdown).
pub fn membarrier(cmd: u32) -> SysResult {
    const QUERY: u32 = 0;
    const GLOBAL: u32 = 1 << 0;
    const PRIVATE_EXPEDITED: u32 = 1 << 3;
    const REGISTER_PRIVATE_EXPEDITED: u32 = 1 << 4;
    let supported = GLOBAL | PRIVATE_EXPEDITED | REGISTER_PRIVATE_EXPEDITED;
    if cmd == QUERY {
        return Ok(supported as i64);
    }
    if cmd & !supported != 0 || cmd.count_ones() != 1 {
        return Err(EINVAL);
    }
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    if cmd != REGISTER_PRIVATE_EXPEDITED
        && let Some(vm) = process::current().and_then(|p| p.vm())
    {
        let pml4 = vm.lock().pml4;
        crate::arch::x86_64::smp::tlb_shootdown(pml4);
    }
    Ok(0)
}
