//! Sockets (IPv4/IPv6 TCP and UDP, ICMP echo) and name resolution.

use crate::sys::{self, nr};
use crate::{Error, Result};
use alloc::string::String;
use alloc::vec::Vec;

pub const AF_UNIX: u16 = 1;
pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
pub const SOCK_RAW: u32 = 3;
pub const SOCK_NONBLOCK: u32 = 0o4000;
pub const SOCK_CLOEXEC: u32 = 0o2000000;
pub const IPPROTO_ICMP: u32 = 1;
pub const IPPROTO_TCP: u32 = 6;
pub const IPPROTO_UDP: u32 = 17;
pub const SOL_SOCKET: u32 = 1;
pub const SO_REUSEADDR: u32 = 2;
pub const SO_BROADCAST: u32 = 6;
pub const SO_PEERCRED: u32 = 17;
pub const SO_RCVTIMEO: u32 = 20;
pub const SO_SNDTIMEO: u32 = 21;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4(pub [u8; 4]);

impl Ipv4 {
    pub const ANY: Ipv4 = Ipv4([0, 0, 0, 0]);
    pub fn parse(s: &str) -> Option<Ipv4> {
        let mut out = [0u8; 4];
        let mut parts = s.split('.');
        for o in out.iter_mut() {
            *o = parts.next()?.parse().ok()?;
        }
        parts.next().is_none().then_some(Ipv4(out))
    }
}

impl core::fmt::Display for Ipv4 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketAddr {
    pub ip: Ipv4,
    pub port: u16,
}

impl core::fmt::Display for SocketAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.ip, self.port)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SockaddrIn {
    family: u16,
    port_be: u16,
    addr: [u8; 4],
    zero: [u8; 8],
}

impl SockaddrIn {
    fn from(a: SocketAddr) -> SockaddrIn {
        SockaddrIn {
            family: AF_INET,
            port_be: a.port.to_be(),
            addr: a.ip.0,
            zero: [0; 8],
        }
    }
    fn to(self) -> SocketAddr {
        SocketAddr {
            ip: Ipv4(self.addr),
            port: u16::from_be(self.port_be),
        }
    }
}

/// An IPv4 or IPv6 address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpAddr {
    V4(Ipv4),
    V6([u8; 16]),
}

impl IpAddr {
    /// Parse dotted IPv4 or textual IPv6 (optionally in brackets, with
    /// `::` compression and an embedded IPv4 tail).
    pub fn parse(s: &str) -> Option<IpAddr> {
        let s = s.trim();
        if let Some(v4) = Ipv4::parse(s) {
            return Some(IpAddr::V4(v4));
        }
        let s = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')).unwrap_or(s);
        // Drop a zone index ("fe80::1%eth0").
        let s = s.split('%').next().unwrap_or(s);
        if !s.contains(':') {
            return None;
        }
        let parse_groups = |part: &str, out: &mut Vec<u16>| -> Option<()> {
            if part.is_empty() {
                return Some(());
            }
            for g in part.split(':') {
                if g.contains('.') {
                    let v4 = Ipv4::parse(g)?;
                    out.push(u16::from_be_bytes([v4.0[0], v4.0[1]]));
                    out.push(u16::from_be_bytes([v4.0[2], v4.0[3]]));
                } else {
                    if g.is_empty() || g.len() > 4 {
                        return None;
                    }
                    out.push(u16::from_str_radix(g, 16).ok()?);
                }
            }
            Some(())
        };
        let mut head = Vec::new();
        let mut tail = Vec::new();
        match s.split_once("::") {
            Some((h, t)) => {
                parse_groups(h, &mut head)?;
                parse_groups(t, &mut tail)?;
                if head.len() + tail.len() > 7 {
                    return None;
                }
            }
            None => {
                parse_groups(s, &mut head)?;
                if head.len() != 8 {
                    return None;
                }
            }
        }
        let mut g = [0u16; 8];
        g[..head.len()].copy_from_slice(&head);
        g[8 - tail.len()..].copy_from_slice(&tail);
        let mut a = [0u8; 16];
        for (i, v) in g.iter().enumerate() {
            a[2 * i..2 * i + 2].copy_from_slice(&v.to_be_bytes());
        }
        Some(IpAddr::V6(a))
    }

