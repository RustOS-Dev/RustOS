extern crate std;
use super::*;
use alloc::vec;
use alloc::vec::Vec;

fn v6(s: &[u16; 8]) -> [u8; 16] {
    let mut a = [0u8; 16];
    for (i, g) in s.iter().enumerate() {
        a[2 * i..2 * i + 2].copy_from_slice(&g.to_be_bytes());
    }
    a
}

fn ra_frame(flags: u8, opts: &[u8], hop: u8) -> Vec<u8> {
    let mut icmp = vec![134, 0, 0, 0, 64, flags, 0x07, 0x08, 0, 0, 0, 0, 0, 0, 0, 0];
    icmp.extend_from_slice(opts);
    let mut f = vec![
        0x33, 0x33, 0, 0, 0, 1, 0x52, 0x54, 0, 0x12, 0x34, 0x02, 0x86, 0xDD,
    ];
    f.extend_from_slice(&[0x60, 0, 0, 0]);
    f.extend_from_slice(&(icmp.len() as u16).to_be_bytes());
    f.push(58);
    f.push(hop);
    f.extend_from_slice(&v6(&[0xfe80, 0, 0, 0, 0, 0, 0, 1]));
    f.extend_from_slice(&v6(&[0xff02, 0, 0, 0, 0, 0, 0, 1]));
    f.extend_from_slice(&icmp);
    f
}

#[test]
fn router_advertisement_options() {
    let dns1 = v6(&[0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x53]);
    let dns2 = v6(&[0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x54]);
    let mut opts = vec![25, 5, 0, 0, 0, 0, 0x0e, 0x10];
    opts.extend_from_slice(&dns1);
    opts.extend_from_slice(&dns2);
    // DNSSL "example.org": lifetime + name + padding to 8.
    let mut dnssl = vec![31, 0, 0, 0, 0, 0, 0x0e, 0x10, 7];
    dnssl.extend_from_slice(b"example");
    dnssl.push(3);
    dnssl.extend_from_slice(b"org");
    dnssl.push(0);
    while dnssl.len() % 8 != 0 {
        dnssl.push(0);
    }
    dnssl[1] = (dnssl.len() / 8) as u8;
    opts.extend_from_slice(&dnssl);
    let uri = b"https://portal.example/login";
    let mut cp = vec![37, 0];
    cp.extend_from_slice(uri);
    while cp.len() % 8 != 0 {
        cp.push(0);
    }
    cp[1] = (cp.len() / 8) as u8;
    opts.extend_from_slice(&cp);

    let ra = ra::parse_frame(&ra_frame(0xC0, &opts, 255)).unwrap();
    assert!(ra.managed && ra.other);
    assert_eq!(ra.router_lifetime, 0x0708);
    assert_eq!(ra.router, v6(&[0xfe80, 0, 0, 0, 0, 0, 0, 1]));
    assert_eq!(ra.dns, vec![(dns1, 3600), (dns2, 3600)]);
    assert_eq!(ra.search, vec![alloc::string::String::from("example.org")]);
    assert_eq!(
        ra.captive_portal.as_deref(),
        Some("https://portal.example/login")
    );

    let plain = ra::parse_frame(&ra_frame(0x40, &[], 255)).unwrap();
    assert!(!plain.managed && plain.other && plain.dns.is_empty());
    // Hop limit must be 255 (not forwarded) and malformed options stop parsing.
    assert!(ra::parse_frame(&ra_frame(0, &[], 64)).is_none());
    assert!(ra::parse_frame(&ra_frame(0, &[25, 0, 1, 2, 3, 4, 5, 6], 255)).is_some());
    assert!(ra::parse_frame(&[0u8; 20]).is_none());
}

#[test]
fn captive_portal_uris() {
    assert_eq!(
        capport::parse_uri(b"https://a/b\0\0").as_deref(),
        Some("https://a/b")
    );
    assert_eq!(
        capport::parse_uri(b"urn:ietf:params:capport:unrestricted"),
        None
    );
    assert_eq!(capport::parse_uri(b"ftp://x"), None);
    assert_eq!(capport::parse_uri(b""), None);
    let mut opts = vec![0, 1, 4, 255, 255, 255, 0, 114, 16];
    opts.extend_from_slice(b"http://10.0.0.1/");
    opts.push(255);
    assert_eq!(
        capport::from_dhcpv4_options(&opts).as_deref(),
        Some("http://10.0.0.1/")
    );
    assert_eq!(capport::from_dhcpv4_options(&[3, 4, 1, 2, 3, 4, 255]), None);
    assert_eq!(capport::from_dhcpv4_options(&[114, 200, 1]), None);
}

