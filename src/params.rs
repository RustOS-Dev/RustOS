//! Boot-time kernel parameters.
//!
//! RustOS has no bootloader command line, so parameters come from a file:
//! `/storage/etc/kernel.conf` (persistent, edited on the machine) or else
//! `/etc/kernel.conf` from the initramfs, then, in QEMU, the fw_cfg file
//! `opt/rustos/kernel.conf` on top (the test harness passes switches that
//! way). Each line holds `key=value` pairs (or a bare `key`, meaning `1`)
//! separated by spaces or `;` (for one-line fw_cfg strings); `#` starts a
//! comment. The file is read once storage is mounted, so the switches only
//! affect code that runs afterwards (drivers keep checking them at run
//! time). `/proc/cmdline` shows the active set.
//!
//! Known keys:
//! * `iwlwifi.debug=1` — log every Wi-Fi host command and notification.
//! * `iwlwifi.mode=legacy|ht|vht|he` — highest 802.11 mode to use.
//! * `iwlwifi.width=20|40|80|160` — widest channel to use (MHz).
//! * `iwlwifi.agg=0` — no A-MPDU aggregation (block ack).
//! * `net.debug=1` — log a one-line summary of every frame sent/received.
//! * `log.persist=1` — mirror the kernel log to `/storage/log/kernel.log`.
//! * `desktop=none` — no eDEX-DE session on the first console at boot
//!   (read by `/etc/rc` through `/proc/cmdline`; `desktop.login=1` asks
//!   for a login first, `desktop.user=NAME` picks the account).

use crate::sync::RwLock;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use core::sync::atomic::{AtomicBool, Ordering};

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
        for tok in line
            .split(|c: char| c.is_whitespace() || c == ';')
            .filter(|t| !t.is_empty())
        {
            let (k, v) = tok.split_once('=').unwrap_or((tok, "1"));
            if !k.is_empty() {
                out.insert(k.to_string(), v.to_string());
            }
        }
    }
    out
}

pub const FW_CFG_FILE: &str = "opt/rustos/kernel.conf";

/// Load the first parameter file that exists, then the fw_cfg file over it.
/// Returns where the parameters came from.
pub fn load() -> Option<String> {
    let mut map = BTreeMap::new();
    let mut from = FILES.iter().find_map(|&path| {
        let data = crate::vfs::read_all(path).ok()?;
        map = parse(&String::from_utf8_lossy(&data));
        Some(path.to_string())
    });
    if let Some(data) = crate::arch::x86_64::fwcfg::read_file(FW_CFG_FILE) {
        map.extend(parse(&String::from_utf8_lossy(&data)));
        let fw = alloc::format!("fw_cfg {FW_CFG_FILE}");
        from = Some(match from {
            Some(f) => alloc::format!("{f} and {fw}"),
            None => fw,
        });
    }
    from.as_ref()?;
    *PARAMS.write() = map;
    refresh();
    from
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
