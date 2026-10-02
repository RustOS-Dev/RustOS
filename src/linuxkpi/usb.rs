//! RustOS USB devices for Linux USB drivers (src/linuxkpi/c/usb.c).
//!
//! Interfaces that no RustOS driver claims are offered to LinuxKPI, whose
//! USB bus (on Linux's driver core) matches them against Linux drivers.
//! Control transfers are synchronous. URBs are queued per endpoint and run
//! one at a time by a worker thread, which waits on the RustOS transfer and
//! then calls the URB's completion in C.

use crate::errno::*;
use crate::mm::dma::DmaBuffer;
use crate::sched::{self, WaitQueue};
use crate::sync::IrqMutex as Mutex;
use crate::usb::UsbDevice;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use usb_desc::{Endpoint, Interface, TransferType};

unsafe extern "C" {
    /// A new device: build the Linux usb_device (reads its descriptors).
    fn kpi_usb_device_add(
        handle: u64,
        busnum: u32,
        devnum: u32,
        speed: u32,
        port: u32,
        cfgval: u32,
    ) -> c_int;
    fn kpi_usb_bus_init() -> c_int;
    /// Offer interface `ifnum` to Linux drivers; 1 if one bound it.
    fn kpi_usb_probe_interface(handle: u64, ifnum: u32) -> c_int;
    fn kpi_usb_device_remove(handle: u64);
    /// URB done: status 0 or -errno, `actual` bytes moved.
    fn kpi_usb_complete(ctx: *mut c_void, status: c_int, actual: u32);
}

struct Req {
    ctx: usize,
    /// Unlinked while waiting: given back at once.
    cancelled: bool,
    buf: usize,
    len: usize,
    setup: Option<[u8; 8]>,
    zero_packet: bool,
}

struct EpWorker {
    queue: Mutex<VecDeque<Req>>,
    /// The request in flight (its ctx), and a request to abandon it.
    current: Mutex<Option<usize>>,
    cancel: AtomicBool,
    wq: WaitQueue,
    stop: AtomicBool,
}

struct Dev {
    usb: Arc<UsbDevice>,
    workers: Mutex<BTreeMap<u8, Arc<EpWorker>>>,
    added: AtomicBool,
    /// The C struct usb_device.
    cookie: AtomicUsize,
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static DEVS: Mutex<BTreeMap<u64, Arc<Dev>>> = Mutex::new(BTreeMap::new());

fn dev(handle: u64) -> Option<Arc<Dev>> {
    DEVS.lock().get(&handle).cloned()
}

fn errno(e: Errno) -> c_int {
    -e.0
}

/// The RustOS-side handle of `usb`, creating it (and the Linux device) on
/// first sight.
fn handle_of(usb: &Arc<UsbDevice>) -> u64 {
    if let Some((&h, _)) = DEVS.lock().iter().find(|(_, d)| Arc::ptr_eq(&d.usb, usb)) {
        return h;
    }
    let h = NEXT.fetch_add(1, Ordering::SeqCst);
    let d = Arc::new(Dev {
        usb: usb.clone(),
        workers: Mutex::new(BTreeMap::new()),
        added: AtomicBool::new(false),
        cookie: AtomicUsize::new(0),
    });
    DEVS.lock().insert(h, d);
    usb.on_detach(move || remove(h));
    h
}

fn remove(handle: u64) {
    let Some(d) = dev(handle) else {
        return;
    };
    // Fail what is queued and in flight, then unbind the Linux drivers
    // (which look the device up by its handle while they disconnect).
    for w in d.workers.lock().values() {
        w.stop.store(true, Ordering::SeqCst);
        w.wq.wake_all();
    }
    if d.added.load(Ordering::SeqCst) {
        unsafe { kpi_usb_device_remove(handle) };
    }
    DEVS.lock().remove(&handle);
}

/// RustOS USB driver probe: offer an unclaimed interface to Linux.
fn probe(usb: &Arc<UsbDevice>, iface: &Interface) -> bool {
    if !super::ready() {
        return false;
    }
    let h = handle_of(usb);
    let d = dev(h).unwrap();
    if !d.added.swap(true, Ordering::SeqCst) {
        let speed = match usb.speed {
            crate::usb::Speed::Low => 1,
            crate::usb::Speed::Full => 2,
            crate::usb::Speed::High => 3,
            _ => 5,
        };
        let r = unsafe {
            kpi_usb_device_add(
                h,
                usb.hc.index as u32 + 1,
                usb.slot as u32,
                speed,
                usb.port as u32,
                usb.config.lock().as_ref().map_or(1, |c| c.value as u32),
            )
        };
        if r != 0 {
            crate::println!("[linuxkpi] {}: usb_device setup failed: {}", usb.name(), r);
            return false;
        }
    }
    unsafe { kpi_usb_probe_interface(h, iface.number as u32) != 0 }
}

/// Register the Linux "usb" bus (before Linux drivers register).
pub fn bus_init() -> bool {
    unsafe { kpi_usb_bus_init() == 0 }
}

/// Offer devices to Linux drivers (after their module_init()).
pub fn init() {
    crate::usb::register_driver("linux", probe);
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_usb_cookie(handle: u64) -> *mut c_void {
    dev(handle).map_or(core::ptr::null_mut(), |d| {
        d.cookie.load(Ordering::SeqCst) as *mut c_void
    })
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_usb_set_cookie(handle: u64, cookie: *mut c_void) {
    if let Some(d) = dev(handle) {
        d.cookie.store(cookie as usize, Ordering::SeqCst);
    }
}

/// A Linux driver claims interface `ifnum` (usb_driver_claim_interface):
/// enable the endpoints of its default setting.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_usb_claim(handle: u64, ifnum: u32) -> c_int {
    (rustos_kpi_usb_set_interface(handle, ifnum, 0, 0) == 0) as c_int
}

fn endpoint(usb: &UsbDevice, addr: u8) -> Option<Endpoint> {
    usb.config.lock().as_ref().and_then(|c| {
        c.interfaces
            .iter()
            .flat_map(|i| i.endpoints.iter())
            .find(|e| e.address == addr)
            .copied()
    })
}

/// Enable the endpoints of interface `ifnum`, alternate setting `alt`, on
/// the controller (and select the setting on the device if `select`).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_usb_set_interface(
    handle: u64,
    ifnum: u32,
    alt: u32,
    select: c_int,
) -> c_int {
    let Some(d) = dev(handle) else {
        return errno(ENODEV);
    };
    let eps: Vec<Endpoint> = d
        .usb
        .config
        .lock()
        .as_ref()
        .and_then(|c| {
            c.interfaces
                .iter()
                .find(|i| i.number as u32 == ifnum && i.alternate as u32 == alt)
                .map(|i| i.endpoints.clone())
        })
        .unwrap_or_default();
    if select != 0
        && let Err(e) = d.usb.control_out(
            0x01,
            crate::usb::REQ_SET_INTERFACE,
            alt as u16,
            ifnum as u16,
            &[],
        )
    {
        return errno(e);
    }
    if eps.is_empty() {
        return 0;
    }
    match d.usb.configure_endpoints(&eps) {
        Ok(()) => 0,
        Err(e) => errno(e),
    }
}

/// A control transfer: `setup` is the 8-byte request; returns the bytes
/// transferred or -errno.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_usb_control(
    handle: u64,
    setup: *const u8,
    data: *mut u8,
    timeout_ms: u32,
) -> c_int {
    let Some(d) = dev(handle) else {
        return errno(ENODEV);
    };
    let s = unsafe { core::slice::from_raw_parts(setup, 8) };
    control(&d.usb, s.try_into().unwrap(), data, timeout_ms)
}