    pub fn is_v6(&self) -> bool {
        matches!(self, IpAddr::V6(_))
    }

    /// Link-local (fe80::/10, 169.254/16) or loopback.
    pub fn is_local_scope(&self) -> bool {
        match self {
            IpAddr::V4(v) => v.0[0] == 127 || (v.0[0] == 169 && v.0[1] == 254),
            IpAddr::V6(a) => (a[0] == 0xfe && a[1] & 0xc0 == 0x80) || *a == LOOPBACK6,
        }
    }
}

pub const LOOPBACK6: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

impl core::fmt::Display for IpAddr {
    /// RFC 5952 text form (longest zero run compressed).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IpAddr::V4(v) => write!(f, "{}", v),
            IpAddr::V6(a) => {
                let g: Vec<u16> = (0..8).map(|i| u16::from_be_bytes([a[2 * i], a[2 * i + 1]])).collect();
                let (mut best, mut best_len, mut i) = (8, 0, 0);
                while i < 8 {
                    if g[i] == 0 {
                        let start = i;
                        while i < 8 && g[i] == 0 {
                            i += 1;
                        }
                        if i - start > best_len && i - start > 1 {
                            best = start;
                            best_len = i - start;
                        }
                    } else {
                        i += 1;
                    }
                }
                let mut i = 0;
                while i < 8 {
                    if i == best {
                        f.write_str(if i == 0 { "::" } else { ":" })?;
                        i += best_len;
                        continue;
                    }
                    write!(f, "{:x}", g[i])?;
                    if i < 7 {
                        f.write_str(":")?;
                    }
                    i += 1;
                }
                Ok(())
            }
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SockaddrIn6 {
    family: u16,
    port_be: u16,
    flowinfo: u32,
    addr: [u8; 16],
    scope: u32,
}

/// Credentials of the process at the other end of an `AF_UNIX` socket
/// (`SO_PEERCRED`): taken when it connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ucred {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

/// A socket descriptor, closed on drop.
pub struct Socket {
    fd: i32,
}

impl Socket {
    pub fn new(domain: u16, ty: u32, proto: u32) -> Result<Socket> {
        let fd = sys::check(sys::syscall(
            nr::SOCKET,
            &[
                domain as usize,
                (ty | SOCK_CLOEXEC) as usize,
                proto as usize,
            ],
        ))?;
        Ok(Socket { fd: fd as i32 })
    }

    pub fn fd(&self) -> i32 {
        self.fd
    }

    /// Bind to a raw `sockaddr` (any family; see `unix_addr`).
    pub fn bind_raw(&self, sa: &[u8]) -> Result<()> {
        sys::check(sys::syscall(
            nr::BIND,
            &[self.fd as usize, sa.as_ptr() as usize, sa.len()],
        ))
        .map(|_| ())
    }

    /// Connect to a raw `sockaddr` (any family; see `unix_addr`).
    pub fn connect_raw(&self, sa: &[u8]) -> Result<()> {
        sys::check(sys::syscall(
            nr::CONNECT,
            &[self.fd as usize, sa.as_ptr() as usize, sa.len()],
        ))
        .map(|_| ())
    }

    pub fn bind(&self, a: SocketAddr) -> Result<()> {
        let sa = SockaddrIn::from(a);
        sys::check(sys::syscall(
            nr::BIND,
            &[self.fd as usize, &sa as *const _ as usize, 16],
        ))
        .map(|_| ())
    }

    pub fn connect(&self, a: SocketAddr) -> Result<()> {
        let sa = SockaddrIn::from(a);
        sys::check(sys::syscall(
            nr::CONNECT,
            &[self.fd as usize, &sa as *const _ as usize, 16],
        ))
        .map(|_| ())
    }

    /// Connect to an IPv4 or IPv6 endpoint (the socket's family must
    /// match).
    pub fn connect_ip(&self, ip: IpAddr, port: u16) -> Result<()> {
        match ip {
            IpAddr::V4(v4) => self.connect(SocketAddr { ip: v4, port }),
            IpAddr::V6(a) => {
                let sa = SockaddrIn6 {
                    family: AF_INET6,
                    port_be: port.to_be(),
                    flowinfo: 0,
                    addr: a,
                    scope: 0,
                };
                sys::check(sys::syscall(
                    nr::CONNECT,
                    &[self.fd as usize, &sa as *const _ as usize, 28],
                ))
                .map(|_| ())
            }
        }
    }

