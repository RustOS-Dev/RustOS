//! System applets.

use crate::{err, human, parse_flags};
use rustos_rt::prelude::*;
use rustos_rt::{env, fs, io, process, signal, time};

pub fn clear(_: &[String]) -> i32 {
    print!("\x1b[H\x1b[2J");
    0
}

fn console_ioctl(req: usize, arg: usize) -> isize {
    let fd = match rustos_rt::fs::File::open("/dev/tty0") {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot open /dev/tty0: {}", e);
            return -1;
        }
    };
    rustos_rt::sys::syscall(16, &[fd.fd() as usize, req, arg])
}

/// chvt N: show virtual console N.
pub fn chvt(args: &[String]) -> i32 {
    let Some(n) = args.get(1).and_then(|a| a.parse::<usize>().ok()) else {
        eprintln!("usage: chvt N");
        return 2;
    };
    if console_ioctl(0x5606, n) < 0 {
        eprintln!("chvt: no console {}", n);
        return 1;
    }
    0
}

/// fgconsole: number of the visible virtual console.
pub fn fgconsole(_: &[String]) -> i32 {
    let mut st = [0u16; 3];
    if console_ioctl(0x5603, st.as_mut_ptr() as usize) < 0 {
        return 1;
    }
    println!("{}", st[0]);
    0
}

/// tty: name of the terminal on standard input.
pub fn tty(_: &[String]) -> i32 {
    match rustos_rt::fs::read_link("/proc/self/fd/0") {
        Ok(p) if p.starts_with("/dev/") => {
            println!("{}", p);
            0
        }
        _ => {
            println!("not a tty");
            1
        }
    }
}

pub fn date(args: &[String]) -> i32 {
    let t = time::now();
    let (y, mo, d, h, mi, s) = time::civil(t);
    let wd = time::WEEKDAYS[((t / 86400) % 7) as usize];
    if let Some(fmt) = args.get(1).and_then(|a| a.strip_prefix('+')) {
        let mut out = String::new();
        let mut chars = fmt.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('Y') => out.push_str(&format!("{}", y)),
                Some('m') => out.push_str(&format!("{:02}", mo)),
                Some('d') => out.push_str(&format!("{:02}", d)),
                Some('H') => out.push_str(&format!("{:02}", h)),
                Some('M') => out.push_str(&format!("{:02}", mi)),
                Some('S') => out.push_str(&format!("{:02}", s)),
                Some('s') => out.push_str(&format!("{}", t)),
                Some('a') => out.push_str(wd),
                Some('b') => out.push_str(time::MONTHS[(mo - 1) as usize]),
                Some('F') => out.push_str(&format!("{}-{:02}-{:02}", y, mo, d)),
                Some('T') => out.push_str(&format!("{:02}:{:02}:{:02}", h, mi, s)),
                Some('%') => out.push('%'),
                Some(o) => {
                    out.push('%');
                    out.push(o);
                }
                None => out.push('%'),
            }
        }
        println!("{}", out);
    } else {
        println!(
            "{} {} {:2} {:02}:{:02}:{:02} UTC {}",
            wd,
            time::MONTHS[(mo - 1) as usize],
            d,
            h,
            mi,
            s,
            y
        );
    }
    0
}

pub fn df(args: &[String]) -> i32 {
    let (flags, _, _) = parse_flags(args);
    let h = flags.contains(&'h');
    println!(
        "{:<20} {:>10} {:>10} {:>10} {:>5} Mounted on",
        "Filesystem",
        if h { "Size" } else { "1K-blocks" },
        "Used",
        "Avail",
        "Use%"
    );
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    for line in mounts.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let Ok(s) = fs::statfs(f[1]) else { continue };
        let bs = s.f_bsize.max(1) as u64;
        let total = s.f_blocks * bs;
        let free = s.f_bfree * bs;
        let used = total.saturating_sub(free);
        let pct = if total > 0 { used * 100 / total } else { 0 };
        let fmt = |v: u64| if h { human(v) } else { format!("{}", v / 1024) };
        println!(
            "{:<20} {:>10} {:>10} {:>10} {:>4}% {}",
            f[0],
            fmt(total),
            fmt(used),
            fmt(free),
            pct,
            f[1]
        );
    }
    0
}

