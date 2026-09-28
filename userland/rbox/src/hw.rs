//! Hardware bring-up helpers.
//!
//! * `hwcheck` walks through the checklist in docs/HARDWARE.md (system
//!   inventory, wired Ethernet, Wi-Fi, storage, USB hot-plug) and records
//!   PASS/FAIL/SKIP with the full command output of every step in a
//!   directory on the storage partition, ready to be copied off the
//!   machine.
//! * `bugreport` collects the kernel log and every relevant `/proc` and
//!   `/sys` file into one text file.

use crate::hash::Sha256;
use rustos_rt::prelude::*;
use rustos_rt::{fs, io, process, time};

fn stamp() -> String {
    let (y, mo, d, h, mi, s) = time::civil(time::now());
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, mo, d, h, mi, s)
}

/// Directory for reports: the storage partition if mounted, else /tmp.
fn report_base() -> &'static str {
    if fs::is_dir("/storage") && fs::statfs("/storage").is_ok() {
        "/storage"
    } else {
        "/tmp"
    }
}

/// Run a shell command line, capturing stdout and stderr together.
fn sh(cmd: &str) -> (i32, String) {
    let line = format!("{} 2>&1", cmd);
    match process::output(&["sh", "-c", &line]) {
        Ok((code, out)) => (code, String::from_utf8_lossy(&out).into_owned()),
        Err(e) => (127, format!("cannot run sh: {:?}\n", e)),
    }
}

fn read_trim(path: &str) -> String {
    fs::read_to_string(path)
        .map(|s| String::from(s.trim()))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// bugreport
// ---------------------------------------------------------------------------

const REPORT_FILES: &[&str] = &[
    "/proc/version",
    "/proc/cmdline",
    "/proc/uptime",
    "/proc/cpuinfo",
    "/proc/meminfo",
    "/proc/interrupts",
    "/proc/threads",
    "/proc/mounts",
    "/proc/bus/pci/devices",
    "/proc/bus/pci/config",
    "/proc/bus/usb/devices",
    "/proc/net/dev",
    "/proc/net/if_addrs",
    "/proc/net/route",
    "/proc/net/arp",
    "/proc/net/tcp",
    "/proc/net/udp",
    "/proc/net/wireless",
    "/etc/resolv.conf",
    "/storage/etc/network.conf",
    "/storage/etc/kernel.conf",
];

const REPORT_COMMANDS: &[&str] = &[
    "date",
    "lsblk",
    "df",
    "ip addr",
    "ip route",
    "wifi status",
    "ls -l /storage/lib/firmware /lib/firmware",
];

/// Build the report text.
fn collect_report() -> String {
    let mut r = String::new();
    let u = process::uname();
    r.push_str(&format!(
        "RustOS bug report {}\n{} {} {} {}\n",
        stamp(),
        u.sysname,
        u.release,
        u.version,
        u.machine
    ));
    for f in REPORT_FILES {
        if let Ok(s) = fs::read_to_string(f) {
            r.push_str(&format!("\n===== {} =====\n{}", f, s));
        }
    }
    for c in REPORT_COMMANDS {
        let (code, out) = sh(c);
        r.push_str(&format!("\n===== $ {} (exit {}) =====\n{}", c, code, out));
    }
    // Every network interface attribute.
    if let Ok(ifs) = fs::read_dir("/sys/class/net") {
        for i in ifs {
            let dir = format!("/sys/class/net/{}", i.name);
            r.push_str(&format!("\n===== {} =====\n", dir));
            for attr in [
                "address",
                "operstate",
                "carrier",
                "mtu",
                "speed",
                "type",
                "driver",
                "statistics/rx_packets",
                "statistics/tx_packets",
                "statistics/rx_errors",
                "statistics/tx_errors",
                "wireless/status",
            ] {
                let p = format!("{}/{}", dir, attr);
                if fs::exists(&p) {
                    r.push_str(&format!("{}: {}\n", attr, read_trim(&p)));
                }
            }
        }
    }
    let (_, dmesg) = sh("dmesg");
    r.push_str(&format!("\n===== dmesg =====\n{}", dmesg));
    if let Ok(log) = fs::read_to_string("/storage/log/kernel.log") {
        // The persistent log may hold previous boots: include its tail.
        let tail = &log[log.len().saturating_sub(64 * 1024)..];
        r.push_str(&format!(
            "\n===== /storage/log/kernel.log (last 64 KiB) =====\n{}",
            tail
        ));
    }
    r
}

pub fn bugreport(args: &[String]) -> i32 {
    let mut out = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                out = args.get(i + 1).cloned();
                i += 1;
            }
            "-" => out = Some(String::from("-")),
            "-h" | "--help" => {
                println!("usage: bugreport [-o FILE | -]");
                println!("Collects dmesg, /proc, /sys/class/net and tool output into one file.");
                return 0;
            }
            a => {
                eprintln!("bugreport: unknown option {}", a);
                return 2;
            }
        }
        i += 1;
    }
    let text = collect_report();
    let path = out.unwrap_or_else(|| format!("{}/bugreport-{}.txt", report_base(), stamp()));
    if path == "-" {
        print!("{}", text);
        return 0;
    }
    match fs::write(&path, text.as_bytes()) {
        Ok(()) => {
            fs::sync();
            println!("bug report written to {} ({} bytes)", path, text.len());
            0
        }
        Err(e) => {
            eprintln!("bugreport: {}: {:?}", path, e);
            1
        }
    }
}

