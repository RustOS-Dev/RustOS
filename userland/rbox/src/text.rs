//! Text-processing applets.

use crate::regex::Regex;
use crate::{err, parse_flags, read_input};
use rustos_rt::prelude::*;
use rustos_rt::{fs, io, process};

fn lines_of(data: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(data);
    let mut v: Vec<String> = text.split('\n').map(String::from).collect();
    if text.ends_with('\n') {
        v.pop();
    }
    v
}

fn inputs(ops: &[String]) -> Vec<String> {
    if ops.is_empty() {
        alloc::vec![String::from("-")]
    } else {
        ops.to_vec()
    }
}

pub fn basename(args: &[String]) -> i32 {
    let Some(p) = args.get(1) else {
        eprintln!("usage: basename NAME [SUFFIX]");
        return 1;
    };
    let mut b = fs::basename(p).to_string();
    if let Some(s) = args.get(2)
        && b.ends_with(s.as_str())
        && b != *s
    {
        b.truncate(b.len() - s.len());
    }
    println!("{}", b);
    0
}

pub fn dirname(args: &[String]) -> i32 {
    for p in &args[1..] {
        println!("{}", fs::dirname(p));
    }
    0
}

pub fn echo(args: &[String]) -> i32 {
    let mut newline = true;
    let mut i = 1;
    if args.get(1).map(|s| s.as_str()) == Some("-n") {
        newline = false;
        i = 2;
    }
    print!("{}", args[i..].join(" "));
    if newline {
        println!();
    }
    0
}

pub fn printf(args: &[String]) -> i32 {
    // Share the shell's implementation semantics.
    let refs: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    printf_impl(&refs)
}

