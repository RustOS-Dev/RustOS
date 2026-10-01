//! The service manager: starts the enabled services from /etc/svc (and
//! /storage/etc/svc), restarts them by their policy with back-off, and
//! serves `svc` requests from the control FIFO /run/svc/control,
//! answering in /run/svc/reply.<id> and publishing every state change in
//! /run/svc/status. See docs/SERVICES.md.

use alloc::collections::BTreeMap;
use rustos_rt::prelude::*;
use rustos_rt::{env, fs, io, process, signal};
use svcconf::{Backoff, Command, Reply, Restart, Service, State, Status};

/// A service counts as started once it has run this long.
const STARTUP_MS: u64 = 1_000;
/// Grace period between SIGTERM and SIGKILL on stop.
const STOP_TIMEOUT_MS: u64 = 5_000;

fn now_ms() -> u64 {
    rustos_rt::time::millis()
}

/// Append a line to the service log (/var/log/svc/init.log).
pub fn log(msg: &str) {
    let ms = now_ms();
    let line = format!("[{:>5}.{:03}] {}\n", ms / 1000, ms % 1000, msg);
    let _ = fs::append(svcconf::INIT_LOG, line.as_bytes());
}

/// Log and show on the console (failures).
fn alert(msg: &str) {
    log(msg);
    println!("[init] {}", msg);
    io::flush();
}

struct Unit {
    conf: Service,
    state: State,
    pid: Option<i32>,
    started_at: u64,
    /// Pending (re)start time while waiting out the back-off.
    restart_at: Option<u64>,
    /// A stop was requested: SIGTERM sent, SIGKILL at `kill_at`.
    stopping: bool,
    kill_at: Option<u64>,
    /// Start again once the stopped process has exited (restart).
    start_after_stop: bool,
    backoff: Backoff,
}

impl Unit {
    fn new(conf: Service) -> Unit {
        Unit {
            conf,
            state: State::Stopped,
            pid: None,
            started_at: 0,
            restart_at: None,
            stopping: false,
            kill_at: None,
            start_after_stop: false,
            backoff: Backoff::default(),
        }
    }
}

pub struct Supervisor {
    units: BTreeMap<String, Unit>,
    /// Read end of the control FIFO, and a write end init keeps open so
    /// the read end never sees end-of-file.
    ctl: Option<fs::File>,
    _ctl_keep: Option<fs::File>,
    pending: Vec<u8>,
}

/// Service files: the image's, overridden by the storage partition's.
fn load_confs() -> BTreeMap<String, Service> {
    let mut out = BTreeMap::new();
    for dir in [svcconf::SYSTEM_DIR, svcconf::STORAGE_DIR] {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for e in entries {
            let Some(name) = e.name.strip_suffix(".conf") else {
                continue;
            };
            let path = format!("{}/{}", dir, e.name);
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            match svcconf::parse(name, &text) {
                Ok((s, warnings)) => {
                    for w in warnings {
                        log(&format!("{}: {}", path, w));
                    }
                    out.insert(String::from(name), s);
                }
                Err(e) => log(&format!("{}: {}", path, e)),
            }
        }
    }
    out
}

/// The enabled list: /storage/etc/svc/enabled, else /etc/svc/enabled.
pub fn enabled() -> Vec<String> {
    fs::read_to_string(&format!("{}/enabled", svcconf::STORAGE_DIR))
        .or_else(|_| fs::read_to_string(&format!("{}/enabled", svcconf::SYSTEM_DIR)))
        .map(|t| svcconf::parse_enabled(&t))
        .unwrap_or_default()
}

struct Account {
    uid: u32,
    gid: u32,
    home: String,
}

fn account(user: &str) -> Option<Account> {
    let passwd = fs::read_to_string("/storage/etc/passwd")
        .or_else(|_| fs::read_to_string("/etc/passwd"))
        .unwrap_or_default();
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 7 && f[0] == user).then(|| Account {
            uid: f[2].parse().unwrap_or(0),
            gid: f[3].parse().unwrap_or(0),
            home: f[5].into(),
        })
    })
}