// ---------------------------------------------------------------------------
// hwcheck
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Res {
    Pass,
    Fail,
    Skip,
}

struct Check {
    dir: String,
    batch: bool,
    results: Vec<(String, Res, String)>,
    n: usize,
}

struct Opts {
    batch: bool,
    out: Option<String>,
    http: String,
    https: String,
    dns: String,
    big: Option<String>,
    ssid: Option<String>,
    pass: Option<String>,
    open_ssid: Option<String>,
    rekey_secs: u64,
    sections: Vec<String>,
}

impl Check {
    fn file_name(&self, name: &str) -> String {
        let slug: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        format!("{}/{:02}-{}.log", self.dir, self.n, slug)
    }

    fn record(&mut self, name: &str, res: Res, note: &str, log: &str) -> Res {
        self.n += 1;
        let _ = fs::write(&self.file_name(name), log.as_bytes());
        let tag = match res {
            Res::Pass => "\x1b[32mPASS\x1b[0m",
            Res::Fail => "\x1b[31mFAIL\x1b[0m",
            Res::Skip => "\x1b[33mSKIP\x1b[0m",
        };
        if note.is_empty() {
            println!("  [{}] {}", tag, name);
        } else {
            println!("  [{}] {} — {}", tag, name, note);
        }
        self.results
            .push((String::from(name), res, String::from(note)));
        res
    }

    /// Run `cmd`; PASS if it exits 0 and `ok` accepts the output.
    fn step(&mut self, name: &str, cmd: &str, ok: impl Fn(&str) -> bool) -> Res {
        let t0 = time::millis();
        let (code, out) = sh(cmd);
        let ms = time::millis() - t0;
        let log = format!("$ {}\n{}\n(exit {}, {} ms)\n", cmd, out, code, ms);
        let pass = code == 0 && ok(&out);
        let note = if pass {
            String::new()
        } else {
            let last = out
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            format!("exit {}: {}", code, last.trim())
        };
        self.record(name, if pass { Res::Pass } else { Res::Fail }, &note, &log)
    }

    fn info(&mut self, name: &str, cmd: &str) {
        let (code, out) = sh(cmd);
        self.n += 1;
        let _ = fs::write(
            &self.file_name(name),
            format!("$ {}\n{}\n(exit {})\n", cmd, out, code).as_bytes(),
        );
    }

    fn skip(&mut self, name: &str, why: &str) {
        self.record(name, Res::Skip, why, why);
    }