    /// Send a datagram to an IPv4 or IPv6 endpoint.
    pub fn send_to_ip(&self, buf: &[u8], ip: IpAddr, port: u16) -> Result<usize> {
        match ip {
            IpAddr::V4(v4) => self.send_to(buf, SocketAddr { ip: v4, port }),
            IpAddr::V6(a) => {
                let sa = SockaddrIn6 {
                    family: AF_INET6,
                    port_be: port.to_be(),
                    flowinfo: 0,
                    addr: a,
                    scope: 0,
                };
                sys::check(sys::syscall(
                    nr::SENDTO,
                    &[self.fd as usize, buf.as_ptr() as usize, buf.len(), 0, &sa as *const _ as usize, 28],
                ))
                .map(|n| n as usize)
            }
        }
    }

    /// Receive a datagram (sender address not reported).
    pub fn recv_any(&self, buf: &mut [u8]) -> Result<usize> {
        self.recv(buf)
    }

    pub fn listen(&self, backlog: u32) -> Result<()> {
        sys::check(sys::syscall(
            nr::LISTEN,
            &[self.fd as usize, backlog as usize],
        ))
        .map(|_| ())
    }

    pub fn accept(&self) -> Result<(Socket, SocketAddr)> {
        let mut sa = SockaddrIn::default();
        let mut len: u32 = 16;
        let fd = sys::check(sys::syscall(
            nr::ACCEPT,
            &[
                self.fd as usize,
                &mut sa as *mut _ as usize,
                &mut len as *mut u32 as usize,
            ],
        ))?;
        Ok((Socket { fd: fd as i32 }, sa.to()))
    }

    /// accept4(2) with `flags` (`SOCK_NONBLOCK`, `SOCK_CLOEXEC`), not
    /// reporting the peer's address (for `AF_UNIX` listeners).
    pub fn accept4(&self, flags: u32) -> Result<Socket> {
        let fd = sys::check(sys::syscall(
            nr::ACCEPT4,
            &[self.fd as usize, 0, 0, flags as usize],
        ))?;
        Ok(Socket { fd: fd as i32 })
    }

    pub fn send(&self, buf: &[u8]) -> Result<usize> {
        sys::check(sys::syscall(
            nr::SENDTO,
            &[self.fd as usize, buf.as_ptr() as usize, buf.len(), 0, 0, 0],
        ))
    }

    pub fn send_all(&self, mut buf: &[u8]) -> Result<()> {
        while !buf.is_empty() {
            let n = self.send(buf)?;
            buf = &buf[n..];
        }
        Ok(())
    }

    pub fn recv(&self, buf: &mut [u8]) -> Result<usize> {
        sys::check(sys::syscall(
            nr::RECVFROM,
            &[
                self.fd as usize,
                buf.as_mut_ptr() as usize,
                buf.len(),
                0,
                0,
                0,
            ],
        ))
    }

    pub fn send_to(&self, buf: &[u8], a: SocketAddr) -> Result<usize> {
        let sa = SockaddrIn::from(a);
        sys::check(sys::syscall(
            nr::SENDTO,
            &[
                self.fd as usize,
                buf.as_ptr() as usize,
                buf.len(),
                0,
                &sa as *const _ as usize,
                16,
            ],
        ))
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let mut sa = SockaddrIn::default();
        let mut len: u32 = 16;
        let n = sys::check(sys::syscall(
            nr::RECVFROM,
            &[
                self.fd as usize,
                buf.as_mut_ptr() as usize,
                buf.len(),
                0,
                &mut sa as *mut _ as usize,
                &mut len as *mut u32 as usize,
            ],
        ))?;
        Ok((n, sa.to()))
    }

    pub fn set_option(&self, level: u32, name: u32, val: &[u8]) -> Result<()> {
        sys::check(sys::syscall(
            nr::SETSOCKOPT,
            &[
                self.fd as usize,
                level as usize,
                name as usize,
                val.as_ptr() as usize,
                val.len(),
            ],
        ))
        .map(|_| ())
    }

