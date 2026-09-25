//! Built-in commands.

use crate::shell::{Flow, JobState, Shell};
use rustos_rt::prelude::*;
use rustos_rt::{env, fs, io, process, signal};

const BUILTINS: &[&str] = &[
    ":", ".", "[", "alias", "bg", "break", "cd", "continue", "echo", "eval", "exec", "exit",
    "export", "false", "fg", "help", "history", "jobs", "kill", "local", "printf", "pwd", "read",
    "readonly", "return", "set", "shift", "source", "test", "times", "trap", "true", "type",
    "umask", "unalias", "unset", "wait", "command",
];

pub fn is_builtin(name: &str) -> bool {
    BUILTINS.contains(&name)
}

pub fn names() -> &'static [&'static str] {
    BUILTINS
}

/// Split alias text into words (simple whitespace/quote handling).
pub fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                any = true;
            }
            None if c.is_whitespace() => {
                if !cur.is_empty() || any {
                    out.push(core::mem::take(&mut cur));
                    any = false;
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() || any {
        out.push(cur);
    }
    out
}

pub fn run(sh: &mut Shell, argv: &[String]) -> i32 {
    let args: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    match args[0] {
        ":" | "true" => 0,
        "false" => 1,
        "cd" => cd(sh, &args),
        "pwd" => {
            println!(
                "{}",
                env::current_dir().unwrap_or_else(|_| String::from("/"))
            );
            0
        }
        "echo" => echo(&args),
        "printf" => printf(&args[1..]),
        "exit" => {
            let code = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(sh.last_status);
            if sh.interactive
                && !sh.jobs.iter().all(|j| j.state == JobState::Done)
                && args.len() < 3
            {
                // Behave like bash: warn once about stopped jobs.
                if sh.jobs.iter().any(|j| j.state == JobState::Stopped) {
                    eprintln!("There are stopped jobs.");
                    for j in sh.jobs.iter() {
                        let _ = process::kill(-j.pgid, signal::SIGHUP);
                        let _ = process::kill(-j.pgid, signal::SIGCONT);
                    }
                }
            }
            sh.flow = Some(Flow::Exit(code));
            code
        }
        "export" | "readonly" => {
            if args.len() == 1 || args[1] == "-p" {
                for (k, v) in sh.vars.iter().filter(|(_, v)| v.exported) {
                    println!("export {}=\"{}\"", k, v.value);
                }
                return 0;
            }
            for a in &args[1..] {
                match a.split_once('=') {
                    Some((k, v)) => {
                        sh.set_var(k, v);
                        sh.export(k);
                    }
                    None => sh.export(a),
                }
            }
            0
        }
        "unset" => {
            for a in args[1..].iter().filter(|a| !a.starts_with('-')) {
                sh.unset(a);
            }
            0
        }
        "local" => {
            for a in &args[1..] {
                let (k, v) = a.split_once('=').unwrap_or((a, ""));
                let old = sh.vars.get(k).cloned();
                if let Some(frame) = sh.locals.last_mut() {
                    frame.push((k.to_string(), old));
                }
                sh.set_var(k, v);
            }
            0
        }
        "set" => set(sh, &args),
        "shift" => {
            let n = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1usize);
            if n > sh.positional.len() {
                return 1;
            }
            sh.positional.drain(..n);
            0
        }
        "source" | "." => {
            let Some(path) = args.get(1) else {
                eprintln!("{}: filename argument required", args[0]);
                return 2;
            };
            let full = if path.contains('/') {
                path.to_string()
            } else {
                process::find_in_path(path).unwrap_or_else(|| path.to_string())
            };
            match fs::read_to_string(&full) {
                Ok(src) => {
                    let saved = if args.len() > 2 {
                        Some(core::mem::replace(&mut sh.positional, argv[2..].to_vec()))
                    } else {
                        None
                    };
                    let st = sh.run_source(&src);
                    if let Some(p) = saved {
                        sh.positional = p;
                    }
                    if sh.flow == Some(Flow::Return) {
                        sh.flow = None;
                    }
                    st
                }
                Err(e) => {
                    eprintln!("sh: {}: {}", path, e);
                    1
                }
            }
        }
        "eval" => {
            let src = args[1..].join(" ");
            sh.run_source(&src)
        }
        "exec" => {
            if args.len() == 1 {
                return 0;
            }
            let Some(path) = process::find_in_path(args[1]) else {
                eprintln!("sh: exec: {}: not found", args[1]);
                return 127;
            };
            io::flush();
            let e = process::execve(&path, &argv[1..], &sh.environ());
            eprintln!("sh: exec: {}: {}", args[1], e);
            126
        }
        "return" => {
            let code = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(sh.last_status);
            sh.flow = Some(Flow::Return);
            code
        }
        "break" | "continue" => {
            if sh.loop_depth == 0 {
                eprintln!("sh: {}: only meaningful in a loop", args[0]);
                return 0;
            }
            let n = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(1u32)
                .max(1);
            sh.flow = Some(if args[0] == "break" {
                Flow::Break(n)
            } else {
                Flow::Continue(n)
            });
            0
        }
        "alias" => {
            if args.len() == 1 {
                for (k, v) in &sh.aliases {
                    println!("alias {}='{}'", k, v);
                }
                return 0;
            }
            let mut st = 0;
            for a in &args[1..] {
                match a.split_once('=') {
                    Some((k, v)) => {
                        sh.aliases.insert(k.to_string(), v.to_string());
                    }
                    None => match sh.aliases.get(*a) {
                        Some(v) => println!("alias {}='{}'", a, v),
                        None => {
                            eprintln!("sh: alias: {}: not found", a);
                            st = 1;
                        }
                    },
                }
            }
            st
        }
        "unalias" => {
            for a in &args[1..] {
                if *a == "-a" {
                    sh.aliases.clear();
                } else {
                    sh.aliases.remove(*a);
                }
            }
            0
        }
        "jobs" => {
            sh.reap_jobs(false);
            for j in sh.jobs.iter_mut() {
                let state = crate::shell::job_status(j);
                if args.get(1) == Some(&"-l") {
                    println!("[{}]+ {} {:<22} {}", j.id, j.pgid, state, j.text);
                } else {
                    println!("[{}]+  {:<22}  {}", j.id, state, j.text);
                }
                j.notified = true;
            }
            sh.jobs.retain(|j| j.state != JobState::Done);
            0
        }
        "fg" | "bg" => {
            sh.reap_jobs(false);
            let idx = match job_index(sh, args.get(1).copied()) {
                Some(i) => i,
                None => {
                    eprintln!("sh: {}: no such job", args[0]);
                    return 1;
                }
            };
            if args[0] == "fg" {
                sh.foreground_job(idx)
            } else {
                let j = &mut sh.jobs[idx];
                j.state = JobState::Running;
                eprintln!("[{}]+ {} &", j.id, j.text);
                let _ = process::kill(-j.pgid, signal::SIGCONT);
                0
            }
        }
        "wait" => {
            if args.len() == 1 {
                let mut st = 0;
                while let Ok((pid, s)) = process::waitpid(-1, 0) {
                    st = process::status_code(s);
                    for j in sh.jobs.iter_mut() {
                        if let Some(p) = j.procs.iter_mut().find(|p| p.0 == pid) {
                            p.1 = true;
                            p.2 = s;
                        }
                    }
                }
                sh.jobs.clear();
                return st;
            }
            let mut st = 0;
            for a in &args[1..] {
                let pid = if let Some(i) = job_index(sh, Some(a)).filter(|_| a.starts_with('%')) {
                    sh.jobs[i].pgid
                } else {
                    a.parse().unwrap_or(0)
                };
                match process::waitpid(pid, 0) {
                    Ok((_, s)) => st = process::status_code(s),
                    Err(_) => st = 127,
                }
                sh.jobs.retain(|j| j.pgid != pid);
            }
            st
        }
        "kill" => kill(sh, &args),
        "read" => read(sh, &args),
        "umask" => {
            match args.get(1) {
                Some(m) => {
                    if let Ok(v) = u32::from_str_radix(m, 8) {
                        process::umask(v);
                    }
                }
                None => {
                    let cur = process::umask(0o022);
                    process::umask(cur);
                    println!("{:04o}", cur);
                }
            }
            0
        }
        "type" | "command" => {
            if args[0] == "command" {
                // command -v NAME / command NAME args
                if args.get(1) == Some(&"-v") {
                    let mut st = 0;
                    for a in &args[2..] {
                        if is_builtin(a) || sh.functions.contains_key(*a) {
                            println!("{}", a);
                        } else if let Some(p) = process::find_in_path(a) {
                            println!("{}", p);
                        } else {
                            st = 1;
                        }
                    }
                    return st;
                }
                if args.len() > 1 {
                    let Some(path) = process::find_in_path(args[1]) else {
                        eprintln!("sh: {}: command not found", args[1]);
                        return 127;
                    };
                    return process::run(&args[1..])
                        .unwrap_or(126)
                        .max(if path.is_empty() { 127 } else { 0 });
                }
                return 0;
            }
            let mut st = 0;
            for a in &args[1..] {
                if let Some(v) = sh.aliases.get(*a) {
                    println!("{} is aliased to `{}'", a, v);
                } else if sh.functions.contains_key(*a) {
                    println!("{} is a function", a);
                } else if is_builtin(a) {
                    println!("{} is a shell builtin", a);
                } else if let Some(p) = process::find_in_path(a) {
                    println!("{} is {}", a, p);
                } else {
                    println!("sh: type: {}: not found", a);
                    st = 1;
                }
            }
            st
        }
        "history" => {
            if args.get(1) == Some(&"-c") {
                sh.history.clear();
                return 0;
            }
            for (i, h) in sh.history.iter().enumerate() {
                println!("{:5}  {}", i + 1, h);
            }
            0
        }
        "times" => {
            println!("0m0.000s 0m0.000s\n0m0.000s 0m0.000s");
            0
        }
        "trap" => {
            // Minimal: `trap '' SIG` ignores, `trap - SIG` restores default.
            if args.len() >= 3 {
                for s in &args[2..] {
                    if let Some(sig) = signal::parse(s) {
                        match args[1] {
                            "" => signal::ignore(sig),
                            "-" => signal::default(sig),
                            _ => {}
                        }
                    }
                }
            }
            0
        }
        "test" | "[" => crate::test::test(&args),
        "help" => {
            println!("RustOS sh, a POSIX-style shell. Built-in commands:");
            let mut line = String::from(" ");
            for b in BUILTINS {
                if line.len() + b.len() > 72 {
                    println!("{}", line);
                    line = String::from(" ");
                }
                line.push(' ');
                line.push_str(b);
            }
            println!("{}", line);
            println!(
                "Supports pipes, redirections (< > >> 2> &> 2>&1 <<), $VAR, $(cmd), $((expr)),"
            );
            println!(
                "globs, &&/||, if/while/until/for/case, functions, and job control (& jobs fg bg)."
            );
            println!("Programs in /bin: ls cat grep ps kill mount ping wget wifi ... ('ls /bin').");
            0
        }
        _ => 127,
    }
}