    fn ask(&self, prompt: &str) -> Option<String> {
        if self.batch {
            return None;
        }
        print!("{}", prompt);
        io::flush();
        io::read_line().map(|l| String::from(l.trim()))
    }

    fn section(&self, title: &str) {
        println!("\n\x1b[1m== {} ==\x1b[0m", title);
    }
}

fn ifaces() -> (Vec<String>, Vec<String>) {
    let mut eth = Vec::new();
    let mut wlan = Vec::new();
    for e in fs::read_dir("/sys/class/net").unwrap_or_default() {
        if e.name == "lo" {
            continue;
        }
        if fs::exists(&format!("/sys/class/net/{}/wireless", e.name)) {
            wlan.push(e.name.clone());
        } else {
            eth.push(e.name.clone());
        }
    }
    eth.sort();
    wlan.sort();
    (eth, wlan)
}

fn default_gateway() -> Option<String> {
    let (_, out) = sh("ip route");
    out.lines()
        .find(|l| l.starts_with("default"))
        .and_then(|l| l.split_whitespace().nth(1).map(String::from))
}

/// Connectivity steps shared by Ethernet and Wi-Fi.
fn net_steps(c: &mut Check, o: &Opts, iface: &str) {
    let dhcp = c.step(
        &format!("{} DHCP lease", iface),
        &format!("dhcp {} -t 30", iface),
        |out| out.contains("leased"),
    );
    if dhcp != Res::Pass {
        c.skip(&format!("{} ping gateway", iface), "no lease");
        return;
    }
    match default_gateway() {
        Some(gw) => {
            c.step(
                &format!("{} ping gateway {}", iface, gw),
                &format!("ping -c 3 -i 0.3 {}", gw),
                |out| !out.contains(" 0 received"),
            );
        }
        None => c.skip(&format!("{} ping gateway", iface), "no default route"),
    }
    if o.dns.is_empty() {
        c.skip(&format!("{} DNS", iface), "disabled");
    } else {
        c.step(
            &format!("{} DNS {}", iface, o.dns),
            &format!("nslookup {}", o.dns),
            |out| out.contains("Address"),
        );
    }
    c.step(
        &format!("{} HTTP {}", iface, o.http),
        &format!("wget -q -T 20 -O /dev/null {}", o.http),
        |_| true,
    );
    if o.https.is_empty() {
        c.skip(&format!("{} HTTPS", iface), "disabled");
    } else {
        c.step(
            &format!("{} HTTPS {}", iface, o.https),
            &format!("wget -q -T 20 -O /dev/null {}", o.https),
            |_| true,
        );
    }
    if let Some(big) = &o.big {
        let name = format!("{} throughput {}", iface, big);
        let t0 = time::millis();
        let (code, out) = sh(&format!("wget -q -T 60 -O /tmp/hwcheck.bin {}", big));
        let ms = (time::millis() - t0).max(1);
        let size = fs::metadata("/tmp/hwcheck.bin")
            .map(|m| m.size)
            .unwrap_or(0);
        let _ = fs::remove_file("/tmp/hwcheck.bin");
        let kbps = size * 8 / ms;
        let note = format!(
            "{} bytes in {} ms = {}.{:03} Mbit/s",
            size,
            ms,
            kbps / 1000,
            kbps % 1000
        );
        let res = if code == 0 && size > 0 {
            Res::Pass
        } else {
            Res::Fail
        };
        c.record(&name, res, &note, &format!("{}\n{}\n", out, note));
    }
}

fn wifi_state(iface: &str) -> String {
    let st = read_trim(&format!("/sys/class/net/{}/wireless/status", iface));
    st.split_whitespace()
        .find_map(|kv| kv.strip_prefix("state=").map(String::from))
        .unwrap_or(st)
}

