//! Terminal handling: raw mode, alternate screen, key decoding and a
//! single-line editor for prompts and form fields.

use rustos_rt::io::{self, POLLIN, PollFd};
use rustos_rt::prelude::*;
use rustos_rt::term::{self, Termios};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Esc,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    /// Control + letter (lower case), e.g. Ctrl('c').
    Ctrl(char),
}

pub struct Term {
    saved: Option<Termios>,
    pending: Vec<u8>,
}

impl Term {
    /// Enter raw mode (no echo, no line buffering, no signals: Ctrl-C
    /// arrives as a key) and the alternate screen.
    pub fn enter() -> Term {
        let saved = term::get(0);
        if let Some(mut t) = saved {
            t.lflag &= !(term::ICANON | term::ECHO | term::ISIG);
            t.iflag &= !term::ICRNL;
            t.cc[6] = 1;
            t.cc[5] = 0;
            term::set(0, &t);
        }
        out("\x1b[?1049h\x1b[?25l");
        Term {
            saved,
            pending: Vec::new(),
        }
    }

    pub fn leave(&mut self) {
        out("\x1b[0m\x1b[?25h\x1b[?1049l");
        if let Some(t) = self.saved.take() {
            term::set(0, &t);
        }
    }

    pub fn size(&self) -> (usize, usize) {
        let (r, c) = term::size(1);
        (r.max(5) as usize, c.max(20) as usize)
    }

    /// Input already read but not yet decoded.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn byte(&mut self, timeout_ms: i32) -> Option<u8> {
        if !self.pending.is_empty() {
            return Some(self.pending.remove(0));
        }
        if timeout_ms >= 0 {
            let mut fds = [PollFd {
                fd: 0,
                events: POLLIN,
                revents: 0,
            }];
            if io::poll(&mut fds, timeout_ms).unwrap_or(0) == 0 {
                return None;
            }
        }
        let mut b = [0u8; 64];
        match io::read(0, &mut b) {
            Ok(n) if n > 0 => {
                self.pending.extend_from_slice(&b[1..n]);
                Some(b[0])
            }
            _ => None,
        }
    }

    /// Wait for a key; `None` on timeout (`timeout_ms` < 0 waits forever)
    /// or end of input.
    pub fn key(&mut self, timeout_ms: i32) -> Option<Key> {
        let b = self.byte(timeout_ms)?;
        Some(match b {
            b'\r' | b'\n' => Key::Enter,
            b'\t' => Key::Tab,
            0x7f | 0x08 => Key::Backspace,
            0x1b => {
                let Some(b1) = self.byte(60) else {
                    return Some(Key::Esc);
                };
                if b1 != b'[' && b1 != b'O' {
                    return Some(Key::Esc);
                }
                let mut params = Vec::new();
                let fin = loop {
                    let Some(c) = self.byte(60) else {
                        return Some(Key::Esc);
                    };
                    if c.is_ascii_digit() || c == b';' {
                        params.push(c);
                    } else {
                        break c;
                    }
                };
                let p = core::str::from_utf8(&params).unwrap_or("");
                match (fin, p) {
                    (b'A', _) => Key::Up,
                    (b'B', _) => Key::Down,
                    (b'C', _) => Key::Right,
                    (b'D', _) => Key::Left,
                    (b'H', _) | (b'~', "1") | (b'~', "7") => Key::Home,
                    (b'F', _) | (b'~', "4") | (b'~', "8") => Key::End,
                    (b'Z', _) => Key::BackTab,
                    (b'~', "3") => Key::Delete,
                    (b'~', "5") => Key::PageUp,
                    (b'~', "6") => Key::PageDown,
                    _ => Key::Esc,
                }
            }
            1..=26 => Key::Ctrl((b'a' + b - 1) as char),
            _ => {
                // UTF-8 sequence.
                let len = match b {
                    0xC0..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF7 => 4,
                    _ => 1,
                };
                let mut bytes = vec![b];
                for _ in 1..len {
                    if let Some(c) = self.byte(60) {
                        bytes.push(c);
                    }
                }
                Key::Char(
                    String::from_utf8_lossy(&bytes)
                        .chars()
                        .next()
                        .unwrap_or('?'),
                )
            }
        })
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.leave();
    }
}

pub fn out(s: &str) {
    let _ = io::write_all(1, s.as_bytes());
}

/// Move the cursor (0-based row/column).
pub fn goto(row: usize, col: usize) -> String {
    format!("\x1b[{};{}H", row + 1, col + 1)
}

/// Edit one line at the bottom of the screen. Returns `None` when the
/// user cancels (Esc, Ctrl-C, Ctrl-G).
pub fn edit_line(
    t: &mut Term,
    row: usize,
    width: usize,
    prompt: &str,
    initial: &str,
    masked: bool,
) -> Option<String> {
    let mut buf: Vec<char> = initial.chars().collect();
    let mut cur = buf.len();
    let plen = layout::text_width(prompt);
    out("\x1b[?25h");
    let result = loop {
        let room = width.saturating_sub(plen + 1).max(1);
        let start = cur.saturating_sub(room.saturating_sub(1));
        let shown: String = buf[start..]
            .iter()
            .take(room)
            .map(|&c| if masked { '*' } else { c })
            .collect();
        out(&format!(
            "{}\x1b[0m\x1b[7m{}\x1b[0m {}\x1b[K{}",
            goto(row, 0),
            prompt,
            shown,
            goto(row, plen + 1 + cur - start)
        ));
        match t.key(-1) {
            None => break None,
            Some(Key::Enter) => break Some(buf.iter().collect()),
            Some(Key::Esc) | Some(Key::Ctrl('c')) | Some(Key::Ctrl('g')) => break None,
            Some(Key::Left) | Some(Key::Ctrl('b')) => cur = cur.saturating_sub(1),
            Some(Key::Right) | Some(Key::Ctrl('f')) => cur = (cur + 1).min(buf.len()),
            Some(Key::Home) | Some(Key::Ctrl('a')) => cur = 0,
            Some(Key::End) | Some(Key::Ctrl('e')) => cur = buf.len(),
            Some(Key::Backspace) => {
                if cur > 0 {
                    cur -= 1;
                    buf.remove(cur);
                }
            }
            Some(Key::Delete) | Some(Key::Ctrl('d')) => {
                if cur < buf.len() {
                    buf.remove(cur);
                }
            }
            Some(Key::Ctrl('k')) => buf.truncate(cur),
            Some(Key::Ctrl('u')) => {
                buf.drain(..cur);
                cur = 0;
            }
            Some(Key::Ctrl('w')) => {
                let mut s = cur;
                while s > 0 && buf[s - 1] == ' ' {
                    s -= 1;
                }
                while s > 0 && buf[s - 1] != ' ' {
                    s -= 1;
                }
                buf.drain(s..cur);
                cur = s;
            }
            Some(Key::Char(c)) => {
                buf.insert(cur, c);
                cur += 1;
            }
            _ => {}
        }
    };
    out("\x1b[?25l");
    result
}
