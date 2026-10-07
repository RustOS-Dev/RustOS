//! svc: list, inspect and control the services init supervises.
//!
//! Every subcommand is a request to init on the AF_UNIX socket
//! /run/svc.sock (svcconf::Request), one per connection, answered with
//! the result and, for list/status, the services' state
//! (svcconf::Response). init decides from the caller's uid what it may
//! do: root everything, other users only list and status.
//! Exit status: 0 ok, 1 error, 3 unknown service (docs/SERVICES.md).

use rustos_rt::net::{self, Socket};
use rustos_rt::prelude::*;
use rustos_rt::{io, signal, time};
use svcconf::{Command, Entry, Reply, Request, Response, State};

const USAGE: &str = "usage: svc list [--json] | svc status NAME [--json] | svc start|stop|restart|enable|disable NAME";
const UNKNOWN: i32 = 3;
const NOT_RUNNING: &str = "the service manager is not running";
/// How long to wait for init's answer.
const ANSWER_MS: u64 = 10_000;

/// Send one request to init and read its whole answer (init closes the
/// connection after it).
fn request(req: &Request) -> Result<Response, String> {
    signal::ignore(signal::SIGPIPE);
    let sock = Socket::new(net::AF_UNIX, net::SOCK_STREAM, 0).map_err(|_| NOT_RUNNING)?;
    sock.connect_raw(&net::unix_addr(svcconf::SOCKET))
        .map_err(|_| NOT_RUNNING)?;
    sock.send_all(req.render().as_bytes())
        .map_err(|e| format!("{}: {}", svcconf::SOCKET, e))?;
    let deadline = time::millis() + ANSWER_MS;
    let mut answer = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let left = deadline.saturating_sub(time::millis());
        if left == 0 {
            return Err(String::from("no answer from the service manager"));
        }
        let mut fds = [io::PollFd {
            fd: sock.fd(),
            events: io::POLLIN,
            revents: 0,
        }];
        if !matches!(io::poll(&mut fds, left as i32), Ok(n) if n > 0) {
            continue;
        }
        match sock.recv(&mut buf) {
            Ok(0) => break,
            Ok(n) => answer.extend_from_slice(&buf[..n]),
            // EINTR, EAGAIN
            Err(e) if e.0 == 4 || e.0 == 11 => {}
            Err(e) => return Err(format!("{}: {}", svcconf::SOCKET, e)),
        }
    }
    let text = String::from_utf8_lossy(&answer);
    Response::parse(&text)
        .ok_or_else(|| format!("bad reply '{}'", text.lines().next().unwrap_or("").trim()))
}

/// Report an unsuccessful request about service `name`; the exit status.
fn failure(name: &str, r: Result<Reply, String>) -> i32 {
    match r {
        Ok(Reply::Ok) => 0,
        Ok(Reply::Unknown) => unknown(name),
        Ok(Reply::Denied) => {
            eprintln!("svc: permission denied");
            1
        }
        Ok(Reply::Error(e)) => {
            eprintln!("svc: {}: {}", name, e);
            1
        }
        Err(e) => {
            eprintln!("svc: {}", e);
            1
        }
    }
}

/// The entries of a `list` or `status` answer, or the exit status.
fn query(req: Request) -> Result<Vec<Entry>, i32> {
    let name = match &req {
        Request::Status(n) => n.clone(),
        _ => String::new(),
    };
    match request(&req) {
        Ok(Response {
            reply: Reply::Ok,
            entries,
        }) => Ok(entries),
        Ok(r) => Err(failure(&name, Ok(r.reply))),
        Err(e) => Err(failure(&name, Err(e))),
    }
}

/// The state of service `name`, or the exit status.
fn status_of(name: &str) -> Result<Entry, i32> {
    query(Request::Status(String::from(name)))?
        .into_iter()
        .find(|e| e.name == name)
        .ok_or_else(|| unknown(name))
}

fn unknown(name: &str) -> i32 {
    eprintln!("svc: unknown service '{}'", name);
    UNKNOWN
}