fn wifi_section(c: &mut Check, o: &Opts, iface: &str) {
    c.info(
        &format!("{} status", iface),
        &format!("wifi -i {} status", iface),
    );
    let fw = c.step(
        &format!("{} firmware loaded", iface),
        &format!("wifi -i {} status", iface),
        |out| !out.contains("no-firmware") && !out.contains("state=failed"),
    );
    if fw != Res::Pass {
        println!("    install the firmware into /storage/lib/firmware (docs/WIFI.md)");
        return;
    }
    c.step(
        &format!("{} scan", iface),
        &format!("wifi -i {} scan", iface),
        |out| out.lines().count() >= 2,
    );
    let open = o.open_ssid.clone().or_else(|| {
        c.ask("  SSID of an OPEN network to test (Enter to skip): ")
            .filter(|s| !s.is_empty())
    });
    match open {
        Some(ssid) => {
            if c.step(
                &format!("{} connect open '{}'", iface, ssid),
                &format!("wifi -i {} connect '{}'", iface, ssid),
                |out| out.contains("connected"),
            ) == Res::Pass
            {
                net_steps(c, o, iface);
                if fs::exists("/bin/netcheck") {
                    c.step(
                        &format!("{} captive-portal check", iface),
                        "netcheck",
                        |_| true,
                    );
                }
            }
        }
        None => c.skip(&format!("{} open network", iface), "no SSID given"),
    }
    let ssid = o.ssid.clone().or_else(|| {
        c.ask("  SSID of a WPA2/WPA3 network to test (Enter to skip): ")
            .filter(|s| !s.is_empty())
    });
    let Some(ssid) = ssid else {
        c.skip(&format!("{} WPA network", iface), "no SSID given");
        return;
    };
    let pass = o
        .pass
        .clone()
        .or_else(|| c.ask("  passphrase: "))
        .unwrap_or_default();
    let r = c.step(
        &format!("{} connect '{}'", iface, ssid),
        &format!("wifi -i {} connect '{}' '{}'", iface, ssid, pass),
        |out| out.contains("connected"),
    );
    c.info(
        &format!("{} link after connect", iface),
        &format!("wifi -i {} status", iface),
    );
    if r != Res::Pass {
        return;
    }
    net_steps(c, o, iface);
    if o.rekey_secs > 0 {
        println!("    waiting {} s for a group-key refresh...", o.rekey_secs);
        time::sleep_ms(o.rekey_secs * 1000);
        let st = wifi_state(iface);
        let ok = st == "connected";
        c.record(
            &format!("{} still connected after {} s", iface, o.rekey_secs),
            if ok { Res::Pass } else { Res::Fail },
            &format!("state={}", st),
            &st,
        );
    }
    c.step(
        &format!("{} disconnect", iface),
        &format!("wifi -i {} disconnect", iface),
        |_| true,
    );
    c.step(
        &format!("{} reconnect", iface),
        &format!("wifi -i {} connect '{}' '{}'", iface, ssid, pass),
        |out| out.contains("connected"),
    );
    c.step(
        &format!("{} DHCP after reconnect", iface),
        &format!("dhcp {} -t 30", iface),
        |out| out.contains("leased"),
    );
}

/// Writable filesystems worth testing: /storage and anything under /mnt.
fn test_mounts() -> Vec<String> {
    let mut v = Vec::new();
    for line in fs::read_to_string("/proc/mounts")
        .unwrap_or_default()
        .lines()
    {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let rw = f[3].split(',').any(|o| o == "rw");
        if rw && (f[1] == "/storage" || f[1].starts_with("/mnt/")) {
            v.push(String::from(f[1]));
        }
    }
    // The storage partition first, then the other disks.
    v.sort_by_key(|m| (m != "/storage", m.clone()));
    v
}

