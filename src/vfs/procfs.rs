//! /proc: kernel and process information generated on read.

use super::*;
use crate::process::{self, Process};
use alloc::collections::BTreeMap;
use alloc::format;
use core::fmt::Write;

const FS_ID: usize = 0x9F0C;

type Gen = fn() -> String;

static FILES: RwLock<BTreeMap<&'static str, Gen>> = RwLock::new(BTreeMap::new());

/// Register a generated file, e.g. `register("net/dev", f)`.
pub fn register(path: &'static str, f: Gen) {
    FILES.write().insert(path, f);
}

pub struct ProcFs;

impl ProcFs {
    pub fn new() -> Arc<ProcFs> {
        register_builtin();
        Arc::new(ProcFs)
    }
}

impl FileSystem for ProcFs {
    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(ProcDir::Root)
    }
    fn name(&self) -> &'static str {
        "proc"
    }
    fn read_only(&self) -> bool {
        true
    }
}

enum ProcDir {
    Root,
    /// A directory of registered files ("net", "bus", ...).
    Static(String),
    Pid(u32),
    Fd(u32),
}

struct ProcFile {
    content: fn(Option<&Arc<Process>>) -> String,
    generator: Option<Gen>,
    pid: Option<u32>,
}

struct ProcLink(String);

fn dir_meta(ino: u64) -> Metadata {
    let mut m = Metadata::new(FileType::Directory, 0o555);
    m.ino = ino;
    m.nlink = 2;
    m
}

impl ProcFile {
    fn render(&self) -> String {
        if let Some(g) = self.generator {
            return g();
        }
        let p = self.pid.and_then(process::find);
        (self.content)(p.as_ref())
    }
}

