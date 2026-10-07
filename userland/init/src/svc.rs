//! The service manager: starts the enabled services from /etc/svc (and
//! /storage/etc/svc), restarts them by their policy with back-off, and
//! serves `svc` requests on the AF_UNIX socket /run/svc.sock, one request
//! per connection, allowed by the client's SO_PEERCRED uid. See
//! docs/SERVICES.md.

use alloc::collections::BTreeMap;
use rustos_rt::net::{self, Socket};
use rustos_rt::prelude::*;
use rustos_rt::{env, fs, io, process, signal};
use svcconf::{Backoff, Command, Entry, Reply, Request, Response, Restart, Service, State};

/// A service counts as started once it has run this long.
const STARTUP_MS: u64 = 1_000;
/// Grace period between SIGTERM and SIGKILL on stop.
const STOP_TIMEOUT_MS: u64 = 5_000;
/// A client must send its request within this time.
const CLIENT_TIMEOUT_MS: u64 = 5_000;
/// Connections waiting for their request line at most; more are refused.
const MAX_CLIENTS: usize = 32;

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

/// What reading a client's connection gave.
enum Got {
    Line(String),
    /// Nothing more yet (EAGAIN).
    Wait,
    /// End-of-file, an error or an overlong line: close it.
    Drop,
}

/// A connection whose request line has not arrived yet.
struct Client {
    sock: Socket,
    /// The connecting process's uid (SO_PEERCRED).
    uid: u32,
    pid: u32,
    buf: Vec<u8>,
    deadline: u64,
}

pub struct Supervisor {
    units: BTreeMap<String, Unit>,
    /// The listening control socket (non-blocking).
    listener: Option<Socket>,
    clients: Vec<Client>,
}

/// Listen on /run/svc.sock: a non-blocking stream socket anyone may
/// connect to.
fn listen() -> rustos_rt::Result<Socket> {
    let _ = fs::create_dir_all("/run");
    let _ = fs::remove_file(svcconf::SOCKET);
    let s = Socket::new(net::AF_UNIX, net::SOCK_STREAM | net::SOCK_NONBLOCK, 0)?;
    s.bind_raw(&net::unix_addr(svcconf::SOCKET))?;
    fs::set_permissions(svcconf::SOCKET, 0o666)?;
    s.listen(64)?;
    Ok(s)
}

