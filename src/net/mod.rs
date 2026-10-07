//! Networking: device registry, interfaces and the protocol stack.
//!
//! Drivers implement [`NetDevice`] and call [`register`]; each device gets
//! an interface (`eth0`, `wlan0`, ...) with its own smoltcp `Interface` and
//! socket set, and by default a DHCP client. Connected sockets live in the
//! set of the interface their traffic is routed through; wildcard listeners
//! get one smoltcp socket per interface. A single kernel thread ("netd")
//! polls every interface; socket syscalls work under the same lock and
//! kick the thread when they queue data.

pub mod dhcpv6;
pub mod generic;
pub mod netlink;
pub mod packet;
pub mod rtnetlink;
pub mod socket;
pub mod syscalls;
pub mod unix;

/// Sockets of families other than IP and AF_UNIX (netlink, packet), or
/// `None` if `domain` is not one of them.
pub fn other_family_socket(
    domain: u16,
    kind: u32,
    protocol: u32,
) -> Option<crate::errno::KResult<alloc::sync::Arc<dyn crate::vfs::FileLike>>> {
    match domain {
        netlink::AF_NETLINK => Some(
            netlink::NetlinkSocket::new(kind, protocol)
                .map(|s| s as alloc::sync::Arc<dyn crate::vfs::FileLike>),
        ),
        packet::AF_PACKET => Some(
            packet::PacketSocket::new(kind, protocol)
                .map(|s| s as alloc::sync::Arc<dyn crate::vfs::FileLike>),
        ),
        _ => None,
    }
}

use crate::errno::*;
use crate::sched::WaitQueue;
use crate::sched::mutex::Mutex;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Loopback, Medium};
use smoltcp::socket::{dhcpv4, tcp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr, Ipv6Address,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IfKind {
    Ethernet,
    Wireless,
}

/// A network interface card.
pub trait NetDevice: Send + Sync {
    fn mac(&self) -> [u8; 6];
    fn mtu(&self) -> usize {
        1500
    }
    fn link_up(&self) -> bool {
        true
    }
    /// Link speed in Mbit/s, if known.
    fn speed(&self) -> Option<u32> {
        None
    }
    fn kind(&self) -> IfKind {
        IfKind::Ethernet
    }
    fn driver(&self) -> &'static str;
    /// Queue an Ethernet frame for transmission.
    fn transmit(&self, frame: &[u8]) -> KResult<()>;
    /// Whether the driver takes frames now. While it does not, the stack
    /// keeps its packets (TCP retransmits nothing); the driver calls
    /// [`kick`] when it has room again.
    fn tx_ready(&self) -> bool {
        true
    }
    /// Take the next received Ethernet frame (called by the network thread).
    fn receive(&self) -> Option<Vec<u8>>;
    /// Driver-specific control (wireless configuration etc.).
    fn ioctl(&self, _cmd: u64, _arg: u64) -> KResult<i64> {
        Err(ENOTTY)
    }
    /// Human-readable state for /proc/net/wireless and `wifi status`.
    fn status(&self) -> String {
        String::new()
    }
    /// Stop DMA before reboot / power-off.
    fn shutdown(&self) {}
    /// The interface is being brought up or down (`ip link set`,
    /// SIOCSIFFLAGS). Called without the network lock held.
    fn set_up(&self, _up: bool) -> KResult<()> {
        Ok(())
    }
    /// Change the hardware address (SIOCSIFHWADDR). Called without the
    /// network lock held.
    fn set_mac(&self, _mac: [u8; 6]) -> KResult<()> {
        Err(EOPNOTSUPP)
    }
    /// The device carries bare IP packets, without a link-layer header or
    /// address (tunnels such as WireGuard): `transmit` and `receive` take
    /// and give IP packets, and there is no ARP or DHCP.
    fn ip_only(&self) -> bool {
        false
    }
    /// The link kind the interface was created with (`ip link add NAME
    /// type KIND`), reported in IFLA_LINKINFO.
    fn link_kind(&self) -> Option<&'static str> {
        None
    }
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
}

pub struct Iface {
    pub name: String,
    /// Stable identifier (never reused).
    pub index: u32,
    dev: Option<Arc<dyn NetDevice>>,
    lo: Option<Loopback>,
    pub(crate) iface: Interface,
    pub(crate) sockets: SocketSet<'static>,
    dhcp: Option<SocketHandle>,
    /// TCP sockets closed by their owners, kept until shutdown completes.
    closing: Vec<(SocketHandle, u64)>,
    pub up: bool,
    pub stats: Stats,
    pub dns: Vec<Ipv4Address>,
    pub gateway: Option<Ipv4Address>,
    /// Extra IPv4 routes: (network, gateway).
    pub routes: Vec<(Ipv4Cidr, Ipv4Address)>,
    link: bool,
    /// Neighbours seen in ARP traffic.
    pub arp: BTreeMap<[u8; 4], [u8; 6]>,
    /// IPv6 DNS servers (router advertisement RDNSS, DHCPv6).
    pub dns6: Vec<Ipv6Address>,
    /// DNS search domains (RA DNSSL, DHCPv6).
    pub search: Vec<String>,
    /// Captive-portal URI and where it came from (RFC 8910).
    pub portal: Option<(String, &'static str)>,
    /// Latest router advertisement, handed over by the receive path.
    pending_ra: Option<netproto::ra::RouterAdvert>,
    dhcp6: Option<dhcpv6::Client>,
    /// Address leased through DHCPv6 IA_NA.
    dhcp6_addr: Option<Ipv6Address>,
}

impl Iface {
    fn new(
        name: String,
        index: u32,
        dev: Option<Arc<dyn NetDevice>>,
        lo: Option<Loopback>,
        iface: Interface,
    ) -> Iface {
        let link = dev.as_ref().is_none_or(|d| d.link_up());
        Iface {
            name,
            index,
            dev,
            lo,
            iface,
            sockets: SocketSet::new(Vec::new()),
            dhcp: None,
            closing: Vec::new(),
            up: true,
            stats: Stats::default(),
            dns: Vec::new(),
            gateway: None,
            routes: Vec::new(),
            link,
            arp: BTreeMap::new(),
            dns6: Vec::new(),
            search: Vec::new(),
            portal: None,
            pending_ra: None,
            dhcp6: None,
            dhcp6_addr: None,
        }
    }