fn control(usb: &UsbDevice, s: [u8; 8], data: *mut u8, _timeout_ms: u32) -> c_int {
    let len = u16::from_le_bytes([s[6], s[7]]) as usize;
    let value = u16::from_le_bytes([s[2], s[3]]);
    let index = u16::from_le_bytes([s[4], s[5]]);
    if s[0] & 0x80 != 0 {
        match usb.control_in(s[0] & 0x7f, s[1], value, index, len) {
            Ok(v) => {
                if !data.is_null() {
                    unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), data, v.len().min(len)) };
                }
                v.len() as c_int
            }
            Err(e) => errno(e),
        }
    } else {
        let out = if len == 0 || data.is_null() {
            &[][..]
        } else {
            unsafe { core::slice::from_raw_parts(data, len) }
        };
        match usb.control_out(s[0], s[1], value, index, out) {
            Ok(()) => len as c_int,
            Err(e) => errno(e),
        }
    }
}

/// Queue a transfer on endpoint `ep` (address with direction bit; 0 for
/// control, with `setup`). Completes through kpi_usb_complete(ctx).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_usb_submit(
    handle: u64,
    ep: u8,
    buf: *mut u8,
    len: u32,
    setup: *const u8,
    zero_packet: c_int,
    ctx: *mut c_void,
) -> c_int {
    let Some(d) = dev(handle) else {
        return errno(ENODEV);
    };
    if d.usb.is_gone() {
        return errno(ENODEV);
    }
    let setup = (!setup.is_null()).then(|| {
        let mut s = [0u8; 8];
        s.copy_from_slice(unsafe { core::slice::from_raw_parts(setup, 8) });
        s
    });
    let w = {
        let mut ws = d.workers.lock();
        ws.entry(ep)
            .or_insert_with(|| {
                let w = Arc::new(EpWorker {
                    queue: Mutex::new(VecDeque::new()),
                    current: Mutex::new(None),
                    cancel: AtomicBool::new(false),
                    wq: WaitQueue::new(),
                    stop: AtomicBool::new(false),
                });
                let (w2, d2) = (w.clone(), d.clone());
                sched::spawn(&alloc::format!("usb-ep{:02x}", ep), move || {
                    worker(d2, ep, w2)
                });
                w
            })
            .clone()
    };
    w.queue.lock().push_back(Req {
        ctx: ctx as usize,
        buf: buf as usize,
        len: len as usize,
        setup,
        zero_packet: zero_packet != 0,
        cancelled: false,
    });
    w.wq.wake_all();
    0
}

