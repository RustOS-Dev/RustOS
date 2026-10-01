//! NETLINK_ROUTE: links, addresses and routes from RustOS's network
//! state, for `ip`, wpa_supplicant/hostapd and libnl. Link changes are
//! announced to RTNLGRP_LINK.

use super::Iface;
use super::netlink::{
    self, NETLINK_ROUTE, NLM_F_ACK, NLM_F_DUMP, NLM_F_MULTI, NlHdr, NlMsg, attrs, done_msg,
    error_msg, messages,
};
use crate::errno::*;
use alloc::vec::Vec;
use smoltcp::wire::{IpCidr, Ipv4Address, Ipv4Cidr};

const RTM_NEWLINK: u16 = 16;
const RTM_DELLINK: u16 = 17;
const RTM_GETLINK: u16 = 18;
const RTM_SETLINK: u16 = 19;
const RTM_NEWADDR: u16 = 20;
const RTM_DELADDR: u16 = 21;
const RTM_GETADDR: u16 = 22;
const RTM_NEWROUTE: u16 = 24;
const RTM_GETROUTE: u16 = 26;

const IFLA_ADDRESS: u16 = 1;
const IFLA_BROADCAST: u16 = 2;
const IFLA_IFNAME: u16 = 3;
const IFLA_MTU: u16 = 4;
const IFLA_TXQLEN: u16 = 13;
const IFLA_OPERSTATE: u16 = 16;
const IFLA_LINKMODE: u16 = 17;
const IFLA_CARRIER: u16 = 33;

const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_LABEL: u16 = 3;
const IFA_BROADCAST: u16 = 4;

const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_TABLE: u16 = 15;

const IFF_UP: u32 = 0x1;
const IFF_BROADCAST: u32 = 0x2;
const IFF_LOOPBACK: u32 = 0x8;
const IFF_RUNNING: u32 = 0x40;
const IFF_MULTICAST: u32 = 0x1000;
const IFF_LOWER_UP: u32 = 0x10000;

const ARPHRD_ETHER: u16 = 1;
const ARPHRD_LOOPBACK: u16 = 772;
const AF_INET: u8 = 2;
const AF_INET6: u8 = 10;
const RTNLGRP_LINK: u32 = 1;
const RTNLGRP_IPV4_IFADDR: u32 = 5;

pub fn init() {
    netlink::register_kernel(NETLINK_ROUTE, Some(input), None);
}

fn flags(ifc: &Iface) -> u32 {
    let mut f = if ifc.is_loopback() {
        IFF_LOOPBACK
    } else {
        IFF_BROADCAST | IFF_MULTICAST
    };
    if ifc.up {
        f |= IFF_UP;
        if ifc.link_up() {
            f |= IFF_RUNNING | IFF_LOWER_UP;
        }
    }
    f
}

fn link_msg(ifc: &Iface, ty: u16, nlflags: u16, seq: u32, pid: u32) -> Vec<u8> {
    let mut m = NlMsg::new(ty, nlflags, seq, pid);
    let hatype = if ifc.is_loopback() {
        ARPHRD_LOOPBACK
    } else {
        ARPHRD_ETHER
    };
    let mut hdr = Vec::with_capacity(16);
    hdr.extend_from_slice(&[0, 0]);
    hdr.extend_from_slice(&hatype.to_ne_bytes());
    hdr.extend_from_slice(&(ifc.index as i32).to_ne_bytes());
    hdr.extend_from_slice(&flags(ifc).to_ne_bytes());
    hdr.extend_from_slice(&u32::MAX.to_ne_bytes());
    m.put(&hdr);
    m.attr_str(IFLA_IFNAME, &ifc.name);
    m.attr(IFLA_ADDRESS, &ifc.mac());
    m.attr(
        IFLA_BROADCAST,
        &if ifc.is_loopback() { [0; 6] } else { [0xff; 6] },
    );
    m.attr_u32(IFLA_MTU, ifc.mtu() as u32);
    m.attr_u32(IFLA_TXQLEN, 1000);
    let running = ifc.up && ifc.link_up();
    // IF_OPER_UP (6) or IF_OPER_DOWN (2).
    m.attr_u8(IFLA_OPERSTATE, if running { 6 } else { 2 });
    m.attr_u8(IFLA_LINKMODE, 0);
    m.attr_u8(IFLA_CARRIER, ifc.link_up() as u8);
    m.finish()
}