    /// getsockopt(2) into `val`; returns the option's length.
    pub fn get_option(&self, level: u32, name: u32, val: &mut [u8]) -> Result<usize> {
        let mut len = val.len() as u32;
        sys::check(sys::syscall(
            nr::GETSOCKOPT,
            &[
                self.fd as usize,
                level as usize,
                name as usize,
                val.as_mut_ptr() as usize,
                &mut len as *mut u32 as usize,
            ],
        ))?;
        Ok(len as usize)
    }

    /// The peer's credentials (`SO_PEERCRED`, `AF_UNIX` only).
    pub fn peer_cred(&self) -> Result<Ucred> {
        let mut b = [0u8; 12];
        if self.get_option(SOL_SOCKET, SO_PEERCRED, &mut b)? < 12 {
            return Err(Error(22));
        }
        let f = |i: usize| u32::from_ne_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Ok(Ucred {
            pid: f(0),
            uid: f(4),
            gid: f(8),
        })
    }

    /// Receive timeout in milliseconds (0 = none).
    pub fn set_timeout(&self, ms: u64) -> Result<()> {
        let tv = [(ms / 1000) as i64, ((ms % 1000) * 1000) as i64];
        let b: [u8; 16] = unsafe { core::mem::transmute(tv) };
        self.set_option(SOL_SOCKET, SO_RCVTIMEO, &b)
    }

    /// Bound `connect` and `send` (SO_SNDTIMEO).
    pub fn set_send_timeout(&self, ms: u64) -> Result<()> {
        let tv = [(ms / 1000) as i64, ((ms % 1000) * 1000) as i64];
        let b: [u8; 16] = unsafe { core::mem::transmute(tv) };
        self.set_option(SOL_SOCKET, SO_SNDTIMEO, &b)
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        let mut sa = SockaddrIn::default();
        let mut len: u32 = 16;
        sys::check(sys::syscall(
            nr::GETSOCKNAME,
            &[
                self.fd as usize,
                &mut sa as *mut _ as usize,
                &mut len as *mut u32 as usize,
            ],
        ))?;
        Ok(sa.to())
    }

    pub fn peer_addr(&self) -> Result<SocketAddr> {
        let mut sa = SockaddrIn::default();
        let mut len: u32 = 16;
        sys::check(sys::syscall(
            nr::GETPEERNAME,
            &[
                self.fd as usize,
                &mut sa as *mut _ as usize,
                &mut len as *mut u32 as usize,
            ],
        ))?;
        Ok(sa.to())
    }

    pub fn shutdown(&self, how: u32) -> Result<()> {
        sys::check(sys::syscall(
            nr::SHUTDOWN,
            &[self.fd as usize, how as usize],
        ))
        .map(|_| ())
    }

    pub fn ioctl(&self, cmd: u64, arg: usize) -> Result<usize> {
        sys::check(sys::syscall(
            nr::IOCTL,
            &[self.fd as usize, cmd as usize, arg],
        ))
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        sys::syscall(nr::CLOSE, &[self.fd as usize]);
    }
}

pub fn tcp_connect(a: SocketAddr) -> Result<Socket> {
    let s = Socket::new(AF_INET, SOCK_STREAM, 0)?;
    s.connect(a)?;
    Ok(s)
}

pub fn tcp_listen(a: SocketAddr) -> Result<Socket> {
    let s = Socket::new(AF_INET, SOCK_STREAM, 0)?;
    let _ = s.set_option(SOL_SOCKET, SO_REUSEADDR, &1u32.to_ne_bytes());
    s.bind(a)?;
    s.listen(16)?;
    Ok(s)
}

pub fn udp_bind(a: SocketAddr) -> Result<Socket> {
    let s = Socket::new(AF_INET, SOCK_DGRAM, 0)?;
    s.bind(a)?;
    Ok(s)
}

// ---------------------------------------------------------------------------
// DNS
// ---------------------------------------------------------------------------

/// Nameservers from /etc/resolv.conf (default 10.0.2.3, then 8.8.8.8).
pub fn nameservers() -> Vec<Ipv4> {
    let mut v = Vec::new();
    if let Ok(conf) = crate::fs::read_to_string("/etc/resolv.conf") {
        for line in conf.lines() {
            let mut w = line.split_whitespace();
            if w.next() == Some("nameserver")
                && let Some(ip) = w.next().and_then(Ipv4::parse)
            {
                v.push(ip);
            }
        }
    }
    if v.is_empty() {
        v.push(Ipv4([8, 8, 8, 8]));
    }
    v
}

fn dns_query(name: &str, id: u16) -> Vec<u8> {
    dns_query_type(name, id, 1)
}

/// A DNS query for `name` with record type `qtype` (1 = A, 28 = AAAA).
fn dns_query_type(name: &str, id: u16, qtype: u16) -> Vec<u8> {
    let mut q = Vec::new();
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.trim_end_matches('.').split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&[0, 1]); // IN
    q
}

