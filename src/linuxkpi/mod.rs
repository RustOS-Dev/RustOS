//! LinuxKPI: Linux kernel APIs for drivers compiled from Linux sources
//! (`third_party/linux`, pinned in `third_party/linux/VERSION`). The C side
//! (`src/linuxkpi/c`, built by `build/linuxkpi.rs`) implements the Linux
//! API on top of the services in this module. Design:
//! docs/ROADMAP-ROUND4.md §4.

pub mod acpi;
pub mod chrdev;
pub mod firmware;
pub mod mm;
#[cfg(feature = "linux-mmc")]
pub mod mmc;
pub mod net;
pub mod pci;
pub mod sched;
pub mod sysfs;
#[cfg(feature = "linux-serial")]
pub mod tty;
#[cfg(feature = "linux-usb")]
pub mod usb;

use core::ffi::{CStr, c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, Ordering};

/// Linux version the imported sources come from.
pub const LINUX_VERSION: &str = "6.18.54";

static READY: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    /// lib/sort.c
    fn sort(
        base: *mut c_void,
        num: usize,
        size: usize,
        cmp: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> c_int>,
        swap: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, c_int)>,
    );
    fn kpi_mm_init() -> c_int;
    fn kpi_percpu_init() -> c_int;
    fn kpi_workqueues_init() -> c_int;
    fn kpi_rcu_init() -> c_int;
    fn kpi_devcore_init() -> c_int;
    fn kpi_chrdev_init() -> c_int;
    fn kpi_pci_bus_init() -> c_int;
    fn device_shutdown();
    fn kpi_selftest() -> c_int;
    fn kpi_jiffies_update();
    fn kpi_net_init() -> c_int;
}

unsafe extern "C" fn cmp_u32(a: *const c_void, b: *const c_void) -> c_int {
    let (a, b) = unsafe { (*(a as *const u32), *(b as *const u32)) };
    a.cmp(&b) as c_int
}

/// Set up LinuxKPI before Linux drivers probe: memory, per-CPU areas, the
/// softirq thread and workqueues, then a self-test of the primitives.
pub fn init() {
    let mut v: [u32; 8] = [42, 7, 19, 3, 88, 1, 56, 23];
    unsafe { sort(v.as_mut_ptr().cast(), v.len(), 4, Some(cmp_u32), None) };
    if !v.windows(2).all(|w| w[0] <= w[1]) {
        crate::println!("[linuxkpi] Linux sort() gave a wrong result; LinuxKPI disabled");
        return;
    }
    let steps: [(&str, unsafe extern "C" fn() -> c_int); 2] =
        [("memory", kpi_mm_init), ("per-CPU areas", kpi_percpu_init)];
    for (what, f) in steps {
        let r = unsafe { f() };
        if r != 0 {
            crate::println!("[linuxkpi] {what} setup failed ({r}); LinuxKPI disabled");
            return;
        }
    }
    sched::start_softirq();
    if unsafe { kpi_workqueues_init() } != 0 {
        crate::println!("[linuxkpi] workqueue setup failed; LinuxKPI disabled");
        return;
    }
    if unsafe { kpi_rcu_init() } != 0 {
        crate::println!("[linuxkpi] RCU setup failed; LinuxKPI disabled");
        return;
    }
    if unsafe { kpi_devcore_init() } != 0 {
        crate::println!("[linuxkpi] device core setup failed; LinuxKPI disabled");
        return;
    }
    if unsafe { kpi_chrdev_init() } != 0 {
        crate::println!("[linuxkpi] character device setup failed; LinuxKPI disabled");
        return;
    }
    pci::init();
    if unsafe { kpi_pci_bus_init() } != 0 {
        crate::println!("[linuxkpi] PCI bus setup failed; LinuxKPI disabled");
        return;
    }
    #[cfg(feature = "linux-usb")]
    if !usb::bus_init() {
        crate::println!("[linuxkpi] USB bus setup failed; LinuxKPI disabled");
        return;
    }
    unsafe { kpi_net_init() };
    READY.store(true, Ordering::SeqCst);
    let failed = unsafe { kpi_selftest() };
    crate::println!(
        "[linuxkpi] Linux {} APIs ready ({} CPUs); self-test {}",
        LINUX_VERSION,
        crate::arch::x86_64::cpu::cpu_count(),
        if failed == 0 {
            "passed"
        } else {
            "FAILED; Linux drivers disabled"
        }
    );
    if failed != 0 {
        READY.store(false, Ordering::SeqCst);
    }
}