fn storage_rw(c: &mut Check, mnt: &str) {
    let name = format!("read/write {}", mnt);
    let path = format!("{}/.hwcheck.tmp", mnt);
    let mut data = vec![0u8; 8 << 20];
    let mut x = time::micros() | 1;
    for b in data.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    let mut h = Sha256::new();
    h.update(&data);
    let want = h.finish();
    let t0 = time::millis();
    if let Err(e) = fs::write(&path, &data) {
        c.record(&name, Res::Fail, &format!("write: {:?}", e), "");
        return;
    }
    fs::sync();
    let wms = (time::millis() - t0).max(1);
    let t1 = time::millis();
    let back = fs::read(&path).unwrap_or_default();
    let rms = (time::millis() - t1).max(1);
    let _ = fs::remove_file(&path);
    let mut h = Sha256::new();
    h.update(&back);
    let ok = back.len() == data.len() && h.finish() == want;
    let note = format!(
        "8 MiB, write+sync {} KiB/s, read {} KiB/s",
        8192 * 1000 / wms,
        8192 * 1000 / rms
    );
    c.record(&name, if ok { Res::Pass } else { Res::Fail }, &note, &note);
}

fn block_devices() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir("/dev")
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n.starts_with("sd") && !n.chars().last().is_some_and(|c| c.is_ascii_digit()))
        .collect();
    v.sort();
    v
}

fn wait_for(secs: u64, f: impl Fn() -> bool) -> bool {
    let end = time::millis() + secs * 1000;
    while time::millis() < end {
        if f() {
            return true;
        }
        time::sleep_ms(250);
    }
    false
}

fn hotplug(c: &mut Check) {
    if c.batch {
        c.skip("USB stick hot-plug", "interactive only");
        return;
    }
    let before = block_devices();
    if c.ask("  Insert a USB stick and press Enter (or type 's' to skip): ")
        .is_some_and(|a| a.starts_with('s'))
    {
        c.skip("USB stick hot-plug", "skipped");
        return;
    }
    let found = wait_for(20, || block_devices().len() > before.len());
    if !found {
        c.record(
            "USB stick detected",
            Res::Fail,
            "no new /dev/sd* within 20 s",
            "",
        );
        return;
    }
    let (_, lsblk) = sh("lsblk");
    c.record("USB stick detected", Res::Pass, "", &lsblk);
    let _ = c.ask("  Remove the stick and press Enter: ");
    let gone = wait_for(20, || block_devices().len() <= before.len());
    c.record(
        "USB stick removed",
        if gone { Res::Pass } else { Res::Fail },
        "",
        "",
    );
}

fn usage() {
    println!("usage: hwcheck [options] [section...]");
    println!("sections: system ethernet wifi storage usb (default: all)");
    println!("  -y, --batch        never prompt (skip interactive steps)");
    println!("  -o DIR             result directory (default /storage/hwcheck-DATE)");
    println!("  --ssid S --pass P  WPA2/WPA3 network for the Wi-Fi steps");
    println!("  --open S           open network for the Wi-Fi steps");
    println!("  --http URL         HTTP test URL (default http://example.com/)");
    println!("  --https URL        HTTPS test URL ('' to skip)");
    println!("  --dns NAME         name to resolve (default example.com, '' to skip)");
    println!("  --big URL          large file for a throughput test");
    println!("  --rekey SECS       stay connected SECS seconds (group-key refresh)");
}

