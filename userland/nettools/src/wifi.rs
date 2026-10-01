//! wifi: wireless interface control.
//!
//!   wifi [status] [-i IFACE]
//!   wifi scan [-i IFACE]
//!   wifi connect SSID [PASSPHRASE] [--save] [-i IFACE]
//!   wifi connect SSID PASSWORD --eap peap|ttls --identity ID
//!        [--anonymous ID] [--ca CERT.pem] [-i IFACE]
//!   wifi disconnect [-i IFACE]
//!   wifi auto [-q]            join the first reachable network in wifi.conf
//!   wifi forget SSID
//!   wifi power [on|off|auto]  power save (auto: on while on battery)
//!
//! Saved networks live in /storage/etc/wifi.conf (persistent) or
//! /etc/wifi.conf, as `ssid=...` / `psk=...` pairs, one network per block.
//!
//! RustOS's own drivers (Intel AX210) are driven through interface ioctls;
//! interfaces of Linux drivers (LinuxKPI) through wpa_supplicant, which
//! `wifi` starts on demand (src/wpa.rs).

use crate::wpa;
use rustos_rt::net;
use rustos_rt::prelude::*;

const WIFI_STATUS: u64 = 0x89F8;
const WIFI_SCAN: u64 = 0x89F9;
const WIFI_CONNECT: u64 = 0x89FA;
const WIFI_DISCONNECT: u64 = 0x89FB;
const WIFI_POWER: u64 = 0x89FD;

const CONF_PERSISTENT: &str = "/storage/etc/wifi.conf";
const CONF: &str = "/etc/wifi.conf";

#[repr(C)]
struct WifiIfReq {
    name: [u8; 16],
    buf: u64,
    len: u64,
    arg: u64,
    arg_len: u64,
}

fn request(iface: &str, cmd: u64, buf: &mut [u8], arg: &[u8]) -> rustos_rt::Result<usize> {
    let mut r = WifiIfReq {
        name: [0; 16],
        buf: buf.as_mut_ptr() as u64,
        len: buf.len() as u64,
        arg: arg.as_ptr() as u64,
        arg_len: arg.len() as u64,
    };
    let n = iface.len().min(15);
    r.name[..n].copy_from_slice(&iface.as_bytes()[..n]);
    let s = net::Socket::new(net::AF_INET, net::SOCK_DGRAM, 0)?;
    s.ioctl(cmd, &mut r as *mut WifiIfReq as usize)
}

/// True if the interface belongs to a RustOS driver (answers the wifi
/// ioctls); false for Linux drivers, which wpa_supplicant drives.
fn native(iface: &str) -> bool {
    let mut buf = vec![0u8; 512];
    request(iface, WIFI_STATUS, &mut buf, &[]).is_ok()
}

fn status(iface: &str) -> String {
    let mut buf = vec![0u8; 512];
    match request(iface, WIFI_STATUS, &mut buf, &[]) {
        Ok(n) => String::from_utf8_lossy(&buf[..n]).into_owned(),
        Err(_) => wpa::status(iface),
    }
}

fn field<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    let i = status.find(&format!("{}=", key))?;
    let rest = &status[i + key.len() + 1..];
    if let Some(q) = rest.strip_prefix('"') {
        return q.split('"').next();
    }
    rest.split_whitespace().next()
}

fn default_iface() -> Option<String> {
    net::interfaces()
        .into_iter()
        .map(|i| i.name)
        .find(|n| n.starts_with("wlan"))
}

fn print_status(iface: &str) {
    let s = status(iface);
    let state = field(&s, "state").unwrap_or("unknown");
    println!("{}: {}", iface, state);
    for k in [
        "ssid", "bssid", "channel", "signal", "security", "mode", "rate", "country", "firmware",
        "power", "msg",
    ] {
        if let Some(v) = field(&s, k) {
            println!("  {:<9}{}", k, v);
        }
    }
    if state == "connected"
        && let Some(i) = net::interface(iface)
        && let Some(a) = i.addrs.first()
    {
        println!("  {:<9}{}", "address", a);
    }
}

fn scan_text(iface: &str) -> Result<String, String> {
    if !native(iface) {
        return wpa::scan(iface);
    }
    let mut buf = vec![0u8; 16384];
    request(iface, WIFI_SCAN, &mut buf, &[])
        .map(|n| String::from_utf8_lossy(&buf[..n]).into_owned())
        .map_err(|e| format!("{}", e))
}