#[test]
fn dhcpv6_client_messages() {
    let duid = dhcpv6::duid_ll([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
    assert_eq!(duid, vec![0, 3, 0, 1, 0x52, 0x54, 0, 0x12, 0x34, 0x56]);
    let info = dhcpv6::build(
        dhcpv6::INFORMATION_REQUEST,
        [1, 2, 3],
        &duid,
        None,
        None,
        None,
        0,
    );
    assert_eq!(&info[..4], &[11, 1, 2, 3]);
    // ClientID, ORO (23, 24, 103), elapsed time.
    assert_eq!(&info[4..8], &[0, 1, 0, 10]);
    assert_eq!(&info[18..30], &[0, 6, 0, 6, 0, 23, 0, 24, 0, 103, 0, 8]);
    let lease = dhcpv6::Lease {
        addr: v6(&[0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x100]),
        preferred: 100,
        valid: 200,
        t1: 50,
        t2: 80,
    };
    let req = dhcpv6::build(
        dhcpv6::REQUEST,
        [9, 9, 9],
        &duid,
        Some(&[0, 1, 2]),
        Some(7),
        Some(&lease),
        150,
    );
    let parsed = dhcpv6::parse(&req).unwrap();
    assert_eq!(parsed.msg_type, dhcpv6::REQUEST);
    assert_eq!(parsed.server_id.as_deref(), Some(&[0u8, 1, 2][..]));
    assert_eq!(parsed.client_id.as_deref(), Some(&duid[..]));
    assert_eq!(parsed.lease.unwrap().addr, lease.addr);
    assert_eq!(parsed.lease.unwrap().t1, 0);
}

fn server_reply(ty: u8, ia: Option<(&[u8; 16], u32, u16)>) -> Vec<u8> {
    let mut m = vec![ty, 0xaa, 0xbb, 0xcc];
    let mut o = |code: u16, d: &[u8]| {
        m.extend_from_slice(&code.to_be_bytes());
        m.extend_from_slice(&(d.len() as u16).to_be_bytes());
        m.extend_from_slice(d);
    };
    o(2, &[0, 3, 0, 1, 2, 2, 2, 2, 2, 2]);
    let mut dns = Vec::new();
    dns.extend_from_slice(&v6(&[0xfd00, 0, 0, 0, 0, 0, 0, 0x53]));
    o(23, &dns);
    o(24, b"\x04corp\x07example\x00");
    o(103, b"https://login.corp.example/");
    if let Some((addr, valid, status)) = ia {
        let mut ia = vec![0, 0, 0, 7, 0, 0, 0x0e, 0x10, 0, 0, 0x15, 0x18];
        let mut a = addr.to_vec();
        a.extend_from_slice(&3000u32.to_be_bytes());
        a.extend_from_slice(&valid.to_be_bytes());
        ia.extend_from_slice(&5u16.to_be_bytes());
        ia.extend_from_slice(&(a.len() as u16).to_be_bytes());
        ia.extend_from_slice(&a);
        if status != 0 {
            ia.extend_from_slice(&[0, 13, 0, 2]);
            ia.extend_from_slice(&status.to_be_bytes());
        }
        o(3, &ia);
    }
    m
}

#[test]
fn dhcpv6_server_replies() {
    let addr = v6(&[0xfd00, 0, 0, 0, 0, 0, 0, 0x1234]);
    let m = dhcpv6::parse(&server_reply(dhcpv6::ADVERTISE, Some((&addr, 7200, 0)))).unwrap();
    assert_eq!(m.msg_type, dhcpv6::ADVERTISE);
    assert_eq!(m.xid, [0xaa, 0xbb, 0xcc]);
    assert_eq!(m.server_id.as_ref().unwrap().len(), 10);
    assert_eq!(m.dns, vec![v6(&[0xfd00, 0, 0, 0, 0, 0, 0, 0x53])]);
    assert_eq!(m.domains, vec![alloc::string::String::from("corp.example")]);
    assert_eq!(
        m.captive_portal.as_deref(),
        Some("https://login.corp.example/")
    );
    let l = m.lease.unwrap();
    assert_eq!(
        (l.addr, l.preferred, l.valid, l.t1, l.t2),
        (addr, 3000, 7200, 3600, 5400)
    );

    // IA with NoAddrsAvail (2) and no address.
    let mut bad = server_reply(dhcpv6::REPLY, Some((&addr, 0, 2)));
    let m = dhcpv6::parse(&bad).unwrap();
    assert!(m.lease.is_none());
    // Truncated options are ignored rather than read out of bounds.
    bad.truncate(bad.len() - 3);
    assert!(dhcpv6::parse(&bad).is_some());
    assert!(dhcpv6::parse(&[1, 2]).is_none());
}
