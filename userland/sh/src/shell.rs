//! Shell state and command execution.

use crate::ast::*;
use crate::expand;
use alloc::collections::BTreeMap;
use rustos_rt::prelude::*;
use rustos_rt::{env, fs, io, process, signal, term};

#[derive(Clone)]
pub struct Var {
    pub value: String,
    pub exported: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobState {
    Running,
    Stopped,
    Done,
}

pub struct Job {
    pub id: usize,
    pub pgid: i32,
    /// (pid, finished, raw status)
    pub procs: Vec<(i32, bool, i32)>,
    pub text: String,
    pub state: JobState,
    pub notified: bool,
}

/// Pending non-local control flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flow {
    Break(u32),
    Continue(u32),
    Return,
    Exit(i32),
}

pub struct Shell {
    pub vars: BTreeMap<String, Var>,
    pub positional: Vec<String>,
    pub arg0: String,
    pub last_status: i32,
    pub last_bg: Option<i32>,
    pub shell_pid: i32,
    pub functions: BTreeMap<String, Command>,
    pub aliases: BTreeMap<String, String>,
    pub jobs: Vec<Job>,
    pub interactive: bool,
    pub shell_pgid: i32,
    pub opt_errexit: bool,
    pub opt_xtrace: bool,
    pub opt_nounset: bool,
    pub flow: Option<Flow>,
    pub loop_depth: u32,
    pub func_depth: u32,
    pub history: Vec<String>,
    /// Saved local variables per function call.
    pub locals: Vec<Vec<(String, Option<Var>)>>,
    /// True in a forked child (subshell / pipeline member).
    pub is_child: bool,
}

impl Shell {
    pub fn new(arg0: &str, interactive: bool) -> Shell {
        let mut vars = BTreeMap::new();
        for (k, v) in env::vars() {
            vars.insert(
                k,
                Var {
                    value: v,
                    exported: true,
                },
            );
        }
        let mut sh = Shell {
            vars,
            positional: Vec::new(),
            arg0: arg0.to_string(),
            last_status: 0,
            last_bg: None,
            shell_pid: process::getpid(),
            functions: BTreeMap::new(),
            aliases: BTreeMap::new(),
            jobs: Vec::new(),
            interactive,
            shell_pgid: process::getpgrp(),
            opt_errexit: false,
            opt_xtrace: false,
            opt_nounset: false,
            flow: None,
            loop_depth: 0,
            func_depth: 0,
            history: Vec::new(),
            locals: Vec::new(),
            is_child: false,
        };
        if sh.get_var("PATH").is_none() {
            sh.set_var("PATH", "/bin:/sbin:/usr/bin");
            sh.export("PATH");
        }
        if sh.get_var("PS1").is_none() {
            sh.set_var("PS1", "\\u@\\h:\\w\\$ ");
        }
        sh.set_var("PS2", "> ");
        if let Ok(cwd) = env::current_dir() {
            sh.set_var("PWD", &cwd);
            sh.export("PWD");
        }
        sh.set_var("PPID", &format!("{}", process::getppid()));
        sh
    }

    // ------------------------------------------------------------------
    // Variables
    // ------------------------------------------------------------------

    pub fn get_var(&self, name: &str) -> Option<String> {
        match name {
            "RANDOM" => {
                let mut b = [0u8; 2];
                process::getrandom(&mut b);
                Some(format!("{}", u16::from_ne_bytes(b) & 0x7fff))
            }
            "SECONDS" => Some(format!("{}", rustos_rt::time::millis() / 1000)),
            _ => self.vars.get(name).map(|v| v.value.clone()),
        }
    }

    pub fn set_var(&mut self, name: &str, value: &str) {
        let exported = self.vars.get(name).is_some_and(|v| v.exported);
        self.vars.insert(
            name.to_string(),
            Var {
                value: value.to_string(),
                exported,
            },
        );
        if exported {
            env::set_var(name, value);
        }
    }

    pub fn export(&mut self, name: &str) {
        let v = self.vars.entry(name.to_string()).or_insert(Var {
            value: String::new(),
            exported: true,
        });
        v.exported = true;
        let val = v.value.clone();
        env::set_var(name, &val);
    }

