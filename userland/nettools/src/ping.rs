//! ping: ICMP echo over a datagram ICMP socket.

use crate::err;
use rustos_rt::net::{self, AF_INET, IPPROTO_ICMP, SOCK_DGRAM, Socket, SocketAddr};
use rustos_rt::prelude::*;
use rustos_rt::{signal, time};

fn usage() -> i32 {
    eprintln!("usage: ping [-c COUNT] [-i INTERVAL] [-W TIMEOUT] [-s SIZE] [-I ADDR] [-q] HOST");
    2
}

pub fn ping(args: &[String]) -> i32 {
    let mut count: Option<u32> = None;
    let mut interval_ms = 1000u64;
    let mut wait_ms = 2000u64;
    let mut size = 56usize;
    let mut quiet = false;
    let mut host = None;
    let mut source = None;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        let mut val = || {
            i += 1;
            args.get(i).cloned()
        };
        match a {
            "-c" => count = val().and_then(|v| v.parse().ok()),
            "-i" => {
                interval_ms = val()
                    .and_then(|v| v.parse::<f64>().ok())
                    .map_or(1000, |s| (s * 1000.0) as u64)
                    .max(10)
            }
            "-W" => {
                wait_ms = val()
                    .and_then(|v| v.parse::<u64>().ok())
                    .map_or(2000, |s| s * 1000)
            }
            "-s" => size = val().and_then(|v| v.parse().ok()).unwrap_or(56).min(8192),
            "-q" => quiet = true,
            // Send from this local address (and its interface).
            "-I" => match val().and_then(|v| net::Ipv4::parse(&v)) {
                Some(a) => source = Some(a),
                None => return usage(),
            },
            h if !h.starts_with('-') => host = Some(h.to_string()),
            _ => return usage(),
        }
        i += 1;
    }
    let Some(host) = host else { return usage() };
    let ip = match net::resolve(&host) {
        Ok(v) => v[0],
        Err(e) => return err("ping", &host, e),
    };
    let sock = match Socket::new(AF_INET, SOCK_DGRAM, IPPROTO_ICMP) {
        Ok(s) => s,
        Err(e) => return err("ping", "socket", e),
    };
    if let Some(src) = source
        && let Err(e) = sock.bind(SocketAddr { ip: src, port: 0 })
    {
        return err("ping", "bind", e);
    }
    let _ = sock.set_timeout(wait_ms.max(1));
    println!(
        "PING {} ({}) {}({}) bytes of data.",
        host,
        ip,
        size,
        size + 28
    );
    let stop = signal::interrupted_flag();
    let (mut sent, mut recv) = (0u32, 0u32);
    let (mut tmin, mut tmax, mut tsum) = (u64::MAX, 0u64, 0u64);
    let start = time::millis();
    let mut seq: u16 = 1;
    loop {
        if count.is_some_and(|c| sent >= c) || stop.get() {
            break;
        }
        let mut pkt = vec![0u8; 8 + size];
        pkt[0] = 8; // echo request
        pkt[6..8].copy_from_slice(&seq.to_be_bytes());
        let t0 = time::micros();
        if size >= 8 {
            pkt[8..16].copy_from_slice(&t0.to_ne_bytes());
        }
        for (k, b) in pkt.iter_mut().enumerate().skip(16) {
            *b = k as u8;
        }
        if let Err(e) = sock.send_to(&pkt, SocketAddr { ip, port: 0 }) {
            eprintln!("ping: sendto: {}", e.message());
        } else {
            sent += 1;
        }
        let deadline = time::millis() + wait_ms;
        let mut got = false;
        while time::millis() < deadline && !stop.get() {
            let mut buf = [0u8; 9000];
            match sock.recv_from(&mut buf) {
                Ok((n, from)) if n >= 8 && buf[0] == 0 => {
                    let rseq = u16::from_be_bytes([buf[6], buf[7]]);
                    if rseq != seq {
                        continue;
                    }
                    let rtt = time::micros().saturating_sub(t0);
                    recv += 1;
                    tmin = tmin.min(rtt);
                    tmax = tmax.max(rtt);
                    tsum += rtt;
                    if !quiet {
                        println!(
                            "{} bytes from {}: icmp_seq={} ttl=64 time={}.{:03} ms",
                            n,
                            from.ip,
                            seq,
                            rtt / 1000,
                            rtt % 1000
                        );
                    }
                    got = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        if !got && !quiet && !stop.get() {
            println!("Request timeout for icmp_seq {}", seq);
        }
        seq = seq.wrapping_add(1);
        if count.is_some_and(|c| sent >= c) {
            break;
        }
        let wake = time::millis() + interval_ms.saturating_sub(if got { 0 } else { wait_ms });
        while time::millis() < wake && !stop.get() {
            time::sleep_ms(10.min(wake - time::millis()));
        }
    }
    let elapsed = time::millis() - start;
    println!("\n--- {} ping statistics ---", host);
    let loss = if sent == 0 {
        0
    } else {
        (sent - recv) * 100 / sent
    };
    println!(
        "{} packets transmitted, {} received, {}% packet loss, time {}ms",
        sent, recv, loss, elapsed
    );
    if recv > 0 {
        let avg = tsum / recv as u64;
        println!(
            "rtt min/avg/max = {}.{:03}/{}.{:03}/{}.{:03} ms",
            tmin / 1000,
            tmin % 1000,
            avg / 1000,
            avg % 1000,
            tmax / 1000,
            tmax % 1000
        );
    }
    if recv > 0 { 0 } else { 1 }
}
