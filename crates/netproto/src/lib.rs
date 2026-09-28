//! Network-configuration protocol codecs used by the kernel's network
//! core, kept pure so they are tested on the host:
//!
//! * [`ra`]: IPv6 router advertisement parsing (flags, RDNSS, DNSSL,
//!   captive-portal option) from raw Ethernet frames;
//! * [`dhcpv6`]: DHCPv6 client messages (stateless Information-Request
//!   and stateful Solicit/Request/Renew with IA_NA);
//! * [`capport`]: captive-portal URI handling (RFC 8910) shared by DHCPv4
//!   option 114, DHCPv6 option 103 and the RA option.

#![no_std]

extern crate alloc;

pub mod capport {
    use alloc::string::String;

    /// Value meaning "no portal" (RFC 8910 section 2).
    pub const UNRESTRICTED: &str = "urn:ietf:params:capport:unrestricted";

    /// Interpret a captive-portal option payload; `None` for the
    /// "unrestricted" URN, empty or non-HTTPS/HTTP values.
    pub fn parse_uri(data: &[u8]) -> Option<String> {
        let s = core::str::from_utf8(data)
            .ok()?
            .trim_matches(|c: char| c == '\0' || c.is_whitespace());
        if s.is_empty() || s.eq_ignore_ascii_case(UNRESTRICTED) {
            return None;
        }
        let lower = s.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) {
            return None;
        }
        Some(String::from(s))
    }

    /// Extract option 114 from DHCPv4 options (after the magic cookie):
    /// `code len data...` records, 0 = pad, 255 = end.
    pub fn from_dhcpv4_options(opts: &[u8]) -> Option<String> {
        let mut i = 0;
        while i < opts.len() {
            let code = opts[i];
            if code == 0 {
                i += 1;
                continue;
            }
            if code == 255 {
                break;
            }
            let len = *opts.get(i + 1)? as usize;
            let data = opts.get(i + 2..i + 2 + len)?;
            if code == 114 {
                return parse_uri(data);
            }
            i += 2 + len;
        }
        None
    }
}

pub mod ra {
    use alloc::string::String;
    use alloc::vec::Vec;

    /// Information from one router advertisement.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct RouterAdvert {
        /// Router's link-local address (IPv6 source).
        pub router: [u8; 16],
        /// "Managed address configuration": use stateful DHCPv6.
        pub managed: bool,
        /// "Other configuration": use stateless DHCPv6 for DNS etc.
        pub other: bool,
        pub router_lifetime: u16,
        /// RDNSS servers with their lifetime (seconds).
        pub dns: Vec<([u8; 16], u32)>,
        pub search: Vec<String>,
        pub captive_portal: Option<String>,
    }

    /// Parse an Ethernet frame; returns the RA if it is one (IPv6, next
    /// header ICMPv6, type 134, hop limit 255).
    pub fn parse_frame(frame: &[u8]) -> Option<RouterAdvert> {
        if frame.len() < 14 + 40 + 16 || frame[12..14] != [0x86, 0xDD] {
            return None;
        }
        let ip = &frame[14..];
        if ip[0] >> 4 != 6 || ip[6] != 58 || ip[7] != 255 {
            return None;
        }
        let plen = u16::from_be_bytes([ip[4], ip[5]]) as usize;
        let icmp = ip.get(40..40 + plen)?;
        if icmp.len() < 16 || icmp[0] != 134 || icmp[1] != 0 {
            return None;
        }
        let mut ra = RouterAdvert::default();
        ra.router.copy_from_slice(&ip[8..24]);
        ra.managed = icmp[5] & 0x80 != 0;
        ra.other = icmp[5] & 0x40 != 0;
        ra.router_lifetime = u16::from_be_bytes([icmp[6], icmp[7]]);
        let mut opts = &icmp[16..];
        while opts.len() >= 8 {
            let ty = opts[0];
            let len = opts[1] as usize * 8;
            if len == 0 || len > opts.len() {
                break;
            }
            let o = &opts[..len];
            match ty {
                // RDNSS (RFC 8106): reserved(2) lifetime(4) addresses.
                25 if len >= 24 => {
                    let life = u32::from_be_bytes([o[4], o[5], o[6], o[7]]);
                    for a in o[8..].chunks_exact(16) {
                        let mut x = [0u8; 16];
                        x.copy_from_slice(a);
                        ra.dns.push((x, life));
                    }
                }
                // DNSSL: lifetime, then DNS-encoded names.
                31 if len >= 16 => {
                    let mut p = &o[8..];
                    while !p.is_empty() && p[0] != 0 {
                        let mut name = String::new();
                        while let Some(&l) = p.first() {
                            p = &p[1..];
                            if l == 0 || l as usize > p.len() {
                                break;
                            }
                            if !name.is_empty() {
                                name.push('.');
                            }
                            name.push_str(core::str::from_utf8(&p[..l as usize]).unwrap_or(""));
                            p = &p[l as usize..];
                        }
                        if !name.is_empty() {
                            ra.search.push(name);
                        }
                    }
                }
                // Captive portal (RFC 8910): the URI, NUL padded.
                37 => ra.captive_portal = super::capport::parse_uri(&o[2..]),
                _ => {}
            }
            opts = &opts[len..];
        }
        Some(ra)
    }
}

