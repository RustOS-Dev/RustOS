//! AF_NETLINK sockets. User messages go to a kernel-side handler per
//! protocol (`register_kernel`): rtnetlink (`rtnetlink.rs`) for
//! NETLINK_ROUTE, Linux's generic netlink through LinuxKPI for
//! NETLINK_GENERIC. Handlers answer with `unicast`/`multicast`.

use super::generic::{Ancillary, GenericSocket, Received};
use crate::errno::*;
use crate::sched::WaitQueue;
use crate::sync::Mutex;
use crate::vfs::{FileLike, FileType, Metadata, POLLIN, POLLOUT};
use alloc::collections::{BTreeSet, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const AF_NETLINK: u16 = 16;
pub const NETLINK_ROUTE: u32 = 0;
pub const NETLINK_KOBJECT_UEVENT: u32 = 15;
pub const NETLINK_GENERIC: u32 = 16;
const MAX_PROTO: usize = 32;

const SOL_NETLINK: u32 = 270;
const NETLINK_ADD_MEMBERSHIP: u32 = 1;
const NETLINK_DROP_MEMBERSHIP: u32 = 2;

/// Datagrams queued per socket; more are dropped (the reader sees
/// ENOBUFS once, as on Linux).
const RX_LIMIT: usize = 4096;

/// Handles a datagram a user socket (`portid`) sent to the kernel side of
/// protocol `proto`.
pub type KernelInput = fn(proto: u32, portid: u32, data: &[u8]);
/// Hears that user socket `portid` of protocol `proto` closed.
pub type KernelRelease = fn(proto: u32, portid: u32);

static HANDLERS: Mutex<[Option<KernelInput>; MAX_PROTO]> = Mutex::new([None; MAX_PROTO]);
static RELEASE: Mutex<[Option<KernelRelease>; MAX_PROTO]> = Mutex::new([None; MAX_PROTO]);
static SOCKETS: Mutex<Vec<Weak<NetlinkSocket>>> = Mutex::new(Vec::new());
static NEXT_PORT: AtomicU32 = AtomicU32::new(0x8000_0000);

/// Set (or with `None`, remove) the kernel side of `proto`.
pub fn register_kernel(proto: u32, f: Option<KernelInput>, release: Option<KernelRelease>) {
    if (proto as usize) < MAX_PROTO {
        HANDLERS.lock()[proto as usize] = f;
        RELEASE.lock()[proto as usize] = release;
    }
}

pub fn has_kernel(proto: u32) -> bool {
    HANDLERS
        .lock()
        .get(proto as usize)
        .is_some_and(|h| h.is_some())
}

fn sockets(proto: u32) -> Vec<Arc<NetlinkSocket>> {
    let mut list = SOCKETS.lock();
    list.retain(|w| w.strong_count() > 0);
    list.iter()
        .filter_map(|w| w.upgrade())
        .filter(|s| s.proto == proto)
        .collect()
}

/// Deliver `data` to the socket bound to `portid`. ECONNREFUSED if none.
pub fn unicast(proto: u32, portid: u32, data: Vec<u8>) -> KResult<()> {
    let s = sockets(proto)
        .into_iter()
        .find(|s| s.portid.load(Ordering::SeqCst) == portid)
        .ok_or(ECONNREFUSED)?;
    s.deliver(data);
    Ok(())
}

/// Deliver `data` to every socket in multicast group `group` (1-based).
pub fn multicast(proto: u32, group: u32, data: &[u8]) {
    multicast_except(proto, group, 0, data);
}

/// Multicast, skipping the socket bound to `exclude` (if not 0); returns
/// how many sockets got it.
pub fn multicast_except(proto: u32, group: u32, exclude: u32, data: &[u8]) -> usize {
    let mut n = 0;
    for s in sockets(proto) {
        if (exclude == 0 || s.portid.load(Ordering::SeqCst) != exclude)
            && s.groups.lock().contains(&group)
        {
            s.deliver(data.to_vec());
            n += 1;
        }
    }
    n
}

pub fn has_listeners(proto: u32, group: u32) -> bool {
    sockets(proto)
        .iter()
        .any(|s| s.groups.lock().contains(&group))
}

pub struct NetlinkSocket {
    proto: u32,
    ty: u32,
    portid: AtomicU32,
    groups: Mutex<BTreeSet<u32>>,
    rx: Mutex<VecDeque<Vec<u8>>>,
    overrun: AtomicBool,
    wq: WaitQueue,
}

impl NetlinkSocket {
    pub fn new(ty: u32, proto: u32) -> KResult<Arc<NetlinkSocket>> {
        if (proto as usize) >= MAX_PROTO {
            return Err(EPROTONOSUPPORT);
        }
        if proto != NETLINK_KOBJECT_UEVENT && !has_kernel(proto) {
            return Err(EPROTONOSUPPORT);
        }
        let s = Arc::new(NetlinkSocket {
            proto,
            ty,
            portid: AtomicU32::new(0),
            groups: Mutex::new(BTreeSet::new()),
            rx: Mutex::new(VecDeque::new()),
            overrun: AtomicBool::new(false),
            wq: WaitQueue::new(),
        });
        SOCKETS.lock().push(Arc::downgrade(&s));
        Ok(s)
    }

    fn deliver(&self, data: Vec<u8>) {
        {
            let mut rx = self.rx.lock();
            if rx.len() >= RX_LIMIT {
                self.overrun.store(true, Ordering::SeqCst);
                return;
            }
            rx.push_back(data);
        }
        self.wq.wake_all();
    }

    /// Assign a port id if the socket has none: the process id if free,
    /// else a unique one.
    fn autobind(&self) -> u32 {
        let cur = self.portid.load(Ordering::SeqCst);
        if cur != 0 {
            return cur;
        }
        let pid = crate::process::current().map(|p| p.pid).unwrap_or(0);
        let taken = |id: u32| {
            sockets(self.proto)
                .iter()
                .any(|s| s.portid.load(Ordering::SeqCst) == id)
        };
        let id = if pid != 0 && !taken(pid) {
            pid
        } else {
            NEXT_PORT.fetch_add(1, Ordering::SeqCst)
        };
        let _ = self
            .portid
            .compare_exchange(0, id, Ordering::SeqCst, Ordering::SeqCst);
        self.portid.load(Ordering::SeqCst)
    }

    fn addr(pid: u32, groups: u32) -> Vec<u8> {
        let mut v = AF_NETLINK.to_ne_bytes().to_vec();
        v.extend_from_slice(&[0, 0]);
        v.extend_from_slice(&pid.to_ne_bytes());
        v.extend_from_slice(&groups.to_ne_bytes());
        v
    }
}

impl Drop for NetlinkSocket {
    fn drop(&mut self) {
        let portid = *self.portid.get_mut();
        if portid == 0 {
            return;
        }
        let r = RELEASE.lock().get(self.proto as usize).copied().flatten();
        if let Some(f) = r {
            f(self.proto, portid);
        }
    }
}

impl GenericSocket for NetlinkSocket {
    fn sock_type(&self) -> u32 {
        self.ty
    }

    fn family(&self) -> u16 {
        AF_NETLINK
    }

    fn bind(&self, raw: &[u8]) -> KResult<()> {
        if raw.len() < 12 || u16::from_ne_bytes([raw[0], raw[1]]) != AF_NETLINK {
            return Err(EINVAL);
        }
        let pid = u32::from_ne_bytes(raw[4..8].try_into().unwrap());
        let groups = u32::from_ne_bytes(raw[8..12].try_into().unwrap());
        if pid != 0 {
            if sockets(self.proto)
                .iter()
                .any(|s| !core::ptr::eq(s.as_ref(), self) && s.portid.load(Ordering::SeqCst) == pid)
            {
                return Err(EADDRINUSE);
            }
            self.portid.store(pid, Ordering::SeqCst);
        } else {
            self.autobind();
        }
        let mut g = self.groups.lock();
        for bit in 0..32 {
            if groups & (1 << bit) != 0 {
                g.insert(bit + 1);
            }
        }
        Ok(())
    }

    fn connect(&self, _raw: &[u8], _nonblock: bool) -> KResult<()> {
        self.autobind();
        Ok(())
    }

    fn send(
        &self,
        data: &[u8],
        _dest: Option<&[u8]>,
        _anc: Ancillary,
        _nonblock: bool,
    ) -> KResult<usize> {
        let portid = self.autobind();
        let h = HANDLERS.lock()[self.proto as usize];
        match h {
            Some(f) => f(self.proto, portid, data),
            None => return Err(ECONNREFUSED),
        }
        Ok(data.len())
    }

    fn recv(&self, buf: &mut [u8], nonblock: bool, peek: bool) -> KResult<Received> {
        loop {
            if self.overrun.swap(false, Ordering::SeqCst) {
                return Err(ENOBUFS);
            }
            {
                let mut rx = self.rx.lock();
                if let Some(d) = rx.front() {
                    let n = d.len().min(buf.len());
                    buf[..n].copy_from_slice(&d[..n]);
                    let full = d.len();
                    if !peek {
                        rx.pop_front();
                    }
                    return Ok(Received {
                        len: n,
                        full_len: full,
                        from: Some(Self::addr(0, 0)),
                        ancillary: Ancillary::default(),
                    });
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            if !self.wq.wait_interruptible(|| !self.rx.lock().is_empty())
                && crate::process::signal::has_pending()
            {
                return Err(EINTR);
            }
        }
    }

    fn sockname(&self) -> Vec<u8> {
        let groups = self
            .groups
            .lock()
            .iter()
            .filter(|&&g| g <= 32)
            .fold(0u32, |m, &g| m | 1 << (g - 1));
        Self::addr(self.portid.load(Ordering::SeqCst), groups)
    }

    fn peername(&self) -> KResult<Vec<u8>> {
        Ok(Self::addr(0, 0))
    }

    fn setsockopt(&self, level: u32, name: u32, val: &[u8]) -> KResult<()> {
        if level != SOL_NETLINK {
            return Ok(());
        }
        let v = val
            .get(..4)
            .map(|b| u32::from_ne_bytes(b.try_into().unwrap()))
            .ok_or(EINVAL)?;
        match name {
            NETLINK_ADD_MEMBERSHIP => {
                self.autobind();
                self.groups.lock().insert(v);
            }
            NETLINK_DROP_MEMBERSHIP => {
                self.groups.lock().remove(&v);
            }
            // PKTINFO, BROADCAST_ERROR, NO_ENOBUFS, CAP_ACK, EXT_ACK,
            // GET_STRICT_CHK: accepted.
            _ => {}
        }
        Ok(())
    }

    fn getsockopt(&self, level: u32, _name: u32) -> KResult<Vec<u8>> {
        if level == SOL_NETLINK {
            return Ok(0i32.to_ne_bytes().to_vec());
        }
        Err(ENOPROTOOPT)
    }
}

impl FileLike for NetlinkSocket {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        Ok(self.recv(buf, nonblock, false)?.len)
    }

    fn write(&self, buf: &[u8], nonblock: bool) -> KResult<usize> {
        self.send(buf, None, Ancillary::default(), nonblock)
    }

    fn poll(&self) -> u16 {
        let mut ev = POLLOUT;
        if !self.rx.lock().is_empty() || self.overrun.load(Ordering::SeqCst) {
            ev |= POLLIN;
        }
        ev
    }

    fn wait_queue(&self) -> &WaitQueue {
        &self.wq
    }

    /// Interface ioctls work on any socket, as on Linux.
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        super::syscalls::if_ioctl(cmd, arg)
    }

    fn stat(&self) -> KResult<Metadata> {
        Ok(Metadata::new(FileType::Socket, 0o777))
    }

    fn as_socket(&self) -> Option<&dyn GenericSocket> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ------------------------------------------------------- message helpers

pub const NLMSG_HDRLEN: usize = 16;
pub const NLMSG_ERROR: u16 = 2;
pub const NLMSG_DONE: u16 = 3;
pub const NLM_F_REQUEST: u16 = 1;
pub const NLM_F_MULTI: u16 = 2;
pub const NLM_F_ACK: u16 = 4;
pub const NLM_F_DUMP: u16 = 0x300;

/// A netlink message header.
#[derive(Clone, Copy, Debug)]
pub struct NlHdr {
    pub len: u32,
    pub ty: u16,
    pub flags: u16,
    pub seq: u32,
    pub pid: u32,
}

/// Split a datagram into (header, payload) pairs.
pub fn messages(data: &[u8]) -> Vec<(NlHdr, &[u8])> {
    let mut out = Vec::new();
    let mut off = 0;
    while off + NLMSG_HDRLEN <= data.len() {
        let len = u32::from_ne_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        if len < NLMSG_HDRLEN || off + len > data.len() {
            break;
        }
        let h = NlHdr {
            len: len as u32,
            ty: u16::from_ne_bytes(data[off + 4..off + 6].try_into().unwrap()),
            flags: u16::from_ne_bytes(data[off + 6..off + 8].try_into().unwrap()),
            seq: u32::from_ne_bytes(data[off + 8..off + 12].try_into().unwrap()),
            pid: u32::from_ne_bytes(data[off + 12..off + 16].try_into().unwrap()),
        };
        out.push((h, &data[off + NLMSG_HDRLEN..off + len]));
        off += len.next_multiple_of(4);
    }
    out
}

/// Builds a netlink message with attributes.
pub struct NlMsg {
    pub buf: Vec<u8>,
}

impl NlMsg {
    pub fn new(ty: u16, flags: u16, seq: u32, pid: u32) -> NlMsg {
        let mut buf = Vec::with_capacity(256);
        buf.extend_from_slice(&0u32.to_ne_bytes());
        buf.extend_from_slice(&ty.to_ne_bytes());
        buf.extend_from_slice(&flags.to_ne_bytes());
        buf.extend_from_slice(&seq.to_ne_bytes());
        buf.extend_from_slice(&pid.to_ne_bytes());
        NlMsg { buf }
    }

    pub fn put(&mut self, data: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(data);
        self.pad();
        self
    }

    fn pad(&mut self) {
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
    }

    pub fn attr(&mut self, ty: u16, data: &[u8]) -> &mut Self {
        self.buf
            .extend_from_slice(&((4 + data.len()) as u16).to_ne_bytes());
        self.buf.extend_from_slice(&ty.to_ne_bytes());
        self.buf.extend_from_slice(data);
        self.pad();
        self
    }

    pub fn attr_u32(&mut self, ty: u16, v: u32) -> &mut Self {
        self.attr(ty, &v.to_ne_bytes())
    }

    pub fn attr_u8(&mut self, ty: u16, v: u8) -> &mut Self {
        self.attr(ty, &[v])
    }

    pub fn attr_str(&mut self, ty: u16, s: &str) -> &mut Self {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        self.attr(ty, &v)
    }

    pub fn finish(mut self) -> Vec<u8> {
        let len = self.buf.len() as u32;
        self.buf[..4].copy_from_slice(&len.to_ne_bytes());
        self.buf
    }
}

/// An NLMSG_ERROR message (error 0 = ack) answering `req`, for the
/// socket bound to `portid`.
pub fn error_msg(req: &NlHdr, err: i32, portid: u32) -> Vec<u8> {
    let mut m = NlMsg::new(NLMSG_ERROR, 0, req.seq, portid);
    m.put(&err.to_ne_bytes());
    // The original header (no payload: as with NETLINK_CAP_ACK).
    let mut h = Vec::with_capacity(16);
    h.extend_from_slice(&(NLMSG_HDRLEN as u32).to_ne_bytes());
    h.extend_from_slice(&req.ty.to_ne_bytes());
    h.extend_from_slice(&req.flags.to_ne_bytes());
    h.extend_from_slice(&req.seq.to_ne_bytes());
    h.extend_from_slice(&req.pid.to_ne_bytes());
    m.put(&h);
    m.finish()
}

/// NLMSG_DONE ending a dump, for the socket bound to `portid`.
pub fn done_msg(req: &NlHdr, portid: u32) -> Vec<u8> {
    let mut m = NlMsg::new(NLMSG_DONE, NLM_F_MULTI, req.seq, portid);
    m.put(&0i32.to_ne_bytes());
    m.finish()
}

/// Attributes of a message body (after its fixed header): (type, data).
pub fn attrs(data: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut off = 0;
    while off + 4 <= data.len() {
        let len = u16::from_ne_bytes([data[off], data[off + 1]]) as usize;
        let ty = u16::from_ne_bytes([data[off + 2], data[off + 3]]) & 0x3fff;
        if len < 4 || off + len > data.len() {
            break;
        }
        out.push((ty, &data[off + 4..off + len]));
        off += len.next_multiple_of(4);
    }
    out
}
