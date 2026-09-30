//! nc: TCP/UDP client and server that shuttles stdin/stdout.

use crate::err;
use rustos_rt::io::{self, POLLHUP, POLLIN, PollFd};
use rustos_rt::net::{self, AF_INET, Ipv4, SOCK_DGRAM, Socket, SocketAddr};
use rustos_rt::prelude::*;

fn usage() -> i32 {
    eprintln!("usage: nc [-u] [-w SECS] [-z] HOST PORT\n       nc -l [-u] [-p] PORT");
    2
}

/// Copy between stdin and the socket until both directions are done.
fn pump(sock: &Socket, udp_peer: Option<SocketAddr>, mut idle_ms: i32) -> i32 {
    let mut stdin_open = true;
    let mut buf = vec![0u8; 16384];
    loop {
        let mut fds = [
            PollFd {
                fd: sock.fd(),
                events: POLLIN,
                revents: 0,
            },
            PollFd {
                fd: io::STDIN,
                events: if stdin_open { POLLIN } else { 0 },
                revents: 0,
            },
        ];
        let n = match io::poll(
            if stdin_open {
                &mut fds[..]
            } else {
                &mut fds[..1]
            },
            idle_ms,
        ) {
            Ok(n) => n,
            Err(e) => return err("nc", "poll", e),
        };
        if n == 0 {
            return 0; // idle timeout
        }
        if fds[0].revents & (POLLIN | POLLHUP) != 0 {
            match sock.recv(&mut buf) {
                Ok(0) => return 0,
                Ok(n) => {
                    let _ = io::write_all(io::STDOUT, &buf[..n]);
                }
                Err(e) => return err("nc", "recv", e),
            }
        }
        if stdin_open && fds[1].revents & (POLLIN | POLLHUP) != 0 {
            match io::read(io::STDIN, &mut buf) {
                Ok(0) | Err(_) => {
                    stdin_open = false;
                    if udp_peer.is_none() {
                        let _ = sock.shutdown(1);
                    } else if idle_ms < 0 {
                        // No -w: keep listening for replies a little while.
                        idle_ms = 2000;
                    }
                }
                Ok(n) => {
                    let r = match udp_peer {
                        Some(p) => sock.send_to(&buf[..n], p).map(|_| ()),
                        None => sock.send_all(&buf[..n]),
                    };
                    if let Err(e) = r {
                        return err("nc", "send", e);
                    }
                }
            }
        }
    }
}

pub fn nc(args: &[String]) -> i32 {
    let mut listen = false;
    let mut udp = false;
    let mut scan = false;
    let mut wait: i32 = -1;
    let mut pos: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-l" => listen = true,
            "-u" => udp = true,
            "-z" => scan = true,
            "-p" => {}
            "-w" => {
                i += 1;
                wait = args
                    .get(i)
                    .and_then(|w| w.parse::<i32>().ok())
                    .map_or(-1, |s| s * 1000);
            }
            "-n" | "-v" => {}
            a if a.starts_with('-') => return usage(),
            a => pos.push(a.to_string()),
        }
        i += 1;
    }
    if listen {
        let Some(port) = pos.last().and_then(|p| p.parse::<u16>().ok()) else {
            return usage();
        };
        let any = SocketAddr {
            ip: Ipv4::ANY,
            port,
        };
        if udp {
            let s = match net::udp_bind(any) {
                Ok(s) => s,
                Err(e) => return err("nc", "bind", e),
            };
            // Answer whoever talks first.
            let mut buf = vec![0u8; 65536];
            let (n, peer) = match s.recv_from(&mut buf) {
                Ok(x) => x,
                Err(e) => return err("nc", "recv", e),
            };
            let _ = io::write_all(io::STDOUT, &buf[..n]);
            return pump(&s, Some(peer), wait);
        }
        let l = match net::tcp_listen(any) {
            Ok(s) => s,
            Err(e) => return err("nc", "listen", e),
        };
        let (c, _peer) = match l.accept() {
            Ok(x) => x,
            Err(e) => return err("nc", "accept", e),
        };
        drop(l);
        return pump(&c, None, wait);
    }
    if pos.len() < 2 {
        return usage();
    }
    let addr = match net::resolve_addr(&format!("{}:{}", pos[0], pos[1]), 0) {
        Ok(a) => a,
        Err(e) => return err("nc", &pos[0], e),
    };
    if udp {
        let s = match Socket::new(AF_INET, SOCK_DGRAM, 0) {
            Ok(s) => s,
            Err(e) => return err("nc", "socket", e),
        };
        return pump(&s, Some(addr), wait);
    }
    match net::tcp_connect(addr) {
        Ok(s) => {
            if scan {
                println!("Connection to {} port {} succeeded!", pos[0], pos[1]);
                return 0;
            }
            pump(&s, None, wait)
        }
        Err(e) => err("nc", &format!("{}:{}", pos[0], pos[1]), e),
    }
}
