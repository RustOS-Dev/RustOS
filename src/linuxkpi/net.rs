//! Linux `net_device`s as RustOS network interfaces
//! (src/linuxkpi/c/net.c). Transmit calls the driver's `ndo_start_xmit`;
//! frames the driver receives through NAPI are queued here for the RustOS
//! network thread, which pulls them with `receive()`. Tunnels (WireGuard)
//! are IP-only interfaces, and the kinds of link Linux drivers register
//! (rtnl_link_ops) can be created from rtnetlink.

use crate::errno::{EAGAIN, EOPNOTSUPP, KResult};
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
    /// 1 if some transmit queue of the device is running.
    fn kpi_netdev_tx_ready(dev: *mut c_void) -> c_int;
    /// dev_open()/dev_close(); 0 or -errno.
    fn kpi_netdev_set_up(dev: *mut c_void, up: c_int) -> c_int;
    /// ndo_set_mac_address(); 0 or -errno.
    fn kpi_netdev_set_mac(dev: *mut c_void, mac: *const u8) -> c_int;
    /// Create a link of a registered rtnl_link_ops kind; 0 or -errno.
    fn kpi_rtnl_newlink(kind: *const c_char, name: *const c_char) -> c_int;
    /// Remove a link created by kind; 0 or -errno.
    fn kpi_rtnl_dellink(ifindex: c_int) -> c_int;
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
    /// Bare IP packets (a tunnel), not Ethernet frames.
    ip: bool,
    kind: Option<&'static str>,
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
    fn ip_only(&self) -> bool {
        self.ip
    }
    fn link_kind(&self) -> Option<&'static str> {
        self.kind
    }
    fn transmit(&self, frame: &[u8]) -> KResult<()> {
        match unsafe {
            kpi_netdev_xmit(self.dev as *mut c_void, frame.as_ptr(), frame.len() as u32)
        } {
            0 => Ok(()),
            _ => Err(EAGAIN),
        }
    }
    fn tx_ready(&self) -> bool {
        unsafe { kpi_netdev_tx_ready(self.dev as *mut c_void) != 0 }
    }
    fn receive(&self) -> Option<Vec<u8>> {
        without_interrupts(|| self.rx.lock().pop_front())
    }
    fn shutdown(&self) {
        unsafe { kpi_netdev_stop(self.dev as *mut c_void) };
    }
    fn set_mac(&self, mac: [u8; 6]) -> KResult<()> {
        match unsafe { kpi_netdev_set_mac(self.dev as *mut c_void, mac.as_ptr()) } {
            0 => Ok(()),
            e => Err(crate::errno::Errno(-e)),
        }
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

/// Names Linux code hands over that live as long as the kernel (driver
/// names, link kinds), interned so each is leaked once.
static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn intern(s: String) -> &'static str {
    let mut names = NAMES.lock();
    if let Some(n) = names.iter().find(|n| **n == s) {
        return n;
    }
    let n: &'static str = alloc::boxed::Box::leak(s.into_boxed_str());
    names.push(n);
    n
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
    ip: c_int,
    kind: *const c_char,
    driver: *const c_char,
    name: *const c_char,
) -> u64 {
    let mut m = [0u8; 6];
    m.copy_from_slice(unsafe { core::slice::from_raw_parts(mac, 6) });
    // Driver names are string literals in the driver's module; keep a copy.
    let drv: &'static str = c_str(driver).map_or("linux", intern);
    let nd = Arc::new(LinuxNetDev {
        dev: dev as usize,
        driver: drv,
        mac: Mutex::new(m),
        index: AtomicU32::new(0),
        mtu: AtomicU32::new(mtu),
        wireless: wireless != 0,
        ip: ip != 0,
        kind: c_str(kind).map(intern),
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
extern "C" fn rustos_kpi_netdev_state(handle: u64, up: c_int) {
    if let Some(d) = dev(handle) {
        crate::net::set_admin_state(d.index.load(Ordering::SeqCst), up != 0);
    }
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

/// A transmit queue woke up: let the stack send what it held back.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_net_kick() {
    crate::net::kick();
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_ifname_free(name: *const c_char) -> c_int {
    c_str(name).is_some_and(|n| crate::net::name_free(&n)) as c_int
}

// ---------------------------------------------------------- link kinds

/// rtnl_link_ops kinds Linux drivers registered.
static KINDS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn c_string(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.bytes().filter(|&b| b != 0).collect();
    v.push(0);
    v
}

fn newlink(kind: &str, name: Option<&str>) -> KResult<()> {
    if !KINDS.lock().contains(&kind) {
        return Err(EOPNOTSUPP);
    }
    let kind = c_string(kind);
    let name = name.map(c_string);
    let r = unsafe {
        kpi_rtnl_newlink(
            kind.as_ptr().cast(),
            name.as_ref()
                .map_or(core::ptr::null(), |n| n.as_ptr().cast()),
        )
    };
    match r {
        0 => Ok(()),
        e => Err(crate::errno::Errno(-e)),
    }
}

fn dellink(index: u32) -> KResult<()> {
    match unsafe { kpi_rtnl_dellink(index as c_int) } {
        0 => Ok(()),
        e => Err(crate::errno::Errno(-e)),
    }
}

/// rtnl_link_register()/rtnl_link_unregister() of a kind.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_rtnl_kind(kind: *const c_char, add: c_int) {
    let Some(kind) = c_str(kind) else { return };
    let kind = intern(kind);
    let mut kinds = KINDS.lock();
    kinds.retain(|k| *k != kind);
    if add != 0 {
        kinds.push(kind);
        crate::net::rtnetlink::set_link_handler(newlink, dellink);
    }
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
