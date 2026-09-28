//! HTTP/1.1 client pieces for `wget` and the `browse` web browser.
//!
//! Everything protocol-related is pure and host-testable:
//! * [`Request`] serialisation and [`ResponseHead`] parsing,
//! * [`BodyDecoder`] for `Content-Length`, `chunked` and read-until-close
//!   bodies,
//! * [`encoding`]: gzip/deflate content codings and charset decoding,
//! * [`cookie`]: an RFC 6265 cookie jar with HTTP-date parsing,
//! * [`form`]/[`multipart`]: form submission bodies,
//! * [`client`]: a blocking client (redirects, cookies, keep-alive) over
//!   any byte stream supplied by a [`client::Connector`].

#![no_std]

extern crate alloc;

pub mod client;
pub mod cookie;
pub mod encoding;
pub mod multipart;

pub use weburl::form;
pub use weburl::{self, Url};

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Transport failure (message from the connector or stream).
    Io(String),
    /// Malformed response.
    Protocol(&'static str),
    Url(weburl::Error),
    TooManyRedirects,
    UnsupportedScheme(String),
    /// Response exceeded the configured size limit.
    TooLarge,
    Timeout,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(s) => write!(f, "{}", s),
            Error::Protocol(s) => write!(f, "bad HTTP response: {}", s),
            Error::Url(e) => write!(f, "bad URL: {}", e),
            Error::TooManyRedirects => f.write_str("too many redirects"),
            Error::UnsupportedScheme(s) => write!(f, "unsupported URL scheme '{}'", s),
            Error::TooLarge => f.write_str("response too large"),
            Error::Timeout => f.write_str("timed out"),
        }
    }
}

