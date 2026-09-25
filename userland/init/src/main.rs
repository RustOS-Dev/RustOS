//! PID 1: runs /etc/rc, then keeps a login shell running on the console and
//! reaps orphaned processes.

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
        // Reap everything until the login shell exits.
        loop {
            match process::waitpid(-1, 0) {
                Ok((p, _)) if p == pid => break,
                Ok(_) => {}
                Err(_) => rustos_rt::time::sleep_ms(100),
            }
        }
        println!("\n[init] shell exited; restarting");
    }
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
        let argv = vec![String::from("-sh")];
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