pub fn svc(args: &[String]) -> i32 {
    let json = args.iter().skip(1).any(|a| a == "--json");
    let rest: Vec<&str> = args
        .iter()
        .skip(1)
        .map(|s| s.as_str())
        .filter(|a| *a != "--json")
        .collect();
    let r = match rest.as_slice() {
        ["list"] => list(json),
        // Not a name any service can have.
        ["status", name] if !svcconf::valid_name(name) => Err(unknown(name)),
        ["status", name] => status(name, json),
        [cmd, name] => match Command::parse(cmd) {
            Some(_) if !svcconf::valid_name(name) => Err(unknown(name)),
            Some(c) => control(c, name),
            None => Err(usage()),
        },
        _ => Err(usage()),
    };
    r.unwrap_or_else(|code| code)
}

fn usage() -> i32 {
    eprintln!("{}", USAGE);
    1
}

fn or_dash(v: Option<String>) -> String {
    v.unwrap_or_else(|| String::from("-"))
}

fn list(json: bool) -> Result<i32, i32> {
    let all = query(Request::List)?;
    let infos: Vec<svcconf::Info> = all.iter().map(|e| e.info()).collect();
    if json {
        println!("{}", svcconf::json_list(&infos));
        return Ok(0);
    }
    println!(
        "{:<16} {:<9} {:<8} {:>6} {:<6} DESCRIPTION",
        "NAME", "STATE", "ENABLED", "PID", "TTY"
    );
    for i in &infos {
        println!(
            "{:<16} {:<9} {:<8} {:>6} {:<6} {}",
            i.name,
            i.state.as_str(),
            if i.enabled { "yes" } else { "no" },
            or_dash(i.pid.map(|p| p.to_string())),
            i.tty.unwrap_or("-"),
            i.description
        );
    }
    Ok(0)
}

fn status(name: &str, json: bool) -> Result<i32, i32> {
    let e = status_of(name)?;
    let i = e.info();
    if json {
        println!("{}", i.to_json());
        return Ok(0);
    }
    println!("{} - {}", i.name, i.description);
    println!("  state:   {}", i.state.as_str());
    println!("  enabled: {}", if i.enabled { "yes" } else { "no" });
    println!("  pid:     {}", or_dash(i.pid.map(|p| p.to_string())));
    println!("  tty:     {}", i.tty.unwrap_or("-"));
    println!("  exec:    {}", e.exec);
    if i.tty.is_none() {
        println!("  log:     {}", svcconf::log_path(name));
    }
    Ok(0)
}

/// Wait up to `ms` until `done(status)`; returns the last status.
fn wait_for(name: &str, ms: u64, done: impl Fn(&Entry) -> bool) -> Result<Entry, i32> {
    let deadline = time::millis() + ms;
    loop {
        let s = status_of(name)?;
        if done(&s) || time::millis() > deadline {
            return Ok(s);
        }
        time::sleep_ms(50);
    }
}

fn control(cmd: Command, name: &str) -> Result<i32, i32> {
    let old_pid = match cmd {
        Command::Restart => status_of(name)?.pid,
        _ => None,
    };
    match request(&Request::Control(cmd, String::from(name))).map(|r| r.reply) {
        Ok(Reply::Ok) => {}
        // The enabled list's path says what failed.
        Ok(Reply::Error(e)) if matches!(cmd, Command::Enable | Command::Disable) => {
            eprintln!("svc: {}", e);
            return Err(1);
        }
        r => return Err(failure(name, r)),
    }
    let failed = |name: &str| {
        eprintln!("svc: {} failed (see {})", name, svcconf::log_path(name));
        1
    };
    match cmd {
        Command::Enable | Command::Disable => Ok(0),
        Command::Stop => {
            let s = wait_for(name, 10_000, |s| s.pid.is_none())?;
            if s.pid.is_some() {
                eprintln!("svc: {} is still stopping", name);
                return Err(1);
            }
            Ok(0)
        }
        Command::Start | Command::Restart => {
            let s = wait_for(name, 5_000, |s| {
                s.state != State::Starting
                    && !(cmd == Command::Restart && s.pid.is_some() && s.pid == old_pid)
            })?;
            match s.state {
                State::Failed => Err(failed(name)),
                State::Starting if s.pid.is_none() => Err(failed(name)),
                _ => Ok(0),
            }
        }
    }
}
