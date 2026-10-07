//! /dev: device nodes registered by drivers.

use super::*;
use alloc::collections::BTreeMap;

static FS_ID: usize = 0xDE7F5;

#[derive(Clone)]
struct DevNode {
    kind: FileType,
    rdev: u64,
    mode: u32,
    obj: Arc<dyn FileLike>,
    ino: u64,
}

static DEVICES: RwLock<BTreeMap<String, DevNode>> = RwLock::new(BTreeMap::new());
static NEXT_INO: AtomicU64 = AtomicU64::new(100);

/// Register a device node at /dev/<name>. `name` may contain one level of
/// subdirectory (e.g. "input/mice").
pub fn register(name: &str, kind: FileType, rdev: u64, obj: Arc<dyn FileLike>) {
    let mode = if kind == FileType::BlockDevice {
        0o660
    } else {
        0o666
    };
    DEVICES.write().insert(
        name.to_string(),
        DevNode {
            kind,
            rdev,
            mode,
            obj,
            ino: NEXT_INO.fetch_add(1, Ordering::SeqCst),
        },
    );
    notify_poll();
}

pub fn unregister(name: &str) {
    DEVICES.write().remove(name);
}

pub fn get(name: &str) -> Option<Arc<dyn FileLike>> {
    DEVICES.read().get(name).map(|d| d.obj.clone())
}

/// Names of registered devices of a kind.
pub fn list(kind: FileType) -> Vec<String> {
    DEVICES
        .read()
        .iter()
        .filter(|(_, d)| d.kind == kind)
        .map(|(n, _)| n.clone())
        .collect()
}

pub struct DevFs {
    root: Arc<DevDir>,
}

impl DevFs {
    pub fn new() -> Arc<DevFs> {
        register_builtin();
        Arc::new(DevFs {
            root: Arc::new(DevDir {
                prefix: String::new(),
            }),
        })
    }
}

impl FileSystem for DevFs {
    fn root(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }
    fn name(&self) -> &'static str {
        "devtmpfs"
    }
}

struct DevDir {
    prefix: String,
}

