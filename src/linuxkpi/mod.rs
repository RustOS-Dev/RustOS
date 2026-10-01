//! LinuxKPI: Linux kernel APIs for drivers compiled from Linux sources
//! (`third_party/linux`, pinned in `third_party/linux/VERSION`). The C side
//! is built by `build/linuxkpi.rs`; see docs/ROADMAP-ROUND4.md for the
//! design.

pub mod sched;

use core::ffi::{c_int, c_void};

unsafe extern "C" {
    /// lib/sort.c
    fn sort(
        base: *mut c_void,
        num: usize,
        size: usize,
        cmp: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> c_int>,
        swap: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, c_int)>,
    );
    /// lib/bsearch.c
    fn bsearch(
        key: *const c_void,
        base: *const c_void,
        num: usize,
        size: usize,
        cmp: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> c_int>,
    ) -> *mut c_void;
}

unsafe extern "C" fn cmp_u32(a: *const c_void, b: *const c_void) -> c_int {
    let (a, b) = unsafe { (*(a as *const u32), *(b as *const u32)) };
    a.cmp(&b) as c_int
}

/// Linux version the imported sources come from.
pub const LINUX_VERSION: &str = "6.18.54";

/// Boot-time check that code compiled from Linux is linked and callable.
pub fn init() {
    let mut v: [u32; 8] = [42, 7, 19, 3, 88, 1, 56, 23];
    unsafe {
        sort(v.as_mut_ptr().cast(), v.len(), 4, Some(cmp_u32), None);
    }
    let sorted = v.windows(2).all(|w| w[0] <= w[1]);
    let key = 56u32;
    let hit = unsafe {
        bsearch(
            (&key as *const u32).cast(),
            v.as_ptr().cast(),
            v.len(),
            4,
            Some(cmp_u32),
        )
    };
    let found = !hit.is_null() && unsafe { *(hit as *const u32) } == key;
    crate::println!(
        "[linuxkpi] Linux {} code linked: sort {}, bsearch {}",
        LINUX_VERSION,
        if sorted { "ok" } else { "FAILED" },
        if found { "ok" } else { "FAILED" }
    );
}