fn skip_name(p: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let l = *p.get(i)? as usize;
        if l == 0 {
            return Some(i + 1);
        }
        if l & 0xC0 == 0xC0 {
            return Some(i + 2);
        }
        i += 1 + l;
    }
}

fn parse_a_records(p: &[u8], id: u16) -> Option<Vec<Ipv4>> {
    parse_records(p, id).map(|v| {
        v.into_iter()
            .filter_map(|a| match a {
                IpAddr::V4(x) => Some(x),
                IpAddr::V6(_) => None,
            })
            .collect()
    })
}

/// A and AAAA records of a DNS response (`None` if it is not ours).
fn parse_records(p: &[u8], id: u16) -> Option<Vec<IpAddr>> {
    if p.len() < 12 || u16::from_be_bytes([p[0], p[1]]) != id {
        return None;
    }
    if p[3] & 0x0F != 0 {
        return Some(Vec::new());
    }
    let qd = u16::from_be_bytes([p[4], p[5]]);
    let an = u16::from_be_bytes([p[6], p[7]]);
    let mut i = 12;
    for _ in 0..qd {
        i = skip_name(p, i)? + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        i = skip_name(p, i)?;
        let ty = u16::from_be_bytes([*p.get(i)?, *p.get(i + 1)?]);
        let len = u16::from_be_bytes([*p.get(i + 8)?, *p.get(i + 9)?]) as usize;
        i += 10;
        if ty == 1 && len == 4 {
            out.push(IpAddr::V4(Ipv4([p[i], p[i + 1], p[i + 2], p[i + 3]])));
        } else if ty == 28 && len == 16 {
            let mut a = [0u8; 16];
            a.copy_from_slice(p.get(i..i + 16)?);
            out.push(IpAddr::V6(a));
        }
        i += len;
    }
    Some(out)
}

/// Resolve a host name to IPv4 addresses (numeric addresses pass through;
/// /etc/hosts is consulted first).
pub fn resolve(host: &str) -> Result<Vec<Ipv4>> {
    if let Some(ip) = Ipv4::parse(host) {
        return Ok(alloc::vec![ip]);
    }
    if host == "localhost" {
        return Ok(alloc::vec![Ipv4([127, 0, 0, 1])]);
    }
    if let Ok(hosts) = crate::fs::read_to_string("/etc/hosts") {
        for line in hosts.lines() {
            let mut w = line.split_whitespace();
            if let Some(ip) = w.next().and_then(Ipv4::parse)
                && w.any(|n| n == host)
            {
                return Ok(alloc::vec![ip]);
            }
        }
    }
    let mut rnd = [0u8; 2];
    crate::process::getrandom(&mut rnd);
    let id = u16::from_ne_bytes(rnd);
    let query = dns_query(host, id);
    let sock = Socket::new(AF_INET, SOCK_DGRAM, 0)?;
    sock.set_timeout(2000)?;
    for server in nameservers() {
        for _ in 0..2 {
            sock.send_to(
                &query,
                SocketAddr {
                    ip: server,
                    port: 53,
                },
            )?;
            let mut buf = [0u8; 512];
            if let Ok((n, _)) = sock.recv_from(&mut buf)
                && let Some(ips) = parse_a_records(&buf[..n], id)
            {
                return if ips.is_empty() {
                    Err(Error(2))
                } else {
                    Ok(ips)
                };
            }
        }
    }
    Err(Error(110))
}

