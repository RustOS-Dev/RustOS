//! System applets.

use crate::{err, human, parse_flags};
use rustos_rt::prelude::*;
use rustos_rt::{env, fs, io, process, signal, time};

pub fn clear(_: &[String]) -> i32 {
    print!("\x1b[H\x1b[2J");
    0
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
                i += 2;
                continue;
            }
            a => ops.push(a.to_string()),
        }
        i += 1;
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
