//! Cookie jar (RFC 6265 / 6265bis).
//!
//! Stores cookies from `Set-Cookie`, returns the `Cookie` header for a
//! request, and persists to the Netscape `cookies.txt` format used by curl
//! and wget. Captive portals depend on this: the login form's response
//! sets a session cookie that later requests must carry.

use crate::Url;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
    /// Attribute absent: treated like `None` (as Firefox and text browsers
    /// do), which keeps cross-site portal logins working.
    Unset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Lower-case domain without a leading dot.
    pub domain: String,
    /// Only sent to exactly `domain` (no `Domain` attribute).
    pub host_only: bool,
    pub path: String,
    /// Expiry (Unix seconds); `None` for a session cookie.
    pub expires: Option<u64>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    /// Insertion order, for the stable sort in the `Cookie` header.
    pub created: u64,
}

/// Request properties that affect which cookies are sent.
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// The request comes from a page on the same site (or the user typed
    /// the address).
    pub same_site: bool,
    /// A top-level navigation with a safe method (GET/HEAD).
    pub top_level_safe: bool,
}

impl Context {
    /// Typed address or bookmark: everything is sent.
    pub const USER: Context = Context {
        same_site: true,
        top_level_safe: true,
    };
}

const MAX_COOKIES: usize = 3000;
const MAX_PER_DOMAIN: usize = 60;

/// Suffixes under which sites cannot set cookies (besides single-label
/// top-level domains). A tiny subset of the Public Suffix List.
const PUBLIC_SUFFIXES: &[&str] = &[
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "me.uk",
    "com.au",
    "net.au",
    "org.au",
    "edu.au",
    "co.jp",
    "ne.jp",
    "or.jp",
    "co.nz",
    "com.br",
    "com.cn",
    "com.tw",
    "co.in",
    "co.kr",
    "co.za",
    "com.mx",
    "com.tr",
    "github.io",
    "herokuapp.com",
    "blogspot.com",
];

fn is_ip(host: &str) -> bool {
    host.contains(':')
        || (!host.is_empty() && host.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
}

/// `host` equals `domain` or is a subdomain of it.
pub fn domain_match(host: &str, domain: &str) -> bool {
    host == domain
        || (host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
            && !is_ip(host))
}

fn path_match(req: &str, cookie: &str) -> bool {
    req == cookie
        || (req.starts_with(cookie)
            && (cookie.ends_with('/') || req.as_bytes().get(cookie.len()) == Some(&b'/')))
}

fn default_path(url: &Url) -> String {
    match url.path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) if url.path.starts_with('/') => String::from(&url.path[..i]),
        _ => String::from("/"),
    }
}

/// Parse a cookie date (RFC 6265 section 5.1.1): accepts RFC 1123, RFC 850
/// and asctime forms and most variations. Returns Unix seconds.
pub fn parse_date(s: &str) -> Option<u64> {
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    let delim = |c: char| {
        let b = c as u32;
        b == 0x09
            || (0x20..=0x2F).contains(&b)
            || (0x3B..=0x40).contains(&b)
            || (0x5B..=0x60).contains(&b)
            || (0x7B..=0x7E).contains(&b)
    };
    for tok in s.split(delim).filter(|t| !t.is_empty()) {
        if time.is_none() {
            let parts: Vec<&str> = tok.split(':').collect();
            if parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.len() <= 2 && p.bytes().all(|b| b.is_ascii_digit()))
            {
                let v: Vec<u32> = parts.iter().map(|p| p.parse().unwrap_or(99)).collect();
                time = Some((v[0], v[1], v[2]));
                continue;
            }
        }
        let digits = tok.bytes().take_while(|b| b.is_ascii_digit()).count();
        if day.is_none() && (1..=2).contains(&digits) && digits == tok.len() {
            day = tok.parse::<u32>().ok();
            continue;
        }
        if month.is_none() && tok.len() >= 3 {
            const M: [&str; 12] = [
                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
            ];
            let t = tok[..3].to_ascii_lowercase();
            if let Some(i) = M.iter().position(|m| *m == t) {
                month = Some(i as u32 + 1);
                continue;
            }
        }
        if year.is_none() && (2..=4).contains(&digits) {
            year = tok[..digits].parse::<u32>().ok();
            continue;
        }
    }
    let (h, mi, sec) = time?;
    let (d, mo, mut y) = (day?, month?, year?);
    if (70..=99).contains(&y) {
        y += 1900;
    } else if y <= 69 {
        y += 2000;
    }
    if !(1..=31).contains(&d) || y < 1601 || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    if y < 1970 {
        return Some(0);
    }
    Some(days_from_civil(y, mo, d) * 86400 + (h * 3600 + mi * 60 + sec) as u64)
}

