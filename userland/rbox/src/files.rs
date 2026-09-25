//! File applets.

use crate::{err, human, parse_flags, read_input};
use rustos_rt::fs::{self, Metadata};
use rustos_rt::prelude::*;
use rustos_rt::{env, io, time};

fn ask(q: &str) -> bool {
    eprint!("{}", q);
    io::read_line().is_some_and(|l| l.trim_start().starts_with(['y', 'Y']))
}

pub fn cat(args: &[String]) -> i32 {
    let (flags, mut ops, _) = parse_flags(args);
    let number = flags.contains(&'n');
    let number_nonblank = flags.contains(&'b');
    let show_ends = flags.contains(&'E') || flags.contains(&'A');
    if ops.is_empty() {
        ops.push(String::from("-"));
    }
    let mut st = 0;
    let mut line_no = 1;
    for p in &ops {
        if !number && !number_nonblank && !show_ends {
            // Stream in chunks (handles huge files and pipes).
            let fd = if p == "-" {
                0
            } else {
                match fs::File::open(p) {
                    Ok(f) => f.into_raw(),
                    Err(e) => {
                        st = err("cat", p, e);
                        continue;
                    }
                }
            };
            let mut buf = alloc::vec![0u8; 64 * 1024];
            loop {
                match io::read(fd, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        io::flush();
                        if io::write_all(1, &buf[..n]).is_err() {
                            return 1;
                        }
                    }
                    Err(rustos_rt::Error(4)) => {}
                    Err(e) => {
                        st = err("cat", p, e);
                        break;
                    }
                }
            }
            if fd != 0 {
                rustos_rt::process::close(fd);
            }
            continue;
        }
        let data = match read_input(p) {
            Ok(d) => d,
            Err(e) => {
                st = err("cat", p, e);
                continue;
            }
        };
        let text = String::from_utf8_lossy(&data);
        let mut lines: Vec<&str> = text.split('\n').collect();
        if text.ends_with('\n') {
            lines.pop();
        }
        for l in lines {
            let end = if show_ends { "$" } else { "" };
            if (number && !number_nonblank) || (number_nonblank && !l.is_empty()) {
                println!("{:6}\t{}{}", line_no, l, end);
                line_no += 1;
            } else {
                println!("{}{}", l, end);
            }
        }
    }
    st
}

pub fn chmod(args: &[String]) -> i32 {
    let ops: Vec<&String> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with("-R"))
        .collect();
    let recursive = args.iter().any(|a| a == "-R");
    if ops.len() < 2 {
        eprintln!("usage: chmod [-R] MODE FILE...");
        return 1;
    }
    let mode_s = ops[0];
    let mut st = 0;
    fn apply(path: &str, mode_s: &str, recursive: bool, st: &mut i32) {
        let cur = match fs::metadata(path) {
            Ok(m) => m.permissions(),
            Err(e) => {
                *st = err("chmod", path, e);
                return;
            }
        };
        let new = if let Ok(v) = u32::from_str_radix(mode_s, 8) {
            v
        } else {
            let mut m = cur;
            for clause in mode_s.split(',') {
                let op_pos = clause.find(['+', '-', '=']).unwrap_or(0);
                let who = &clause[..op_pos];
                let op = clause.as_bytes().get(op_pos).copied().unwrap_or(b'+');
                let perms = &clause[op_pos + 1..];
                let mut bits = 0;
                for c in perms.chars() {
                    bits |= match c {
                        'r' => 4,
                        'w' => 2,
                        'x' => 1,
                        _ => 0,
                    };
                }
                let mut mask = 0;
                let who = if who.is_empty() { "a" } else { who };
                for c in who.chars() {
                    mask |= match c {
                        'u' => bits << 6,
                        'g' => bits << 3,
                        'o' => bits,
                        _ => (bits << 6) | (bits << 3) | bits,
                    };
                }
                m = match op {
                    b'+' => m | mask,
                    b'-' => m & !mask,
                    _ => mask,
                };
            }
            m
        };
        if let Err(e) = fs::set_permissions(path, new) {
            *st = err("chmod", path, e);
        }
        if recursive && fs::is_dir(path) {
            if let Ok(es) = fs::read_dir(path) {
                for e in es {
                    apply(&fs::join(path, &e.name), mode_s, true, st);
                }
            }
        }
    }
    for p in &ops[1..] {
        apply(p, mode_s, recursive, &mut st);
    }
    st
}

