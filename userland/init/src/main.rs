//! PID 1: runs /etc/rc, then keeps a login shell running on the console
//! and on every extra virtual console listed in /etc/ttys, and reaps
//! orphaned processes.

#![no_std]
#![no_main]

use rustos_rt::prelude::*;
use rustos_rt::{env, fs, process, signal};

rustos_rt::entry!(main);

fn main(_args: Vec<String>) -> i32 {
    if process::getpid() != 1 {
        eprintln!("init: must be run as PID 1");
        return 1;
    }
    // init ignores terminal job-control signals.
    for s in [
        signal::SIGINT,
        signal::SIGTSTP,
        signal::SIGTTIN,
        signal::SIGTTOU,
        signal::SIGQUIT,
    ] {
        signal::ignore(s);
    }
    env::set_var("PATH", "/bin:/sbin:/usr/bin");
    env::set_var("HOME", "/root");
    env::set_var("TERM", "vt100");
    env::set_var("SHELL", "/bin/sh");
    let _ = env::set_current_dir("/");

    if let Ok(name) = fs::read_to_string("/etc/hostname") {
        let _ = env::set_hostname(name.trim());
    }
    if fs::exists("/etc/rc") {
        run_and_wait(&["/bin/sh", "/etc/rc"]);
    }

    let shell = if fs::exists("/bin/sh") {
        "/bin/sh"
    } else {
        "/bin/rsh"
    };
    // Extra terminals: "NAME [login]" per line (e.g. "tty2").
    let conf = fs::read_to_string("/storage/etc/ttys")
        .or_else(|_| fs::read_to_string("/etc/ttys"))
        .unwrap_or_default();
    let mut console_login = false;
    let mut ttys: Vec<(String, bool)> = Vec::new();
    for l in conf
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let mut w = l.split_whitespace();
        let name = w.next().unwrap_or("");
        let login = w.next() == Some("login") && fs::exists("/bin/login");
        if name == "console" {
            console_login = login;
        } else {
            ttys.push((String::from(name), login));
        }
    }
    let program = |login: bool| if login { "/bin/login" } else { shell };
    let mut vc_pids: Vec<(String, bool, i32)> = Vec::new();
    for (t, login) in &ttys {
        if let Ok(pid) = spawn_on_tty(program(*login), t) {
            vc_pids.push((t.clone(), *login, pid));
        }
    }
    let shell = program(console_login);
    loop {
        if let Ok(motd) = fs::read_to_string("/etc/motd") {
            print!("{}", motd);
        }
        let pid = match spawn_session(shell) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("init: cannot start {}: {}", shell, e);
                rustos_rt::time::sleep_ms(5000);
                continue;
            }
        };
        // Reap everything until the console shell exits; respawn the
        // shells of the other consoles.
        loop {
            match process::waitpid(-1, 0) {
                Ok((p, _)) if p == pid => break,
                Ok((p, _)) => {
                    if let Some(i) = vc_pids.iter().position(|(_, _, v)| *v == p) {
                        let (t, login) = (vc_pids[i].0.clone(), vc_pids[i].1);
                        match spawn_on_tty(program(login), &t) {
                            Ok(n) => vc_pids[i].2 = n,
                            Err(_) => {
                                vc_pids.remove(i);
                            }
                        }
                    }
                }
                Err(_) => rustos_rt::time::sleep_ms(100),
            }
        }
        println!("\n[init] shell exited; restarting");
    }
}

/// Start a login shell on /dev/<tty> as its controlling terminal.
fn spawn_on_tty(shell: &str, tty: &str) -> rustos_rt::Result<i32> {
    let pid = process::fork()?;
    if pid == 0 {
        rustos_rt::io::discard_buffered();
        let _ = process::setsid();
        let path = format!("/dev/{}", tty);
        let Ok(f) = fs::File::open_with(&path, fs::O_RDWR, 0) else {
            process::exit(1);
        };
        for fd in 0..3 {
            let _ = process::dup2(f.fd(), fd);
        }
        drop(f);
        for s in [
            signal::SIGINT,
            signal::SIGTSTP,
            signal::SIGTTIN,
            signal::SIGTTOU,
            signal::SIGQUIT,
        ] {
            signal::default(s);
        }
        rustos_rt::term::tcsetpgrp(0, process::getpid());
        if let Ok(motd) = fs::read_to_string("/etc/motd") {
            print!("{}", motd);
        }
        let argv = vec![String::from(if shell.ends_with("login") {
            "login"
        } else {
            "-sh"
        })];
        let e = process::execve(shell, &argv, &env::environ());
        eprintln!("init: exec {}: {}", shell, e);
        process::exit(127);
    }
    Ok(pid)
}

fn spawn_session(shell: &str) -> rustos_rt::Result<i32> {
    let pid = process::fork()?;
    if pid == 0 {
        rustos_rt::io::discard_buffered();
        let _ = process::setsid();
        for s in [
            signal::SIGINT,
            signal::SIGTSTP,
            signal::SIGTTIN,
            signal::SIGTTOU,
            signal::SIGQUIT,
        ] {
            signal::default(s);
        }
        rustos_rt::term::tcsetpgrp(0, process::getpid());
        let argv = vec![String::from(if shell.ends_with("login") {
            "login"
        } else {
            "-sh"
        })];
        let e = process::execve(shell, &argv, &env::environ());
        eprintln!("init: exec {}: {}", shell, e);
        process::exit(127);
    }
    Ok(pid)
}

fn run_and_wait(argv: &[&str]) {
    if let Ok(code) = process::run(argv)
        && code != 0
    {
        eprintln!("init: {} exited with {}", argv.join(" "), code);
    }
}
