//! The JavaScript helper process (`/usr/libexec/jsd`): one per page,
//! spoken to with JSON lines over a pipe pair (see docs/JAVASCRIPT.md).
//! jsd gets only its pipes: stderr goes to /dev/null and every other file
//! descriptor is closed before exec.

use jsproto::Json;
use rustos_rt::io::{self, POLLIN, PollFd};
use rustos_rt::prelude::*;
use rustos_rt::{env, fs, process, time};

pub const JSD: &str = "/usr/libexec/jsd";

pub struct Js {
    pid: i32,
    tx: i32,
    rx: i32,
    buf: Vec<u8>,
    pub dead: bool,
}

/// Whether JavaScript can run here.
pub fn available() -> bool {
    fs::metadata(JSD).is_ok()
}

impl Js {
    pub fn spawn() -> Option<Js> {
        let (to_child_r, to_child_w) = process::pipe().ok()?;
        let (from_child_r, from_child_w) = process::pipe().ok()?;
        let t0 = time::millis();
        let pid = match process::fork() {
            Ok(p) => p,
            Err(_) => {
                for fd in [to_child_r, to_child_w, from_child_r, from_child_w] {
                    process::close(fd);
                }
                return None;
            }
        };
        if pid == 0 {
            io::discard_buffered();
            let _ = process::dup2(to_child_r, 0);
            let _ = process::dup2(from_child_w, 1);
            if let Ok(null) = fs::File::open("/dev/null") {
                let _ = process::dup2(null.fd(), 2);
            }
            for fd in 3..256 {
                process::close(fd);
            }
            let argv = vec![String::from(JSD)];
            let _ = process::execve(JSD, &argv, &env::environ());
            process::exit(127);
        }
        if env::var("BROWSE_DEBUG").is_some() {
            eprintln!("jsd fork took {} ms", time::millis() - t0);
        }
        process::close(to_child_r);
        process::close(from_child_w);
        process::set_cloexec(to_child_w, true);
        process::set_cloexec(from_child_r, true);
        Some(Js {
            pid,
            tx: to_child_w,
            rx: from_child_r,
            buf: Vec::new(),
            dead: false,
        })
    }

    pub fn fd(&self) -> i32 {
        self.rx
    }

    pub fn send(&mut self, m: &Json) {
        if self.dead {
            return;
        }
        let mut line = m.to_json();
        line.push('\n');
        let mut b = line.as_bytes();
        while !b.is_empty() {
            match io::write(self.tx, b) {
                Ok(n) if n > 0 => b = &b[n..],
                _ => {
                    self.dead = true;
                    return;
                }
            }
        }
    }

    /// A complete line already buffered.
    fn take_line(&mut self) -> Option<Json> {
        loop {
            let nl = self.buf.iter().position(|&c| c == b'\n')?;
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let s = String::from_utf8_lossy(&line[..nl]).into_owned();
            if let Ok(j) = Json::parse(&s) {
                return Some(j);
            }
        }
    }

    pub fn has_buffered(&self) -> bool {
        self.buf.contains(&b'\n')
    }

    /// The next message, waiting up to `timeout_ms` (-1: forever). None on
    /// timeout or when jsd has gone.
    pub fn recv(&mut self, timeout_ms: i32) -> Option<Json> {
        let deadline = if timeout_ms >= 0 {
            Some(time::millis() + timeout_ms as u64)
        } else {
            None
        };
        loop {
            if let Some(m) = self.take_line() {
                return Some(m);
            }
            if self.dead {
                return None;
            }
            let wait = match deadline {
                Some(d) => d.saturating_sub(time::millis()) as i32,
                None => -1,
            };
            let mut fds = [PollFd {
                fd: self.rx,
                events: POLLIN,
                revents: 0,
            }];
            if io::poll(&mut fds, wait).unwrap_or(0) == 0 {
                return None;
            }
            let mut b = [0u8; 16384];
            match io::read(self.rx, &mut b) {
                Ok(n) if n > 0 => self.buf.extend_from_slice(&b[..n]),
                _ => self.dead = true,
            }
        }
    }
}

impl Drop for Js {
    fn drop(&mut self) {
        process::close(self.tx);
        process::close(self.rx);
        let _ = process::kill(self.pid, 9);
        let _ = process::waitpid(self.pid, 0);
    }
}
