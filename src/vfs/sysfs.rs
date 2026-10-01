//! /sys: a view of devices. Most nodes are generated on lookup; drivers
//! can also register directories, attribute files and symlinks (LinuxKPI
//! publishes the Linux device model here), which appear alongside.
//!
//! * `class/net/<iface>/` — address, operstate, carrier, mtu, speed, type,
//!   ifindex, driver, wireless status and `statistics/*` counters.
//! * `class/block/<disk>/` (also `block/`) — size (512-byte units), ro,
//!   removable-style metadata, model and partition number.
//! * `devices/system/cpu/` — online / possible / present CPU ranges.

use super::*;
use crate::block::BlockDevice;
use alloc::format;

const FS_ID: usize = 0x5159;

pub struct SysFs;

impl SysFs {
    pub fn new() -> Arc<SysFs> {
        Arc::new(SysFs)
    }
}

impl FileSystem for SysFs {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(SysNode(String::new()))
    }
    fn name(&self) -> &'static str {
        "sysfs"
    }
    fn read_only(&self) -> bool {
        // Registered attribute files may be writable; everything else
        // refuses writes itself.
        false
    }
}

/// What lives at a path.
enum Node {
    Dir(Vec<(String, FileType)>),
    File(String),
    Attr(Arc<dyn Attr>),
    Link(String),
}

// ------------------------------------------------------------ registry

/// A registered attribute file: read with `show`, written with `store`.
pub trait Attr: Send + Sync {
    fn show(&self) -> KResult<Vec<u8>>;
    fn store(&self, _data: &[u8]) -> KResult<usize> {
        Err(EACCES)
    }
    /// Permission bits.
    fn mode(&self) -> u32 {
        0o444
    }
    /// Called once the file is unregistered: returns when no show/store
    /// call is running any more and none will start.
    fn drain(&self) {}
}

enum Reg {
    Dir,
    File(Arc<dyn Attr>),
    Link(String),
}

/// Registered nodes by path relative to /sys ("devices/platform").
static REG: crate::sync::RwLock<alloc::collections::BTreeMap<String, Reg>> =
    crate::sync::RwLock::new(alloc::collections::BTreeMap::new());

fn reg_key(path: &str) -> String {
    path.trim_matches('/').into()
}

fn reg_insert(path: &str, r: Reg) -> KResult<()> {
    let key = reg_key(path);
    let mut reg = REG.write();
    if key.is_empty() || reg.contains_key(&key) {
        return Err(EEXIST);
    }
    reg.insert(key, r);
    Ok(())
}

/// Register a directory.
pub fn add_dir(path: &str) -> KResult<()> {
    reg_insert(path, Reg::Dir)
}

/// Register an attribute file.
pub fn add_file(path: &str, attr: Arc<dyn Attr>) -> KResult<()> {
    reg_insert(path, Reg::File(attr))
}

/// Register a symlink to `target` (an absolute path).
pub fn add_link(path: &str, target: &str) -> KResult<()> {
    reg_insert(path, Reg::Link(target.into()))
}

/// Unregister `path` and everything below it. Attribute files are drained
/// before this returns.
pub fn remove(path: &str) {
    let key = reg_key(path);
    let prefix = format!("{key}/");
    let removed: Vec<Reg> = {
        let mut reg = REG.write();
        let below: Vec<String> = reg
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, _)| k.clone())
            .collect();
        below
            .iter()
            .chain(core::iter::once(&key))
            .filter_map(|k| reg.remove(k))
            .collect()
    };
    for r in removed {
        if let Reg::File(a) = r {
            a.drain();
        }
    }
}

/// Move `old` and everything below it to `new`.
pub fn rename(old: &str, new: &str) -> KResult<()> {
    let (old, new) = (reg_key(old), reg_key(new));
    let prefix = format!("{old}/");
    let mut reg = REG.write();
    if !reg.contains_key(&old) {
        return Err(ENOENT);
    }
    if reg.contains_key(&new) {
        return Err(EEXIST);
    }
    let keys: Vec<String> = core::iter::once(old.clone())
        .chain(
            reg.range(prefix.clone()..)
                .take_while(|(k, _)| k.starts_with(&prefix))
                .map(|(k, _)| k.clone()),
        )
        .collect();
    for k in keys {
        if let Some(v) = reg.remove(&k) {
            reg.insert(format!("{new}{}", &k[old.len()..]), v);
        }
    }
    Ok(())
}

/// Registered entries directly below `key`.
fn reg_children(key: &str) -> Vec<(String, FileType)> {
    let prefix = if key.is_empty() {
        String::new()
    } else {
        format!("{key}/")
    };
    REG.read()
        .range(prefix.clone()..)
        .take_while(|(k, _)| k.starts_with(&prefix))
        .filter(|(k, _)| !k[prefix.len()..].contains('/'))
        .map(|(k, r)| {
            let kind = match r {
                Reg::Dir => FileType::Directory,
                Reg::File(_) => FileType::Regular,
                Reg::Link(_) => FileType::Symlink,
            };
            (k[prefix.len()..].into(), kind)
        })
        .collect()
}