impl Inode for ProcFile {
    fn metadata(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::Regular, 0o444);
        m.size = 0;
        m.ino = 7;
        Ok(m)
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        let s = self.render();
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

impl Inode for ProcLink {
    fn metadata(&self) -> KResult<Metadata> {
        let mut m = Metadata::new(FileType::Symlink, 0o777);
        m.size = self.0.len() as u64;
        Ok(m)
    }
    fn readlink(&self) -> KResult<String> {
        Ok(self.0.clone())
    }
    fn fs_id(&self) -> usize {
        FS_ID
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn empty(_: Option<&Arc<Process>>) -> String {
    String::new()
}

fn pid_file(pid: u32, f: fn(Option<&Arc<Process>>) -> String) -> Arc<dyn Inode> {
    Arc::new(ProcFile {
        content: f,
        generator: None,
        pid: Some(pid),
    })
}

const PID_FILES: [&str; 7] = [
    "stat", "statm", "status", "cmdline", "comm", "maps", "environ",
];

impl Inode for ProcDir {
    fn metadata(&self) -> KResult<Metadata> {
        Ok(dir_meta(match self {
            ProcDir::Root => 1,
            ProcDir::Static(_) => 2,
            ProcDir::Pid(p) => 1000 + *p as u64,
            ProcDir::Fd(p) => 100000 + *p as u64,
        }))
    }

    fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
        match self {
            ProcDir::Root | ProcDir::Static(_) => {
                let prefix = match self {
                    ProcDir::Static(p) => format!("{}/", p),
                    _ => String::new(),
                };
                let full = format!("{}{}", prefix, name);
                if let ProcDir::Root = self {
                    if name == "self" {
                        return Ok(Arc::new(ProcLink(format!("{}", process::current_pid()))));
                    }
                    if let Ok(pid) = name.parse::<u32>() {
                        return process::find(pid)
                            .map(|_| Arc::new(ProcDir::Pid(pid)) as Arc<dyn Inode>)
                            .ok_or(ENOENT);
                    }
                }
                let files = FILES.read();
                if let Some(g) = files.get(full.as_str()) {
                    return Ok(Arc::new(ProcFile {
                        content: empty,
                        generator: Some(*g),
                        pid: None,
                    }));
                }
                let dir_prefix = format!("{}/", full);
                if files.keys().any(|k| k.starts_with(&dir_prefix)) {
                    return Ok(Arc::new(ProcDir::Static(full)));
                }
                Err(ENOENT)
            }
            ProcDir::Pid(pid) => {
                let pid = *pid;
                let p = process::find(pid).ok_or(ENOENT)?;
                match name {
                    "stat" => Ok(pid_file(pid, gen_pid_stat)),
                    "status" => Ok(pid_file(pid, gen_pid_status)),
                    "cmdline" => Ok(pid_file(pid, gen_pid_cmdline)),
                    "comm" => Ok(pid_file(pid, gen_pid_comm)),
                    "maps" => Ok(pid_file(pid, gen_pid_maps)),
                    "statm" => Ok(pid_file(pid, gen_pid_statm)),
                    "environ" => Ok(pid_file(pid, empty)),
                    "cwd" => Ok(Arc::new(ProcLink(p.cwd.lock().clone()))),
                    "exe" => Ok(Arc::new(ProcLink(p.exe.lock().clone()))),
                    "fd" => Ok(Arc::new(ProcDir::Fd(pid))),
                    _ => Err(ENOENT),
                }
            }
            ProcDir::Fd(pid) => {
                let p = process::find(*pid).ok_or(ENOENT)?;
                let fd: i32 = name.parse().map_err(|_| ENOENT)?;
                let f = p.files.lock().get(fd).map_err(|_| ENOENT)?;
                Ok(Arc::new(ProcLink(f.path.clone())))
            }
        }
    }

    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        let mut out = Vec::new();
        match self {
            ProcDir::Root | ProcDir::Static(_) => {
                let prefix = match self {
                    ProcDir::Static(p) => format!("{}/", p),
                    _ => String::new(),
                };
                for k in FILES.read().keys() {
                    let Some(rest) = k.strip_prefix(prefix.as_str()) else {
                        continue;
                    };
                    let (name, kind) = match rest.split_once('/') {
                        Some((d, _)) => (d, FileType::Directory),
                        None => (rest, FileType::Regular),
                    };
                    if !out.iter().any(|e: &DirEntry| e.name == name) {
                        out.push(DirEntry {
                            name: name.to_string(),
                            ino: 3,
                            kind,
                        });
                    }
                }
                if let ProcDir::Root = self {
                    out.push(DirEntry {
                        name: String::from("self"),
                        ino: 4,
                        kind: FileType::Symlink,
                    });
                    for p in process::all() {
                        out.push(DirEntry {
                            name: format!("{}", p.pid),
                            ino: 1000 + p.pid as u64,
                            kind: FileType::Directory,
                        });
                    }
                }
            }
            ProcDir::Pid(_) => {
                for n in PID_FILES {
                    out.push(DirEntry {
                        name: n.to_string(),
                        ino: 5,
                        kind: FileType::Regular,
                    });
                }
                for n in ["cwd", "exe"] {
                    out.push(DirEntry {
                        name: n.to_string(),
                        ino: 6,
                        kind: FileType::Symlink,
                    });
                }
                out.push(DirEntry {
                    name: String::from("fd"),
                    ino: 7,
                    kind: FileType::Directory,
                });
            }
            ProcDir::Fd(pid) => {
                let p = process::find(*pid).ok_or(ENOENT)?;
                for (fd, _) in p.files.lock().list() {
                    out.push(DirEntry {
                        name: format!("{}", fd),
                        ino: 8,
                        kind: FileType::Symlink,
                    });
                }
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

// ---------------------------------------------------------------------------
// Per-process files
// ---------------------------------------------------------------------------

fn state_char(p: &Process) -> char {
    if p.zombie.load(Ordering::SeqCst) {
        return 'Z';
    }
    if p.stopped.load(Ordering::SeqCst) {
        return 'T';
    }
    if p.live_threads().iter().any(|t| {
        t.state() == crate::sched::State::Running || t.state() == crate::sched::State::Ready
    }) {
        'R'
    } else {
        'S'
    }
}

/// `/proc/[pid]/stat` in the Linux layout (52 fields; times in
/// `USER_HZ` clock ticks, `starttime` in clock ticks since boot).
fn gen_pid_stat(p: Option<&Arc<Process>>) -> String {
    use crate::sched::cputime::{USER_HZ, to_clock_t};
    let Some(p) = p else { return String::new() };
    let (vsz, rss) = p
        .vm()
        .map(|v| {
            let s = v.lock();
            (s.virtual_size(), s.resident_pages())
        })
        .unwrap_or((0, 0));
    let threads = p.live_threads();
    let (nice, cpu) = threads.first().map_or((0, 0), |t| {
        (t.nice.load(Ordering::Relaxed) as i64, t.last_cpu())
    });
    let (utime, stime) = p.cpu_times();
    let (cutime, cstime) = p.children_cpu_times();
    let mut s = format!(
        "{} ({}) {} {} {} {} 0 -1 0 0 0 0 0 {} {} {} {} {} {} {} 0 {} {} {} {}",
        p.pid,
        p.name.lock(),
        state_char(p),
        p.ppid.load(Ordering::SeqCst),
        p.pgid.load(Ordering::SeqCst),
        p.sid.load(Ordering::SeqCst),
        to_clock_t(utime),
        to_clock_t(stime),
        to_clock_t(cutime),
        to_clock_t(cstime),
        20 + nice,
        nice,
        threads.len(),
        p.start_ns / (1_000_000_000 / USER_HZ),
        vsz,
        rss,
        u64::MAX,
    );
    // Fields 26-52: startcode endcode startstack kstkesp kstkeip, signal
    // blocked sigignore sigcatch, wchan nswap cnswap, exit_signal
    // processor, then rt_priority .. exit_code (13 zeros).
    let _ = writeln!(
        s,
        " 0 0 0 0 0 {} {} 0 0 0 0 0 17 {} 0 0 0 0 0 0 0 0 0 0 0 0 0",
        p.signals.pending.load(Ordering::SeqCst),
        p.signals.blocked.load(Ordering::SeqCst),
        cpu
    );
    s
}

/// `/proc/[pid]/statm`: size and resident set in pages (shared, text,
/// lib, data and dirty are not tracked and read 0).
fn gen_pid_statm(p: Option<&Arc<Process>>) -> String {
    let Some(p) = p else { return String::new() };
    let (vsz, rss) = p
        .vm()
        .map(|v| {
            let s = v.lock();
            (s.virtual_size(), s.resident_pages())
        })
        .unwrap_or((0, 0));
    format!("{} {} 0 0 0 0 0\n", vsz / 4096, rss)
}

fn gen_pid_status(p: Option<&Arc<Process>>) -> String {
    let Some(p) = p else { return String::new() };
    let (vsz, rss) = p
        .vm()
        .map(|v| {
            let s = v.lock();
            (s.virtual_size(), s.resident_pages())
        })
        .unwrap_or((0, 0));
    format!(
        "Name:\t{}\nState:\t{}\nTgid:\t{}\nPid:\t{}\nPPid:\t{}\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nVmSize:\t{} kB\nVmRSS:\t{} kB\nThreads:\t{}\nSigPnd:\t{:016x}\nSigBlk:\t{:016x}\n",
        p.name.lock(),
        state_char(p),
        p.pid,
        p.pid,
        p.ppid.load(Ordering::SeqCst),
        vsz / 1024,
        rss * 4,
        p.live_threads().len(),
        p.signals.pending.load(Ordering::SeqCst),
        p.signals.blocked.load(Ordering::SeqCst),
    )
}

fn gen_pid_cmdline(p: Option<&Arc<Process>>) -> String {
    let Some(p) = p else { return String::new() };
    let mut s = String::new();
    for a in p.cmdline.lock().iter() {
        s.push_str(a);
        s.push('\0');
    }
    s
}

fn gen_pid_comm(p: Option<&Arc<Process>>) -> String {
    p.map(|p| format!("{}\n", p.name.lock()))
        .unwrap_or_default()
}

fn gen_pid_maps(p: Option<&Arc<Process>>) -> String {
    let Some(p) = p else { return String::new() };
    let mut s = String::new();
    if let Some(vm) = p.vm() {
        for a in vm.lock().areas.values() {
            let _ = writeln!(
                s,
                "{:012x}-{:012x} {}{}{}p 00000000 00:00 0 {}",
                a.start,
                a.end,
                if a.prot & 1 != 0 { 'r' } else { '-' },
                if a.prot & 2 != 0 { 'w' } else { '-' },
                if a.prot & 4 != 0 { 'x' } else { '-' },
                a.name
            );
        }
    }
    s
}

// ---------------------------------------------------------------------------
// System files
// ---------------------------------------------------------------------------

fn gen_version() -> String {
    format!(
        "RustOS version {} (rustc) #1 SMP x86_64\n",
        env!("CARGO_PKG_VERSION")
    )
}

fn gen_uptime() -> String {
    let ns = crate::time::nanos();
    // Idle time summed over all CPUs, as on Linux.
    let idle = crate::sched::cputime::to_clock_t(crate::sched::cputime::total_idle());
    format!(
        "{}.{:02} {}.{:02}\n",
        ns / 1_000_000_000,
        (ns / 10_000_000) % 100,
        idle / 100,
        idle % 100
    )
}

fn gen_meminfo() -> String {
    let (free, total) = crate::mm::memory_stats();
    let (cached, dirty) = crate::mm::pagecache::stats();
    let (cached, dirty) = (cached as u64 * 4, dirty as u64 * 4);
    format!(
        "MemTotal:       {:8} kB\nMemFree:        {:8} kB\nMemAvailable:   {:8} kB\nKernelHeap:     {:8} kB\nSwapTotal:             0 kB\nSwapFree:              0 kB\nCached:         {:8} kB\nDirty:          {:8} kB\n",
        total / 1024,
        free / 1024,
        free / 1024 + cached - dirty,
        crate::allocator::heap_size() / 1024,
        cached,
        dirty
    )
}

fn cpu_brand() -> String {
    let max = core::arch::x86_64::__cpuid(0x8000_0000).eax;
    if max < 0x8000_0004 {
        return String::from("x86_64 processor");
    }
    let mut b = Vec::new();
    for leaf in 0x8000_0002u32..=0x8000_0004 {
        let r = core::arch::x86_64::__cpuid(leaf);
        for v in [r.eax, r.ebx, r.ecx, r.edx] {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    String::from_utf8_lossy(&b)
        .trim_matches(char::from(0))
        .trim()
        .into()
}

fn gen_cpuinfo() -> String {
    let brand = cpu_brand();
    let mut s = String::new();
    for i in 0..crate::cpu::cpu_count().max(1) {
        let _ = write!(
            s,
            "processor\t: {}\nmodel name\t: {}\ncpu MHz\t\t: {}\n\n",
            i,
            brand,
            crate::time::tsc_hz() / 1_000_000
        );
    }
    s
}

fn gen_mounts() -> String {
    let mut s = String::new();
    for (path, fs, src) in super::mounts() {
        let ro = super::mount_fs(&path).is_some_and(|f| f.read_only());
        let _ = writeln!(
            s,
            "{} {} {} {} 0 0",
            src,
            path,
            fs,
            if ro { "ro" } else { "rw" }
        );
    }
    s
}

fn gen_interrupts() -> String {
    let mut s = String::from("       CPU0\n");
    for (v, c) in crate::idt::interrupt_counts() {
        let _ = writeln!(s, "{:4}: {:10}", v, c);
    }
    s
}

/// `/proc/stat`: per-CPU time in `USER_HZ` ticks (user nice system idle
/// iowait irq softirq steal guest guest_nice), interrupts, context
/// switches, boot time, forks and run-queue state.
fn gen_stat() -> String {
    use crate::sched::cputime::{NCLASS, cpu_stat, to_clock_t};
    let ncpu = crate::cpu::cpu_count().max(1) as usize;
    let per_cpu: Vec<[u64; NCLASS]> = (0..ncpu).map(cpu_stat).collect();
    let mut total = [0u64; NCLASS];
    for c in &per_cpu {
        for (t, v) in total.iter_mut().zip(c) {
            *t += v;
        }
    }
    let mut s = String::new();
    let line = |s: &mut String, name: &str, v: &[u64; NCLASS]| {
        s.push_str(name);
        for x in v {
            let _ = write!(s, " {}", to_clock_t(*x));
        }
        s.push('\n');
    };
    line(&mut s, "cpu ", &total);
    for (i, c) in per_cpu.iter().enumerate() {
        line(&mut s, &format!("cpu{}", i), c);
    }
    let counts = crate::idt::interrupt_counts();
    let mut by_vector = [0usize; 256];
    for (v, c) in &counts {
        by_vector[*v as usize] = *c;
    }
    let _ = write!(s, "intr {}", by_vector.iter().sum::<usize>());
    for c in by_vector {
        let _ = write!(s, " {}", c);
    }
    let _ = write!(
        s,
        "\nctxt {}\nbtime {}\nprocesses {}\nprocs_running {}\nprocs_blocked {}\n",
        crate::sched::context_switches(),
        crate::time::unix_time() - crate::time::nanos() / 1_000_000_000,
        process::last_pid(),
        crate::sched::cputime::nr_running(),
        crate::sched::cputime::nr_uninterruptible()
    );
    s
}

/// `/proc/loadavg`: 1, 5 and 15 minute load averages, runnable/total
/// threads and the last process id handed out.
fn gen_loadavg() -> String {
    use crate::sched::cputime::{fmt_load, loadavg_raw, nr_running};
    let [a, b, c] = loadavg_raw();
    format!(
        "{} {} {} {}/{} {}\n",
        fmt_load(a),
        fmt_load(b),
        fmt_load(c),
        nr_running(),
        crate::sched::thread_count(),
        process::last_pid()
    )
}

fn gen_pci() -> String {
    let mut s = String::new();
    for d in crate::pci::enumerate() {
        let _ = writeln!(
            s,
            "{} {:02x}{:02x}: {:04x}:{:04x} (rev {:02x}) {} [{}]",
            d.name(),
            d.class,
            d.subclass,
            d.vendor_id,
            d.device_id,
            d.revision,
            crate::pci::class_name(d.class, d.subclass, d.prog_if),
            crate::pci::ids::vendor_name(d.vendor_id)
        );
    }
    s
}

/// Config space (first 256 bytes, plus the PCIe extended header list)
/// of network and wireless controllers, for bug reports.
fn gen_pci_config() -> String {
    let mut s = String::new();
    for d in crate::pci::enumerate() {
        if d.class != 0x02 && d.class != 0x0d {
            continue;
        }
        let _ = writeln!(s, "{} {:04x}:{:04x}", d.name(), d.vendor_id, d.device_id);
        for row in 0..16u16 {
            let _ = write!(s, "{:03x}:", row * 16);
            for w in 0..4u16 {
                let v = d.read32(row * 16 + w * 4);
                for b in v.to_le_bytes() {
                    let _ = write!(s, " {:02x}", b);
                }
            }
            s.push('\n');
        }
        // Extended capabilities: id/version/next at 0x100 onwards.
        let mut off = 0x100u16;
        let mut guard = 0;
        while off >= 0x100 && guard < 48 {
            let h = d.read32(off);
            if h == 0 || h == 0xFFFF_FFFF {
                break;
            }
            let _ = writeln!(
                s,
                "ext cap {:#06x} v{} at {:#05x}",
                h & 0xFFFF,
                (h >> 16) & 15,
                off
            );
            off = (h >> 20) as u16 & 0xFFC;
            guard += 1;
        }
        s.push('\n');
    }
    s
}

fn gen_filesystems() -> String {
    String::from("nodev\ttmpfs\nnodev\tproc\nnodev\tdevtmpfs\n\tvfat\n\text2\n\text4\n")
}

fn gen_threads() -> String {
    let mut s = String::new();
    for (tid, name, state, user) in crate::sched::thread_list() {
        let _ = writeln!(
            s,
            "{} {:?} {} {}",
            tid,
            state,
            if user { "user" } else { "kernel" },
            name
        );
    }
    s
}

fn gen_cmdline() -> String {
    let p = crate::params::cmdline();
    if p.is_empty() {
        String::from("root=auto\n")
    } else {
        alloc::format!("root=auto {}\n", p)
    }
}

fn register_builtin() {
    register("version", gen_version);
    register("uptime", gen_uptime);
    register("meminfo", gen_meminfo);
    register("cpuinfo", gen_cpuinfo);
    register("mounts", gen_mounts);
    register("interrupts", gen_interrupts);
    register("asound/cards", crate::sound::proc_cards);
    register("stat", gen_stat);
    register("loadavg", gen_loadavg);
    register("bus/pci/devices", gen_pci);
    register("bus/pci/config", gen_pci_config);
    register("filesystems", gen_filesystems);
    register("threads", gen_threads);
    register("cmdline", gen_cmdline);
}
