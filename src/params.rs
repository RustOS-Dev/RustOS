//! Boot-time kernel parameters.
//!
//! RustOS has no bootloader command line, so parameters come from a file:
//! `/storage/etc/kernel.conf` (persistent, edited on the machine) or else
//! `/etc/kernel.conf` from the initramfs. Each line holds `key=value`
//! pairs (or a bare `key`, meaning `1`) separated by spaces; `#` starts a
//! comment. The file is read once storage is mounted, so the switches only
//! affect code that runs afterwards (drivers keep checking them at run
//! time). `/proc/cmdline` shows the active set.
//!
//! Known keys:
//! * `iwlwifi.debug=1` — log every Wi-Fi host command and notification.
//! * `net.debug=1` — log a one-line summary of every frame sent/received.
//! * `log.persist=1` — mirror the kernel log to `/storage/log/kernel.log`.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::RwLock;

static PARAMS: RwLock<BTreeMap<String, String>> = RwLock::new(BTreeMap::new());

/// Fast-path copies of frequently tested switches.
pub static IWL_DEBUG: AtomicBool = AtomicBool::new(false);
pub static NET_DEBUG: AtomicBool = AtomicBool::new(false);

pub const FILES: [&str; 2] = ["/storage/etc/kernel.conf", "/etc/kernel.conf"];

/// Parse `key=value` tokens from a configuration text.
pub fn parse(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        for tok in line.split_whitespace() {
            let (k, v) = tok.split_once('=').unwrap_or((tok, "1"));
            if !k.is_empty() {
                out.insert(k.to_string(), v.to_string());
            }
        }
    }
    out
}

/// Load the first parameter file that exists. Returns its path.
pub fn load() -> Option<&'static str> {
    for path in FILES {
        if let Ok(data) = crate::vfs::read_all(path) {
            let map = parse(&String::from_utf8_lossy(&data));
            *PARAMS.write() = map;
            refresh();
            return Some(path);
        }
    }
    None
}

fn refresh() {
    IWL_DEBUG.store(flag("iwlwifi.debug"), Ordering::Relaxed);
    NET_DEBUG.store(flag("net.debug"), Ordering::Relaxed);
}

pub fn get(key: &str) -> Option<String> {
    PARAMS.read().get(key).cloned()
}

/// True if `key` is set to anything but `0`/`no`/`off`/`false`.
pub fn flag(key: &str) -> bool {
    get(key).is_some_and(|v| !matches!(v.as_str(), "0" | "no" | "off" | "false"))
}

/// Space-separated `key=value` list (for `/proc/cmdline`).
pub fn cmdline() -> String {
    let mut s = String::new();
    for (k, v) in PARAMS.read().iter() {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(k);
        s.push('=');
        s.push_str(v);
    }
    s
}