fn addr_msgs(ifc: &Iface, nlflags: u16, seq: u32, pid: u32) -> Vec<Vec<u8>> {
    ifc.ip_addrs()
        .into_iter()
        .map(|cidr| {
            let mut m = NlMsg::new(RTM_NEWADDR, nlflags, seq, pid);
            let (family, prefix, bytes): (u8, u8, Vec<u8>) = match cidr {
                IpCidr::Ipv4(c) => (AF_INET, c.prefix_len(), c.address().octets().to_vec()),
                IpCidr::Ipv6(c) => (AF_INET6, c.prefix_len(), c.address().octets().to_vec()),
            };
            // ifaddrmsg: family, prefixlen, flags, scope, index.
            // RT_SCOPE_HOST (254), RT_SCOPE_LINK (253), RT_SCOPE_UNIVERSE (0).
            let scope = match cidr {
                _ if ifc.is_loopback() => 254,
                IpCidr::Ipv6(c) if c.address().is_unicast_link_local() => 253,
                _ => 0,
            };
            let mut hdr = alloc::vec![family, prefix, 0x80 /* IFA_F_PERMANENT */, scope];
            hdr.extend_from_slice(&ifc.index.to_ne_bytes());
            m.put(&hdr);
            m.attr(IFA_ADDRESS, &bytes);
            if let IpCidr::Ipv4(c) = cidr {
                m.attr(IFA_LOCAL, &bytes);
                if let Some(b) = c.broadcast() {
                    m.attr(IFA_BROADCAST, &b.octets());
                }
            }
            m.attr_str(IFA_LABEL, &ifc.name);
            m.finish()
        })
        .collect()
}

fn route_msgs(ifc: &Iface, seq: u32, pid: u32) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut route = |dst: Option<Ipv4Cidr>, gw: Option<Ipv4Address>| {
        let mut m = NlMsg::new(RTM_NEWROUTE, NLM_F_MULTI, seq, pid);
        // rtmsg: family, dst_len, src_len, tos, table (main), protocol
        // (boot), scope (universe/link), type (unicast), flags.
        let scope = if gw.is_some() { 0 } else { 253 };
        let mut hdr = alloc::vec![
            AF_INET,
            dst.map_or(0, |d| d.prefix_len()),
            0,
            0,
            254,
            3,
            scope,
            1
        ];
        hdr.extend_from_slice(&0u32.to_ne_bytes());
        m.put(&hdr);
        m.attr_u32(RTA_TABLE, 254);
        if let Some(d) = dst {
            m.attr(RTA_DST, &d.network().address().octets());
        }
        if let Some(g) = gw {
            m.attr(RTA_GATEWAY, &g.octets());
        }
        m.attr_u32(RTA_OIF, ifc.index);
        out.push(m.finish());
    };
    if let Some(c) = ifc.ipv4() {
        route(Some(c), None);
    }
    for (net, gw) in &ifc.routes {
        route(Some(*net), Some(*gw));
    }
    if let Some(gw) = ifc.gateway {
        route(None, Some(gw));
    }
    out
}

/// Announce `ifc`'s state to RTNLGRP_LINK listeners.
pub fn link_event(ifc: &Iface) {
    if netlink::has_listeners(NETLINK_ROUTE, RTNLGRP_LINK) {
        netlink::multicast(
            NETLINK_ROUTE,
            RTNLGRP_LINK,
            &link_msg(ifc, RTM_NEWLINK, 0, 0, 0),
        );
    }
}

fn addr_event(ifc: &Iface) {
    if netlink::has_listeners(NETLINK_ROUTE, RTNLGRP_IPV4_IFADDR) {
        for m in addr_msgs(ifc, 0, 0, 0) {
            netlink::multicast(NETLINK_ROUTE, RTNLGRP_IPV4_IFADDR, &m);
        }
    }
}

/// The interface a link/address request names (index, or IFLA_IFNAME).
fn target<'a>(net: &'a mut super::Net, index: i32, body: &[u8]) -> Option<&'a mut Iface> {
    if index > 0 {
        return net.by_index(index as u32);
    }
    let name = attrs(body).into_iter().find(|(t, _)| *t == IFLA_IFNAME)?.1;
    let name = core::str::from_utf8(name).ok()?.trim_end_matches('\0');
    net.iface_mut(name)
}

