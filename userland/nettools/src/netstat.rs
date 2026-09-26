//! netstat: TCP/UDP sockets from /proc/net and interface statistics.

use rustos_rt::fs;
use rustos_rt::net::Ipv4;
use rustos_rt::prelude::*;

const STATES: [&str; 12] = [
    "UNKNOWN",
    "ESTABLISHED",
    "SYN_SENT",
    "SYN_RECV",
    "FIN_WAIT1",
    "FIN_WAIT2",
    "TIME_WAIT",
    "CLOSE",
    "CLOSE_WAIT",
    "LAST_ACK",
    "LISTEN",
    "CLOSING",
];

fn addr(s: &str) -> String {
    let (a, p) = s.split_once(':').unwrap_or((s, "0"));
    let port = u16::from_str_radix(p, 16).unwrap_or(0);
    if a.len() == 8 {
        let ip = Ipv4(u32::from_str_radix(a, 16).unwrap_or(0).to_le_bytes());
        format!("{}:{}", ip, if port == 0 { String::from("*") } else { port.to_string() })
    } else {
        // IPv6: 4 little-endian words.
        let mut b = [0u8; 16];
        for w in 0..4 {
            let v = u32::from_str_radix(a.get(w * 8..w * 8 + 8).unwrap_or("0"), 16).unwrap_or(0);
            b[w * 4..w * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let groups: Vec<String> = b
            .chunks(2)
            .map(|c| format!("{:x}", u16::from_be_bytes([c[0], c[1]])))
            .collect();
        format!("[{}]:{}", groups.join(":"), port)
    }
}

pub fn netstat(args: &[String]) -> i32 {
    let flags: String = args[1..]
        .iter()
        .filter(|a| a.starts_with('-'))
        .flat_map(|a| a.chars().skip(1))
        .collect();
    if flags.contains('i') {
        print!("{}", fs::read_to_string("/proc/net/dev").unwrap_or_default());
        return 0;
    }
    if flags.contains('r') {
        return crate::ifcfg::route(&[String::from("route")]);
    }
    let want_tcp = flags.contains('t') || !flags.contains('u');
    let want_udp = flags.contains('u') || !flags.contains('t');
    let listening_only = flags.contains('l');
    let all = flags.contains('a');
    println!("Proto Recv-Q Send-Q {:<23} {:<23} State", "Local Address", "Foreign Address");
    for (proto, file, on) in [("tcp", "/proc/net/tcp", want_tcp), ("udp", "/proc/net/udp", want_udp)] {
        if !on {
            continue;
        }
        let data = fs::read_to_string(file).unwrap_or_default();
        for line in data.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 5 {
                continue;
            }
            let st = usize::from_str_radix(f[3], 16).unwrap_or(0).min(11);
            let listening = st == 10 || (proto == "udp");
            if listening_only && !listening {
                continue;
            }
            if !listening_only && !all && listening && proto == "tcp" {
                continue;
            }
            let (tx, rx) = f[4].split_once(':').unwrap_or(("0", "0"));
            println!(
                "{:<5} {:>6} {:>6} {:<23} {:<23} {}",
                proto,
                usize::from_str_radix(rx, 16).unwrap_or(0),
                usize::from_str_radix(tx, 16).unwrap_or(0),
                addr(f[1]),
                addr(f[2]),
                if proto == "udp" { "" } else { STATES[st] }
            );
        }
    }
    0
}