pub fn cmp(args: &[String]) -> i32 {
    let (_, ops, _) = parse_flags(args);
    if ops.len() < 2 {
        eprintln!("usage: cmp FILE1 FILE2");
        return 2;
    }
    let (a, b) = match (read_input(&ops[0]), read_input(&ops[1])) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) => return err("cmp", &ops[0], e) + 1,
        (_, Err(e)) => return err("cmp", &ops[1], e) + 1,
    };
    let mut line = 1;
    for i in 0..a.len().min(b.len()) {
        if a[i] != b[i] {
            println!(
                "{} {} differ: byte {}, line {}",
                ops[0],
                ops[1],
                i + 1,
                line
            );
            return 1;
        }
        if a[i] == b'\n' {
            line += 1;
        }
    }
    if a.len() != b.len() {
        println!(
            "cmp: EOF on {}",
            if a.len() < b.len() { &ops[0] } else { &ops[1] }
        );
        return 1;
    }
    0
}

fn copy_recursive(
    src: &str,
    dst: &str,
    recursive: bool,
    preserve: bool,
    verbose: bool,
) -> rustos_rt::Result<()> {
    let m = fs::symlink_metadata(src)?;
    if m.is_symlink() {
        let t = fs::read_link(src)?;
        let _ = fs::remove_file(dst);
        return fs::symlink(&t, dst);
    }
    if m.is_dir() {
        if !recursive {
            eprintln!("cp: -r not specified; omitting directory '{}'", src);
            return Err(rustos_rt::Error(21));
        }
        match fs::create_dir(dst) {
            Ok(()) | Err(rustos_rt::Error(17)) => {}
            Err(e) => return Err(e),
        }
        for e in fs::read_dir(src)? {
            copy_recursive(
                &fs::join(src, &e.name),
                &fs::join(dst, &e.name),
                true,
                preserve,
                verbose,
            )?;
        }
    } else {
        fs::copy(src, dst)?;
        if verbose {
            println!("'{}' -> '{}'", src, dst);
        }
    }
    if preserve {
        let _ = fs::set_permissions(dst, m.permissions());
    }
    Ok(())
}

pub fn cp(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let recursive = flags.iter().any(|c| matches!(c, 'r' | 'R' | 'a'));
    let preserve = flags.iter().any(|c| matches!(c, 'p' | 'a'));
    let interactive = flags.contains(&'i');
    let force = flags.contains(&'f');
    let verbose = flags.contains(&'v');
    let no_clobber = flags.contains(&'n');
    if ops.len() < 2 {
        eprintln!("usage: cp [-rpifvn] SOURCE... DEST");
        return 1;
    }
    let dest = &ops[ops.len() - 1];
    let dest_is_dir = fs::is_dir(dest);
    if ops.len() > 2 && !dest_is_dir {
        eprintln!("cp: target '{}' is not a directory", dest);
        return 1;
    }
    let mut st = 0;
    for src in &ops[..ops.len() - 1] {
        let target = if dest_is_dir {
            fs::join(dest, fs::basename(src))
        } else {
            dest.clone()
        };
        if fs::exists(&target) {
            if no_clobber {
                continue;
            }
            if interactive && !force && !ask(&format!("cp: overwrite '{}'? ", target)) {
                continue;
            }
            if force {
                let _ = fs::remove_file(&target);
            }
        }
        if let Err(e) = copy_recursive(src, &target, recursive, preserve, verbose)
            && e.0 != 21
        {
            st = err("cp", src, e);
        }
    }
    st
}

