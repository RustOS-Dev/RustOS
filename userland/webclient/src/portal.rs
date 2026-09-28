//! Captive-portal detection.
//!
//! A network with a captive portal intercepts web traffic until the user
//! logs in. We learn about it in two ways:
//! * the network announces the portal URI (DHCPv4 option 114, DHCPv6
//!   option 103 or the router-advertisement option, RFC 8910), which the
//!   kernel publishes in `/proc/net/captive_portal`;
//! * a probe: fetch a URL known to return `204 No Content`
//!   (configurable in `/etc/portal.conf`); a redirect or any other answer
//!   means something intercepted the request.
//!
//! The detected login page is written to `/run/portal`, which
//! `browse --portal` opens.

use crate::{Client, Net};
use alloc::string::{String, ToString};
use rustos_rt::fs;

pub const DEFAULT_PROBE: &str = "http://connectivitycheck.gstatic.com/generate_204";
pub const STATE_FILE: &str = "/run/portal";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The probe got the expected answer: full Internet access.
    Online,
    /// Traffic is intercepted; the login page (if known).
    Portal(Option<String>),
    /// No working connection (error message).
    Offline(String),
}

/// Probe URL and expected status code from `/etc/portal.conf`
/// (`url=...` and `expect=...` lines).
pub fn probe_config() -> (String, u16) {
    let mut url = String::from(DEFAULT_PROBE);
    let mut expect = 204;
    for path in ["/storage/etc/portal.conf", "/etc/portal.conf"] {
        if let Ok(t) = fs::read_to_string(path) {
            for line in t.lines() {
                let line = line.trim();
                if let Some(v) = line.strip_prefix("url=") {
                    url = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("expect=") {
                    expect = v.trim().parse().unwrap_or(204);
                }
            }
            break;
        }
    }
    (url, expect)
}

/// Portal URI announced by the network: (interface, source, URI).
pub fn announced() -> Option<(String, String, String)> {
    let t = fs::read_to_string("/proc/net/captive_portal").ok()?;
    let line = t.lines().next()?;
    let mut w = line.splitn(3, ' ');
    Some((
        w.next()?.to_string(),
        w.next()?.to_string(),
        w.next()?.to_string(),
    ))
}

fn record(status: &Status) {
    match status {
        Status::Portal(Some(url)) => {
            let _ = fs::create_dir_all("/run");
            let _ = fs::write(STATE_FILE, url.as_bytes());
        }
        Status::Online => {
            let _ = fs::remove_file(STATE_FILE);
        }
        _ => {}
    }
}

/// Run the check (at most `timeout_ms` per connection attempt).
pub fn check(timeout_ms: u64) -> Status {
    let (probe, expect) = probe_config();
    let mut net = Net::new();
    net.timeout_ms = timeout_ms;
    let mut c = Client::new(net);
    c.opts.follow_redirects = false;
    c.opts.cookies = false;
    c.opts.keep_alive = false;
    let status = match httpc::Url::parse(&probe)
        .map_err(httpc::Error::from)
        .and_then(|u| c.get(u))
    {
        Ok(r) if r.status() == expect && (expect != 204 || r.body.is_empty()) => Status::Online,
        Ok(r) => {
            let loc = r
                .head
                .headers
                .get("Location")
                .and_then(|l| r.url.join(l).ok())
                .map(|u| u.to_string());
            let announced = announced().map(|a| a.2);
            Status::Portal(announced.or(loc).or(Some(r.url.to_string())))
        }
        Err(e) => match announced() {
            Some((_, _, url)) => Status::Portal(Some(url)),
            None => Status::Offline(e.to_string()),
        },
    };
    record(&status);
    status
}

/// One-line human description.
pub fn describe(s: &Status) -> String {
    match s {
        Status::Online => String::from("Internet access OK"),
        Status::Portal(Some(u)) => alloc::format!(
            "captive portal detected: {} (log in with 'browse --portal')",
            u
        ),
        Status::Portal(None) => {
            String::from("captive portal detected (log in with 'browse --portal')")
        }
        Status::Offline(e) => alloc::format!("no Internet access: {}", e),
    }
}
