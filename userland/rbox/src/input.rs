//! evtest: print events from /dev/input/event* (or /dev/input/js*).

use crate::err;
use rustos_rt::fs;
use rustos_rt::prelude::*;

fn ev_type(t: u16) -> &'static str {
    match t {
        0 => "SYN",
        1 => "KEY",
        2 => "REL",
        3 => "ABS",
        _ => "?",
    }
}

/// evtest [-c COUNT] [DEVICE]: print COUNT events (default: forever).
pub fn evtest(args: &[String]) -> i32 {
    let mut count: Option<usize> = None;
    let mut dev = String::from("/dev/input/event0");
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-c" => {
                i += 1;
                count = args.get(i).and_then(|c| c.parse().ok());
            }
            a => dev = a.to_string(),
        }
        i += 1;
    }
    let f = match fs::File::open(&dev) {
        Ok(f) => f,
        Err(e) => return err("evtest", &dev, e),
    };
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