pub fn dd(args: &[String]) -> i32 {
    let mut input = String::from("-");
    let mut output = String::from("-");
    let mut bs = 512usize;
    let mut count: Option<usize> = None;
    let mut skip = 0u64;
    let mut seek = 0u64;
    for a in &args[1..] {
        let Some((k, v)) = a.split_once('=') else {
            continue;
        };
        let num = |v: &str| -> usize {
            let (n, mul) = match v.chars().last() {
                Some('K') | Some('k') => (&v[..v.len() - 1], 1024),
                Some('M') => (&v[..v.len() - 1], 1024 * 1024),
                Some('G') => (&v[..v.len() - 1], 1024 * 1024 * 1024),
                _ => (v, 1),
            };
            n.parse::<usize>().unwrap_or(0) * mul
        };
        match k {
            "if" => input = v.to_string(),
            "of" => output = v.to_string(),
            "bs" => bs = num(v).max(1),
            "count" => count = Some(num(v)),
            "skip" => skip = num(v) as u64,
            "seek" => seek = num(v) as u64,
            _ => {}
        }
    }
    let inf = if input == "-" {
        None
    } else {
        fs::File::open(&input)
            .map_err(|e| err("dd", &input, e))
            .ok()
    };
    if input != "-" && inf.is_none() {
        return 1;
    }
    let outf = if output == "-" {
        None
    } else {
        fs::File::open_with(&output, fs::O_WRONLY | fs::O_CREAT, 0o644)
            .map_err(|e| err("dd", &output, e))
            .ok()
    };
    if output != "-" && outf.is_none() {
        return 1;
    }
    let in_fd = inf.as_ref().map_or(0, |f| f.fd());
    let out_fd = outf.as_ref().map_or(1, |f| f.fd());
    if skip > 0 {
        if let Some(f) = &inf {
            let _ = f.seek((skip * bs as u64) as i64, 0);
        }
    }
    if seek > 0
        && let Some(f) = &outf
    {
        let _ = f.seek((seek * bs as u64) as i64, 0);
    }
    let mut buf = alloc::vec![0u8; bs];
    let (mut full, mut partial, mut total) = (0usize, 0usize, 0u64);
    let start = time::millis();
    loop {
        if count.is_some_and(|c| full + partial >= c) {
            break;
        }
        let n = match io::read(in_fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return err("dd", &input, e),
        };
        if n == bs {
            full += 1;
        } else {
            partial += 1;
        }
        if let Err(e) = io::write_all(out_fd, &buf[..n]) {
            return err("dd", &output, e);
        }
        total += n as u64;
    }
    let ms = (time::millis() - start).max(1);
    eprintln!(
        "{}+{} records in\n{}+{} records out",
        full, partial, full, partial
    );
    eprintln!(
        "{} bytes copied, {}.{:03} s, {}/s",
        total,
        ms / 1000,
        ms % 1000,
        human(total * 1000 / ms)
    );
    0
}

fn du_walk(path: &str, all: bool, human_sz: bool, summarize: bool, depth: usize) -> u64 {
    let Ok(m) = fs::symlink_metadata(path) else {
        return 0;
    };
    let mut total = m.blocks.max(m.size.div_ceil(512)) * 512;
    if m.is_dir() {
        if let Ok(es) = fs::read_dir(path) {
            for e in es {
                total += du_walk(
                    &fs::join(path, &e.name),
                    all,
                    human_sz,
                    summarize,
                    depth + 1,
                );
            }
        }
        if !summarize || depth == 0 {
            print_du(total, path, human_sz);
        }
    } else if (all && !summarize) || depth == 0 {
        print_du(total, path, human_sz);
    }
    total
}

fn print_du(bytes: u64, path: &str, human_sz: bool) {
    if human_sz {
        println!("{}\t{}", human(bytes), path);
    } else {
        println!("{}\t{}", bytes.div_ceil(1024), path);
    }
}

pub fn du(args: &[String]) -> i32 {
    let (flags, mut ops, _) = parse_flags(args);
    if ops.is_empty() {
        ops.push(String::from("."));
    }
    for p in &ops {
        du_walk(
            p,
            flags.contains(&'a'),
            flags.contains(&'h'),
            flags.contains(&'s'),
            0,
        );
    }
    0
}

fn find_walk(
    path: &str,
    name: Option<&str>,
    kind: Option<char>,
    maxdepth: usize,
    depth: usize,
    exec: &[String],
) {
    let Ok(m) = fs::symlink_metadata(path) else {
        return;
    };
    let base = fs::basename(path);
    let name_ok = name.is_none_or(|n| glob_match(n, base));
    let kind_ok = match kind {
        Some('f') => m.is_file(),
        Some('d') => m.is_dir(),
        Some('l') => m.is_symlink(),
        _ => true,
    };
    if name_ok && kind_ok {
        if exec.is_empty() {
            println!("{}", path);
        } else {
            let argv: Vec<String> = exec
                .iter()
                .map(|a| {
                    if a == "{}" {
                        path.to_string()
                    } else {
                        a.clone()
                    }
                })
                .collect();
            let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
            let _ = rustos_rt::process::run(&refs);
        }
    }
    if m.is_dir()
        && depth < maxdepth
        && let Ok(es) = fs::read_dir(path)
    {
        for e in es {
            find_walk(
                &fs::join(path, &e.name),
                name,
                kind,
                maxdepth,
                depth + 1,
                exec,
            );
        }
    }
}