fn printf_impl(args: &[&str]) -> i32 {
    let Some(fmt) = args.first() else { return 1 };
    let mut out = String::new();
    let mut ai = 1;
    let chars: Vec<char> = fmt.chars().collect();
    loop {
        let before = ai;
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '\\' if i + 1 < chars.len() => {
                    out.push(match chars[i + 1] {
                        'n' => '\n',
                        't' => '\t',
                        '\\' => '\\',
                        'e' => '\x1b',
                        c => c,
                    });
                    i += 2;
                }
                '%' if i + 1 < chars.len() => {
                    let mut j = i + 1;
                    let mut spec = String::new();
                    while j < chars.len() && "-0123456789.".contains(chars[j]) {
                        spec.push(chars[j]);
                        j += 1;
                    }
                    let conv = chars.get(j).copied().unwrap_or('%');
                    let a = args.get(ai).copied().unwrap_or("");
                    let left = spec.starts_with('-');
                    let zero = spec.starts_with('0');
                    let width: usize = spec
                        .trim_start_matches(['-', '0'])
                        .split('.')
                        .next()
                        .unwrap_or("")
                        .parse()
                        .unwrap_or(0);
                    let body = match conv {
                        '%' => {
                            ai -= 1;
                            String::from("%")
                        }
                        's' => a.to_string(),
                        'd' | 'i' => format!("{}", a.parse::<i64>().unwrap_or(0)),
                        'x' => format!("{:x}", a.parse::<i64>().unwrap_or(0)),
                        'X' => format!("{:X}", a.parse::<i64>().unwrap_or(0)),
                        'o' => format!("{:o}", a.parse::<i64>().unwrap_or(0)),
                        'c' => a.chars().next().map(String::from).unwrap_or_default(),
                        c => format!("%{}", c),
                    };
                    ai += 1;
                    let pad = width.saturating_sub(body.chars().count());
                    if left {
                        out.push_str(&body);
                        out.push_str(&" ".repeat(pad));
                    } else {
                        out.push_str(&(if zero { "0" } else { " " }).repeat(pad));
                        out.push_str(&body);
                    }
                    i = j + 1;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        if ai >= args.len() || ai == before {
            break;
        }
    }
    print!("{}", out);
    0
}

pub fn cut(args: &[String]) -> i32 {
    let mut delim = '\t';
    let mut fields: Vec<(usize, usize)> = Vec::new();
    let mut chars_mode = false;
    let mut ops = Vec::new();
    let mut i = 1;
    let parse_list = |s: &str| -> Vec<(usize, usize)> {
        s.split(',')
            .filter_map(|r| {
                let (a, b) = match r.split_once('-') {
                    Some((a, b)) => (
                        a.parse().unwrap_or(1),
                        if b.is_empty() {
                            usize::MAX
                        } else {
                            b.parse().unwrap_or(usize::MAX)
                        },
                    ),
                    None => {
                        let n = r.parse().ok()?;
                        (n, n)
                    }
                };
                Some((a, b))
            })
            .collect()
    };
    while i < args.len() {
        let a = &args[i];
        if let Some(d) = a.strip_prefix("-d") {
            delim = if d.is_empty() {
                i += 1;
                args.get(i).and_then(|s| s.chars().next()).unwrap_or('\t')
            } else {
                d.chars().next().unwrap()
            };
        } else if let Some(f) = a.strip_prefix("-f").or_else(|| a.strip_prefix("-c")) {
            chars_mode = a.starts_with("-c");
            let spec = if f.is_empty() {
                i += 1;
                args.get(i).cloned().unwrap_or_default()
            } else {
                f.to_string()
            };
            fields = parse_list(&spec);
        } else {
            ops.push(a.clone());
        }
        i += 1;
    }
    let sel = |n: usize| fields.iter().any(|&(a, b)| n >= a && n <= b);
    for p in inputs(&ops) {
        let data = match read_input(&p) {
            Ok(d) => d,
            Err(e) => return err("cut", &p, e),
        };
        for line in lines_of(&data) {
            if chars_mode {
                let s: String = line
                    .chars()
                    .enumerate()
                    .filter(|(k, _)| sel(k + 1))
                    .map(|(_, c)| c)
                    .collect();
                println!("{}", s);
            } else if !line.contains(delim) {
                println!("{}", line);
            } else {
                let parts: Vec<&str> = line
                    .split(delim)
                    .enumerate()
                    .filter(|(k, _)| sel(k + 1))
                    .map(|(_, p)| p)
                    .collect();
                println!("{}", parts.join(&delim.to_string()));
            }
        }
    }
    0
}

/// Line diff via longest common subsequence (normal "diff" output).
pub fn diff(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let unified = flags.contains(&'u');
    if ops.len() != 2 {
        eprintln!("usage: diff [-u] FILE1 FILE2");
        return 2;
    }
    let (a, b) = match (read_input(&ops[0]), read_input(&ops[1])) {
        (Ok(a), Ok(b)) => (lines_of(&a), lines_of(&b)),
        (Err(e), _) => return err("diff", &ops[0], e) + 1,
        (_, Err(e)) => return err("diff", &ops[1], e) + 1,
    };
    let (n, m) = (a.len(), b.len());
    if n * m > 4_000_000 {
        eprintln!("diff: files too large");
        return 2;
    }
    let mut lcs = alloc::vec![alloc::vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut differ = false;
    if unified {
        println!("--- {}\n+++ {}", ops[0], ops[1]);
    }
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            if unified {
                println!(" {}", a[i]);
            }
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            differ = true;
            if unified {
                println!("+{}", b[j]);
            } else {
                println!("{}a{}\n> {}", i, j + 1, b[j]);
            }
            j += 1;
        } else {
            differ = true;
            if unified {
                println!("-{}", a[i]);
            } else {
                println!("{}d{}\n< {}", i + 1, j, a[i]);
            }
            i += 1;
        }
    }
    differ as i32
}

