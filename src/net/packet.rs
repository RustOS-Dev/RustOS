//! AF_PACKET sockets: raw (whole Ethernet frames) and datagram (payload
//! with a `sockaddr_ll` naming the peer) access to an interface, for
//! wpa_supplicant's EAPOL path, DHCP clients and servers, and packet
//! capture. Received frames are copied here before the IP stack sees them;
//! sent frames go straight to the driver.

use super::generic::{Ancillary, GenericSocket, Received};
use crate::errno::*;
use crate::sched::WaitQueue;
use crate::sync::IrqMutex as Mutex;
use crate::vfs::{FileLike, FileType, Metadata, POLLIN, POLLOUT};
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU16, AtomicU32, AtomicUsize, Ordering};

pub const AF_PACKET: u16 = 17;
const SOCK_DGRAM: u32 = 2;
const SOCK_RAW: u32 = 3;
/// Obsolete raw type (busybox arping): whole frames, addressed by
/// interface name (`struct sockaddr_pkt`).
const SOCK_PACKET: u32 = 10;
/// Every protocol (host byte order).
const ETH_P_ALL: u16 = 3;
const ARPHRD_ETHER: u16 = 1;

const PACKET_HOST: u8 = 0;
const PACKET_BROADCAST: u8 = 1;
const PACKET_MULTICAST: u8 = 2;
const PACKET_OTHERHOST: u8 = 3;
const PACKET_OUTGOING: u8 = 4;

/// Frames queued per socket; more are dropped.
const RX_LIMIT: usize = 256;

static SOCKETS: Mutex<Vec<Weak<PacketSocket>>> = Mutex::new(Vec::new());
/// Open packet sockets, so the receive path can skip the lock when none.
static OPEN: AtomicUsize = AtomicUsize::new(0);