/// What to execute for a service: (program path, argv).
fn command(conf: &Service) -> Result<(String, Vec<String>), String> {
    if svcconf::needs_shell(&conf.exec) {
        if !fs::exists("/bin/sh") {
            return Err(String::from("/bin/sh: not found"));
        }
        // A missing program given by absolute path fails right away
        // instead of going through the restart back-off.
        if let Some(first) = conf.exec.split_whitespace().next()
            && first.starts_with('/')
            && !first.contains(|c: char| "\"'$`*?[".contains(c))
            && !fs::exists(first)
        {
            return Err(format!("{}: not found", first));
        }
        let argv = vec![String::from("sh"), String::from("-c"), conf.exec.clone()];
        return Ok((String::from("/bin/sh"), argv));
    }
    let argv = svcconf::split_command(&conf.exec);
    let path = process::find_in_path(&argv[0]).ok_or_else(|| format!("{}: not found", argv[0]))?;
    Ok((path, argv))
}

/// Fork and exec a service. The child is a session leader; with `tty=`
/// the terminal becomes its controlling terminal and standard I/O,
/// otherwise output goes to /var/log/svc/NAME.log.
fn spawn(conf: &Service) -> Result<i32, String> {
    let (path, argv) = command(conf)?;
    let acct = match &conf.user {
        Some(u) => Some(account(u).ok_or_else(|| format!("unknown user {}", u))?),
        None => None,
    };
    let tty = conf.tty.as_ref().map(|t| format!("/dev/{}", t));
    if let Some(t) = &tty
        && !fs::exists(t)
    {
        return Err(format!("{}: no such terminal", t));
    }
    let log_path = svcconf::log_path(&conf.name);
    let pid = process::fork().map_err(|e| format!("fork: {}", e))?;
    if pid != 0 {
        return Ok(pid);
    }
    // Child.
    io::discard_buffered();
    let _ = process::setsid();
    for s in [
        signal::SIGINT,
        signal::SIGTSTP,
        signal::SIGTTIN,
        signal::SIGTTOU,
        signal::SIGQUIT,
        signal::SIGCHLD,
        signal::SIGPIPE,
    ] {
        signal::default(s);
    }
    signal::mask(2, 0);
    if let Some(t) = &tty {
        let Ok(f) = fs::File::open_with(t, fs::O_RDWR, 0) else {
            process::exit(126);
        };
        for fd in 0..3 {
            let _ = process::dup2(f.fd(), fd);
        }
        drop(f);
        rustos_rt::term::tcsetpgrp(0, process::getpid());
    } else {
        if let Ok(f) = fs::File::open_with("/dev/null", fs::O_RDONLY, 0) {
            let _ = process::dup2(f.fd(), 0);
        }
        if let Ok(f) =
            fs::File::open_with(&log_path, fs::O_WRONLY | fs::O_CREAT | fs::O_APPEND, 0o644)
        {
            let _ = process::dup2(f.fd(), 1);
            let _ = process::dup2(f.fd(), 2);
        }
    }
    let mut dir = String::from("/");
    if let Some(a) = &acct {
        // setgid(106) before setuid(105).
        rustos_rt::sys::syscall(106, &[a.gid as usize]);
        rustos_rt::sys::syscall(105, &[a.uid as usize]);
        let user = conf.user.as_deref().unwrap_or("root");
        env::set_var("HOME", &a.home);
        env::set_var("USER", user);
        env::set_var("LOGNAME", user);
        dir = a.home.clone();
    }
    if env::set_current_dir(&dir).is_err() {
        let _ = env::set_current_dir("/");
    }
    env::set_var("SVC_NAME", &conf.name);
    for (k, v) in &conf.env {
        env::set_var(k, v);
    }
    let e = process::execve(&path, &argv, &env::environ());
    eprintln!("svc: exec {}: {}", path, e);
    process::exit(127);
}

impl Supervisor {
    pub fn new() -> Supervisor {
        let _ = fs::create_dir_all(svcconf::RUN_DIR);
        let _ = fs::create_dir_all(svcconf::LOG_DIR);
        let _ = fs::remove_file(svcconf::CONTROL);
        let (ctl, keep) = match fs::mkfifo(svcconf::CONTROL, 0o600) {
            Ok(()) => (
                fs::File::open_with(
                    svcconf::CONTROL,
                    fs::O_RDONLY | fs::O_NONBLOCK | fs::O_CLOEXEC,
                    0,
                )
                .ok(),
                fs::File::open_with(svcconf::CONTROL, fs::O_WRONLY | fs::O_CLOEXEC, 0).ok(),
            ),
            Err(e) => {
                log(&format!("{}: {}", svcconf::CONTROL, e));
                (None, None)
            }
        };
        let mut s = Supervisor {
            units: BTreeMap::new(),
            ctl,
            _ctl_keep: keep,
            pending: Vec::new(),
        };
        s.reload();
        s.write_status();
        s
    }