pub fn glob_match(p: &str, s: &str) -> bool {
    let p: Vec<char> = p.chars().collect();
    let s: Vec<char> = s.chars().collect();
    fn m(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => m(&p[1..], s) || (!s.is_empty() && m(p, &s[1..])),
            (Some('?'), Some(_)) => m(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => m(&p[1..], &s[1..]),
            _ => false,
        }
    }
    m(&p, &s)
}

pub fn find(args: &[String]) -> i32 {
    let mut paths = Vec::new();
    let mut name = None;
    let mut kind = None;
    let mut maxdepth = usize::MAX;
    let mut exec = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-name" | "-iname" => {
                name = args.get(i + 1).cloned();
                i += 2;
            }
            "-type" => {
                kind = args.get(i + 1).and_then(|s| s.chars().next());
                i += 2;
            }
            "-maxdepth" => {
                maxdepth = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(usize::MAX);
                i += 2;
            }
            "-exec" => {
                i += 1;
                while i < args.len() && args[i] != ";" && args[i] != "\\;" {
                    exec.push(args[i].clone());
                    i += 1;
                }
                i += 1;
            }
            "-print" => i += 1,
            p => {
                paths.push(p.to_string());
                i += 1;
            }
        }
    }
    if paths.is_empty() {
        paths.push(String::from("."));
    }
    for p in &paths {
        find_walk(
            p.trim_end_matches('/').max("/"),
            name.as_deref(),
            kind,
            maxdepth,
            0,
            &exec,
        );
    }
    0
}

