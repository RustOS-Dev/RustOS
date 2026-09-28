//! DHCPv6 client (RFC 8415), started by router advertisements.
//!
//! * "Other configuration" (O flag): stateless Information-Request for DNS
//!   servers, search domains and the captive-portal URI.
//! * "Managed" (M flag): Solicit/Advertise/Request/Reply for an IA_NA
//!   address, renewed at T1 and dropped when it expires.
//!
//! Message encoding and parsing live in `netproto::dhcpv6` (host-tested);
//! this module owns the UDP socket, timers and retransmission.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use netproto::dhcpv6 as proto;
use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::udp;
use smoltcp::wire::{IpEndpoint, Ipv6Address};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Info,
    Solicit,
    Request,
    Bound,
    Renew,
    /// Stateless reply received; refresh later.
    Done,
}

/// Configuration learned from a server.
#[derive(Debug, Default)]
pub struct Update {
    pub dns: Vec<Ipv6Address>,
    pub domains: Vec<String>,
    pub portal: Option<String>,
    /// Newly bound address and its valid lifetime (seconds).
    pub addr: Option<(Ipv6Address, u32)>,
    /// The previously bound address expired.
    pub lost: Option<Ipv6Address>,
}

pub struct Client {
    h: SocketHandle,
    pub stateful: bool,
    state: State,
    xid: [u8; 3],
    duid: Vec<u8>,
    iaid: u32,
    server_id: Option<Vec<u8>>,
    lease: Option<proto::Lease>,
    lease_at: u64,
    started: u64,
    next_tx: u64,
    rto: u64,
    tries: u32,
}

/// Stateless information is refreshed after this long (RFC 8415 default).
const INFO_REFRESH_MS: u64 = 86_400_000;

fn random_xid() -> [u8; 3] {
    let mut b = [0u8; 3];
    crate::drivers::random::fill(&mut b);
    b
}

impl Client {
    pub fn new(sockets: &mut SocketSet<'static>, mac: [u8; 6], stateful: bool, now: u64) -> Client {
        let rx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0u8; 4096]);
        let tx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0u8; 2048]);
        let mut s = udp::Socket::new(rx, tx);
        let _ = s.bind(proto::CLIENT_PORT);
        let h = sockets.add(s);
        Client {
            h,
            stateful,
            state: if stateful {
                State::Solicit
            } else {
                State::Info
            },
            xid: random_xid(),
            duid: proto::duid_ll(mac),
            iaid: u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]),
            server_id: None,
            lease: None,
            lease_at: 0,
            started: now,
            // RFC 8415: random initial delay; keep it short.
            next_tx: now + 100,
            rto: 1000,
            tries: 0,
        }
    }

    pub fn remove(self, sockets: &mut SocketSet<'static>) -> Option<Ipv6Address> {
        sockets.remove(self.h);
        self.lease
            .filter(|_| matches!(self.state, State::Bound | State::Renew))
            .map(|l| Ipv6Address::from(l.addr))
    }

    fn restart(&mut self, state: State, now: u64) {
        self.state = state;
        self.xid = random_xid();
        self.started = now;
        self.next_tx = now;
        self.rto = 1000;
        self.tries = 0;
    }

    fn send(&mut self, sockets: &mut SocketSet<'static>, now: u64) {
        let (ty, server, ia, lease) = match self.state {
            State::Info => (proto::INFORMATION_REQUEST, None, None, None),
            State::Solicit => (proto::SOLICIT, None, Some(self.iaid), None),
            State::Request => (
                proto::REQUEST,
                self.server_id.as_deref(),
                Some(self.iaid),
                self.lease.as_ref(),
            ),
            State::Renew => (
                proto::RENEW,
                self.server_id.as_deref(),
                Some(self.iaid),
                self.lease.as_ref(),
            ),
            State::Bound | State::Done => return,
        };
        let elapsed = ((now - self.started) / 10).min(0xFFFF) as u16;
        let msg = proto::build(ty, self.xid, &self.duid, server, ia, lease, elapsed);
        let s = sockets.get_mut::<udp::Socket>(self.h);
        let dst = IpEndpoint::new(
            Ipv6Address::from(proto::ALL_SERVERS).into(),
            proto::SERVER_PORT,
        );
        let _ = s.send_slice(&msg, dst);
        self.tries += 1;
        self.next_tx = now + self.rto;
        self.rto = (self.rto * 2).min(120_000);
        // Give up on a server that stops answering and start over.
        if self.state == State::Request && self.tries > 10 {
            self.restart(State::Solicit, now);
        }
    }

    /// Process replies and timers. Returns new configuration, if any.
    pub fn poll(&mut self, sockets: &mut SocketSet<'static>, now: u64) -> Option<Update> {
        let mut update = None;
        loop {
            let s = sockets.get_mut::<udp::Socket>(self.h);
            let Ok((data, _)) = s.recv() else { break };
            let Some(m) = proto::parse(data) else {
                continue;
            };
            if m.xid != self.xid || m.client_id.as_deref().is_some_and(|c| c != self.duid) {
                continue;
            }
            let info = |m: &proto::Message| Update {
                dns: m.dns.iter().map(|a| Ipv6Address::from(*a)).collect(),
                domains: m.domains.clone(),
                portal: m.captive_portal.clone(),
                ..Update::default()
            };
            match (self.state, m.msg_type) {
                (State::Info, proto::REPLY) => {
                    update = Some(info(&m));
                    self.state = State::Done;
                    self.next_tx = now + INFO_REFRESH_MS;
                }
                (State::Solicit, proto::ADVERTISE) if m.lease.is_some() && m.status == 0 => {
                    self.server_id = m.server_id.clone();
                    self.lease = m.lease;
                    self.restart(State::Request, now);
                }
                (State::Request | State::Renew, proto::REPLY) => match m.lease {
                    Some(l) if m.status == 0 => {
                        let mut u = info(&m);
                        u.addr = Some((Ipv6Address::from(l.addr), l.valid));
                        self.lease = Some(l);
                        self.lease_at = now;
                        self.state = State::Bound;
                        update = Some(u);
                    }
                    _ => self.restart(State::Solicit, now + 5000),
                },
                _ => {}
            }
        }
        match self.state {
            State::Done if now >= self.next_tx => self.restart(State::Info, now),
            State::Bound => {
                let l = self.lease.unwrap();
                let t1 = if l.t1 > 0 { l.t1 } else { l.preferred / 2 };
                if now >= self.lease_at + l.valid as u64 * 1000 {
                    let u = update.get_or_insert_with(Update::default);
                    u.lost = Some(Ipv6Address::from(l.addr));
                    self.lease = None;
                    self.restart(State::Solicit, now);
                } else if now >= self.lease_at + t1 as u64 * 1000 {
                    self.restart(State::Renew, now);
                }
            }
            _ => {}
        }
        if now >= self.next_tx {
            self.send(sockets, now);
        }
        update
    }

    /// Milliseconds until the next scheduled transmission.
    pub fn delay(&self, now: u64) -> u64 {
        match self.state {
            State::Bound => 1000,
            _ => self.next_tx.saturating_sub(now),
        }
    }
}