pub fn grep(args: &[String]) -> i32 {
    let mut flags = Vec::new();
    let mut ops = Vec::new();
    let mut patterns = Vec::new();
    let mut i = 1;
    let mut after = 0usize;
    while i < args.len() {
        let a = &args[i];
        if a == "-e" {
            if let Some(p) = args.get(i + 1) {
                patterns.push(p.clone());
            }
            i += 2;
            continue;
        }
        if a == "-A" || a == "-C" {
            after = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
            i += 2;
            continue;
        }
        if a.starts_with('-') && a.len() > 1 && a != "-" {
            flags.extend(a[1..].chars());
        } else {
            ops.push(a.clone());
        }
        i += 1;
    }
    if patterns.is_empty() {
        if ops.is_empty() {
            eprintln!("usage: grep [-iEFvnclqrwoH] PATTERN [FILE...]");
            return 2;
        }
        patterns.push(ops.remove(0));
    }
    let icase = flags.contains(&'i');
    let extended = flags.contains(&'E');
    let fixed = flags.contains(&'F');
    let invert = flags.contains(&'v');
    let numbers = flags.contains(&'n');
    let count = flags.contains(&'c');
    let list = flags.contains(&'l');
    let quiet = flags.contains(&'q');
    let recursive = flags.contains(&'r') || flags.contains(&'R');
    let word = flags.contains(&'w');
    let only = flags.contains(&'o');
    let regexes: Vec<Regex> = if fixed {
        Vec::new()
    } else {
        let mut v = Vec::new();
        for p in &patterns {
            let p = if word {
                format!("\\b({})\\b", p)
            } else {
                p.clone()
            };
            match Regex::new(&p, extended || word, icase) {
                Ok(r) => v.push(r),
                Err(e) => {
                    eprintln!("grep: {}", e);
                    return 2;
                }
            }
        }
        v
    };
    let matches = |line: &str| -> Option<(usize, usize)> {
        if fixed {
            for p in &patterns {
                let found = if icase {
                    line.to_lowercase().find(&p.to_lowercase())
                } else {
                    line.find(p.as_str())
                };
                if let Some(s) = found {
                    return Some((s, s + p.len()));
                }
            }
            None
        } else {
            regexes
                .iter()
                .find_map(|r| r.find_at(line, 0).map(|(s, e, _)| (s, e)))
        }
    };
    let mut files = inputs(&ops);
    if recursive {
        let mut expanded = Vec::new();
        fn walk(p: &str, out: &mut Vec<String>) {
            if fs::is_dir(p) {
                if let Ok(es) = fs::read_dir(p) {
                    for e in es {
                        walk(&fs::join(p, &e.name), out);
                    }
                }
            } else {
                out.push(p.to_string());
            }
        }
        for f in &files {
            if f == "-" {
                walk(".", &mut expanded);
            } else {
                walk(f, &mut expanded);
            }
        }
        files = expanded;
    }
    let show_name = files.len() > 1 || flags.contains(&'H');
    let mut any = false;
    for f in &files {
        let data = match read_input(f) {
            Ok(d) => d,
            Err(e) => {
                if !flags.contains(&'s') {
                    eprintln!("grep: {}: {}", f, e);
                }
                continue;
            }
        };
        let mut n = 0;
        let mut trailing = 0usize;
        for (ln, line) in lines_of(&data).iter().enumerate() {
            let m = matches(line);
            if m.is_some() != invert {
                n += 1;
                any = true;
                if quiet {
                    return 0;
                }
                if list {
                    println!("{}", f);
                    break;
                }
                if !count {
                    let prefix = format!(
                        "{}{}",
                        if show_name {
                            format!("{}:", f)
                        } else {
                            String::new()
                        },
                        if numbers {
                            format!("{}:", ln + 1)
                        } else {
                            String::new()
                        }
                    );
                    if only && let Some((s, e)) = m {
                        println!("{}{}", prefix, &line[s..e]);
                    } else if io::isatty(1)
                        && let Some((s, e)) = m
                    {
                        println!(
                            "{}{}\x1b[1;31m{}\x1b[0m{}",
                            prefix,
                            &line[..s],
                            &line[s..e],
                            &line[e..]
                        );
                    } else {
                        println!("{}{}", prefix, line);
                    }
                }
                trailing = after;
            } else if trailing > 0 && !count {
                trailing -= 1;
                println!("{}", line);
            }
        }
        if count {
            if show_name {
                println!("{}:{}", f, n);
            } else {
                println!("{}", n);
            }
        }
    }
    if any { 0 } else { 1 }
}

fn count_arg(args: &[String], default: usize) -> (usize, bool, Vec<String>) {
    let mut n = default;
    let mut from_start = false;
    let mut ops = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a == "-n" || a == "-c" {
            if let Some(v) = args.get(i + 1) {
                from_start = v.starts_with('+');
                n = v.trim_start_matches(['+', '-']).parse().unwrap_or(default);
            }
            i += 2;
            continue;
        }
        if a.len() > 1 && a.starts_with('-') && a[1..].chars().all(|c| c.is_ascii_digit()) {
            n = a[1..].parse().unwrap_or(default);
        } else if a == "-f" {
        } else {
            ops.push(a.clone());
        }
        i += 1;
    }
    (n, from_start, ops)
}

