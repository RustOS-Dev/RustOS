//! URL parsing and resolution for the web tools (`wget`, `browse`).
//!
//! Follows RFC 3986 (components, relative resolution, dot-segment removal)
//! with the leniency browsers apply to real-world pages (WHATWG URL):
//! surrounding whitespace is trimmed, tabs/newlines are dropped, `\` counts
//! as `/` in http(s)/file URLs, stray characters such as spaces or non-ASCII
//! text are percent-encoded, host names are lower-cased and non-ASCII host
//! labels are converted to punycode (`xn--`). Default ports are elided.
//!
//! Also provides percent-encoding and `application/x-www-form-urlencoded`
//! helpers. Pure `no_std` + `alloc`; tested on the host.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No scheme and no base URL to resolve against.
    RelativeWithoutBase,
    InvalidScheme,
    InvalidPort,
    InvalidHost,
    /// A hierarchical URL (http, https, ...) without a host.
    MissingHost,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Error::RelativeWithoutBase => "relative URL without a base",
            Error::InvalidScheme => "invalid scheme",
            Error::InvalidPort => "invalid port",
            Error::InvalidHost => "invalid host",
            Error::MissingHost => "missing host",
        })
    }
}

/// A parsed absolute URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// Lower-case scheme without the colon.
    pub scheme: String,
    /// `user[:password]` (percent-encoded), empty if absent.
    pub userinfo: String,
    /// Host (lower case; IPv6 without brackets), `None` without authority.
    pub host: Option<String>,
    /// Explicit port, `None` when absent or equal to the default.
    pub port: Option<u16>,
    /// Path (percent-encoded). For opaque URLs (`mailto:`, `data:`)
    /// everything after the scheme up to `?`/`#`.
    pub path: String,
    /// Query without `?` (percent-encoded).
    pub query: Option<String>,
    /// Fragment without `#`.
    pub fragment: Option<String>,
}

/// Default port for a scheme.
pub fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        "gopher" => Some(70),
        _ => None,
    }
}

/// Schemes with authority and `/` paths and the browser leniencies.
pub fn is_special(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file")
}

fn valid_scheme(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphabetic()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
}

/// Remove leading/trailing C0 controls and spaces, and all tabs/newlines.
fn clean(input: &str) -> String {
    input
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .filter(|&c| c != '\t' && c != '\n' && c != '\r')
        .collect()
}