/// Where `svc enable`/`disable` keep the enabled list: the storage
/// partition when there is one, else the image's (this boot only).
fn enabled_dir() -> &'static str {
    if fs::is_dir("/storage") {
        svcconf::STORAGE_DIR
    } else {
        svcconf::SYSTEM_DIR
    }
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
        let _ = fs::create_dir_all(svcconf::LOG_DIR);
        let listener = listen()
            .map_err(|e| alert(&format!("{}: {}", svcconf::SOCKET, e)))
            .ok();
        let mut s = Supervisor {
            units: BTreeMap::new(),
            listener,
            clients: Vec::new(),
        };
        s.reload();
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

    /// Add `name` to the enabled list or remove it.
    fn set_enabled(&mut self, name: &str, enable: bool) -> Reply {
        if !self.units.contains_key(name) {
            return Reply::Unknown;
        }
        let dir = enabled_dir();
        let path = format!("{}/enabled", dir);
        let base = fs::read_to_string(&path)
            .or_else(|_| fs::read_to_string(&format!("{}/enabled", svcconf::SYSTEM_DIR)))
            .unwrap_or_default();
        let text = svcconf::edit_enabled(&base, name, enable);
        if text == base && fs::exists(&path) {
            return Reply::Ok;
        }
        match fs::create_dir_all(dir).and_then(|_| fs::write(&path, text.as_bytes())) {
            Ok(()) => Reply::Ok,
            Err(e) => Reply::Error(format!("{}: {}", path, e)),
        }
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
        true
    }

    /// Timed work: due restarts, start-up completion, stop escalation.
    pub fn tick(&mut self) {
        let now = now_ms();
        let due: Vec<String> = self
            .units
            .iter()
            .filter(|(_, u)| u.restart_at.is_some_and(|t| t <= now))
            .map(|(n, _)| n.clone())
            .collect();
        for n in due {
            self.launch(&n);
        }
        for (n, u) in self.units.iter_mut() {
            if u.state == State::Starting
                && u.pid.is_some()
                && now.saturating_sub(u.started_at) >= STARTUP_MS
            {
                u.state = State::Running;
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
        // Clients that never sent their request.
        self.clients.retain(|c| c.deadline > now);
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

    /// Descriptors to poll for input: the control socket and the
    /// connections waiting for their request.
    pub fn poll_fds(&self) -> Vec<i32> {
        self.listener
            .iter()
            .map(|l| l.fd())
            .chain(self.clients.iter().map(|c| c.sock.fd()))
            .collect()
    }

    /// Accept new connections and serve the requests that have arrived:
    /// one line per connection, answered with a [`Response`] before the
    /// connection is closed.
    pub fn handle_requests(&mut self) {
        let Some(listener) = &self.listener else {
            return;
        };
        let now = now_ms();
        while let Ok(sock) = listener.accept4(net::SOCK_NONBLOCK | net::SOCK_CLOEXEC) {
            if self.clients.len() >= MAX_CLIENTS {
                log("too many pending svc connections");
                continue;
            }
            // Unknown credentials get the rights of nobody.
            let cred = sock.peer_cred().unwrap_or(net::Ucred {
                pid: 0,
                uid: u32::MAX,
                gid: u32::MAX,
            });
            self.clients.push(Client {
                sock,
                uid: cred.uid,
                pid: cred.pid,
                buf: Vec::new(),
                deadline: now + CLIENT_TIMEOUT_MS,
            });
        }
        let mut waiting = Vec::new();
        for mut c in core::mem::take(&mut self.clients) {
            let mut chunk = [0u8; 256];
            let got = loop {
                match c.sock.recv(&mut chunk) {
                    Ok(n) if n > 0 => {
                        c.buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = c.buf.iter().position(|&b| b == b'\n') {
                            c.buf.truncate(i);
                            break Got::Line(String::from_utf8_lossy(&c.buf).into_owned());
                        }
                        if c.buf.len() > svcconf::MAX_REQUEST {
                            break Got::Drop;
                        }
                    }
                    Err(e) if e.0 == 11 => break Got::Wait,
                    _ => break Got::Drop,
                }
            };
            match got {
                Got::Wait => waiting.push(c),
                Got::Drop => {}
                Got::Line(l) => {
                    let resp = self.serve(&l, c.uid, c.pid);
                    // The answer is small and the connection's buffer
                    // empty: it fits without blocking.
                    let _ = c.sock.send_all(resp.render().as_bytes());
                }
            }
        }
        self.clients.extend(waiting);
    }

    /// Answer one request from a client running as `uid`.
    fn serve(&mut self, line: &str, uid: u32, pid: u32) -> Response {
        let Some(req) = Request::parse(line) else {
            log(&format!("bad request: {}", line.trim()));
            return Response::new(Reply::Error(String::from("bad request")));
        };
        if !svcconf::permitted(&req, uid) {
            log(&format!(
                "denied: {} (uid {}, pid {})",
                req.render().trim_end(),
                uid,
                pid
            ));
            return Response::new(Reply::Denied);
        }
        // Pick up new, changed and removed service files.
        self.reload();
        match req {
            Request::List => Response {
                reply: Reply::Ok,
                entries: {
                    let en = enabled();
                    self.units.values().map(|u| entry(u, &en)).collect()
                },
            },
            Request::Status(name) => match self.units.get(&name) {
                Some(u) => Response {
                    reply: Reply::Ok,
                    entries: vec![entry(u, &enabled())],
                },
                None => Response::new(Reply::Unknown),
            },
            Request::Control(cmd, name) => {
                log(&format!("request: {} {}", cmd.as_str(), name));
                Response::new(match cmd {
                    Command::Start => self.start(&name),
                    Command::Stop => self.stop(&name),
                    Command::Restart => self.restart(&name),
                    Command::Enable => self.set_enabled(&name, true),
                    Command::Disable => self.set_enabled(&name, false),
                })
            }
        }
    }
}

/// What `svc` is told about a unit.
fn entry(u: &Unit, enabled: &[String]) -> Entry {
    Entry {
        name: u.conf.name.clone(),
        description: u.conf.description.clone(),
        exec: u.conf.exec.clone(),
        enabled: enabled.contains(&u.conf.name),
        state: u.state,
        pid: u.pid.map(|p| p as u32),
        tty: u.conf.tty.clone(),
    }
}
