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
    let blob = loop {
        match crate::firmware::load(&name) {
            Ok(b) => break b,
            Err(e) => {
                if storage_mounted() || crate::time::nanos() >= STORAGE_WAIT_NS {
                    return -e.0;
                }
                if !waited {
                    crate::println!("[firmware] {name}: waiting for the boot drive to be mounted");
                    waited = true;
                }
                crate::sched::sleep_until(crate::time::nanos() + 500_000_000);
            }
        }
    };
    let p = alloc(blob.len());
    if p.is_null() {
        return -crate::errno::ENOMEM.0;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(blob.as_ptr(), p as *mut u8, blob.len());
        *data = p;
        *size = blob.len();
    }
    0
}