    pub fn unset(&mut self, name: &str) {
        self.vars.remove(name);
        env::remove_var(name);
        self.functions.remove(name);
    }

    pub fn environ(&self) -> Vec<String> {
        self.vars
            .iter()
            .filter(|(_, v)| v.exported)
            .map(|(k, v)| format!("{}={}", k, v.value))
            .collect()
    }

    pub fn option_flags(&self) -> String {
        let mut s = String::new();
        if self.opt_errexit {
            s.push('e');
        }
        if self.interactive {
            s.push('i');
        }
        if self.opt_nounset {
            s.push('u');
        }
        if self.opt_xtrace {
            s.push('x');
        }
        s
    }

    // ------------------------------------------------------------------
    // Entry points
    // ------------------------------------------------------------------

    /// Parse and run source text; returns the exit status.
    pub fn run_source(&mut self, src: &str) -> i32 {
        match crate::parser::parse(src) {
            Ok(list) => self.run_list(&list),
            Err(e) => {
                let msg = if e.0 == "incomplete" {
                    String::from("syntax error: unexpected end of file")
                } else {
                    e.0
                };
                eprintln!("sh: {}", msg);
                self.last_status = 2;
                2
            }
        }
    }

    pub fn run_list(&mut self, list: &List) -> i32 {
        for ao in list {
            if self.flow.is_some() {
                break;
            }
            if ao.background {
                self.run_background(ao);
                self.last_status = 0;
            } else {
                let st = self.run_andor(ao);
                self.last_status = st;
                if self.opt_errexit && st != 0 && self.flow.is_none() {
                    self.flow = Some(Flow::Exit(st));
                }
            }
        }
        self.last_status
    }

    fn run_andor(&mut self, ao: &AndOr) -> i32 {
        let mut st = self.run_pipeline(&ao.first, true, &ao.text);
        for (c, p) in &ao.rest {
            if self.flow.is_some() {
                break;
            }
            let run = match c {
                Connector::And => st == 0,
                Connector::Or => st != 0,
            };
            if run {
                st = self.run_pipeline(p, true, &ao.text);
            }
        }
        st
    }

