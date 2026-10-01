//! Service definitions for init's service manager and the `svc` tool:
//! the `/etc/svc/NAME.conf` format, the enabled list, the state file
//! init publishes, the control protocol, restart back-off and the JSON
//! that `svc --json` prints.
//!
//! A service file is `key=value` lines (`#` comments):
//!
//! ```text
//! description=Tor anonymity daemon
//! exec=/usr/bin/tor -f /etc/tor/torrc
//! tty=tty2            # optional: run on /dev/tty2 as its session leader
//! user=root           # optional
//! restart=on-failure  # no | on-failure | always
//! after=dbus seatd    # start after these (when they are started too)
//! env=KEY=VALUE       # may repeat
//! ```

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Service files shipped in the image.
pub const SYSTEM_DIR: &str = "/etc/svc";
/// Service files on the storage partition; they override the image's.
pub const STORAGE_DIR: &str = "/storage/etc/svc";
/// Runtime directory: control FIFO, state file and replies.
pub const RUN_DIR: &str = "/run/svc";
/// Named pipe init reads requests from.
pub const CONTROL: &str = "/run/svc/control";
/// State of every known service, rewritten by init on each change.
pub const STATUS: &str = "/run/svc/status";
/// Standard output and error of services without a terminal.
pub const LOG_DIR: &str = "/var/log/svc";
/// init's own service log.
pub const INIT_LOG: &str = "/var/log/svc/init.log";

/// Path of the reply init writes for request `id`.
pub fn reply_path(id: u32) -> String {
    format!("{}/reply.{}", RUN_DIR, id)
}

/// Path of the log file of service `name`.
pub fn log_path(name: &str) -> String {
    format!("{}/{}.log", LOG_DIR, name)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    No,
    OnFailure,
    Always,
}