/// Nameservers of either family from /etc/resolv.conf.
pub fn nameservers_all() -> Vec<IpAddr> {
    let mut v = Vec::new();
    if let Ok(conf) = crate::fs::read_to_string("/etc/resolv.conf") {
        for line in conf.lines() {
            let mut w = line.split_whitespace();
            if w.next() == Some("nameserver")
                && let Some(ip) = w.next().and_then(IpAddr::parse)
            {
                v.push(ip);
            }
        }
    }
    if v.is_empty() {
        v.push(IpAddr::V4(Ipv4([8, 8, 8, 8])));
    }
    v
}

/// True if some interface has a global (non link-local) IPv6 address.
pub fn have_global_ipv6() -> bool {
    crate::fs::read_to_string("/proc/net/if_addrs")
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|w| w.split('/').next().and_then(IpAddr::parse))
        .any(|a| a.is_v6() && !a.is_local_scope())
}

fn hosts_lookup(host: &str) -> Vec<IpAddr> {
    let mut v = Vec::new();
    if let Ok(hosts) = crate::fs::read_to_string("/etc/hosts") {
        for line in hosts.lines() {
            let line = line.split('#').next().unwrap_or("");
            let mut w = line.split_whitespace();
            if let Some(ip) = w.next().and_then(IpAddr::parse)
                && w.any(|n| n.eq_ignore_ascii_case(host))
            {
                v.push(ip);
            }
        }
    }
    v
}

/// Resolve `host` to IPv4 and IPv6 addresses in connection order: IPv6
/// first only when this machine has a global IPv6 address (a simple
/// RFC 6724 policy), then IPv4. Literals, `localhost` and /etc/hosts are
/// handled locally; DNS asks for AAAA and A records.
pub fn resolve_all(host: &str) -> Result<Vec<IpAddr>> {
    if let Some(ip) = IpAddr::parse(host) {
        return Ok(alloc::vec![ip]);
    }
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(alloc::vec![IpAddr::V4(Ipv4([127, 0, 0, 1])), IpAddr::V6(LOOPBACK6)]);
    }
    let from_hosts = hosts_lookup(host);
    if !from_hosts.is_empty() {
        return Ok(from_hosts);
    }
    let want6 = have_global_ipv6();
    let mut rnd = [0u8; 2];
    crate::process::getrandom(&mut rnd);
    let id = u16::from_ne_bytes(rnd);
    let q4 = dns_query_type(host, id, 1);
    let q6 = dns_query_type(host, id.wrapping_add(1), 28);
    let mut v4 = None;
    let mut v6 = None;
    let mut last_err = Error(110);
    'servers: for server in nameservers_all() {
        let family = if server.is_v6() { AF_INET6 } else { AF_INET };
        let Ok(sock) = Socket::new(family, SOCK_DGRAM, 0) else { continue };
        sock.set_timeout(2000)?;
        for _ in 0..2 {
            if v4.is_none() {
                let _ = sock.send_to_ip(&q4, server, 53);
            }
            if want6 && v6.is_none() {
                let _ = sock.send_to_ip(&q6, server, 53);
            }
            loop {
                let mut buf = [0u8; 1500];
                let Ok(n) = sock.recv_any(&mut buf) else { break };
                if let Some(r) = parse_records(&buf[..n], id) {
                    v4 = Some(r);
                } else if let Some(r) = parse_records(&buf[..n], id.wrapping_add(1)) {
                    v6 = Some(r);
                }
                if v4.is_some() && (!want6 || v6.is_some()) {
                    break 'servers;
                }
            }
            if v4.is_some() {
                // Don't wait long for a missing AAAA answer.
                break 'servers;
            }
            last_err = Error(110);
        }
    }
    let mut out = Vec::new();
    if want6 {
        out.extend(v6.unwrap_or_default().into_iter().filter(|a| a.is_v6()));
    }
    out.extend(v4.clone().unwrap_or_default().into_iter().filter(|a| !a.is_v6()));
    if out.is_empty() {
        return Err(if v4.is_some() { Error(2) } else { last_err });
    }
    Ok(out)
}