fn days_from_civil(y: u32, m: u32, d: u32) -> u64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468) as u64
}

#[derive(Debug, Clone, Default)]
pub struct Jar {
    pub cookies: Vec<Cookie>,
    counter: u64,
}

impl Jar {
    pub fn new() -> Jar {
        Jar::default()
    }

    /// Store the cookie from one `Set-Cookie` header received for `url`.
    /// Returns false if it was rejected.
    pub fn store(&mut self, url: &Url, header: &str, now: u64) -> bool {
        let mut parts = header.split(';');
        let pair = parts.next().unwrap_or("");
        let (name, value) = match pair.split_once('=') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => ("", pair.trim()),
        };
        if name.is_empty() && value.is_empty() {
            return false;
        }
        let host = url.host_str().to_ascii_lowercase();
        let secure_origin = url.is_secure() || host == "localhost" || host == "127.0.0.1";
        let mut c = Cookie {
            name: name.to_string(),
            value: value.trim_matches('"').to_string(),
            domain: host.clone(),
            host_only: true,
            path: default_path(url),
            expires: None,
            secure: false,
            http_only: false,
            same_site: SameSite::Unset,
            created: 0,
        };
        let mut max_age: Option<i64> = None;
        for attr in parts {
            let (k, v) = match attr.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (attr.trim(), ""),
            };
            match k.to_ascii_lowercase().as_str() {
                "expires" => {
                    if let Some(t) = parse_date(v) {
                        c.expires = Some(t);
                    }
                }
                "max-age" => {
                    if let Ok(n) = v.parse::<i64>() {
                        max_age = Some(n);
                    }
                }
                "domain" => {
                    let d = v.trim_start_matches('.').to_ascii_lowercase();
                    if !d.is_empty() {
                        if !domain_match(&host, &d) {
                            return false;
                        }
                        if d != host && (!d.contains('.') || PUBLIC_SUFFIXES.contains(&d.as_str()))
                        {
                            return false;
                        }
                        c.domain = d;
                        c.host_only = false;
                    }
                }
                "path" => {
                    if v.starts_with('/') {
                        c.path = v.to_string();
                    }
                }
                "secure" => c.secure = true,
                "httponly" => c.http_only = true,
                "samesite" => {
                    c.same_site = match v.to_ascii_lowercase().as_str() {
                        "strict" => SameSite::Strict,
                        "lax" => SameSite::Lax,
                        "none" => SameSite::None,
                        _ => SameSite::Unset,
                    }
                }
                _ => {}
            }
        }
        if let Some(ma) = max_age {
            c.expires = Some(if ma <= 0 {
                0
            } else {
                now.saturating_add(ma as u64)
            });
        }
        if c.secure && !secure_origin {
            return false;
        }
        if c.name.starts_with("__Secure-") && !c.secure {
            return false;
        }
        if c.name.starts_with("__Host-") && (!c.secure || !c.host_only || c.path != "/") {
            return false;
        }
        // Replace a cookie with the same identity (keeping its age).
        let pos = self
            .cookies
            .iter()
            .position(|o| o.name == c.name && o.domain == c.domain && o.path == c.path);
        if let Some(p) = pos {
            c.created = self.cookies[p].created;
            self.cookies.remove(p);
        } else {
            self.counter += 1;
            c.created = self.counter;
        }
        if c.expires.is_some_and(|e| e <= now) {
            return true; // deletion
        }
        self.cookies.push(c);
        self.enforce_limits();
        true
    }

    fn enforce_limits(&mut self) {
        let last = self.cookies.last().map(|c| c.domain.clone());
        if let Some(d) = last {
            while self.cookies.iter().filter(|c| c.domain == d).count() > MAX_PER_DOMAIN {
                let oldest = self
                    .cookies
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.domain == d)
                    .min_by_key(|(_, c)| c.created)
                    .map(|(i, _)| i)
                    .unwrap();
                self.cookies.remove(oldest);
            }
        }
        while self.cookies.len() > MAX_COOKIES {
            let oldest = self
                .cookies
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| c.created)
                .map(|(i, _)| i)
                .unwrap();
            self.cookies.remove(oldest);
        }
    }

    /// Store every `Set-Cookie` header of a response.
    pub fn store_all<'a>(&mut self, url: &Url, headers: impl Iterator<Item = &'a str>, now: u64) {
        for h in headers {
            self.store(url, h, now);
        }
    }

    /// Cookies that apply to a request for `url`, in header order.
    pub fn matching(&self, url: &Url, now: u64, ctx: Context) -> Vec<&Cookie> {
        let host = url.host_str().to_ascii_lowercase();
        let path = if url.path.is_empty() {
            "/"
        } else {
            url.path.as_str()
        };
        let secure_origin = url.is_secure() || host == "localhost" || host == "127.0.0.1";
        let mut v: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                (if c.host_only {
                    host == c.domain
                } else {
                    domain_match(&host, &c.domain)
                }) && path_match(path, &c.path)
                    && (!c.secure || secure_origin)
                    && c.expires.is_none_or(|e| e > now)
                    && match c.same_site {
                        SameSite::Strict => ctx.same_site,
                        SameSite::Lax => ctx.same_site || ctx.top_level_safe,
                        SameSite::None | SameSite::Unset => true,
                    }
            })
            .collect();
        v.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.created.cmp(&b.created))
        });
        v
    }

    /// Value for the `Cookie` request header, if any cookie applies.
    pub fn header(&self, url: &Url, now: u64, ctx: Context) -> Option<String> {
        let v = self.matching(url, now, ctx);
        if v.is_empty() {
            return None;
        }
        let parts: Vec<String> = v
            .iter()
            .map(|c| {
                if c.name.is_empty() {
                    c.value.clone()
                } else {
                    format!("{}={}", c.name, c.value)
                }
            })
            .collect();
        Some(parts.join("; "))
    }

    /// Drop expired cookies (and, with `session`, session cookies).
    pub fn purge(&mut self, now: u64, session: bool) {
        self.cookies
            .retain(|c| c.expires.map_or(!session, |e| e > now));
    }

    /// Remove every cookie for a domain (and its subdomains).
    pub fn clear_domain(&mut self, domain: &str) {
        self.cookies.retain(|c| !domain_match(&c.domain, domain));
    }

    /// Netscape `cookies.txt`. Session cookies are included with expiry 0
    /// only when `session` is set.
    pub fn save(&self, session: bool) -> String {
        let mut s = String::from("# Netscape HTTP Cookie File\n# Written by RustOS\n\n");
        for c in &self.cookies {
            if c.expires.is_none() && !session {
                continue;
            }
            let prefix = if c.http_only { "#HttpOnly_" } else { "" };
            let dom = if c.host_only {
                c.domain.clone()
            } else {
                format!(".{}", c.domain)
            };
            s.push_str(&format!(
                "{}{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                prefix,
                dom,
                if c.host_only { "FALSE" } else { "TRUE" },
                c.path,
                if c.secure { "TRUE" } else { "FALSE" },
                c.expires.unwrap_or(0),
                c.name,
                c.value
            ));
        }
        s
    }

    /// Load a `cookies.txt` (curl/wget/Netscape format).
    pub fn load(&mut self, text: &str, now: u64) {
        for line in text.lines() {
            let (line, http_only) = match line.strip_prefix("#HttpOnly_") {
                Some(l) => (l, true),
                None => (line, false),
            };
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 7 {
                continue;
            }
            let exp: u64 = f[4].parse().unwrap_or(0);
            if exp != 0 && exp <= now {
                continue;
            }
            self.counter += 1;
            let c = Cookie {
                domain: f[0].trim_start_matches('.').to_ascii_lowercase(),
                host_only: !f[1].eq_ignore_ascii_case("TRUE"),
                path: f[2].to_string(),
                secure: f[3].eq_ignore_ascii_case("TRUE"),
                expires: if exp == 0 { None } else { Some(exp) },
                name: f[5].to_string(),
                value: f[6].to_string(),
                http_only,
                same_site: SameSite::Unset,
                created: self.counter,
            };
            self.cookies
                .retain(|o| !(o.name == c.name && o.domain == c.domain && o.path == c.path));
            self.cookies.push(c);
        }
    }
}
