//! sh: the RustOS shell.
//!
//! Usage: `sh` (interactive), `sh FILE [ARGS]`, `sh -c COMMAND [ARGS]`.

#![no_std]
#![no_main]

extern crate alloc;

mod ast;
mod builtins;
mod editor;
mod expand;
mod parser;
mod shell;
mod test;

use rustos_rt::prelude::*;
use rustos_rt::{env, fs, io, process, signal};
use shell::{Flow, Shell};

rustos_rt::entry!(main);

fn main(args: Vec<String>) -> i32 {
    let login = args.first().is_some_and(|a| a.starts_with('-'));
    let mut i = 1;
    let mut command: Option<String> = None;
    let mut force_interactive = false;
    let (mut e, mut x) = (false, false);
    while i < args.len() && args[i].starts_with('-') && args[i].len() > 1 {
        match args[i].as_str() {
            "-c" => {
                command = args.get(i + 1).cloned();
                i += 2;
                break;
            }
            "-i" => force_interactive = true,
            "-l" | "--login" => {}
            "-e" => e = true,
            "-x" => x = true,
            "--" => {
                i += 1;
                break;
            }
            other => {
                eprintln!("sh: {}: invalid option", other);
                return 2;
            }
        }
        i += 1;
    }

    if let Some(cmd) = command {
        let mut sh = Shell::new(args.get(i).map_or("sh", |s| s.as_str()), false);
        sh.positional = args.iter().skip(i + 1).cloned().collect();
        sh.opt_errexit = e;
        sh.opt_xtrace = x;
        let st = sh.run_source(&cmd);
        return finish(&mut sh, st);
    }

    if let Some(script) = args.get(i) {
        let src = match fs::read_to_string(script) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("sh: {}: {}", script, err);
                return 127;
            }
        };
        let mut sh = Shell::new(script, false);
        sh.positional = args[i + 1..].to_vec();
        sh.opt_errexit = e;
        sh.opt_xtrace = x;
        let st = sh.run_source(&src);
        return finish(&mut sh, st);
    }

    let interactive = force_interactive || io::isatty(0);
    let mut sh = Shell::new(&args[0], interactive);
    sh.opt_errexit = e;
    sh.opt_xtrace = x;
    if interactive {
        for s in [
            signal::SIGINT,
            signal::SIGQUIT,
            signal::SIGTSTP,
            signal::SIGTTIN,
            signal::SIGTTOU,
        ] {
            signal::ignore(s);
        }
        // Take the terminal: own process group in the foreground.
        let me = process::getpid();
        if process::getpgrp() != me {
            let _ = process::setpgid(0, 0);
        }
        sh.shell_pgid = process::getpgrp();
        rustos_rt::term::tcsetpgrp(0, sh.shell_pgid);
    }
    if login {
        for rc in ["/etc/profile", "/root/.profile"] {
            if fs::exists(rc)
                && let Ok(src) = fs::read_to_string(rc)
            {
                sh.run_source(&src);
            }
        }
    }
    if interactive && let Ok(src) = fs::read_to_string("/root/.shrc") {
        sh.run_source(&src);
    }
    if let Ok(h) = fs::read_to_string("/root/.sh_history") {
        sh.history = h.lines().map(String::from).collect();
    }
    repl(&mut sh);
    let st = sh.last_status;
    finish(&mut sh, st)
}

fn finish(sh: &mut Shell, st: i32) -> i32 {
    io::flush();
    match sh.flow {
        Some(Flow::Exit(code)) => code,
        _ => st,
    }
}

fn prompt(sh: &Shell) -> String {
    let ps1 = sh.get_var("PS1").unwrap_or_else(|| String::from("$ "));
    let mut out = String::new();
    let mut chars = ps1.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => out.push_str(&sh.get_var("USER").unwrap_or_else(|| String::from("root"))),
            Some('h') => {
                let h = env::hostname();
                out.push_str(h.split('.').next().unwrap_or(&h));
            }
            Some('H') => out.push_str(&env::hostname()),
            Some('w') | Some('W') => {
                let cwd = env::current_dir().unwrap_or_else(|_| String::from("?"));
                let home = sh.get_var("HOME").unwrap_or_default();
                if !home.is_empty() && (cwd == home || cwd.starts_with(&format!("{}/", home))) {
                    out.push('~');
                    out.push_str(&cwd[home.len()..]);
                } else {
                    out.push_str(&cwd);
                }
            }
            Some('$') => out.push('#'),
            Some('n') => out.push('\n'),
            Some('e') => out.push('\x1b'),
            Some('[') | Some(']') => {}
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn repl(sh: &mut Shell) {
    loop {
        if sh.interactive {
            sh.reap_jobs(true);
        }
        let mut src = String::new();
        let mut ps = prompt(sh);
        loop {
            let line = if sh.interactive {
                let history = sh.history.clone();
                let funcs: Vec<String> = sh.functions.keys().cloned().collect();
                let aliases: Vec<String> = sh.aliases.keys().cloned().collect();
                let completer = move |w: &str| complete_command(w, &funcs, &aliases);
                match editor::read_line(&ps, &history, &completer) {
                    editor::Line::Text(t) => Some(t),
                    editor::Line::Eof => None,
                    editor::Line::Interrupted => {
                        src.clear();
                        sh.last_status = 130;
                        break;
                    }
                }
            } else {
                io::read_line()
            };
            let Some(line) = line else {
                if src.is_empty() {
                    if sh.interactive {
                        println!("exit");
                    }
                    save_history(sh);
                    return;
                }
                // Unterminated input at EOF.
                sh.run_source(&src);
                return;
            };
            src.push_str(&line);
            src.push('\n');
            match parser::parse(&src) {
                Err(e) if e.0 == "incomplete" => {
                    ps = sh.get_var("PS2").unwrap_or_else(|| String::from("> "));
                    continue;
                }
                _ => break,
            }
        }
        let trimmed = src.trim();
        if trimmed.is_empty() {
            continue;
        }
        if sh.interactive {
            let entry = trimmed.replace('\n', " ");
            if sh.history.last() != Some(&entry) {
                sh.history.push(entry);
                if sh.history.len() > 500 {
                    sh.history.remove(0);
                }
            }
        }
        sh.run_source(&src);
        io::flush();
        if let Some(Flow::Exit(_)) = sh.flow {
            save_history(sh);
            return;
        }
        // Stray break/continue/return at top level are ignored.
        sh.flow = None;
    }
}

fn save_history(sh: &Shell) {
    if sh.interactive && fs::is_dir("/root") {
        let mut s = String::new();
        for h in sh
            .history
            .iter()
            .rev()
            .take(200)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            s.push_str(h);
            s.push('\n');
        }
        let _ = fs::write("/root/.sh_history", s.as_bytes());
    }
}

fn complete_command(word: &str, funcs: &[String], aliases: &[String]) -> Vec<String> {
    let mut out: Vec<String> = builtins::names()
        .iter()
        .filter(|b| b.starts_with(word))
        .map(|b| String::from(*b))
        .collect();
    out.extend(funcs.iter().filter(|f| f.starts_with(word)).cloned());
    out.extend(aliases.iter().filter(|f| f.starts_with(word)).cloned());
    let path = env::var("PATH").unwrap_or_else(|| String::from("/bin"));
    for dir in path.split(':') {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries {
                if e.name.starts_with(word) {
                    out.push(e.name);
                }
            }
        }
    }
    out
}