pub fn hexdump(args: &[String]) -> i32 {
    let (flags, mut ops, _) = parse_flags(args);
    let _ = flags;
    if ops.is_empty() {
        ops.push(String::from("-"));
    }
    let mut st = 0;
    for p in &ops {
        let data = match read_input(p) {
            Ok(d) => d,
            Err(e) => {
                st = err("hexdump", p, e);
                continue;
            }
        };
        for (i, chunk) in data.chunks(16).enumerate() {
            let mut hex = String::new();
            for (j, b) in chunk.iter().enumerate() {
                hex.push_str(&format!("{:02x} ", b));
                if j == 7 {
                    hex.push(' ');
                }
            }
            let ascii: String = chunk
                .iter()
                .map(|&b| {
                    if (0x20..0x7f).contains(&b) {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            println!("{:08x}  {:<49} |{}|", i * 16, hex, ascii);
        }
        println!("{:08x}", data.len());
    }
    st
}

pub fn ln(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    if ops.len() < 2 {
        eprintln!("usage: ln [-sf] TARGET LINK");
        return 1;
    }
    let target = &ops[0];
    let mut link = ops[1].clone();
    if fs::is_dir(&link) {
        link = fs::join(&link, fs::basename(target));
    }
    if flags.contains(&'f') {
        let _ = fs::remove_file(&link);
    }
    let r = if flags.contains(&'s') {
        fs::symlink(target, &link)
    } else {
        fs::hard_link(target, &link)
    };
    match r {
        Ok(()) => 0,
        Err(e) => err("ln", &link, e),
    }
}

struct LsEntry {
    name: String,
    path: String,
    meta: Option<Metadata>,
}

fn fmt_time(t: i64) -> String {
    let (y, mo, d, h, mi, _) = time::civil(t.max(0) as u64);
    let now = time::now();
    let (cy, ..) = time::civil(now);
    if y == cy {
        format!(
            "{} {:2} {:02}:{:02}",
            time::MONTHS[(mo - 1) as usize],
            d,
            h,
            mi
        )
    } else {
        format!("{} {:2}  {}", time::MONTHS[(mo - 1) as usize], d, y)
    }
}

pub fn ls(args: &[String]) -> i32 {
    let (flags, mut ops, long) = parse_flags(args);
    let all = flags.contains(&'a');
    let almost_all = flags.contains(&'A');
    let long_fmt = flags.contains(&'l');
    let human_sz = flags.contains(&'h');
    let one = flags.contains(&'1') || !io::isatty(1);
    let recursive = flags.contains(&'R');
    let dir_only = flags.contains(&'d');
    let by_time = flags.contains(&'t');
    let by_size = flags.contains(&'S');
    let reverse = flags.contains(&'r');
    let inode = flags.contains(&'i');
    let classify = flags.contains(&'F');
    let color = long.iter().any(|l| l.starts_with("color")) || io::isatty(1);
    if ops.is_empty() {
        ops.push(String::from("."));
    }
    let mut st = 0;
    let multiple = ops.len() > 1 || recursive;
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for p in &ops {
        match fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() && !dir_only => dirs.push(p.clone()),
            Ok(m) => files.push(LsEntry {
                name: p.clone(),
                path: p.clone(),
                meta: Some(m),
            }),
            Err(e) => {
                eprintln!("ls: cannot access '{}': {}", p, e);
                st = 2;
            }
        }
    }
    let opts = (long_fmt, human_sz, one, inode, classify, color);
    if !files.is_empty() {
        print_entries(&mut files, opts, by_time, by_size, reverse);
    }
    let mut first = files.is_empty();
    let mut queue = dirs;
    while !queue.is_empty() {
        let d = queue.remove(0);
        if multiple {
            if !first {
                println!();
            }
            println!("{}:", d);
        }
        first = false;
        let entries = match fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("ls: cannot open directory '{}': {}", d, e);
                st = 2;
                continue;
            }
        };
        let mut list: Vec<LsEntry> = Vec::new();
        if all {
            for n in [".", ".."] {
                let path = fs::join(&d, n);
                list.push(LsEntry {
                    name: n.to_string(),
                    meta: fs::metadata(&path).ok(),
                    path,
                });
            }
        }
        for e in entries {
            if e.name.starts_with('.') && !all && !almost_all {
                continue;
            }
            let path = fs::join(&d, &e.name);
            list.push(LsEntry {
                meta: fs::symlink_metadata(&path).ok(),
                name: e.name,
                path,
            });
        }
        if long_fmt {
            let blocks: u64 = list.iter().filter_map(|e| e.meta.map(|m| m.blocks)).sum();
            println!("total {}", blocks / 2);
        }
        print_entries(&mut list, opts, by_time, by_size, reverse);
        if recursive {
            let mut sub: Vec<String> = list
                .iter()
                .filter(|e| e.name != "." && e.name != ".." && e.meta.is_some_and(|m| m.is_dir()))
                .map(|e| e.path.clone())
                .collect();
            sub.extend(queue);
            queue = sub;
        }
    }
    st
}

fn colorize(e: &LsEntry, color: bool) -> String {
    let Some(m) = e.meta else {
        return e.name.clone();
    };
    if !color {
        return e.name.clone();
    }
    let code = if m.is_dir() {
        "1;34"
    } else if m.is_symlink() {
        "1;36"
    } else if m.mode & fs::S_IFMT == fs::S_IFCHR || m.mode & fs::S_IFMT == fs::S_IFBLK {
        "1;33"
    } else if m.mode & 0o111 != 0 {
        "1;32"
    } else {
        return e.name.clone();
    };
    format!("\x1b[{}m{}\x1b[0m", code, e.name)
}