    pub fn is_loopback(&self) -> bool {
        self.lo.is_some()
    }
    /// A point-to-point interface for bare IP packets (a tunnel).
    pub fn is_ip_only(&self) -> bool {
        self.dev.as_ref().is_some_and(|d| d.ip_only())
    }
    pub fn device(&self) -> Option<&Arc<dyn NetDevice>> {
        self.dev.as_ref()
    }
    pub fn mac(&self) -> [u8; 6] {
        self.dev.as_ref().map_or([0; 6], |d| d.mac())
    }
    pub fn mtu(&self) -> usize {
        self.dev.as_ref().map_or(65536, |d| d.mtu())
    }
    pub fn link_up(&self) -> bool {
        self.dev.as_ref().is_none_or(|d| d.link_up())
    }
    pub fn ipv4(&self) -> Option<Ipv4Cidr> {
        self.iface.ip_addrs().iter().find_map(|c| match c {
            IpCidr::Ipv4(c) => Some(*c),
            #[allow(unreachable_patterns)]
            _ => None,
        })
    }
    pub fn ip_addrs(&self) -> Vec<IpCidr> {
        self.iface.ip_addrs().to_vec()
    }
    pub fn dhcp_enabled(&self) -> bool {
        self.dhcp.is_some()
    }

    /// Replace the IPv4 address (keeps IPv6 addresses).
    pub fn set_ipv4(&mut self, cidr: Option<Ipv4Cidr>) {
        self.iface.update_ip_addrs(|addrs| {
            addrs.retain(|a| !matches!(a, IpCidr::Ipv4(_)));
            if let Some(c) = cidr {
                let _ = addrs.push(IpCidr::Ipv4(c));
            }
        });
        self.apply_routes();
    }

    pub fn set_gateway(&mut self, gw: Option<Ipv4Address>) {
        self.gateway = gw;
        self.apply_routes();
    }

    /// Stop the DHCP client (a static address was configured).
    pub fn stop_dhcp(&mut self) {
        if let Some(h) = self.dhcp.take() {
            self.sockets.remove(h);
        }
    }

    fn apply_routes(&mut self) {
        let gw = self.gateway;
        let extra = self.routes.clone();
        self.iface.routes_mut().update(|r| r.clear());
        if let Some(g) = gw {
            let _ = self.iface.routes_mut().add_default_ipv4_route(g);
        }
        for (net, via) in extra {
            self.iface.routes_mut().update(|r| {
                let _ = r.push(smoltcp::iface::Route {
                    cidr: IpCidr::Ipv4(net),
                    via_router: IpAddress::Ipv4(via),
                    preferred_until: None,
                    expires_at: None,
                });
            });
        }
    }

    /// How well this interface reaches `addr` (higher is better).
    fn route_score(&self, addr: &IpAddress) -> Option<u8> {
        if !self.up {
            return None;
        }
        if is_loopback_addr(addr) {
            return self.is_loopback().then_some(200);
        }
        for c in self.iface.ip_addrs() {
            if c.contains_addr(addr) || c.address() == *addr {
                return Some(c.prefix_len() + 1);
            }
        }
        if let IpAddress::Ipv4(a) = addr {
            if let Some((net, _)) = self.routes.iter().find(|(n, _)| n.contains_addr(a)) {
                return Some(net.prefix_len());
            }
            if a.is_broadcast() && self.ipv4().is_some() {
                return Some(1);
            }
            if self.gateway.is_some() && self.link_up() {
                return Some(0);
            }
        }
        None
    }

    /// Queue a TCP socket for removal once it has finished closing.
    pub fn retire(&mut self, h: SocketHandle) {
        self.closing.push((h, crate::time::millis() + 60_000));
    }

