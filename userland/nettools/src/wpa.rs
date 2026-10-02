//! wpa_supplicant control-interface client: how `wifi` drives interfaces
//! of Linux drivers (LinuxKPI), whose 802.11 security runs in
//! wpa_supplicant. One wpa_supplicant per interface is started on demand
//! with a minimal configuration; networks are added through the control
//! socket, never written to its configuration file.

use rustos_rt::net;
use rustos_rt::prelude::*;

const CTRL_DIR: &str = "/var/run/wpa_supplicant";

/// A request socket to one interface's wpa_supplicant.
pub struct Ctrl {
    sock: net::Socket,
    local: String,
}

impl Drop for Ctrl {
    fn drop(&mut self) {
        let _ = rustos_rt::fs::remove_file(&self.local);
    }
}

impl Ctrl {
    pub fn open(iface: &str) -> rustos_rt::Result<Ctrl> {
        let sock = net::Socket::new(net::AF_UNIX, net::SOCK_DGRAM, 0)?;
        let local = format!(
            "/tmp/wifi-{}-{}",
            rustos_rt::process::getpid(),
            rustos_rt::time::millis() % 100_000
        );
        let _ = rustos_rt::fs::remove_file(&local);
        sock.bind_raw(&net::unix_addr(&local))?;
        let c = Ctrl { sock, local };
        c.sock
            .connect_raw(&net::unix_addr(&format!("{}/{}", CTRL_DIR, iface)))?;
        c.sock.set_timeout(5_000)?;
        Ok(c)
    }

    /// Send a command and return its reply.
    pub fn request(&self, cmd: &str) -> rustos_rt::Result<String> {
        self.sock.send(cmd.as_bytes())?;
        let mut buf = vec![0u8; 16384];
        loop {
            let n = self.sock.recv(&mut buf)?;
            let reply = String::from_utf8_lossy(&buf[..n]).into_owned();
            // Unsolicited events ("<3>CTRL-EVENT-...") only reach attached
            // monitors, but skip any that slip through.
            if !reply.starts_with('<') {
                return Ok(reply);
            }
        }
    }

    /// Wait (attached) for an event whose text contains `name`.
    pub fn wait_event(&self, name: &str, timeout_ms: u64) -> bool {
        let start = rustos_rt::time::millis();
        let mut buf = vec![0u8; 4096];
        while rustos_rt::time::millis() - start < timeout_ms {
            match self.sock.recv(&mut buf) {
                Ok(n) if String::from_utf8_lossy(&buf[..n]).contains(name) => return true,
                Ok(_) => {}
                Err(_) => {}
            }
        }
        false
    }

    /// A command whose reply must be "OK".
    pub fn ok(&self, cmd: &str) -> Result<(), String> {
        match self.request(cmd) {
            Ok(r) if r.trim() == "OK" => Ok(()),
            Ok(r) => Err(format!("{}: {}", cmd.split(' ').next().unwrap_or(cmd), r.trim())),
            Err(e) => Err(format!("{}", e)),
        }
    }
}

/// Connect to the interface's wpa_supplicant, starting one if none runs.
pub fn ensure(iface: &str) -> Result<Ctrl, String> {
    if let Ok(c) = Ctrl::open(iface)
        && c.request("PING").is_ok_and(|r| r.trim() == "PONG")
    {
        return Ok(c);
    }
    let _ = rustos_rt::fs::create_dir_all("/var/run");
    let _ = rustos_rt::fs::create_dir_all("/var/log");
    let conf = format!("/var/run/wpa_supplicant-{}.conf", iface);
    let text = format!(
        "# Written by `wifi`; networks are added at run time.\nctrl_interface={}\npmf=1\nsae_pwe=2\n",
        CTRL_DIR
    );
    rustos_rt::fs::write(&conf, text.as_bytes()).map_err(|e| format!("{}: {}", conf, e))?;
    let log = format!("/var/log/wpa_supplicant-{}.log", iface);
    match rustos_rt::process::run(&[
        "wpa_supplicant",
        "-B",
        "-i",
        iface,
        "-c",
        &conf,
        "-f",
        &log,
    ]) {
        Ok(0) => {}
        Ok(rc) => return Err(format!("wpa_supplicant exited with {} (see {})", rc, log)),
        Err(e) => return Err(format!("cannot start wpa_supplicant: {}", e)),
    }
    let start = rustos_rt::time::millis();
    while rustos_rt::time::millis() - start < 10_000 {
        if let Ok(c) = Ctrl::open(iface)
            && c.request("PING").is_ok_and(|r| r.trim() == "PONG")
        {
            return Ok(c);
        }
        rustos_rt::time::sleep_ms(200);
    }
    Err(format!("wpa_supplicant did not start (see {})", log))
}

