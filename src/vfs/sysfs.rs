//! /sys: a read-only view of devices, generated on lookup.
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
        true
    }
}

/// What lives at a path.
enum Node {
    Dir(Vec<(String, FileType)>),
    File(String),
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

fn resolve(path: &str) -> Option<Node> {
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

struct SysNode(String);

impl Inode for SysNode {
    fn metadata(&self) -> KResult<Metadata> {
        let (kind, mode, size) = match resolve(&self.0).ok_or(ENOENT)? {
            Node::Dir(_) => (FileType::Directory, 0o555, 0),
            Node::File(s) => (FileType::Regular, 0o444, s.len() as u64),
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
        let Node::File(s) = resolve(&self.0).ok_or(ENOENT)? else {
            return Err(EISDIR);
        };
        let b = s.as_bytes();
        let off = off as usize;
        if off >= b.len() {
            return Ok(0);
        }
        let n = buf.len().min(b.len() - off);
        buf[..n].copy_from_slice(&b[off..off + n]);
        Ok(n)
    }

    fn fs_id(&self) -> usize {
        FS_ID
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