impl From<weburl::Error> for Error {
    fn from(e: weburl::Error) -> Error {
        Error::Url(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Ordered, case-insensitive header list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers(pub Vec<(String, String)>);

impl Headers {
    pub fn new() -> Headers {
        Headers(Vec::new())
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.0
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
    /// Replace any existing value.
    pub fn set(&mut self, name: &str, value: &str) {
        self.remove(name);
        self.0.push((name.to_string(), value.to_string()));
    }
    pub fn add(&mut self, name: &str, value: &str) {
        self.0.push((name.to_string(), value.to_string()));
    }
    pub fn remove(&mut self, name: &str) {
        self.0.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
    }
    /// True if a comma-separated header contains `token`.
    pub fn has_token(&self, name: &str, token: &str) -> bool {
        self.get_all(name)
            .flat_map(|v| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case(token))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: Url,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl Request {
    pub fn get(url: Url) -> Request {
        Request {
            method: String::from("GET"),
            url,
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    pub fn post(url: Url, content_type: &str, body: Vec<u8>) -> Request {
        let mut r = Request {
            method: String::from("POST"),
            url,
            headers: Headers::new(),
            body,
        };
        r.headers.set("Content-Type", content_type);
        r
    }

    /// The request as sent on the wire (origin-form target, `Host` and
    /// `Content-Length` added when missing).
    pub fn serialize(&self) -> Vec<u8> {
        let mut s = format!("{} {} HTTP/1.1\r\n", self.method, self.url.request_target());
        if !self.headers.contains("Host") {
            s.push_str(&format!("Host: {}\r\n", self.url.authority()));
        }
        for (k, v) in &self.headers.0 {
            s.push_str(k);
            s.push_str(": ");
            s.push_str(v);
            s.push_str("\r\n");
        }
        let needs_len = !self.body.is_empty()
            || !matches!(self.method.as_str(), "GET" | "HEAD" | "DELETE" | "OPTIONS");
        if needs_len && !self.headers.contains("Content-Length") {
            s.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }
        s.push_str("\r\n");
        let mut out = s.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHead {
    /// HTTP minor version (0 for HTTP/1.0, 1 for HTTP/1.1).
    pub minor: u8,
    pub status: u16,
    pub reason: String,
    pub headers: Headers,
}

impl ResponseHead {
    /// Parse a status line and headers from the start of `buf`. Returns
    /// `None` until the terminating blank line is present, else the head
    /// and the number of bytes it occupied. Tolerates bare `\n` line ends
    /// and folded (obsolete) continuation lines.
    pub fn parse(buf: &[u8]) -> Result<Option<(ResponseHead, usize)>> {
        let end = match find_head_end(buf) {
            Some(e) => e,
            None => {
                if buf.len() > 256 * 1024 {
                    return Err(Error::Protocol("header section too large"));
                }
                return Ok(None);
            }
        };
        let text = String::from_utf8_lossy(&buf[..end]);
        let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
        let status_line = lines.next().ok_or(Error::Protocol("empty response"))?;
        let rest = status_line
            .strip_prefix("HTTP/1.")
            .ok_or(Error::Protocol("not an HTTP/1.x response"))?;
        let minor = rest.as_bytes().first().map_or(1, |b| b.wrapping_sub(b'0'));
        let mut parts = rest.get(1..).unwrap_or("").trim_start().splitn(2, ' ');
        let status: u16 = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .ok_or(Error::Protocol("bad status code"))?;
        let reason = parts.next().unwrap_or("").trim().to_string();
        let mut headers = Headers::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if line.starts_with([' ', '\t']) {
                if let Some(last) = headers.0.last_mut() {
                    last.1.push(' ');
                    last.1.push_str(line.trim());
                }
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.add(k.trim(), v.trim());
            }
        }
        Ok(Some((
            ResponseHead {
                minor,
                status,
                reason,
                headers,
            },
            end,
        )))
    }

    /// Whether the connection may be reused after this response.
    pub fn keep_alive(&self) -> bool {
        if self.headers.has_token("Connection", "close") {
            return false;
        }
        self.minor >= 1 || self.headers.has_token("Connection", "keep-alive")
    }

    pub fn content_type(&self) -> (String, Option<String>) {
        parse_content_type(self.headers.get("Content-Type").unwrap_or(""))
    }
}

/// Offset just past the `\r\n\r\n` (or `\n\n`) ending a header block.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\n' {
            if buf.get(i + 1) == Some(&b'\n') {
                return Some(i + 2);
            }
            if buf.get(i + 1) == Some(&b'\r') && buf.get(i + 2) == Some(&b'\n') {
                return Some(i + 3);
            }
        }
        i += 1;
    }
    None
}

/// Split `text/html; charset=UTF-8` into (`text/html`, `Some("utf-8")`).
pub fn parse_content_type(v: &str) -> (String, Option<String>) {
    let mut it = v.split(';');
    let mime = it.next().unwrap_or("").trim().to_ascii_lowercase();
    let mut charset = None;
    for p in it {
        if let Some((k, val)) = p.split_once('=') {
            if k.trim().eq_ignore_ascii_case("charset") {
                charset = Some(val.trim().trim_matches(['"', '\'']).to_ascii_lowercase());
            }
        }
    }
    (mime, charset)
}

/// How a response body is delimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    None,
    Length(u64),
    Chunked,
    UntilClose,
}

/// Body framing for a response to `method` (RFC 9112 section 6.3).
pub fn framing(method: &str, head: &ResponseHead) -> Result<Framing> {
    if method == "HEAD" || head.status / 100 == 1 || head.status == 204 || head.status == 304 {
        return Ok(Framing::None);
    }
    if let Some(te) = head.headers.get("Transfer-Encoding") {
        if te
            .split(',')
            .last()
            .is_some_and(|t| t.trim().eq_ignore_ascii_case("chunked"))
        {
            return Ok(Framing::Chunked);
        }
        return Ok(Framing::UntilClose);
    }
    if let Some(cl) = head.headers.get("Content-Length") {
        // Duplicate identical values ("5, 5") are allowed.
        let first = cl.split(',').next().unwrap_or("").trim();
        let n: u64 = first
            .parse()
            .map_err(|_| Error::Protocol("bad Content-Length"))?;
        return Ok(Framing::Length(n));
    }
    Ok(Framing::UntilClose)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chunk {
    Size,
    Data(u64),
    DataEnd,
    Trailer,
    Done,
}

/// Incremental body decoder: feed raw bytes, get body bytes.
#[derive(Debug, Clone)]
pub struct BodyDecoder {
    framing: Framing,
    remaining: u64,
    chunk: Chunk,
    line: Vec<u8>,
    done: bool,
}

impl BodyDecoder {
    pub fn new(framing: Framing) -> BodyDecoder {
        BodyDecoder {
            framing,
            remaining: match framing {
                Framing::Length(n) => n,
                _ => 0,
            },
            chunk: Chunk::Size,
            line: Vec::new(),
            done: matches!(framing, Framing::None | Framing::Length(0)),
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Consume `input`, appending body bytes to `out`. Returns how many
    /// input bytes were used (the rest belongs to the next response).
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<usize> {
        match self.framing {
            Framing::None => {
                self.done = true;
                Ok(0)
            }
            Framing::UntilClose => {
                out.extend_from_slice(input);
                Ok(input.len())
            }
            Framing::Length(_) => {
                let n = (input.len() as u64).min(self.remaining) as usize;
                out.extend_from_slice(&input[..n]);
                self.remaining -= n as u64;
                if self.remaining == 0 {
                    self.done = true;
                }
                Ok(n)
            }
            Framing::Chunked => self.feed_chunked(input, out),
        }
    }

    /// The peer closed the connection: fine for read-until-close bodies.
    pub fn finish(&mut self) -> Result<()> {
        match self.framing {
            Framing::UntilClose | Framing::None => {
                self.done = true;
                Ok(())
            }
            _ if self.done => Ok(()),
            _ => Err(Error::Protocol(
                "connection closed before the end of the body",
            )),
        }
    }

    fn feed_chunked(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<usize> {
        let mut i = 0;
        while i < input.len() && !self.done {
            match self.chunk {
                Chunk::Size | Chunk::DataEnd | Chunk::Trailer => {
                    let b = input[i];
                    i += 1;
                    if b != b'\n' {
                        if self.line.len() > 4096 {
                            return Err(Error::Protocol("chunk header too long"));
                        }
                        self.line.push(b);
                        continue;
                    }
                    let line = core::mem::take(&mut self.line);
                    let line = line.strip_suffix(b"\r").unwrap_or(&line);
                    match self.chunk {
                        Chunk::Size => {
                            let hex = line.split(|&c| c == b';').next().unwrap_or(b"");
                            let hex = core::str::from_utf8(hex).unwrap_or("").trim();
                            let n = u64::from_str_radix(hex, 16)
                                .map_err(|_| Error::Protocol("bad chunk size"))?;
                            self.chunk = if n == 0 {
                                Chunk::Trailer
                            } else {
                                Chunk::Data(n)
                            };
                        }
                        Chunk::DataEnd => {
                            if !line.is_empty() {
                                return Err(Error::Protocol("missing CRLF after chunk"));
                            }
                            self.chunk = Chunk::Size;
                        }
                        _ => {
                            if line.is_empty() {
                                self.chunk = Chunk::Done;
                                self.done = true;
                            }
                        }
                    }
                }
                Chunk::Data(n) => {
                    let take = ((input.len() - i) as u64).min(n) as usize;
                    out.extend_from_slice(&input[i..i + take]);
                    i += take;
                    let left = n - take as u64;
                    self.chunk = if left == 0 {
                        Chunk::DataEnd
                    } else {
                        Chunk::Data(left)
                    };
                }
                Chunk::Done => break,
            }
        }
        Ok(i)
    }
}

/// Where a redirect response leads, and whether the method becomes GET.
pub fn redirect_target(method: &str, head: &ResponseHead, base: &Url) -> Option<(Url, bool)> {
    let to_get = match head.status {
        301 | 302 => method == "POST",
        303 => method != "HEAD",
        307 | 308 => false,
        _ => return None,
    };
    let loc = head.headers.get("Location")?;
    let mut url = base.join(loc).ok()?;
    // A fragment-less Location inherits the original fragment.
    if url.fragment.is_none() {
        url.fragment = base.fragment.clone();
    }
    Some((url, to_get))
}

#[cfg(test)]
mod tests;
