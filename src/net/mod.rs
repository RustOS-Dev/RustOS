//! Networking: device registry, interfaces and the protocol stack.
//!
//! Drivers implement [`NetDevice`] and call [`register`]; each device gets
//! an interface (`eth0`, `wlan0`, ...) with its own smoltcp `Interface` and
//! socket set, and by default a DHCP client. Connected sockets live in the
//! set of the interface their traffic is routed through; wildcard listeners
//! get one smoltcp socket per interface. A single kernel thread ("netd")
//! polls every interface; socket syscalls work under the same lock and
//! kick the thread when they queue data.

pub mod socket;
pub mod syscalls;

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
        }
    }

    pub fn is_loopback(&self) -> bool {
        self.lo.is_some()
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
                self.set_ipv4(Some(addr));
                self.set_gateway(router);
                self.dns = dns;
                write_resolv_conf(&self.dns);
            }
            Some(dhcpv4::Event::Deconfigured) => {
                crate::println!("[net] {}: DHCP lease lost", self.name);
                self.set_ipv4(None);
                self.set_gateway(None);
            }
            None => {}
        }
    }

    /// Poll this interface. Returns the delay until it wants polling again.
    fn poll(&mut self, now: Instant) -> Option<u64> {
        if !self.up {
            return None;
        }
        let link = self.link_up();
        if link != self.link {
            self.link = link;
            crate::println!(
                "[net] {}: link {}",
                self.name,
                if link { "up" } else { "down" }
            );
            if let Some(h) = self.dhcp {
                self.sockets.get_mut::<dhcpv4::Socket>(h).reset();
            }
        }
        if let Some(lo) = self.lo.as_mut() {
            self.iface.poll(now, lo, &mut self.sockets);
        } else if let Some(dev) = self.dev.clone() {
            if !link {
                // Drop frames while the link is down.
                while dev.receive().is_some() {}
                return None;
            }
            let mut p = Phy {
                dev: &dev,
                stats: &mut self.stats,
                arp: Some(&mut self.arp),
            };
            self.iface.poll(now, &mut p, &mut self.sockets);
        }
        self.dhcp_poll();
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
        self.iface
            .poll_delay(now, &self.sockets)
            .map(|d| d.total_millis())
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
    dev: &'a Arc<dyn NetDevice>,
    stats: &'a mut Stats,
    arp: Option<&'a mut BTreeMap<[u8; 4], [u8; 6]>>,
}

struct RxTok(Vec<u8>);
struct TxTok<'a> {
    dev: &'a Arc<dyn NetDevice>,
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
        match self.dev.transmit(&buf) {
            Ok(()) => {
                stats.tx_packets += 1;
                stats.tx_bytes += len as u64;
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
        let frame = self.dev.receive()?;
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
        self.stats.rx_packets += 1;
        self.stats.rx_bytes += frame.len() as u64;
        Some((
            RxTok(frame),
            TxTok {
                dev: self.dev,
                stats: self.stats as *mut Stats,
            },
        ))
    }

    fn transmit(&mut self, _t: Instant) -> Option<TxTok<'_>> {
        Some(TxTok {
            dev: self.dev,
            stats: self.stats as *mut Stats,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
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

    fn poll_all(&mut self) -> Option<u64> {
        let now = Instant::from_millis(crate::time::millis() as i64);
        let mut next: Option<u64> = None;
        for ifc in self.ifaces.iter_mut() {
            if let Some(d) = ifc.poll(now) {
                next = Some(next.map_or(d, |n| n.min(d)));
            }
        }
        next
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

fn write_resolv_conf(dns: &[Ipv4Address]) {
    if dns.is_empty() {
        return;
    }
    let mut s = String::from("# generated by the kernel DHCP client\n");
    for d in dns {
        let _ = writeln!(s, "nameserver {}", d);
    }
    let _ = crate::vfs::write_all("/etc/resolv.conf", s.as_bytes());
}

/// Register a NIC. Returns the interface name. DHCP starts automatically.
pub fn register(dev: Arc<dyn NetDevice>) -> String {
    let mut g = NET.lock();
    let net = g.as_mut().expect("net::init not called");
    let prefix = match dev.kind() {
        IfKind::Ethernet => "eth",
        IfKind::Wireless => "wlan",
    };
    let n = (0..)
        .find(|n| net.iface(&format!("{}{}", prefix, n)).is_none())
        .unwrap();
    let name = format!("{}{}", prefix, n);
    let mac = dev.mac();
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    config.random_seed = random_seed();
    // IPv6 stateless autoconfiguration from router advertisements.
    config.slaac = true;
    let now = Instant::from_millis(crate::time::millis() as i64);
    let mut stats = Stats::default();
    let mut p = Phy {
        dev: &dev,
        stats: &mut stats,
        arp: None,
    };
    let mut iface = Interface::new(config, &mut p, now);
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
    ifc.dhcp = Some(ifc.sockets.add(dhcpv4::Socket::new()));
    net.ifaces.push(ifc);
    drop(g);
    kick();
    name
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
        if ifc.is_loopback() {
            return Err(EINVAL);
        }
        match (on, ifc.dhcp) {
            (true, Some(h)) => ifc.sockets.get_mut::<dhcpv4::Socket>(h).reset(),
            (true, None) => ifc.dhcp = Some(ifc.sockets.add(dhcpv4::Socket::new())),
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
        let delay = with(|n| n.poll_all()).flatten();
        EPOCH.fetch_add(1, Ordering::SeqCst);
        SOCK_WQ.wake_all();
        crate::vfs::notify_poll();
        let ms = delay.unwrap_or(100).min(100);
        if ms > 0 {
            NET_WQ.wait_timeout(ms, || KICK.load(Ordering::SeqCst));
        }
    }
}

/// Create the loopback interface and start the network thread.
pub fn init() {
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
        ifaces: vec![Iface::new(String::from("lo"), 0, None, Some(lo_dev), iface)],
        next_index: 1,
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
    q: spin::Mutex<VecDeque<Vec<u8>>>,
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
            q: spin::Mutex::new(VecDeque::new()),
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
