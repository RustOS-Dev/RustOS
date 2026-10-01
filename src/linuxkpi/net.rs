//! Linux `net_device`s as RustOS network interfaces
//! (src/linuxkpi/c/net.c). Transmit calls the driver's `ndo_start_xmit`;
//! frames the driver receives through NAPI are queued here for the RustOS
//! network thread, which pulls them with `receive()`.

use crate::errno::{EAGAIN, KResult};
use crate::net::{IfKind, NetDevice};
use crate::sync::Mutex;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use x86_64::instructions::interrupts::without_interrupts;

unsafe extern "C" {
    /// Build an skb from `data` and hand it to the driver. 0 on success.
    fn kpi_netdev_xmit(dev: *mut c_void, data: *const u8, len: u32) -> c_int;
    fn kpi_netdev_stop(dev: *mut c_void);
}

/// Frames waiting for the network thread, per interface (bounded).
const RX_QUEUE_MAX: usize = 1024;

struct LinuxNetDev {
    dev: usize,
    driver: &'static str,
    mac: [u8; 6],
    mtu: AtomicU32,
    wireless: bool,
    carrier: AtomicBool,
    rx: Mutex<VecDeque<Vec<u8>>>,
}

impl NetDevice for LinuxNetDev {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn mtu(&self) -> usize {
        self.mtu.load(Ordering::Relaxed) as usize
    }
    fn link_up(&self) -> bool {
        self.carrier.load(Ordering::SeqCst)
    }
    fn kind(&self) -> IfKind {
        if self.wireless {
            IfKind::Wireless
        } else {
            IfKind::Ethernet
        }
    }
    fn driver(&self) -> &'static str {
        self.driver
    }
    fn transmit(&self, frame: &[u8]) -> KResult<()> {
        match unsafe {
            kpi_netdev_xmit(self.dev as *mut c_void, frame.as_ptr(), frame.len() as u32)
        } {
            0 => Ok(()),
            _ => Err(EAGAIN),
        }
    }
    fn receive(&self) -> Option<Vec<u8>> {
        without_interrupts(|| self.rx.lock().pop_front())
    }
    fn shutdown(&self) {
        unsafe { kpi_netdev_stop(self.dev as *mut c_void) };
    }
}

static NEXT: AtomicU32 = AtomicU32::new(1);
static DEVS: Mutex<BTreeMap<u64, Arc<LinuxNetDev>>> = Mutex::new(BTreeMap::new());

/// Register a Linux net_device; writes the interface name (eth0, wlan0, …)
/// into `name` and returns a handle for the other calls.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netdev_register(
    dev: *mut c_void,
    mac: *const u8,
    mtu: u32,
    wireless: c_int,
    driver: *const c_char,
    name: *mut u8,
    name_len: u32,
) -> u64 {
    let mut m = [0u8; 6];
    m.copy_from_slice(unsafe { core::slice::from_raw_parts(mac, 6) });
    // Driver names are string literals in the driver's module; keep a copy.
    let drv = if driver.is_null() {
        "linux"
    } else {
        let s = unsafe { core::ffi::CStr::from_ptr(driver) }.to_string_lossy();
        &*alloc::boxed::Box::leak(String::from(s).into_boxed_str())
    };
    let nd = Arc::new(LinuxNetDev {
        dev: dev as usize,
        driver: drv,
        mac: m,
        mtu: AtomicU32::new(mtu),
        wireless: wireless != 0,
        carrier: AtomicBool::new(false),
        rx: Mutex::new(VecDeque::new()),
    });
    let handle = NEXT.fetch_add(1, Ordering::SeqCst) as u64;
    without_interrupts(|| DEVS.lock().insert(handle, nd.clone()));
    let ifname = crate::net::register(nd);
    let out = unsafe { core::slice::from_raw_parts_mut(name, name_len as usize) };
    let n = ifname.len().min(out.len().saturating_sub(1));
    out[..n].copy_from_slice(&ifname.as_bytes()[..n]);
    out[n] = 0;
    crate::println!("[linuxkpi] {} registered as {}", drv, ifname);
    handle
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netdev_rx(handle: u64, data: *const u8, len: u32) {
    let Some(nd) = without_interrupts(|| DEVS.lock().get(&handle).cloned()) else {
        return;
    };
    let frame = unsafe { core::slice::from_raw_parts(data, len as usize) }.to_vec();
    without_interrupts(|| {
        let mut q = nd.rx.lock();
        if q.len() < RX_QUEUE_MAX {
            q.push_back(frame);
        }
    });
    crate::net::kick();
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netdev_carrier(handle: u64, on: c_int) {
    if let Some(nd) = without_interrupts(|| DEVS.lock().get(&handle).cloned()) {
        nd.carrier.store(on != 0, Ordering::SeqCst);
        crate::net::kick();
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netdev_mtu(handle: u64, mtu: u32) {
    if let Some(nd) = without_interrupts(|| DEVS.lock().get(&handle).cloned()) {
        nd.mtu.store(mtu, Ordering::Relaxed);
    }
}
