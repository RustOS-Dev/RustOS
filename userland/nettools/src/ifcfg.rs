//! Interface configuration: ip, ifconfig, route, arp, dhcp.

use crate::err;
use rustos_rt::net::{self, IfInfo, Ipv4};
use rustos_rt::prelude::*;
use rustos_rt::{fs, time};

fn show_iface(i: &IfInfo, brief: bool) {
    if brief {
        println!(
            "{:<8} {:<5} {}",
            i.name,
            if i.up && i.link { "UP" } else if i.up { "NO-CARRIER" } else { "DOWN" },
            i.addrs.join(" ")
        );
        return;
    }
    let mut flags = vec![];
    if i.up {
        flags.push("UP");
    }
    if i.driver == "loopback" {
        flags.push("LOOPBACK");
    } else {
        flags.push("BROADCAST");
        flags.push("MULTICAST");
    }
    if i.up && i.link {
        flags.push("RUNNING");
    }
    println!("{}: {}: <{}> mtu {}", i.index, i.name, flags.join(","), i.mtu);
    if i.driver != "loopback" {
        println!("    link/ether {} driver {}{}", i.mac, i.driver, if i.dhcp { " dhcp" } else { "" });
    } else {
        println!("    link/loopback");
    }
    for a in &i.addrs {
        let fam = if a.contains(':') { "inet6" } else { "inet" };
        println!("    {} {}", fam, a);
    }
}

fn find(name: &str) -> Option<IfInfo> {
    let i = net::interface(name);
    if i.is_none() {
        eprintln!("Device \"{}\" does not exist.", name);
    }
    i
}

/// ip addr|link|route ...
pub fn ip(args: &[String]) -> i32 {
    let a: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    let brief = a.first() == Some(&"-br") || a.first() == Some(&"-brief");
    let a = if brief { &a[1..] } else { &a[..] };
    let obj = a.first().copied().unwrap_or("addr");
    let rest = if a.is_empty() { &a[..] } else { &a[1..] };
    match obj {
        "a" | "addr" | "address" | "l" | "link" => match rest.first().copied() {
            None | Some("show") | Some("list") => {
                let only = rest.iter().position(|&w| w == "dev").and_then(|p| rest.get(p + 1));
                for i in net::interfaces() {
                    if only.is_none_or(|o| *o == i.name) {
                        show_iface(&i, brief);
                    }
                }
                0
            }
            Some("add") | Some("del") => {
                let Some(cidr) = rest.get(1).and_then(|c| net::parse_cidr(c, 24)) else {
                    eprintln!("usage: ip addr add|del A.B.C.D/P dev IFACE");
                    return 1;
                };
                let Some(dev) = rest.iter().position(|&w| w == "dev").and_then(|p| rest.get(p + 1)) else {
                    eprintln!("ip: missing 'dev IFACE'");
                    return 1;
                };
                let r = if rest[0] == "add" {
                    net::set_address(dev, cidr.0, cidr.1)
                } else {
                    net::set_address(dev, Ipv4::ANY, 0)
                };
                r.map_or_else(|e| err("ip", dev, e), |_| 0)
            }
            Some("set") => {
                let Some(dev) = rest.get(1) else {
                    eprintln!("usage: ip link set IFACE up|down");
                    return 1;
                };
                let up = match rest.get(2).copied() {
                    Some("up") => true,
                    Some("down") => false,
                    _ => {
                        eprintln!("usage: ip link set IFACE up|down");
                        return 1;
                    }
                };
                net::set_up(dev, up).map_or_else(|e| err("ip", dev, e), |_| 0)
            }
            Some(o) => {
                eprintln!("ip: unknown command '{}'", o);
                1
            }
        },
        "r" | "route" => route_cmd(rest),
        "n" | "neigh" => arp(&[]),
        _ => {
            eprintln!("usage: ip [-br] {{addr|link|route|neigh}} [show|add|del|set] ...");
            1
        }
    }
}

fn show_routes() {
    println!("{:<18} {:<16} {:<8}", "Destination", "Gateway", "Iface");
    for i in net::interfaces() {
        if let Some(g) = i.gateway {
            println!("{:<18} {:<16} {:<8}", "default", g.to_string(), i.name);
        }
    }
    let data = fs::read_to_string("/proc/net/route").unwrap_or_default();
    for line in data.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 8 || f[1] == "00000000" {
            continue;
        }
        let hex = |s: &str| Ipv4(u32::from_str_radix(s, 16).unwrap_or(0).to_le_bytes());
        let dst = hex(f[1]);
        let gw = hex(f[2]);
        let mask = hex(f[7]);
        println!(
            "{:<18} {:<16} {:<8}",
            format!("{}/{}", dst, net::mask_prefix(mask)),
            if gw == Ipv4::ANY { String::from("*") } else { gw.to_string() },
            f[0]
        );
    }
}