fn scan(iface: &str) -> i32 {
    match scan_text(iface) {
        Ok(text) => {
            println!(
                "{:<17}  {:>4}  {:>6}  {:<10}  SSID",
                "BSSID", "CHAN", "SIGNAL", "SECURITY"
            );
            for l in text.lines() {
                let f: Vec<&str> = l.splitn(5, '\t').collect();
                if f.len() == 5 {
                    let ssid = if f[4].is_empty() { "(hidden)" } else { f[4] };
                    println!(
                        "{:<17}  {:>4}  {:>6}  {:<10}  {}",
                        f[0], f[1], f[2], f[3], ssid
                    );
                }
            }
            0
        }
        Err(e) => {
            eprintln!("wifi: scan failed: {}", e);
            if field(&status(iface), "state") == Some("no-firmware") {
                eprintln!(
                    "wifi: {}",
                    field(&status(iface), "msg").unwrap_or("firmware missing")
                );
            }
            1
        }
    }
}

/// After joining a network: wait for an IPv4 lease, then look for a
/// captive portal (hotspots in hotels, trains, cafés). Prints a hint when
/// one is found; with `quiet`, prints nothing otherwise.
fn after_connect(iface: &str, quiet: bool) {
    let start = rustos_rt::time::millis();
    while rustos_rt::time::millis() - start < 15_000 {
        if rustos_rt::net::interface(iface).is_some_and(|i| i.ipv4().is_some()) {
            break;
        }
        rustos_rt::time::sleep_ms(250);
    }
    let st = webclient::portal::check(5000);
    match st {
        webclient::portal::Status::Online if quiet => {}
        _ => println!("wifi: {}", webclient::portal::describe(&st)),
    }
}

/// Start connecting and wait for the outcome.
fn connect(
    iface: &str,
    ssid: &str,
    pass: &str,
    eap: Option<&wpa::Eap>,
    timeout_ms: u64,
    quiet: bool,
) -> bool {
    let started = if native(iface) {
        if eap.is_some() {
            Err(String::from("802.1X (--eap) needs a Linux driver (wpa_supplicant)"))
        } else {
            let mut s = ssid.as_bytes().to_vec();
            request(iface, WIFI_CONNECT, &mut s, pass.as_bytes()).map_err(|e| format!("{}", e))
        }
    } else {
        wpa::connect(iface, ssid, pass, eap).map(|_| 0)
    };
    if let Err(e) = started {
        if !quiet {
            eprintln!("wifi: connect: {}", e);
        }
        return false;
    }
    let start = rustos_rt::time::millis();
    // Let the request reach the driver before polling.
    rustos_rt::time::sleep_ms(300);
    while rustos_rt::time::millis() - start < timeout_ms {
        let st = status(iface);
        match field(&st, "state") {
            Some("connected") => {
                if !quiet {
                    println!("wifi: connected to \"{}\"", ssid);
                }
                return true;
            }
            Some("failed") => {
                if !quiet {
                    eprintln!("wifi: {}", field(&st, "msg").unwrap_or("connection failed"));
                }
                return false;
            }
            _ => {
                if let Some(why) = wpa::failure(iface) {
                    if !quiet {
                        eprintln!("wifi: {}", why);
                    }
                    let _ = wpa::disconnect(iface);
                    return false;
                }
                rustos_rt::time::sleep_ms(250)
            }
        }
    }
    if !quiet {
        eprintln!("wifi: timed out connecting to \"{}\"", ssid);
    }
    false
}

/// Saved networks in file order.
fn saved() -> Vec<(String, String)> {
    let text = rustos_rt::fs::read_to_string(CONF_PERSISTENT)
        .or_else(|_| rustos_rt::fs::read_to_string(CONF))
        .unwrap_or_default();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(v) = line.strip_prefix("ssid=") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            cur = Some((unquote(v), String::new()));
        } else if let Some(v) = line.strip_prefix("psk=")
            && let Some(c) = cur.as_mut()
        {
            c.1 = unquote(v);
        }
    }
    out.extend(cur);
    out
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(v)
        .into()
}

fn write_saved(list: &[(String, String)]) -> i32 {
    let mut s = String::from("# Saved wireless networks (managed by `wifi`).\n");
    for (ssid, psk) in list {
        s.push_str(&format!("\nssid=\"{}\"\n", ssid));
        if !psk.is_empty() {
            s.push_str(&format!("psk=\"{}\"\n", psk));
        }
    }
    let path = if rustos_rt::fs::is_dir("/storage") {
        let _ = rustos_rt::fs::create_dir_all("/storage/etc");
        CONF_PERSISTENT
    } else {
        CONF
    };
    match rustos_rt::fs::write(path, s.as_bytes()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("wifi: {}: {}", path, e);
            1
        }
    }
}

fn save(ssid: &str, pass: &str) -> i32 {
    let mut list = saved();
    list.retain(|(s, _)| s != ssid);
    list.insert(0, (ssid.into(), pass.into()));
    write_saved(&list)
}