impl Restart {
    pub fn parse(s: &str) -> Option<Restart> {
        match s {
            "no" => Some(Restart::No),
            "on-failure" => Some(Restart::OnFailure),
            "always" => Some(Restart::Always),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Stopped,
    /// Launched less than a second ago, or waiting to be restarted.
    Starting,
    /// Could not be started, or gave up after repeated failures.
    Failed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Stopped => "stopped",
            State::Starting => "starting",
            State::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        match s {
            "running" => Some(State::Running),
            "stopped" => Some(State::Stopped),
            "starting" => Some(State::Starting),
            "failed" => Some(State::Failed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub description: String,
    /// Command line; run through `/bin/sh -c` when [`needs_shell`].
    pub exec: String,
    /// Terminal name without `/dev/` (e.g. `tty2`).
    pub tty: Option<String>,
    pub user: Option<String>,
    pub restart: Restart,
    pub after: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Service names: letters, digits and `-_.@`, not starting with `.` or `-`.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with(['.', '-'])
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
}

fn valid_env_key(k: &str) -> bool {
    !k.is_empty()
        && !k.starts_with(|c: char| c.is_ascii_digit())
        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn unquote(v: &str) -> &str {
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// Parse the service file of `name`. Unknown keys are reported in the
/// returned warnings and otherwise ignored.
pub fn parse(name: &str, text: &str) -> Result<(Service, Vec<String>), String> {
    if !valid_name(name) {
        return Err(format!("invalid service name '{}'", name));
    }
    let mut s = Service {
        name: name.to_string(),
        description: String::new(),
        exec: String::new(),
        tty: None,
        user: None,
        restart: Restart::OnFailure,
        after: Vec::new(),
        env: Vec::new(),
    };
    let mut warnings = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("line {}: expected key=value", i + 1));
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "description" => s.description = unquote(v).to_string(),
            // The command line is kept as written: quotes belong to the shell.
            "exec" => s.exec = v.to_string(),
            "tty" => {
                let t = unquote(v);
                let t = t.strip_prefix("/dev/").unwrap_or(t);
                if t.is_empty() {
                    s.tty = None;
                } else if t.len() > 3
                    && t.starts_with("tty")
                    && t[3..].bytes().all(|b| b.is_ascii_digit())
                {
                    s.tty = Some(t.to_string());
                } else {
                    return Err(format!("line {}: tty must be ttyN", i + 1));
                }
            }
            "user" => {
                let u = unquote(v);
                s.user = (!u.is_empty() && u != "root").then(|| u.to_string());
            }
            "restart" => {
                s.restart = Restart::parse(unquote(v)).ok_or_else(|| {
                    format!("line {}: restart must be no, on-failure or always", i + 1)
                })?;
            }
            "after" => {
                for d in unquote(v).split_whitespace() {
                    if !valid_name(d) {
                        return Err(format!("line {}: invalid service name '{}'", i + 1, d));
                    }
                    if d != name && !s.after.iter().any(|a| a == d) {
                        s.after.push(d.to_string());
                    }
                }
            }
            "env" => {
                let Some((ek, ev)) = v.split_once('=') else {
                    return Err(format!("line {}: env needs KEY=VALUE", i + 1));
                };
                if !valid_env_key(ek.trim()) {
                    return Err(format!("line {}: invalid variable name '{}'", i + 1, ek));
                }
                s.env
                    .push((ek.trim().to_string(), unquote(ev.trim()).to_string()));
            }
            _ => warnings.push(format!("line {}: unknown key '{}'", i + 1, k)),
        }
    }
    if s.exec.is_empty() {
        return Err(String::from("no exec= line"));
    }
    Ok((s, warnings))
}

/// Whether a command line needs `/bin/sh -c` (quoting, redirection,
/// pipes, variables, globbing, ...); otherwise it is split at spaces.
pub fn needs_shell(cmd: &str) -> bool {
    cmd.contains(|c: char| "|&;<>()$`\\\"'*?[]#~{}\n".contains(c))
}

/// The argument vector of a command line without shell syntax.
pub fn split_command(cmd: &str) -> Vec<String> {
    cmd.split_whitespace().map(String::from).collect()
}

/// Names in an enabled list: one per line, `#` comments.
pub fn parse_enabled(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        let n = l.split('#').next().unwrap_or("").trim();
        if valid_name(n) && !out.iter().any(|o| o == n) {
            out.push(n.to_string());
        }
    }
    out
}

/// `text` (an enabled list) with `name` added or removed; comments and
/// other entries are kept.
pub fn edit_enabled(text: &str, name: &str, enable: bool) -> String {
    let mut out = String::new();
    let mut present = false;
    for l in text.lines() {
        let n = l.split('#').next().unwrap_or("").trim();
        if n == name {
            if !enable || present {
                continue;
            }
            present = true;
        }
        out.push_str(l);
        out.push('\n');
    }
    if enable && !present {
        out.push_str(name);
        out.push('\n');
    }
    out
}

/// Order `wanted` so that each service comes after the services it names
/// in `after=` that are also wanted. `deps(name)` gives a service's
/// `after` list. Cycles are broken at the point they are found.
pub fn start_order<'a>(wanted: &[String], deps: impl Fn(&str) -> Vec<String> + 'a) -> Vec<String> {
    fn visit(
        n: &str,
        wanted: &[String],
        deps: &dyn Fn(&str) -> Vec<String>,
        visiting: &mut Vec<String>,
        out: &mut Vec<String>,
    ) {
        if out.iter().any(|o| o == n) || visiting.iter().any(|v| v == n) {
            return;
        }
        visiting.push(n.to_string());
        for d in deps(n) {
            if wanted.contains(&d) {
                visit(&d, wanted, deps, visiting, out);
            }
        }
        visiting.pop();
        out.push(n.to_string());
    }
    let mut out = Vec::new();
    let mut visiting = Vec::new();
    for w in wanted {
        visit(w, wanted, &deps, &mut visiting, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// State file
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub pid: Option<u32>,
}

/// The state file: `NAME STATE PID` per line (`-` for no process).
pub fn render_status<'a>(entries: impl Iterator<Item = (&'a str, Status)>) -> String {
    let mut s = String::new();
    for (name, st) in entries {
        s.push_str(&format!(
            "{} {} {}\n",
            name,
            st.state.as_str(),
            st.pid.map_or(String::from("-"), |p| p.to_string())
        ));
    }
    s
}

pub fn parse_status(text: &str) -> Vec<(String, Status)> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let name = f.next()?;
            let state = State::parse(f.next()?)?;
            let pid = f.next().and_then(|p| p.parse().ok());
            Some((name.to_string(), Status { state, pid }))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Control protocol
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    Restart,
}