fn reg_node(key: &str) -> Option<Node> {
    match REG.read().get(key)? {
        Reg::Dir => Some(Node::Dir(Vec::new())),
        Reg::File(a) => Some(Node::Attr(a.clone())),
        Reg::Link(t) => Some(Node::Link(t.clone())),
    }
}

fn dir(names: &[&str]) -> Node {
    Node::Dir(
        names
            .iter()
            .map(|n| (String::from(*n), FileType::Directory))
            .collect(),
    )
}

fn files(names: &[&str]) -> Vec<(String, FileType)> {
    names
        .iter()
        .map(|n| (String::from(*n), FileType::Regular))
        .collect()
}

struct NetInfo {
    name: String,
    index: u32,
    mac: [u8; 6],
    mtu: usize,
    up: bool,
    link: bool,
    loopback: bool,
    wireless: bool,
    speed: Option<u32>,
    driver: &'static str,
    status: String,
    stats: crate::net::Stats,
}

fn net_ifaces() -> Vec<NetInfo> {
    crate::net::with(|net| {
        net.ifaces
            .iter()
            .map(|i| {
                let dev = i.device();
                NetInfo {
                    name: i.name.clone(),
                    index: i.index,
                    mac: dev.map_or([0; 6], |d| d.mac()),
                    mtu: dev.map_or(65536, |d| d.mtu()),
                    up: i.up,
                    link: i.link_up(),
                    loopback: i.is_loopback(),
                    wireless: dev.is_some_and(|d| d.kind() == crate::net::IfKind::Wireless),
                    speed: dev.and_then(|d| d.speed()),
                    driver: dev.map_or("loopback", |d| d.driver()),
                    status: dev.map(|d| d.status()).unwrap_or_default(),
                    stats: i.stats,
                }
            })
            .collect()
    })
    .unwrap_or_default()
}

fn cpu_range() -> String {
    let n = crate::arch::x86_64::smp::online();
    if n <= 1 {
        String::from("0\n")
    } else {
        format!("0-{}\n", n - 1)
    }
}

fn net_node(rest: &[&str]) -> Option<Node> {
    let ifaces = net_ifaces();
    let Some(first) = rest.first() else {
        return Some(Node::Dir(
            ifaces
                .iter()
                .map(|i| (i.name.clone(), FileType::Directory))
                .collect(),
        ));
    };
    let i = ifaces.into_iter().find(|i| i.name == *first)?;
    let v = |s: String| Some(Node::File(s + "\n"));
    match rest.get(1..).unwrap_or(&[]) {
        [] => {
            let mut e = files(&[
                "address",
                "addr_len",
                "broadcast",
                "carrier",
                "driver",
                "flags",
                "ifindex",
                "mtu",
                "operstate",
                "speed",
                "type",
            ]);
            e.push((String::from("statistics"), FileType::Directory));
            if i.wireless {
                e.push((String::from("wireless"), FileType::Directory));
            }
            Some(Node::Dir(e))
        }
        ["address"] => {
            let m = i.mac;
            v(format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                m[0], m[1], m[2], m[3], m[4], m[5]
            ))
        }
        ["addr_len"] => v(String::from("6")),
        ["broadcast"] => v(String::from("ff:ff:ff:ff:ff:ff")),
        ["carrier"] => v(String::from(if i.link { "1" } else { "0" })),
        ["driver"] => v(String::from(i.driver)),
        ["flags"] => {
            let mut f = if i.up { 0x1 } else { 0 };
            f |= if i.loopback { 0x8 } else { 0x1002 };
            if i.up && i.link {
                f |= 0x40;
            }
            v(format!("{:#x}", f))
        }
        ["ifindex"] => v(format!("{}", i.index)),
        ["mtu"] => v(format!("{}", i.mtu)),
        ["operstate"] => v(String::from(match (i.up, i.link) {
            (_, _) if i.loopback => "unknown",
            (true, true) => "up",
            (true, false) => "lowerlayerdown",
            _ => "down",
        })),
        ["speed"] => v(i.speed.map_or(String::from("-1"), |s| format!("{}", s))),
        ["type"] => v(String::from(if i.loopback { "772" } else { "1" })),
        ["wireless"] if i.wireless => Some(Node::Dir(files(&["status"]))),
        ["wireless", "status"] if i.wireless => v(i.status),
        ["statistics"] => Some(Node::Dir(files(&[
            "rx_bytes",
            "rx_packets",
            "rx_dropped",
            "tx_bytes",
            "tx_packets",
            "tx_errors",
        ]))),
        ["statistics", s] => {
            let st = i.stats;
            let n = match *s {
                "rx_bytes" => st.rx_bytes,
                "rx_packets" => st.rx_packets,
                "rx_dropped" => st.rx_dropped,
                "tx_bytes" => st.tx_bytes,
                "tx_packets" => st.tx_packets,
                "tx_errors" => st.tx_errors,
                _ => return None,
            };
            v(format!("{}", n))
        }
        _ => None,
    }
}

