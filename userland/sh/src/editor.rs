//! Interactive line editor: cursor movement, history and tab completion.

use rustos_rt::prelude::*;
use rustos_rt::{fs, io, term};

pub enum Line {
    Text(String),
    Eof,
    Interrupted,
}

fn out(s: &str) {
    let _ = io::write_all(1, s.as_bytes());
}

fn read_byte() -> Option<u8> {
    let mut b = [0u8; 1];
    loop {
        match io::read(0, &mut b) {
            Ok(1) => return Some(b[0]),
            Ok(_) => return None,
            Err(rustos_rt::Error(4)) => continue,
            Err(_) => return None,
        }
    }
}

pub fn read_line(prompt: &str, history: &[String], complete: &dyn Fn(&str) -> Vec<String>) -> Line {
    io::flush();
    let Some(orig) = term::raw_mode(0) else {
        // Not a terminal: plain line reading.
        out(prompt);
        return match io::read_line() {
            Some(l) => Line::Text(l),
            None => Line::Eof,
        };
    };
    let result = edit(prompt, history, complete);
    term::set(0, &orig);
    result
}

fn edit(prompt: &str, history: &[String], complete: &dyn Fn(&str) -> Vec<String>) -> Line {
    let mut buf: Vec<char> = Vec::new();
    let mut cur = 0usize;
    let mut hist_idx = history.len();
    let mut saved_line: Vec<char> = Vec::new();
    let (_, cols) = term::size(1);
    let prompt_len = visible_len(prompt);
    out(prompt);
    let redraw = |buf: &[char], cur: usize| {
        let s: String = buf.iter().collect();
        out("\r");
        out(prompt);
        out(&s);
        out("\x1b[K");
        let back = buf.len() - cur;
        if back > 0 {
            out(&format!("\x1b[{}D", back));
        }
    };
    let _ = cols;
    let _ = prompt_len;
    loop {
        let Some(b) = read_byte() else {
            if buf.is_empty() {
                return Line::Eof;
            }
            out("\r\n");
            return Line::Text(buf.iter().collect());
        };
        match b {
            b'\r' | b'\n' => {
                out("\r\n");
                return Line::Text(buf.iter().collect());
            }
            0x03 => {
                out("^C\r\n");
                return Line::Interrupted;
            }
            0x04 => {
                if buf.is_empty() {
                    out("\r\n");
                    return Line::Eof;
                }
                if cur < buf.len() {
                    buf.remove(cur);
                    redraw(&buf, cur);
                }
            }
            0x7f | 0x08 => {
                if cur > 0 {
                    cur -= 1;
                    buf.remove(cur);
                    redraw(&buf, cur);
                }
            }
            0x01 => {
                cur = 0;
                redraw(&buf, cur);
            }
            0x05 => {
                cur = buf.len();
                redraw(&buf, cur);
            }
            0x0b => {
                buf.truncate(cur);
                redraw(&buf, cur);
            }
            0x15 => {
                buf.drain(..cur);
                cur = 0;
                redraw(&buf, cur);
            }
            0x17 => {
                let mut i = cur;
                while i > 0 && buf[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && buf[i - 1] != ' ' {
                    i -= 1;
                }
                buf.drain(i..cur);
                cur = i;
                redraw(&buf, cur);
            }
            0x0c => {
                out("\x1b[H\x1b[2J");
                redraw(&buf, cur);
            }
            b'\t' => {
                let line: String = buf[..cur].iter().collect();
                let start = line
                    .rfind([' ', '\t', '|', ';', '&', '<', '>'])
                    .map_or(0, |i| i + 1);
                let word = &line[start..];
                let first_word = line[..start].trim().is_empty()
                    || line[..start].trim_end().ends_with(['|', ';', '&']);
                let mut cands = if first_word && !word.contains('/') {
                    complete(word)
                } else {
                    complete_path(word)
                };
                cands.sort();
                cands.dedup();
                if cands.len() == 1 {
                    let c = &cands[0];
                    let add: Vec<char> = c[word.len()..].chars().collect();
                    let n = add.len();
                    for (k, ch) in add.into_iter().enumerate() {
                        buf.insert(cur + k, ch);
                    }
                    cur += n;
                    if !c.ends_with('/') {
                        buf.insert(cur, ' ');
                        cur += 1;
                    }
                    redraw(&buf, cur);
                } else if cands.len() > 1 {
                    let common = common_prefix(&cands);
                    if common.len() > word.len() {
                        let add: Vec<char> = common[word.len()..].chars().collect();
                        let n = add.len();
                        for (k, ch) in add.into_iter().enumerate() {
                            buf.insert(cur + k, ch);
                        }
                        cur += n;
                        redraw(&buf, cur);
                    } else {
                        out("\r\n");
                        let names: Vec<&str> = cands
                            .iter()
                            .map(|c| fs::basename(c.trim_end_matches('/')))
                            .collect();
                        let width = names.iter().map(|n| n.len()).max().unwrap_or(0) + 2;
                        let per_line = (80 / width).max(1);
                        for (k, n) in names.iter().enumerate() {
                            out(&format!("{:<w$}", n, w = width));
                            if (k + 1) % per_line == 0 {
                                out("\r\n");
                            }
                        }
                        if names.len() % per_line != 0 {
                            out("\r\n");
                        }
                        redraw(&buf, cur);
                    }
                }
            }
            0x1b => {
                let Some(b1) = read_byte() else { continue };
                if b1 != b'[' && b1 != b'O' {
                    continue;
                }
                let Some(b2) = read_byte() else { continue };
                match b2 {
                    b'A' => {
                        if hist_idx > 0 {
                            if hist_idx == history.len() {
                                saved_line = buf.clone();
                            }
                            hist_idx -= 1;
                            buf = history[hist_idx].chars().collect();
                            cur = buf.len();
                            redraw(&buf, cur);
                        }
                    }
                    b'B' => {
                        if hist_idx < history.len() {
                            hist_idx += 1;
                            buf = if hist_idx == history.len() {
                                saved_line.clone()
                            } else {
                                history[hist_idx].chars().collect()
                            };
                            cur = buf.len();
                            redraw(&buf, cur);
                        }
                    }
                    b'C' => {
                        if cur < buf.len() {
                            cur += 1;
                            out("\x1b[C");
                        }
                    }
                    b'D' => {
                        if cur > 0 {
                            cur -= 1;
                            out("\x1b[D");
                        }
                    }
                    b'H' => {
                        cur = 0;
                        redraw(&buf, cur);
                    }
                    b'F' => {
                        cur = buf.len();
                        redraw(&buf, cur);
                    }
                    b'1'..=b'8' => {
                        let Some(t) = read_byte() else { continue };
                        if t == b'~' {
                            match b2 {
                                b'3' => {
                                    if cur < buf.len() {
                                        buf.remove(cur);
                                        redraw(&buf, cur);
                                    }
                                }
                                b'1' | b'7' => {
                                    cur = 0;
                                    redraw(&buf, cur);
                                }
                                b'4' | b'8' => {
                                    cur = buf.len();
                                    redraw(&buf, cur);
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            c if c >= 0x20 => {
                // Collect UTF-8 continuation bytes.
                let mut bytes = alloc::vec![c];
                let extra = match c {
                    0xC0..=0xDF => 1,
                    0xE0..=0xEF => 2,
                    0xF0..=0xF7 => 3,
                    _ => 0,
                };
                for _ in 0..extra {
                    if let Some(n) = read_byte() {
                        bytes.push(n);
                    }
                }
                for ch in String::from_utf8_lossy(&bytes).chars() {
                    buf.insert(cur, ch);
                    cur += 1;
                }
                if cur == buf.len() {
                    out(core::str::from_utf8(&bytes).unwrap_or("?"));
                } else {
                    redraw(&buf, cur);
                }
            }
            _ => {}
        }
    }
}

fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            if c.is_ascii_alphabetic() {
                esc = false;
            }
        } else if c == '\x1b' {
            esc = true;
        } else {
            n += 1;
        }
    }
    n
}

fn common_prefix(v: &[String]) -> String {
    let mut p = v[0].clone();
    for s in &v[1..] {
        while !s.starts_with(&p) {
            p.pop();
        }
    }
    p
}

pub fn complete_path(word: &str) -> Vec<String> {
    let (dir, prefix) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let list_dir = if dir.is_empty() { "." } else { dir };
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(list_dir) {
        for e in entries {
            if e.name.starts_with(prefix) && (!e.name.starts_with('.') || prefix.starts_with('.')) {
                let mut c = format!("{}{}", dir, e.name);
                if fs::is_dir(&c) {
                    c.push('/');
                }
                out.push(c);
            }
        }
    }
    out
}