pub fn dmesg(args: &[String]) -> i32 {
    let (flags, _, _) = parse_flags(args);
    match fs::read("/dev/kmsg") {
        Ok(d) => {
            io::flush();
            let _ = io::write_all(1, &d);
            if flags.contains(&'c') {
                // Clearing is not supported; the ring simply rolls over.
            }
            0
        }
        Err(e) => err("dmesg", "/dev/kmsg", e),
    }
}

pub fn env(args: &[String]) -> i32 {
    if args.len() > 1 {
        let mut i = 1;
        while i < args.len() {
            if let Some((k, v)) = args[i].split_once('=') {
                env::set_var(k, v);
                i += 1;
            } else {
                break;
            }
        }
        if i < args.len() {
            let Some(path) = process::find_in_path(&args[i]) else {
                eprintln!("env: {}: No such file or directory", args[i]);
                return 127;
            };
            let e = process::execve(&path, &args[i..], &env::environ());
            eprintln!("env: {}: {}", args[i], e);
            return 126;
        }
    }
    for (k, v) in env::vars() {
        println!("{}={}", k, v);
    }
    0
}

pub fn free(args: &[String]) -> i32 {
    let (flags, _, _) = parse_flags(args);
    let si = process::sysinfo();
    let unit = si.mem_unit.max(1) as u64;
    let total = si.totalram * unit;
    let free = si.freeram * unit;
    let used = total - free;
    let f = |v: u64| {
        if flags.contains(&'h') {
            human(v)
        } else if flags.contains(&'m') {
            format!("{}", v >> 20)
        } else {
            format!("{}", v >> 10)
        }
    };
    println!("{:>7} {:>12} {:>12} {:>12}", "", "total", "used", "free");
    println!(
        "{:>7} {:>12} {:>12} {:>12}",
        "Mem:",
        f(total),
        f(used),
        f(free)
    );
    println!("{:>7} {:>12} {:>12} {:>12}", "Swap:", f(0), f(0), f(0));
    0
}

pub fn hostname(args: &[String]) -> i32 {
    match args.get(1) {
        Some(n) => match env::set_hostname(n) {
            Ok(()) => 0,
            Err(e) => err("hostname", n, e),
        },
        None => {
            println!("{}", env::hostname());
            0
        }
    }
}

pub fn id(_: &[String]) -> i32 {
    println!("uid=0(root) gid=0(root) groups=0(root)");
    0
}

pub fn whoami(_: &[String]) -> i32 {
    println!("root");
    0
}

pub fn kill(args: &[String]) -> i32 {
    if args.get(1).map(|s| s.as_str()) == Some("-l") {
        println!(
            "HUP INT QUIT ILL TRAP ABRT BUS FPE KILL USR1 SEGV USR2 PIPE ALRM TERM STKFLT CHLD CONT STOP TSTP TTIN TTOU"
        );
        return 0;
    }
    let mut sig = signal::SIGTERM;
    let mut rest = &args[1..];
    if rest.len() > 1 && rest[0].starts_with('-') {
        let spec = if rest[0] == "-s" && rest.len() > 2 {
            let s = rest[1].as_str();
            rest = &rest[1..];
            s
        } else {
            &rest[0][1..]
        };
        match signal::parse(spec) {
            Some(n) => sig = n,
            None => {
                eprintln!("kill: {}: invalid signal", spec);
                return 1;
            }
        }
        rest = &rest[1..];
    }
    let mut st = 0;
    for a in rest {
        match a.parse::<i32>() {
            Ok(p) => {
                if let Err(e) = process::kill(p, sig) {
                    eprintln!("kill: ({}) - {}", p, e);
                    st = 1;
                }
            }
            Err(_) => {
                eprintln!("kill: {}: invalid argument", a);
                st = 1;
            }
        }
    }
    st
}

pub fn lsblk(_: &[String]) -> i32 {
    println!(
        "{:<12} {:>10} {:<6} {}",
        "NAME", "SIZE", "TYPE", "MOUNTPOINT"
    );
    let parts = fs::read_to_string("/proc/partitions").unwrap_or_default();
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    for line in parts.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let name = f[3];
        let blocks: u64 = f[2].parse().unwrap_or(0);
        let dev = format!("/dev/{}", name);
        let mp = mounts
            .lines()
            .find(|l| l.split_whitespace().next() == Some(dev.as_str()))
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");
        let is_part = name.chars().last().is_some_and(|c| c.is_ascii_digit())
            && (name.contains('p') || name.starts_with("sd") || name.starts_with("vd"));
        println!(
            "{:<12} {:>10} {:<6} {}",
            name,
            human(blocks * 1024),
            if is_part { "part" } else { "disk" },
            mp
        );
    }
    0
}