fn job_index(sh: &Shell, spec: Option<&str>) -> Option<usize> {
    match spec {
        None | Some("%%") | Some("%+") => (!sh.jobs.is_empty()).then(|| sh.jobs.len() - 1),
        Some("%-") => (sh.jobs.len() >= 2).then(|| sh.jobs.len() - 2),
        Some(s) => {
            let n: usize = s.trim_start_matches('%').parse().ok()?;
            sh.jobs.iter().position(|j| j.id == n)
        }
    }
}

fn cd(sh: &mut Shell, args: &[&str]) -> i32 {
    let target = match args.get(1) {
        None => sh.get_var("HOME").unwrap_or_else(|| String::from("/")),
        Some(&"-") => {
            let old = sh.get_var("OLDPWD").unwrap_or_else(|| String::from("/"));
            println!("{}", old);
            old
        }
        Some(p) => p.to_string(),
    };
    let old = env::current_dir().unwrap_or_default();
    match env::set_current_dir(&target) {
        Ok(()) => {
            let new = env::current_dir().unwrap_or(target);
            sh.set_var("OLDPWD", &old);
            sh.export("OLDPWD");
            sh.set_var("PWD", &new);
            sh.export("PWD");
            0
        }
        Err(e) => {
            eprintln!("sh: cd: {}: {}", target, e);
            1
        }
    }
}