    fn dhcp_poll(&mut self) {
        let Some(h) = self.dhcp else { return };
        let event = self.sockets.get_mut::<dhcpv4::Socket>(h).poll();
        match event {
            Some(dhcpv4::Event::Configured(cfg)) => {
                let (addr, router) = (cfg.address, cfg.router);
                let dns: Vec<Ipv4Address> = cfg.dns_servers.iter().copied().collect();
                crate::println!(
                    "[net] {}: DHCP lease {} gateway {} dns {:?}",
                    self.name,
                    addr,
                    router.map_or(String::from("-"), |r| format!("{}", r)),
                    dns
                );
                let portal = cfg.packet.as_ref().and_then(|p| {
                    p.options()
                        .find(|o| o.kind == 114)
                        .and_then(|o| netproto::capport::parse_uri(o.data))
                });
                self.set_ipv4(Some(addr));
                self.set_gateway(router);
                self.dns = dns;
                if let Some(url) = portal {
                    crate::println!("[net] {}: captive portal {} (DHCP)", self.name, url);
                    self.portal = Some((url, "dhcp"));
                } else if self.portal.as_ref().is_some_and(|p| p.1 == "dhcp") {
                    self.portal = None;
                }
                self.write_resolv_conf();
            }
            Some(dhcpv4::Event::Deconfigured) => {
                crate::println!("[net] {}: DHCP lease lost", self.name);
                self.set_ipv4(None);
                self.set_gateway(None);
                if self.portal.as_ref().is_some_and(|p| p.1 == "dhcp") {
                    self.portal = None;
                }
            }
            None => {}
        }
    }

    fn write_resolv_conf(&self) {
        write_resolv_conf(&self.dns, &self.dns6, &self.search);
    }

    /// Act on a router advertisement: IPv6 DNS servers, search domains,
    /// the captive-portal option, and DHCPv6 for the M/O flags.
    fn handle_ra(&mut self, ra: netproto::ra::RouterAdvert) {
        let now = crate::time::millis();
        let mut changed = false;
        for (a, life) in &ra.dns {
            let a = Ipv6Address::from(*a);
            if *life == 0 {
                changed |= self.dns6.contains(&a);
                self.dns6.retain(|d| *d != a);
            } else if !self.dns6.contains(&a) {
                self.dns6.push(a);
                changed = true;
            }
        }
        for d in &ra.search {
            if !self.search.contains(d) {
                self.search.push(d.clone());
                changed = true;
            }
        }
        // A source only replaces its own announcement (DHCPv4 wins).
        if let Some(url) = &ra.captive_portal
            && self
                .portal
                .as_ref()
                .is_none_or(|p| p.1 == "ra" && p.0 != *url)
        {
            crate::println!(
                "[net] {}: captive portal {} (router advertisement)",
                self.name,
                url
            );
            self.portal = Some((url.clone(), "ra"));
        }
        if changed {
            crate::println!(
                "[net] {}: IPv6 DNS {:?} (router advertisement)",
                self.name,
                self.dns6
            );
            self.write_resolv_conf();
        }
        let want = if ra.managed {
            Some(true)
        } else if ra.other {
            Some(false)
        } else {
            None
        };
        match (want, self.dhcp6.as_ref().map(|c| c.stateful)) {
            (Some(w), Some(cur)) if w == cur => {}
            (Some(w), _) => {
                if let Some(old) = self.dhcp6.take()
                    && let Some(a) = old.remove(&mut self.sockets)
                {
                    self.drop_v6_addr(a);
                }
                crate::println!(
                    "[net] {}: starting DHCPv6 ({})",
                    self.name,
                    if w { "stateful" } else { "stateless" }
                );
                let mac = self.mac();
                self.dhcp6 = Some(dhcpv6::Client::new(&mut self.sockets, mac, w, now));
            }
            (None, _) => {}
        }
    }

    fn drop_v6_addr(&mut self, a: Ipv6Address) {
        self.iface
            .update_ip_addrs(|addrs| addrs.retain(|c| c.address() != IpAddress::Ipv6(a)));
        if self.dhcp6_addr == Some(a) {
            self.dhcp6_addr = None;
        }
    }

    fn dhcp6_poll(&mut self) -> Option<u64> {
        let now = crate::time::millis();
        let client = self.dhcp6.as_mut()?;
        let upd = client.poll(&mut self.sockets, now);
        let delay = client.delay(now);
        if let Some(u) = upd {
            if let Some(a) = u.lost {
                crate::println!("[net] {}: DHCPv6 address {} expired", self.name, a);
                self.drop_v6_addr(a);
            }
            if let Some((a, valid)) = u.addr
                && self.dhcp6_addr != Some(a)
            {
                if let Some(old) = self.dhcp6_addr {
                    self.drop_v6_addr(old);
                }
                crate::println!(
                    "[net] {}: DHCPv6 address {}/128 (valid {} s)",
                    self.name,
                    a,
                    valid
                );
                self.iface.update_ip_addrs(|addrs| {
                    let _ = addrs.push(IpCidr::new(IpAddress::Ipv6(a), 128));
                });
                self.dhcp6_addr = Some(a);
            }
            let mut changed = false;
            for d in u.dns {
                if !self.dns6.contains(&d) {
                    self.dns6.push(d);
                    changed = true;
                }
            }
            for d in u.domains {
                if !self.search.contains(&d) {
                    self.search.push(d);
                    changed = true;
                }
            }
            if changed {
                crate::println!("[net] {}: IPv6 DNS {:?} (DHCPv6)", self.name, self.dns6);
                self.write_resolv_conf();
            }
            if let Some(url) = u.portal
                && self
                    .portal
                    .as_ref()
                    .is_none_or(|p| (p.1 == "dhcpv6" || p.1 == "ra") && p.0 != url)
            {
                crate::println!("[net] {}: captive portal {} (DHCPv6)", self.name, url);
                self.portal = Some((url, "dhcpv6"));
            }
        }
        Some(delay)
    }