fn block_node(rest: &[&str]) -> Option<Node> {
    let disks = crate::block::disks();
    let Some(first) = rest.first() else {
        return Some(Node::Dir(
            disks
                .iter()
                .map(|d| (d.name.clone(), FileType::Directory))
                .collect(),
        ));
    };
    let d = disks.into_iter().find(|d| d.name == *first)?;
    let v = |s: String| Some(Node::File(s + "\n"));
    match rest.get(1..).unwrap_or(&[]) {
        [] => {
            let mut e = files(&["size", "ro", "model", "dev"]);
            if d.parent.is_some() {
                e.extend(files(&["partition"]));
            }
            Some(Node::Dir(e))
        }
        ["size"] => v(format!("{}", d.dev.size_bytes() / 512)),
        ["ro"] => v(String::from(if d.dev.read_only() { "1" } else { "0" })),
        ["model"] => v(d.dev.model()),
        ["dev"] => v(format!("/dev/{}", d.name)),
        ["partition"] => v(format!("{}", d.part.as_ref().map_or(0, |p| p.index))),
        _ => None,
    }
}

fn generated(path: &str) -> Option<Node> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [] => Some(dir(&["block", "class", "devices"])),
        ["class"] => Some(dir(&["block", "net"])),
        ["class", "net", rest @ ..] => net_node(rest),
        ["class", "block", rest @ ..] | ["block", rest @ ..] => block_node(rest),
        ["devices"] => Some(dir(&["system"])),
        ["devices", "system"] => Some(dir(&["cpu"])),
        ["devices", "system", "cpu"] => Some(Node::Dir(files(&["online", "possible", "present"]))),
        ["devices", "system", "cpu", "online" | "present"] => Some(Node::File(cpu_range())),
        ["devices", "system", "cpu", "possible"] => {
            let n = crate::arch::x86_64::acpi::platform().cpus.len().max(1);
            Some(Node::File(if n == 1 {
                String::from("0\n")
            } else {
                format!("0-{}\n", n - 1)
            }))
        }
        _ => None,
    }
}

fn resolve(path: &str) -> Option<Node> {
    let key = reg_key(path);
    let node = match generated(&key) {
        Some(n) => n,
        None => reg_node(&key)?,
    };
    Some(match node {
        Node::Dir(mut entries) => {
            for (name, kind) in reg_children(&key) {
                if !entries.iter().any(|(n, _)| *n == name) {
                    entries.push((name, kind));
                }
            }
            Node::Dir(entries)
        }
        n => n,
    })
}

struct SysNode(String);

impl Inode for SysNode {
    fn metadata(&self) -> KResult<Metadata> {
        let (kind, mode, size) = match resolve(&self.0).ok_or(ENOENT)? {
            Node::Dir(_) => (FileType::Directory, 0o555, 0),
            Node::File(s) => (FileType::Regular, 0o444, s.len() as u64),
            // Linux reports a page for attribute files.
            Node::Attr(a) => (FileType::Regular, a.mode() & 0o777, 4096),
            Node::Link(t) => (FileType::Symlink, 0o777, t.len() as u64),
        };
        let mut m = Metadata::new(kind, mode);
        m.size = size;
        m.ino = 0x5000 + self.0.len() as u64;
        Ok(m)
    }

    fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
        let p = format!("{}/{}", self.0, name);
        resolve(&p).ok_or(ENOENT)?;
        Ok(Arc::new(SysNode(p)))
    }

    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        match resolve(&self.0).ok_or(ENOENT)? {
            Node::Dir(entries) => Ok(entries
                .into_iter()
                .enumerate()
                .map(|(i, (name, kind))| DirEntry {
                    name,
                    ino: 0x6000 + i as u64,
                    kind,
                })
                .collect()),
            _ => Err(ENOTDIR),
        }
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        let data = match resolve(&self.0).ok_or(ENOENT)? {
            Node::File(s) => s.into_bytes(),
            Node::Attr(a) => a.show()?,
            Node::Link(_) => return Err(EINVAL),
            Node::Dir(_) => return Err(EISDIR),
        };
        let b = &data[..];
        let off = off as usize;
        if off >= b.len() {
            return Ok(0);
        }
        let n = buf.len().min(b.len() - off);
        buf[..n].copy_from_slice(&b[off..off + n]);
        Ok(n)
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        match resolve(&self.0).ok_or(ENOENT)? {
            // Like sysfs, a store sees the whole write from offset 0.
            Node::Attr(a) if off == 0 => a.store(buf),
            Node::Attr(_) => Ok(0),
            Node::Dir(_) => Err(EISDIR),
            _ => Err(EACCES),
        }
    }

    fn truncate(&self, _size: u64) -> KResult<()> {
        match resolve(&self.0).ok_or(ENOENT)? {
            Node::Attr(_) => Ok(()),
            _ => Err(EACCES),
        }
    }

    fn readlink(&self) -> KResult<String> {
        match resolve(&self.0).ok_or(ENOENT)? {
            Node::Link(t) => Ok(t),
            _ => Err(EINVAL),
        }
    }

    fn fs_id(&self) -> usize {
        FS_ID
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