pub fn head(args: &[String]) -> i32 {
    let (n, _, ops) = count_arg(args, 10);
    let bytes = args.iter().any(|a| a == "-c");
    let files = inputs(&ops);
    for (k, f) in files.iter().enumerate() {
        if files.len() > 1 {
            println!("{}==> {} <==", if k > 0 { "\n" } else { "" }, f);
        }
        // Stream the input so endless sources (/dev/zero, pipes) work.
        let fd = if f == "-" {
            io::STDIN
        } else {
            match rustos_rt::fs::File::open(f) {
                Ok(file) => file.into_raw(),
                Err(e) => return err("head", f, e),
            }
        };
        io::flush();
        let mut left = n;
        let mut buf = alloc::vec![0u8; 65536];
        while left > 0 {
            let got = match io::read(fd, &mut buf) {
                Ok(0) => break,
                Ok(g) => g,
                Err(e) => {
                    if f != "-" {
                        rustos_rt::process::close(fd);
                    }
                    return err("head", f, e);
                }
            };
            let take = if bytes {
                got.min(left)
            } else {
                // Up to and including the n-th remaining newline.
                let mut end = got;
                for (i, &b) in buf[..got].iter().enumerate() {
                    if b == b'\n' {
                        left -= 1;
                        if left == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                }
                end
            };
            if bytes {
                left -= take;
            }
            if io::write_all(1, &buf[..take]).is_err() {
                break;
            }
        }
        if f != "-" {
            rustos_rt::process::close(fd);
        }
    }
    0
}

pub fn tail(args: &[String]) -> i32 {
    let (n, from_start, ops) = count_arg(args, 10);
    let follow = args.iter().any(|a| a == "-f");
    let files = inputs(&ops);
    for (k, f) in files.iter().enumerate() {
        if files.len() > 1 {
            println!("{}==> {} <==", if k > 0 { "\n" } else { "" }, f);
        }
        let data = match read_input(f) {
            Ok(d) => d,
            Err(e) => return err("tail", f, e),
        };
        let lines = lines_of(&data);
        let start = if from_start {
            n.saturating_sub(1)
        } else {
            lines.len().saturating_sub(n)
        };
        for l in &lines[start.min(lines.len())..] {
            println!("{}", l);
        }
        if follow && f != "-" {
            let mut off = data.len() as u64;
            loop {
                io::flush();
                rustos_rt::time::sleep_ms(500);
                let Ok(file) = fs::File::open(f) else { break };
                let mut buf = [0u8; 4096];
                while let Ok(r) = file.read_at(off, &mut buf) {
                    if r == 0 {
                        break;
                    }
                    let _ = io::write_all(1, &buf[..r]);
                    off += r as u64;
                }
            }
        }
    }
    0
}

pub fn more(args: &[String]) -> i32 {
    let (_, ops, _) = parse_flags(args);
    let (rows, _) = rustos_rt::term::size(1);
    let tty = io::isatty(1) && fs::File::open("/dev/tty").is_ok();
    for f in inputs(&ops) {
        let data = match read_input(&f) {
            Ok(d) => d,
            Err(e) => return err("more", &f, e),
        };
        let lines = lines_of(&data);
        let page = rows.saturating_sub(1).max(1) as usize;
        let mut i = 0;
        while i < lines.len() {
            for l in lines.iter().skip(i).take(page) {
                println!("{}", l);
            }
            i += page;
            if i >= lines.len() || !tty {
                continue;
            }
            print!("\x1b[7m--More-- ({}%)\x1b[0m", i * 100 / lines.len());
            io::flush();
            let tty_f = fs::File::open("/dev/tty").unwrap();
            let orig = rustos_rt::term::raw_mode(tty_f.fd());
            let mut b = [0u8; 1];
            let _ = tty_f.read(&mut b);
            if let Some(o) = orig {
                rustos_rt::term::set(tty_f.fd(), &o);
            }
            print!("\r\x1b[K");
            if b[0] == b'q' {
                return 0;
            }
        }
    }
    0
}

pub fn nl(args: &[String]) -> i32 {
    let (_, ops, _) = parse_flags(args);
    let mut n = 1;
    for f in inputs(&ops) {
        let data = match read_input(&f) {
            Ok(d) => d,
            Err(e) => return err("nl", &f, e),
        };
        for l in lines_of(&data) {
            if l.is_empty() {
                println!();
            } else {
                println!("{:6}\t{}", n, l);
                n += 1;
            }
        }
    }
    0
}

pub fn rev(args: &[String]) -> i32 {
    let (_, ops, _) = parse_flags(args);
    for f in inputs(&ops) {
        let data = match read_input(&f) {
            Ok(d) => d,
            Err(e) => return err("rev", &f, e),
        };
        for l in lines_of(&data) {
            println!("{}", l.chars().rev().collect::<String>());
        }
    }
    0
}

pub fn tac(args: &[String]) -> i32 {
    let (_, ops, _) = parse_flags(args);
    for f in inputs(&ops) {
        let data = match read_input(&f) {
            Ok(d) => d,
            Err(e) => return err("tac", &f, e),
        };
        for l in lines_of(&data).iter().rev() {
            println!("{}", l);
        }
    }
    0
}

/// sed: s///[gpI N], d, p, q, =; addresses N, $, /re/, ranges; -n -e -i -E.
pub fn sed(args: &[String]) -> i32 {
    let mut scripts = Vec::new();
    let mut ops = Vec::new();
    let mut quiet = false;
    let mut in_place = false;
    let mut extended = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => quiet = true,
            "-i" => in_place = true,
            "-E" | "-r" => extended = true,
            "-e" => {
                if let Some(s) = args.get(i + 1) {
                    scripts.push(s.clone());
                }
                i += 1;
            }
            a => {
                if scripts.is_empty() && !a.starts_with('-') {
                    scripts.push(a.to_string());
                } else {
                    ops.push(a.to_string());
                }
            }
        }
        i += 1;
    }
    let script = scripts.join(";");
    let cmds = match parse_sed(&script, extended) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sed: {}", e);
            return 1;
        }
    };
    for f in inputs(&ops) {
        let data = match read_input(&f) {
            Ok(d) => d,
            Err(e) => return err("sed", &f, e),
        };
        let lines = lines_of(&data);
        let mut out = String::new();
        let mut in_range = alloc::vec![false; cmds.len()];
        'lines: for (ln, line) in lines.iter().enumerate() {
            let mut pat = line.clone();
            let last = ln + 1 == lines.len();
            let mut deleted = false;
            for (ci, c) in cmds.iter().enumerate() {
                let sel = match (&c.addr1, &c.addr2) {
                    (None, _) => true,
                    (Some(a), None) => addr_match(a, ln + 1, last, &pat),
                    (Some(a), Some(b)) => {
                        if in_range[ci] {
                            if addr_match(b, ln + 1, last, &pat) {
                                in_range[ci] = false;
                            }
                            true
                        } else if addr_match(a, ln + 1, last, &pat) {
                            in_range[ci] = !addr_match(b, ln + 1, last, &pat);
                            true
                        } else {
                            false
                        }
                    }
                };
                if sel == c.negate {
                    continue;
                }
                match &c.op {
                    SedOp::Subst(re, rep, global, print) => {
                        let (new, changed) = substitute(re, rep, &pat, *global);
                        pat = new;
                        if changed && *print {
                            out.push_str(&pat);
                            out.push('\n');
                        }
                    }
                    SedOp::Delete => {
                        deleted = true;
                        break;
                    }
                    SedOp::Print => {
                        out.push_str(&pat);
                        out.push('\n');
                    }
                    SedOp::Quit => {
                        if !quiet {
                            out.push_str(&pat);
                            out.push('\n');
                        }
                        break 'lines;
                    }
                    SedOp::LineNo => out.push_str(&format!("{}\n", ln + 1)),
                }
            }
            if !deleted && !quiet {
                out.push_str(&pat);
                out.push('\n');
            }
        }
        if in_place && f != "-" {
            if let Err(e) = fs::write(&f, out.as_bytes()) {
                return err("sed", &f, e);
            }
        } else {
            print!("{}", out);
        }
    }
    0
}