    /// Re-read the service files. Running services keep their state; a
    /// service whose file is gone is forgotten once it is not running.
    fn reload(&mut self) {
        let mut confs = load_confs();
        self.units.retain(|name, u| {
            if let Some(c) = confs.remove(name) {
                u.conf = c;
                true
            } else {
                u.pid.is_some()
            }
        });
        for (name, c) in confs {
            self.units.insert(name, Unit::new(c));
        }
    }

    /// Start the enabled services, each after the ones it names in
    /// `after=`.
    pub fn start_enabled(&mut self) {
        let mut wanted = Vec::new();
        for n in enabled() {
            if self.units.contains_key(&n) {
                wanted.push(n);
            } else {
                log(&format!("{}: enabled but no service file", n));
            }
        }
        let units = &self.units;
        let order = svcconf::start_order(&wanted, |n| {
            units
                .get(n)
                .map(|u| u.conf.after.clone())
                .unwrap_or_default()
        });
        for n in order {
            let _ = self.start(&n);
        }
        self.write_status();
    }

    /// Terminals that enabled services run on (no shell is started there).
    pub fn claimed_ttys(&self) -> Vec<(String, String)> {
        let en = enabled();
        self.units
            .values()
            .filter(|u| en.contains(&u.conf.name))
            .filter_map(|u| Some((u.conf.tty.clone()?, u.conf.name.clone())))
            .collect()
    }

    fn launch(&mut self, name: &str) -> Reply {
        let Some(u) = self.units.get_mut(name) else {
            return Reply::Unknown;
        };
        u.restart_at = None;
        match spawn(&u.conf) {
            Ok(pid) => {
                u.pid = Some(pid);
                u.state = State::Starting;
                u.started_at = now_ms();
                log(&format!("{}: started (pid {})", name, pid));
                Reply::Ok
            }
            Err(e) => {
                u.state = State::Failed;
                alert(&format!("service {} failed: {}", name, e));
                Reply::Error(e)
            }
        }
    }

    fn start(&mut self, name: &str) -> Reply {
        let Some(u) = self.units.get_mut(name) else {
            return Reply::Unknown;
        };
        if u.pid.is_some() {
            if u.stopping {
                u.start_after_stop = true;
            }
            return Reply::Ok;
        }
        u.backoff.reset();
        self.launch(name)
    }

    fn stop(&mut self, name: &str) -> Reply {
        let Some(u) = self.units.get_mut(name) else {
            return Reply::Unknown;
        };
        u.restart_at = None;
        u.start_after_stop = false;
        match u.pid {
            Some(pid) => {
                if !u.stopping {
                    u.stopping = true;
                    u.kill_at = Some(now_ms() + STOP_TIMEOUT_MS);
                    // The service leads its own process group.
                    let _ = process::kill(-pid, signal::SIGTERM);
                    let _ = process::kill(-pid, signal::SIGCONT);
                    log(&format!("{}: stopping (pid {})", name, pid));
                }
            }
            None => u.state = State::Stopped,
        }
        Reply::Ok
    }

    fn restart(&mut self, name: &str) -> Reply {
        let Some(u) = self.units.get(name) else {
            return Reply::Unknown;
        };
        if u.pid.is_none() {
            return self.start(name);
        }
        let r = self.stop(name);
        if let Some(u) = self.units.get_mut(name) {
            u.start_after_stop = true;
            u.backoff.reset();
        }
        r
    }

