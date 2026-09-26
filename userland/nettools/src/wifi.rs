//! wifi: wireless interface control (scan, connect, status, disconnect).

use rustos_rt::net;
use rustos_rt::prelude::*;

pub fn wifi(_args: &[String]) -> i32 {
    let wl: Vec<_> = net::interfaces()
        .into_iter()
        .filter(|i| i.name.starts_with("wlan"))
        .collect();
    if wl.is_empty() {
        eprintln!("wifi: no wireless interfaces");
        return 1;
    }
    print!("{}", rustos_rt::fs::read_to_string("/proc/net/wireless").unwrap_or_default());
    0
}
