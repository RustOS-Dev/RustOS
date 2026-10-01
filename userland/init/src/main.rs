//! PID 1: runs /etc/rc, starts the enabled services and supervises them
//! (see svc.rs), keeps a login shell running on the console and on every
//! extra virtual console listed in /etc/ttys, and reaps orphaned
//! processes.

#![no_std]
#![no_main]

extern crate alloc;

mod svc;

use rustos_rt::prelude::*;
use rustos_rt::{env, fs, io, process, signal};

rustos_rt::entry!(main);

/// SIGCHLD only has to interrupt init's poll().
extern "C" fn on_sigchld(_: i32) {}

/// Longest sleep of the main loop (a SIGCHLD that arrives just before
/// poll() is noticed by then at the latest).
const MAX_WAIT_MS: u64 = 1_000;

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
    signal::handle(signal::SIGCHLD, on_sigchld);

    let mut services = svc::Supervisor::new();
    services.start_enabled();

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
    let claimed = services.claimed_ttys();
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
        } else if let Some((_, s)) = claimed.iter().find(|(t, _)| t == name) {
            svc::log(&format!("{}: used by service {}, no shell", name, s));
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
    let mut console: Option<i32> = None;
    let mut console_retry = 0u64;
    let mut first = true;
    loop {
        // (Re)start the console shell.
        if console.is_none() && rustos_rt::time::millis() >= console_retry {
            if !first {
                println!("\n[init] shell exited; restarting");
            }
            first = false;
            if let Ok(motd) = fs::read_to_string("/etc/motd") {
                print!("{}", motd);
            }
            match spawn_session(shell) {
                Ok(p) => console = Some(p),
                Err(e) => {
                    eprintln!("init: cannot start {}: {}", shell, e);
                    console_retry = rustos_rt::time::millis() + 5000;
                }
            }
            io::flush();
        }

        // Sleep until a child exits (SIGCHLD interrupts poll), a request
        // arrives or timed service work is due.
        let mut timeout = services
            .next_timeout()
            .unwrap_or(MAX_WAIT_MS)
            .min(MAX_WAIT_MS);
        if console.is_none() {
            timeout = timeout.min(console_retry.saturating_sub(rustos_rt::time::millis()));
        }
        let mut fds: Vec<io::PollFd> = services
            .control_fd()
            .map(|fd| io::PollFd {
                fd,
                events: io::POLLIN,
                revents: 0,
            })
            .into_iter()
            .collect();
        if fds.is_empty() {
            rustos_rt::time::sleep_ms(timeout);
        } else {
            let _ = io::poll(&mut fds, timeout as i32);
        }

        // Reap everything; respawn the shells of the consoles.
        while let Ok((p, status)) = process::waitpid(-1, process::WNOHANG) {
            if p <= 0 {
                break;
            }
            if console == Some(p) {
                console = None;
            } else if let Some(i) = vc_pids.iter().position(|(_, _, v)| *v == p) {
                let (t, login) = (vc_pids[i].0.clone(), vc_pids[i].1);
                match spawn_on_tty(program(login), &t) {
                    Ok(n) => vc_pids[i].2 = n,
                    Err(_) => {
                        vc_pids.remove(i);
                    }
                }
            } else {
                services.child_exited(p, status);
            }
        }
        services.handle_requests();
        services.tick();
    }
}

/// Start a login shell on /dev/<tty> as its controlling terminal.
fn spawn_on_tty(shell: &str, tty: &str) -> rustos_rt::Result<i32> {
    let pid = process::fork()?;
    if pid == 0 {
        io::discard_buffered();
        let _ = process::setsid();
        let path = format!("/dev/{}", tty);
        let Ok(f) = fs::File::open_with(&path, fs::O_RDWR, 0) else {
            process::exit(1);
        };
        for fd in 0..3 {
            let _ = process::dup2(f.fd(), fd);
        }
        drop(f);
        default_signals();
        rustos_rt::term::tcsetpgrp(0, process::getpid());
        if let Ok(motd) = fs::read_to_string("/etc/motd") {
            print!("{}", motd);
        }
        exec_shell(shell);
    }
    Ok(pid)
}

fn spawn_session(shell: &str) -> rustos_rt::Result<i32> {
    let pid = process::fork()?;
    if pid == 0 {
        io::discard_buffered();
        let _ = process::setsid();
        default_signals();
        rustos_rt::term::tcsetpgrp(0, process::getpid());
        exec_shell(shell);
    }
    Ok(pid)
}

/// Undo init's signal setup in a child.
fn default_signals() {
    for s in [
        signal::SIGINT,
        signal::SIGTSTP,
        signal::SIGTTIN,
        signal::SIGTTOU,
        signal::SIGQUIT,
        signal::SIGCHLD,
    ] {
        signal::default(s);
    }
}

fn exec_shell(shell: &str) -> ! {
    let argv = vec![String::from(if shell.ends_with("login") {
        "login"
    } else {
        "-sh"
    })];
    let e = process::execve(shell, &argv, &env::environ());
    eprintln!("init: exec {}: {}", shell, e);
    process::exit(127);
}

fn run_and_wait(argv: &[&str]) {
    if let Ok(code) = process::run(argv)
        && code != 0
    {
        eprintln!("init: {} exited with {}", argv.join(" "), code);
    }
}
