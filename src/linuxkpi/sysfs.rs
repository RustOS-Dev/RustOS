//! sysfs for LinuxKPI: the C side (src/linuxkpi/c/sysfs.c) publishes
//! kobject directories, attribute files and links into RustOS's /sys
//! registry (`crate::vfs::sysfs`).

use crate::errno::*;
use crate::vfs::sysfs::{self, Attr};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

unsafe extern "C" {
    /// Fill `buf` (a page) from the attribute; bytes written or -errno.
    fn kpi_sysfs_show(cookie: *mut c_void, buf: *mut u8) -> isize;
    /// Store `len` bytes into the attribute; bytes consumed or -errno.
    fn kpi_sysfs_store(cookie: *mut c_void, buf: *const u8, len: usize) -> isize;
}

const PAGE: usize = 4096;

struct KpiAttr {
    cookie: usize,
    mode: u32,
    /// show/store calls running; `drain` waits for them.
    active: AtomicU32,
    dead: AtomicBool,
}

impl KpiAttr {
    fn enter(&self) -> KResult<()> {
        self.active.fetch_add(1, Ordering::SeqCst);
        if self.dead.load(Ordering::SeqCst) {
            self.active.fetch_sub(1, Ordering::SeqCst);
            return Err(ENOENT);
        }
        Ok(())
    }
}

fn result(r: isize) -> KResult<usize> {
    if r < 0 {
        Err(Errno(-r as i32))
    } else {
        Ok(r as usize)
    }
}

impl Attr for KpiAttr {
    fn show(&self) -> KResult<Vec<u8>> {
        self.enter()?;
        let mut buf = vec![0u8; PAGE];
        let r = unsafe { kpi_sysfs_show(self.cookie as *mut c_void, buf.as_mut_ptr()) };
        self.active.fetch_sub(1, Ordering::SeqCst);
        buf.truncate(result(r)?.min(PAGE));
        Ok(buf)
    }

    fn store(&self, data: &[u8]) -> KResult<usize> {
        if self.mode & 0o222 == 0 {
            return Err(EACCES);
        }
        self.enter()?;
        // Linux hands store() at most a page, NUL-terminated.
        let mut buf = Vec::with_capacity(data.len().min(PAGE - 1) + 1);
        buf.extend_from_slice(&data[..data.len().min(PAGE - 1)]);
        buf.push(0);
        let r = unsafe { kpi_sysfs_store(self.cookie as *mut c_void, buf.as_ptr(), buf.len() - 1) };
        self.active.fetch_sub(1, Ordering::SeqCst);
        result(r)
    }

    fn mode(&self) -> u32 {
        self.mode
    }

    fn drain(&self) {
        self.dead.store(true, Ordering::SeqCst);
        while self.active.load(Ordering::SeqCst) != 0 {
            crate::sched::yield_now();
        }
    }
}

fn path(p: *const c_char) -> String {
    String::from(unsafe { CStr::from_ptr(p) }.to_string_lossy())
}

fn status(r: KResult<()>) -> c_int {
    match r {
        Ok(()) => 0,
        Err(e) => -e.0,
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_sysfs_mkdir(p: *const c_char) -> c_int {
    status(sysfs::add_dir(&path(p)))
}

/// `cookie` is handed back to kpi_sysfs_show/store; the C side may free it
/// once `rustos_kpi_sysfs_remove` of this path (or a parent) returns.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_sysfs_add_file(p: *const c_char, mode: u32, cookie: *mut c_void) -> c_int {
    let attr = Arc::new(KpiAttr {
        cookie: cookie as usize,
        mode,
        active: AtomicU32::new(0),
        dead: AtomicBool::new(false),
    });
    status(sysfs::add_file(&path(p), attr))
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_sysfs_add_link(p: *const c_char, target: *const c_char) -> c_int {
    status(sysfs::add_link(&path(p), &path(target)))
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_sysfs_remove(p: *const c_char) {
    sysfs::remove(&path(p));
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_sysfs_rename(old: *const c_char, new: *const c_char) -> c_int {
    status(sysfs::rename(&path(old), &path(new)))
}

/// A uevent from the Linux device model (kobject_uevent_env).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_uevent(buf: *const u8, len: usize) {
    crate::net::netlink::uevent_raw(unsafe { core::slice::from_raw_parts(buf, len) });
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_uevent_seqnum() -> u64 {
    crate::net::netlink::uevent_seqnum()
}