    fn run_background(&mut self, ao: &AndOr) {
        let pid = match process::fork() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("sh: fork: {}", e);
                return;
            }
        };
        if pid == 0 {
            io::discard_buffered();
            self.is_child = true;
            let _ = process::setpgid(0, 0);
            self.reset_child_signals();
            let was_interactive = self.interactive;
            // The job runs in its own group and must not touch the terminal.
            self.interactive = false;
            if !was_interactive {
                // Background jobs in scripts read from /dev/null.
                if let Ok(f) = fs::File::open("/dev/null") {
                    let _ = process::dup2(f.fd(), 0);
                }
            }
            let mut fg = ao.clone();
            fg.background = false;
            let st = self.run_andor(&fg);
            process::exit(st);
        }
        let _ = process::setpgid(pid, pid);
        self.last_bg = Some(pid);
        let id = self.next_job_id();
        if self.interactive {
            eprintln!("[{}] {}", id, pid);
        }
        self.jobs.push(Job {
            id,
            pgid: pid,
            procs: alloc::vec![(pid, false, 0)],
            text: ao.text.clone(),
            state: JobState::Running,
            notified: false,
        });
    }

    fn next_job_id(&self) -> usize {
        (1..)
            .find(|i| !self.jobs.iter().any(|j| j.id == *i))
            .unwrap()
    }

    pub fn reset_child_signals(&self) {
        for s in [
            signal::SIGINT,
            signal::SIGQUIT,
            signal::SIGTSTP,
            signal::SIGTTIN,
            signal::SIGTTOU,
            signal::SIGCHLD,
        ] {
            signal::default(s);
        }
    }

    // ------------------------------------------------------------------
    // Pipelines
    // ------------------------------------------------------------------

    fn run_pipeline(&mut self, p: &Pipeline, _fg: bool, text: &str) -> i32 {
        let st = if p.cmds.len() == 1 && self.runs_in_shell(&p.cmds[0]) {
            self.run_command(&p.cmds[0])
        } else {
            self.run_forked_pipeline(&p.cmds, text)
        };
        if p.negate { (st == 0) as i32 } else { st }
    }

    /// Whether a command can run without forking (builtins, functions,
    /// compound commands, assignments).
    fn runs_in_shell(&self, c: &Command) -> bool {
        match c {
            Command::Simple { words, .. } => {
                let Some(first) = words.first() else {
                    return true;
                };
                match literal(first) {
                    Some(name) => {
                        crate::builtins::is_builtin(&name)
                            || self.functions.contains_key(&name)
                            || self.aliases.contains_key(&name)
                    }
                    None => false,
                }
            }
            Command::Subshell(..) => false,
            _ => true,
        }
    }

    fn run_forked_pipeline(&mut self, cmds: &[Command], text: &str) -> i32 {
        let n = cmds.len();
        let mut pids: Vec<i32> = Vec::new();
        let mut pgid = 0;
        let mut prev_read: Option<i32> = None;
        for (i, c) in cmds.iter().enumerate() {
            let (next_read, write) = if i + 1 < n {
                match process::pipe() {
                    Ok((r, w)) => (Some(r), Some(w)),
                    Err(e) => {
                        eprintln!("sh: pipe: {}", e);
                        return 1;
                    }
                }
            } else {
                (None, None)
            };
            let pid = match process::fork() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("sh: fork: {}", e);
                    return 1;
                }
            };
            if pid == 0 {
                io::discard_buffered();
                self.is_child = true;
                let me = process::getpid();
                let pg = if pgid == 0 { me } else { pgid };
                if self.interactive && !self.is_nested_child() {
                    let _ = process::setpgid(0, pg);
                    term::tcsetpgrp(0, pg);
                }
                self.reset_child_signals();
                if let Some(r) = prev_read {
                    let _ = process::dup2(r, 0);
                    process::close(r);
                }
                if let Some(w) = write {
                    let _ = process::dup2(w, 1);
                    process::close(w);
                }
                if let Some(r) = next_read {
                    process::close(r);
                }
                let st = self.run_in_child(c);
                process::exit(st);
            }
            if pgid == 0 {
                pgid = pid;
            }
            if self.interactive {
                let _ = process::setpgid(pid, pgid);
            }
            pids.push(pid);
            if let Some(r) = prev_read {
                process::close(r);
            }
            if let Some(w) = write {
                process::close(w);
            }
            prev_read = next_read;
        }
        if self.interactive && !self.is_child {
            term::tcsetpgrp(0, pgid);
        }
        let st = self.wait_foreground(pgid, pids, text);
        if self.interactive && !self.is_child {
            term::tcsetpgrp(0, self.shell_pgid);
        }
        st
    }

    fn is_nested_child(&self) -> bool {
        // A subshell's own children stay in the subshell's process group.
        false
    }

    /// Run a command in an already-forked child and return its status.
    fn run_in_child(&mut self, c: &Command) -> i32 {
        match c {
            Command::Simple { .. } => {
                let st = self.run_command(c);
                self.exit_status_after(st)
            }
            Command::Subshell(list, redirs) => {
                let saved = match self.apply_redirs(redirs) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("sh: {}", e);
                        return 1;
                    }
                };
                let st = self.run_list(list);
                self.restore_redirs(saved);
                self.exit_status_after(st)
            }
            _ => {
                let st = self.run_command(c);
                self.exit_status_after(st)
            }
        }
    }

    fn exit_status_after(&mut self, st: i32) -> i32 {
        match self.flow {
            Some(Flow::Exit(code)) => code,
            _ => st,
        }
    }

    /// Wait for a foreground process group; handles stop (Ctrl-Z).
    fn wait_foreground(&mut self, pgid: i32, pids: Vec<i32>, text: &str) -> i32 {
        let mut procs: Vec<(i32, bool, i32)> = pids.iter().map(|&p| (p, false, 0)).collect();
        loop {
            if procs.iter().all(|p| p.1) {
                break;
            }
            // Interactive shells put each job in its own process group;
            // scripts do not, so wait for the members individually.
            let target = if self.interactive && !self.is_child {
                -pgid
            } else {
                procs.iter().find(|p| !p.1).map_or(-pgid, |p| p.0)
            };
            match process::waitpid(target, process::WUNTRACED) {
                Ok((pid, st)) => {
                    if process::wifstopped(st) {
                        let id = self.next_job_id();
                        if let Some(p) = procs.iter_mut().find(|p| p.0 == pid) {
                            p.2 = st;
                        }
                        eprintln!("\n[{}]+  Stopped                 {}", id, text);
                        self.jobs.push(Job {
                            id,
                            pgid,
                            procs,
                            text: text.to_string(),
                            state: JobState::Stopped,
                            notified: true,
                        });
                        return 128 + process::wstopsig(st);
                    }
                    if let Some(p) = procs.iter_mut().find(|p| p.0 == pid) {
                        p.1 = true;
                        p.2 = st;
                    }
                }
                Err(rustos_rt::Error(4)) => continue,
                Err(_) => {
                    // Children may have been reaped elsewhere.
                    for p in procs.iter_mut() {
                        if let Ok((r, st)) = process::waitpid(p.0, process::WNOHANG)
                            && r == p.0
                        {
                            p.1 = true;
                            p.2 = st;
                        } else {
                            p.1 = true;
                        }
                    }
                }
            }
        }
        let last = procs.last().map_or(0, |p| p.2);
        if process::wifsignaled(last) {
            let sig = process::wtermsig(last);
            if sig != signal::SIGINT && sig != signal::SIGPIPE {
                eprintln!("{}", signal::name(sig));
            }
        }
        process::status_code(last)
    }

    /// Resume a job in the foreground.
    pub fn foreground_job(&mut self, idx: usize) -> i32 {
        let job = self.jobs.remove(idx);
        eprintln!("{}", job.text);
        if self.interactive {
            term::tcsetpgrp(0, job.pgid);
        }
        let _ = process::kill(-job.pgid, signal::SIGCONT);
        let pids: Vec<i32> = job.procs.iter().filter(|p| !p.1).map(|p| p.0).collect();
        let st = self.wait_foreground(job.pgid, pids, &job.text);
        if self.interactive {
            term::tcsetpgrp(0, self.shell_pgid);
        }
        st
    }

    /// Collect finished background jobs; print notifications if requested.
    pub fn reap_jobs(&mut self, notify: bool) {
        loop {
            match process::waitpid(-1, process::WNOHANG | process::WUNTRACED) {
                Ok((0, _)) | Err(_) => break,
                Ok((pid, st)) => {
                    for j in self.jobs.iter_mut() {
                        if let Some(p) = j.procs.iter_mut().find(|p| p.0 == pid) {
                            if process::wifstopped(st) {
                                j.state = JobState::Stopped;
                                j.notified = false;
                            } else {
                                p.1 = true;
                                p.2 = st;
                            }
                        }
                        if j.procs.iter().all(|p| p.1) && j.state != JobState::Done {
                            j.state = JobState::Done;
                            j.notified = false;
                        }
                    }
                }
            }
        }
        if notify {
            for j in self.jobs.iter_mut() {
                if !j.notified && j.state != JobState::Running {
                    j.notified = true;
                    eprintln!("[{}]+  {:<22}  {}", j.id, job_status(j), j.text);
                }
            }
        }
        self.jobs
            .retain(|j| !(j.state == JobState::Done && j.notified));
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    pub fn run_command(&mut self, c: &Command) -> i32 {
        match c {
            Command::Simple {
                assigns,
                words,
                redirs,
            } => self.run_simple(assigns, words, redirs),
            Command::If {
                branches,
                otherwise,
                redirs,
            } => self.with_redirs(redirs, |sh| {
                for (cond, body) in branches {
                    let st = sh.run_list_noerr(cond);
                    if sh.flow.is_some() {
                        return st;
                    }
                    if st == 0 {
                        return sh.run_list(body);
                    }
                }
                match otherwise {
                    Some(b) => sh.run_list(b),
                    None => 0,
                }
            }),
            Command::While {
                cond,
                body,
                until,
                redirs,
            } => self.with_redirs(redirs, |sh| {
                let mut st = 0;
                sh.loop_depth += 1;
                loop {
                    let c = sh.run_list_noerr(cond);
                    if sh.flow.is_some() {
                        break;
                    }
                    if (c == 0) == *until {
                        break;
                    }
                    st = sh.run_list(body);
                    if sh.loop_control() {
                        break;
                    }
                }
                sh.loop_depth -= 1;
                st
            }),
            Command::For {
                var,
                items,
                body,
                redirs,
            } => self.with_redirs(redirs, |sh| {
                let values = match items {
                    Some(ws) => match expand::expand_words(sh, ws) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("sh: {}", e);
                            return 1;
                        }
                    },
                    None => sh.positional.clone(),
                };
                let mut st = 0;
                sh.loop_depth += 1;
                for v in values {
                    sh.set_var(var, &v);
                    st = sh.run_list(body);
                    if sh.loop_control() {
                        break;
                    }
                }
                sh.loop_depth -= 1;
                st
            }),
            Command::Case { word, arms, redirs } => self.with_redirs(redirs, |sh| {
                let w = match expand::expand_word_str(sh, word) {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("sh: {}", e);
                        return 1;
                    }
                };
                for (pats, body) in arms {
                    for p in pats {
                        let pat = expand::expand_pattern(sh, p).unwrap_or_default();
                        if expand::fnmatch(&pat, &w) {
                            return sh.run_list(body);
                        }
                    }
                }
                0
            }),
            Command::Group(list, redirs) => self.with_redirs(redirs, |sh| sh.run_list(list)),
            Command::Subshell(..) => self.run_forked_pipeline(core::slice::from_ref(c), ""),
            Command::FuncDef(name, body) => {
                self.functions.insert(name.clone(), (**body).clone());
                0
            }
        }
    }

    /// Run a condition list: `set -e` does not apply inside conditions.
    fn run_list_noerr(&mut self, l: &List) -> i32 {
        let e = self.opt_errexit;
        self.opt_errexit = false;
        let st = self.run_list(l);
        self.opt_errexit = e;
        st
    }

    /// Handle break/continue after a loop body; returns true to leave the loop.
    fn loop_control(&mut self) -> bool {
        match self.flow {
            Some(Flow::Break(n)) => {
                self.flow = if n > 1 {
                    Some(Flow::Break(n - 1))
                } else {
                    None
                };
                true
            }
            Some(Flow::Continue(n)) => {
                if n > 1 {
                    self.flow = Some(Flow::Continue(n - 1));
                    true
                } else {
                    self.flow = None;
                    false
                }
            }
            Some(_) => true,
            None => false,
        }
    }

    fn with_redirs(&mut self, redirs: &[Redir], f: impl FnOnce(&mut Shell) -> i32) -> i32 {
        if redirs.is_empty() {
            return f(self);
        }
        let saved = match self.apply_redirs(redirs) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("sh: {}", e);
                return 1;
            }
        };
        let st = f(self);
        self.restore_redirs(saved);
        st
    }

    fn run_simple(&mut self, assigns: &[(String, Word)], words: &[Word], redirs: &[Redir]) -> i32 {
        // Expand assignments first (left to right).
        let mut vals = Vec::new();
        for (name, w) in assigns {
            match expand::expand_word_str(self, w) {
                Ok(v) => vals.push((name.clone(), v)),
                Err(e) => {
                    eprintln!("sh: {}", e);
                    return 1;
                }
            }
        }
        let mut argv = match expand::expand_words(self, words) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("sh: {}", e);
                return 1;
            }
        };
        // Aliases (first word, not recursively).
        if let Some(first) = argv.first()
            && let Some(a) = self.aliases.get(first).cloned()
        {
            let expanded = crate::builtins::split_words(&a);
            argv.splice(0..1, expanded);
        }
        if self.opt_xtrace && !argv.is_empty() {
            let mut line = String::from("+");
            for (k, v) in &vals {
                line.push_str(&format!(" {}={}", k, v));
            }
            for a in &argv {
                line.push(' ');
                line.push_str(a);
            }
            eprintln!("{}", line);
        }
        if argv.is_empty() {
            for (k, v) in &vals {
                self.set_var(k, v);
            }
            // Redirections still happen (e.g. `> file` truncates).
            return match self.apply_redirs(redirs) {
                Ok(s) => {
                    self.restore_redirs(s);
                    0
                }
                Err(e) => {
                    eprintln!("sh: {}", e);
                    1
                }
            };
        }
        let name = argv[0].clone();

        // Functions.
        if let Some(body) = self.functions.get(&name).cloned() {
            return self.with_redirs(redirs, |sh| {
                let saved_pos = core::mem::replace(&mut sh.positional, argv[1..].to_vec());
                sh.func_depth += 1;
                sh.locals.push(Vec::new());
                let st = sh.run_command(&body);
                if let Some(locals) = sh.locals.pop() {
                    for (k, old) in locals.into_iter().rev() {
                        match old {
                            Some(v) => {
                                sh.vars.insert(k, v);
                            }
                            None => {
                                sh.vars.remove(&k);
                            }
                        }
                    }
                }
                sh.func_depth -= 1;
                sh.positional = saved_pos;
                if sh.flow == Some(Flow::Return) {
                    sh.flow = None;
                }
                st
            });
        }

        // Builtins.
        if crate::builtins::is_builtin(&name) {
            // Prefix assignments apply for the duration of the builtin.
            let saved: Vec<(String, Option<Var>)> = vals
                .iter()
                .map(|(k, v)| {
                    let old = self.vars.get(k).cloned();
                    self.set_var(k, v);
                    (k.clone(), old)
                })
                .collect();
            let st = self.with_redirs(redirs, |sh| crate::builtins::run(sh, &argv));
            if name != "export" && name != "readonly" {
                for (k, old) in saved {
                    match old {
                        Some(v) => {
                            self.vars.insert(k, v);
                        }
                        None => {
                            self.vars.remove(&k);
                        }
                    }
                }
            }
            io::flush();
            return st;
        }

        // External command.
        let Some(path) = process::find_in_path(&name) else {
            eprintln!("sh: {}: command not found", name);
            return 127;
        };
        let mut envp = self.environ();
        for (k, v) in &vals {
            envp.retain(|e| !e.starts_with(&format!("{}=", k)));
            envp.push(format!("{}={}", k, v));
        }
        if self.is_child {
            // Already forked (pipeline member): exec directly.
            if let Err(e) = self.apply_redirs(redirs) {
                eprintln!("sh: {}", e);
                return 1;
            }
            io::flush();
            let e = process::execve(&path, &argv, &envp);
            eprintln!("sh: {}: {}", name, e);
            return if e.0 == 2 { 127 } else { 126 };
        }
        let pid = match process::fork() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("sh: fork: {}", e);
                return 1;
            }
        };
        if pid == 0 {
            io::discard_buffered();
            if self.interactive {
                let _ = process::setpgid(0, 0);
                term::tcsetpgrp(0, process::getpid());
            }
            self.reset_child_signals();
            if let Err(e) = self.apply_redirs(redirs) {
                eprintln!("sh: {}", e);
                process::exit(1);
            }
            let e = process::execve(&path, &argv, &envp);
            eprintln!("sh: {}: {}", name, e);
            process::exit(if e.0 == 2 { 127 } else { 126 });
        }
        if self.interactive {
            let _ = process::setpgid(pid, pid);
            term::tcsetpgrp(0, pid);
        }
        let text = argv.join(" ");
        let st = self.wait_foreground(pid, alloc::vec![pid], &text);
        if self.interactive {
            term::tcsetpgrp(0, self.shell_pgid);
        }
        st
    }

    // ------------------------------------------------------------------
    // Redirections
    // ------------------------------------------------------------------

    /// Apply redirections, returning (fd, saved copy) pairs to restore.
    pub fn apply_redirs(&mut self, redirs: &[Redir]) -> Result<Vec<(i32, i32)>, String> {
        let mut saved = Vec::new();
        for r in redirs {
            let fds: Vec<i32> = if r.fd == -1 {
                alloc::vec![1, 2]
            } else {
                alloc::vec![r.fd]
            };
            let src_fd = match &r.kind {
                RedirKind::Here(body, expand) => {
                    let text = body.borrow().clone();
                    let text = if *expand {
                        expand::expand_inline(self, &text).map_err(|e| e.to_string())?
                    } else {
                        text
                    };
                    let (rd, wr) = process::pipe().map_err(|e| format!("pipe: {}", e))?;
                    // Small bodies fit in the pipe buffer; write and close.
                    let _ = io::write_all(wr, text.as_bytes());
                    process::close(wr);
                    rd
                }
                RedirKind::Dup => {
                    let t = expand::expand_word_str(self, &r.target)?;
                    if t == "-" {
                        for &fd in &fds {
                            saved.push((fd, save_fd(fd)));
                            process::close(fd);
                        }
                        continue;
                    }
                    let src: i32 = t
                        .parse()
                        .map_err(|_| format!("{}: ambiguous redirect", t))?;
                    for &fd in &fds {
                        saved.push((fd, save_fd(fd)));
                        process::dup2(src, fd).map_err(|e| format!("{}: {}", src, e))?;
                    }
                    continue;
                }
                kind => {
                    let target = expand::expand_word_str(self, &r.target)?;
                    let flags = match kind {
                        RedirKind::In => fs::O_RDONLY,
                        RedirKind::Out => fs::O_WRONLY | fs::O_CREAT | fs::O_TRUNC,
                        RedirKind::Append => fs::O_WRONLY | fs::O_CREAT | fs::O_APPEND,
                        _ => fs::O_RDWR | fs::O_CREAT,
                    };
                    let f = fs::File::open_with(&target, flags & !fs::O_CLOEXEC, 0o666)
                        .map_err(|e| format!("{}: {}", target, e))?;
                    f.into_raw()
                }
            };
            for &fd in &fds {
                saved.push((fd, save_fd(fd)));
                let _ = process::dup2(src_fd, fd);
            }
            if !fds.contains(&src_fd) {
                process::close(src_fd);
            }
            // The opened descriptor must not leak into exec'd programs.
            for &fd in &fds {
                process::set_cloexec(fd, false);
            }
        }
        Ok(saved)
    }

    pub fn restore_redirs(&mut self, saved: Vec<(i32, i32)>) {
        io::flush();
        for (fd, copy) in saved.into_iter().rev() {
            if copy >= 0 {
                let _ = process::dup2(copy, fd);
                process::close(copy);
            } else {
                process::close(fd);
            }
        }
    }

    // ------------------------------------------------------------------
    // Command substitution
    // ------------------------------------------------------------------

    pub fn capture(&mut self, src: &str) -> String {
        let (r, w) = match process::pipe() {
            Ok(p) => p,
            Err(_) => return String::new(),
        };
        let pid = match process::fork() {
            Ok(p) => p,
            Err(_) => return String::new(),
        };
        if pid == 0 {
            io::discard_buffered();
            self.is_child = true;
            self.interactive = false;
            process::close(r);
            let _ = process::dup2(w, 1);
            process::close(w);
            let st = self.run_source(src);
            let st = self.exit_status_after(st);
            process::exit(st);
        }
        process::close(w);
        let f = fs::File::from_raw(r);
        let out = f.read_to_end().unwrap_or_default();
        let st = loop {
            match process::waitpid(pid, 0) {
                Ok((_, st)) => break st,
                Err(rustos_rt::Error(4)) => continue,
                Err(_) => break 0,
            }
        };
        self.last_status = process::status_code(st);
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// Human-readable state of a job ("Running", "Done", "Exit 3", "Terminated").
pub fn job_status(j: &Job) -> String {
    match j.state {
        JobState::Running => String::from("Running"),
        JobState::Stopped => String::from("Stopped"),
        JobState::Done => {
            let st = j.procs.last().map_or(0, |p| p.2);
            if process::wifsignaled(st) {
                String::from(signal::name(process::wtermsig(st)))
            } else {
                match process::status_code(st) {
                    0 => String::from("Done"),
                    c => format!("Exit {}", c),
                }
            }
        }
    }
}

/// Duplicate `fd` above 10 so it can be restored (-1 if it was closed).
fn save_fd(fd: i32) -> i32 {
    let r = rustos_rt::sys::syscall(rustos_rt::sys::nr::FCNTL, &[fd as usize, 1030, 10]);
    if r < 0 { -1 } else { r as i32 }
}

/// The literal text of a word if it has no expansions.
pub fn literal(w: &Word) -> Option<String> {
    let mut s = String::new();
    for p in w {
        match p {
            WordPart::Lit(t) | WordPart::Quoted(t) => s.push_str(t),
            _ => return None,
        }
    }
    Some(s)
}
