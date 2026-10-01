//! svc: list, inspect and control the services init supervises.
//!
//! Service files and the enabled list are read directly; the state comes
//! from init's /run/svc/status; start/stop/restart are requests written
//! to the control FIFO /run/svc/control, answered in /run/svc/reply.<pid>.
//! Exit status: 0 ok, 1 error, 3 unknown service (docs/SERVICES.md).

use alloc::collections::BTreeMap;
use rustos_rt::prelude::*;
use rustos_rt::{fs, process, signal, time};
use svcconf::{Command, Info, Reply, Service, State, Status};

const USAGE: &str = "usage: svc list [--json] | svc status NAME [--json] | svc start|stop|restart|enable|disable NAME";
const UNKNOWN: i32 = 3;

/// Service files: the image's, overridden by the storage partition's.
fn services() -> BTreeMap<String, Service> {
    let mut out = BTreeMap::new();
    for dir in [svcconf::SYSTEM_DIR, svcconf::STORAGE_DIR] {
        for e in fs::read_dir(dir).unwrap_or_default() {
            let Some(name) = e.name.strip_suffix(".conf") else {
                continue;
            };
            let path = format!("{}/{}", dir, e.name);
            match fs::read_to_string(&path).map(|t| svcconf::parse(name, &t)) {
                Ok(Ok((s, _))) => {
                    out.insert(String::from(name), s);
                }
                Ok(Err(e)) => eprintln!("svc: {}: {}", path, e),
                Err(_) => {}
            }
        }
    }
    out
}

fn enabled_path(dir: &str) -> String {
    format!("{}/enabled", dir)
}

fn enabled() -> Vec<String> {
    fs::read_to_string(&enabled_path(svcconf::STORAGE_DIR))
        .or_else(|_| fs::read_to_string(&enabled_path(svcconf::SYSTEM_DIR)))
        .map(|t| svcconf::parse_enabled(&t))
        .unwrap_or_default()
}

fn states() -> BTreeMap<String, Status> {
    fs::read_to_string(svcconf::STATUS)
        .map(|t| svcconf::parse_status(&t).into_iter().collect())
        .unwrap_or_default()
}

fn status_of(name: &str) -> Status {
    states().get(name).copied().unwrap_or(Status {
        state: State::Stopped,
        pid: None,
    })
}

fn info<'a>(s: &'a Service, enabled: &[String], st: &BTreeMap<String, Status>) -> Info<'a> {
    let status = st.get(&s.name).copied().unwrap_or(Status {
        state: State::Stopped,
        pid: None,
    });
    Info {
        name: &s.name,
        description: &s.description,
        enabled: enabled.contains(&s.name),
        state: status.state,
        pid: status.pid,
        tty: s.tty.as_deref(),
    }
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
    match rest.as_slice() {
        ["list"] => list(json),
        ["status", name] => status(name, json),
        [cmd @ ("start" | "stop" | "restart"), name] => control(Command::parse(cmd).unwrap(), name),
        ["enable", name] => set_enabled(name, true),
        ["disable", name] => set_enabled(name, false),
        _ => {
            eprintln!("{}", USAGE);
            1
        }
    }
}

fn or_dash(v: Option<String>) -> String {
    v.unwrap_or_else(|| String::from("-"))
}

