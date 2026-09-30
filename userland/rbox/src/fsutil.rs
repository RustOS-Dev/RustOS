//! truncate, fallocate, getfattr.

use crate::err;
use rustos_rt::fs;
use rustos_rt::prelude::*;
use rustos_rt::sys::{check, syscall};

const SYS_FTRUNCATE: usize = 77;
const SYS_FALLOCATE: usize = 285;
const SYS_LGETXATTR: usize = 192;
const SYS_LLISTXATTR: usize = 195;
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

fn cstring(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// Call a get/list xattr syscall twice: once for the size, once for data.
fn xattr_call(nr: usize, path: &[u8], name: Option<&[u8]>) -> rustos_rt::Result<Vec<u8>> {
    let args = |buf: usize, len: usize| -> Vec<usize> {
        match name {
            Some(n) => vec![path.as_ptr() as usize, n.as_ptr() as usize, buf, len],
            None => vec![path.as_ptr() as usize, buf, len],
        }
    };
    let n = check(syscall(nr, &args(0, 0)))? as usize;
    let mut v = vec![0u8; n];
    let n = check(syscall(nr, &args(v.as_mut_ptr() as usize, n)))? as usize;
    v.truncate(n);
    Ok(v)
}

/// getfattr [-d] [-n NAME] FILE...: print extended attributes.
pub fn getfattr(args: &[String]) -> i32 {
    let mut name = None;
    let mut files = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-n" if i + 1 < args.len() => {
                name = Some(args[i + 1].clone());
                i += 1;
            }
            "-d" => {}
            f => files.push(f.to_string()),
        }
        i += 1;
    }
    if files.is_empty() {
        eprintln!("usage: getfattr [-d] [-n NAME] FILE...");
        return 2;
    }
    let mut rc = 0;
    for f in &files {
        let p = cstring(f);
        let names: Vec<String> = match &name {
            Some(n) => vec![n.clone()],
            None => match xattr_call(SYS_LLISTXATTR, &p, None) {
                Ok(l) => l
                    .split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect(),
                Err(e) => {
                    err("getfattr", f, e);
                    rc = 1;
                    continue;
                }
            },
        };
        println!("# file: {}", f);
        for n in names {
            let cn = cstring(&n);
            match xattr_call(SYS_LGETXATTR, &p, Some(&cn)) {
                Ok(v) => {
                    let text = v.iter().all(|&b| (0x20..0x7F).contains(&b) || b == b'\n');
                    if text {
                        println!("{}=\"{}\"", n, String::from_utf8_lossy(&v));
                    } else {
                        let hex: String = v.iter().map(|b| format!("{:02x}", b)).collect();
                        println!("{}=0x{}", n, hex);
                    }
                }
                Err(e) => {
                    err("getfattr", &n, e);
                    rc = 1;
                }
            }
        }
    }
    rc
}
