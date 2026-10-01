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
    /// dev_open()/dev_close(); 0 or -errno.
    fn kpi_netdev_set_up(dev: *mut c_void, up: c_int) -> c_int;
}

/// Frames waiting for the network thread, per interface (bounded).
const RX_QUEUE_MAX: usize = 1024;

struct LinuxNetDev {
    dev: usize,
    driver: &'static str,
    mac: Mutex<[u8; 6]>,
    index: AtomicU32,
    mtu: AtomicU32,
    wireless: bool,
    carrier: AtomicBool,
    rx: Mutex<VecDeque<Vec<u8>>>,
}

impl NetDevice for LinuxNetDev {
    fn mac(&self) -> [u8; 6] {
        *self.mac.lock()
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
    fn set_up(&self, up: bool) -> KResult<()> {
        match unsafe { kpi_netdev_set_up(self.dev as *mut c_void, up as c_int) } {
            0 => Ok(()),
            e => Err(crate::errno::Errno(-e)),
        }
    }
}

static NEXT: AtomicU32 = AtomicU32::new(1);
static DEVS: Mutex<BTreeMap<u64, Arc<LinuxNetDev>>> = Mutex::new(BTreeMap::new());

fn dev(handle: u64) -> Option<Arc<LinuxNetDev>> {
    without_interrupts(|| DEVS.lock().get(&handle).cloned())
}

fn c_str(p: *const c_char) -> Option<String> {
    (!p.is_null()).then(|| String::from(unsafe { core::ffi::CStr::from_ptr(p) }.to_string_lossy()))
}

/// Register a Linux net_device under `name` (chosen by dev_alloc_name());
/// returns a handle for the other calls.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netdev_register(
    dev: *mut c_void,
    mac: *const u8,
    mtu: u32,
    wireless: c_int,
    ether: c_int,
    driver: *const c_char,
    name: *const c_char,
) -> u64 {
    let mut m = [0u8; 6];
    m.copy_from_slice(unsafe { core::slice::from_raw_parts(mac, 6) });
    // Driver names are string literals in the driver's module; keep a copy.
    let drv: &'static str = match c_str(driver) {
        Some(s) => alloc::boxed::Box::leak(s.into_boxed_str()),
        None => "linux",
    };
    let nd = Arc::new(LinuxNetDev {
        dev: dev as usize,
        driver: drv,
        mac: Mutex::new(m),
        index: AtomicU32::new(0),
        mtu: AtomicU32::new(mtu),
        wireless: wireless != 0,
        carrier: AtomicBool::new(false),
        rx: Mutex::new(VecDeque::new()),
    });
    let handle = NEXT.fetch_add(1, Ordering::SeqCst) as u64;
    without_interrupts(|| DEVS.lock().insert(handle, nd.clone()));
    let want = c_str(name);
    let (ifname, index) = crate::net::register_named(nd.clone(), want.as_deref());
    nd.index.store(index, Ordering::SeqCst);
    if ether == 0 {
        let _ = crate::net::set_dhcp(&ifname, false);
    }
    crate::println!("[linuxkpi] {} registered as {}", drv, ifname);
    handle
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netdev_ifindex(handle: u64) -> c_int {
    dev(handle).map_or(0, |d| d.index.load(Ordering::SeqCst) as c_int)
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netdev_unregister(handle: u64) {
    if let Some(d) = without_interrupts(|| DEVS.lock().remove(&handle)) {
        crate::net::unregister_index(d.index.load(Ordering::SeqCst));
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netdev_set_mac(handle: u64, mac: *const u8) {
    if let Some(d) = dev(handle) {
        d.mac
            .lock()
            .copy_from_slice(unsafe { core::slice::from_raw_parts(mac, 6) });
        crate::net::kick();
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_ifname_free(name: *const c_char) -> c_int {
    c_str(name).is_some_and(|n| crate::net::name_free(&n)) as c_int
}

// ------------------------------------------------------------- netlink

type NlInput = unsafe extern "C" fn(u32, u32, *const u8, usize);
type NlRelease = unsafe extern "C" fn(u32, u32);

/// The C kernel sockets, per netlink protocol.
static NL_C: Mutex<[(usize, usize); 32]> = Mutex::new([(0, 0); 32]);

fn nl_input(proto: u32, portid: u32, data: &[u8]) {
    let f = NL_C.lock().get(proto as usize).map_or(0, |e| e.0);
    if f != 0 {
        let f: NlInput = unsafe { core::mem::transmute(f) };
        unsafe { f(proto, portid, data.as_ptr(), data.len()) };
    }
}

fn nl_release(proto: u32, portid: u32) {
    let f = NL_C.lock().get(proto as usize).map_or(0, |e| e.1);
    if f != 0 {
        let f: NlRelease = unsafe { core::mem::transmute(f) };
        unsafe { f(proto, portid) };
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netlink_register(
    unit: u32,
    input: Option<NlInput>,
    release: Option<NlRelease>,
) {
    if unit as usize >= 32 {
        return;
    }
    NL_C.lock()[unit as usize] = (
        input.map_or(0, |f| f as usize),
        release.map_or(0, |f| f as usize),
    );
    if input.is_some() {
        crate::net::netlink::register_kernel(unit, Some(nl_input), Some(nl_release));
    } else {
        crate::net::netlink::register_kernel(unit, None, None);
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netlink_unicast(
    proto: u32,
    portid: u32,
    data: *const u8,
    len: usize,
) -> c_int {
    let d = unsafe { core::slice::from_raw_parts(data, len) }.to_vec();
    match crate::net::netlink::unicast(proto, portid, d) {
        Ok(()) => 0,
        Err(e) => -e.0,
    }
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netlink_multicast(
    proto: u32,
    group: u32,
    exclude: u32,
    data: *const u8,
    len: usize,
) -> c_int {
    let d = unsafe { core::slice::from_raw_parts(data, len) };
    crate::net::netlink::multicast_except(proto, group, exclude, d) as c_int
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_netlink_has_listeners(proto: u32, group: u32) -> c_int {
    crate::net::netlink::has_listeners(proto, group) as c_int
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
