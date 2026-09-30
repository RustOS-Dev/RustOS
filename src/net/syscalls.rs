//! Socket system calls, sockaddr conversion and interface ioctls.

use super::socket::{AF_INET, AF_INET6, AF_UNIX, Proto, Socket};
use crate::errno::*;
use crate::process::uaccess;
use crate::syscall::nr;
use crate::vfs::{self, File, FileLike, POLLHUP, POLLIN, POLLOUT};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use smoltcp::wire::{IpAddress, IpCidr, IpEndpoint, Ipv4Address, Ipv4Cidr, Ipv6Address};

const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;
const SOCK_RAW: u32 = 3;
const SOCK_NONBLOCK: u32 = 0o4000;
const SOCK_CLOEXEC: u32 = 0o2000000;

const MSG_PEEK: u32 = 0x2;
const MSG_DONTWAIT: u32 = 0x40;

const SOL_SOCKET: u32 = 1;
const IPPROTO_IP: u32 = 0;
const IPPROTO_TCP: u32 = 6;
const SO_REUSEADDR: u32 = 2;
const SO_TYPE: u32 = 3;
const SO_ERROR: u32 = 4;
const SO_BROADCAST: u32 = 6;
const SO_SNDBUF: u32 = 7;
const SO_RCVBUF: u32 = 8;
const SO_KEEPALIVE: u32 = 9;
const SO_REUSEPORT: u32 = 15;
const SO_RCVTIMEO: u32 = 20;
const SO_SNDTIMEO: u32 = 21;
const SO_ACCEPTCONN: u32 = 30;
const TCP_NODELAY: u32 = 1;
const IP_TTL: u32 = 2;

fn cur_files() -> KResult<Arc<crate::process::Process>> {
    crate::process::current().ok_or(ESRCH)
}

fn get_file(fd: i32) -> KResult<Arc<File>> {
    cur_files()?.files.lock().get(fd)
}

fn with_socket<R>(fd: i32, f: impl FnOnce(&Socket, &File) -> KResult<R>) -> KResult<R> {
    let file = get_file(fd)?;
    let s = file
        .stream()
        .and_then(|s| s.as_any().downcast_ref::<Socket>())
        .ok_or(ENOTSOCK)?;
    f(s, &file)
}

fn install(obj: Arc<dyn FileLike>, flags: u32) -> KResult<i64> {
    let mut fl = vfs::O_RDWR;
    if flags & SOCK_NONBLOCK != 0 {
        fl |= vfs::O_NONBLOCK;
    }
    let file = File::from_stream(obj, fl, "socket:");
    let fd = cur_files()?
        .files
        .lock()
        .install(file, flags & SOCK_CLOEXEC != 0)?;
    Ok(fd as i64)
}

/// Parse a user sockaddr into an endpoint.
pub fn read_sockaddr(addr: u64, len: u64) -> KResult<IpEndpoint> {
    if addr == 0 || len < 2 {
        return Err(EINVAL);
    }
    let b = uaccess::read_bytes(addr, (len as usize).min(128))?;
    let family = u16::from_ne_bytes([b[0], b[1]]);
    match family {
        AF_INET if b.len() >= 8 => Ok(IpEndpoint::new(
            IpAddress::Ipv4(Ipv4Address::new(b[4], b[5], b[6], b[7])),
            u16::from_be_bytes([b[2], b[3]]),
        )),
        AF_INET6 if b.len() >= 24 => {
            let mut a = [0u8; 16];
            a.copy_from_slice(&b[8..24]);
            let v6 = Ipv6Address::from(a);
            // v4-mapped addresses go to the IPv4 stack.
            let addr = match v6.to_ipv4_mapped() {
                Some(v4) => IpAddress::Ipv4(v4),
                None => IpAddress::Ipv6(v6),
            };
            Ok(IpEndpoint::new(addr, u16::from_be_bytes([b[2], b[3]])))
        }
        0 => Ok(IpEndpoint::new(IpAddress::v4(0, 0, 0, 0), 0)), // AF_UNSPEC
        _ => Err(EAFNOSUPPORT),
    }
}