fn print_entries(
    list: &mut [LsEntry],
    opts: (bool, bool, bool, bool, bool, bool),
    by_time: bool,
    by_size: bool,
    reverse: bool,
) {
    let (long_fmt, human_sz, one, inode, classify, color) = opts;
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    if by_time {
        list.sort_by_key(|e| core::cmp::Reverse(e.meta.map_or(0, |m| m.mtime)));
    }
    if by_size {
        list.sort_by_key(|e| core::cmp::Reverse(e.meta.map_or(0, |m| m.size)));
    }
    if reverse {
        list.reverse();
    }
    let suffix = |e: &LsEntry| -> &'static str {
        if !classify {
            return "";
        }
        match e.meta {
            Some(m) if m.is_dir() => "/",
            Some(m) if m.is_symlink() => "@",
            Some(m) if m.mode & 0o111 != 0 => "*",
            _ => "",
        }
    };
    if long_fmt {
        let wsize = list
            .iter()
            .map(|e| {
                let s = e.meta.map_or(0, |m| m.size);
                if human_sz {
                    human(s).len()
                } else {
                    format!("{}", s).len()
                }
            })
            .max()
            .unwrap_or(1);
        for e in list.iter() {
            let Some(m) = e.meta else {
                println!("?????????? ? ? ? ? {}", e.name);
                continue;
            };
            let size = if m.mode & fs::S_IFMT == fs::S_IFCHR || m.mode & fs::S_IFMT == fs::S_IFBLK {
                format!("{}, {}", m.rdev >> 8, m.rdev & 0xff)
            } else if human_sz {
                human(m.size)
            } else {
                format!("{}", m.size)
            };
            let link = if m.is_symlink() {
                format!(" -> {}", fs::read_link(&e.path).unwrap_or_default())
            } else {
                String::new()
            };
            let ino = if inode {
                format!("{:8} ", m.ino)
            } else {
                String::new()
            };
            println!(
                "{}{} {:2} root root {:>w$} {} {}{}{}",
                ino,
                m.mode_string(),
                m.nlink,
                size,
                fmt_time(m.mtime),
                colorize(e, color),
                suffix(e),
                link,
                w = wsize
            );
        }
        return;
    }
    if one {
        for e in list.iter() {
            if inode {
                print!("{:8} ", e.meta.map_or(0, |m| m.ino));
            }
            println!("{}{}", colorize(e, color), suffix(e));
        }
        return;
    }
    // Column output sized to the terminal.
    let (_, cols) = rustos_rt::term::size(1);
    let width = list
        .iter()
        .map(|e| e.name.len() + suffix(e).len())
        .max()
        .unwrap_or(1)
        + 2;
    let per_row = (cols as usize / width).max(1);
    let rows = list.len().div_ceil(per_row);
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..per_row {
            let i = c * rows + r;
            if let Some(e) = list.get(i) {
                let visible = e.name.len() + suffix(e).len();
                line.push_str(&colorize(e, color));
                line.push_str(suffix(e));
                if c + 1 < per_row && (c + 1) * rows + r < list.len() {
                    line.push_str(&" ".repeat(width - visible));
                }
            }
        }
        println!("{}", line);
    }
}

fn hash_files(args: &[String], name: &str, sha: bool) -> i32 {
    let (_, mut ops, _) = parse_flags(args);
    if ops.is_empty() {
        ops.push(String::from("-"));
    }
    let mut st = 0;
    for p in &ops {
        let fd = if p == "-" {
            0
        } else {
            match fs::File::open(p) {
                Ok(f) => f.into_raw(),
                Err(e) => {
                    st = err(name, p, e);
                    continue;
                }
            }
        };
        let mut md5 = crate::hash::Md5::new();
        let mut s256 = crate::hash::Sha256::new();
        let mut buf = alloc::vec![0u8; 64 * 1024];
        loop {
            match io::read(fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if sha {
                        s256.update(&buf[..n]);
                    } else {
                        md5.update(&buf[..n]);
                    }
                }
                Err(rustos_rt::Error(4)) => {}
                Err(e) => {
                    st = err(name, p, e);
                    break;
                }
            }
        }
        if fd != 0 {
            rustos_rt::process::close(fd);
        }
        let digest: Vec<u8> = if sha {
            s256.finish().to_vec()
        } else {
            md5.finish().to_vec()
        };
        let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
        println!("{}  {}", hex, p);
    }
    st
}

pub fn md5sum(args: &[String]) -> i32 {
    hash_files(args, "md5sum", false)
}

pub fn sha256sum(args: &[String]) -> i32 {
    hash_files(args, "sha256sum", true)
}