pub fn lspci(args: &[String]) -> i32 {
    let (flags, _, _) = parse_flags(args);
    let data = fs::read_to_string("/proc/bus/pci/devices").unwrap_or_default();
    for line in data.lines() {
        if flags.contains(&'n') {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() >= 3 {
                println!("{} {} {}", f[0], f[1], f[2]);
            }
        } else {
            println!("{}", line);
        }
    }
    0
}

pub fn lsusb(_: &[String]) -> i32 {
    match fs::read_to_string("/proc/bus/usb/devices") {
        Ok(d) => {
            print!("{}", d);
            0
        }
        Err(_) => {
            println!("(no USB controller)");
            0
        }
    }
}

pub fn mount(args: &[String]) -> i32 {
    let mut ty = String::from("auto");
    let mut ro = false;
    let mut extra: Vec<String> = Vec::new();
    let mut ops = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => {
                ty = args.get(i + 1).cloned().unwrap_or(ty);
                i += 2;
                continue;
            }
            "-o" => {
                if let Some(o) = args.get(i + 1) {
                    for x in o.split(',').filter(|x| !x.is_empty()) {
                        match x {
                            "ro" => ro = true,
                            "rw" => {}
                            _ => extra.push(x.to_string()),
                        }
                    }
                }
                i += 2;
                continue;
            }
            "-r" => ro = true,
            a => ops.push(a.to_string()),
        }
        i += 1;
    }
    if ro {
        ty.push_str(",ro");
    }
    for x in &extra {
        ty.push(',');
        ty.push_str(x);
    }
    if ops.is_empty() {
        print!("{}", fs::read_to_string("/proc/mounts").unwrap_or_default());
        return 0;
    }
    if ops.len() == 1 {
        eprintln!("usage: mount [-t TYPE] SOURCE TARGET");
        return 1;
    }
    if !fs::is_dir(&ops[1]) {
        let _ = fs::create_dir_all(&ops[1]);
    }
    match fs::mount(&ops[0], &ops[1], &ty) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mount: {} on {}: {}", ops[0], ops[1], e);
            1
        }
    }
}

pub fn umount(args: &[String]) -> i32 {
    let mut st = 0;
    for t in args[1..].iter().filter(|a| !a.starts_with('-')) {
        if let Err(e) = fs::umount(t) {
            st = err("umount", t, e);
        }
    }
    st
}

pub fn poweroff(_: &[String]) -> i32 {
    println!("Powering off...");
    io::flush();
    let _ = process::reboot(process::REBOOT_POWER_OFF);
    1
}

pub fn reboot(_: &[String]) -> i32 {
    println!("Rebooting...");
    io::flush();
    let _ = process::reboot(process::REBOOT_RESTART);
    1
}

struct ProcInfo {
    pid: i32,
    ppid: i32,
    pgid: i32,
    state: char,
    name: String,
    cmd: String,
    vsz: u64,
    rss: u64,
    threads: u32,
    start: u64,
}