fn encode_sockaddr(ep: &IpEndpoint, family: u16) -> Vec<u8> {
    match (ep.addr, family) {
        (IpAddress::Ipv4(a), AF_INET) => {
            let mut b = vec![0u8; 16];
            b[0..2].copy_from_slice(&AF_INET.to_ne_bytes());
            b[2..4].copy_from_slice(&ep.port.to_be_bytes());
            b[4..8].copy_from_slice(&a.octets());
            b
        }
        (addr, _) => {
            let v6 = match addr {
                IpAddress::Ipv4(a) => a.to_ipv6_mapped(),
                IpAddress::Ipv6(a) => a,
            };
            let mut b = vec![0u8; 28];
            b[0..2].copy_from_slice(&AF_INET6.to_ne_bytes());
            b[2..4].copy_from_slice(&ep.port.to_be_bytes());
            b[8..24].copy_from_slice(&v6.octets());
            b
        }
    }
}

/// Write a sockaddr to user memory (addr/lenptr may be null).
fn write_sockaddr(addr: u64, lenptr: u64, ep: &IpEndpoint, family: u16) -> KResult<()> {
    if addr == 0 || lenptr == 0 {
        return Ok(());
    }
    let b = encode_sockaddr(ep, family);
    let cap: u32 = uaccess::read_user(lenptr)?;
    let n = (cap as usize).min(b.len());
    uaccess::copy_to_user(addr, &b[..n])?;
    uaccess::write_user(lenptr, &(b.len() as u32))?;
    Ok(())
}

fn nonblock(file: &File, flags: u32) -> bool {
    file.flags() & vfs::O_NONBLOCK != 0 || flags & MSG_DONTWAIT != 0
}

fn read_iov(iov: u64, cnt: u64) -> KResult<Vec<(u64, usize)>> {
    let mut v = Vec::new();
    for i in 0..cnt.min(1024) {
        let base: u64 = uaccess::read_user(iov + i * 16)?;
        let len: u64 = uaccess::read_user(iov + i * 16 + 8)?;
        v.push((base, len as usize));
    }
    Ok(v)
}