pub mod dhcpv6 {
    use alloc::string::String;
    use alloc::vec::Vec;

    pub const CLIENT_PORT: u16 = 546;
    pub const SERVER_PORT: u16 = 547;
    /// All_DHCP_Relay_Agents_and_Servers (ff02::1:2).
    pub const ALL_SERVERS: [u8; 16] = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 2];

    pub const SOLICIT: u8 = 1;
    pub const ADVERTISE: u8 = 2;
    pub const REQUEST: u8 = 3;
    pub const RENEW: u8 = 5;
    pub const REBIND: u8 = 6;
    pub const REPLY: u8 = 7;
    pub const INFORMATION_REQUEST: u8 = 11;

    const OPT_CLIENTID: u16 = 1;
    const OPT_SERVERID: u16 = 2;
    const OPT_IA_NA: u16 = 3;
    const OPT_IAADDR: u16 = 5;
    const OPT_ORO: u16 = 6;
    const OPT_ELAPSED_TIME: u16 = 8;
    const OPT_STATUS_CODE: u16 = 13;
    const OPT_DNS_SERVERS: u16 = 23;
    const OPT_DOMAIN_LIST: u16 = 24;
    const OPT_CAPTIVE_PORTAL: u16 = 103;

    /// DUID-LL (type 3, hardware type 1 = Ethernet) from a MAC address.
    pub fn duid_ll(mac: [u8; 6]) -> Vec<u8> {
        let mut d = alloc::vec![0, 3, 0, 1];
        d.extend_from_slice(&mac);
        d
    }

    fn opt(out: &mut Vec<u8>, code: u16, data: &[u8]) {
        out.extend_from_slice(&code.to_be_bytes());
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
    }

    /// An address leased through IA_NA.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Lease {
        pub addr: [u8; 16],
        pub preferred: u32,
        pub valid: u32,
        pub t1: u32,
        pub t2: u32,
    }

    /// Build a client message.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        msg_type: u8,
        xid: [u8; 3],
        client_id: &[u8],
        server_id: Option<&[u8]>,
        iaid: Option<u32>,
        lease: Option<&Lease>,
        elapsed_cs: u16,
    ) -> Vec<u8> {
        let mut m = alloc::vec![msg_type, xid[0], xid[1], xid[2]];
        opt(&mut m, OPT_CLIENTID, client_id);
        if let Some(s) = server_id {
            opt(&mut m, OPT_SERVERID, s);
        }
        if let Some(id) = iaid {
            let mut ia = Vec::new();
            ia.extend_from_slice(&id.to_be_bytes());
            ia.extend_from_slice(&0u32.to_be_bytes()); // T1: server decides
            ia.extend_from_slice(&0u32.to_be_bytes()); // T2
            if let Some(l) = lease {
                let mut a = Vec::new();
                a.extend_from_slice(&l.addr);
                a.extend_from_slice(&l.preferred.to_be_bytes());
                a.extend_from_slice(&l.valid.to_be_bytes());
                opt(&mut ia, OPT_IAADDR, &a);
            }
            opt(&mut m, OPT_IA_NA, &ia);
        }
        let mut oro = Vec::new();
        for c in [OPT_DNS_SERVERS, OPT_DOMAIN_LIST, OPT_CAPTIVE_PORTAL] {
            oro.extend_from_slice(&c.to_be_bytes());
        }
        opt(&mut m, OPT_ORO, &oro);
        opt(&mut m, OPT_ELAPSED_TIME, &elapsed_cs.to_be_bytes());
        m
    }

    /// A parsed server message.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Message {
        pub msg_type: u8,
        pub xid: [u8; 3],
        pub server_id: Option<Vec<u8>>,
        pub client_id: Option<Vec<u8>>,
        /// Top-level status code (0 = success when absent).
        pub status: u16,
        pub lease: Option<Lease>,
        /// IA_NA present but carrying an error status (e.g. NoAddrsAvail).
        pub ia_status: u16,
        pub dns: Vec<[u8; 16]>,
        pub domains: Vec<String>,
        pub captive_portal: Option<String>,
    }

    fn options(mut p: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
        core::iter::from_fn(move || {
            if p.len() < 4 {
                return None;
            }
            let code = u16::from_be_bytes([p[0], p[1]]);
            let len = u16::from_be_bytes([p[2], p[3]]) as usize;
            let data = p.get(4..4 + len)?;
            p = &p[4 + len..];
            Some((code, data))
        })
    }

    fn status_of(d: &[u8]) -> u16 {
        if d.len() >= 2 {
            u16::from_be_bytes([d[0], d[1]])
        } else {
            0
        }
    }

    fn names(mut p: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        while let Some(&l) = p.first() {
            p = &p[1..];
            if l == 0 {
                if !cur.is_empty() {
                    out.push(core::mem::take(&mut cur));
                }
                continue;
            }
            let l = l as usize;
            if l > p.len() {
                break;
            }
            if !cur.is_empty() {
                cur.push('.');
            }
            cur.push_str(core::str::from_utf8(&p[..l]).unwrap_or(""));
            p = &p[l..];
        }
        out
    }

    pub fn parse(p: &[u8]) -> Option<Message> {
        if p.len() < 4 {
            return None;
        }
        let mut m = Message {
            msg_type: p[0],
            xid: [p[1], p[2], p[3]],
            ..Message::default()
        };
        for (code, d) in options(&p[4..]) {
            match code {
                OPT_SERVERID => m.server_id = Some(d.to_vec()),
                OPT_CLIENTID => m.client_id = Some(d.to_vec()),
                OPT_STATUS_CODE => m.status = status_of(d),
                OPT_DNS_SERVERS => {
                    for a in d.chunks_exact(16) {
                        let mut x = [0u8; 16];
                        x.copy_from_slice(a);
                        m.dns.push(x);
                    }
                }
                OPT_DOMAIN_LIST => m.domains = names(d),
                OPT_CAPTIVE_PORTAL => m.captive_portal = super::capport::parse_uri(d),
                OPT_IA_NA if d.len() >= 12 => {
                    let t1 = u32::from_be_bytes([d[4], d[5], d[6], d[7]]);
                    let t2 = u32::from_be_bytes([d[8], d[9], d[10], d[11]]);
                    for (c2, d2) in options(&d[12..]) {
                        match c2 {
                            OPT_IAADDR if d2.len() >= 24 => {
                                // An IAADDR may carry its own status.
                                let st = options(&d2[24..])
                                    .find(|(c, _)| *c == OPT_STATUS_CODE)
                                    .map_or(0, |(_, s)| status_of(s));
                                let valid = u32::from_be_bytes([d2[20], d2[21], d2[22], d2[23]]);
                                if st == 0 && valid > 0 {
                                    let mut addr = [0u8; 16];
                                    addr.copy_from_slice(&d2[..16]);
                                    m.lease = Some(Lease {
                                        addr,
                                        preferred: u32::from_be_bytes([
                                            d2[16], d2[17], d2[18], d2[19],
                                        ]),
                                        valid,
                                        t1,
                                        t2,
                                    });
                                }
                            }
                            OPT_STATUS_CODE => m.ia_status = status_of(d2),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Some(m)
    }
}

#[cfg(test)]
mod tests;
