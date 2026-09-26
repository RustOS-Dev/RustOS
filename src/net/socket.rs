//! BSD sockets over smoltcp: TCP, UDP and ICMP echo ("ping") sockets for
//! IPv4 and IPv6.
//!
//! Every smoltcp socket lives in one interface's socket set; a [`Slot`]
//! names it. Connected TCP sockets have one slot on the interface their
//! peer is routed through. Wildcard TCP listeners and bound datagram
//! sockets get a slot on every interface (added lazily as interfaces
//! appear).

use super::{EPOCH, Net, SOCK_WQ, kick, with};
use crate::errno::*;
use crate::vfs::{FileLike, FileType, Metadata, POLLERR, POLLHUP, POLLIN, POLLOUT};
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt::Write;
use core::sync::atomic::{AtomicU16, Ordering};
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{AnySocket, icmp, tcp, udp};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint};

pub const AF_UNIX: u16 = 1;
pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;

const TCP_BUF: usize = 64 * 1024;
const UDP_PACKETS: usize = 64;
const UDP_BUF: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
    /// ICMP echo socket; `raw` sockets get an IPv4 header on receive.
    Icmp {
        raw: bool,
    },
}

/// A smoltcp socket in a particular interface's socket set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    ifc: u32,
    h: SocketHandle,
}

fn sock<T: AnySocket<'static>>(net: &mut Net, s: Slot) -> Option<&mut T> {
    net.by_index(s.ifc).map(|i| i.sockets.get_mut::<T>(s.h))
}

#[derive(Default)]
struct Inner {
    /// TCP connection.
    conn: Option<Slot>,
    /// TCP listening sockets (backlog), across interfaces.
    listeners: Vec<Slot>,
    backlog_len: usize,
    /// UDP / ICMP sockets, one per interface.
    dgram: Vec<Slot>,
    bound: Option<IpListenEndpoint>,
    owns_port: bool,
    remote: Option<IpEndpoint>,
    rcvtimeo: Option<u64>,
    sndtimeo: Option<u64>,
    nodelay: bool,
    keepalive: bool,
    shut_rd: bool,
    shut_wr: bool,
    ident: u16,
    connecting: bool,
    established: bool,
    error: Option<Errno>,
    ttl: Option<u8>,
    /// Round-robin start for datagram receive.
    rr: usize,
}

pub struct Socket {
    pub family: u16,
    pub proto: Proto,
    inner: spin::Mutex<Inner>,
}

static NEXT_PORT: AtomicU16 = AtomicU16::new(49152);
/// Bound (protocol, port) pairs.
static PORTS: spin::Mutex<BTreeSet<(u8, u16)>> = spin::Mutex::new(BTreeSet::new());

fn proto_id(p: Proto) -> u8 {
    match p {
        Proto::Tcp => 6,
        Proto::Udp => 17,
        Proto::Icmp { .. } => 1,
    }
}

fn ephemeral(p: Proto) -> u16 {
    let mut ports = PORTS.lock();
    loop {
        let mut n = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
        if n < 49152 {
            NEXT_PORT.store(49153, Ordering::Relaxed);
            n = 49152;
        }
        if ports.insert((proto_id(p), n)) {
            return n;
        }
    }
}

fn tcp_socket() -> tcp::Socket<'static> {
    tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; TCP_BUF]),
        tcp::SocketBuffer::new(vec![0; TCP_BUF]),
    )
}

fn udp_socket() -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; UDP_PACKETS],
            vec![0; UDP_BUF],
        ),
        udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; UDP_PACKETS],
            vec![0; UDP_BUF],
        ),
    )
}

fn icmp_socket() -> icmp::Socket<'static> {
    icmp::Socket::new(
        icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 16], vec![0; 16 * 1024]),
        icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 16], vec![0; 16 * 1024]),
    )
}

enum Step<T> {
    Ready(KResult<T>),
    Pending,
}
use Step::*;

fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    for c in data.chunks(2) {
        let w = if c.len() == 2 {
            u16::from_be_bytes([c[0], c[1]])
        } else {
            u16::from_be_bytes([c[0], 0])
        };
        sum += w as u32;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn listening_state(s: tcp::State) -> bool {
    matches!(s, tcp::State::Listen | tcp::State::SynReceived)
}

impl Socket {
    pub fn new(family: u16, proto: Proto) -> Arc<Socket> {
        Arc::new(Socket {
            family,
            proto,
            inner: spin::Mutex::new(Inner {
                backlog_len: 1,
                ..Default::default()
            }),
        })
    }

    fn unspecified(&self) -> IpAddress {
        if self.family == AF_INET6 {
            IpAddress::Ipv6(smoltcp::wire::Ipv6Address::UNSPECIFIED)
        } else {
            IpAddress::v4(0, 0, 0, 0)
        }
    }

    /// Run `f` until it is ready, blocking between network polls.
    fn block<T>(
        &self,
        nonblock: bool,
        timeout: Option<u64>,
        mut f: impl FnMut(&mut Net, &mut Inner) -> Step<T>,
    ) -> KResult<T> {
        let deadline = timeout.map(|t| crate::time::millis() + t);
        loop {
            let epoch = EPOCH.load(Ordering::SeqCst);
            let step = with(|net| {
                let mut st = self.inner.lock();
                f(net, &mut st)
            })
            .ok_or(ENETDOWN)?;
            if let Ready(r) = step {
                kick();
                return r;
            }
            if nonblock {
                return Err(EAGAIN);
            }
            if deadline.is_some_and(|d| crate::time::millis() >= d) {
                return Err(EAGAIN);
            }
            if crate::process::signal::has_pending() {
                return Err(EINTR);
            }
            kick();
            SOCK_WQ.wait_timeout(50, || EPOCH.load(Ordering::SeqCst) != epoch);
        }
    }

    // ------------------------------------------------------------------
    // Binding and per-interface slots
    // ------------------------------------------------------------------

    pub fn bind(&self, ep: IpEndpoint) -> KResult<()> {
        if !ep.addr.is_unspecified() && with(|net| net.owner_of(&ep.addr)).flatten().is_none() {
            return Err(EADDRNOTAVAIL);
        }
        let mut st = self.inner.lock();
        if st.bound.is_some() {
            return Err(EINVAL);
        }
        let port = if ep.port == 0 {
            ephemeral(self.proto)
        } else {
            if !PORTS.lock().insert((proto_id(self.proto), ep.port)) {
                return Err(EADDRINUSE);
            }
            ep.port
        };
        let addr = (!ep.addr.is_unspecified()).then_some(ep.addr);
        st.bound = Some(IpListenEndpoint { addr, port });
        st.owns_port = true;
        if matches!(self.proto, Proto::Icmp { .. }) {
            st.ident = port;
        }
        Ok(())
    }

    fn ensure_bound(&self, st: &mut Inner) -> IpListenEndpoint {
        if st.bound.is_none() {
            let port = ephemeral(self.proto);
            st.bound = Some(IpListenEndpoint { addr: None, port });
            st.owns_port = true;
            if matches!(self.proto, Proto::Icmp { .. }) {
                st.ident = port;
            }
        }
        st.bound.unwrap()
    }

    /// Interfaces a bound socket should be present on.
    fn target_ifaces(net: &Net, st: &Inner) -> Vec<u32> {
        match st.bound.and_then(|b| b.addr) {
            Some(a) => net.owner_of(&a).into_iter().collect(),
            None => net.up_indices(),
        }
    }

    /// Drop slots whose interface disappeared.
    fn prune(net: &mut Net, slots: &mut Vec<Slot>) {
        slots.retain(|s| net.by_index(s.ifc).is_some());
    }

    /// The datagram socket on interface `ifc`, creating it if needed.
    fn dgram_slot(&self, net: &mut Net, st: &mut Inner, ifc: u32) -> KResult<Slot> {
        if let Some(s) = st.dgram.iter().find(|s| s.ifc == ifc) {
            return Ok(*s);
        }
        let ep = self.ensure_bound(st);
        let ttl = st.ttl;
        let ident = st.ident;
        let iface = net.by_index(ifc).ok_or(ENETDOWN)?;
        let h = match self.proto {
            Proto::Udp => {
                let mut s = udp_socket();
                s.bind(ep).map_err(|_| EADDRINUSE)?;
                s.set_hop_limit(ttl);
                iface.sockets.add(s)
            }
            Proto::Icmp { .. } => {
                let mut s = icmp_socket();
                s.bind(icmp::Endpoint::Ident(ident))
                    .map_err(|_| EADDRINUSE)?;
                s.set_hop_limit(ttl);
                iface.sockets.add(s)
            }
            Proto::Tcp => return Err(EINVAL),
        };
        let slot = Slot { ifc, h };
        st.dgram.push(slot);
        Ok(slot)
    }

    /// Make sure a bound datagram socket listens on every interface.
    fn ensure_dgram_all(&self, net: &mut Net, st: &mut Inner) {
        Self::prune(net, &mut st.dgram);
        for ifc in Self::target_ifaces(net, st) {
            let _ = self.dgram_slot(net, st, ifc);
        }
    }

    /// Keep `backlog_len` listening sockets on every interface.
    fn ensure_listeners(&self, net: &mut Net, st: &mut Inner) {
        Self::prune(net, &mut st.listeners);
        let Some(ep) = st.bound else { return };
        for ifc in Self::target_ifaces(net, st) {
            let have = st.listeners.iter().filter(|s| s.ifc == ifc).count();
            for _ in have..st.backlog_len {
                let Some(iface) = net.by_index(ifc) else {
                    break;
                };
                let mut s = tcp_socket();
                if s.listen(ep).is_err() {
                    break;
                }
                s.set_nagle_enabled(!st.nodelay);
                let h = iface.sockets.add(s);
                st.listeners.push(Slot { ifc, h });
            }
        }
    }

    /// Interface for traffic to `dest`: the bound address's owner, or the
    /// route.
    fn egress_iface(net: &Net, st: &Inner, dest: &IpAddress) -> KResult<u32> {
        if let Some(a) = st.bound.and_then(|b| b.addr) {
            return net.owner_of(&a).ok_or(EADDRNOTAVAIL);
        }
        net.route(dest).ok_or(ENETUNREACH)
    }

    // ------------------------------------------------------------------
    // TCP connection setup
    // ------------------------------------------------------------------

    pub fn listen(&self, backlog: usize) -> KResult<()> {
        if self.proto != Proto::Tcp {
            return Err(EOPNOTSUPP);
        }
        with(|net| {
            let mut st = self.inner.lock();
            if st.conn.is_some() {
                return Err(EINVAL);
            }
            self.ensure_bound(&mut st);
            st.backlog_len = backlog.clamp(1, 16);
            self.ensure_listeners(net, &mut st);
            if st.listeners.is_empty() {
                return Err(EADDRINUSE);
            }
            Ok(())
        })
        .ok_or(ENETDOWN)?
    }

    pub fn is_listening(&self) -> bool {
        !self.inner.lock().listeners.is_empty()
    }

    pub fn accept(&self, nonblock: bool) -> KResult<Arc<Socket>> {
        let timeout = {
            let st = self.inner.lock();
            if st.listeners.is_empty() {
                return Err(EINVAL);
            }
            st.rcvtimeo
        };
        let family = self.family;
        self.block(nonblock, timeout, |net, st| {
            self.ensure_listeners(net, st);
            let ready = st.listeners.iter().position(|&s| {
                sock::<tcp::Socket>(net, s).is_some_and(|t| !listening_state(t.state()))
            });
            let Some(i) = ready else { return Pending };
            let slot = st.listeners.remove(i);
            self.ensure_listeners(net, st);
            let Some(t) = sock::<tcp::Socket>(net, slot) else {
                return Pending;
            };
            let child = Socket::new(family, Proto::Tcp);
            {
                let mut c = child.inner.lock();
                c.conn = Some(slot);
                c.remote = t.remote_endpoint();
                c.bound = t.local_endpoint().map(|e| e.into());
                c.established = true;
                c.nodelay = st.nodelay;
            }
            Ready(Ok(child))
        })
    }

    pub fn connect(&self, remote: IpEndpoint, nonblock: bool) -> KResult<()> {
        if self.proto != Proto::Tcp {
            // Datagram "connect": remember the default peer.
            let mut st = self.inner.lock();
            st.remote = (!remote.addr.is_unspecified()).then_some(remote);
            return Ok(());
        }
        {
            let st = self.inner.lock();
            if st.established {
                return Err(EISCONN);
            }
            if st.connecting && nonblock {
                return Err(EALREADY);
            }
        }
        with(|net| -> KResult<()> {
            let mut st = self.inner.lock();
            if st.connecting {
                return Ok(());
            }
            if !st.listeners.is_empty() {
                return Err(EINVAL);
            }
            let ifc = Self::egress_iface(net, &st, &remote.addr)?;
            let local = self.ensure_bound(&mut st);
            let mut s = tcp_socket();
            s.set_nagle_enabled(!st.nodelay);
            if st.keepalive {
                s.set_keep_alive(Some(smoltcp::time::Duration::from_secs(75)));
            }
            s.set_timeout(Some(smoltcp::time::Duration::from_secs(75)));
            s.set_hop_limit(st.ttl);
            let iface = net.by_index(ifc).ok_or(ENETDOWN)?;
            s.connect(iface.iface.context(), remote, local)
                .map_err(|e| match e {
                    tcp::ConnectError::Unaddressable => EADDRNOTAVAIL,
                    tcp::ConnectError::InvalidState => EISCONN,
                })?;
            let h = iface.sockets.add(s);
            st.conn = Some(Slot { ifc, h });
            st.remote = Some(remote);
            st.connecting = true;
            Ok(())
        })
        .ok_or(ENETDOWN)??;
        kick();
        if nonblock {
            return Err(EINPROGRESS);
        }
        self.block(false, None, |net, st| {
            self.update_connect(net, st);
            if st.connecting {
                Pending
            } else if st.established {
                Ready(Ok(()))
            } else {
                Ready(Err(st.error.take().unwrap_or(ECONNREFUSED)))
            }
        })
    }

    /// Progress a pending connect (for blocking connect, poll, SO_ERROR).
    fn update_connect(&self, net: &mut Net, st: &mut Inner) {
        if !st.connecting {
            return;
        }
        let state = st
            .conn
            .and_then(|s| sock::<tcp::Socket>(net, s))
            .map(|t| t.state());
        match state {
            Some(tcp::State::SynSent | tcp::State::SynReceived) => {}
            Some(tcp::State::Closed | tcp::State::TimeWait) | None => {
                st.connecting = false;
                st.error = Some(if state.is_none() {
                    ENETDOWN
                } else {
                    ECONNREFUSED
                });
            }
            Some(_) => {
                st.connecting = false;
                st.established = true;
            }
        }
    }

    // ------------------------------------------------------------------
    // Data
    // ------------------------------------------------------------------

    fn tcp_send(&self, buf: &[u8], nonblock: bool, timeout: Option<u64>) -> KResult<usize> {
        let mut done = 0;
        loop {
            let r = self.block(nonblock, timeout, |net, st| {
                if st.shut_wr {
                    return Ready(Err(EPIPE));
                }
                self.update_connect(net, st);
                if st.connecting {
                    return Pending;
                }
                let Some(slot) = st.conn else {
                    return Ready(Err(ENOTCONN));
                };
                let Some(s) = sock::<tcp::Socket>(net, slot) else {
                    return Ready(Err(ENETDOWN));
                };
                if !s.may_send() {
                    return Ready(Err(EPIPE));
                }
                if s.can_send() {
                    return Ready(s.send_slice(&buf[done..]).map_err(|_| EPIPE));
                }
                Pending
            });
            match r {
                Ok(n) => done += n,
                Err(EPIPE) if done == 0 => {
                    crate::process::signal::send_to_current(crate::process::signal::SIGPIPE);
                    return Err(EPIPE);
                }
                Err(_) if done > 0 => return Ok(done),
                Err(e) => return Err(e),
            }
            if done >= buf.len() || nonblock {
                return Ok(done);
            }
        }
    }

    pub fn send_to(&self, buf: &[u8], dest: Option<IpEndpoint>, nonblock: bool) -> KResult<usize> {
        let timeout = self.inner.lock().sndtimeo;
        if self.proto == Proto::Tcp {
            return self.tcp_send(buf, nonblock, timeout);
        }
        let dest = dest.or(self.inner.lock().remote).ok_or(EDESTADDRREQ)?;
        let icmp = matches!(self.proto, Proto::Icmp { .. });
        if !icmp && dest.port == 0 {
            return Err(EINVAL);
        }
        if icmp && buf.len() < 8 {
            return Err(EINVAL);
        }
        self.block(nonblock, timeout, |net, st| {
            let ifc = match Self::egress_iface(net, st, &dest.addr) {
                Ok(i) => i,
                Err(e) => return Ready(Err(e)),
            };
            let slot = match self.dgram_slot(net, st, ifc) {
                Ok(s) => s,
                Err(e) => return Ready(Err(e)),
            };
            if icmp {
                let ident = st.ident;
                let Some(s) = sock::<icmp::Socket>(net, slot) else {
                    return Ready(Err(ENETDOWN));
                };
                if !s.can_send() {
                    return Pending;
                }
                // Kernel-owned echo identifier, fresh checksum (ICMPv4).
                let mut pkt = buf.to_vec();
                pkt[4..6].copy_from_slice(&ident.to_be_bytes());
                if matches!(dest.addr, IpAddress::Ipv4(_)) {
                    pkt[2] = 0;
                    pkt[3] = 0;
                    let c = checksum(&pkt);
                    pkt[2..4].copy_from_slice(&c.to_be_bytes());
                }
                return match s.send_slice(&pkt, dest.addr) {
                    Ok(()) => Ready(Ok(buf.len())),
                    Err(icmp::SendError::BufferFull) => Pending,
                    Err(_) => Ready(Err(EINVAL)),
                };
            }
            let Some(s) = sock::<udp::Socket>(net, slot) else {
                return Ready(Err(ENETDOWN));
            };
            if buf.len() > s.payload_send_capacity() {
                return Ready(Err(EMSGSIZE));
            }
            match s.send_slice(buf, dest) {
                Ok(()) => Ready(Ok(buf.len())),
                Err(udp::SendError::BufferFull) => Pending,
                Err(udp::SendError::Unaddressable) => Ready(Err(EINVAL)),
            }
        })
    }

    /// Receive into `buf`; returns (bytes, source endpoint).
    pub fn recv_from(
        &self,
        buf: &mut [u8],
        nonblock: bool,
        peek: bool,
    ) -> KResult<(usize, Option<IpEndpoint>)> {
        let timeout = self.inner.lock().rcvtimeo;
        match self.proto {
            Proto::Tcp => self.block(nonblock, timeout, |net, st| {
                if st.shut_rd {
                    return Ready(Ok((0, None)));
                }
                self.update_connect(net, st);
                if st.connecting {
                    return Pending;
                }
                let Some(slot) = st.conn else {
                    return Ready(Err(ENOTCONN));
                };
                let remote = st.remote;
                let Some(s) = sock::<tcp::Socket>(net, slot) else {
                    return Ready(Err(ENETDOWN));
                };
                if s.can_recv() {
                    let r = if peek {
                        s.peek_slice(buf)
                    } else {
                        s.recv_slice(buf)
                    };
                    return Ready(r.map(|n| (n, remote)).map_err(|_| ECONNRESET));
                }
                if !s.may_recv() {
                    return Ready(Ok((0, remote)));
                }
                Pending
            }),
            Proto::Udp | Proto::Icmp { .. } => self.block(nonblock, timeout, |net, st| {
                self.ensure_dgram_all(net, st);
                let n_slots = st.dgram.len();
                let filter = st.remote;
                for k in 0..n_slots {
                    let slot = st.dgram[(st.rr + k) % n_slots];
                    let got = match self.proto {
                        Proto::Udp => {
                            let Some(s) = sock::<udp::Socket>(net, slot) else {
                                continue;
                            };
                            recv_udp(s, buf, peek, filter)
                        }
                        Proto::Icmp { raw } => {
                            let Some(s) = sock::<icmp::Socket>(net, slot) else {
                                continue;
                            };
                            recv_icmp(s, buf, raw)
                        }
                        Proto::Tcp => None,
                    };
                    if let Some(r) = got {
                        st.rr = (st.rr + k + 1) % n_slots.max(1);
                        return Ready(Ok(r));
                    }
                }
                Pending
            }),
        }
    }

    pub fn shutdown(&self, how: u32) -> KResult<()> {
        with(|net| {
            let mut st = self.inner.lock();
            if how == 0 || how == 2 {
                st.shut_rd = true;
            }
            if (how == 1 || how == 2) && !st.shut_wr {
                st.shut_wr = true;
                if let Some(slot) = st.conn
                    && let Some(s) = sock::<tcp::Socket>(net, slot)
                {
                    s.close();
                }
            }
        });
        kick();
        Ok(())
    }

    pub fn local_endpoint(&self) -> IpEndpoint {
        let unspec = self.unspecified();
        with(|net| {
            let st = self.inner.lock();
            if let Some(slot) = st.conn
                && let Some(t) = sock::<tcp::Socket>(net, slot)
                && let Some(ep) = t.local_endpoint()
            {
                return ep;
            }
            match st.bound {
                Some(b) => IpEndpoint::new(b.addr.unwrap_or(unspec), b.port),
                None => IpEndpoint::new(unspec, 0),
            }
        })
        .unwrap_or(IpEndpoint::new(unspec, 0))
    }

    pub fn peer_endpoint(&self) -> Option<IpEndpoint> {
        let st = self.inner.lock();
        if self.proto == Proto::Tcp && !st.established && !st.connecting {
            return None;
        }
        st.remote
    }

    // ------------------------------------------------------------------
    // Options
    // ------------------------------------------------------------------

    pub fn set_timeout(&self, recv: bool, ms: Option<u64>) {
        let mut st = self.inner.lock();
        if recv {
            st.rcvtimeo = ms;
        } else {
            st.sndtimeo = ms;
        }
    }

    pub fn set_nodelay(&self, on: bool) {
        with(|net| {
            let mut st = self.inner.lock();
            st.nodelay = on;
            if let Some(slot) = st.conn
                && let Some(s) = sock::<tcp::Socket>(net, slot)
            {
                s.set_nagle_enabled(!on);
            }
        });
    }

    pub fn set_keepalive(&self, on: bool) {
        with(|net| {
            let mut st = self.inner.lock();
            st.keepalive = on;
            if let Some(slot) = st.conn
                && let Some(s) = sock::<tcp::Socket>(net, slot)
            {
                s.set_keep_alive(on.then(|| smoltcp::time::Duration::from_secs(75)));
            }
        });
    }

    pub fn set_ttl(&self, ttl: u8) {
        self.inner.lock().ttl = Some(ttl);
    }

    pub fn nodelay(&self) -> bool {
        self.inner.lock().nodelay
    }

    /// SO_ERROR: pending error (cleared on read).
    pub fn take_error(&self) -> i32 {
        with(|net| {
            let mut st = self.inner.lock();
            self.update_connect(net, &mut st);
            st.error.take().map_or(0, |e| e.0)
        })
        .unwrap_or(0)
    }

    pub fn recv_queue(&self) -> usize {
        with(|net| {
            let st = self.inner.lock();
            if let Some(slot) = st.conn {
                return sock::<tcp::Socket>(net, slot).map_or(0, |s| s.recv_queue());
            }
            let mut n = 0;
            for &slot in &st.dgram {
                n += match self.proto {
                    Proto::Udp => sock::<udp::Socket>(net, slot).map_or(0, |s| s.recv_queue()),
                    _ => sock::<icmp::Socket>(net, slot).map_or(0, |s| s.recv_queue()),
                };
            }
            n
        })
        .unwrap_or(0)
    }
}

fn recv_udp(
    s: &mut udp::Socket<'static>,
    buf: &mut [u8],
    peek: bool,
    filter: Option<IpEndpoint>,
) -> Option<(usize, Option<IpEndpoint>)> {
    loop {
        let (data, ep) = if peek {
            let (d, m) = s.peek().ok()?;
            (d, m.endpoint)
        } else {
            let (d, m) = s.recv().ok()?;
            (d, m.endpoint)
        };
        if filter.is_some_and(|f| f != ep) {
            if peek {
                let _ = s.recv();
            }
            continue;
        }
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        return Some((n, Some(ep)));
    }
}

fn recv_icmp(
    s: &mut icmp::Socket<'static>,
    buf: &mut [u8],
    raw: bool,
) -> Option<(usize, Option<IpEndpoint>)> {
    let (data, addr) = s.recv().ok()?;
    let mut pkt = Vec::with_capacity(data.len() + 20);
    if raw && let IpAddress::Ipv4(src) = addr {
        let total = (data.len() + 20) as u16;
        pkt.extend_from_slice(&[0x45, 0]);
        pkt.extend_from_slice(&total.to_be_bytes());
        pkt.extend_from_slice(&[0, 0, 0, 0, 64, 1, 0, 0]);
        pkt.extend_from_slice(&src.octets());
        pkt.extend_from_slice(&[0, 0, 0, 0]);
    }
    pkt.extend_from_slice(data);
    let n = pkt.len().min(buf.len());
    buf[..n].copy_from_slice(&pkt[..n]);
    Some((n, Some(IpEndpoint::new(addr, 0))))
}

impl FileLike for Socket {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        self.recv_from(buf, nonblock, false).map(|(n, _)| n)
    }

    fn write(&self, buf: &[u8], nonblock: bool) -> KResult<usize> {
        self.send_to(buf, None, nonblock)
    }

    fn poll(&self) -> u16 {
        with(|net| {
            let mut st = self.inner.lock();
            self.update_connect(net, &mut st);
            if !st.listeners.is_empty() {
                self.ensure_listeners(net, &mut st);
                let ready = st.listeners.iter().any(|&s| {
                    sock::<tcp::Socket>(net, s).is_some_and(|t| !listening_state(t.state()))
                });
                return if ready { POLLIN } else { 0 };
            }
            let mut ev = 0;
            if st.error.is_some() {
                ev |= POLLERR;
            }
            match self.proto {
                Proto::Tcp => {
                    let Some(slot) = st.conn else {
                        return if st.error.is_some() {
                            POLLERR | POLLHUP
                        } else {
                            POLLOUT
                        };
                    };
                    let Some(s) = sock::<tcp::Socket>(net, slot) else {
                        return POLLERR | POLLHUP;
                    };
                    if st.connecting {
                        return ev;
                    }
                    if s.can_recv() || !s.may_recv() || st.shut_rd {
                        ev |= POLLIN;
                    }
                    if s.can_send() {
                        ev |= POLLOUT;
                    }
                    if !s.may_recv() && !s.may_send() {
                        ev |= POLLHUP;
                    }
                }
                _ => {
                    ev |= POLLOUT;
                    if st.bound.is_some() {
                        self.ensure_dgram_all(net, &mut st);
                    }
                    for &slot in &st.dgram {
                        let readable = match self.proto {
                            Proto::Udp => {
                                sock::<udp::Socket>(net, slot).is_some_and(|s| s.can_recv())
                            }
                            _ => sock::<icmp::Socket>(net, slot).is_some_and(|s| s.can_recv()),
                        };
                        if readable {
                            ev |= POLLIN;
                        }
                    }
                }
            }
            ev
        })
        .unwrap_or(POLLERR)
    }

    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        const FIONREAD: u64 = 0x541B;
        if cmd == FIONREAD {
            let n = self.recv_queue() as i32;
            crate::process::uaccess::write_user(arg, &n)?;
            return Ok(0);
        }
        super::syscalls::if_ioctl(cmd, arg)
    }

    fn stat(&self) -> KResult<Metadata> {
        Ok(Metadata::new(FileType::Socket, 0o777))
    }

    fn close(&self) {
        with(|net| {
            let mut st = self.inner.lock();
            for slot in st.listeners.drain(..) {
                if let Some(i) = net.by_index(slot.ifc) {
                    i.sockets.get_mut::<tcp::Socket>(slot.h).abort();
                    i.sockets.remove(slot.h);
                }
            }
            if let Some(slot) = st.conn.take()
                && let Some(i) = net.by_index(slot.ifc)
            {
                i.sockets.get_mut::<tcp::Socket>(slot.h).close();
                i.retire(slot.h);
            }
            for slot in st.dgram.drain(..) {
                if let Some(i) = net.by_index(slot.ifc) {
                    i.sockets.remove(slot.h);
                }
            }
            // Accepted children share the listener's port; only the owner
            // of the binding releases it.
            if let Some(b) = st.bound.take()
                && st.owns_port
            {
                PORTS.lock().remove(&(proto_id(self.proto), b.port));
            }
        });
        kick();
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn hex_addr(a: &IpAddress) -> String {
    match a {
        IpAddress::Ipv4(v) => format!("{:08X}", u32::from_le_bytes(v.octets())),
        IpAddress::Ipv6(v) => {
            let mut s = String::new();
            for c in v.octets().chunks(4) {
                let _ = write!(s, "{:08X}", u32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
            s
        }
    }
}

fn tcp_state_code(s: tcp::State) -> u8 {
    match s {
        tcp::State::Established => 1,
        tcp::State::SynSent => 2,
        tcp::State::SynReceived => 3,
        tcp::State::FinWait1 => 4,
        tcp::State::FinWait2 => 5,
        tcp::State::TimeWait => 6,
        tcp::State::Closed => 7,
        tcp::State::CloseWait => 8,
        tcp::State::LastAck => 9,
        tcp::State::Listen => 10,
        tcp::State::Closing => 11,
    }
}

/// /proc/net/tcp and /proc/net/udp (Linux layout). Wildcard listeners
/// appear once even though they have a socket per interface.
pub fn gen_sockets(tcp_only: bool) -> String {
    let mut s = String::from(
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n",
    );
    let any = IpAddress::v4(0, 0, 0, 0);
    let mut lines: BTreeSet<String> = BTreeSet::new();
    with(|net| {
        for ifc in &net.ifaces {
            for (_, sock) in ifc.sockets.iter() {
                let (local, remote, st, txq, rxq) = match sock {
                    smoltcp::socket::Socket::Tcp(t) if tcp_only => {
                        let l = t.local_endpoint().unwrap_or_else(|| {
                            let le = t.listen_endpoint();
                            IpEndpoint::new(le.addr.unwrap_or(any), le.port)
                        });
                        let r = t.remote_endpoint().unwrap_or(IpEndpoint::new(any, 0));
                        (
                            l,
                            r,
                            tcp_state_code(t.state()),
                            t.send_queue(),
                            t.recv_queue(),
                        )
                    }
                    smoltcp::socket::Socket::Udp(u) if !tcp_only => {
                        let le = u.endpoint();
                        let l = IpEndpoint::new(le.addr.unwrap_or(any), le.port);
                        (
                            l,
                            IpEndpoint::new(any, 0),
                            7,
                            u.send_queue(),
                            u.recv_queue(),
                        )
                    }
                    _ => continue,
                };
                lines.insert(format!(
                    "{}:{:04X} {}:{:04X} {:02X} {:08X}:{:08X} 00:00000000 00000000     0        0 0",
                    hex_addr(&local.addr),
                    local.port,
                    hex_addr(&remote.addr),
                    remote.port,
                    st,
                    txq,
                    rxq
                ));
            }
        }
    });
    for (n, l) in lines.iter().enumerate() {
        let _ = writeln!(s, "{:4}: {}", n, l);
    }
    s
}