pub fn dispatch(n: u64, a: [u64; 6]) -> KResult<i64> {
    match n {
        nr::SOCKET => socket(a[0] as u16, a[1] as u32, a[2] as u32),
        nr::SOCKETPAIR => socketpair(a[0] as u16, a[1] as u32, a[3]),
        nr::BIND => with_socket(a[0] as i32, |s, _| {
            s.bind(read_sockaddr(a[1], a[2])?)?;
            Ok(0)
        }),
        nr::LISTEN => with_socket(a[0] as i32, |s, _| {
            s.listen(a[1] as usize)?;
            Ok(0)
        }),
        nr::CONNECT => with_socket(a[0] as i32, |s, f| {
            let ep = read_sockaddr(a[1], a[2])?;
            s.connect(ep, nonblock(f, 0))?;
            Ok(0)
        }),
        nr::ACCEPT | nr::ACCEPT4 => {
            let flags = if n == nr::ACCEPT4 { a[3] as u32 } else { 0 };
            let (child, family) = with_socket(a[0] as i32, |s, f| {
                Ok((s.accept(nonblock(f, 0))?, s.family))
            })?;
            if let Some(peer) = child.peer_endpoint() {
                write_sockaddr(a[1], a[2], &peer, family)?;
            }
            install(child, flags)
        }
        nr::SENDTO => with_socket(a[0] as i32, |s, f| {
            let data = uaccess::read_bytes(a[1], (a[2] as usize).min(1 << 20))?;
            let dest = if a[4] != 0 {
                Some(read_sockaddr(a[4], a[5])?)
            } else {
                None
            };
            Ok(s.send_to(&data, dest, nonblock(f, a[3] as u32))? as i64)
        }),
        nr::RECVFROM => with_socket(a[0] as i32, |s, f| {
            let flags = a[3] as u32;
            let mut buf = vec![0u8; (a[2] as usize).min(1 << 20)];
            let (n, from) = s.recv_from(&mut buf, nonblock(f, flags), flags & MSG_PEEK != 0)?;
            uaccess::copy_to_user(a[1], &buf[..n])?;
            if let Some(ep) = from {
                write_sockaddr(a[4], a[5], &ep, s.family)?;
            }
            Ok(n as i64)
        }),
        nr::SENDMSG => with_socket(a[0] as i32, |s, f| {
            let msg = a[1];
            let name: u64 = uaccess::read_user(msg)?;
            let namelen: u32 = uaccess::read_user(msg + 8)?;
            let iov: u64 = uaccess::read_user(msg + 16)?;
            let iovlen: u64 = uaccess::read_user(msg + 24)?;
            let mut data = Vec::new();
            for (base, len) in read_iov(iov, iovlen)? {
                data.extend_from_slice(&uaccess::read_bytes(base, len.min(1 << 20))?);
            }
            let dest = if name != 0 {
                Some(read_sockaddr(name, namelen as u64)?)
            } else {
                None
            };
            Ok(s.send_to(&data, dest, nonblock(f, a[2] as u32))? as i64)
        }),
        nr::RECVMSG => with_socket(a[0] as i32, |s, f| {
            let msg = a[1];
            let flags = a[2] as u32;
            let name: u64 = uaccess::read_user(msg)?;
            let iov: u64 = uaccess::read_user(msg + 16)?;
            let iovlen: u64 = uaccess::read_user(msg + 24)?;
            let iovs = read_iov(iov, iovlen)?;
            let total: usize = iovs.iter().map(|(_, l)| *l).sum();
            let mut buf = vec![0u8; total.min(1 << 20)];
            let (n, from) = s.recv_from(&mut buf, nonblock(f, flags), flags & MSG_PEEK != 0)?;
            let mut off = 0;
            for (base, len) in iovs {
                if off >= n {
                    break;
                }
                let m = len.min(n - off);
                uaccess::copy_to_user(base, &buf[off..off + m])?;
                off += m;
            }
            if let Some(ep) = from
                && name != 0
            {
                write_sockaddr(name, msg + 8, &ep, s.family)?;
            }
            // No control data.
            uaccess::write_user(msg + 40, &0u64)?;
            uaccess::write_user(msg + 48, &0i32)?;
            Ok(n as i64)
        }),
        nr::SHUTDOWN => with_socket(a[0] as i32, |s, _| {
            if a[1] > 2 {
                return Err(EINVAL);
            }
            s.shutdown(a[1] as u32)?;
            Ok(0)
        }),
        nr::GETSOCKNAME => with_socket(a[0] as i32, |s, _| {
            write_sockaddr(a[1], a[2], &s.local_endpoint(), s.family)?;
            Ok(0)
        }),
        nr::GETPEERNAME => with_socket(a[0] as i32, |s, _| {
            let ep = s.peer_endpoint().ok_or(ENOTCONN)?;
            write_sockaddr(a[1], a[2], &ep, s.family)?;
            Ok(0)
        }),
        nr::SETSOCKOPT => with_socket(a[0] as i32, |s, _| {
            setsockopt(s, a[1] as u32, a[2] as u32, a[3], a[4] as usize)
        }),
        nr::GETSOCKOPT => with_socket(a[0] as i32, |s, _| {
            getsockopt(s, a[1] as u32, a[2] as u32, a[3], a[4])
        }),
        _ => Err(ENOSYS),
    }
}