fn kv<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

/// 802.11 channel of a frequency in MHz.
pub fn channel(freq: u32) -> u32 {
    match freq {
        2484 => 14,
        2412..=2472 => (freq - 2407) / 5,
        5955..=7115 => (freq - 5950) / 5,
        5000..=5925 => (freq - 5000) / 5,
        _ => 0,
    }
}

/// Security from scan-result flags ("[WPA2-PSK-CCMP][ESS]").
fn security(flags: &str) -> &'static str {
    let psk = flags.contains("-PSK");
    let sae = flags.contains("SAE");
    if flags.contains("-EAP") {
        "WPA2-EAP"
    } else if psk && sae {
        "WPA2/WPA3"
    } else if sae {
        "WPA3"
    } else if flags.contains("WPA2") || flags.contains("RSN") {
        "WPA2"
    } else if flags.contains("WPA") {
        "WPA"
    } else if flags.contains("OWE") {
        "OWE"
    } else if flags.contains("WEP") {
        "WEP"
    } else {
        "open"
    }
}

/// Status in the native drivers' format (`state=... ssid="..." ...`).
pub fn status(iface: &str) -> String {
    let Ok(c) = Ctrl::open(iface) else {
        return String::from("state=disconnected msg=\"wpa_supplicant not running\"");
    };
    let Ok(st) = c.request("STATUS") else {
        return String::new();
    };
    let wpa = kv(&st, "wpa_state").unwrap_or("UNKNOWN");
    let state = match wpa {
        "COMPLETED" => "connected",
        "SCANNING" | "AUTHENTICATING" | "ASSOCIATING" | "ASSOCIATED" | "4WAY_HANDSHAKE"
        | "GROUP_HANDSHAKE" => "connecting",
        "INTERFACE_DISABLED" => "down",
        _ => "disconnected",
    };
    let mut out = format!("state={} mode=station", state);
    if let Some(s) = kv(&st, "ssid") {
        out.push_str(&format!(" ssid=\"{}\"", s));
    }
    if let Some(b) = kv(&st, "bssid") {
        out.push_str(&format!(" bssid={}", b));
    }
    if let Some(f) = kv(&st, "freq").and_then(|f| f.parse().ok()) {
        out.push_str(&format!(" channel={}", channel(f)));
    }
    if let Some(k) = kv(&st, "key_mgmt") {
        // "WPA2/IEEE 802.1X/EAP" -> WPA2-EAP; WPA2-PSK, SAE, OWE, NONE as is.
        let k = if k.contains("802.1X") {
            format!("{}-EAP", k.split('/').next().unwrap_or("WPA2"))
        } else {
            k.replace(' ', "-")
        };
        out.push_str(&format!(" security={}", k));
    }
    if state == "connected"
        && let Ok(sig) = c.request("SIGNAL_POLL")
    {
        if let Some(r) = kv(&sig, "RSSI") {
            out.push_str(&format!(" signal={}", r));
        }
        if let Some(r) = kv(&sig, "LINKSPEED") {
            out.push_str(&format!(" rate={}Mbit/s", r));
        }
    }
    out
}