impl Command {
    pub fn as_str(self) -> &'static str {
        match self {
            Command::Start => "start",
            Command::Stop => "stop",
            Command::Restart => "restart",
        }
    }

    pub fn parse(s: &str) -> Option<Command> {
        match s {
            "start" => Some(Command::Start),
            "stop" => Some(Command::Stop),
            "restart" => Some(Command::Restart),
            _ => None,
        }
    }
}

/// A request line: `COMMAND NAME ID` (ID: the client's pid, which names
/// the reply file).
pub fn request_line(cmd: Command, name: &str, id: u32) -> String {
    format!("{} {} {}\n", cmd.as_str(), name, id)
}

pub fn parse_request(line: &str) -> Option<(Command, String, u32)> {
    let mut f = line.split_whitespace();
    let cmd = Command::parse(f.next()?)?;
    let name = f.next()?;
    let id = f.next()?.parse().ok()?;
    (valid_name(name) && f.next().is_none()).then(|| (cmd, name.to_string(), id))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Ok,
    Unknown,
    Error(String),
}

impl Reply {
    pub fn render(&self) -> String {
        match self {
            Reply::Ok => String::from("ok\n"),
            Reply::Unknown => String::from("unknown\n"),
            Reply::Error(m) => format!("error {}\n", m.replace('\n', " ")),
        }
    }

