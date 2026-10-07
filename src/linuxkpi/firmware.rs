//! Firmware loading for LinuxKPI's `request_firmware()`
//! (src/linuxkpi/c/firmware.c) on `crate::firmware`.

use alloc::string::String;
use core::ffi::{CStr, c_char, c_int, c_void};

/// Boot drives (a USB stick in particular) may still be enumerating when a
/// Linux driver asks for firmware: a lookup that misses waits for them
/// until this long after boot.
const STORAGE_WAIT_NS: u64 = 60_000_000_000;

fn storage_mounted() -> bool {
    crate::vfs::mounts()
        .iter()
        .any(|(path, _, _)| path == "/storage" || path == "/boot/efi")
}

/// Load firmware `name` into a buffer from `alloc` (vmalloc on the C
/// side). Returns 0 or a negative errno.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_firmware_load(
    name: *const c_char,
    alloc: extern "C" fn(usize) -> *mut c_void,
    data: *mut *mut c_void,
    size: *mut usize,
) -> c_int {
    let name = String::from(unsafe { CStr::from_ptr(name) }.to_string_lossy());
    // Names are paths relative to the firmware directories.
    if name.split('/').any(|c| c.is_empty() || c == "..") {
        return -crate::errno::EINVAL.0;
    }
    let mut waited = false;
    let path = loop {
        match crate::firmware::find(&name) {
            Some(p) => break p,
            None => {
                if storage_mounted() || crate::time::nanos() >= STORAGE_WAIT_NS {
                    // Drivers whose firmware is optional probe for several
                    // names: only say why when the image lacks its firmware.
                    if let Some(why) = crate::firmware::stock_missing() {
                        crate::println!("[firmware] {name}: not found ({why})");
                    }
                    return -crate::errno::ENOENT.0;
                }
                if !waited {
                    crate::println!("[firmware] {name}: waiting for the boot drive to be mounted");
                    waited = true;
                }
                crate::sched::sleep_until(crate::time::nanos() + 500_000_000);
            }
        }
    };
    // Read straight into the driver's (vmalloc) buffer: GPU firmware runs
    // to tens of megabytes, too much to stage on the kernel heap.
    let inode = match crate::vfs::lookup(&path) {
        Ok(i) => i,
        Err(e) => return -e.0,
    };
    let len = match inode.metadata() {
        Ok(m) => m.size as usize,
        Err(e) => return -e.0,
    };
    let p = alloc(len.max(1));
    if p.is_null() {
        return -crate::errno::ENOMEM.0;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(p as *mut u8, len) };
    let mut done = 0;
    while done < len {
        match inode.read_at(done as u64, &mut buf[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) => return -e.0,
        }
    }
    crate::println!("[firmware] loaded {} ({} bytes)", path, done);
    unsafe {
        *data = p;
        *size = done;
    }
    0
}