/// Cancel URB `ctx` on `ep`: dequeued if still waiting (1), or its
/// transfer stopped if running (2; it then completes with an error).
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_usb_cancel(handle: u64, ep: u8, ctx: *mut c_void) -> c_int {
    let Some(d) = dev(handle) else {
        return 0;
    };
    let Some(w) = d.workers.lock().get(&ep).cloned() else {
        return 0;
    };
    if let Some(r) = w.queue.lock().iter_mut().find(|r| r.ctx == ctx as usize) {
        r.cancelled = true;
        w.wq.wake_all();
        return 1;
    }
    if *w.current.lock() == Some(ctx as usize) {
        w.cancel.store(true, Ordering::SeqCst);
        return 2;
    }
    0
}

#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_usb_clear_halt(handle: u64, ep: u8) -> c_int {
    let Some(d) = dev(handle) else {
        return errno(ENODEV);
    };
    match endpoint(&d.usb, ep) {
        Some(e) => {
            d.usb.clear_halt(&e);
            0
        }
        None => errno(EINVAL),
    }
}

fn worker(d: Arc<Dev>, ep: u8, w: Arc<EpWorker>) {
    loop {
        w.wq.wait_until(|| w.stop.load(Ordering::SeqCst) || !w.queue.lock().is_empty());
        if w.stop.load(Ordering::SeqCst) {
            // Unplugged: fail what is left.
            let left: Vec<Req> = w.queue.lock().drain(..).collect();
            for r in left {
                unsafe { kpi_usb_complete(r.ctx as *mut c_void, -(ESHUTDOWN.0), 0) };
            }
            return;
        }
        let Some(r) = w.queue.lock().pop_front() else {
            continue;
        };
        if r.cancelled {
            unsafe { kpi_usb_complete(r.ctx as *mut c_void, -(ENOENT.0), 0) };
            continue;
        }
        w.cancel.store(false, Ordering::SeqCst);
        *w.current.lock() = Some(r.ctx);
        let (status, actual) = run(&d.usb, ep, &r, &w);
        *w.current.lock() = None;
        unsafe { kpi_usb_complete(r.ctx as *mut c_void, status, actual as u32) };
    }
}

/// Run one transfer to completion; returns (status, bytes).
fn run(usb: &UsbDevice, ep: u8, r: &Req, w: &EpWorker) -> (c_int, usize) {
    if let Some(s) = r.setup {
        let n = control(usb, s, r.buf as *mut u8, 5000);
        return if n < 0 { (n, 0) } else { (0, n as usize) };
    }
    let Some(e) = endpoint(usb, ep) else {
        return (errno(EINVAL), 0);
    };
    if e.transfer_type() == TransferType::Isochronous {
        return (errno(EOPNOTSUPP), 0);
    }
    let Some(mut buf) = DmaBuffer::new(r.len.max(1)) else {
        return (errno(ENOMEM), 0);
    };
    let is_in = e.is_in();
    if !is_in && r.len > 0 {
        buf.as_mut_slice()[..r.len]
            .copy_from_slice(unsafe { core::slice::from_raw_parts(r.buf as *const u8, r.len) });
    }
    let td = match usb.submit(&e, 0, &buf, 0, r.len) {
        Ok(td) => td,
        Err(err) => return (errno(err), 0),
    };
    let res = usb.wait_abortable(&td, &|| {
        w.cancel.load(Ordering::SeqCst) || w.stop.load(Ordering::SeqCst)
    });
    let n = match res {
        Ok(n) => n,
        Err(EPIPE) => {
            usb.clear_halt(&e);
            return (-(EPIPE.0), 0);
        }
        // Cancelled (usb_kill_urb/usb_unlink_urb), or unplugged.
        Err(ECANCELED) if w.stop.load(Ordering::SeqCst) => return (-(ESHUTDOWN.0), 0),
        Err(ECANCELED) => return (-(ENOENT.0), 0),
        Err(ENODEV) => return (-(ESHUTDOWN.0), 0),
        Err(err) => return (errno(err), 0),
    };
    if is_in {
        let n = n.min(r.len);
        unsafe { core::ptr::copy_nonoverlapping(buf.as_slice().as_ptr(), r.buf as *mut u8, n) };
        (0, n)
    } else {
        // URB_ZERO_PACKET: end a transfer that fills whole packets.
        let mps = e.packet_size() as usize;
        if r.zero_packet && r.len > 0 && mps > 0 && r.len.is_multiple_of(mps) {
            let _ = usb.transfer(&e, &buf, 0, Some(5000));
        }
        (0, n)
    }
}
