//! nslookup / host.

use crate::err;
use rustos_rt::net;
use rustos_rt::prelude::*;

pub fn nslookup(args: &[String]) -> i32 {
    let Some(name) = args.iter().skip(1).find(|a| !a.starts_with('-')) else {
        eprintln!("usage: {} NAME", args[0]);
        return 2;
    };
    let servers = net::nameservers();
    let host_style = args[0].ends_with("host");
    if !host_style {
        println!("Server:\t\t{}\nAddress:\t{}#53\n", servers[0], servers[0]);
    }
    let v4only = args.iter().any(|a| a == "-4");
    let v6only = args.iter().any(|a| a == "-6");
    match net::resolve_all(name).map(|v| {
        v.into_iter()
            .filter(|a| !(v4only && a.is_v6()) && !(v6only && !a.is_v6()))
            .collect::<Vec<_>>()
    }) {
        Ok(ips) => {
            if !host_style {
                println!("Name:\t{}", name);
            }
            for ip in ips {
                if host_style {
                    let kind = if ip.is_v6() {
                        "IPv6 address"
                    } else {
                        "address"
                    };
                    println!("{} has {} {}", name, kind, ip);
                } else {
                    println!("Address: {}", ip);
                }
            }
            0
        }
        Err(e) => err(&args[0], name, e),
    }
}