    /// Poll this interface. Returns the delay until it wants polling again.
    fn poll(&mut self, now: Instant) -> Option<u64> {
        if !self.up {
            return None;
        }
        if let Some(dev) = self.dev.as_ref().filter(|d| !d.ip_only()) {
            // The driver may change its address (SIOCSIFHWADDR, MLO).
            let mac = HardwareAddress::Ethernet(EthernetAddress(dev.mac()));
            if self.iface.hardware_addr() != mac {
                self.iface.set_hardware_addr(mac);
            }
        }
        let link = self.link_up();
        if link != self.link {
            self.link = link;
            crate::println!(
                "[net] {}: link {}",
                self.name,
                if link { "up" } else { "down" }
            );
            rtnetlink::link_event(self);
            if let Some(h) = self.dhcp {
                self.sockets.get_mut::<dhcpv4::Socket>(h).reset();
            }
            // A new link may be a new network: forget IPv6 configuration
            // and the portal; the next RA starts over.
            if let Some(c) = self.dhcp6.take()
                && let Some(a) = c.remove(&mut self.sockets)
            {
                self.drop_v6_addr(a);
            }
            self.dns6.clear();
            self.search.clear();
            self.portal = None;
        }
        if let Some(lo) = self.lo.as_mut() {
            self.iface.poll(now, lo, &mut self.sockets);
        } else if let Some(dev) = self.dev.clone() {
            if !link {
                // Frames while the link is down reach packet sockets only
                // (EAPOL can precede the carrier).
                while let Some(f) = dev.receive() {
                    packet::tap(self.index, dev.mac(), &f, false);
                }
                return None;
            }
            let ip = dev.ip_only();
            let mut p = Phy {
                index: self.index,
                dev: &dev,
                ip,
                stats: &mut self.stats,
                arp: (!ip).then_some(&mut self.arp),
                ra: (!ip).then_some(&mut self.pending_ra),
            };
            self.iface.poll(now, &mut p, &mut self.sockets);
        }
        self.dhcp_poll();
        if let Some(ra) = self.pending_ra.take() {
            self.handle_ra(ra);
        }
        let dhcp6_delay = self.dhcp6_poll();
        // Reap sockets whose close has completed.
        let t = crate::time::millis();
        let sockets = &mut self.sockets;
        self.closing.retain(|&(h, deadline)| {
            let s = sockets.get_mut::<tcp::Socket>(h);
            let done = matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait);
            if done || t >= deadline {
                s.abort();
                sockets.remove(h);
                false
            } else {
                true
            }
        });
        let d = self
            .iface
            .poll_delay(now, &self.sockets)
            .map(|d| d.total_millis());
        match (d, dhcp6_delay) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

pub fn is_loopback_addr(a: &IpAddress) -> bool {
    match a {
        IpAddress::Ipv4(v) => v.is_loopback(),
        IpAddress::Ipv6(v) => v.is_loopback(),
    }
}

/// smoltcp device adapter for a driver.
struct Phy<'a> {
    /// Interface index (for packet sockets).
    index: u32,
    dev: &'a Arc<dyn NetDevice>,
    /// Bare IP packets instead of Ethernet frames (`NetDevice::ip_only`).
    ip: bool,
    stats: &'a mut Stats,
    arp: Option<&'a mut BTreeMap<[u8; 4], [u8; 6]>>,
    /// Receives router advertisements seen on the wire.
    ra: Option<&'a mut Option<netproto::ra::RouterAdvert>>,
}

/// One-line description of an Ethernet frame (`net.debug=1`).
fn frame_summary(f: &[u8]) -> String {
    if f.len() < 14 {
        return format!("runt {} bytes", f.len());
    }
    let et = u16::from_be_bytes([f[12], f[13]]);
    let p = &f[14..];
    let mac = |m: &[u8]| {
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    };
    let body = match et {
        0x0800 if p.len() >= 20 => {
            let ihl = ((p[0] & 15) as usize) * 4;
            let ports = if (p[9] == 6 || p[9] == 17) && p.len() >= ihl + 4 {
                format!(
                    " {}->{}",
                    u16::from_be_bytes([p[ihl], p[ihl + 1]]),
                    u16::from_be_bytes([p[ihl + 2], p[ihl + 3]])
                )
            } else {
                String::new()
            };
            let proto = match p[9] {
                1 => "icmp",
                6 => "tcp",
                17 => "udp",
                _ => "ip",
            };
            format!(
                "{} {}.{}.{}.{} > {}.{}.{}.{}{}",
                proto, p[12], p[13], p[14], p[15], p[16], p[17], p[18], p[19], ports
            )
        }
        0x0806 if p.len() >= 28 => format!(
            "arp op {} {}.{}.{}.{} > {}.{}.{}.{}",
            p[7], p[14], p[15], p[16], p[17], p[24], p[25], p[26], p[27]
        ),
        0x86DD if p.len() >= 40 => format!("ipv6 next {}", p[6]),
        0x888E => String::from("eapol"),
        _ => format!("type {:#06x}", et),
    };
    format!(
        "{} > {} {} ({} bytes)",
        mac(&f[6..12]),
        mac(&f[0..6]),
        body,
        f.len()
    )
}

struct RxTok(Vec<u8>);
struct TxTok<'a> {
    index: u32,
    dev: &'a Arc<dyn NetDevice>,
    ip: bool,
    stats: *mut Stats,
}