fn unescape_echo(s: &str, out: &mut String) -> bool {
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('a') => out.push('\x07'),
            Some('b') => out.push('\x08'),
            Some('e') => out.push('\x1b'),
            Some('\\') => out.push('\\'),
            Some('c') => return false,
            Some('0') => {
                let mut v = 0u32;
                for _ in 0..3 {
                    match chars.peek() {
                        Some(d @ '0'..='7') => {
                            v = v * 8 + d.to_digit(8).unwrap();
                            chars.next();
                        }
                        _ => break,
                    }
                }
                out.push(char::from_u32(v).unwrap_or('?'));
            }
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    true
}

fn echo(args: &[&str]) -> i32 {
    let mut newline = true;
    let mut escapes = false;
    let mut i = 1;
    while i < args.len() {
        let a = args[i];
        if a.len() > 1 && a.starts_with('-') && a[1..].chars().all(|c| matches!(c, 'n' | 'e' | 'E'))
        {
            for c in a[1..].chars() {
                match c {
                    'n' => newline = false,
                    'e' => escapes = true,
                    _ => escapes = false,
                }
            }
            i += 1;
        } else {
            break;
        }
    }
    let mut out = String::new();
    for (j, a) in args[i..].iter().enumerate() {
        if j > 0 {
            out.push(' ');
        }
        if escapes {
            if !unescape_echo(a, &mut out) {
                print!("{}", out);
                return 0;
            }
        } else {
            out.push_str(a);
        }
    }
    if newline {
        out.push('\n');
    }
    print!("{}", out);
    0
}