/// Run the Linux drivers' module_init()/*_initcall() functions in level
/// order (after the native RustOS drivers probed).
pub fn run_initcalls() {
    if !ready() {
        return;
    }
    unsafe extern "C" {
        fn kpi_initcall_bounds(
            level: c_int,
            start: *mut *const i32,
            stop: *mut *const i32,
        ) -> c_int;
        /// src/linuxkpi/c/net.c: open interfaces registered meanwhile.
        fn kpi_netdev_open_pending();
    }
    for level in 0.. {
        let (mut start, mut stop) = (core::ptr::null(), core::ptr::null());
        if unsafe { kpi_initcall_bounds(level, &mut start, &mut stop) } != 0 {
            break;
        }
        let mut p = start;
        while p < stop {
            // Each entry is the function's address relative to the entry.
            let off = unsafe { p.read_unaligned() };
            if off != 0 {
                let f: extern "C" fn() -> c_int =
                    unsafe { core::mem::transmute((p as isize + off as isize) as *const ()) };
                let r = f();
                if r != 0 {
                    crate::println!("[linuxkpi] initcall at {:#x} returned {}", f as usize, r);
                }
            }
            p = unsafe { p.add(1) };
        }
    }
    unsafe { kpi_netdev_open_pending() };
    #[cfg(feature = "linux-usb")]
    usb::init();
}

/// Shut down Linux devices (reboot and power-off): each bus's shutdown
/// method, in reverse probe order.
pub fn shutdown() {
    if ready() {
        unsafe { device_shutdown() };
    }
}

/// Whether LinuxKPI initialized (Linux drivers may probe).
pub fn ready() -> bool {
    READY.load(Ordering::SeqCst)
}

/// Timer tick on CPU 0: advance jiffies.
pub fn tick() {
    if READY.load(Ordering::Relaxed) {
        unsafe { kpi_jiffies_update() };
    }
}

// --------------------------------------------------------------- logging

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_log(level: c_int, msg: *const u8, len: u64) {
    let bytes = unsafe { core::slice::from_raw_parts(msg, len as usize) };
    let text = core::str::from_utf8(bytes).unwrap_or("<invalid UTF-8>");
    // KERN_DEBUG (7) only with linux.debug set in kernel.conf.
    if level >= 7 && !crate::params::flag("linux.debug") {
        return;
    }
    crate::println!("[linux] {}", text);
}

fn cstr(p: *const c_char) -> &'static str {
    if p.is_null() {
        return "?";
    }
    unsafe { CStr::from_ptr(p) }.to_str().unwrap_or("?")
}

/// Print the return addresses of the caller's stack frames (the kernel is
/// built with frame pointers).
#[unsafe(no_mangle)]
pub extern "C" fn rustos_kpi_backtrace() {
    let mut rbp: u64;
    unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp) };
    crate::println!("[linux] backtrace:");
    for _ in 0..16 {
        if rbp < 0xffff_8000_0000_0000 || rbp & 7 != 0 {
            break;
        }
        let ret = unsafe { *((rbp + 8) as *const u64) };
        crate::println!("[linux]   {:#x}", ret);
        rbp = unsafe { *(rbp as *const u64) };
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_warn(file: *const c_char, line: c_int) {
    crate::println!("[linux] WARNING at {}:{}", cstr(file), line);
    rustos_kpi_backtrace();
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_bug(file: *const c_char, line: c_int) -> ! {
    panic!("Linux BUG at {}:{}", cstr(file), line);
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_panic(msg: *const c_char) -> ! {
    panic!("{}", cstr(msg));
}