impl phy::RxToken for RxTok {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for TxTok<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        // SAFETY: the stats outlive the token (both borrowed from the Phy).
        let stats = unsafe { &mut *self.stats };
        if crate::params::NET_DEBUG.load(Ordering::Relaxed) && !self.ip {
            crate::println!("[net] tx {}", frame_summary(&buf));
        }
        match self.dev.transmit(&buf) {
            Ok(()) => {
                stats.tx_packets += 1;
                stats.tx_bytes += len as u64;
                // Packet sockets see Ethernet frames only.
                if !self.ip {
                    packet::tap(self.index, self.dev.mac(), &buf, true);
                }
            }
            Err(_) => stats.tx_errors += 1,
        }
        r
    }
}

impl Device for Phy<'_> {
    type RxToken<'a>
        = RxTok
    where
        Self: 'a;
    type TxToken<'a>
        = TxTok<'a>
    where
        Self: 'a;

    fn receive(&mut self, _t: Instant) -> Option<(RxTok, TxTok<'_>)> {
        // A frame may need an answer (ACK, ARP reply): leave it queued
        // until the driver can send.
        if !self.dev.tx_ready() {
            return None;
        }
        let frame = self.dev.receive()?;
        if crate::params::NET_DEBUG.load(Ordering::Relaxed) && !self.ip {
            crate::println!("[net] rx {}", frame_summary(&frame));
        }
        // Learn neighbours from ARP traffic for /proc/net/arp.
        if frame.len() >= 42
            && frame[12..14] == [0x08, 0x06]
            && let Some(arp) = self.arp.as_mut()
        {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&frame[22..28]);
            let ip = [frame[28], frame[29], frame[30], frame[31]];
            if ip != [0; 4] {
                if arp.len() >= 256 {
                    arp.clear();
                }
                arp.insert(ip, mac);
            }
        }
        // Router advertisements: smoltcp does SLAAC itself, but the M/O
        // flags, RDNSS and captive-portal options are ours to act on.
        if frame.len() > 54
            && frame[12..14] == [0x86, 0xDD]
            && frame[20] == 58
            && frame[54] == 134
            && let Some(slot) = self.ra.as_mut()
            && let Some(ra) = netproto::ra::parse_frame(&frame)
        {
            **slot = Some(ra);
        }
        self.stats.rx_packets += 1;
        self.stats.rx_bytes += frame.len() as u64;
        if !self.ip {
            packet::tap(self.index, self.dev.mac(), &frame, false);
        }
        Some((
            RxTok(frame),
            TxTok {
                index: self.index,
                dev: self.dev,
                ip: self.ip,
                stats: self.stats as *mut Stats,
            },
        ))
    }

    fn transmit(&mut self, _t: Instant) -> Option<TxTok<'_>> {
        if !self.dev.tx_ready() {
            return None;
        }
        Some(TxTok {
            index: self.index,
            dev: self.dev,
            ip: self.ip,
            stats: self.stats as *mut Stats,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        if self.ip {
            c.medium = Medium::Ip;
            c.max_transmission_unit = self.dev.mtu();
            c.max_burst_size = Some(32);
            return c;
        }
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = self.dev.mtu() + 14;
        c.max_burst_size = Some(32);
        c
    }
}

pub struct Net {
    pub ifaces: Vec<Iface>,
    next_index: u32,
}

impl Net {
    pub fn iface(&self, name: &str) -> Option<&Iface> {
        self.ifaces.iter().find(|i| i.name == name)
    }
    pub fn iface_mut(&mut self, name: &str) -> Option<&mut Iface> {
        self.ifaces.iter_mut().find(|i| i.name == name)
    }
    pub fn by_index(&mut self, index: u32) -> Option<&mut Iface> {
        self.ifaces.iter_mut().find(|i| i.index == index)
    }

    /// The interface used to reach `addr` (best match), by stable index.
    pub fn route(&self, addr: &IpAddress) -> Option<u32> {
        self.ifaces
            .iter()
            .filter_map(|i| i.route_score(addr).map(|s| (s, i.index)))
            .max_by_key(|(s, _)| *s)
            .map(|(_, n)| n)
    }

    /// The interface that owns local address `addr`.
    pub fn owner_of(&self, addr: &IpAddress) -> Option<u32> {
        if is_loopback_addr(addr) {
            return self
                .ifaces
                .iter()
                .find(|i| i.is_loopback())
                .map(|i| i.index);
        }
        self.ifaces
            .iter()
            .find(|i| i.iface.ip_addrs().iter().any(|c| c.address() == *addr))
            .map(|i| i.index)
    }

    pub fn up_indices(&self) -> Vec<u32> {
        self.ifaces
            .iter()
            .filter(|i| i.up)
            .map(|i| i.index)
            .collect()
    }

