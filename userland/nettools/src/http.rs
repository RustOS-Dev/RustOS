//! wget (HTTP/1.1 and HTTPS client) and httpd (static file server).

use crate::err;
use rustos_rt::net::{self, Ipv4, Socket, SocketAddr};
use rustos_rt::prelude::*;
use rustos_rt::{fs, io};
use webclient::httpc::client::Sink;
use webclient::httpc::cookie::Context;
use webclient::httpc::{self, Request, ResponseHead, Url};
use webclient::nettls::Versions;

const USAGE: &str =
    "usage: wget [-q] [-v] [-S] [-k] [-O FILE] [-T SECS] [-U AGENT] [--header 'K: V']
            [--post-data DATA | --post-file FILE] [--load-cookies FILE] [--save-cookies FILE]
            [--keep-session-cookies] [--max-redirect N] [--secure-protocol auto|TLSv1_2|TLSv1_3]
            [--content-on-error] URL...";

/// Writes the body to a file or stdout, created on the first byte (so a
/// failed request leaves no empty file behind).
struct Output {
    path: String,
    file: Option<fs::File>,
    bytes: u64,
    show_headers: bool,
    keep_errors: bool,
    status: u16,
    failed: Option<String>,
}

impl Sink for Output {
    fn head(&mut self, h: &ResponseHead, url: &Url) -> httpc::Result<()> {
        self.status = h.status;
        if self.show_headers {
            eprintln!("  {} -> HTTP/1.{} {} {}", url, h.minor, h.status, h.reason);
            for (k, v) in &h.headers.0 {
                eprintln!("  {}: {}", k, v);
            }
        }
        Ok(())
    }

    fn data(&mut self, d: &[u8]) -> httpc::Result<()> {
        if self.status >= 400 && !self.keep_errors {
            return Ok(());
        }
        if self.path == "-" {
            io::write_all(1, d).map_err(|e| httpc::Error::Io(e.to_string()))?;
        } else {
            if self.file.is_none() {
                match fs::File::create(&self.path) {
                    Ok(f) => self.file = Some(f),
                    Err(e) => {
                        self.failed = Some(format!("{}: {}", self.path, e));
                        return Err(httpc::Error::Io(format!("{}: {}", self.path, e)));
                    }
                }
            }
            self.file
                .as_ref()
                .unwrap()
                .write_all(d)
                .map_err(|e| httpc::Error::Io(format!("{}: {}", self.path, e)))?;
        }
        self.bytes += d.len() as u64;
        Ok(())
    }
}