/// Connect a TCP socket to `host:port`, trying every resolved address in
/// order with `timeout_ms` per attempt. Returns the socket and the
/// address used.
pub fn connect_host(host: &str, port: u16, timeout_ms: u64) -> Result<(Socket, IpAddr)> {
    let addrs = resolve_all(host)?;
    let mut err = Error(111);
    for ip in addrs {
        let family = if ip.is_v6() { AF_INET6 } else { AF_INET };
        let sock = Socket::new(family, SOCK_STREAM, 0)?;
        if timeout_ms > 0 {
            sock.set_send_timeout(timeout_ms)?;
        }
        match sock.connect_ip(ip, port) {
            Ok(()) => return Ok((sock, ip)),
            Err(e) => err = e,
        }
    }
    Err(err)
}

/// Parse "host:port" and resolve it.
pub fn resolve_addr(s: &str, default_port: u16) -> Result<SocketAddr> {
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| Error(22))?),
        None => (s, default_port),
    };
    let ip = resolve(host)?[0];
    Ok(SocketAddr { ip, port })
}

/// One network interface as reported by /proc/net/if_addrs.
#[derive(Debug, Clone, Default)]
pub struct IfInfo {
    pub name: String,
    pub index: u32,
    pub up: bool,
    pub mtu: u32,
    pub mac: String,
    pub link: bool,
    pub dhcp: bool,
    pub driver: String,
    pub gateway: Option<Ipv4>,
    /// "a.b.c.d/p" and IPv6 "x::y/p" strings.
    pub addrs: Vec<String>,
}

impl IfInfo {
    /// First IPv4 address and prefix length.
    pub fn ipv4(&self) -> Option<(Ipv4, u8)> {
        self.addrs.iter().find_map(|a| {
            let (ip, p) = a.split_once('/')?;
            Some((Ipv4::parse(ip)?, p.parse().ok()?))
        })
    }
}

pub fn interfaces() -> Vec<IfInfo> {
    let data = crate::fs::read_to_string("/proc/net/if_addrs").unwrap_or_default();
    data.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            // Interfaces without addresses (a new tunnel) end at field 9.
            if f.len() < 9 {
                return None;
            }
            Some(IfInfo {
                name: f[0].into(),
                index: f[1].parse().unwrap_or(0),
                up: f[2] == "up",
                mtu: f[3].parse().unwrap_or(0),
                mac: f[4].into(),
                link: f[5] == "link",
                dhcp: f[6] == "dhcp",
                driver: f[7].into(),
                gateway: Ipv4::parse(f[8]),
                addrs: f[9..].iter().map(|s| String::from(*s)).collect(),
            })
        })
        .collect()
}

pub fn interface(name: &str) -> Option<IfInfo> {
    interfaces().into_iter().find(|i| i.name == name)
}

pub const SIOCADDRT: u64 = 0x890B;
pub const SIOCDELRT: u64 = 0x890C;
pub const SIOCGIFFLAGS: u64 = 0x8913;
pub const SIOCSIFFLAGS: u64 = 0x8914;
pub const SIOCSIFADDR: u64 = 0x8916;
pub const SIOCSIFNETMASK: u64 = 0x891C;
pub const SIOCRDHCP: u64 = 0x89F0;
pub const SIOCRGATEWAY: u64 = 0x89F1;
pub const SIOCRDNS: u64 = 0x89F2;
pub const SIOCRWIFI: u64 = 0x89F8;

/// struct ifreq: 16-byte name + 24-byte union.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IfReq {
    pub name: [u8; 16],
    pub data: [u8; 24],
}

impl IfReq {
    pub fn new(name: &str) -> IfReq {
        let mut r = IfReq { name: [0; 16], data: [0; 24] };
        let n = name.len().min(15);
        r.name[..n].copy_from_slice(&name.as_bytes()[..n]);
        r
    }
    pub fn set_sockaddr(&mut self, ip: Ipv4) {
        let sa = SockaddrIn::from(SocketAddr { ip, port: 0 });
        let b: [u8; 16] = unsafe { core::mem::transmute(sa) };
        self.data[..16].copy_from_slice(&b);
    }
}