    pub fn parse(s: &str) -> Option<Reply> {
        let s = s.trim_end_matches('\n');
        match s {
            "ok" => Some(Reply::Ok),
            "unknown" => Some(Reply::Unknown),
            _ => s
                .strip_prefix("error ")
                .map(|m| Reply::Error(m.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Restart back-off
// ---------------------------------------------------------------------------

/// Failures within this window count towards giving up.
pub const FAILURE_WINDOW_MS: u64 = 60_000;
/// Give up (state failed) at this many failures within the window.
pub const MAX_FAILURES: usize = 5;
pub const MIN_DELAY_MS: u64 = 1_000;
pub const MAX_DELAY_MS: u64 = 30_000;
/// A run at least this long resets the back-off delay.
pub const HEALTHY_RUN_MS: u64 = 60_000;

/// Restart policy state of one service: 1 s, 2 s, 4 s ... 30 s between
/// consecutive failed runs; failed after 5 failures within 60 s.
#[derive(Clone, Debug, Default)]
pub struct Backoff {
    failures: Vec<u64>,
    consecutive: u32,
}

impl Backoff {
    /// Forget past failures (a manual start).
    pub fn reset(&mut self) {
        self.failures.clear();
        self.consecutive = 0;
    }

    /// A run of `ran_ms` ended at `now_ms`; `failed` if it exited with an
    /// error or a signal. Returns the delay before restarting, or None
    /// to give up.
    pub fn exited(&mut self, now_ms: u64, ran_ms: u64, failed: bool) -> Option<u64> {
        if ran_ms >= HEALTHY_RUN_MS {
            self.consecutive = 0;
        }
        if !failed {
            return Some(MIN_DELAY_MS);
        }
        self.failures
            .retain(|&t| now_ms.saturating_sub(t) < FAILURE_WINDOW_MS);
        self.failures.push(now_ms);
        if self.failures.len() >= MAX_FAILURES {
            return None;
        }
        let shift = self.consecutive.min(5);
        self.consecutive += 1;
        Some((MIN_DELAY_MS << shift).min(MAX_DELAY_MS))
    }
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// One entry of `svc list --json` / `svc status --json`.
#[derive(Clone, Debug)]
pub struct Info<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub enabled: bool,
    pub state: State,
    pub pid: Option<u32>,
    pub tty: Option<&'a str>,
}

/// A JSON string literal.
pub fn json_string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

impl Info<'_> {
    /// `{"name":..,"description":..,"enabled":..,"state":..,"pid":..,"tty":..}`
    pub fn to_json(&self) -> String {
        format!(
            "{{\"name\":{},\"description\":{},\"enabled\":{},\"state\":\"{}\",\"pid\":{},\"tty\":{}}}",
            json_string(self.name),
            json_string(self.description),
            self.enabled,
            self.state.as_str(),
            self.pid.map_or(String::from("null"), |p| p.to_string()),
            self.tty.map_or(String::from("null"), json_string),
        )
    }
}

/// A JSON array of entries.
pub fn json_list(items: &[Info]) -> String {
    let objs: Vec<String> = items.iter().map(|i| i.to_json()).collect();
    format!("[{}]", objs.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn parses_a_service() {
        let (s, w) = parse(
            "edex",
            "# eDEX\ndescription = \"eDEX-DE greeter\"\nexec=/usr/bin/edex-comp --greeter\n\
             tty=/dev/tty1\nuser=ari\nrestart=always\nafter=seatd dbus seatd\n\
             env=XDG_RUNTIME_DIR=/run/user/0\nenv=A=\"b c\"\ncolour=blue\n",
        )
        .unwrap();
        assert_eq!(s.description, "eDEX-DE greeter");
        assert_eq!(s.exec, "/usr/bin/edex-comp --greeter");
        assert_eq!(s.tty.as_deref(), Some("tty1"));
        assert_eq!(s.user.as_deref(), Some("ari"));
        assert_eq!(s.restart, Restart::Always);
        assert_eq!(s.after, vec!["seatd", "dbus"]);
        assert_eq!(
            s.env,
            vec![
                ("XDG_RUNTIME_DIR".into(), "/run/user/0".into()),
                ("A".into(), "b c".into())
            ]
        );
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn defaults_and_errors() {
        let (s, _) = parse("tor", "exec=tor").unwrap();
        assert_eq!(s.restart, Restart::OnFailure);
        assert_eq!((s.tty, s.user), (None, None));
        assert!(parse("tor", "description=x").is_err());
        assert!(parse("tor", "exec=tor\nrestart=sometimes").is_err());
        assert!(parse("tor", "exec=tor\ntty=console").is_err());
        assert!(parse("tor", "exec=tor\nnonsense").is_err());
        assert!(parse("../x", "exec=tor").is_err());
        assert!(parse("tor", "exec=tor\nenv=1A=b").is_err());
        assert_eq!(parse("t", "exec=t\nuser=root").unwrap().0.user, None);
    }

    #[test]
    fn names() {
        assert!(valid_name("rustos-nmd"));
        assert!(valid_name("getty@tty2"));
        assert!(!valid_name("-x"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(""));
    }

    #[test]
    fn shell_detection() {
        assert!(!needs_shell("/usr/bin/tor -f /etc/tor/torrc"));
        assert!(!needs_shell("httpd -p 80 --root=/srv"));
        assert!(needs_shell("while true; do sleep 1; done"));
        assert!(needs_shell("echo $HOME"));
        assert!(needs_shell("a > b"));
        assert_eq!(split_command(" a  b c "), vec!["a", "b", "c"]);
    }

    #[test]
    fn enabled_list() {
        let t = "# enabled services\ndbus\n\ntor # anonymity\ndbus\n";
        assert_eq!(parse_enabled(t), vec!["dbus", "tor"]);
        let e = edit_enabled(t, "seatd", true);
        assert_eq!(parse_enabled(&e), vec!["dbus", "tor", "seatd"]);
        assert!(e.starts_with("# enabled services\n"));
        let d = edit_enabled(&e, "dbus", false);
        assert_eq!(parse_enabled(&d), vec!["tor", "seatd"]);
        assert_eq!(edit_enabled(&d, "tor", true), d);
        assert_eq!(edit_enabled("", "x", true), "x\n");
    }

    #[test]
    fn ordering() {
        let deps = |n: &str| -> Vec<String> {
            match n {
                "edex" => vec!["seatd".into(), "dbus".into()],
                "upower" => vec!["dbus".into()],
                "a" => vec!["b".into()],
                "b" => vec!["a".into()],
                _ => vec![],
            }
        };
        let w: Vec<String> = ["edex", "upower", "dbus", "seatd"]
            .map(String::from)
            .to_vec();
        let o = start_order(&w, deps);
        let pos = |n: &str| o.iter().position(|x| x == n).unwrap();
        assert!(pos("seatd") < pos("edex") && pos("dbus") < pos("edex"));
        assert!(pos("dbus") < pos("upower"));
        assert_eq!(o.len(), 4);
        // Dependencies that are not wanted are ignored; cycles end.
        assert_eq!(start_order(&["edex".into()], deps), vec!["edex"]);
        assert_eq!(start_order(&["a".into(), "b".into()], deps).len(), 2);
    }

    #[test]
    fn status_round_trip() {
        let e = [
            (
                "tor",
                Status {
                    state: State::Running,
                    pid: Some(42),
                },
            ),
            (
                "dbus",
                Status {
                    state: State::Failed,
                    pid: None,
                },
            ),
        ];
        let t = render_status(e.iter().map(|(n, s)| (*n, *s)));
        assert_eq!(t, "tor running 42\ndbus failed -\n");
        let p = parse_status(&t);
        assert_eq!(p[0], ("tor".into(), e[0].1));
        assert_eq!(p[1], ("dbus".into(), e[1].1));
    }

    #[test]
    fn protocol() {
        let l = request_line(Command::Restart, "tor", 77);
        assert_eq!(l, "restart tor 77\n");
        assert_eq!(
            parse_request(&l),
            Some((Command::Restart, "tor".into(), 77))
        );
        assert_eq!(parse_request("start ../x 1"), None);
        assert_eq!(parse_request("halt tor 1"), None);
        for r in [
            Reply::Ok,
            Reply::Unknown,
            Reply::Error("no such file".into()),
        ] {
            assert_eq!(Reply::parse(&r.render()), Some(r));
        }
    }

    #[test]
    fn backoff() {
        let mut b = Backoff::default();
        // 1, 2, 4, 8 s, then the fifth failure within 60 s gives up.
        assert_eq!(b.exited(0, 10, true), Some(1000));
        assert_eq!(b.exited(1000, 10, true), Some(2000));
        assert_eq!(b.exited(3000, 10, true), Some(4000));
        assert_eq!(b.exited(7000, 10, true), Some(8000));
        assert_eq!(b.exited(15000, 10, true), None);
        // Failures spread out keep backing off up to 30 s.
        let mut b = Backoff::default();
        let mut t = 0;
        let mut last = 0;
        for _ in 0..8 {
            t += 25_000;
            last = b.exited(t, 20_000, true).unwrap();
        }
        assert_eq!(last, MAX_DELAY_MS);
        // A clean exit restarts after the minimum delay; a long run resets.
        assert_eq!(b.exited(t + 1, 5, false), Some(MIN_DELAY_MS));
        assert_eq!(b.exited(t + 200_000, 120_000, true), Some(MIN_DELAY_MS));
        b.reset();
        assert_eq!(b.exited(0, 0, true), Some(1000));
    }

    #[test]
    fn json() {
        let i = Info {
            name: "tor",
            description: "Tor anonymity daemon",
            enabled: false,
            state: State::Stopped,
            pid: None,
            tty: None,
        };
        assert_eq!(
            i.to_json(),
            r#"{"name":"tor","description":"Tor anonymity daemon","enabled":false,"state":"stopped","pid":null,"tty":null}"#
        );
        let j = Info {
            name: "edex",
            description: "say \"hi\"\\\n",
            enabled: true,
            state: State::Running,
            pid: Some(12),
            tty: Some("tty1"),
        };
        assert_eq!(
            j.to_json(),
            r#"{"name":"edex","description":"say \"hi\"\\\n","enabled":true,"state":"running","pid":12,"tty":"tty1"}"#
        );
        assert_eq!(json_list(&[]), "[]");
        let one = i.to_json();
        assert_eq!(json_list(&[i.clone(), i]), format!("[{},{}]", one, one));
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    }
}