pub fn wifi(args: &[String]) -> i32 {
    let mut iface: Option<String> = None;
    let mut rest: Vec<&str> = Vec::new();
    let mut save_it = false;
    let mut quiet = false;
    let mut eap_method: Option<String> = None;
    let mut identity: Option<String> = None;
    let mut anonymous: Option<String> = None;
    let mut ca: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-i" => {
                iface = args.get(i + 1).cloned();
                i += 1;
            }
            "--eap" => {
                eap_method = args.get(i + 1).cloned();
                i += 1;
            }
            "--identity" => {
                identity = args.get(i + 1).cloned();
                i += 1;
            }
            "--anonymous" => {
                anonymous = args.get(i + 1).cloned();
                i += 1;
            }
            "--ca" => {
                ca = args.get(i + 1).cloned();
                i += 1;
            }
            "--save" | "-s" => save_it = true,
            "-q" => quiet = true,
            a => rest.push(a),
        }
        i += 1;
    }
    let cmd = rest.first().copied().unwrap_or("status");
    if cmd == "forget" {
        let Some(ssid) = rest.get(1) else {
            eprintln!("usage: wifi forget SSID");
            return 2;
        };
        let mut list = saved();
        list.retain(|(s, _)| s != ssid);
        return write_saved(&list);
    }
    let Some(iface) = iface.or_else(default_iface) else {
        if !quiet {
            eprintln!("wifi: no wireless interface");
        }
        return 1;
    };
    match cmd {
        "status" => {
            print_status(&iface);
            0
        }
        "scan" => scan(&iface),
        "connect" => {
            let Some(ssid) = rest.get(1) else {
                eprintln!("usage: wifi connect SSID [PASSPHRASE] [--save]");
                return 2;
            };
            let pass = rest.get(2).copied().unwrap_or("");
            let eap = match (&eap_method, &identity) {
                (Some(m), Some(id)) => Some(wpa::Eap {
                    method: m,
                    identity: id,
                    anonymous: anonymous.as_deref(),
                    ca_cert: ca.as_deref(),
                }),
                (Some(_), None) => {
                    eprintln!("wifi: --eap needs --identity");
                    return 2;
                }
                _ => None,
            };
            if eap.is_none() && !pass.is_empty() && !(8..=63).contains(&pass.len()) {
                eprintln!("wifi: passphrase must be 8-63 characters");
                return 2;
            }
            if !connect(&iface, ssid, pass, eap.as_ref(), 30_000, false) {
                return 1;
            }
            let rc = if save_it { save(ssid, pass) } else { 0 };
            if !rest.contains(&"--no-portal-check") {
                after_connect(&iface, false);
            }
            rc
        }
        "power" => match rest.get(1).copied() {
            None | Some("status") => {
                let s = status(&iface);
                println!("{}", field(&s, "power").unwrap_or("unknown"));
                0
            }
            Some("on" | "off" | "auto") if !native(&iface) => {
                eprintln!("wifi: power save control is not available for this driver yet");
                1
            }
            Some(m @ ("on" | "off" | "auto")) => {
                let mut b = m.as_bytes().to_vec();
                match request(&iface, WIFI_POWER, &mut b, &[]) {
                    Ok(_) => 0,
                    Err(e) => {
                        eprintln!("wifi: {}", e);
                        1
                    }
                }
            }
            _ => {
                eprintln!("usage: wifi power [on|off|auto|status]");
                2
            }
        },
        "disconnect" if !native(&iface) => match wpa::disconnect(&iface) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("wifi: {}", e);
                1
            }
        },
        "disconnect" => match request(&iface, WIFI_DISCONNECT, &mut [], &[]) {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("wifi: {}", e);
                1
            }
        },
        "auto" => {
            let list = saved();
            if list.is_empty() {
                return 0;
            }
            // Wait for the driver to load its firmware.
            let start = rustos_rt::time::millis();
            while rustos_rt::time::millis() - start < 20_000 {
                match field(&status(&iface), "state") {
                    Some("starting") | None => rustos_rt::time::sleep_ms(500),
                    _ => break,
                }
            }
            for (ssid, psk) in &list {
                if !quiet {
                    println!("wifi: trying \"{}\"", ssid);
                }
                if connect(&iface, ssid, psk, None, 25_000, quiet) {
                    if quiet {
                        println!("wifi: connected to \"{}\"", ssid);
                    }
                    after_connect(&iface, quiet);
                    return 0;
                }
            }
            1
        }
        _ => {
            eprintln!(
                "usage: wifi [status|scan|connect SSID [PASS] [--save] [--eap M --identity ID]|disconnect|auto|forget SSID|power [on|off|auto]] [-i IFACE]"
            );
            2
        }
    }
}