enum SedAddr {
    Line(usize),
    Last,
    Re(Regex),
}

enum SedOp {
    Subst(Regex, String, bool, bool),
    Delete,
    Print,
    Quit,
    LineNo,
}

struct SedCmd {
    addr1: Option<SedAddr>,
    addr2: Option<SedAddr>,
    negate: bool,
    op: SedOp,
}

fn addr_match(a: &SedAddr, ln: usize, last: bool, line: &str) -> bool {
    match a {
        SedAddr::Line(n) => *n == ln,
        SedAddr::Last => last,
        SedAddr::Re(r) => r.is_match(line),
    }
}

fn parse_sed(s: &str, ext: bool) -> Result<Vec<SedCmd>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    let delimited = |i: &mut usize, d: char| -> String {
        let mut r = String::new();
        while *i < c.len() && c[*i] != d {
            if c[*i] == '\\' && *i + 1 < c.len() && c[*i + 1] == d {
                r.push(d);
                *i += 2;
                continue;
            }
            r.push(c[*i]);
            *i += 1;
        }
        *i += 1;
        r
    };
    let addr = |i: &mut usize| -> Result<Option<SedAddr>, String> {
        if *i >= c.len() {
            return Ok(None);
        }
        if c[*i].is_ascii_digit() {
            let mut n = 0;
            while *i < c.len() && c[*i].is_ascii_digit() {
                n = n * 10 + c[*i].to_digit(10).unwrap() as usize;
                *i += 1;
            }
            return Ok(Some(SedAddr::Line(n)));
        }
        if c[*i] == '$' {
            *i += 1;
            return Ok(Some(SedAddr::Last));
        }
        if c[*i] == '/' {
            *i += 1;
            let mut r = String::new();
            while *i < c.len() && c[*i] != '/' {
                r.push(c[*i]);
                *i += 1;
            }
            *i += 1;
            return Ok(Some(SedAddr::Re(Regex::new(&r, ext, false)?)));
        }
        Ok(None)
    };
    while i < c.len() {
        while i < c.len() && (c[i] == ';' || c[i].is_whitespace()) {
            i += 1;
        }
        if i >= c.len() {
            break;
        }
        let a1 = addr(&mut i)?;
        let mut a2 = None;
        if a1.is_some() && i < c.len() && c[i] == ',' {
            i += 1;
            a2 = addr(&mut i)?;
        }
        let mut negate = false;
        if i < c.len() && c[i] == '!' {
            negate = true;
            i += 1;
        }
        let Some(&cmd) = c.get(i) else { break };
        i += 1;
        let op = match cmd {
            's' => {
                let d = *c.get(i).ok_or("unterminated s")?;
                i += 1;
                let re = delimited(&mut i, d);
                let rep = delimited(&mut i, d);
                let mut global = false;
                let mut print = false;
                let mut icase = false;
                while i < c.len() && !matches!(c[i], ';' | '\n' | ' ') {
                    match c[i] {
                        'g' => global = true,
                        'p' => print = true,
                        'I' | 'i' => icase = true,
                        _ => {}
                    }
                    i += 1;
                }
                SedOp::Subst(Regex::new(&re, ext, icase)?, rep, global, print)
            }
            'd' => SedOp::Delete,
            'p' => SedOp::Print,
            'q' => SedOp::Quit,
            '=' => SedOp::LineNo,
            o => return Err(format!("unknown command: `{}'", o)),
        };
        out.push(SedCmd {
            addr1: a1,
            addr2: a2,
            negate,
            op,
        });
    }
    Ok(out)
}

