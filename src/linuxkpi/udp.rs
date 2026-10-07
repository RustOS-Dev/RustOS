//! In-kernel UDP sockets for Linux UDP tunnels (WireGuard):
//! src/linuxkpi/c/udptunnel.c's `udp_sock_create4()` opens one here.
//!
//! Each socket is a RustOS UDP socket (`net::socket`) served by a kernel
//! thread: it sends what Linux queued (`rustos_kpi_udp_send` runs in any
//! context, so it only queues) and hands every datagram received to
//! `kpi_udp_rx()`, which feeds the tunnel's encap_rcv() callback.

use crate::errno::*;
use crate::net::socket::{AF_INET, Proto, Socket};
use crate::net::{EPOCH, SOCK_WQ};
use crate::sync::IrqMutex;
use crate::vfs::FileLike;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

unsafe extern "C" {
    /// A datagram for the Linux socket `ctx` (addresses in network order,
    /// ports in host order).
    fn kpi_udp_rx(
        ctx: *mut c_void,
        saddr: u32,
        sport: u16,
        daddr: u32,
        dport: u16,
        data: *const u8,
        len: u32,
    );
}

/// Datagrams waiting to be sent, per socket; more are dropped.
const TX_QUEUE_MAX: usize = 1024;
/// Datagrams handed to Linux per round, before sending again.
const RX_BATCH: usize = 64;

struct KernelUdp {
    sock: Arc<Socket>,
    /// The Linux `struct sock`.
    ctx: usize,
    port: u16,
    tx: IrqMutex<VecDeque<(IpEndpoint, Vec<u8>)>>,
    /// Bumped by every queued datagram (wakes the thread).
    tx_gen: AtomicU64,
    closing: AtomicBool,
    done: AtomicBool,
}

static SOCKETS: IrqMutex<BTreeMap<u64, Arc<KernelUdp>>> = IrqMutex::new(BTreeMap::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

fn ipv4(addr: u32) -> Ipv4Address {
    Ipv4Address::from_octets(addr.to_ne_bytes())
}

fn run(k: Arc<KernelUdp>) {
    let mut buf = vec![0u8; 65536];
    while !k.closing.load(Ordering::SeqCst) {
        let epoch = EPOCH.load(Ordering::SeqCst);
        let queued = k.tx_gen.load(Ordering::SeqCst);
        let mut busy = false;
        loop {
            let next = k.tx.lock().pop_front();
            let Some((ep, data)) = next else { break };
            match k.sock.send_to(&data, Some(ep), true) {
                Ok(_) => busy = true,
                Err(EAGAIN) => {
                    // The socket's buffer is full: retry after the stack ran.
                    k.tx.lock().push_front((ep, data));
                    break;
                }
                // Unroutable: dropped, as IP would.
                Err(_) => busy = true,
            }
        }
        for _ in 0..RX_BATCH {
            let Ok((n, Some(from))) = k.sock.recv_from(&mut buf, true, false) else {
                break;
            };
            busy = true;
            let IpAddress::Ipv4(src) = from.addr else {
                continue;
            };
            let saddr = u32::from_ne_bytes(src.octets());
            // SAFETY: ctx is the Linux socket, alive until close() has
            // stopped this thread.
            unsafe {
                kpi_udp_rx(
                    k.ctx as *mut c_void,
                    saddr,
                    from.port,
                    0,
                    k.port,
                    buf.as_ptr(),
                    n as u32,
                )
            };
        }
        if !busy {
            SOCK_WQ.wait_timeout(100, || {
                EPOCH.load(Ordering::SeqCst) != epoch
                    || k.tx_gen.load(Ordering::SeqCst) != queued
                    || k.closing.load(Ordering::SeqCst)
            });
        }
    }
    k.done.store(true, Ordering::SeqCst);
}

/// Open a UDP socket bound to `addr:port` (0: any address, any port) for
/// the Linux socket `ctx`. Returns 0 or -errno; `handle` and the port
/// bound (`bound`) are filled in.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_udp_open(
    addr: u32,
    port: u16,
    ctx: *mut c_void,
    handle: *mut u64,
    bound: *mut u16,
) -> c_int {
    let sock = Socket::new(AF_INET, Proto::Udp);
    if let Err(e) = sock.bind(IpEndpoint::new(IpAddress::Ipv4(ipv4(addr)), port)) {
        return -e.0;
    }
    let port = sock.local_endpoint().port;
    let k = Arc::new(KernelUdp {
        sock,
        ctx: ctx as usize,
        port,
        tx: IrqMutex::new(VecDeque::new()),
        tx_gen: AtomicU64::new(0),
        closing: AtomicBool::new(false),
        done: AtomicBool::new(false),
    });
    let h = NEXT.fetch_add(1, Ordering::SeqCst);
    SOCKETS.lock().insert(h, k.clone());
    crate::sched::spawn(&format!("kpi-udp/{port}"), move || run(k));
    unsafe {
        *handle = h;
        *bound = port;
    }
    0
}

/// Queue `data` for `daddr:dport` (`saddr` is RustOS's choice). Any
/// context. 0 or -errno.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_udp_send(
    handle: u64,
    _saddr: u32,
    daddr: u32,
    dport: u16,
    data: *const u8,
    len: u32,
) -> c_int {
    let Some(k) = SOCKETS.lock().get(&handle).cloned() else {
        return -ENOTCONN.0;
    };
    // SAFETY: the caller passes a buffer of `len` bytes.
    let d = unsafe { core::slice::from_raw_parts(data, len as usize) }.to_vec();
    let ep = IpEndpoint::new(IpAddress::Ipv4(ipv4(daddr)), dport);
    {
        let mut q = k.tx.lock();
        if q.len() >= TX_QUEUE_MAX {
            return -ENOBUFS.0;
        }
        q.push_back((ep, d));
    }
    k.tx_gen.fetch_add(1, Ordering::SeqCst);
    SOCK_WQ.wake_all();
    0
}

/// Close the socket: its thread stops (no callback runs after this
/// returns) and the port is released.
#[unsafe(no_mangle)]
extern "C" fn rustos_kpi_udp_close(handle: u64) {
    let Some(k) = SOCKETS.lock().remove(&handle) else {
        return;
    };
    k.closing.store(true, Ordering::SeqCst);
    SOCK_WQ.wake_all();
    while !k.done.load(Ordering::SeqCst) {
        crate::time::sleep_ms(1);
    }
    k.sock.close();
}
