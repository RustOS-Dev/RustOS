//! ntpdate: set the clock from an SNTP server.

use crate::err;
use rustos_rt::net::{self, AF_INET, SOCK_DGRAM, Socket, SocketAddr};
use rustos_rt::prelude::*;
use rustos_rt::time;

const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

pub fn ntpdate(args: &[String]) -> i32 {
    let query_only = args.iter().any(|a| a == "-q");
    let server = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| String::from("pool.ntp.org"));
    let ip = match net::resolve(&server) {
        Ok(v) => v[0],
        Err(e) => return err("ntpdate", &server, e),
    };
    let s = match Socket::new(AF_INET, SOCK_DGRAM, 0) {
        Ok(s) => s,
        Err(e) => return err("ntpdate", "socket", e),
    };
    let _ = s.set_timeout(3000);
    let mut req = [0u8; 48];
    req[0] = 0x23; // LI 0, version 4, client
    for _ in 0..3 {
        let t0 = time::millis();
        if let Err(e) = s.send_to(&req, SocketAddr { ip, port: 123 }) {
            return err("ntpdate", &server, e);
        }
        let mut buf = [0u8; 128];
        if let Ok((n, _)) = s.recv_from(&mut buf)
            && n >= 48
        {
            let rtt = time::millis() - t0;
            let secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) as u64;
            let frac = u32::from_be_bytes([buf[44], buf[45], buf[46], buf[47]]) as u64;
            if secs < NTP_UNIX_OFFSET {
                eprintln!("ntpdate: bad reply from {}", server);
                return 1;
            }
            let usec = (frac * 1_000_000 >> 32) + rtt * 500;
            let unix = secs - NTP_UNIX_OFFSET + usec / 1_000_000;
            let offset = unix as i64 - time::now() as i64;
            println!(
                "server {}, stratum {}, offset {} s, delay {} ms",
                ip, buf[1], offset, rtt
            );
            if !query_only {
                if let Err(e) = time::set_time(unix, usec % 1_000_000) {
                    return err("ntpdate", "settimeofday", e);
                }
                let (y, mo, d, h, mi, sec) = time::civil(unix);
                println!(
                    "clock set to {:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
                    y, mo, d, h, mi, sec
                );
            }
            return 0;
        }
    }
    eprintln!("ntpdate: no reply from {}", server);
    1
}
