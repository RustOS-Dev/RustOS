//! truncate, fallocate, getfattr, chown, quota, setquota.

use crate::err;
use rustos_rt::fs;
use rustos_rt::prelude::*;
use rustos_rt::sys::{check, syscall};

const SYS_FTRUNCATE: usize = 77;
const SYS_FALLOCATE: usize = 285;
const SYS_LGETXATTR: usize = 192;
const SYS_LLISTXATTR: usize = 195;
const SYS_QUOTACTL: usize = 179;
const SYS_LCHOWN: usize = 94;
const Q_GETQUOTA: usize = 0x80_0007;
const Q_SETQUOTA: usize = 0x80_0008;
const QIF_LIMITS: u32 = 1 | 4;
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

/// Quota type from -u / -g / -P.
fn quota_kind(flag: &str) -> Option<usize> {
    match flag {
        "-u" => Some(0),
        "-g" => Some(1),
        "-P" => Some(2),
        _ => None,
    }
}

/// quota -u|-g|-P ID DEVICE: usage and limits of one id.
pub fn quota(args: &[String]) -> i32 {
    let (Some(kind), Some(id), Some(dev)) = (
        args.get(1).and_then(|f| quota_kind(f)),
        args.get(2).and_then(|i| i.parse::<u32>().ok()),
        args.get(3),
    ) else {
        eprintln!("usage: quota -u|-g|-P ID DEVICE");
        return 2;
    };
    let d = cstring(dev);
    let mut b = [0u8; 72];
    let r = check(syscall(
        SYS_QUOTACTL,
        &[
            (Q_GETQUOTA << 8) | kind,
            d.as_ptr() as usize,
            id as usize,
            b.as_mut_ptr() as usize,
        ],
    ));
    if let Err(e) = r {
        return err("quota", dev, e);
    }
    let g = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
    println!(
        "{:>10} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "KiB", "soft", "hard", "inodes", "soft", "hard"
    );
    println!(
        "{:>10} {:>8} {:>8} {:>8} {:>8} {:>8}",
        g(2).div_ceil(1024),
        g(1),
        g(0),
        g(5),
        g(4),
        g(3)
    );
    0
}

/// setquota -u|-g|-P ID BSOFT BHARD ISOFT IHARD DEVICE (blocks in KiB).
pub fn setquota(args: &[String]) -> i32 {
    let kind = args.get(1).and_then(|f| quota_kind(f));
    let nums: Vec<Option<u64>> = (2..7)
        .map(|i| args.get(i).and_then(|s| s.parse().ok()))
        .collect();
    let (Some(kind), Some(dev)) = (kind, args.get(7)) else {
        eprintln!("usage: setquota -u|-g|-P ID BSOFT BHARD ISOFT IHARD DEVICE");
        return 2;
    };
    let [Some(id), Some(bs), Some(bh), Some(is), Some(ih)] = nums[..] else {
        eprintln!("setquota: bad number");
        return 2;
    };
    let mut b = [0u8; 72];
    for (i, v) in [bh, bs, 0, ih, is].iter().enumerate() {
        b[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    b[64..68].copy_from_slice(&QIF_LIMITS.to_le_bytes());
    let d = cstring(dev);
    match check(syscall(
        SYS_QUOTACTL,
        &[
            (Q_SETQUOTA << 8) | kind,
            d.as_ptr() as usize,
            id as usize,
            b.as_mut_ptr() as usize,
        ],
    )) {
        Ok(_) => 0,
        Err(e) => err("setquota", dev, e),
    }
}

/// chown UID[:GID] FILE... (numeric ids; ":GID" alone changes the group).
pub fn chown(args: &[String]) -> i32 {
    let Some(spec) = args.get(1) else {
        eprintln!("usage: chown UID[:GID] FILE...");
        return 2;
    };
    let (u, g) = spec.split_once(':').unwrap_or((spec, ""));
    let id = |s: &str| -> Option<usize> {
        if s.is_empty() {
            Some(u32::MAX as usize)
        } else {
            s.parse::<u32>().ok().map(|v| v as usize)
        }
    };
    let (Some(uid), Some(gid)) = (id(u), id(g)) else {
        eprintln!("chown: numeric ids only: {}", spec);
        return 2;
    };
    let mut rc = 0;
    for f in &args[2..] {
        let p = cstring(f);
        if let Err(e) = check(syscall(SYS_LCHOWN, &[p.as_ptr() as usize, uid, gid])) {
            rc = err("chown", f, e);
        }
    }
    rc
}