    /// A child exited: returns false if it is not a service.
    pub fn child_exited(&mut self, pid: i32, status: i32) -> bool {
        let Some(name) = self
            .units
            .iter()
            .find(|(_, u)| u.pid == Some(pid))
            .map(|(n, _)| n.clone())
        else {
            return false;
        };
        let now = now_ms();
        let u = self.units.get_mut(&name).unwrap();
        u.pid = None;
        u.kill_at = None;
        let ran = now.saturating_sub(u.started_at);
        let failed = !(process::wifexited(status) && process::wexitstatus(status) == 0);
        let how = if process::wifsignaled(status) {
            format!("killed by {}", signal::name(process::wtermsig(status)))
        } else {
            format!("exited with status {}", process::wexitstatus(status))
        };
        log(&format!("{}: {} after {} ms", name, how, ran));
        let mut relaunch = false;
        if u.stopping {
            u.stopping = false;
            u.state = State::Stopped;
            relaunch = core::mem::take(&mut u.start_after_stop);
        } else {
            match (u.conf.restart, failed) {
                (Restart::No, true) => {
                    u.state = State::Failed;
                    alert(&format!("service {} failed: {}", name, how));
                }
                (Restart::No, false) | (Restart::OnFailure, false) => u.state = State::Stopped,
                _ => match u.backoff.exited(now, ran, failed) {
                    Some(delay) => {
                        u.state = State::Starting;
                        u.restart_at = Some(now + delay);
                        log(&format!("{}: restarting in {} ms", name, delay));
                    }
                    None => {
                        u.state = State::Failed;
                        alert(&format!(
                            "service {} failed: {} failures within {} s, giving up",
                            name,
                            svcconf::MAX_FAILURES,
                            svcconf::FAILURE_WINDOW_MS / 1000
                        ));
                    }
                },
            }
        }
        if relaunch {
            self.launch(&name);
        }
        // A service whose file was removed is forgotten once it is gone.
        if self.units.get(&name).is_some_and(|u| u.pid.is_none()) {
            let confs = load_confs();
            if !confs.contains_key(&name) {
                self.units.remove(&name);
            }
        }
        self.write_status();
        true
    }

    /// Timed work: due restarts, start-up completion, stop escalation.
    pub fn tick(&mut self) {
        let now = now_ms();
        let mut changed = false;
        let due: Vec<String> = self
            .units
            .iter()
            .filter(|(_, u)| u.restart_at.is_some_and(|t| t <= now))
            .map(|(n, _)| n.clone())
            .collect();
        for n in due {
            self.launch(&n);
            changed = true;
        }
        for (n, u) in self.units.iter_mut() {
            if u.state == State::Starting
                && u.pid.is_some()
                && now.saturating_sub(u.started_at) >= STARTUP_MS
            {
                u.state = State::Running;
                changed = true;
            }
            if let (Some(t), Some(pid)) = (u.kill_at, u.pid)
                && t <= now
            {
                log(&format!("{}: did not stop, sending SIGKILL", n));
                let _ = process::kill(-pid, signal::SIGKILL);
                let _ = process::kill(pid, signal::SIGKILL);
                u.kill_at = None;
            }
        }
        if changed {
            self.write_status();
        }
    }

    /// Milliseconds until the next timed work (None: nothing pending).
    pub fn next_timeout(&self) -> Option<u64> {
        let now = now_ms();
        self.units
            .values()
            .flat_map(|u| {
                [
                    u.restart_at,
                    u.kill_at,
                    (u.state == State::Starting && u.pid.is_some())
                        .then_some(u.started_at + STARTUP_MS),
                ]
            })
            .flatten()
            .map(|t| t.saturating_sub(now))
            .min()
    }

    /// The control FIFO's descriptor, for poll.
    pub fn control_fd(&self) -> Option<i32> {
        self.ctl.as_ref().map(|f| f.fd())
    }

    /// Serve the requests waiting in the control FIFO.
    pub fn handle_requests(&mut self) {
        let Some(ctl) = &self.ctl else {
            return;
        };
        let mut buf = [0u8; 512];
        while let Ok(n) = ctl.read(&mut buf) {
            if n == 0 {
                break;
            }
            self.pending.extend_from_slice(&buf[..n]);
        }
        while let Some(i) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line).into_owned();
            let Some((cmd, name, id)) = svcconf::parse_request(&line) else {
                log(&format!("bad request: {}", line.trim()));
                continue;
            };
            self.reload();
            log(&format!("request: {} {}", cmd.as_str(), name));
            let reply = match cmd {
                Command::Start => self.start(&name),
                Command::Stop => self.stop(&name),
                Command::Restart => self.restart(&name),
            };
            self.write_status();
            let path = svcconf::reply_path(id);
            let tmp = format!("{}.tmp", path);
            if fs::write(&tmp, reply.render().as_bytes()).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
        if self.pending.len() > 4096 {
            self.pending.clear();
        }
    }

    /// Publish every service's state in /run/svc/status.
    fn write_status(&self) {
        let text = svcconf::render_status(self.units.iter().map(|(n, u)| {
            (
                n.as_str(),
                Status {
                    state: u.state,
                    pid: u.pid.map(|p| p as u32),
                },
            )
        }));
        let tmp = format!("{}.tmp", svcconf::STATUS);
        if fs::write(&tmp, text.as_bytes()).is_ok() {
            let _ = fs::rename(&tmp, svcconf::STATUS);
        }
    }
}