/// printf FORMAT [ARGS...] (supports %s %d %i %u %x %X %o %c %% %b and
/// width/precision/flags).
pub fn printf(args: &[&str]) -> i32 {
    let Some(fmt) = args.first() else {
        eprintln!("printf: usage: printf format [arguments]");
        return 2;
    };
    let mut argi = 1;
    let mut out = String::new();
    loop {
        let chars: Vec<char> = fmt.chars().collect();
        let mut i = 0;
        let consumed_before = argi;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                let mut s = String::new();
                let mut esc = String::from("\\");
                if i + 1 < chars.len() {
                    esc.push(chars[i + 1]);
                }
                unescape_echo(&esc, &mut s);
                out.push_str(&s);
                i += 2;
                continue;
            }
            if c != '%' {
                out.push(c);
                i += 1;
                continue;
            }
            i += 1;
            if i < chars.len() && chars[i] == '%' {
                out.push('%');
                i += 1;
                continue;
            }
            let mut flags = String::new();
            while i < chars.len() && "-+ 0#".contains(chars[i]) {
                flags.push(chars[i]);
                i += 1;
            }
            let mut width = String::new();
            while i < chars.len() && chars[i].is_ascii_digit() {
                width.push(chars[i]);
                i += 1;
            }
            let mut prec: Option<usize> = None;
            if i < chars.len() && chars[i] == '.' {
                i += 1;
                let mut p = String::new();
                while i < chars.len() && chars[i].is_ascii_digit() {
                    p.push(chars[i]);
                    i += 1;
                }
                prec = Some(p.parse().unwrap_or(0));
            }
            let Some(&conv) = chars.get(i) else { break };
            i += 1;
            let arg = args.get(argi).copied().unwrap_or("");
            argi += 1;
            let num = || -> i64 {
                if let Some(c) = arg.strip_prefix('\'') {
                    return c.chars().next().map_or(0, |c| c as i64);
                }
                if let Some(h) = arg.strip_prefix("0x") {
                    return i64::from_str_radix(h, 16).unwrap_or(0);
                }
                arg.parse().unwrap_or(0)
            };
            let mut s = match conv {
                's' => match prec {
                    Some(p) => arg.chars().take(p).collect(),
                    None => arg.to_string(),
                },
                'b' => {
                    let mut s = String::new();
                    unescape_echo(arg, &mut s);
                    s
                }
                'd' | 'i' => {
                    let v = num();
                    if flags.contains('+') && v >= 0 {
                        format!("+{}", v)
                    } else {
                        format!("{}", v)
                    }
                }
                'u' => format!("{}", num() as u64),
                'x' => format!("{:x}", num()),
                'X' => format!("{:X}", num()),
                'o' => format!("{:o}", num()),
                'c' => arg.chars().next().map(String::from).unwrap_or_default(),
                _ => {
                    argi -= 1;
                    format!("%{}", conv)
                }
            };
            if let Ok(w) = width.parse::<usize>() {
                let len = s.chars().count();
                if len < w {
                    let pad = w - len;
                    if flags.contains('-') {
                        s.push_str(&" ".repeat(pad));
                    } else if flags.contains('0') && "diuxXo".contains(conv) {
                        let neg = s.starts_with('-');
                        let body = s.trim_start_matches('-').to_string();
                        s = format!("{}{}{}", if neg { "-" } else { "" }, "0".repeat(pad), body);
                    } else {
                        s = format!("{}{}", " ".repeat(pad), s);
                    }
                }
            }
            out.push_str(&s);
        }
        // Reuse the format while arguments remain.
        if argi >= args.len() || argi == consumed_before {
            break;
        }
    }
    print!("{}", out);
    0
}