pub fn mkdir(args: &[String]) -> i32 {
    let mut parents = false;
    let mut mode = 0o777;
    let mut ops = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a == "-p" {
            parents = true;
        } else if a == "-m" {
            mode = args
                .get(i + 1)
                .and_then(|m| u32::from_str_radix(m, 8).ok())
                .unwrap_or(0o777);
            i += 1;
        } else if let Some(m) = a.strip_prefix("-m") {
            mode = u32::from_str_radix(m, 8).unwrap_or(0o777);
        } else if a == "-pm" || a == "-mp" {
            parents = true;
            mode = args
                .get(i + 1)
                .and_then(|m| u32::from_str_radix(m, 8).ok())
                .unwrap_or(0o777);
            i += 1;
        } else {
            ops.push(a.clone());
        }
        i += 1;
    }
    let mut st = 0;
    for p in &ops {
        let r = if parents {
            fs::create_dir_all(p).and_then(|_| fs::set_permissions(p, mode & 0o7777))
        } else {
            fs::create_dir_mode(p, mode).and_then(|_| {
                if mode != 0o777 {
                    fs::set_permissions(p, mode)
                } else {
                    Ok(())
                }
            })
        };
        if let Err(e) = r {
            st = err("mkdir", p, e);
        }
    }
    st
}

pub fn mkfifo(args: &[String]) -> i32 {
    let mut st = 0;
    for p in &args[1..] {
        if let Err(e) = fs::mkfifo(p, 0o644) {
            st = err("mkfifo", p, e);
        }
    }
    st
}

pub fn mv(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let interactive = flags.contains(&'i');
    let force = flags.contains(&'f');
    let no_clobber = flags.contains(&'n');
    let verbose = flags.contains(&'v');
    if ops.len() < 2 {
        eprintln!("usage: mv [-ifnv] SOURCE... DEST");
        return 1;
    }
    let dest = &ops[ops.len() - 1];
    let dest_dir = fs::is_dir(dest);
    let mut st = 0;
    for src in &ops[..ops.len() - 1] {
        let target = if dest_dir {
            fs::join(dest, fs::basename(src))
        } else {
            dest.clone()
        };
        if fs::exists(&target) {
            if no_clobber {
                continue;
            }
            if interactive && !force && !ask(&format!("mv: overwrite '{}'? ", target)) {
                continue;
            }
        }
        let r = match fs::rename(src, &target) {
            Err(rustos_rt::Error(18)) => {
                // Cross-device: copy then remove.
                copy_recursive(src, &target, true, true, false)
                    .and_then(|_| fs::remove_dir_all(src))
            }
            other => other,
        };
        match r {
            Ok(()) => {
                if verbose {
                    println!("renamed '{}' -> '{}'", src, target);
                }
            }
            Err(e) => st = err("mv", src, e),
        }
    }
    st
}

pub fn pwd(_args: &[String]) -> i32 {
    println!(
        "{}",
        env::current_dir().unwrap_or_else(|_| String::from("/"))
    );
    0
}

fn canonical(p: &str) -> String {
    let abs = if p.starts_with('/') {
        p.to_string()
    } else {
        fs::join(&env::current_dir().unwrap_or_default(), p)
    };
    let mut parts: Vec<String> = Vec::new();
    let mut todo: Vec<String> = abs
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    todo.reverse();
    let mut guard = 0;
    while let Some(c) = todo.pop() {
        match c.as_str() {
            "." => {}
            ".." => {
                parts.pop();
            }
            _ => {
                let cur = format!(
                    "/{}",
                    parts
                        .iter()
                        .chain(core::iter::once(&c))
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("/")
                );
                if guard < 40
                    && let Ok(t) = fs::read_link(&cur)
                {
                    guard += 1;
                    if t.starts_with('/') {
                        parts.clear();
                    }
                    let mut more: Vec<String> = t
                        .split('/')
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect();
                    more.reverse();
                    todo.extend(more);
                    continue;
                }
                parts.push(c);
            }
        }
    }
    format!("/{}", parts.join("/"))
}

pub fn readlink(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let mut st = 0;
    for p in &ops {
        if flags.contains(&'f') {
            println!("{}", canonical(p));
        } else {
            match fs::read_link(p) {
                Ok(t) => println!("{}", t),
                Err(_) => st = 1,
            }
        }
    }
    st
}

pub fn realpath(args: &[String]) -> i32 {
    for p in &args[1..] {
        println!("{}", canonical(p));
    }
    0
}