fn route_cmd(a: &[&str]) -> i32 {
    match a.first().copied() {
        None | Some("show") | Some("list") => {
            show_routes();
            0
        }
        Some(op @ ("add" | "del" | "delete")) => {
            let add = op == "add";
            let Some(target) = a.get(1) else {
                eprintln!("usage: ip route add|del {{default|A.B.C.D/P}} via GW [dev IFACE]");
                return 1;
            };
            let (dst, prefix) = if *target == "default" {
                (Ipv4::ANY, 0)
            } else {
                match net::parse_cidr(target, 32) {
                    Some(c) => c,
                    None => {
                        eprintln!("ip: bad destination '{}'", target);
                        return 1;
                    }
                }
            };
            let gw = a
                .iter()
                .position(|&w| w == "via")
                .and_then(|p| a.get(p + 1))
                .and_then(|g| Ipv4::parse(g))
                .unwrap_or(Ipv4::ANY);
            let dev = a.iter().position(|&w| w == "dev").and_then(|p| a.get(p + 1)).copied();
            net::route(add, dst, prefix, gw, dev).map_or_else(|e| err("ip route", target, e), |_| 0)
        }
        Some(o) => {
            eprintln!("ip route: unknown command '{}'", o);
            1
        }
    }
}

/// route [-n] | route add|del default gw GW | route add -net N netmask M gw GW [dev IF]
pub fn route(args: &[String]) -> i32 {
    let a: Vec<&str> = args[1..].iter().map(|s| s.as_str()).filter(|s| *s != "-n").collect();
    match a.first().copied() {
        None => {
            show_routes();
            0
        }
        Some(op @ ("add" | "del")) => {
            let get = |k: &str| a.iter().position(|&w| w == k).and_then(|p| a.get(p + 1)).copied();
            let (dst, prefix) = if a.contains(&"default") {
                (Ipv4::ANY, 0)
            } else if let Some(n) = get("-net").or(get("-host")) {
                let p = get("netmask").and_then(Ipv4::parse).map_or(32, net::mask_prefix);
                match net::parse_cidr(n, p) {
                    Some(c) => c,
                    None => {
                        eprintln!("route: bad network '{}'", n);
                        return 1;
                    }
                }
            } else {
                eprintln!("usage: route add|del default gw GW | route add -net N netmask M gw GW");
                return 1;
            };
            let gw = get("gw").and_then(Ipv4::parse).unwrap_or(Ipv4::ANY);
            net::route(op == "add", dst, prefix, gw, get("dev")).map_or_else(|e| err("route", op, e), |_| 0)
        }
        Some(o) => {
            eprintln!("route: unknown command '{}'", o);
            1
        }
    }
}

/// ifconfig [IFACE [ADDR] [netmask M] [up|down]]
pub fn ifconfig(args: &[String]) -> i32 {
    let a: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    let all = a.first() == Some(&"-a");
    let a = if all { &a[1..] } else { &a[..] };
    let print = |i: &IfInfo| {
        let v4 = i.ipv4();
        println!(
            "{}: flags=<{}{}> mtu {}",
            i.name,
            if i.up { "UP," } else { "" },
            if i.driver == "loopback" { "LOOPBACK" } else { "BROADCAST,MULTICAST" },
            i.mtu
        );
        if let Some((ip, p)) = v4 {
            println!("        inet {}  netmask {}", ip, net::prefix_mask(p));
        }
        for a in i.addrs.iter().filter(|a| a.contains(':')) {
            println!("        inet6 {}", a);
        }
        if i.driver != "loopback" {
            println!("        ether {}  ({})", i.mac, i.driver);
        }
        let stats = fs::read_to_string("/proc/net/dev").unwrap_or_default();
        if let Some(l) = stats.lines().find(|l| l.trim_start().starts_with(&format!("{}:", i.name))) {
            let f: Vec<&str> = l.split(':').nth(1).unwrap_or("").split_whitespace().collect();
            if f.len() >= 10 {
                println!("        RX packets {}  bytes {}", f[1], f[0]);
                println!("        TX packets {}  bytes {}  errors {}", f[9], f[8], f[10]);
            }
        }
        println!();
    };
    if a.is_empty() {
        for i in net::interfaces() {
            if all || i.up {
                print(&i);
            }
        }
        return 0;
    }
    let Some(i) = find(a[0]) else { return 1 };
    if a.len() == 1 {
        print(&i);
        return 0;
    }
    let mut k = 1;
    let mut addr: Option<(Ipv4, u8)> = None;
    while k < a.len() {
        match a[k] {
            "up" => {
                if let Err(e) = net::set_up(&i.name, true) {
                    return err("ifconfig", &i.name, e);
                }
            }
            "down" => {
                if let Err(e) = net::set_up(&i.name, false) {
                    return err("ifconfig", &i.name, e);
                }
            }
            "netmask" => {
                k += 1;
                let Some(m) = a.get(k).and_then(|m| Ipv4::parse(m)) else {
                    eprintln!("ifconfig: bad netmask");
                    return 1;
                };
                let ip = addr.map(|x| x.0).or(i.ipv4().map(|x| x.0)).unwrap_or(Ipv4::ANY);
                addr = Some((ip, net::mask_prefix(m)));
            }
            w => match net::parse_cidr(w, i.ipv4().map_or(24, |x| x.1)) {
                Some(c) => addr = Some(c),
                None => {
                    eprintln!("ifconfig: unknown argument '{}'", w);
                    return 1;
                }
            },
        }
        k += 1;
    }
    if let Some((ip, p)) = addr
        && let Err(e) = net::set_address(&i.name, ip, p)
    {
        return err("ifconfig", &i.name, e);
    }
    0
}