fn set(sh: &mut Shell, args: &[&str]) -> i32 {
    if args.len() == 1 {
        for (k, v) in &sh.vars {
            println!("{}='{}'", k, v.value);
        }
        return 0;
    }
    let mut i = 1;
    while i < args.len() {
        let a = args[i];
        if a == "--" {
            sh.positional = args[i + 1..].iter().map(|s| s.to_string()).collect();
            return 0;
        }
        if (a.starts_with('-') || a.starts_with('+')) && a.len() > 1 {
            let on = a.starts_with('-');
            if a == "-o" || a == "+o" {
                if let Some(opt) = args.get(i + 1) {
                    match *opt {
                        "errexit" => sh.opt_errexit = on,
                        "xtrace" => sh.opt_xtrace = on,
                        "nounset" => sh.opt_nounset = on,
                        _ => {}
                    }
                    i += 2;
                    continue;
                }
            }
            for c in a[1..].chars() {
                match c {
                    'e' => sh.opt_errexit = on,
                    'x' => sh.opt_xtrace = on,
                    'u' => sh.opt_nounset = on,
                    _ => {}
                }
            }
            i += 1;
        } else {
            sh.positional = args[i..].iter().map(|s| s.to_string()).collect();
            return 0;
        }
    }
    0
}

fn kill(sh: &mut Shell, args: &[&str]) -> i32 {
    let mut sig = signal::SIGTERM;
    let mut i = 1;
    if args.get(1) == Some(&"-l") {
        println!(
            "HUP INT QUIT ILL TRAP ABRT BUS FPE KILL USR1 SEGV USR2 PIPE ALRM TERM STKFLT CHLD CONT STOP TSTP TTIN TTOU"
        );
        return 0;
    }
    if let Some(a) = args.get(1)
        && a.starts_with('-')
    {
        let s = if *a == "-s" {
            i += 1;
            args.get(2).copied().unwrap_or("TERM")
        } else {
            &a[1..]
        };
        match signal::parse(s) {
            Some(n) => sig = n,
            None => {
                eprintln!("kill: {}: invalid signal", s);
                return 1;
            }
        }
        i += 1;
    }
    if i >= args.len() {
        eprintln!("kill: usage: kill [-s sig | -sig] pid | %job ...");
        return 2;
    }
    let mut st = 0;
    for a in &args[i..] {
        let pid = if a.starts_with('%') {
            match job_index(sh, Some(a)) {
                Some(j) => -sh.jobs[j].pgid,
                None => {
                    eprintln!("kill: {}: no such job", a);
                    st = 1;
                    continue;
                }
            }
        } else {
            match a.parse::<i32>() {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("kill: {}: arguments must be process or job IDs", a);
                    st = 1;
                    continue;
                }
            }
        };
        if let Err(e) = process::kill(pid, sig) {
            eprintln!("kill: ({}) - {}", pid, e);
            st = 1;
        } else if sig == signal::SIGCONT || sig == signal::SIGKILL || sig == signal::SIGTERM {
            for j in sh.jobs.iter_mut() {
                if -j.pgid == pid && sig == signal::SIGCONT {
                    j.state = JobState::Running;
                }
            }
        }
    }
    st
}

fn read(sh: &mut Shell, args: &[&str]) -> i32 {
    let mut prompt = None;
    let mut raw = false;
    let mut names = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i] {
            "-p" => {
                prompt = args.get(i + 1).copied();
                i += 1;
            }
            "-r" => raw = true,
            n => names.push(n),
        }
        i += 1;
    }
    if let Some(p) = prompt {
        eprint!("{}", p);
    }
    let Some(mut line) = io::read_line() else {
        return 1;
    };
    if !raw {
        line = line.replace("\\\n", "");
    }
    if names.is_empty() {
        sh.set_var("REPLY", &line);
        return 0;
    }
    let mut fields: Vec<&str> = line.split_whitespace().collect();
    for (j, n) in names.iter().enumerate() {
        if j + 1 == names.len() {
            let rest = fields.join(" ");
            sh.set_var(n, &rest);
        } else if fields.is_empty() {
            sh.set_var(n, "");
        } else {
            sh.set_var(n, fields.remove(0));
        }
    }
    0
}