pub fn hwcheck(args: &[String]) -> i32 {
    let mut o = Opts {
        batch: !io::isatty(0),
        out: None,
        http: String::from("http://example.com/"),
        https: String::from("https://example.com/"),
        dns: String::from("example.com"),
        big: None,
        ssid: None,
        pass: None,
        open_ssid: None,
        rekey_secs: 0,
        sections: Vec::new(),
    };
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        let mut val = || {
            i += 1;
            args.get(i).cloned().unwrap_or_default()
        };
        match a {
            "-y" | "--batch" => o.batch = true,
            "-o" => o.out = Some(val()),
            "--http" => o.http = val(),
            "--https" => o.https = val(),
            "--dns" => o.dns = val(),
            "--big" => o.big = Some(val()),
            "--ssid" => o.ssid = Some(val()),
            "--pass" => o.pass = Some(val()),
            "--open" => o.open_ssid = Some(val()),
            "--rekey" => o.rekey_secs = val().parse().unwrap_or(0),
            "-h" | "--help" => {
                usage();
                return 0;
            }
            s if !s.starts_with('-') => o.sections.push(String::from(s)),
            s => {
                eprintln!("hwcheck: unknown option {}", s);
                usage();
                return 2;
            }
        }
        i += 1;
    }
    let want = |s: &str| o.sections.is_empty() || o.sections.iter().any(|x| x == s);
    let dir = o
        .out
        .clone()
        .unwrap_or_else(|| format!("{}/hwcheck-{}", report_base(), stamp()));
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("hwcheck: {}: {:?}", dir, e);
        return 1;
    }
    let mut c = Check {
        dir: dir.clone(),
        batch: o.batch,
        results: Vec::new(),
        n: 0,
    };
    println!("RustOS hardware check — results in {}", dir);
    let (eth, wlan) = ifaces();

    if want("system") {
        c.section("System");
        for (name, cmd) in [
            ("version", "cat /proc/version /proc/cmdline"),
            ("cpu", "cat /proc/cpuinfo"),
            ("memory", "cat /proc/meminfo"),
            ("pci", "lspci"),
            ("pci config", "cat /proc/bus/pci/config"),
            ("usb", "lsusb"),
            ("block devices", "lsblk"),
            ("interfaces", "ip addr"),
            ("interrupts", "cat /proc/interrupts"),
        ] {
            c.info(name, cmd);
        }
        println!(
            "  inventory saved ({} network interfaces: {:?} {:?})",
            eth.len() + wlan.len(),
            eth,
            wlan
        );
    }
    if want("ethernet") {
        c.section("Wired Ethernet");
        if eth.is_empty() {
            c.skip("Ethernet", "no wired interface found");
        }
        for e in &eth {
            let drv = read_trim(&format!("/sys/class/net/{}/driver", e));
            let carrier = read_trim(&format!("/sys/class/net/{}/carrier", e)) == "1";
            let speed = read_trim(&format!("/sys/class/net/{}/speed", e));
            let note = format!("driver {}, {} Mbit/s", drv, speed);
            c.record(
                &format!("{} link up", e),
                if carrier { Res::Pass } else { Res::Fail },
                &note,
                &note,
            );
            if carrier {
                net_steps(&mut c, &o, e);
            }
        }
    }
    if want("wifi") {
        c.section("Wi-Fi");
        if wlan.is_empty() {
            c.skip("Wi-Fi", "no wireless interface found");
        }
        for w in &wlan {
            wifi_section(&mut c, &o, w);
        }
    }
    if want("storage") {
        c.section("Storage");
        c.info("mounts", "cat /proc/mounts; df");
        let mounts = test_mounts();
        if mounts.is_empty() {
            c.skip("storage read/write", "no writable /storage or /mnt/*");
        }
        for m in mounts {
            storage_rw(&mut c, &m);
        }
    }
    if want("usb") {
        c.section("USB");
        hotplug(&mut c);
    }

    let count = |r: Res| c.results.iter().filter(|x| x.1 == r).count();
    let (p, f, s) = (count(Res::Pass), count(Res::Fail), count(Res::Skip));
    let mut summary = format!(
        "hwcheck {}: {} passed, {} failed, {} skipped\n",
        stamp(),
        p,
        f,
        s
    );
    for (name, r, note) in &c.results {
        let tag = match r {
            Res::Pass => "PASS",
            Res::Fail => "FAIL",
            Res::Skip => "SKIP",
        };
        summary.push_str(&format!(
            "{} {}{}{}\n",
            tag,
            name,
            if note.is_empty() { "" } else { " — " },
            note
        ));
    }
    let _ = fs::write(&format!("{}/summary.txt", dir), summary.as_bytes());
    let _ = fs::write(
        &format!("{}/bugreport.txt", dir),
        collect_report().as_bytes(),
    );
    fs::sync();
    println!(
        "\n{} passed, {} failed, {} skipped — summary in {}/summary.txt",
        p, f, s, dir
    );
    if f > 0 { 1 } else { 0 }
}