/// A frame seen on interface `index` (MAC `mac`): copy it to the sockets
/// that want it. `outgoing` frames go only to ETH_P_ALL sockets, as on
/// Linux.
pub fn tap(index: u32, mac: [u8; 6], frame: &[u8], outgoing: bool) {
    if OPEN.load(Ordering::Relaxed) == 0 || frame.len() < 14 {
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let pkttype = if outgoing {
        PACKET_OUTGOING
    } else if frame[0..6] == [0xff; 6] {
        PACKET_BROADCAST
    } else if frame[0] & 1 != 0 {
        PACKET_MULTICAST
    } else if frame[0..6] == mac {
        PACKET_HOST
    } else {
        PACKET_OTHERHOST
    };
    let socks: Vec<Arc<PacketSocket>> = SOCKETS.lock().iter().filter_map(|w| w.upgrade()).collect();
    for s in socks {
        let proto = s.proto.load(Ordering::SeqCst);
        let bound = s.ifindex.load(Ordering::SeqCst);
        let wants = match proto {
            0 => false,
            ETH_P_ALL => true,
            p => p == ethertype && !outgoing,
        };
        if wants && (bound == 0 || bound == index) {
            s.deliver(index, ethertype, pkttype, frame);
        }
    }
}

struct Frame {
    data: Vec<u8>,
    from: Vec<u8>,
}

pub struct PacketSocket {
    ty: u32,
    /// Protocol (host byte order); 0 receives nothing.
    proto: AtomicU16,
    /// Bound interface; 0 for all.
    ifindex: AtomicU32,
    rx: Mutex<VecDeque<Frame>>,
    wq: WaitQueue,
}

/// `struct sockaddr_ll`.
fn sockaddr_ll(ifindex: u32, proto: u16, pkttype: u8, addr: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(20);
    v.extend_from_slice(&AF_PACKET.to_ne_bytes());
    v.extend_from_slice(&proto.to_be_bytes());
    v.extend_from_slice(&(ifindex as i32).to_ne_bytes());
    v.extend_from_slice(&ARPHRD_ETHER.to_ne_bytes());
    v.push(pkttype);
    v.push(addr.len().min(8) as u8);
    let mut a = [0u8; 8];
    let n = addr.len().min(8);
    a[..n].copy_from_slice(&addr[..n]);
    v.extend_from_slice(&a);
    v
}

/// Parsed `sockaddr_ll` (or `sockaddr_pkt`: interface name in
/// sa_data): (protocol, ifindex, address).
fn parse_ll(raw: &[u8]) -> KResult<(u16, u32, [u8; 6])> {
    if raw.len() >= 16 && u16::from_ne_bytes([raw[0], raw[1]]) != AF_PACKET {
        // sockaddr_pkt: family (any), device[14], protocol.
        let name = &raw[2..16];
        let n = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        let name = core::str::from_utf8(&name[..n]).map_err(|_| EINVAL)?;
        let index = super::with(|net| net.iface(name).map(|i| i.index))
            .flatten()
            .ok_or(ENODEV)?;
        let proto = raw
            .get(16..18)
            .map_or(0, |p| u16::from_be_bytes([p[0], p[1]]));
        return Ok((proto, index, [0; 6]));
    }
    if raw.len() < 12 || u16::from_ne_bytes([raw[0], raw[1]]) != AF_PACKET {
        return Err(EINVAL);
    }
    let proto = u16::from_be_bytes([raw[2], raw[3]]);
    let ifindex = i32::from_ne_bytes(raw[4..8].try_into().unwrap()) as u32;
    let mut addr = [0u8; 6];
    if raw.len() >= 18 {
        addr.copy_from_slice(&raw[12..18]);
    }
    Ok((proto, ifindex, addr))
}

impl PacketSocket {
    pub fn new(ty: u32, protocol: u32) -> KResult<Arc<PacketSocket>> {
        if ty != SOCK_RAW && ty != SOCK_DGRAM && ty != SOCK_PACKET {
            return Err(ESOCKTNOSUPPORT);
        }
        if crate::process::current().is_some_and(|p| p.uid.load(Ordering::Relaxed) != 0) {
            return Err(EPERM);
        }
        let s = Arc::new(PacketSocket {
            ty,
            // The protocol argument is in network byte order.
            proto: AtomicU16::new(u16::from_be(protocol as u16)),
            ifindex: AtomicU32::new(0),
            rx: Mutex::new(VecDeque::new()),
            wq: WaitQueue::new(),
        });
        let mut list = SOCKETS.lock();
        list.retain(|w| w.strong_count() > 0);
        list.push(Arc::downgrade(&s));
        OPEN.store(list.len(), Ordering::SeqCst);
        Ok(s)
    }

    fn deliver(&self, index: u32, ethertype: u16, pkttype: u8, frame: &[u8]) {
        let data = if self.ty != SOCK_DGRAM {
            frame.to_vec()
        } else {
            frame[14..].to_vec()
        };
        let from = sockaddr_ll(index, ethertype, pkttype, &frame[6..12]);
        {
            let mut rx = self.rx.lock();
            if rx.len() >= RX_LIMIT {
                return;
            }
            rx.push_back(Frame { data, from });
        }
        self.wq.wake_all();
    }

    /// The interface's device and MAC.
    fn device(index: u32) -> KResult<(Arc<dyn super::NetDevice>, [u8; 6])> {
        super::with(|net| {
            net.by_index(index)
                .and_then(|i| i.device().cloned())
                .map(|d| {
                    let m = d.mac();
                    (d, m)
                })
                .ok_or(ENXIO)
        })
        .ok_or(ENODEV)?
    }
}

impl Drop for PacketSocket {
    fn drop(&mut self) {
        let mut list = SOCKETS.lock();
        list.retain(|w| w.strong_count() > 0);
        OPEN.store(list.len(), Ordering::SeqCst);
    }
}

impl GenericSocket for PacketSocket {
    fn sock_type(&self) -> u32 {
        self.ty
    }

    fn family(&self) -> u16 {
        AF_PACKET
    }

    fn bind(&self, addr: &[u8]) -> KResult<()> {
        let (proto, ifindex, _) = parse_ll(addr)?;
        if ifindex != 0 {
            Self::device(ifindex)?;
        }
        self.ifindex.store(ifindex, Ordering::SeqCst);
        if proto != 0 {
            self.proto.store(proto, Ordering::SeqCst);
        }
        Ok(())
    }

    fn connect(&self, _addr: &[u8], _nonblock: bool) -> KResult<()> {
        Err(EOPNOTSUPP)
    }

    fn send(
        &self,
        data: &[u8],
        dest: Option<&[u8]>,
        _anc: Ancillary,
        _nonblock: bool,
    ) -> KResult<usize> {
        let (proto, index, addr) = match dest {
            Some(d) => parse_ll(d)?,
            None => (
                self.proto.load(Ordering::SeqCst),
                self.ifindex.load(Ordering::SeqCst),
                [0; 6],
            ),
        };
        let index = if index == 0 {
            self.ifindex.load(Ordering::SeqCst)
        } else {
            index
        };
        if index == 0 {
            return Err(ENXIO);
        }
        let (dev, mac) = Self::device(index)?;
        let frame = if self.ty != SOCK_DGRAM {
            if data.len() < 14 {
                return Err(EINVAL);
            }
            data.to_vec()
        } else {
            let mut f = Vec::with_capacity(14 + data.len());
            f.extend_from_slice(&addr);
            f.extend_from_slice(&mac);
            f.extend_from_slice(&proto.to_be_bytes());
            f.extend_from_slice(data);
            f
        };
        if frame.len() > dev.mtu() + 14 {
            return Err(EMSGSIZE);
        }
        dev.transmit(&frame).map_err(|_| ENOBUFS)?;
        tap(index, mac, &frame, true);
        Ok(data.len())
    }

    fn recv(&self, buf: &mut [u8], nonblock: bool, peek: bool) -> KResult<Received> {
        loop {
            {
                let mut rx = self.rx.lock();
                if let Some(f) = rx.front() {
                    let n = f.data.len().min(buf.len());
                    buf[..n].copy_from_slice(&f.data[..n]);
                    let full = f.data.len();
                    let from = f.from.clone();
                    if !peek {
                        rx.pop_front();
                    }
                    return Ok(Received {
                        len: n,
                        full_len: full,
                        from: Some(from),
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
        let index = self.ifindex.load(Ordering::SeqCst);
        let mac = Self::device(index).map(|(_, m)| m).unwrap_or([0; 6]);
        sockaddr_ll(index, self.proto.load(Ordering::SeqCst), 0, &mac[..6])
    }

    /// PACKET_ADD_MEMBERSHIP (multicast, promiscuous), SO_ATTACH_FILTER
    /// and buffer sizes are accepted; every frame for the socket's
    /// protocol is delivered regardless.
    fn setsockopt(&self, _level: u32, _name: u32, _val: &[u8]) -> KResult<()> {
        Ok(())
    }
}

impl FileLike for PacketSocket {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        Ok(self.recv(buf, nonblock, false)?.len)
    }

    fn write(&self, buf: &[u8], nonblock: bool) -> KResult<usize> {
        self.send(buf, None, Ancillary::default(), nonblock)
    }

    fn poll(&self) -> u16 {
        let mut ev = POLLOUT;
        if !self.rx.lock().is_empty() {
            ev |= POLLIN;
        }
        ev
    }

    fn wait_queue(&self) -> &WaitQueue {
        &self.wq
    }

    /// Interface ioctls (SIOCGIFINDEX, SIOCGIFHWADDR, ...).
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