impl Inode for DevDir {
    fn metadata(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::Directory, 0o755);
        m.ino = 1;
        Ok(m)
    }
    fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
        let full = if self.prefix.is_empty() {
            name.to_string()
        } else {
            alloc::format!("{}/{}", self.prefix, name)
        };
        let devs = DEVICES.read();
        if let Some(d) = devs.get(&full) {
            return Ok(Arc::new(DevNodeInode { node: d.clone() }));
        }
        let dir_prefix = alloc::format!("{}/", full);
        if devs.keys().any(|k| k.starts_with(&dir_prefix)) {
            return Ok(Arc::new(DevDir { prefix: full }));
        }
        if self.prefix.is_empty() && name == "shm" {
            // Mount point of the /dev/shm tmpfs.
            return Ok(Arc::new(DevDir {
                prefix: String::from("shm"),
            }));
        }
        if self.prefix.is_empty() {
            let target = match name {
                "stdin" => Some("/proc/self/fd/0"),
                "stdout" => Some("/proc/self/fd/1"),
                "stderr" => Some("/proc/self/fd/2"),
                "fd" => Some("/proc/self/fd"),
                _ => None,
            };
            if let Some(t) = target {
                return Ok(Arc::new(DevLink(t)));
            }
        }
        Err(ENOENT)
    }
    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        let devs = DEVICES.read();
        let prefix = if self.prefix.is_empty() {
            String::new()
        } else {
            alloc::format!("{}/", self.prefix)
        };
        let mut out: Vec<DirEntry> = Vec::new();
        for (name, d) in devs.iter() {
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                None => out.push(DirEntry {
                    name: rest.to_string(),
                    ino: d.ino,
                    kind: d.kind,
                }),
                Some((dir, _)) => {
                    if !out.iter().any(|e| e.name == dir) {
                        out.push(DirEntry {
                            name: dir.to_string(),
                            ino: 2,
                            kind: FileType::Directory,
                        });
                    }
                }
            }
        }
        if self.prefix.is_empty() {
            for l in ["stdin", "stdout", "stderr", "fd"] {
                out.push(DirEntry {
                    name: l.to_string(),
                    ino: 3,
                    kind: FileType::Symlink,
                });
            }
        }
        Ok(out)
    }
    fn fs_id(&self) -> usize {
        FS_ID
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct DevLink(&'static str);

impl Inode for DevLink {
    fn metadata(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::Symlink, 0o777);
        m.size = self.0.len() as u64;
        Ok(m)
    }
    fn readlink(&self) -> KResult<String> {
        Ok(self.0.to_string())
    }
    fn fs_id(&self) -> usize {
        FS_ID
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct DevNodeInode {
    node: DevNode,
}

impl Inode for DevNodeInode {
    fn metadata(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(self.node.kind, self.node.mode);
        m.rdev = self.node.rdev;
        m.ino = self.node.ino;
        if let Some(sz) = self.node.obj.size() {
            m.size = sz;
        }
        Ok(m)
    }
    fn open(&self, flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        Ok(Some(
            self.node
                .obj
                .open_instance(flags)?
                .unwrap_or_else(|| self.node.obj.clone()),
        ))
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        self.node.obj.read_at(off, buf).unwrap_or(Err(EINVAL))
    }
    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        self.node.obj.write_at(off, buf).unwrap_or(Err(EINVAL))
    }
    fn fs_id(&self) -> usize {
        FS_ID
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Built-in character devices
// ---------------------------------------------------------------------------

struct Null;
struct Zero;
struct Full;
struct Random;
struct Kmsg;

impl FileLike for Null {
    fn read(&self, _b: &mut [u8], _nb: bool) -> KResult<usize> {
        Ok(0)
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl FileLike for Zero {
    fn read(&self, b: &mut [u8], _nb: bool) -> KResult<usize> {
        b.fill(0);
        Ok(b.len())
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl FileLike for Full {
    fn read(&self, b: &mut [u8], _nb: bool) -> KResult<usize> {
        b.fill(0);
        Ok(b.len())
    }
    fn write(&self, _b: &[u8], _nb: bool) -> KResult<usize> {
        Err(ENOSPC)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl FileLike for Random {
    fn read(&self, b: &mut [u8], _nb: bool) -> KResult<usize> {
        crate::drivers::random::fill(b);
        Ok(b.len())
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        Ok(b.len())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl FileLike for Kmsg {
    fn read(&self, _b: &mut [u8], _nb: bool) -> KResult<usize> {
        Ok(0)
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Option<KResult<usize>> {
        let mut all = alloc::vec![0u8; crate::klog::len()];
        let n = crate::klog::read_tail(&mut all);
        let off = off as usize;
        if off >= n {
            return Some(Ok(0));
        }
        let m = buf.len().min(n - off);
        buf[..m].copy_from_slice(&all[off..off + m]);
        Some(Ok(m))
    }
    fn write(&self, b: &[u8], _nb: bool) -> KResult<usize> {
        crate::klog::write_bytes(b);
        crate::drivers::serial::write_raw(b);
        Ok(b.len())
    }
    fn size(&self) -> Option<u64> {
        Some(crate::klog::len() as u64)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn register_builtin() {
    register("null", FileType::CharDevice, (1 << 8) | 3, Arc::new(Null));
    register("zero", FileType::CharDevice, (1 << 8) | 5, Arc::new(Zero));
    register("full", FileType::CharDevice, (1 << 8) | 7, Arc::new(Full));
    register(
        "random",
        FileType::CharDevice,
        (1 << 8) | 8,
        Arc::new(Random),
    );
    register(
        "urandom",
        FileType::CharDevice,
        (1 << 8) | 9,
        Arc::new(Random),
    );
    register("kmsg", FileType::CharDevice, (1 << 8) | 11, Arc::new(Kmsg));
    let tty = crate::tty::console();
    register(
        "tty",
        FileType::CharDevice,
        5 << 8,
        Arc::new(crate::tty::pty::Ctty),
    );
    register("console", FileType::CharDevice, (5 << 8) | 1, tty.clone());
    register(
        "tty0",
        FileType::CharDevice,
        4 << 8,
        Arc::new(crate::tty::pty::ActiveVc),
    );
    for (i, vc) in crate::tty::vcs().iter().enumerate() {
        register(
            &alloc::format!("tty{}", i + 1),
            FileType::CharDevice,
            (4 << 8) | (i as u64 + 1),
            vc.clone(),
        );
    }
    register("ttyS0", FileType::CharDevice, (4 << 8) | 64, tty);
    register(
        "ptmx",
        FileType::CharDevice,
        (5 << 8) | 2,
        Arc::new(crate::tty::pty::Ptmx),
    );
    if let Some(fb) = crate::drivers::fbdev::FbDev::new() {
        register("fb0", FileType::CharDevice, 29 << 8, fb);
    }
    register(
        "input/mice",
        FileType::CharDevice,
        (13 << 8) | 63,
        Arc::new(crate::drivers::mouse::MouseDev),
    );
    register(
        "input/event0",
        FileType::CharDevice,
        (13 << 8) | 64,
        crate::drivers::input::event0(),
    );
    register(
        "input/js0",
        FileType::CharDevice,
        13 << 8,
        Arc::new(crate::drivers::input::JoystickDev),
    );
}
