//! wget (HTTP/1.1 client) and httpd (static file server).

use crate::err;
use rustos_rt::net::{self, Ipv4, Socket, SocketAddr};
use rustos_rt::prelude::*;
use rustos_rt::{fs, io, time};

struct Url {
    host: String,
    port: u16,
    path: String,
}

fn parse_url(u: &str) -> Option<Url> {
    let rest = if let Some(r) = u.strip_prefix("http://") {
        r
    } else if u.starts_with("https://") {
        return None;
    } else {
        u
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (hostport, 80),
    };
    if host.is_empty() {
        return None;
    }
    Some(Url {
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

/// Buffered reader over a socket.
struct Conn {
    s: Socket,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl Conn {
    fn fill(&mut self) -> rustos_rt::Result<bool> {
        if self.eof {
            return Ok(false);
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let mut tmp = [0u8; 16384];
        let n = self.s.recv(&mut tmp)?;
        if n == 0 {
            self.eof = true;
            return Ok(false);
        }
        self.buf.extend_from_slice(&tmp[..n]);
        Ok(true)
    }

    fn line(&mut self) -> rustos_rt::Result<Option<String>> {
        loop {
            if let Some(i) = self.buf[self.pos..].iter().position(|&b| b == b'\n') {
                let l = String::from_utf8_lossy(&self.buf[self.pos..self.pos + i])
                    .trim_end_matches('\r')
                    .to_string();
                self.pos += i + 1;
                return Ok(Some(l));
            }
            if !self.fill()? {
                return Ok(None);
            }
        }
    }

    /// Up to `max` bytes (0 = at end of stream).
    fn read(&mut self, max: usize) -> rustos_rt::Result<Vec<u8>> {
        if self.pos >= self.buf.len() && !self.fill()? {
            return Ok(Vec::new());
        }
        let n = max.min(self.buf.len() - self.pos);
        let v = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        Ok(v)
    }
}

enum Sink {
    Stdout,
    File(fs::File),
}

impl Sink {
    fn write(&mut self, d: &[u8]) -> rustos_rt::Result<()> {
        match self {
            Sink::Stdout => io::write_all(io::STDOUT, d),
            Sink::File(f) => f.write_all(d),
        }
    }
}

pub fn wget(args: &[String]) -> i32 {
    let mut out: Option<String> = None;
    let mut quiet = false;
    let mut timeout = 30u64;
    let mut url = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-O" => {
                i += 1;
                out = args.get(i).cloned();
            }
            "-q" => quiet = true,
            "-T" => {
                i += 1;
                timeout = args.get(i).and_then(|t| t.parse().ok()).unwrap_or(timeout);
            }
            u => url = Some(u.to_string()),
        }
        i += 1;
    }
    let Some(mut url_s) = url else {
        eprintln!("usage: wget [-q] [-O FILE] [-T SECS] http://HOST[:PORT]/PATH");
        return 2;
    };
    for _redirect in 0..6 {
        let Some(u) = parse_url(&url_s) else {
            eprintln!("wget: unsupported URL '{}' (only http:// is supported)", url_s);
            return 1;
        };
        let addr = match net::resolve(&u.host) {
            Ok(v) => SocketAddr { ip: v[0], port: u.port },
            Err(e) => return err("wget", &u.host, e),
        };
        if !quiet {
            eprintln!("Connecting to {} ({})...", u.host, addr);
        }
        let s = match net::tcp_connect(addr) {
            Ok(s) => s,
            Err(e) => return err("wget", &format!("{}", addr), e),
        };
        let _ = s.set_timeout(timeout * 1000);
        let host_hdr = if u.port == 80 { u.host.clone() } else { format!("{}:{}", u.host, u.port) };
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: RustOS-wget/1.0\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            u.path, host_hdr
        );
        if let Err(e) = s.send_all(req.as_bytes()) {
            return err("wget", "send", e);
        }
        let mut c = Conn { s, buf: Vec::new(), pos: 0, eof: false };
        let status_line = match c.line() {
            Ok(Some(l)) => l,
            _ => {
                eprintln!("wget: no response");
                return 1;
            }
        };
        let code: u32 = status_line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        let mut length: Option<usize> = None;
        let mut chunked = false;
        let mut location = None;
        while let Ok(Some(h)) = c.line() {
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                let v = v.trim();
                match k.to_ascii_lowercase().as_str() {
                    "content-length" => length = v.parse().ok(),
                    "transfer-encoding" => chunked = v.eq_ignore_ascii_case("chunked"),
                    "location" => location = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        if !quiet {
            eprintln!("HTTP request sent, awaiting response... {}", status_line);
        }
        if (300..400).contains(&code) {
            if let Some(l) = location {
                url_s = if l.starts_with('/') {
                    format!("http://{}{}", host_hdr, l)
                } else {
                    l
                };
                if !quiet {
                    eprintln!("Redirected to {}", url_s);
                }
                continue;
            }
        }
        if code != 200 {
            eprintln!("wget: server returned {}", status_line);
            return 1;
        }
        let name = out.clone().unwrap_or_else(|| {
            let b = u.path.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("");
            if b.is_empty() { String::from("index.html") } else { b.to_string() }
        });
        let mut sink = if name == "-" {
            Sink::Stdout
        } else {
            match fs::File::create(&name) {
                Ok(f) => Sink::File(f),
                Err(e) => return err("wget", &name, e),
            }
        };
        let start = time::millis();
        let mut total = 0usize;
        let r: rustos_rt::Result<()> = (|| {
            if chunked {
                loop {
                    let Some(l) = c.line()? else { break };
                    let size = usize::from_str_radix(l.split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
                    if size == 0 {
                        break;
                    }
                    let mut left = size;
                    while left > 0 {
                        let d = c.read(left)?;
                        if d.is_empty() {
                            return Ok(());
                        }
                        sink.write(&d)?;
                        left -= d.len();
                        total += d.len();
                    }
                    let _ = c.line()?;
                }
            } else {
                loop {
                    let want = length.map_or(65536, |l| (l - total).min(65536));
                    if want == 0 {
                        break;
                    }
                    let d = c.read(want)?;
                    if d.is_empty() {
                        break;
                    }
                    sink.write(&d)?;
                    total += d.len();
                }
            }
            Ok(())
        })();
        if let Err(e) = r {
            return err("wget", "transfer", e);
        }
        if length.is_some_and(|l| l != total) {
            eprintln!("wget: connection closed after {} of {} bytes", total, length.unwrap());
            return 1;
        }
        if !quiet {
            let ms = (time::millis() - start).max(1);
            eprintln!(
                "'{}' saved [{} bytes, {} KB/s]",
                name,
                total,
                total as u64 * 1000 / 1024 / ms
            );
        }
        return 0;
    }
    eprintln!("wget: too many redirects");
    1
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html",
        "txt" | "md" | "rs" | "conf" => "text/plain",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn respond(s: &Socket, code: &str, ctype: &str, body: &[u8], head: bool) {
    let hdr = format!(
        "HTTP/1.1 {}\r\nServer: RustOS-httpd\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        code,
        ctype,
        body.len()
    );
    let _ = s.send_all(hdr.as_bytes());
    if !head {
        let _ = s.send_all(body);
    }
}

fn serve(s: Socket, root: &str) {
    let _ = s.set_timeout(10_000);
    let mut c = Conn { s, buf: Vec::new(), pos: 0, eof: false };
    let Ok(Some(req)) = c.line() else { return };
    while let Ok(Some(h)) = c.line() {
        if h.is_empty() {
            break;
        }
    }
    let mut parts = req.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");
    let s = &c.s;
    if method != "GET" && method != "HEAD" {
        respond(s, "405 Method Not Allowed", "text/plain", b"method not allowed\n", false);
        return;
    }
    let head = method == "HEAD";
    if target.split('/').any(|seg| seg == "..") {
        respond(s, "403 Forbidden", "text/plain", b"forbidden\n", head);
        return;
    }
    let mut path = format!("{}{}", root.trim_end_matches('/'), target);
    if fs::is_dir(&path) {
        let index = format!("{}/index.html", path.trim_end_matches('/'));
        if fs::exists(&index) {
            path = index;
        } else {
            let mut body = format!("<html><body><h1>Index of {}</h1><ul>\n", target);
            for e in fs::read_dir(&path).unwrap_or_default() {
                let slash = if e.is_dir() { "/" } else { "" };
                body.push_str(&format!(
                    "<li><a href=\"{}{}{}\">{}{}</a></li>\n",
                    target.trim_end_matches('/'),
                    if target.ends_with('/') { "" } else { "/" },
                    e.name,
                    e.name,
                    slash
                ));
            }
            body.push_str("</ul></body></html>\n");
            respond(s, "200 OK", "text/html", body.as_bytes(), head);
            return;
        }
    }
    match fs::read(&path) {
        Ok(data) => respond(s, "200 OK", content_type(&path), &data, head),
        Err(_) => respond(s, "404 Not Found", "text/plain", b"not found\n", head),
    }
}

/// httpd [-p PORT] [-d DIR]: serve files until killed.
pub fn httpd(args: &[String]) -> i32 {
    let mut port = 80u16;
    let mut root = String::from("/srv/www");
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-p" => {
                i += 1;
                port = args.get(i).and_then(|p| p.parse().ok()).unwrap_or(port);
            }
            "-d" | "-h" => {
                i += 1;
                root = args.get(i).cloned().unwrap_or(root);
            }
            "-f" => {}
            _ => {
                eprintln!("usage: httpd [-p PORT] [-d DIR]");
                return 2;
            }
        }
        i += 1;
    }
    let l = match net::tcp_listen(SocketAddr { ip: Ipv4::ANY, port }) {
        Ok(l) => l,
        Err(e) => return err("httpd", &format!("port {}", port), e),
    };
    eprintln!("httpd: serving {} on port {}", root, port);
    loop {
        match l.accept() {
            Ok((c, peer)) => {
                let _ = peer;
                serve(c, &root);
            }
            Err(e) => {
                if e.0 == 4 {
                    return 0;
                }
                return err("httpd", "accept", e);
            }
        }
    }
}
