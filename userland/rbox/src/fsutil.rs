//! truncate, fallocate.

use crate::err;
use rustos_rt::fs;
use rustos_rt::prelude::*;
use rustos_rt::sys::{check, syscall};

const SYS_FTRUNCATE: usize = 77;
const SYS_FALLOCATE: usize = 285;
const O_WRONLY: u32 = 1;
const O_CREAT: u32 = 0o100;

/// Parse a size with an optional K/M/G suffix (powers of 1024).
fn parse_size(s: &str) -> Option<u64> {
    let (num, mul) = match s.as_bytes().last()? {
        b'K' | b'k' => (&s[..s.len() - 1], 1u64 << 10),
        b'M' | b'm' => (&s[..s.len() - 1], 1 << 20),
        b'G' | b'g' => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<u64>().ok()?.checked_mul(mul)
}

fn open_for_write(path: &str) -> rustos_rt::Result<fs::File> {
    fs::File::open_with(path, O_WRONLY | O_CREAT, 0o644)
}

/// truncate -s SIZE FILE...
pub fn truncate(args: &[String]) -> i32 {
    let mut size = None;
    let mut files = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-s" => {
                i += 1;
                size = args.get(i).and_then(|s| parse_size(s));
            }
            a => files.push(a.to_string()),
        }
        i += 1;
    }
    let (Some(size), false) = (size, files.is_empty()) else {
        eprintln!("usage: truncate -s SIZE FILE...");
        return 2;
    };
    let mut rc = 0;
    for f in &files {
        let r = open_for_write(f).and_then(|file| {
            check(syscall(SYS_FTRUNCATE, &[file.fd() as usize, size as usize])).map(|_| ())
        });
        if let Err(e) = r {
            rc = err("truncate", f, e);
        }
    }
    rc
}

/// fallocate [-n] [-o OFFSET] -l LENGTH FILE
pub fn fallocate(args: &[String]) -> i32 {
    let (mut off, mut len, mut keep) = (0u64, None, false);
    let mut file = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--keep-size" => keep = true,
            "-o" => {
                i += 1;
                off = args.get(i).and_then(|s| parse_size(s)).unwrap_or(0);
            }
            "-l" => {
                i += 1;
                len = args.get(i).and_then(|s| parse_size(s));
            }
            a => file = Some(a.to_string()),
        }
        i += 1;
    }
    let (Some(len), Some(file)) = (len, file) else {
        eprintln!("usage: fallocate [-n] [-o OFFSET] -l LENGTH FILE");
        return 2;
    };
    let r = open_for_write(&file).and_then(|f| {
        check(syscall(
            SYS_FALLOCATE,
            &[f.fd() as usize, keep as usize, off as usize, len as usize],
        ))
        .map(|_| ())
    });
    match r {
        Ok(()) => 0,
        Err(e) => err("fallocate", &file, e),
    }
}