/// Split off `scheme:` if the input starts with a valid scheme.
fn split_scheme(s: &str) -> Option<(String, &str)> {
    let i = s.find(':')?;
    let sch = &s[..i];
    // A single letter followed by ':' and '\' or '/' is a Windows drive,
    // not a scheme; treat such input as relative.
    if valid_scheme(sch) {
        Some((sch.to_ascii_lowercase(), &s[i + 1..]))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Percent encoding
// ---------------------------------------------------------------------------

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Percent-encode every byte of `s` for which `keep` is false (bytes >= 0x80
/// are always encoded). Existing `%XX` escapes are preserved when
/// `keep(b'%')` is true.
pub fn percent_encode(s: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b < 0x80 && b > 0x20 && b != 0x7F && keep(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 15) as usize] as char);
        }
    }
    out
}

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode `%XX` escapes (invalid escapes are kept literally).
pub fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hexval(b[i + 1]), hexval(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Decode `%XX` escapes into a string (invalid UTF-8 replaced).
pub fn percent_decode_str(s: &str) -> String {
    String::from_utf8_lossy(&percent_decode(s)).into_owned()
}

fn path_keep(b: u8) -> bool {
    !matches!(
        b,
        b' ' | b'"' | b'<' | b'>' | b'`' | b'#' | b'?' | b'{' | b'}'
    )
}
fn query_keep_special(b: u8) -> bool {
    !matches!(b, b' ' | b'"' | b'<' | b'>' | b'#' | b'\'')
}
fn query_keep(b: u8) -> bool {
    !matches!(b, b' ' | b'"' | b'<' | b'>' | b'#')
}
fn fragment_keep(b: u8) -> bool {
    !matches!(b, b' ' | b'"' | b'<' | b'>' | b'`')
}
fn userinfo_keep(b: u8) -> bool {
    !matches!(
        b,
        b' ' | b'"'
            | b'<'
            | b'>'
            | b'`'
            | b'#'
            | b'?'
            | b'{'
            | b'}'
            | b'/'
            | b';'
            | b'='
            | b'@'
            | b'['
            | b'\\'
            | b']'
            | b'^'
            | b'|'
    )
}

/// `application/x-www-form-urlencoded` serialisation and parsing.
pub mod form {
    use super::*;

    fn keep(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'*' | b'-' | b'.' | b'_')
    }

    /// Encode one name or value (space becomes `+`).
    pub fn encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for &b in s.as_bytes() {
            if b == b' ' {
                out.push('+');
            } else if keep(b) {
                out.push(b as char);
            } else {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 15) as usize] as char);
            }
        }
        out
    }

    /// `a=1&b=2` from name/value pairs.
    pub fn serialize<'a, I>(pairs: I) -> String
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut out = String::new();
        for (k, v) in pairs {
            if !out.is_empty() {
                out.push('&');
            }
            out.push_str(&encode(k));
            out.push('=');
            out.push_str(&encode(v));
        }
        out
    }

    /// Decode one component (`+` is a space).
    pub fn decode(s: &str) -> String {
        let s: String = s.chars().map(|c| if c == '+' { ' ' } else { c }).collect();
        percent_decode_str(&s)
    }

    /// Parse `a=1&b=2` into pairs.
    pub fn parse(q: &str) -> Vec<(String, String)> {
        q.split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (decode(k), decode(v))
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Hosts
// ---------------------------------------------------------------------------

/// RFC 3492 punycode encoding of one label (without the `xn--` prefix).
pub fn punycode_encode(label: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    fn adapt(mut delta: u32, num: u32, first: bool) -> u32 {
        delta = if first { delta / DAMP } else { delta / 2 };
        delta += delta / num;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) / 2 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + (BASE - TMIN + 1) * delta / (delta + SKEW)
    }
    fn digit(d: u32) -> char {
        (if d < 26 {
            b'a' + d as u8
        } else {
            b'0' + (d - 26) as u8
        }) as char
    }
    let input: Vec<u32> = label.chars().map(|c| c as u32).collect();
    let mut out: String = label.chars().filter(|c| c.is_ascii()).collect();
    let basic = out.len() as u32;
    let mut h = basic;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias) = (128u32, 0u32, 72u32);
    while (h as usize) < input.len() {
        let m = *input.iter().filter(|&&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(h + 1)?)?;
        n = m;
        for &c in &input {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                bias = adapt(delta, h + 1, h == basic);
                delta = 0;
                h += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    Some(out)
}

/// Normalise a host: lower case, percent-decoded, IDNA to punycode.
fn parse_host(raw: &str, special: bool) -> Result<String, Error> {
    if let Some(inner) = raw.strip_prefix('[') {
        let v6 = inner.strip_suffix(']').ok_or(Error::InvalidHost)?;
        if v6.is_empty()
            || !v6
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
        {
            return Err(Error::InvalidHost);
        }
        return Ok(v6.to_ascii_lowercase());
    }
    if !special {
        return Ok(percent_encode(raw, |b| b != b' '));
    }
    let decoded = percent_decode_str(raw);
    if decoded.is_empty() {
        return Ok(decoded);
    }
    let mut labels = Vec::new();
    for label in decoded.split(['.', '\u{3002}']) {
        let lower = label.to_lowercase();
        if lower.is_ascii() {
            labels.push(lower);
        } else {
            labels.push(format!(
                "xn--{}",
                punycode_encode(&lower).ok_or(Error::InvalidHost)?
            ));
        }
    }
    let host = labels.join(".");
    if host.bytes().any(|b| {
        matches!(
            b,
            0..=0x20
                | b'#'
                | b'%'
                | b'/'
                | b':'
                | b'<'
                | b'>'
                | b'?'
                | b'@'
                | b'['
                | b'\\'
                | b']'
                | b'^'
                | b'|'
        )
    }) {
        return Err(Error::InvalidHost);
    }
    Ok(host)
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// RFC 3986 section 5.2.4 (also treats `%2e` as `.`).
pub fn remove_dot_segments(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    let segs: Vec<&str> = path.split('/').collect();
    let n = segs.len();
    for (i, seg) in segs.iter().enumerate() {
        if i == 0 && absolute {
            continue;
        }
        let last = i + 1 == n;
        let dot = |s: &str| s == "." || s.eq_ignore_ascii_case("%2e");
        let dotdot = |s: &str| {
            s == ".."
                || s.eq_ignore_ascii_case(".%2e")
                || s.eq_ignore_ascii_case("%2e.")
                || s.eq_ignore_ascii_case("%2e%2e")
        };
        if dot(seg) {
            if last {
                out.push("");
            }
        } else if dotdot(seg) {
            out.pop();
            if last {
                out.push("");
            }
        } else {
            out.push(seg);
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{}", joined)
    } else {
        joined
    }
}

// ---------------------------------------------------------------------------
// Components of a (possibly relative) reference
// ---------------------------------------------------------------------------

struct Parts<'a> {
    authority: Option<&'a str>,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

fn split_parts(s: &str, special: bool) -> Parts<'_> {
    let (s, fragment) = match s.find('#') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let (s, query) = match s.find('?') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    // The scheme (if any) is handled by the callers.
    let rest = split_scheme(s).map_or(s, |(_, rest)| rest);
    let slash = |b: u8| b == b'/' || (special && b == b'\\');
    let rb = rest.as_bytes();
    if rb.len() >= 2 && slash(rb[0]) && slash(rb[1]) {
        let a = &rest[2..];
        let end = a.bytes().position(slash).unwrap_or(a.len());
        Parts {
            authority: Some(&a[..end]),
            path: &a[end..],
            query,
            fragment,
        }
    } else {
        Parts {
            authority: None,
            path: rest,
            query,
            fragment,
        }
    }
}

impl Url {
    /// Parse an absolute URL.
    pub fn parse(input: &str) -> Result<Url, Error> {
        let s = clean(input);
        let (scheme, _) = split_scheme(&s).ok_or(Error::RelativeWithoutBase)?;
        let special = is_special(&scheme);
        let s = if special { s.replace('\\', "/") } else { s };
        let mut p = split_parts(&s, special);
        // "http:example.com" / "http:/example.com": special schemes always
        // have an authority; take it after any number of slashes.
        if special && scheme != "file" && p.authority.is_none() {
            let rest = p.path.trim_start_matches('/');
            let end = rest.find('/').unwrap_or(rest.len());
            p.authority = Some(&rest[..end]);
            p.path = &rest[end..];
        }
        Url::from_parts(scheme, p)
    }

    fn from_parts(scheme: String, p: Parts) -> Result<Url, Error> {
        let special = is_special(&scheme);
        let mut url = Url {
            scheme,
            userinfo: String::new(),
            host: None,
            port: None,
            path: String::new(),
            query: None,
            fragment: None,
        };
        if let Some(auth) = p.authority {
            let (ui, hp) = match auth.rfind('@') {
                Some(i) => (&auth[..i], &auth[i + 1..]),
                None => ("", auth),
            };
            url.userinfo = percent_encode(ui, |b| userinfo_keep(b) || b == b':' || b == b'%');
            // Port: after the last ':' unless inside an IPv6 literal.
            let (h, port) = match hp.rfind(':') {
                Some(i) if !hp[i..].contains(']') => (&hp[..i], Some(&hp[i + 1..])),
                _ => (hp, None),
            };
            let host = parse_host(h, special)?;
            if special && url.scheme != "file" && host.is_empty() {
                return Err(Error::MissingHost);
            }
            url.host = Some(host);
            if let Some(ps) = port {
                if !ps.is_empty() {
                    let n: u32 = ps.parse().map_err(|_| Error::InvalidPort)?;
                    if n > 65535 {
                        return Err(Error::InvalidPort);
                    }
                    if default_port(&url.scheme) != Some(n as u16) {
                        url.port = Some(n as u16);
                    }
                }
            }
        } else if special && url.scheme != "file" {
            return Err(Error::MissingHost);
        }
        if url.host.is_some() || special {
            let path = percent_encode(p.path, |b| path_keep(b) || b == b'%');
            url.path = remove_dot_segments(&path);
            if url.path.is_empty() && special {
                url.path.push('/');
            }
        } else {
            // Opaque (mailto:, data:, javascript:) or rootless path.
            url.path = percent_encode(p.path, |b| b != b'#' && b != b'"');
        }
        url.query = p.query.map(|q| {
            percent_encode(q, |b| {
                (if special {
                    query_keep_special(b)
                } else {
                    query_keep(b)
                }) || b == b'%'
            })
        });
        url.fragment = p
            .fragment
            .map(|f| percent_encode(f, |b| fragment_keep(b) || b == b'%'));
        Ok(url)
    }

    /// Resolve `reference` against this URL (RFC 3986 section 5.2.2).
    pub fn join(&self, reference: &str) -> Result<Url, Error> {
        let r = clean(reference);
        let special = is_special(&self.scheme);
        let r = if special { r.replace('\\', "/") } else { r };
        // A reference with a scheme is absolute (browsers treat "http:foo"
        // with the base's scheme as relative; follow that).
        if let Some((sch, rest)) = split_scheme(&r) {
            if !(sch == self.scheme && special && !rest.starts_with('/')) {
                return Url::parse(&r);
            }
            let rel = rest.to_string();
            return self.join(&rel);
        }
        if self.host.is_none() && !special && !self.path.starts_with('/') {
            // Opaque base: only fragment-only references resolve.
            if let Some(f) = r.strip_prefix('#') {
                let mut u = self.clone();
                u.fragment = Some(percent_encode(f, |b| fragment_keep(b) || b == b'%'));
                return Ok(u);
            }
            return Err(Error::RelativeWithoutBase);
        }
        let p = split_parts(&r, special);
        if p.authority.is_some() {
            return Url::from_parts(self.scheme.clone(), p);
        }
        let mut u = self.clone();
        u.fragment = p
            .fragment
            .map(|f| percent_encode(f, |b| fragment_keep(b) || b == b'%'));
        let qk = |b: u8| {
            (if special {
                query_keep_special(b)
            } else {
                query_keep(b)
            }) || b == b'%'
        };
        if p.path.is_empty() {
            if let Some(q) = p.query {
                u.query = Some(percent_encode(q, qk));
            }
            return Ok(u);
        }
        let path = percent_encode(p.path, |b| path_keep(b) || b == b'%');
        let merged = if path.starts_with('/') {
            path
        } else if self.host.is_some() && self.path.is_empty() {
            format!("/{}", path)
        } else {
            let dir = match self.path.rfind('/') {
                Some(i) => &self.path[..=i],
                None => "",
            };
            format!("{}{}", dir, path)
        };
        u.path = remove_dot_segments(&merged);
        if u.path.is_empty() && special {
            u.path.push('/');
        }
        u.query = p.query.map(|q| percent_encode(q, qk));
        Ok(u)
    }

    /// Parse `input` relative to an optional base (as a browser address
    /// bar does for links; without a base a bare `host/path` gets
    /// `http://`).
    pub fn parse_with_base(input: &str, base: Option<&Url>) -> Result<Url, Error> {
        match base {
            Some(b) => b.join(input),
            None => Url::parse(input),
        }
    }

    /// Interpret user input in an address bar: absolute URLs as given,
    /// `/path` as a local file, otherwise `http://` is assumed.
    pub fn from_user_input(input: &str) -> Result<Url, Error> {
        let s = clean(input);
        if s.starts_with('/') {
            return Url::parse(&format!("file://{}", s));
        }
        match Url::parse(&s) {
            // Hierarchical URLs and the well-known opaque schemes are taken
            // as given; anything else ("example.com/x", "localhost:8080",
            // which parses as scheme "localhost") gets "http://".
            Ok(u)
                if u.host.is_some()
                    || matches!(
                        u.scheme.as_str(),
                        "about" | "data" | "mailto" | "javascript" | "file"
                    ) =>
            {
                Ok(u)
            }
            _ => Url::parse(&format!("http://{}", s)),
        }
    }

    pub fn host_str(&self) -> &str {
        self.host.as_deref().unwrap_or("")
    }

    /// Port, or the scheme's default.
    pub fn port_or_default(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }

    /// `host[:port]` as used in the `Host` header (IPv6 bracketed).
    pub fn authority(&self) -> String {
        let h = self.host_str();
        let h = if h.contains(':') {
            format!("[{}]", h)
        } else {
            h.to_string()
        };
        match self.port {
            Some(p) => format!("{}:{}", h, p),
            None => h,
        }
    }

    /// Path plus query: the HTTP request target.
    pub fn request_target(&self) -> String {
        let mut s = if self.path.is_empty() {
            String::from("/")
        } else {
            self.path.clone()
        };
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        s
    }

    /// `scheme://host[:port]`.
    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.authority())
    }

    pub fn is_secure(&self) -> bool {
        matches!(self.scheme.as_str(), "https" | "wss")
    }

    /// The URL without its fragment.
    pub fn without_fragment(&self) -> Url {
        let mut u = self.clone();
        u.fragment = None;
        u
    }

    /// Decoded user name and password.
    pub fn credentials(&self) -> Option<(String, String)> {
        if self.userinfo.is_empty() {
            return None;
        }
        let (u, p) = self
            .userinfo
            .split_once(':')
            .unwrap_or((&self.userinfo, ""));
        Some((percent_decode_str(u), percent_decode_str(p)))
    }

    /// Last path segment, decoded (for download file names).
    pub fn file_name(&self) -> String {
        let seg = self.path.rsplit('/').next().unwrap_or("");
        percent_decode_str(seg)
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}:", self.scheme)?;
        if self.host.is_some() {
            f.write_str("//")?;
            if !self.userinfo.is_empty() {
                write!(f, "{}@", self.userinfo)?;
            }
            f.write_str(&self.authority())?;
        }
        f.write_str(&self.path)?;
        if let Some(q) = &self.query {
            write!(f, "?{}", q)?;
        }
        if let Some(fr) = &self.fragment {
            write!(f, "#{}", fr)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
