//! evtest: print events from /dev/input/event* (or /dev/input/js*), or
//! describe devices through the EVIOCG*/JSIOCG* ioctls.

use crate::err;
use rustos_rt::fs;
use rustos_rt::prelude::*;
use rustos_rt::sys::{check, syscall};

const SYS_IOCTL: usize = 16;

/// _IOC(read, type, nr, size).
fn ior(ty: u8, nr: u8, size: usize) -> usize {
    (2 << 30) | (size << 16) | ((ty as usize) << 8) | nr as usize
}

fn ioctl_buf(f: &fs::File, cmd: usize, buf: &mut [u8]) -> Option<usize> {
    check(syscall(
        SYS_IOCTL,
        &[f.fd() as usize, cmd, buf.as_mut_ptr() as usize],
    ))
    .ok()
    .map(|n| n as usize)
}

fn bits(b: &[u8]) -> Vec<u16> {
    (0..b.len() * 8)
        .filter(|i| b[i / 8] & (1 << (i % 8)) != 0)
        .map(|i| i as u16)
        .collect()
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// evtest -i: what the device is and what it reports.
fn info(dev: &str, f: &fs::File) -> i32 {
    if dev.contains("/js") {
        let mut b = [0u8; 128];
        let (mut axes, mut btns, mut ver) = ([0u8], [0u8], [0u8; 4]);
        ioctl_buf(f, ior(b'j', 0x01, 4), &mut ver);
        ioctl_buf(f, ior(b'j', 0x11, 1), &mut axes);
        ioctl_buf(f, ior(b'j', 0x12, 1), &mut btns);
        ioctl_buf(f, ior(b'j', 0x13, b.len()), &mut b);
        println!("Joystick driver version is {:#x}", u32::from_le_bytes(ver));
        println!("Joystick name: \"{}\"", cstr(&b));
        println!("Axes: {}  Buttons: {}", axes[0], btns[0]);
        return 0;
    }
    let mut v = [0u8; 4];
    if ioctl_buf(f, ior(b'E', 0x01, 4), &mut v).is_none() {
        eprintln!("evtest: {}: not an evdev device", dev);
        return 1;
    }
    let v = u32::from_le_bytes(v);
    println!(
        "Input driver version is {}.{}.{}",
        v >> 16,
        (v >> 8) & 0xFF,
        v & 0xFF
    );
    let mut id = [0u8; 8];
    ioctl_buf(f, ior(b'E', 0x02, 8), &mut id);
    let w = |i: usize| u16::from_le_bytes([id[i * 2], id[i * 2 + 1]]);
    println!(
        "Input device ID: bus {:#x} vendor {:#x} product {:#x} version {:#x}",
        w(0),
        w(1),
        w(2),
        w(3)
    );
    let mut name = [0u8; 256];
    ioctl_buf(f, ior(b'E', 0x06, name.len()), &mut name);
    println!("Input device name: \"{}\"", cstr(&name));
    let mut phys = [0u8; 256];
    if ioctl_buf(f, ior(b'E', 0x07, phys.len()), &mut phys).is_some_and(|n| n > 1) {
        println!("Input device phys: \"{}\"", cstr(&phys));
    }
    println!("Supported events:");
    let mut evb = [0u8; 4];
    ioctl_buf(f, ior(b'E', 0x20, 4), &mut evb);
    for t in bits(&evb) {
        let mut cb = [0u8; 96];
        let n = ioctl_buf(f, ior(b'E', 0x20 + t as u8, cb.len()), &mut cb).unwrap_or(0);
        let codes = bits(&cb[..n]);
        let shown: Vec<String> = codes.iter().take(12).map(|c| format!("{:#x}", c)).collect();
        let more = if codes.len() > 12 { " ..." } else { "" };
        println!(
            "  Event type {} ({}): {} codes {}{}",
            t,
            ev_type(t),
            codes.len(),
            shown.join(" "),
            more
        );
        if t == 3 {
            for c in codes {
                let mut a = [0u8; 24];
                ioctl_buf(f, ior(b'E', 0x40 + c as u8, 24), &mut a);
                let g = |i: usize| i32::from_le_bytes(a[i * 4..i * 4 + 4].try_into().unwrap());
                println!(
                    "    Axis {:#x}: value {} min {} max {} fuzz {} flat {} resolution {}",
                    c,
                    g(0),
                    g(1),
                    g(2),
                    g(3),
                    g(4),
                    g(5)
                );
            }
        }
    }
    let mut led = [0u8; 8];
    if ioctl_buf(f, ior(b'E', 0x19, led.len()), &mut led).is_some() {
        let on: Vec<String> = bits(&led).iter().map(|l| format!("{}", l)).collect();
        if !on.is_empty() {
            println!("LEDs on: {}", on.join(" "));
        }
    }
    0
}

/// evtest -l: the event devices and their names.
fn list() -> i32 {
    let mut names: Vec<String> = fs::read_dir("/dev/input")
        .map(|d| {
            d.into_iter()
                .map(|e| e.name)
                .filter(|n| n.starts_with("event"))
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|n| n[5..].parse::<u32>().unwrap_or(0));
    for n in names {
        let path = format!("/dev/input/{}", n);
        let Ok(f) = fs::File::open(&path) else {
            continue;
        };
        let mut name = [0u8; 256];
        ioctl_buf(&f, ior(b'E', 0x06, name.len()), &mut name);
        println!("{}:\t{}", path, cstr(&name));
    }
    0
}

fn ev_type(t: u16) -> &'static str {
    match t {
        0 => "SYN",
        1 => "KEY",
        2 => "REL",
        3 => "ABS",
        4 => "MSC",
        0x11 => "LED",
        0x14 => "REP",
        _ => "?",
    }
}

/// evtest [-c COUNT] [DEVICE]: print COUNT events (default: forever);
/// evtest -i [DEVICE]: describe the device; evtest -l: list devices.
pub fn evtest(args: &[String]) -> i32 {
    let mut count: Option<usize> = None;
    let mut dev = String::from("/dev/input/event0");
    let mut describe = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-c" => {
                i += 1;
                count = args.get(i).and_then(|c| c.parse().ok());
            }
            "-i" => describe = true,
            "-l" => return list(),
            a => dev = a.to_string(),
        }
        i += 1;
    }
    let f = match fs::File::open(&dev) {
        Ok(f) => f,
        Err(e) => return err("evtest", &dev, e),
    };
    if describe {
        return info(&dev, &f);
    }
    let js = dev.contains("/js");
    let size = if js { 8 } else { 24 };
    let mut buf = vec![0u8; size * 64];
    let mut seen = 0usize;
    loop {
        let n = match f.read(&mut buf) {
            Ok(n) => n,
            Err(e) => return err("evtest", "read", e),
        };
        for ev in buf[..n].chunks_exact(size) {
            if js {
                let value = i16::from_le_bytes([ev[4], ev[5]]);
                let kind = if ev[6] & 0x7F == 1 { "button" } else { "axis" };
                println!("js {} {} value {}", kind, ev[7], value);
            } else {
                let t = u16::from_le_bytes([ev[16], ev[17]]);
                let code = u16::from_le_bytes([ev[18], ev[19]]);
                let value = i32::from_le_bytes([ev[20], ev[21], ev[22], ev[23]]);
                if t == 0 {
                    continue;
                }
                println!("event {} code {:#x} value {}", ev_type(t), code, value);
            }
            let _ = rustos_rt::io::flush();
            seen += 1;
            if count.is_some_and(|c| seen >= c) {
                return 0;
            }
        }
    }
}