fn procs() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    for e in fs::read_dir("/proc").unwrap_or_default() {
        let Ok(pid) = e.name.parse::<i32>() else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(&format!("/proc/{}/stat", pid)) else {
            continue;
        };
        let (Some(l), Some(r)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let name = stat[l + 1..r].to_string();
        let f: Vec<&str> = stat[r + 2..].split_whitespace().collect();
        let g = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let cmd = fs::read(&format!("/proc/{}/cmdline", pid))
            .map(|b| {
                String::from_utf8_lossy(&b)
                    .replace('\0', " ")
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        out.push(ProcInfo {
            pid,
            ppid: g(1) as i32,
            pgid: g(2) as i32,
            state: f.first().and_then(|s| s.chars().next()).unwrap_or('?'),
            name: name.clone(),
            cmd: if cmd.is_empty() {
                format!("[{}]", name)
            } else {
                cmd
            },
            threads: g(16) as u32,
            start: g(18),
            vsz: g(19),
            rss: g(20),
        });
    }
    out.sort_by_key(|p| p.pid);
    out
}

pub fn ps(args: &[String]) -> i32 {
    let full = args
        .iter()
        .skip(1)
        .any(|a| a.contains('f') || a.contains('u') || a == "aux");
    let list = procs();
    let total_mem = process::sysinfo().totalram.max(1);
    if full {
        println!("USER       PID  PPID  PGID %MEM    VSZ   RSS STAT START COMMAND");
        let uptime = process::sysinfo().uptime as u64;
        for p in &list {
            let started_s = uptime.saturating_sub((uptime * 250).saturating_sub(p.start) / 250);
            let _ = started_s;
            let mem = p.rss * 4096 * 1000 / total_mem;
            println!(
                "root  {:>8} {:>5} {:>5} {:>2}.{} {:>6} {:>5} {:<4} {:>5} {}",
                p.pid,
                p.ppid,
                p.pgid,
                mem / 10,
                mem % 10,
                p.vsz / 1024,
                p.rss * 4,
                p.state,
                p.start / 250,
                p.cmd
            );
        }
    } else {
        println!("  PID TTY          TIME CMD");
        for p in &list {
            println!("{:>5} tty1     00:00:00 {}", p.pid, p.name);
        }
    }
    0
}

pub fn sleep(args: &[String]) -> i32 {
    let mut ms = 0u64;
    for a in &args[1..] {
        let (num, mul) = match a.chars().last() {
            Some('s') => (&a[..a.len() - 1], 1000.0),
            Some('m') => (&a[..a.len() - 1], 60_000.0),
            Some('h') => (&a[..a.len() - 1], 3_600_000.0),
            _ => (a.as_str(), 1000.0),
        };
        ms += (num.parse::<f64>().unwrap_or(0.0) * mul) as u64;
    }
    time::sleep_ms(ms);
    0
}

pub fn sync(_: &[String]) -> i32 {
    fs::sync();
    0
}

pub fn test(args: &[String]) -> i32 {
    // Delegate to the shell's implementation.
    let mut argv: Vec<String> = alloc::vec![
        String::from("/bin/sh"),
        String::from("-c"),
        String::from("test \"$@\""),
        String::from("test")
    ];
    let skip_bracket = args[0].ends_with('[');
    for a in &args[1..] {
        if skip_bracket && a == "]" {
            continue;
        }
        argv.push(a.clone());
    }
    let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    process::run(&refs).unwrap_or(2)
}

pub fn time(args: &[String]) -> i32 {
    if args.len() < 2 {
        return 0;
    }
    let start = time::micros();
    let refs: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    let st = process::run(&refs).unwrap_or(127);
    let us = time::micros() - start;
    eprintln!(
        "\nreal\t{}m{}.{:03}s",
        us / 60_000_000,
        (us / 1_000_000) % 60,
        (us / 1000) % 1000
    );
    st
}

pub fn top(args: &[String]) -> i32 {
    let once = args.iter().any(|a| a == "-n1" || a == "-b");
    loop {
        let si = process::sysinfo();
        print!("\x1b[H\x1b[2J");
        println!(
            "top - up {}s, {} tasks, Mem: {} total, {} free",
            si.uptime,
            si.procs,
            human(si.totalram),
            human(si.freeram)
        );
        println!("\n  PID  PPID S   RSS  THR COMMAND");
        for p in procs() {
            println!(
                "{:>5} {:>5} {} {:>5} {:>4} {}",
                p.pid,
                p.ppid,
                p.state,
                p.rss * 4,
                p.threads,
                p.cmd
            );
        }
        io::flush();
        if once {
            return 0;
        }
        time::sleep_ms(2000);
    }
}

pub fn uname(args: &[String]) -> i32 {
    let (flags, _, _) = parse_flags(args);
    let u = process::uname();
    let all = flags.contains(&'a');
    let mut parts = Vec::new();
    if all || flags.contains(&'s') || flags.is_empty() {
        parts.push(u.sysname.clone());
    }
    if all || flags.contains(&'n') {
        parts.push(u.nodename.clone());
    }
    if all || flags.contains(&'r') {
        parts.push(u.release.clone());
    }
    if all || flags.contains(&'v') {
        parts.push(u.version.clone());
    }
    if all || flags.contains(&'m') {
        parts.push(u.machine.clone());
    }
    println!("{}", parts.join(" "));
    0
}

pub fn uptime(_: &[String]) -> i32 {
    let si = process::sysinfo();
    let (_, _, _, h, m, s) = time::civil(time::now());
    let up = si.uptime as u64;
    println!(
        " {:02}:{:02}:{:02} up {}:{:02}, {} tasks",
        h,
        m,
        s,
        up / 3600,
        (up / 60) % 60,
        si.procs
    );
    0
}

pub fn watch(args: &[String]) -> i32 {
    let mut interval = 2000;
    let mut i = 1;
    if args.get(1).map(|s| s.as_str()) == Some("-n") {
        interval = args.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(2) * 1000;
        i = 3;
    }
    let cmd = args[i..].join(" ");
    loop {
        print!("\x1b[H\x1b[2JEvery {}s: {}\n\n", interval / 1000, cmd);
        io::flush();
        let _ = process::run(&["/bin/sh", "-c", &cmd]);
        time::sleep_ms(interval);
    }
}

pub fn which(args: &[String]) -> i32 {
    let mut st = 0;
    for a in &args[1..] {
        match process::find_in_path(a) {
            Some(p) => println!("{}", p),
            None => st = 1,
        }
    }
    st
}

/// mkfs[.vfat] [-t vfat] [-F 12|16|32] [-n LABEL] DEVICE
pub fn mkfs(args: &[String]) -> i32 {
    let mut label = String::from("NO NAME");
    let mut kind = None;
    let mut dev = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "-L" => {
                label = args.get(i + 1).cloned().unwrap_or(label);
                i += 1;
            }
            "-F" => {
                kind = match args.get(i + 1).map(|s| s.as_str()) {
                    Some("12") => Some(fat_format::FatType::Fat12),
                    Some("16") => Some(fat_format::FatType::Fat16),
                    Some("32") => Some(fat_format::FatType::Fat32),
                    _ => {
                        eprintln!("mkfs: -F must be 12, 16 or 32");
                        return 1;
                    }
                };
                i += 1;
            }
            "-t" => {
                let t = args.get(i + 1).map(|s| s.as_str()).unwrap_or("");
                if !matches!(t, "vfat" | "fat" | "msdos") {
                    eprintln!("mkfs: unsupported filesystem type '{}' (only vfat)", t);
                    return 1;
                }
                i += 1;
            }
            a => dev = Some(a.to_string()),
        }
        i += 1;
    }
    let Some(dev) = dev else {
        eprintln!("usage: mkfs.vfat [-F 12|16|32] [-n LABEL] DEVICE");
        return 1;
    };
    let f = match fs::File::open_with(&dev, fs::O_RDWR, 0) {
        Ok(f) => f,
        Err(e) => return err("mkfs", &dev, e),
    };
    let mut size = 0u64;
    if f.ioctl(0x8008_1272, &mut size as *mut u64 as usize)
        .is_err()
    {
        size = f.metadata().map(|m| m.size).unwrap_or(0);
    }
    let sectors = size / 512;
    let serial = time::now() as u32;
    let opts = fat_format::Options {
        label: &label,
        serial,
        fat_type: kind,
        hidden_sectors: 0,
    };
    match fat_format::format(sectors, &opts, |lba, s| {
        f.write_at(lba * 512, s).map(|_| ())
    }) {
        Ok(l) => {
            let _ = f.sync();
            println!(
                "{}: {}, {} clusters of {} bytes, label {}",
                dev,
                match l.fat_type {
                    fat_format::FatType::Fat12 => "FAT12",
                    fat_format::FatType::Fat16 => "FAT16",
                    fat_format::FatType::Fat32 => "FAT32",
                },
                l.clusters,
                l.sectors_per_cluster * 512,
                label.to_uppercase()
            );
            0
        }
        Err(fat_format::Error::Io(e)) => err("mkfs", &dev, e),
        Err(e) => {
            eprintln!("mkfs: {}: {:?}", dev, e);
            1
        }
    }
}