pub fn arp(_args: &[String]) -> i32 {
    print!("{}", fs::read_to_string("/proc/net/arp").unwrap_or_default());
    0
}

/// dhcp [-r] [-t SECS] [IFACE]: (re)start the kernel DHCP client and wait
/// for a lease.
pub fn dhcp(args: &[String]) -> i32 {
    let mut release = false;
    let mut timeout = 15u64;
    let mut name = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-r" => release = true,
            "-t" => {
                i += 1;
                timeout = args.get(i).and_then(|t| t.parse().ok()).unwrap_or(timeout);
            }
            "-q" | "-v" => {}
            n => name = Some(n.to_string()),
        }
        i += 1;
    }
    let name = match name {
        Some(n) => n,
        None => match net::interfaces().into_iter().find(|i| i.driver != "loopback") {
            Some(i) => i.name,
            None => {
                eprintln!("dhcp: no network interfaces");
                return 1;
            }
        },
    };
    if release {
        if let Err(e) = net::set_dhcp(&name, false) {
            return err("dhcp", &name, e);
        }
        let _ = net::set_address(&name, Ipv4::ANY, 0);
        println!("{}: lease released", name);
        return 0;
    }
    // Clear the old address so we can tell when the new lease arrives.
    let _ = net::set_address(&name, Ipv4::ANY, 0);
    if let Err(e) = net::set_dhcp(&name, true) {
        return err("dhcp", &name, e);
    }
    println!("{}: requesting DHCP lease...", name);
    let start = time::millis();
    while time::millis() - start < timeout * 1000 {
        if let Some(i) = net::interface(&name)
            && let Some((ip, p)) = i.ipv4()
        {
            println!(
                "{}: leased {}/{}{}",
                name,
                ip,
                p,
                i.gateway.map_or(String::new(), |g| format!(" gateway {}", g))
            );
            return 0;
        }
        time::sleep_ms(200);
    }
    eprintln!("{}: no DHCP lease after {} s", name, timeout);
    1
}

/// ifup [-a | IFACE] [-q]: apply /etc/network.conf.
pub fn ifup(args: &[String]) -> i32 {
    let quiet = args.iter().any(|a| a == "-q");
    let all = args.iter().any(|a| a == "-a");
    let only: Option<&String> = args[1..].iter().find(|a| !a.starts_with('-'));
    if !all && only.is_none() {
        eprintln!("usage: ifup -a | ifup IFACE");
        return 2;
    }
    let conf = fs::read_to_string("/storage/etc/network.conf")
        .or_else(|_| fs::read_to_string("/etc/network.conf"))
        .unwrap_or_default();
    let mut dns: Vec<Ipv4> = Vec::new();
    let mut status = 0;
    for line in conf.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() < 2 || only.is_some_and(|o| o != w[0]) {
            continue;
        }
        let name = w[0];
        if net::interface(name).is_none() {
            if !quiet {
                eprintln!("ifup: {}: no such interface", name);
            }
            continue;
        }
        let r = match w[1] {
            "dhcp" => net::set_up(name, true).and_then(|_| net::set_dhcp(name, true)),
            "down" => net::set_up(name, false),
            "static" => {
                let Some((ip, p)) = w.get(2).and_then(|c| net::parse_cidr(c, 24)) else {
                    eprintln!("ifup: {}: bad address", name);
                    status = 1;
                    continue;
                };
                let gw = w.get(3).and_then(|g| Ipv4::parse(g));
                dns.extend(w.iter().skip(4).filter_map(|d| Ipv4::parse(d)));
                net::set_up(name, true)
                    .and_then(|_| net::set_address(name, ip, p))
                    .and_then(|_| net::set_gateway(name, gw))
            }
            m => {
                eprintln!("ifup: {}: unknown method '{}'", name, m);
                status = 1;
                continue;
            }
        };
        match r {
            Ok(()) => {
                if !quiet {
                    println!("ifup: {} {}", name, w[1]);
                }
            }
            Err(e) => status = err("ifup", name, e),
        }
    }
    if !dns.is_empty() {
        let mut s = String::from("# written by ifup from network.conf\n");
        for d in dns {
            s.push_str(&format!("nameserver {}\n", d));
        }
        let _ = fs::write("/etc/resolv.conf", s.as_bytes());
    }
    status
}