pub fn wget(args: &[String]) -> i32 {
    let mut out: Option<String> = None;
    let mut quiet = false;
    let mut verbose = false;
    let mut show_headers = false;
    let mut insecure = false;
    let mut timeout = 20u64;
    let mut post: Option<Vec<u8>> = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut agent: Option<String> = None;
    let mut load_cookies: Option<String> = None;
    let mut save_cookies: Option<String> = None;
    let mut keep_session = false;
    let mut max_redirect = 20usize;
    let mut versions = Versions::Both;
    let mut keep_errors = false;
    let mut urls = Vec::new();

    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        // "--opt=value" or "--opt value" / "-O value".
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a, None),
        };
        let value = |i: &mut usize| -> Option<String> {
            inline.clone().or_else(|| {
                *i += 1;
                args.get(*i).cloned()
            })
        };
        match flag {
            "-q" | "--quiet" => quiet = true,
            "-v" | "--verbose" => verbose = true,
            "-S" | "--server-response" => show_headers = true,
            "-k" | "--no-check-certificate" => insecure = true,
            "--keep-session-cookies" => keep_session = true,
            "--content-on-error" => keep_errors = true,
            "-O" | "--output-document" => out = value(&mut i),
            "-T" | "--timeout" => {
                timeout = value(&mut i)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(timeout)
            }
            "-U" | "--user-agent" => agent = value(&mut i),
            "--header" => {
                if let Some(h) = value(&mut i)
                    && let Some((k, v)) = h.split_once(':')
                {
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            "--post-data" => post = value(&mut i).map(String::into_bytes),
            "--post-file" => {
                let f = value(&mut i).unwrap_or_default();
                match fs::read(&f) {
                    Ok(d) => post = Some(d),
                    Err(e) => return err("wget", &f, e),
                }
            }
            "--load-cookies" => load_cookies = value(&mut i),
            "--save-cookies" => save_cookies = value(&mut i),
            "--max-redirect" => {
                max_redirect = value(&mut i)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(max_redirect)
            }
            "--secure-protocol" => {
                versions = match value(&mut i).unwrap_or_default().as_str() {
                    "TLSv1_2" => Versions::Tls12Only,
                    "TLSv1_3" => Versions::Tls13Only,
                    _ => Versions::Both,
                }
            }
            "-h" | "--help" => {
                println!("{}", USAGE);
                return 0;
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                eprintln!("wget: unknown option {}\n{}", a, USAGE);
                return 2;
            }
            _ => urls.push(a.to_string()),
        }
        i += 1;
    }
    if urls.is_empty() {
        eprintln!("{}", USAGE);
        return 2;
    }

    let mut netc = webclient::Net::new();
    netc.timeout_ms = timeout * 1000;
    netc.insecure = insecure;
    netc.versions = versions;
    netc.verbose = verbose;
    let mut client = webclient::client(netc);
    client.opts.compression = false; // save exactly what the server sends
    client.opts.max_redirects = max_redirect;
    client.opts.keep_alive = urls.len() > 1;
    if let Some(a) = agent {
        client.opts.user_agent = a;
    }
    if let Some(f) = &load_cookies {
        webclient::load_cookies(&mut client.jar, f);
    }

    let mut status = 0;
    for raw in &urls {
        let url = match Url::from_user_input(raw) {
            Ok(u) => u,
            Err(e) => {
                eprintln!("wget: {}: {}", raw, e);
                status = 1;
                continue;
            }
        };
        let path = out.clone().unwrap_or_else(|| {
            let n = url.file_name();
            if n.is_empty() {
                String::from("index.html")
            } else {
                n
            }
        });
        let mut req = match &post {
            Some(body) => Request::post(
                url.clone(),
                "application/x-www-form-urlencoded",
                body.clone(),
            ),
            None => Request::get(url.clone()),
        };
        for (k, v) in &headers {
            req.headers.set(k, v);
        }
        if !quiet && path != "-" {
            eprintln!("--> {}", url);
        }
        let mut sink = Output {
            path: path.clone(),
            file: None,
            bytes: 0,
            show_headers,
            keep_errors,
            status: 0,
            failed: None,
        };
        match client.send_streaming(req, Context::USER, &mut sink) {
            Ok((_, head, _)) => {
                if head.status >= 400 {
                    eprintln!(
                        "wget: {}: server returned {} {}",
                        url, head.status, head.reason
                    );
                    status = 8;
                } else if !quiet && path != "-" {
                    eprintln!("saved '{}' ({} bytes)", path, sink.bytes);
                }
                if let Some(f) = sink.file.take() {
                    let _ = f.sync();
                }
            }
            Err(e) => {
                let tls_verify = client
                    .connector
                    .last_tls_error
                    .as_ref()
                    .is_some_and(|t| t.is_certificate_error());
                eprintln!("wget: {}: {}", url, e);
                if tls_verify {
                    eprintln!("wget: use --no-check-certificate (-k) to connect anyway");
                }
                status = if tls_verify { 5 } else { 4 };
            }
        }
    }
    if let Some(f) = &save_cookies
        && !webclient::save_cookies(&client.jar, f, keep_session)
    {
        eprintln!("wget: cannot write {}", f);
    }
    status
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
    // Read the request head (up to the blank line).
    let mut head_buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !head_buf.windows(4).any(|w| w == b"\r\n\r\n") && head_buf.len() < 64 * 1024 {
        match s.recv(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => head_buf.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&head_buf).into_owned();
    let req = text.lines().next().unwrap_or("");
    let mut parts = req.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");
    let target = &webclient::httpc::weburl::percent_decode_str(target);
    let s = &s;
    if method != "GET" && method != "HEAD" {
        respond(
            s,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed\n",
            false,
        );
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
    let l = match net::tcp_listen(SocketAddr {
        ip: Ipv4::ANY,
        port,
    }) {
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