fn substitute(re: &Regex, rep: &str, s: &str, global: bool) -> (String, bool) {
    let mut out = String::new();
    let mut pos = 0;
    let mut changed = false;
    while pos <= s.len() {
        let Some((st, en, caps)) = re.find_at(s, pos) else {
            break;
        };
        changed = true;
        out.push_str(&s[pos..st]);
        let mut chars = rep.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '&' => out.push_str(&s[st..en]),
                '\\' => match chars.next() {
                    Some(d @ '1'..='9') => {
                        let g = d as usize - '0' as usize;
                        if let Some(Some((a, b))) = caps.get(g) {
                            out.push_str(&s[*a..*b]);
                        }
                    }
                    Some('n') => out.push('\n'),
                    Some(o) => out.push(o),
                    None => out.push('\\'),
                },
                o => out.push(o),
            }
        }
        if en == st {
            if let Some(ch) = s[en..].chars().next() {
                out.push(ch);
                pos = en + ch.len_utf8();
            } else {
                pos = en + 1;
            }
        } else {
            pos = en;
        }
        if !global {
            break;
        }
    }
    if pos < s.len() {
        out.push_str(&s[pos..]);
    }
    (out, changed)
}

pub fn seq(args: &[String]) -> i32 {
    let nums: Vec<i64> = args[1..].iter().filter_map(|a| a.parse().ok()).collect();
    let (first, step, last) = match nums.len() {
        1 => (1, 1, nums[0]),
        2 => (nums[0], 1, nums[1]),
        3 => (nums[0], nums[1], nums[2]),
        _ => {
            eprintln!("usage: seq [FIRST [STEP]] LAST");
            return 1;
        }
    };
    if step == 0 {
        return 1;
    }
    let mut i = first;
    while (step > 0 && i <= last) || (step < 0 && i >= last) {
        println!("{}", i);
        i += step;
    }
    0
}