/// Handle one request; returns the reply messages and an error for the ack.
fn handle(h: &NlHdr, body: &[u8], pid: u32) -> (Vec<Vec<u8>>, i32) {
    let dump = h.flags & NLM_F_DUMP == NLM_F_DUMP;
    let mut out = Vec::new();
    // A link to bring up or down, done after the lock is released.
    let mut set_up: Option<(alloc::string::String, bool)> = None;
    let r = super::with(|net| -> KResult<()> {
        match h.ty {
            RTM_GETLINK if dump => {
                for ifc in &net.ifaces {
                    out.push(link_msg(ifc, RTM_NEWLINK, NLM_F_MULTI, h.seq, pid));
                }
                out.push(done_msg(h, pid));
            }
            RTM_GETLINK => {
                let index = body
                    .get(4..8)
                    .map_or(0, |b| i32::from_ne_bytes(b.try_into().unwrap()));
                let ifc = target(net, index, body.get(16..).unwrap_or(&[])).ok_or(ENODEV)?;
                out.push(link_msg(ifc, RTM_NEWLINK, 0, h.seq, pid));
            }
            RTM_SETLINK | RTM_NEWLINK => {
                if body.len() < 16 {
                    return Err(EINVAL);
                }
                let index = i32::from_ne_bytes(body[4..8].try_into().unwrap());
                let fl = u32::from_ne_bytes(body[8..12].try_into().unwrap());
                let change = u32::from_ne_bytes(body[12..16].try_into().unwrap());
                let ifc = target(net, index, &body[16..]).ok_or(ENODEV)?;
                // As Linux: flags = change ? (old & ~change) | (new & change) : new;
                // nothing changes when both are 0.
                if fl != 0 || change != 0 {
                    let up = if change != 0 {
                        if change & IFF_UP != 0 {
                            fl & IFF_UP != 0
                        } else {
                            ifc.up
                        }
                    } else {
                        fl & IFF_UP != 0
                    };
                    if up != ifc.up {
                        set_up = Some((ifc.name.clone(), up));
                    }
                }
            }
            RTM_DELLINK => return Err(EOPNOTSUPP),
            RTM_GETADDR => {
                for ifc in &net.ifaces {
                    out.extend(addr_msgs(ifc, NLM_F_MULTI, h.seq, pid));
                }
                out.push(done_msg(h, pid));
            }
            RTM_NEWADDR | RTM_DELADDR => {
                if body.len() < 8 {
                    return Err(EINVAL);
                }
                let (family, prefix) = (body[0], body[1]);
                let index = u32::from_ne_bytes(body[4..8].try_into().unwrap());
                if family != AF_INET {
                    return Err(EOPNOTSUPP);
                }
                let addr = attrs(&body[8..])
                    .into_iter()
                    .find(|(t, _)| *t == IFA_LOCAL || *t == IFA_ADDRESS)
                    .and_then(|(_, d)| <[u8; 4]>::try_from(d).ok())
                    .ok_or(EINVAL)?;
                let ifc = net.by_index(index).ok_or(ENODEV)?;
                if h.ty == RTM_NEWADDR {
                    ifc.set_ipv4(Some(Ipv4Cidr::new(Ipv4Address::from_octets(addr), prefix)));
                } else if ifc.ipv4().is_some_and(|c| c.address().octets() == addr) {
                    ifc.set_ipv4(None);
                }
                addr_event(ifc);
            }
            RTM_GETROUTE => {
                for ifc in &net.ifaces {
                    out.extend(route_msgs(ifc, h.seq, pid));
                }
                out.push(done_msg(h, pid));
            }
            _ => return Err(EOPNOTSUPP),
        }
        Ok(())
    });
    let r = match (r, set_up) {
        (Some(Ok(())), Some((name, up))) => Some(super::set_link_up(&name, up)),
        (r, _) => r,
    };
    let err = match r {
        Some(Ok(())) => 0,
        Some(Err(e)) => -e.0,
        None => -ENODEV.0,
    };
    (out, err)
}

fn input(_proto: u32, portid: u32, data: &[u8]) {
    for (h, body) in messages(data) {
        let (replies, err) = handle(&h, body, portid);
        for r in replies {
            let _ = netlink::unicast(NETLINK_ROUTE, portid, r);
        }
        if err != 0 || h.flags & NLM_F_ACK != 0 {
            let _ = netlink::unicast(NETLINK_ROUTE, portid, error_msg(&h, err, portid));
        }
    }
    super::kick();
}