    /// Poll every interface: (milliseconds until the next poll is due,
    /// whether any packet moved).
    fn poll_all(&mut self) -> (Option<u64>, bool) {
        let now = Instant::from_millis(crate::time::millis() as i64);
        let mut next: Option<u64> = None;
        let mut moved = false;
        for ifc in self.ifaces.iter_mut() {
            let before = ifc.stats.rx_packets + ifc.stats.tx_packets;
            if let Some(d) = ifc.poll(now) {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
            moved |= ifc.stats.rx_packets + ifc.stats.tx_packets != before;
        }
        (next, moved)
    }
}

pub static NET: Mutex<Option<Net>> = Mutex::new(None);
static NET_WQ: WaitQueue = WaitQueue::new();
/// Socket waiters block here; woken after every stack poll.
pub static SOCK_WQ: WaitQueue = WaitQueue::new();
pub static EPOCH: AtomicU64 = AtomicU64::new(0);
static KICK: AtomicBool = AtomicBool::new(false);

/// Wake the network thread (from drivers' receive interrupts or after a
/// socket operation queued data). Interrupt-safe.
/// Quiesce every network device (reboot / power-off).
pub fn shutdown_devices() {
    let devs: Vec<Arc<dyn NetDevice>> =
        with(|net| net.ifaces.iter().filter_map(|i| i.dev.clone()).collect()).unwrap_or_default();
    for d in devs {
        d.shutdown();
    }
}

pub fn kick() {
    KICK.store(true, Ordering::SeqCst);
    NET_WQ.wake_all();
}

/// Run `f` with the stack locked. Returns None before initialisation.
pub fn with<R>(f: impl FnOnce(&mut Net) -> R) -> Option<R> {
    let mut g = NET.lock();
    g.as_mut().map(f)
}

fn random_seed() -> u64 {
    let mut b = [0u8; 8];
    crate::drivers::random::fill(&mut b);
    u64::from_ne_bytes(b)
}

fn write_resolv_conf(dns: &[Ipv4Address], dns6: &[Ipv6Address], search: &[String]) {
    if dns.is_empty() && dns6.is_empty() {
        return;
    }
    let mut s = String::from("# generated by the kernel (DHCP, DHCPv6, router advertisements)\n");
    if !search.is_empty() {
        let _ = writeln!(s, "search {}", search.join(" "));
    }
    for d in dns {
        let _ = writeln!(s, "nameserver {}", d);
    }
    for d in dns6 {
        let _ = writeln!(s, "nameserver {}", d);
    }
    let _ = crate::vfs::write_all("/etc/resolv.conf", s.as_bytes());
}

/// A DHCPv4 client that also asks for the domain name and captive-portal
/// URI (option 114) and keeps the raw reply to read them.
fn new_dhcp4() -> dhcpv4::Socket<'static> {
    static REQUEST: [u8; 5] = [1, 3, 6, 15, 114];
    let mut s = dhcpv4::Socket::new();
    s.set_parameter_request_list(&REQUEST);
    s.set_receive_packet_buffer(alloc::boxed::Box::leak(vec![0u8; 1500].into_boxed_slice()));
    s
}

/// Register a NIC. Returns the interface name. DHCP starts automatically.
pub fn register(dev: Arc<dyn NetDevice>) -> String {
    register_named(dev, None).0
}

/// True if no interface is called `name`.
pub fn name_free(name: &str) -> bool {
    with(|net| net.iface(name).is_none()).unwrap_or(true)
}

/// Register an interface under `name` (if given and free) or the next
/// free ethN/wlanN; returns its name and index.
pub fn register_named(dev: Arc<dyn NetDevice>, want: Option<&str>) -> (String, u32) {
    let mut g = NET.lock();
    let net = g.as_mut().expect("net::init not called");
    let prefix = match dev.kind() {
        IfKind::Ethernet => "eth",
        IfKind::Wireless => "wlan",
    };
    let name = match want {
        Some(w) if !w.is_empty() && net.iface(w).is_none() => String::from(w),
        _ => {
            let n = (0..)
                .find(|n| net.iface(&format!("{}{}", prefix, n)).is_none())
                .unwrap();
            format!("{}{}", prefix, n)
        }
    };
    let mac = dev.mac();
    // Tunnels have no link layer: no hardware address, ARP or SLAAC.
    let ip = dev.ip_only();
    let mut config = Config::new(if ip {
        HardwareAddress::Ip
    } else {
        HardwareAddress::Ethernet(EthernetAddress(mac))
    });
    config.random_seed = random_seed();
    // IPv6 stateless autoconfiguration from router advertisements.
    config.slaac = !ip;
    let now = Instant::from_millis(crate::time::millis() as i64);
    let mut stats = Stats::default();
    let mut p = Phy {
        index: 0,
        dev: &dev,
        ip,
        stats: &mut stats,
        arp: None,
        ra: None,
    };
    let mut iface = Interface::new(config, &mut p, now);
    if !ip {
        // IPv6 link-local address from the MAC (EUI-64).
        let ll = Ipv6Address::new(
            0xfe80,
            0,
            0,
            0,
            u16::from_be_bytes([mac[0] ^ 2, mac[1]]),
            u16::from_be_bytes([mac[2], 0xff]),
            u16::from_be_bytes([0xfe, mac[3]]),
            u16::from_be_bytes([mac[4], mac[5]]),
        );
        iface.update_ip_addrs(|a| {
            let _ = a.push(IpCidr::new(IpAddress::Ipv6(ll), 64));
        });
    }
    let index = net.next_index;
    net.next_index += 1;
    crate::println!(
        "[net] {}: {} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}{}",
        name,
        dev.driver(),
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        dev.speed()
            .map_or(String::new(), |s| format!(", {} Mbit/s", s))
    );
    let mut ifc = Iface::new(name.clone(), index, Some(dev), None, iface);
    if !ip {
        ifc.dhcp = Some(ifc.sockets.add(new_dhcp4()));
    }
    net.ifaces.push(ifc);
    drop(g);
    kick();
    (name, index)
}