/// Scan and return results as "bssid\tchannel\tsignal\tsecurity\tssid"
/// lines.
pub fn scan(iface: &str) -> Result<String, String> {
    let c = ensure(iface)?;
    // Hear the scan finish: a full sweep (2.4, 5, 6 GHz) takes seconds.
    let events = Ctrl::open(iface).map_err(|e| format!("{}", e))?;
    events.ok("ATTACH")?;
    // FAIL-BUSY: a scan is already running; its results serve.
    match c.request("SCAN").map_err(|e| format!("{}", e))?.trim() {
        "OK" | "FAIL-BUSY" => {}
        r => return Err(format!("scan: {}", r)),
    }
    events.wait_event("CTRL-EVENT-SCAN-RESULTS", 30_000);
    let _ = events.request("DETACH");
    let res = c.request("SCAN_RESULTS").map_err(|e| format!("{}", e))?;
    let mut out = String::new();
    for l in res.lines().skip(1) {
        let f: Vec<&str> = l.splitn(5, '\t').collect();
        if f.len() == 5 {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                f[0],
                channel(f[1].parse().unwrap_or(0)),
                f[2],
                security(f[3]),
                f[4]
            ));
        }
    }
    Ok(out)
}

/// Enterprise (802.1X) credentials.
pub struct Eap<'a> {
    pub method: &'a str,
    pub identity: &'a str,
    pub anonymous: Option<&'a str>,
    pub ca_cert: Option<&'a str>,
}

/// Quote a value for SET_NETWORK.
fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('"', ""))
}

/// Replace the configured network with `ssid` and select it.
pub fn connect(iface: &str, ssid: &str, pass: &str, eap: Option<&Eap>) -> Result<(), String> {
    let c = ensure(iface)?;
    c.ok("REMOVE_NETWORK all")?;
    let id = c
        .request("ADD_NETWORK")
        .map_err(|e| format!("{}", e))?
        .trim()
        .to_string();
    if id.parse::<u32>().is_err() {
        return Err(format!("ADD_NETWORK: {}", id));
    }
    let set = |k: &str, v: &str| c.ok(&format!("SET_NETWORK {} {} {}", id, k, v));
    set("ssid", &q(ssid))?;
    set("scan_ssid", "1")?;
    match eap {
        Some(e) => {
            set("key_mgmt", "WPA-EAP WPA-EAP-SHA256")?;
            set("eap", &e.method.to_ascii_uppercase())?;
            set("identity", &q(e.identity))?;
            set("password", &q(pass))?;
            if let Some(a) = e.anonymous {
                set("anonymous_identity", &q(a))?;
            }
            if let Some(ca) = e.ca_cert {
                set("ca_cert", &q(ca))?;
            }
            if matches!(e.method.to_ascii_lowercase().as_str(), "peap" | "ttls") {
                set("phase2", &q("auth=MSCHAPV2"))?;
            }
            set("ieee80211w", "1")?;
        }
        None if pass.is_empty() => {
            set("key_mgmt", "NONE OWE")?;
        }
        None => {
            // WPA2 and WPA3 (SAE) both, with optional PMF: the AP picks.
            set("key_mgmt", "WPA-PSK WPA-PSK-SHA256 SAE")?;
            set("psk", &q(pass))?;
            set("sae_password", &q(pass))?;
            set("ieee80211w", "1")?;
        }
    }
    c.ok(&format!("SELECT_NETWORK {}", id))?;
    c.ok("ENABLE_NETWORK all")?;
    Ok(())
}

/// Why the current network is not connecting, if wpa_supplicant gave up
/// on it for now.
pub fn failure(iface: &str) -> Option<String> {
    let c = Ctrl::open(iface).ok()?;
    let list = c.request("LIST_NETWORKS").ok()?;
    list.lines()
        .skip(1)
        .find(|l| l.contains("[TEMP-DISABLED]"))
        .map(|_| String::from("authentication failed (wrong password?)"))
}

pub fn disconnect(iface: &str) -> Result<(), String> {
    let c = Ctrl::open(iface).map_err(|e| format!("{}", e))?;
    c.ok("DISCONNECT")
}
