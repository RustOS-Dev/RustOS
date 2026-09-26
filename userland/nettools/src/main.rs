//! nettools: multi-call binary with the networking commands.

#![no_std]
#![no_main]

extern crate alloc;

mod dns;
mod http;
mod ifcfg;
mod nc;
mod netstat;
mod ntp;
mod ping;
mod wifi;

use rustos_rt::prelude::*;

rustos_rt::entry!(main);

type Applet = fn(&[String]) -> i32;

const APPLETS: &[(&str, Applet)] = &[
    ("arp", ifcfg::arp),
    ("dhclient", ifcfg::dhcp),
    ("dhcp", ifcfg::dhcp),
    ("host", dns::nslookup),
    ("httpd", http::httpd),
    ("ifconfig", ifcfg::ifconfig),
    ("ip", ifcfg::ip),
    ("nc", nc::nc),
    ("netstat", netstat::netstat),
    ("nslookup", dns::nslookup),
    ("ntpdate", ntp::ntpdate),
    ("ping", ping::ping),
    ("route", ifcfg::route),
    ("wget", http::wget),
    ("wifi", wifi::wifi),
];

fn main(args: Vec<String>) -> i32 {
    let name = args
        .first()
        .map(|a| rustos_rt::fs::basename(a).to_string())
        .unwrap_or_default();
    if let Some((_, f)) = APPLETS.iter().find(|(n, _)| *n == name) {
        return f(&args);
    }
    if args.len() > 1
        && let Some((_, f)) = APPLETS.iter().find(|(n, _)| *n == args[1])
    {
        return f(&args[1..]);
    }
    eprintln!("usage: nettools <applet> [args]\napplets:");
    for (n, _) in APPLETS {
        eprint!(" {}", n);
    }
    eprintln!();
    1
}

pub fn err(applet: &str, what: &str, e: rustos_rt::Error) -> i32 {
    eprintln!("{}: {}: {}", applet, what, e.message());
    1
}
