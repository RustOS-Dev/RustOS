//! A small JSON value type with a parser and serializer (RFC 8259).

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Json {
    pub fn get(&self, k: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.get(k),
            _ => None,
        }
    }
    pub fn str(&self, k: &str) -> Option<&str> {
        match self.get(k)? {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn num(&self, k: &str) -> Option<f64> {
        match self.get(k)? {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    pub fn int(&self, k: &str) -> Option<i64> {
        self.num(k).map(|n| n as i64)
    }
    pub fn bool(&self, k: &str) -> Option<bool> {
        match self.get(k)? {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn arr(&self, k: &str) -> Option<&[Json]> {
        match self.get(k)? {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Json::Str(s) = self { Some(s) } else { None }
    }
    pub fn as_i64(&self) -> Option<i64> {
        if let Json::Num(n) = self { Some(*n as i64) } else { None }
    }
    pub fn as_arr(&self) -> Option<&[Json]> {
        if let Json::Arr(a) = self { Some(a) } else { None }
    }

    /// Builder: an object from key/value pairs.
    pub fn obj<const N: usize>(pairs: [(&str, Json); N]) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::Str(s.to_string())
    }
}
impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::Str(s)
    }
}
impl From<i64> for Json {
    fn from(n: i64) -> Json {
        Json::Num(n as f64)
    }
}
impl From<usize> for Json {
    fn from(n: usize) -> Json {
        Json::Num(n as f64)
    }
}
impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}
impl From<f64> for Json {
    fn from(n: f64) -> Json {
        Json::Num(n)
    }
}

pub fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl Json {
    /// Serialize without whitespace (never contains a raw newline, so a
    /// message fits on one line).
    pub fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => {
                if n.is_finite() {
                    if *n == (*n as i64) as f64 && n.abs() < 1e15 {
                        let _ = write!(out, "{}", *n as i64);
                    } else {
                        let _ = write!(out, "{}", n);
                    }
                } else {
                    out.push_str("null");
                }
            }
            Json::Str(s) => write_str(out, s),
            Json::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_json(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    pub fn parse(s: &str) -> Result<Json, String> {
        let mut p = P { b: s.as_bytes(), i: 0, depth: 0 };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format_err("trailing characters", p.i));
        }
        Ok(v)
    }
}

fn format_err(what: &str, at: usize) -> String {
    let mut s = String::from(what);
    let _ = write!(s, " at byte {}", at);
    s
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn lit(&mut self, s: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            Ok(v)
        } else {
            Err(format_err("bad literal", self.i))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.depth += 1;
        if self.depth > 256 {
            return Err(String::from("nesting too deep"));
        }
        let r = match self.b.get(self.i) {
            None => Err(String::from("unexpected end")),
            Some(b'n') => self.lit("null", Json::Null),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        a.push(self.value()?);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(format_err("expected , or ]", self.i)),
                        }
                    }
                }
                Ok(Json::Arr(a))
            }
            Some(b'{') => {
                self.i += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        if self.b.get(self.i) != Some(&b'"') {
                            return Err(format_err("expected key", self.i));
                        }
                        let k = self.string()?;
                        self.ws();
                        if self.b.get(self.i) != Some(&b':') {
                            return Err(format_err("expected :", self.i));
                        }
                        self.i += 1;
                        self.ws();
                        let v = self.value()?;
                        m.insert(k, v);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(format_err("expected , or }", self.i)),
                        }
                    }
                }
                Ok(Json::Obj(m))
            }
            Some(_) => self.number(),
        };
        self.depth -= 1;
        r
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let s = self.b.get(self.i..self.i + 4).ok_or_else(|| String::from("short \\u escape"))?;
        let s = core::str::from_utf8(s).map_err(|_| String::from("bad \\u escape"))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| format_err("bad \\u escape", self.i))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.b.len() && self.b[self.i] != b'"' && self.b[self.i] != b'\\' {
                self.i += 1;
            }
            out.push_str(core::str::from_utf8(&self.b[start..self.i]).map_err(|_| String::from("invalid UTF-8"))?);
            match self.b.get(self.i) {
                None => return Err(String::from("unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => {
                    self.i += 1;
                    let e = *self.b.get(self.i).ok_or_else(|| String::from("bad escape"))?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut c = self.hex4()?;
                            if (0xD800..0xDC00).contains(&c) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    c = 0x10000 + ((c - 0xD800) << 10) + (lo - 0xDC00);
                                }
                            }
                            out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err(format_err("bad escape", self.i)),
                    }
                }
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            self.i += 1;
        }
        let s = core::str::from_utf8(&self.b[start..self.i]).unwrap_or("");
        s.parse::<f64>().map(Json::Num).map_err(|_| format_err("bad number", start))
    }
}