/// Bring an interface up or down: the driver first (without the network
/// lock, as it may call back into the stack), then the stack.
pub fn set_link_up(name: &str, up: bool) -> KResult<()> {
    let (dev, cur) = with(|net| {
        net.iface(name)
            .map(|i| (i.device().cloned(), i.up))
            .ok_or(ENODEV)
    })
    .ok_or(ENODEV)??;
    // Tell the driver even if the stack already agrees: interfaces a Linux
    // driver adds after boot are up here but not yet opened there.
    if let Some(d) = dev {
        d.set_up(up)?;
    }
    if cur == up {
        return Ok(());
    }
    with(|net| {
        if let Some(ifc) = net.iface_mut(name)
            && ifc.up != up
        {
            ifc.up = up;
            rtnetlink::link_event(ifc);
        }
    });
    kick();
    Ok(())
}

/// Record that the driver opened or closed an interface (it changed state
/// on its own, or `set_link_up` asked it to).
pub fn set_admin_state(index: u32, up: bool) {
    with(|net| {
        if let Some(ifc) = net.by_index(index)
            && ifc.up != up
        {
            ifc.up = up;
            rtnetlink::link_event(ifc);
        }
    });
    kick();
}

/// Remove the interface with index `index`.
pub fn unregister_index(index: u32) {
    if let Some(name) = with(|net| {
        net.ifaces
            .iter()
            .find(|i| i.index == index)
            .map(|i| i.name.clone())
    })
    .flatten()
    {
        unregister(&name);
    }
}

/// Remove an interface (hot-unplugged NIC).
pub fn unregister(name: &str) {
    with(|net| {
        if let Some(pos) = net.ifaces.iter().position(|i| i.name == name) {
            net.ifaces.remove(pos);
            crate::println!("[net] {}: removed", name);
        }
    });
    EPOCH.fetch_add(1, Ordering::SeqCst);
    SOCK_WQ.wake_all();
}

/// Enable or disable the DHCP client on an interface.
pub fn set_dhcp(name: &str, on: bool) -> KResult<()> {
    with(|net| {
        let ifc = net.iface_mut(name).ok_or(ENODEV)?;
        if ifc.is_loopback() || ifc.is_ip_only() {
            return Err(EINVAL);
        }
        match (on, ifc.dhcp) {
            (true, Some(h)) => ifc.sockets.get_mut::<dhcpv4::Socket>(h).reset(),
            (true, None) => ifc.dhcp = Some(ifc.sockets.add(new_dhcp4())),
            (false, _) => ifc.stop_dhcp(),
        }
        Ok(())
    })
    .ok_or(ENODEV)??;
    kick();
    Ok(())
}

fn netd() {
    loop {
        KICK.store(false, Ordering::SeqCst);
        let (delay, moved) = with(|n| n.poll_all()).unwrap_or((None, false));
        EPOCH.fetch_add(1, Ordering::SeqCst);
        SOCK_WQ.wake_all();
        let mut ms = delay.unwrap_or(100).min(100);
        if ms == 0 && !moved {
            // smoltcp asks to be polled again at once, yet nothing was
            // sent or received: a timer it will never advance (SLAAC keeps
            // its last router-solicitation time once the retries are used
            // up without an answer). Wait for an event instead of spinning.
            ms = 20;
        }
        if ms > 0 {
            NET_WQ.wait_timeout(ms, || KICK.load(Ordering::SeqCst));
        } else {
            // Let woken socket users run (and drain buffers) before the
            // next poll.
            crate::sched::yield_now();
        }
    }
}

/// Create the loopback interface and start the network thread.
pub fn init() {
    rtnetlink::init();
    let mut lo_dev = Loopback::new(Medium::Ip);
    let mut config = Config::new(HardwareAddress::Ip);
    config.random_seed = random_seed();
    let mut iface = Interface::new(
        config,
        &mut lo_dev,
        Instant::from_millis(crate::time::millis() as i64),
    );
    iface.update_ip_addrs(|a| {
        let _ = a.push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8));
        let _ = a.push(IpCidr::new(IpAddress::Ipv6(Ipv6Address::LOCALHOST), 128));
    });
    *NET.lock() = Some(Net {
        // Linux numbering: 0 means "no interface"; loopback is 1.
        ifaces: vec![Iface::new(String::from("lo"), 1, None, Some(lo_dev), iface)],
        next_index: 2,
    });
    register_procfs();
    crate::sched::spawn("netd", netd);
}

// ---------------------------------------------------------------------------
// /proc/net
// ---------------------------------------------------------------------------

