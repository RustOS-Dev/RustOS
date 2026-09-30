//! bt: Bluetooth control through /dev/bluetooth. The kernel runs the
//! command and streams its output; lines starting with "? " ask for an
//! answer (a passkey, yes/no), which is read from standard input and
//! written back to the device.

use crate::err;
use rustos_rt::fs;
use rustos_rt::prelude::*;

const DEV: &str = "/dev/bluetooth";

pub fn bt(args: &[String]) -> i32 {
    let words: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    if words.first().is_some_and(|w| *w == "-h" || *w == "--help") {
        println!("usage: bt [status | power on|off | scan [SECONDS] | devices | pair ADDR |");
        println!("          connect ADDR | disconnect ADDR | remove ADDR | list | attach PORT]");
        return 0;
    }
    let line = if words.is_empty() {
        String::from("status")
    } else {
        words.join(" ")
    };
    let f = match fs::File::open_with(DEV, fs::O_RDWR, 0) {
        Ok(f) => f,
        Err(e) => return err("bt", DEV, e),
    };
    if let Err(e) = f.write(line.as_bytes()) {
        return err("bt", DEV, e);
    }
    let mut status = 0;
    let mut pending = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return err("bt", DEV, e),
        };
        pending.extend_from_slice(&buf[..n]);
        while let Some(i) = pending.iter().position(|&b| b == b'\n') {
            let l: Vec<u8> = pending.drain(..=i).collect();
            let l = String::from_utf8_lossy(&l[..l.len() - 1]).into_owned();
            if let Some(q) = l.strip_prefix("? ") {
                print!("{} ", q);
                rustos_rt::io::flush();
                let answer = rustos_rt::io::read_line().unwrap_or_default();
                let _ = f.write(answer.trim().as_bytes());
            } else {
                if l.starts_with("error: ") {
                    status = 1;
                }
                println!("{}", l);
            }
        }
    }
    status
}