/// Issue an interface ioctl through a throwaway datagram socket.
pub fn if_ioctl(cmd: u64, req: &mut IfReq) -> Result<()> {
    let s = Socket::new(AF_INET, SOCK_DGRAM, 0)?;
    s.ioctl(cmd, req as *mut IfReq as usize).map(|_| ())
}

pub fn set_up(name: &str, up: bool) -> Result<()> {
    let mut r = IfReq::new(name);
    if_ioctl(SIOCGIFFLAGS, &mut r)?;
    let mut f = u16::from_ne_bytes([r.data[0], r.data[1]]);
    if up {
        f |= 1;
    } else {
        f &= !1;
    }
    r.data[..2].copy_from_slice(&f.to_ne_bytes());
    if_ioctl(SIOCSIFFLAGS, &mut r)
}

/// Assign a static IPv4 address (disables DHCP on the interface).
pub fn set_address(name: &str, ip: Ipv4, prefix: u8) -> Result<()> {
    let mut r = IfReq::new(name);
    r.set_sockaddr(ip);
    if_ioctl(SIOCSIFADDR, &mut r)?;
    if ip == Ipv4::ANY {
        return Ok(());
    }
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
    let mut r = IfReq::new(name);
    r.set_sockaddr(Ipv4(mask.to_be_bytes()));
    if_ioctl(SIOCSIFNETMASK, &mut r)
}

pub fn set_gateway(name: &str, gw: Option<Ipv4>) -> Result<()> {
    let mut r = IfReq::new(name);
    r.set_sockaddr(gw.unwrap_or(Ipv4::ANY));
    if_ioctl(SIOCRGATEWAY, &mut r)
}

pub fn set_dhcp(name: &str, on: bool) -> Result<()> {
    let mut r = IfReq::new(name);
    r.data[..4].copy_from_slice(&(on as i32).to_ne_bytes());
    if_ioctl(SIOCRDHCP, &mut r)
}

/// Add or delete a route (`dst`/`prefix` via `gw`); prefix 0 = default.
pub fn route(add: bool, dst: Ipv4, prefix: u8, gw: Ipv4, dev: Option<&str>) -> Result<()> {
    let mut rt = [0u8; 120];
    let enc = |ip: Ipv4| -> [u8; 16] { unsafe { core::mem::transmute(SockaddrIn::from(SocketAddr { ip, port: 0 })) } };
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
    rt[8..24].copy_from_slice(&enc(dst));
    rt[24..40].copy_from_slice(&enc(gw));
    rt[40..56].copy_from_slice(&enc(Ipv4(mask.to_be_bytes())));
    rt[56..58].copy_from_slice(&3u16.to_ne_bytes());
    let mut devname = [0u8; 16];
    if let Some(d) = dev {
        let n = d.len().min(15);
        devname[..n].copy_from_slice(&d.as_bytes()[..n]);
        rt[104..112].copy_from_slice(&(devname.as_ptr() as u64).to_ne_bytes());
    }
    let s = Socket::new(AF_INET, SOCK_DGRAM, 0)?;
    s.ioctl(if add { SIOCADDRT } else { SIOCDELRT }, rt.as_mut_ptr() as usize)
        .map(|_| ())
}

/// Parse "a.b.c.d/p" (prefix defaults to `def`).
pub fn parse_cidr(s: &str, def: u8) -> Option<(Ipv4, u8)> {
    match s.split_once('/') {
        Some((ip, p)) => Some((Ipv4::parse(ip)?, p.parse().ok().filter(|&p: &u8| p <= 32)?)),
        None => Some((Ipv4::parse(s)?, def)),
    }
}

/// Prefix length of a dotted netmask.
pub fn mask_prefix(mask: Ipv4) -> u8 {
    u32::from_be_bytes(mask.0).count_ones() as u8
}

pub fn prefix_mask(p: u8) -> Ipv4 {
    let m = if p == 0 { 0 } else { u32::MAX << (32 - p as u32) };
    Ipv4(m.to_be_bytes())
}

/// A `sockaddr_un` for filesystem path `path`.
pub fn unix_addr(path: &str) -> Vec<u8> {
    let mut v = AF_UNIX.to_ne_bytes().to_vec();
    v.extend_from_slice(path.as_bytes());
    v.push(0);
    v
}