fn register_procfs() {
    use crate::vfs::procfs::register;
    register("net/dev", gen_dev);
    register("net/route", gen_route);
    register("net/if_addrs", gen_if_addrs);
    register("net/tcp", || socket::gen_sockets(true));
    register("net/udp", || socket::gen_sockets(false));
    register("net/wireless", gen_wireless);
    register("net/arp", gen_arp);
    register("net/captive_portal", gen_captive_portal);
}

fn gen_dev() -> String {
    let mut s = String::from(
        "Inter-|   Receive                            |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n",
    );
    with(|net| {
        for i in &net.ifaces {
            let st = i.stats;
            let _ = writeln!(
                s,
                "{:>6}: {:>8} {:>7}    0 {:>4}    0     0          0         0 {:>8} {:>7} {:>4}    0    0     0       0          0",
                i.name,
                st.rx_bytes,
                st.rx_packets,
                st.rx_dropped,
                st.tx_bytes,
                st.tx_packets,
                st.tx_errors
            );
        }
    });
    s
}

fn hex_le(a: Ipv4Address) -> String {
    format!("{:08X}", u32::from_le_bytes(a.octets()))
}

fn gen_route() -> String {
    let mut s = String::from(
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n",
    );
    with(|net| {
        for i in &net.ifaces {
            if let Some(gw) = i.gateway {
                let _ = writeln!(
                    s,
                    "{}\t00000000\t{}\t0003\t0\t0\t0\t00000000\t0\t0\t0",
                    i.name,
                    hex_le(gw)
                );
            }
            if let Some(c) = i.ipv4() {
                let net4 = c.network();
                let _ = writeln!(
                    s,
                    "{}\t{}\t00000000\t0001\t0\t0\t0\t{}\t0\t0\t0",
                    i.name,
                    hex_le(net4.address()),
                    hex_le(net4.netmask())
                );
            }
            for (n, via) in &i.routes {
                let _ = writeln!(
                    s,
                    "{}\t{}\t{}\t0003\t0\t0\t0\t{}\t0\t0\t0",
                    i.name,
                    hex_le(n.network().address()),
                    hex_le(*via),
                    hex_le(n.netmask())
                );
            }
        }
    });
    s
}

/// One line per interface: name index flags mtu mac link dhcp driver gw addrs...
fn gen_if_addrs() -> String {
    let mut s = String::new();
    with(|net| {
        for i in &net.ifaces {
            let m = i.mac();
            let addrs: Vec<String> = i.ip_addrs().iter().map(|a| format!("{}", a)).collect();
            let _ = writeln!(
                s,
                "{} {} {} {} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} {} {} {} {} {}",
                i.name,
                i.index,
                if i.up { "up" } else { "down" },
                i.mtu(),
                m[0],
                m[1],
                m[2],
                m[3],
                m[4],
                m[5],
                if i.link_up() { "link" } else { "nolink" },
                if i.dhcp_enabled() { "dhcp" } else { "static" },
                i.dev.as_ref().map_or("loopback", |d| d.driver()),
                i.gateway.map_or(String::from("-"), |g| format!("{}", g)),
                addrs.join(" ")
            );
        }
    });
    s
}

/// `IFACE SOURCE URI` for every interface that learned a captive-portal
/// URI (DHCPv4 option 114, DHCPv6 option 103, RA option 37).
fn gen_captive_portal() -> String {
    let mut s = String::new();
    with(|net| {
        for i in &net.ifaces {
            if let Some((url, src)) = &i.portal {
                let _ = writeln!(s, "{} {} {}", i.name, src, url);
            }
        }
    });
    s
}

fn gen_arp() -> String {
    let mut s = String::from(
        "IP address       HW type     Flags       HW address            Mask     Device\n",
    );
    with(|net| {
        for i in &net.ifaces {
            for (ip, m) in &i.arp {
                let _ = writeln!(
                    s,
                    "{:<16} 0x1         0x2         {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}     *        {}",
                    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
                    m[0],
                    m[1],
                    m[2],
                    m[3],
                    m[4],
                    m[5],
                    i.name
                );
            }
        }
    });
    s
}

fn gen_wireless() -> String {
    let mut s = String::new();
    with(|net| {
        for i in &net.ifaces {
            if let Some(d) = &i.dev
                && d.kind() == IfKind::Wireless
            {
                let _ = writeln!(s, "{}: {}", i.name, d.status());
            }
        }
    });
    s
}

/// Frames queued by a driver's interrupt handler for the network thread.
pub struct RxQueue {
    q: crate::sync::Mutex<VecDeque<Vec<u8>>>,
    pub dropped: AtomicU64,
}

impl Default for RxQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl RxQueue {
    pub const fn new() -> RxQueue {
        RxQueue {
            q: crate::sync::Mutex::new(VecDeque::new()),
            dropped: AtomicU64::new(0),
        }
    }
    pub fn push(&self, frame: Vec<u8>) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut q = self.q.lock();
            if q.len() >= 512 {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            q.push_back(frame);
        });
        kick();
    }
    pub fn pop(&self) -> Option<Vec<u8>> {
        x86_64::instructions::interrupts::without_interrupts(|| self.q.lock().pop_front())
    }
}