pub fn sort(args: &[String]) -> i32 {
    let mut flags = Vec::new();
    let mut ops = Vec::new();
    let mut key: Option<usize> = None;
    let mut sep: Option<char> = None;
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a == "-k" {
            key = args
                .get(i + 1)
                .and_then(|k| k.split(',').next()?.parse().ok());
            i += 2;
            continue;
        }
        if a == "-t" {
            sep = args.get(i + 1).and_then(|s| s.chars().next());
            i += 2;
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            flags.extend(a[1..].chars());
        } else {
            ops.push(a.clone());
        }
        i += 1;
    }
    let mut lines = Vec::new();
    for f in inputs(&ops) {
        match read_input(&f) {
            Ok(d) => lines.extend(lines_of(&d)),
            Err(e) => return err("sort", &f, e),
        }
    }
    let keyf = |l: &String| -> String {
        match key {
            Some(k) => {
                let fields: Vec<&str> = match sep {
                    Some(c) => l.split(c).collect(),
                    None => l.split_whitespace().collect(),
                };
                fields.get(k - 1).map(|s| s.to_string()).unwrap_or_default()
            }
            None => l.clone(),
        }
    };
    let numeric = flags.contains(&'n') || flags.contains(&'h');
    let fold = flags.contains(&'f');
    lines.sort_by(|a, b| {
        let (ka, kb) = (keyf(a), keyf(b));
        if numeric {
            let pa: f64 = parse_num(&ka);
            let pb: f64 = parse_num(&kb);
            pa.partial_cmp(&pb).unwrap_or(core::cmp::Ordering::Equal)
        } else if fold {
            ka.to_lowercase().cmp(&kb.to_lowercase())
        } else {
            ka.cmp(&kb)
        }
    });
    if flags.contains(&'r') {
        lines.reverse();
    }
    if flags.contains(&'u') {
        lines.dedup();
    }
    for l in lines {
        println!("{}", l);
    }
    0
}