/// Kernel `struct termios` (the TCGETS/TCSETS layout).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Termios {
    iflag: u32,
    oflag: u32,
    cflag: u32,
    lflag: u32,
    line: u8,
    cc: [u8; 19],
}

const BAUDS: [(u32, u32); 21] = [
    (0, 0o0),
    (50, 0o1),
    (75, 0o2),
    (110, 0o3),
    (134, 0o4),
    (150, 0o5),
    (200, 0o6),
    (300, 0o7),
    (600, 0o10),
    (1200, 0o11),
    (1800, 0o12),
    (2400, 0o13),
    (4800, 0o14),
    (9600, 0o15),
    (19200, 0o16),
    (38400, 0o17),
    (57600, 0o10001),
    (115200, 0o10002),
    (230400, 0o10003),
    (460800, 0o10004),
    (921600, 0o10007),
];
const CBAUD: u32 = 0o10017;

/// stty [-F DEVICE] [SPEED] [raw|cooked|echo|-echo|cs7|cs8|crtscts|-crtscts]
/// Print or change a terminal's line settings.
pub fn stty(args: &[String]) -> i32 {
    const TCGETS: u64 = 0x5401;
    const TCSETS: u64 = 0x5402;
    let mut dev: Option<String> = None;
    let mut ops = Vec::new();
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        if a == "-F" {
            dev = it.next().cloned();
        } else if let Some(d) = a.strip_prefix("-F") {
            dev = Some(d.to_string());
        } else {
            ops.push(a.clone());
        }
    }
    let file = match &dev {
        Some(d) => match fs::File::open_with(d, fs::O_RDWR | fs::O_NONBLOCK, 0) {
            Ok(f) => Some(f),
            Err(e) => return err("stty", d, e),
        },
        None => None,
    };
    let fd = file.as_ref().map_or(0, |f| f.fd());
    // The descriptor stays owned by `file` (or is stdin).
    let ioctl = |cmd: u64, arg: usize| {
        let f = fs::File::from_raw(fd);
        let r = f.ioctl(cmd, arg);
        f.into_raw();
        r
    };
    let mut t = Termios::default();
    if let Err(e) = ioctl(TCGETS, &mut t as *mut Termios as usize) {
        return err("stty", dev.as_deref().unwrap_or("standard input"), e);
    }
    if ops.is_empty() {
        let speed = BAUDS
            .iter()
            .find(|(_, c)| *c == t.cflag & CBAUD)
            .map_or(0, |(b, _)| *b);
        let bits = 5 + ((t.cflag >> 4) & 3);
        println!(
            "speed {} baud; cs{}{}{}{}",
            speed,
            bits,
            if t.lflag & 0o10 != 0 {
                " echo"
            } else {
                " -echo"
            },
            if t.lflag & 0o2 != 0 {
                " icanon"
            } else {
                " -icanon"
            },
            if t.cflag & 0o20000000000 != 0 {
                " crtscts"
            } else {
                ""
            }
        );
        return 0;
    }
    for op in &ops {
        if let Ok(speed) = op.parse::<u32>() {
            match BAUDS.iter().find(|(b, _)| *b == speed) {
                Some((_, c)) => t.cflag = (t.cflag & !CBAUD) | c,
                None => {
                    eprintln!("stty: unsupported speed {}", speed);
                    return 1;
                }
            }
            continue;
        }
        match op.as_str() {
            "raw" => {
                t.iflag = 0;
                t.oflag = 0;
                t.lflag &= !(0o2 | 0o10 | 0o1 | 0o100000);
                t.cc[6] = 1; // VMIN
                t.cc[5] = 0; // VTIME
            }
            "cooked" | "sane" => {
                t.iflag = 0o400; // ICRNL
                t.oflag = 0o5; // OPOST | ONLCR
                t.lflag |= 0o2 | 0o10 | 0o1 | 0o20 | 0o40;
            }
            "echo" => t.lflag |= 0o10,
            "-echo" => t.lflag &= !0o10,
            "cs7" => t.cflag = (t.cflag & !0o60) | 0o40,
            "cs8" => t.cflag |= 0o60,
            "crtscts" => t.cflag |= 0o20000000000,
            "-crtscts" => t.cflag &= !0o20000000000,
            _ => {
                eprintln!("stty: unknown setting '{}'", op);
                return 1;
            }
        }
    }
    match ioctl(TCSETS, &t as *const Termios as usize) {
        Ok(_) => 0,
        Err(e) => err("stty", dev.as_deref().unwrap_or("standard input"), e),
    }
}
