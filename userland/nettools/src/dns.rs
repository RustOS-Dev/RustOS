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
    match net::resolve(name) {
        Ok(ips) => {
            if !host_style {
                println!("Name:\t{}", name);
            }
            for ip in ips {
                if host_style {
                    println!("{} has address {}", name, ip);
                } else {
                    println!("Address: {}", ip);
                }
            }
            0
        }
        Err(e) => err(&args[0], name, e),
    }
}