fn socket(domain: u16, ty: u32, protocol: u32) -> KResult<i64> {
    let flags = ty & (SOCK_NONBLOCK | SOCK_CLOEXEC);
    let kind = ty & 0xF;
    if domain == AF_UNIX {
        return Err(EAFNOSUPPORT);
    }
    if domain != AF_INET && domain != AF_INET6 {
        return Err(EAFNOSUPPORT);
    }
    let proto = match (kind, protocol) {
        (SOCK_STREAM, 0 | 6) => Proto::Tcp,
        (SOCK_DGRAM, 0 | 17) => Proto::Udp,
        (SOCK_DGRAM, 1 | 58) => Proto::Icmp { raw: false },
        (SOCK_RAW, 1 | 58) => Proto::Icmp {
            raw: domain == AF_INET,
        },
        (SOCK_STREAM | SOCK_DGRAM | SOCK_RAW, _) => return Err(EPROTONOSUPPORT),
        _ => return Err(ESOCKTNOSUPPORT),
    };
    install(Socket::new(domain, proto), flags)
}

// ---------------------------------------------------------------------------
// socketpair (AF_UNIX) over two pipes
// ---------------------------------------------------------------------------

struct PairEnd {
    rx: Arc<dyn FileLike>,
    tx: Arc<dyn FileLike>,
}

