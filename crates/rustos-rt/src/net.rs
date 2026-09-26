//! Sockets (IPv4/IPv6 TCP and UDP, ICMP echo) and name resolution.

use crate::sys::{self, nr};
use crate::{Error, Result};
use alloc::string::String;
use alloc::vec::Vec;

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
pub const SO_RCVTIMEO: u32 = 20;

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

    /// Receive timeout in milliseconds (0 = none).
    pub fn set_timeout(&self, ms: u64) -> Result<()> {
        let tv = [(ms / 1000) as i64, ((ms % 1000) * 1000) as i64];
        let b: [u8; 16] = unsafe { core::mem::transmute(tv) };
        self.set_option(SOL_SOCKET, SO_RCVTIMEO, &b)
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
    let mut q = Vec::new();
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.trim_end_matches('.').split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, 1, 0, 1]); // A, IN
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
            out.push(Ipv4([p[i], p[i + 1], p[i + 2], p[i + 3]]));
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
            if f.len() < 10 {
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
