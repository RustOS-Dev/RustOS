//! Bonding keys, kept in `/storage/etc/bluetooth/keys` (or
//! `/etc/bluetooth/keys` without a storage partition), one device per
//! line:
//!
//! ```text
//! local irk=<32 hex>
//! AA:BB:CC:DD:EE:FF le random ltk=.. ediv=.. rand=.. size=16 auth=1 sc=1 irk=.. name=Keyboard
//! 11:22:33:44:55:66 bredr public linkkey=.. keytype=5 name=Mouse
//! ```

use crate::sync::Mutex;
use alloc::string::String;
use alloc::vec::Vec;
use bt::{Addr, AddrType, crypto};
use core::fmt::Write;

/// A bonded device.
#[derive(Clone, Debug, Default)]
pub struct BondEntry {
    pub addr: Addr,
    pub addr_type: AddrType,
    pub le: bool,
    pub name: String,
    pub ltk: Option<[u8; 16]>,
    pub ediv: u16,
    pub rand: [u8; 8],
    pub key_size: u8,
    pub authenticated: bool,
    pub secure: bool,
    pub irk: Option<[u8; 16]>,
    pub link_key: Option<[u8; 16]>,
    pub link_key_type: u8,
}

struct Store {
    loaded: bool,
    local_irk: Option<[u8; 16]>,
    bonds: Vec<BondEntry>,
}

static STORE: Mutex<Store> = Mutex::new(Store {
    loaded: false,
    local_irk: None,
    bonds: Vec::new(),
});

fn path() -> &'static str {
    if crate::vfs::exists("/storage/etc") || crate::vfs::exists("/storage") {
        "/storage/etc/bluetooth/keys"
    } else {
        "/etc/bluetooth/keys"
    }
}

fn hex(b: &[u8]) -> String {
    let mut s = String::new();
    for x in b {
        let _ = write!(s, "{:02x}", x);
    }
    s
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N {
        return None;
    }
    let mut a = [0u8; N];
    for (i, b) in a.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(a)
}

fn parse_line(l: &str) -> Option<BondEntry> {
    let mut it = l.split_whitespace();
    let addr = Addr::parse(it.next()?)?;
    let le = it.next()? == "le";
    let addr_type = if it.next()? == "random" {
        AddrType::Random
    } else {
        AddrType::Public
    };
    let mut b = BondEntry {
        addr,
        addr_type,
        le,
        key_size: 16,
        ..Default::default()
    };
    for kv in it {
        let (k, v) = kv.split_once('=')?;
        match k {
            "ltk" => b.ltk = unhex(v),
            "ediv" => b.ediv = v.parse().unwrap_or(0),
            "rand" => b.rand = unhex(v).unwrap_or([0; 8]),
            "size" => b.key_size = v.parse().unwrap_or(16),
            "auth" => b.authenticated = v == "1",
            "sc" => b.secure = v == "1",
            "irk" => b.irk = unhex(v),
            "linkkey" => b.link_key = unhex(v),
            "keytype" => b.link_key_type = v.parse().unwrap_or(0),
            // Names are stored with '_' for spaces.
            "name" => b.name = v.replace('_', " "),
            _ => {}
        }
    }
    Some(b)
}

/// Read the keys file (once; later calls are no-ops).
pub fn load() {
    let mut s = STORE.lock();
    if s.loaded {
        return;
    }
    s.loaded = true;
    let Ok(data) = crate::vfs::read_all(path()) else {
        return;
    };
    for l in String::from_utf8_lossy(&data).lines() {
        let l = l.trim();
        if let Some(v) = l.strip_prefix("local irk=") {
            s.local_irk = unhex(v);
        } else if !l.is_empty()
            && !l.starts_with('#')
            && let Some(b) = parse_line(l)
        {
            s.bonds.push(b);
        }
    }
    if !s.bonds.is_empty() {
        crate::println!("[bt] {} bonded device(s) from {}", s.bonds.len(), path());
    }
}

fn save(s: &Store) {
    let mut out = String::from("# Bluetooth bonding keys (written by the kernel)\n");
    if let Some(k) = s.local_irk {
        let _ = writeln!(out, "local irk={}", hex(&k));
    }
    for b in &s.bonds {
        let _ = write!(
            out,
            "{} {} {}",
            b.addr,
            if b.le { "le" } else { "bredr" },
            if b.addr_type == AddrType::Random {
                "random"
            } else {
                "public"
            }
        );
        if let Some(k) = b.ltk {
            let _ = write!(
                out,
                " ltk={} ediv={} rand={} size={} auth={} sc={}",
                hex(&k),
                b.ediv,
                hex(&b.rand),
                b.key_size,
                b.authenticated as u8,
                b.secure as u8
            );
        }
        if let Some(k) = b.irk {
            let _ = write!(out, " irk={}", hex(&k));
        }
        if let Some(k) = b.link_key {
            let _ = write!(out, " linkkey={} keytype={}", hex(&k), b.link_key_type);
        }
        if !b.name.is_empty() {
            let _ = write!(out, " name={}", b.name.replace(char::is_whitespace, "_"));
        }
        out.push('\n');
    }
    let p = path();
    let dir = &p[..p.rfind('/').unwrap_or(0)];
    let _ = crate::vfs::mkdir_p(dir);
    if let Err(e) = crate::vfs::write_all(p, out.as_bytes()) {
        crate::println!("[bt] cannot save {}: {:?}", p, e);
    }
}

/// Our identity resolving key (made on first use).
pub fn local_irk() -> [u8; 16] {
    load();
    let mut s = STORE.lock();
    if let Some(k) = s.local_irk {
        return k;
    }
    let mut k = [0u8; 16];
    crate::drivers::random::fill(&mut k);
    s.local_irk = Some(k);
    save(&s);
    k
}

pub fn all() -> Vec<BondEntry> {
    STORE.lock().bonds.clone()
}

pub fn get(a: &Addr) -> Option<BondEntry> {
    STORE.lock().bonds.iter().find(|b| b.addr == *a).cloned()
}

/// The bond for an address as seen on the air: the identity address, or
/// a resolvable private address made with a bonded device's IRK.
pub fn resolve(a: &Addr) -> Option<BondEntry> {
    let s = STORE.lock();
    s.bonds
        .iter()
        .find(|b| b.addr == *a || b.irk.is_some_and(|k| crypto::rpa_matches(&k, a)))
        .cloned()
}

/// Add or replace a bond and save the file.
pub fn put(b: BondEntry) {
    load();
    let mut s = STORE.lock();
    s.bonds.retain(|x| x.addr != b.addr);
    s.bonds.push(b);
    save(&s);
}

/// Forget a device; true if it was bonded.
pub fn remove(a: &Addr) -> bool {
    let mut s = STORE.lock();
    let n = s.bonds.len();
    s.bonds.retain(|x| x.addr != *a);
    let removed = s.bonds.len() != n;
    if removed {
        save(&s);
    }
    removed
}
