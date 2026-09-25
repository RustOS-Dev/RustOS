//! `test` / `[` expression evaluation.

use rustos_rt::fs;
use rustos_rt::prelude::*;

pub fn test(args: &[&str]) -> i32 {
    let mut a: Vec<&str> = args[1..].to_vec();
    if args[0] == "[" {
        if a.last() != Some(&"]") {
            eprintln!("[: missing `]'");
            return 2;
        }
        a.pop();
    }
    match eval(&a) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("test: {}", e);
            2
        }
    }
}

fn eval(a: &[&str]) -> Result<bool, String> {
    let mut p = P { a, i: 0 };
    let r = p.or()?;
    if p.i != a.len() {
        return Err(format!("{}: unexpected argument", a[p.i]));
    }
    Ok(r)
}

struct P<'a> {
    a: &'a [&'a str],
    i: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<&str> {
        self.a.get(self.i).copied()
    }
    fn or(&mut self) -> Result<bool, String> {
        let mut v = self.and()?;
        while self.peek() == Some("-o") {
            self.i += 1;
            v = self.and()? || v;
        }
        Ok(v)
    }
    fn and(&mut self) -> Result<bool, String> {
        let mut v = self.not()?;
        while self.peek() == Some("-a") {
            self.i += 1;
            v = self.not()? && v;
        }
        Ok(v)
    }
    fn not(&mut self) -> Result<bool, String> {
        if self.peek() == Some("!") && self.a.len() - self.i > 1 {
            self.i += 1;
            return Ok(!self.not()?);
        }
        self.primary()
    }
    fn primary(&mut self) -> Result<bool, String> {
        let rest = self.a.len() - self.i;
        if rest == 0 {
            return Ok(false);
        }
        if self.peek() == Some("(") {
            self.i += 1;
            let v = self.or()?;
            if self.peek() != Some(")") {
                return Err(String::from("missing `)'"));
            }
            self.i += 1;
            return Ok(v);
        }
        // Binary operators.
        if rest >= 3 {
            let (l, op, r) = (self.a[self.i], self.a[self.i + 1], self.a[self.i + 2]);
            let res = match op {
                "=" | "==" => Some(l == r),
                "!=" => Some(l != r),
                "<" => Some(l < r),
                ">" => Some(l > r),
                "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                    let x: i64 = l
                        .trim()
                        .parse()
                        .map_err(|_| format!("{}: integer expression expected", l))?;
                    let y: i64 = r
                        .trim()
                        .parse()
                        .map_err(|_| format!("{}: integer expression expected", r))?;
                    Some(match op {
                        "-eq" => x == y,
                        "-ne" => x != y,
                        "-lt" => x < y,
                        "-le" => x <= y,
                        "-gt" => x > y,
                        _ => x >= y,
                    })
                }
                "-nt" | "-ot" => {
                    let ml = fs::metadata(l).map(|m| m.mtime).unwrap_or(i64::MIN);
                    let mr = fs::metadata(r).map(|m| m.mtime).unwrap_or(i64::MIN);
                    Some(if op == "-nt" { ml > mr } else { ml < mr })
                }
                "-ef" => {
                    let (a, b) = (fs::metadata(l), fs::metadata(r));
                    Some(matches!((a, b), (Ok(a), Ok(b)) if a.ino == b.ino && a.dev == b.dev))
                }
                _ => None,
            };
            if let Some(v) = res {
                self.i += 3;
                return Ok(v);
            }
        }
        // Unary operators.
        if rest >= 2 {
            let (op, arg) = (self.a[self.i], self.a[self.i + 1]);
            let res = match op {
                "-n" => Some(!arg.is_empty()),
                "-z" => Some(arg.is_empty()),
                "-e" => Some(fs::exists(arg)),
                "-f" => Some(fs::metadata(arg).is_ok_and(|m| m.is_file())),
                "-d" => Some(fs::metadata(arg).is_ok_and(|m| m.is_dir())),
                "-h" | "-L" => Some(fs::symlink_metadata(arg).is_ok_and(|m| m.is_symlink())),
                "-s" => Some(fs::metadata(arg).is_ok_and(|m| m.size > 0)),
                "-r" | "-w" => Some(fs::exists(arg)),
                "-x" => Some(fs::metadata(arg).is_ok_and(|m| m.mode & 0o111 != 0)),
                "-c" => Some(fs::metadata(arg).is_ok_and(|m| m.mode & fs::S_IFMT == fs::S_IFCHR)),
                "-b" => Some(fs::metadata(arg).is_ok_and(|m| m.mode & fs::S_IFMT == fs::S_IFBLK)),
                "-p" => Some(fs::metadata(arg).is_ok_and(|m| m.mode & fs::S_IFMT == fs::S_IFIFO)),
                "-S" => Some(fs::metadata(arg).is_ok_and(|m| m.mode & fs::S_IFMT == fs::S_IFSOCK)),
                "-t" => Some(rustos_rt::io::isatty(arg.parse().unwrap_or(-1))),
                _ => None,
            };
            if let Some(v) = res {
                self.i += 2;
                return Ok(v);
            }
        }
        let s = self.a[self.i];
        self.i += 1;
        Ok(!s.is_empty())
    }
}