fn list(json: bool) -> i32 {
    let all = services();
    let (en, st) = (enabled(), states());
    let infos: Vec<Info> = all.values().map(|s| info(s, &en, &st)).collect();
    if json {
        println!("{}", svcconf::json_list(&infos));
        return 0;
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
    0
}

fn status(name: &str, json: bool) -> i32 {
    let all = services();
    let Some(s) = all.get(name) else {
        return unknown(name);
    };
    let i = info(s, &enabled(), &states());
    if json {
        println!("{}", i.to_json());
        return 0;
    }
    println!("{} - {}", i.name, i.description);
    println!("  state:   {}", i.state.as_str());
    println!("  enabled: {}", if i.enabled { "yes" } else { "no" });
    println!("  pid:     {}", or_dash(i.pid.map(|p| p.to_string())));
    println!("  tty:     {}", i.tty.unwrap_or("-"));
    println!("  exec:    {}", s.exec);
    if i.tty.is_none() {
        println!("  log:     {}", svcconf::log_path(name));
    }
    0
}

/// Send a request to init and wait for its reply.
fn request(cmd: Command, name: &str) -> Result<Reply, String> {
    const NOT_RUNNING: &str = "the service manager is not running";
    signal::ignore(signal::SIGPIPE);
    let id = process::getpid() as u32;
    let reply = svcconf::reply_path(id);
    let _ = fs::remove_file(&reply);
    let ctl = fs::File::open_with(svcconf::CONTROL, fs::O_WRONLY | fs::O_NONBLOCK, 0)
        .map_err(|_| String::from(NOT_RUNNING))?;
    ctl.write_all(svcconf::request_line(cmd, name, id).as_bytes())
        .map_err(|e| format!("{}: {}", svcconf::CONTROL, e))?;
    drop(ctl);
    let deadline = time::millis() + 10_000;
    loop {
        if let Ok(t) = fs::read_to_string(&reply) {
            let _ = fs::remove_file(&reply);
            return Reply::parse(&t).ok_or_else(|| format!("bad reply '{}'", t.trim()));
        }
        if time::millis() > deadline {
            return Err(String::from("no answer from the service manager"));
        }
        time::sleep_ms(20);
    }
}

/// Wait up to `ms` until `done(status)`; returns the last status.
fn wait_for(name: &str, ms: u64, done: impl Fn(&Status) -> bool) -> Status {
    let deadline = time::millis() + ms;
    loop {
        let s = status_of(name);
        if done(&s) || time::millis() > deadline {
            return s;
        }
        time::sleep_ms(50);
    }
}

fn control(cmd: Command, name: &str) -> i32 {
    if !services().contains_key(name) {
        return unknown(name);
    }
    let old_pid = status_of(name).pid;
    match request(cmd, name) {
        Ok(Reply::Ok) => {}
        Ok(Reply::Unknown) => return unknown(name),
        Ok(Reply::Error(e)) => {
            eprintln!("svc: {}: {}", name, e);
            return 1;
        }
        Err(e) => {
            eprintln!("svc: {}", e);
            return 1;
        }
    }
    let failed = |name: &str| {
        eprintln!("svc: {} failed (see {})", name, svcconf::log_path(name));
        1
    };
    match cmd {
        Command::Stop => {
            let s = wait_for(name, 10_000, |s| s.pid.is_none());
            if s.pid.is_some() {
                eprintln!("svc: {} is still stopping", name);
                return 1;
            }
            0
        }
        Command::Start | Command::Restart => {
            let s = wait_for(name, 5_000, |s| {
                s.state != State::Starting
                    && !(cmd == Command::Restart && s.pid.is_some() && s.pid == old_pid)
            });
            match s.state {
                State::Failed => failed(name),
                State::Starting if s.pid.is_none() => failed(name),
                _ => 0,
            }
        }
    }
}

fn set_enabled(name: &str, enable: bool) -> i32 {
    if !services().contains_key(name) {
        return unknown(name);
    }
    // Persist on the storage partition when there is one.
    let dir = if fs::is_dir("/storage") {
        svcconf::STORAGE_DIR
    } else {
        svcconf::SYSTEM_DIR
    };
    let path = enabled_path(dir);
    let base = fs::read_to_string(&path)
        .or_else(|_| fs::read_to_string(&enabled_path(svcconf::SYSTEM_DIR)))
        .unwrap_or_default();
    let text = svcconf::edit_enabled(&base, name, enable);
    if text == base && fs::exists(&path) {
        return 0;
    }
    if let Err(e) = fs::create_dir_all(dir).and_then(|_| fs::write(&path, text.as_bytes())) {
        eprintln!("svc: {}: {}", path, e);
        return 1;
    }
    0
}