impl FileLike for PairEnd {
    fn read(&self, buf: &mut [u8], nb: bool) -> KResult<usize> {
        self.rx.read(buf, nb)
    }
    fn write(&self, buf: &[u8], nb: bool) -> KResult<usize> {
        self.tx.write(buf, nb)
    }
    fn poll(&self) -> u16 {
        (self.rx.poll() & (POLLIN | POLLHUP)) | (self.tx.poll() & POLLOUT)
    }
    fn wait_queue(&self) -> &crate::sched::WaitQueue {
        self.rx.wait_queue() // shared with `tx`
    }
    fn stat(&self) -> KResult<crate::vfs::Metadata> {
        Ok(crate::vfs::Metadata::new(
            crate::vfs::FileType::Socket,
            0o777,
        ))
    }
    fn close(&self) {
        self.rx.close();
        self.tx.close();
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn socketpair(domain: u16, ty: u32, sv: u64) -> KResult<i64> {
    if domain != AF_UNIX {
        return Err(EOPNOTSUPP);
    }
    let flags = ty & (SOCK_NONBLOCK | SOCK_CLOEXEC);
    let wq = Arc::new(crate::sched::WaitQueue::new());
    let (r1, w1) = vfs::pipe::pipe_on(wq.clone());
    let (r2, w2) = vfs::pipe::pipe_on(wq);
    let a = Arc::new(PairEnd { rx: r1, tx: w2 });
    let b = Arc::new(PairEnd { rx: r2, tx: w1 });
    let fa = install(a, flags)? as i32;
    let fb = install(b, flags)? as i32;
    uaccess::write_user(sv, &[fa, fb])?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

fn read_timeval_ms(ptr: u64, len: usize) -> KResult<Option<u64>> {
    if len < 16 {
        return Err(EINVAL);
    }
    let sec: i64 = uaccess::read_user(ptr)?;
    let usec: i64 = uaccess::read_user(ptr + 8)?;
    let ms = sec.max(0) as u64 * 1000 + usec.max(0) as u64 / 1000;
    Ok((ms > 0).then_some(ms))
}

fn read_int(ptr: u64, len: usize) -> KResult<i32> {
    if len < 4 {
        return Err(EINVAL);
    }
    uaccess::read_user(ptr)
}

fn setsockopt(s: &Socket, level: u32, name: u32, val: u64, len: usize) -> KResult<i64> {
    match (level, name) {
        (SOL_SOCKET, SO_RCVTIMEO) => s.set_timeout(true, read_timeval_ms(val, len)?),
        (SOL_SOCKET, SO_SNDTIMEO) => s.set_timeout(false, read_timeval_ms(val, len)?),
        (SOL_SOCKET, SO_KEEPALIVE) => s.set_keepalive(read_int(val, len)? != 0),
        (SOL_SOCKET, SO_REUSEADDR | SO_REUSEPORT | SO_BROADCAST | SO_SNDBUF | SO_RCVBUF) => {}
        (SOL_SOCKET, 13) => {} // SO_LINGER
        (IPPROTO_TCP, TCP_NODELAY) => s.set_nodelay(read_int(val, len)? != 0),
        (IPPROTO_TCP, _) => {}
        (IPPROTO_IP, IP_TTL) => s.set_ttl(read_int(val, len)?.clamp(1, 255) as u8),
        (IPPROTO_IP, _) | (41, _) => {} // IPv4/IPv6 options we don't model
        _ => return Err(ENOPROTOOPT),
    }
    Ok(0)
}

fn getsockopt(s: &Socket, level: u32, name: u32, val: u64, lenptr: u64) -> KResult<i64> {
    let v: i32 = match (level, name) {
        (SOL_SOCKET, SO_ERROR) => s.take_error(),
        (SOL_SOCKET, SO_TYPE) => match s.proto {
            Proto::Tcp => SOCK_STREAM as i32,
            Proto::Udp => SOCK_DGRAM as i32,
            Proto::Icmp { raw } => {
                if raw {
                    SOCK_RAW as i32
                } else {
                    SOCK_DGRAM as i32
                }
            }
        },
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => 65536,
        (SOL_SOCKET, SO_ACCEPTCONN) => s.is_listening() as i32,
        (SOL_SOCKET, SO_REUSEADDR | SO_KEEPALIVE | SO_BROADCAST) => 0,
        (IPPROTO_TCP, TCP_NODELAY) => s.nodelay() as i32,
        _ => return Err(ENOPROTOOPT),
    };
    uaccess::write_user(val, &v)?;
    uaccess::write_user(lenptr, &4u32)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Interface ioctls (struct ifreq: name[16] + union at offset 16)
// ---------------------------------------------------------------------------

const SIOCADDRT: u64 = 0x890B;
const SIOCDELRT: u64 = 0x890C;
const SIOCGIFCONF: u64 = 0x8912;
const SIOCGIFFLAGS: u64 = 0x8913;
const SIOCSIFFLAGS: u64 = 0x8914;
const SIOCGIFADDR: u64 = 0x8915;
const SIOCSIFADDR: u64 = 0x8916;
const SIOCGIFBRDADDR: u64 = 0x8919;
const SIOCGIFNETMASK: u64 = 0x891B;
const SIOCSIFNETMASK: u64 = 0x891C;
const SIOCGIFMTU: u64 = 0x8921;
const SIOCGIFHWADDR: u64 = 0x8927;
const SIOCGIFINDEX: u64 = 0x8933;
/// RustOS: int at offset 16: 1 = start DHCP, 0 = stop.
pub const SIOCRDHCP: u64 = 0x89F0;
/// RustOS: set default gateway (sockaddr_in at 16; 0.0.0.0 clears).
pub const SIOCRGATEWAY: u64 = 0x89F1;
/// RustOS: set DNS servers: count (u32) at 16, then up to 3 IPv4 addresses.
pub const SIOCRDNS: u64 = 0x89F2;
/// RustOS: wireless control range, forwarded to the device driver.
pub const SIOCRWIFI_FIRST: u64 = 0x89F8;
pub const SIOCRWIFI_LAST: u64 = 0x89FF;

const IFF_UP: u16 = 0x1;
const IFF_BROADCAST: u16 = 0x2;
const IFF_LOOPBACK: u16 = 0x8;
const IFF_RUNNING: u16 = 0x40;
const IFF_MULTICAST: u16 = 0x1000;

fn ifr_name(arg: u64) -> KResult<alloc::string::String> {
    let b = uaccess::read_bytes(arg, 16)?;
    let end = b.iter().position(|&c| c == 0).unwrap_or(16);
    core::str::from_utf8(&b[..end])
        .map(alloc::string::String::from)
        .map_err(|_| EINVAL)
}

fn write_sin(ptr: u64, a: Ipv4Address) -> KResult<()> {
    let b = encode_sockaddr(&IpEndpoint::new(IpAddress::Ipv4(a), 0), AF_INET);
    uaccess::copy_to_user(ptr, &b)
}

fn read_sin(ptr: u64) -> KResult<Ipv4Address> {
    match read_sockaddr(ptr, 16)?.addr {
        IpAddress::Ipv4(a) => Ok(a),
        _ => Err(EAFNOSUPPORT),
    }
}

fn need_admin() -> KResult<()> {
    let uid =
        crate::process::current().map_or(0, |p| p.uid.load(core::sync::atomic::Ordering::Relaxed));
    if uid != 0 { Err(EPERM) } else { Ok(()) }
}

pub fn if_ioctl(cmd: u64, arg: u64) -> KResult<i64> {
    if cmd == SIOCGIFCONF {
        let len: i32 = uaccess::read_user(arg)?;
        let buf: u64 = uaccess::read_user(arg + 8)?;
        let entries: Vec<(alloc::string::String, Ipv4Address)> = super::with(|net| {
            net.ifaces
                .iter()
                .filter_map(|i| i.ipv4().map(|c| (i.name.clone(), c.address())))
                .collect()
        })
        .unwrap_or_default();
        let mut used = 0i32;
        for (name, addr) in entries {
            if buf != 0 {
                if used + 40 > len {
                    break;
                }
                let mut rec = [0u8; 40];
                rec[..name.len().min(15)].copy_from_slice(&name.as_bytes()[..name.len().min(15)]);
                let sa = encode_sockaddr(&IpEndpoint::new(IpAddress::Ipv4(addr), 0), AF_INET);
                rec[16..32].copy_from_slice(&sa);
                uaccess::copy_to_user(buf + used as u64, &rec)?;
            }
            used += 40;
        }
        uaccess::write_user(arg, &used)?;
        return Ok(0);
    }
    if cmd == SIOCADDRT || cmd == SIOCDELRT {
        need_admin()?;
        // struct rtentry: pad(8) dst(16) gateway(16) genmask(16) flags(u16) ... dev ptr at 104
        let dst = read_sin(arg + 8)?;
        let gw = read_sin(arg + 24)?;
        let mask = read_sin(arg + 40)?;
        let devp: u64 = uaccess::read_user(arg + 104).unwrap_or(0);
        let dev = if devp != 0 {
            Some(uaccess::read_cstr(devp, 16)?)
        } else {
            None
        };
        let prefix = u32::from_be_bytes(mask.octets()).count_ones() as u8;
        return super::with(|net| {
            let idx = match &dev {
                Some(d) => net.iface(d).map(|i| i.index),
                None => net.route(&IpAddress::Ipv4(gw)),
            }
            .ok_or(ENETUNREACH)?;
            let ifc = net.by_index(idx).ok_or(ENODEV)?;
            if prefix == 0 {
                ifc.set_gateway(if cmd == SIOCADDRT { Some(gw) } else { None });
            } else {
                let c = Ipv4Cidr::new(dst, prefix);
                ifc.routes.retain(|(n, _)| *n != c);
                if cmd == SIOCADDRT {
                    ifc.routes.push((c, gw));
                }
                let g = ifc.gateway;
                ifc.set_gateway(g);
            }
            Ok(0)
        })
        .ok_or(ENETDOWN)?;
    }
    if !(0x8900..=0x89FF).contains(&cmd) {
        return Err(ENOTTY);
    }
    let name = ifr_name(arg)?;
    let data = arg + 16;
    if (SIOCRWIFI_FIRST..=SIOCRWIFI_LAST).contains(&cmd) {
        let dev = super::with(|net| net.iface(&name).and_then(|i| i.device().cloned()))
            .flatten()
            .ok_or(ENODEV)?;
        if cmd != SIOCRWIFI_FIRST {
            need_admin()?;
        }
        return dev.ioctl(cmd, arg);
    }
    match cmd {
        SIOCSIFFLAGS | SIOCSIFADDR | SIOCSIFNETMASK | SIOCRDHCP | SIOCRGATEWAY | SIOCRDNS => {
            need_admin()?
        }
        _ => {}
    }
    if cmd == SIOCRDHCP {
        let on: i32 = uaccess::read_user(data)?;
        super::set_dhcp(&name, on != 0)?;
        return Ok(0);
    }
    let r = super::with(|net| -> KResult<i64> {
        let ifc = net.iface_mut(&name).ok_or(ENODEV)?;
        match cmd {
            SIOCGIFFLAGS => {
                let mut f = if ifc.up { IFF_UP } else { 0 };
                if ifc.is_loopback() {
                    f |= IFF_LOOPBACK;
                } else {
                    f |= IFF_BROADCAST | IFF_MULTICAST;
                }
                if ifc.up && ifc.link_up() {
                    f |= IFF_RUNNING;
                }
                uaccess::write_user(data, &f)?;
            }
            SIOCSIFFLAGS => {
                let f: u16 = uaccess::read_user(data)?;
                ifc.up = f & IFF_UP != 0;
            }
            SIOCGIFADDR => write_sin(data, ifc.ipv4().ok_or(EADDRNOTAVAIL)?.address())?,
            SIOCGIFNETMASK => write_sin(data, ifc.ipv4().ok_or(EADDRNOTAVAIL)?.netmask())?,
            SIOCGIFBRDADDR => write_sin(
                data,
                ifc.ipv4()
                    .ok_or(EADDRNOTAVAIL)?
                    .broadcast()
                    .unwrap_or(Ipv4Address::BROADCAST),
            )?,
            SIOCSIFADDR => {
                let a = read_sin(data)?;
                let prefix = ifc
                    .ipv4()
                    .map_or(if a.octets()[0] < 128 { 8 } else { 24 }, |c| c.prefix_len());
                // A static address disables DHCP on the interface.
                ifc.stop_dhcp();
                if a.is_unspecified() {
                    ifc.set_ipv4(None);
                } else {
                    ifc.set_ipv4(Some(Ipv4Cidr::new(a, prefix)));
                }
            }
            SIOCSIFNETMASK => {
                let m = read_sin(data)?;
                let c = ifc.ipv4().ok_or(EADDRNOTAVAIL)?;
                let prefix = u32::from_be_bytes(m.octets()).count_ones() as u8;
                ifc.set_ipv4(Some(Ipv4Cidr::new(c.address(), prefix)));
            }
            SIOCGIFMTU => uaccess::write_user(data, &(ifc.mtu() as i32))?,
            SIOCGIFHWADDR => {
                let mut b = [0u8; 16];
                b[0..2].copy_from_slice(
                    &(if ifc.is_loopback() { 772u16 } else { 1u16 }).to_ne_bytes(),
                );
                b[2..8].copy_from_slice(&ifc.mac());
                uaccess::copy_to_user(data, &b)?;
            }
            SIOCGIFINDEX => uaccess::write_user(data, &(ifc.index as i32))?,
            SIOCRGATEWAY => {
                let g = read_sin(data)?;
                ifc.set_gateway((!g.is_unspecified()).then_some(g));
            }
            SIOCRDNS => {
                let n: u32 = uaccess::read_user(data)?;
                let mut v = Vec::new();
                for i in 0..n.min(3) as u64 {
                    let o: [u8; 4] = uaccess::read_user(data + 4 + i * 4)?;
                    v.push(Ipv4Address::from(o));
                }
                ifc.dns = v;
            }
            _ => return Err(ENOTTY),
        }
        Ok(0)
    })
    .ok_or(ENETDOWN)?;
    super::kick();
    r
}

#[allow(dead_code)]
fn _unused(_: IpCidr) {}