fn remove(path: &str, recursive: bool, force: bool, interactive: bool, verbose: bool) -> i32 {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            if force {
                return 0;
            }
            return err("rm", path, e);
        }
    };
    if m.is_dir() {
        if !recursive {
            eprintln!("rm: cannot remove '{}': Is a directory", path);
            return 1;
        }
        if interactive && !ask(&format!("rm: descend into directory '{}'? ", path)) {
            return 0;
        }
        let mut st = 0;
        if let Ok(es) = fs::read_dir(path) {
            for e in es {
                st |= remove(&fs::join(path, &e.name), true, force, interactive, verbose);
            }
        }
        if interactive && !ask(&format!("rm: remove directory '{}'? ", path)) {
            return st;
        }
        if let Err(e) = fs::remove_dir(path) {
            return err("rm", path, e);
        }
        if verbose {
            println!("removed directory '{}'", path);
        }
        return st;
    }
    if interactive && !ask(&format!("rm: remove '{}'? ", path)) {
        return 0;
    }
    match fs::remove_file(path) {
        Ok(()) => {
            if verbose {
                println!("removed '{}'", path);
            }
            0
        }
        Err(e) => err("rm", path, e),
    }
}

pub fn rm(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let recursive = flags.iter().any(|c| matches!(c, 'r' | 'R'));
    let force = flags.contains(&'f');
    let interactive = flags.contains(&'i') && !force;
    let once = flags.contains(&'I');
    let verbose = flags.contains(&'v');
    if ops.is_empty() {
        if force {
            return 0;
        }
        eprintln!("usage: rm [-rfiIv] FILE...");
        return 1;
    }
    if once
        && (ops.len() > 3 || recursive)
        && !ask(&format!(
            "rm: remove {} argument(s){}? ",
            ops.len(),
            if recursive { " recursively" } else { "" }
        ))
    {
        return 0;
    }
    let mut st = 0;
    for p in &ops {
        if p == "/" || p == "/." {
            eprintln!("rm: refusing to remove '/'");
            st = 1;
            continue;
        }
        st |= remove(p, recursive, force, interactive, verbose);
    }
    st
}

pub fn rmdir(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let mut st = 0;
    for p in &ops {
        let mut cur = p.trim_end_matches('/').to_string();
        loop {
            if let Err(e) = fs::remove_dir(&cur) {
                st = err("rmdir", &cur, e);
                break;
            }
            if !flags.contains(&'p') {
                break;
            }
            let parent = fs::dirname(&cur).to_string();
            if parent == "." || parent == "/" {
                break;
            }
            cur = parent;
        }
    }
    st
}

pub fn stat(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let mut st = 0;
    for p in &ops {
        let m = if flags.contains(&'L') {
            fs::metadata(p)
        } else {
            fs::symlink_metadata(p)
        };
        match m {
            Ok(m) => {
                let kind = match m.mode & fs::S_IFMT {
                    fs::S_IFDIR => "directory",
                    fs::S_IFLNK => "symbolic link",
                    fs::S_IFCHR => "character special file",
                    fs::S_IFBLK => "block special file",
                    fs::S_IFIFO => "fifo",
                    fs::S_IFSOCK => "socket",
                    _ => {
                        if m.size == 0 {
                            "regular empty file"
                        } else {
                            "regular file"
                        }
                    }
                };
                println!("  File: {}", p);
                println!("  Size: {:<15} Blocks: {:<10} {}", m.size, m.blocks, kind);
                println!(
                    "Device: {:<14} Inode: {:<11} Links: {}",
                    m.dev, m.ino, m.nlink
                );
                println!(
                    "Access: ({:04o}/{})  Uid: ({}/root)   Gid: ({}/root)",
                    m.permissions(),
                    m.mode_string(),
                    m.uid,
                    m.gid
                );
                let t = |v: i64| {
                    let (y, mo, d, h, mi, s) = time::civil(v.max(0) as u64);
                    format!("{}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s)
                };
                println!("Access: {}", t(m.atime));
                println!("Modify: {}", t(m.mtime));
                println!("Change: {}", t(m.ctime));
            }
            Err(e) => st = err("stat", p, e),
        }
    }
    st
}

pub fn touch(args: &[String]) -> i32 {
    let (_, ops, _) = parse_flags(args);
    let mut st = 0;
    for p in &ops {
        if let Err(e) = fs::touch(p) {
            st = err("touch", p, e);
        }
    }
    st
}