fn parse_num(s: &str) -> f64 {
    let t = s.trim();
    let (num, mul) = match t.chars().last() {
        Some('K') => (&t[..t.len() - 1], 1024.0),
        Some('M') => (&t[..t.len() - 1], 1048576.0),
        Some('G') => (&t[..t.len() - 1], 1073741824.0),
        _ => (t, 1.0),
    };
    let mut end = 0;
    for (i, c) in num.char_indices() {
        if c.is_ascii_digit() || c == '.' || (i == 0 && c == '-') {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    num[..end].parse::<f64>().unwrap_or(0.0) * mul
}

pub fn tee(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let append = flags.contains(&'a');
    let files: Vec<fs::File> = ops
        .iter()
        .filter_map(|p| {
            let fl = fs::O_WRONLY | fs::O_CREAT | if append { fs::O_APPEND } else { fs::O_TRUNC };
            fs::File::open_with(p, fl, 0o644)
                .map_err(|e| err("tee", p, e))
                .ok()
        })
        .collect();
    let mut buf = alloc::vec![0u8; 8192];
    loop {
        match io::read(0, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let _ = io::write_all(1, &buf[..n]);
                for f in &files {
                    let _ = f.write_all(&buf[..n]);
                }
            }
            Err(rustos_rt::Error(4)) => {}
            Err(_) => break,
        }
    }
    0
}

fn expand_set(s: &str) -> Vec<char> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '[' && s[i..].starts_with("[:") {
            let rest: String = c[i..].iter().collect();
            if let Some(end) = rest.find(":]") {
                let name = &rest[2..end];
                let range: Vec<char> = match name {
                    "lower" => ('a'..='z').collect(),
                    "upper" => ('A'..='Z').collect(),
                    "digit" => ('0'..='9').collect(),
                    "alpha" => ('a'..='z').chain('A'..='Z').collect(),
                    "alnum" => ('a'..='z').chain('A'..='Z').chain('0'..='9').collect(),
                    "space" => alloc::vec![' ', '\t', '\n', '\r', '\x0b', '\x0c'],
                    _ => Vec::new(),
                };
                out.extend(range);
                i += end + 2;
                continue;
            }
        }
        if c[i] == '\\' && i + 1 < c.len() {
            out.push(match c[i + 1] {
                'n' => '\n',
                't' => '\t',
                o => o,
            });
            i += 2;
            continue;
        }
        if i + 2 < c.len() && c[i + 1] == '-' {
            for ch in c[i]..=c[i + 2] {
                out.push(ch);
            }
            i += 3;
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

pub fn tr(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let delete = flags.contains(&'d');
    let squeeze = flags.contains(&'s');
    let set1 = ops.first().map(|s| expand_set(s)).unwrap_or_default();
    let set2 = ops.get(1).map(|s| expand_set(s)).unwrap_or_default();
    let mut input = Vec::new();
    let _ = io::stdin().read_to_end(&mut input);
    let text = String::from_utf8_lossy(&input);
    let mut out = String::new();
    let mut last: Option<char> = None;
    for ch in text.chars() {
        let mapped = if let Some(pos) = set1.iter().position(|&c| c == ch) {
            if delete {
                continue;
            }
            if set2.is_empty() {
                ch
            } else {
                set2[pos.min(set2.len() - 1)]
            }
        } else {
            ch
        };
        if squeeze
            && last == Some(mapped)
            && (set2.contains(&mapped) || (set2.is_empty() && set1.contains(&mapped)))
        {
            continue;
        }
        out.push(mapped);
        last = Some(mapped);
    }
    print!("{}", out);
    0
}

pub fn uniq(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let data = match read_input(ops.first().map_or("-", |s| s.as_str())) {
        Ok(d) => d,
        Err(e) => return err("uniq", "input", e),
    };
    let lines = lines_of(&data);
    let mut groups: Vec<(String, usize)> = Vec::new();
    for l in lines {
        let eq = |a: &str, b: &str| {
            if flags.contains(&'i') {
                a.to_lowercase() == b.to_lowercase()
            } else {
                a == b
            }
        };
        match groups.last_mut() {
            Some((prev, n)) if eq(prev, &l) => *n += 1,
            _ => groups.push((l, 1)),
        }
    }
    for (l, n) in groups {
        if flags.contains(&'d') && n < 2 {
            continue;
        }
        if flags.contains(&'u') && n > 1 {
            continue;
        }
        if flags.contains(&'c') {
            println!("{:7} {}", n, l);
        } else {
            println!("{}", l);
        }
    }
    0
}

pub fn wc(args: &[String]) -> i32 {
    let (flags, ops, _) = parse_flags(args);
    let all = !flags.iter().any(|c| matches!(c, 'l' | 'w' | 'c' | 'm'));
    let mut totals = (0usize, 0usize, 0usize);
    let files = inputs(&ops);
    let mut st = 0;
    for f in &files {
        let data = match read_input(f) {
            Ok(d) => d,
            Err(e) => {
                st = err("wc", f, e);
                continue;
            }
        };
        let l = data.iter().filter(|&&b| b == b'\n').count();
        let w = String::from_utf8_lossy(&data).split_whitespace().count();
        let c = data.len();
        totals.0 += l;
        totals.1 += w;
        totals.2 += c;
        print_wc((l, w, c), &flags, all, if f == "-" { "" } else { f });
    }
    if files.len() > 1 {
        print_wc(totals, &flags, all, "total");
    }
    st
}

fn print_wc(v: (usize, usize, usize), flags: &[char], all: bool, name: &str) {
    let mut s = String::new();
    if all || flags.contains(&'l') {
        s.push_str(&format!("{:7} ", v.0));
    }
    if all || flags.contains(&'w') {
        s.push_str(&format!("{:7} ", v.1));
    }
    if all || flags.contains(&'c') || flags.contains(&'m') {
        s.push_str(&format!("{:7} ", v.2));
    }
    s.push_str(name);
    println!("{}", s.trim_end());
}

pub fn xargs(args: &[String]) -> i32 {
    let mut cmd: Vec<String> = Vec::new();
    let mut per = usize::MAX;
    let mut replace: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-n" => {
                per = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(usize::MAX);
                i += 2;
                continue;
            }
            "-I" => {
                replace = args.get(i + 1).cloned();
                i += 2;
                continue;
            }
            _ => {
                cmd.extend(args[i..].iter().cloned());
                break;
            }
        }
    }
    if cmd.is_empty() {
        cmd.push(String::from("echo"));
    }
    let mut input = Vec::new();
    let _ = io::stdin().read_to_end(&mut input);
    let text = String::from_utf8_lossy(&input);
    let mut st = 0;
    if let Some(r) = replace {
        for line in text.lines().filter(|l| !l.is_empty()) {
            let argv: Vec<String> = cmd.iter().map(|a| a.replace(&r, line)).collect();
            let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
            st |= process::run(&refs).unwrap_or(127);
        }
        return st;
    }
    let items: Vec<&str> = text.split_whitespace().collect();
    for chunk in items.chunks(per.min(items.len().max(1))) {
        let mut argv: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        argv.extend(chunk.iter());
        st |= process::run(&argv).unwrap_or(127);
    }
    if items.is_empty() {
        let argv: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        st = process::run(&argv).unwrap_or(127);
    }
    st
}

pub fn yes(args: &[String]) -> i32 {
    let s = if args.len() > 1 {
        args[1..].join(" ")
    } else {
        String::from("y")
    };
    let line = format!("{}\n", s).repeat(64);
    loop {
        io::flush();
        if io::write_all(1, line.as_bytes()).is_err() {
            return 0;
        }
    }
}
