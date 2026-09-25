//! Safe access to user memory from syscalls.
//!
//! Every access first validates the range against the current process's
//! areas and faults the pages in (resolving copy-on-write for writes), so the
//! actual copy can never take an unresolvable fault in kernel mode.

use crate::errno::*;
use alloc::string::String;
use alloc::vec::Vec;

fn ensure(addr: u64, len: usize, write: bool) -> KResult<()> {
    if len == 0 {
        return Ok(());
    }
    let p = super::current().ok_or(EFAULT)?;
    let vm = p.vm().ok_or(EFAULT)?;
    vm.lock().ensure(addr, len as u64, write)
}

pub fn copy_from_user(dst: &mut [u8], src: u64) -> KResult<()> {
    ensure(src, dst.len(), false)?;
    unsafe { core::ptr::copy_nonoverlapping(src as *const u8, dst.as_mut_ptr(), dst.len()) };
    Ok(())
}

pub fn copy_to_user(dst: u64, src: &[u8]) -> KResult<()> {
    ensure(dst, src.len(), true)?;
    unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len()) };
    Ok(())
}

pub fn read_user<T: Copy>(addr: u64) -> KResult<T> {
    let mut v = core::mem::MaybeUninit::<T>::uninit();
    let buf = unsafe {
        core::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, core::mem::size_of::<T>())
    };
    copy_from_user(buf, addr)?;
    Ok(unsafe { v.assume_init() })
}

pub fn write_user<T: Copy>(addr: u64, v: &T) -> KResult<()> {
    let buf = unsafe {
        core::slice::from_raw_parts(v as *const T as *const u8, core::mem::size_of::<T>())
    };
    copy_to_user(addr, buf)
}

pub fn read_bytes(addr: u64, len: usize) -> KResult<Vec<u8>> {
    let mut v = alloc::vec![0u8; len];
    copy_from_user(&mut v, addr)?;
    Ok(v)
}

/// Read a NUL-terminated string of at most `max` bytes.
pub fn read_cstr(addr: u64, max: usize) -> KResult<String> {
    if addr == 0 {
        return Err(EFAULT);
    }
    let mut out = Vec::new();
    let mut a = addr;
    loop {
        // Validate page by page so strings near the end of an area work.
        let page_left = 4096 - (a % 4096) as usize;
        ensure(a, 1, false)?;
        let chunk = unsafe { core::slice::from_raw_parts(a as *const u8, page_left) };
        if let Some(pos) = chunk.iter().position(|&b| b == 0) {
            out.extend_from_slice(&chunk[..pos]);
            break;
        }
        out.extend_from_slice(chunk);
        if out.len() > max {
            return Err(ENAMETOOLONG);
        }
        a += page_left as u64;
    }
    if out.len() > max {
        return Err(ENAMETOOLONG);
    }
    String::from_utf8(out).map_err(|_| EINVAL)
}

/// Read a NULL-terminated array of string pointers (argv/envp).
pub fn read_str_array(addr: u64) -> KResult<Vec<String>> {
    let mut out = Vec::new();
    if addr == 0 {
        return Ok(out);
    }
    for i in 0..4096u64 {
        let p: u64 = read_user(addr + i * 8)?;
        if p == 0 {
            return Ok(out);
        }
        out.push(read_cstr(p, 128 * 1024)?);
    }
    Err(E2BIG)
}
