//! Linux tty devices (src/linuxkpi/c/tty.c) as RustOS terminals: each
//! registered device (`/dev/ttyUSB0`, `/dev/ttyACM0`, ...) is a
//! [`crate::tty::Tty`] whose output and line settings go to the Linux
//! driver.

use crate::errno::*;
use crate::sync::Mutex;
use crate::tty::{Termios, Tty, TtyDriver};
use crate::vfs::{FileType, devfs};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use core::ffi::{CStr, c_char, c_int, c_void};
use core::sync::atomic::{AtomicU64, Ordering};

unsafe extern "C" {
    fn kpi_tty_open(kt: *mut c_void) -> c_int;
    fn kpi_tty_close(kt: *mut c_void);
    fn kpi_tty_write(kt: *mut c_void, buf: *const u8, len: u32) -> c_int;
    fn kpi_tty_set_termios(kt: *mut c_void, flags: *const u32, cc: *const u8);
}

struct LinuxTty {
    /// The C `struct kpi_tty`.
    kt: usize,
    rdev: u64,
}

impl TtyDriver for LinuxTty {
    fn rdev(&self) -> u64 {
        self.rdev
    }

    fn open(&self) -> KResult<()> {
        match unsafe { kpi_tty_open(self.kt as *mut c_void) } {
            0 => Ok(()),
            e => Err(Errno(-e)),
        }
    }

    fn close(&self) {
        unsafe { kpi_tty_close(self.kt as *mut c_void) };
    }

    fn write(&self, data: &[u8]) -> KResult<usize> {
        let n = unsafe {
            kpi_tty_write(
                self.kt as *mut c_void,
                data.as_ptr(),
                data.len().min(u32::MAX as usize) as u32,
            )
        };
        if n < 0 {
            Err(Errno(-n))
        } else {
            Ok(n as usize)
        }
    }

    fn set_termios(&self, t: &Termios) {
        let flags = [t.iflag, t.oflag, t.cflag, t.lflag];
        unsafe { kpi_tty_set_termios(self.kt as *mut c_void, flags.as_ptr(), t.cc.as_ptr()) };
    }
}

struct Dev {
    name: String,
    tty: Arc<Tty>,
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static DEVS: Mutex<BTreeMap<u64, Dev>> = Mutex::new(BTreeMap::new());

fn tty(handle: u64) -> Option<Arc<Tty>> {
    DEVS.lock().get(&handle).map(|d| d.tty.clone())
}

/// A Linux tty device appeared: create its terminal and `/dev` node.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_tty_register(
    name: *const c_char,
    major: u32,
    minor: u32,
    kt: *mut c_void,
    cflag: u32,
) -> u64 {
    let name = String::from(unsafe { CStr::from_ptr(name) }.to_string_lossy());
    let rdev = ((major as u64) << 8) | (minor as u64 & 0xff) | ((minor as u64 & !0xff) << 12);
    let drv = Arc::new(LinuxTty {
        kt: kt as usize,
        rdev,
    });
    let tty = Tty::new_driver(drv, cflag);
    let h = NEXT.fetch_add(1, Ordering::SeqCst);
    devfs::register(&name, FileType::CharDevice, rdev, tty.clone());
    crate::println!("[linuxkpi] /dev/{} (tty {}:{})", name, major, minor);
    DEVS.lock().insert(h, Dev { name, tty });
    h
}

/// The device went away: hang up its terminal and remove the node.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_tty_unregister(handle: u64) {
    let Some(d) = DEVS.lock().remove(&handle) else {
        return;
    };
    d.tty.hang_up();
    devfs::unregister(&d.name);
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_tty_hangup(handle: u64) {
    if let Some(t) = tty(handle) {
        t.hang_up();
    }
}

/// Characters from the device (Linux flip buffer work).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_tty_receive(handle: u64, data: *const u8, len: usize) {
    if let Some(t) = tty(handle)
        && !data.is_null()
    {
        t.receive_bytes(unsafe { core::slice::from_raw_parts(data, len) });
    }
}
