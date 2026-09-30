//! Fetching pages: HTTP(S) through `httpc` + `webclient` (cookies kept in
//! a persistent jar), `file:` URLs and directory listings, and built-in
//! `about:` pages.

use rustos_rt::prelude::*;
use rustos_rt::{fs, time};
use webclient::httpc::client::Sink;
use webclient::httpc::cookie::Context;
use webclient::httpc::{self, Request, ResponseHead, Url, encoding};
use webclient::{Client, Net};

pub struct Loaded {
    /// Final URL (after redirects).
    pub url: Url,
    pub status: u16,
    pub mime: String,
    pub text: String,
    pub headers: Vec<(String, String)>,
    /// TLS version and cipher, if HTTPS.
    pub tls: String,
    pub redirects: usize,
}

pub enum LoadError {
    /// The server certificate was rejected (host, reason).
    Certificate(String, String),
    Other(String),
}

pub struct Loader {
    pub client: Client<Net>,
    pub cookie_path: &'static str,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn mime_for_path(p: &str) -> &'static str {
    let lower = p.to_ascii_lowercase();
    if lower.ends_with(".html") || lower.ends_with(".htm") || lower.ends_with(".xhtml") {
        "text/html"
    } else {
        "text/plain"
    }
}

const HELP: &str = include_str!("help.html");

impl Loader {
    pub fn new(insecure: bool) -> Loader {
        let mut net = Net::new();
        net.insecure = insecure;
        net.timeout_ms = 30_000;
        let mut client = webclient::client(net);
        client.opts.accept =
            String::from("text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8");
        client.opts.user_agent = String::from("Mozilla/5.0 (compatible; RustOS browse/1.0; text)");
        let cookie_path = webclient::cookie_file();
        webclient::load_cookies(&mut client.jar, cookie_path);
        Loader {
            client,
            cookie_path,
        }
    }

    pub fn save_cookies(&self) {
        webclient::save_cookies(&self.client.jar, self.cookie_path, false);
    }

    fn about(&mut self, url: &Url) -> Loaded {
        let page = url.path.as_str();
        let html = match page {
            "help" => String::from(HELP),
            "cookies" => {
                if let Some(q) = &url.query {
                    for (k, v) in httpc::form::parse(q) {
                        if k == "delete" {
                            self.client.jar.clear_domain(&v);
                            self.save_cookies();
                        }
                    }
                }
                let now = time::now();
                let mut s = String::from(
                    "<title>Cookies</title><h1>Cookies</h1><table><tr><th>Domain<th>Name<th>Expires<th></tr>",
                );
                let mut domains: Vec<String> = Vec::new();
                for c in &self.client.jar.cookies {
                    if c.expires.is_some_and(|e| e <= now) {
                        continue;
                    }
                    let exp = match c.expires {
                        Some(e) => {
                            let (y, mo, d, _, _, _) = time::civil(e);
                            format!("{:04}-{:02}-{:02}", y, mo, d)
                        }
                        None => String::from("session"),
                    };
                    s.push_str(&format!(
                        "<tr><td>{}<td>{}<td>{}<td><a href=\"about:cookies?delete={}\">delete domain</a></tr>",
                        escape(&c.domain),
                        escape(&c.name),
                        exp,
                        escape(&c.domain)
                    ));
                    if !domains.contains(&c.domain) {
                        domains.push(c.domain.clone());
                    }
                }
                s.push_str("</table>");
                if domains.is_empty() {
                    s.push_str("<p>No cookies stored.</p>");
                }
                s.push_str(&format!(
                    "<p>Persistent cookies are saved in {}.</p>",
                    self.cookie_path
                ));
                s
            }
            _ => String::from("<title>about:blank</title>"),
        };
        Loaded {
            url: url.clone(),
            status: 200,
            mime: String::from("text/html"),
            text: html,
            headers: Vec::new(),
            tls: String::new(),
            redirects: 0,
        }
    }

    fn file(&self, url: &Url) -> Result<Loaded, LoadError> {
        let path = httpc::weburl::percent_decode_str(&url.path);
        let path = if path.is_empty() {
            String::from("/")
        } else {
            path
        };
        let (mime, text) = if fs::is_dir(&path) {
            let mut entries =
                fs::read_dir(&path).map_err(|e| LoadError::Other(format!("{}: {}", path, e)))?;
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            let mut s = format!(
                "<title>{}</title><h1>Index of {}</h1><ul>",
                escape(&path),
                escape(&path)
            );
            if path != "/" {
                s.push_str("<li><a href=\"../\">../</a>");
            }
            for e in entries {
                let slash = if e.is_dir() { "/" } else { "" };
                s.push_str(&format!(
                    "<li><a href=\"{}{}\">{}{}</a>",
                    httpc::weburl::percent_encode(&e.name, |b| b != b'%' && b != b'#' && b != b'?'),
                    slash,
                    escape(&e.name),
                    slash
                ));
            }
            s.push_str("</ul>");
            (String::from("text/html"), s)
        } else {
            let data = fs::read(&path).map_err(|e| LoadError::Other(format!("{}: {}", path, e)))?;
            let mime = mime_for_path(&path);
            let cs = if mime == "text/html" {
                html::sniff_charset(&data)
            } else {
                None
            };
            (
                String::from(mime),
                encoding::decode_text(&data, cs.as_deref()),
            )
        };
        Ok(Loaded {
            url: url.clone(),
            status: 200,
            mime,
            text,
            headers: Vec::new(),
            tls: String::new(),
            redirects: 0,
        })
    }

    /// Load a request.
    pub fn fetch(&mut self, req: Request, ctx: Context) -> Result<Loaded, LoadError> {
        match req.url.scheme.as_str() {
            "about" => return Ok(self.about(&req.url)),
            "file" => return self.file(&req.url),
            "http" | "https" => {}
            other => {
                return Err(LoadError::Other(format!(
                    "'{}:' links are not supported",
                    other
                )));
            }
        }
        let host = req.url.host_str().to_string();
        let r = match self.client.send(req, ctx) {
            Ok(r) => r,
            Err(e) => {
                let tls = self.client.connector.last_tls_error.take();
                return Err(match tls {
                    Some(t) if t.is_certificate_error() => {
                        LoadError::Certificate(host, t.to_string())
                    }
                    _ => LoadError::Other(e.to_string()),
                });
            }
        };
        let (mime, cs) = r.head.content_type();
        let mime = if mime.is_empty() {
            mime_for_path(&r.url.path).to_string()
        } else {
            mime
        };
        let cs = cs.or_else(|| {
            if mime.contains("html") {
                html::sniff_charset(&r.body)
            } else {
                None
            }
        });
        let text = encoding::decode_text(&r.body, cs.as_deref());
        if r.head.headers.contains("Set-Cookie") {
            self.save_cookies();
        }
        Ok(Loaded {
            url: r.url.clone(),
            status: r.head.status,
            mime,
            text,
            headers: r.head.headers.0.clone(),
            tls: if r.url.is_secure() {
                self.client.connector.last_tls.clone()
            } else {
                String::new()
            },
            redirects: r.redirects.len(),
        })
    }

    /// Fetch a resource as bytes (images, fonts): (body, content type).
    pub fn fetch_bytes(
        &mut self,
        url: &Url,
        referrer: &Url,
        limit: usize,
    ) -> Option<(Vec<u8>, String)> {
        match url.scheme.as_str() {
            "file" => {
                let path = httpc::weburl::percent_decode_str(&url.path);
                let d = fs::read(&path).ok()?;
                return (d.len() <= limit).then(|| (d, String::new()));
            }
            "data" => {
                let s = url.to_string();
                let (meta, data) = s.strip_prefix("data:")?.split_once(',')?;
                let bytes = if meta.ends_with(";base64") {
                    webclient::nettls::base64_decode(&httpc::weburl::percent_decode_str(data))?
                } else {
                    httpc::weburl::percent_decode(data)
                };
                return Some((bytes, meta.split(';').next().unwrap_or("").to_string()));
            }
            "http" | "https" => {}
            _ => return None,
        }
        let ctx = Context {
            same_site: httpc::client::site(url.host_str())
                == httpc::client::site(referrer.host_str()),
            top_level_safe: false,
        };
        let max = self.client.opts.max_body;
        self.client.opts.max_body = limit;
        let r = self.client.send(Request::get(url.clone()), ctx);
        self.client.opts.max_body = max;
        let r = r.ok()?;
        if r.head.status >= 400 {
            return None;
        }
        let (mime, _) = r.head.content_type();
        Some((r.body, mime))
    }

    /// Save a URL to a file (streamed).
    pub fn download(&mut self, url: Url, path: &str) -> Result<u64, String> {
        struct ToFile {
            f: Option<fs::File>,
            path: String,
            n: u64,
            status: u16,
        }
        impl Sink for ToFile {
            fn head(&mut self, h: &ResponseHead, _: &Url) -> httpc::Result<()> {
                self.status = h.status;
                Ok(())
            }
            fn data(&mut self, d: &[u8]) -> httpc::Result<()> {
                if self.f.is_none() {
                    self.f = Some(
                        fs::File::create(&self.path)
                            .map_err(|e| httpc::Error::Io(e.to_string()))?,
                    );
                }
                self.f
                    .as_ref()
                    .unwrap()
                    .write_all(d)
                    .map_err(|e| httpc::Error::Io(e.to_string()))?;
                self.n += d.len() as u64;
                Ok(())
            }
        }
        if url.scheme == "file" {
            let src = httpc::weburl::percent_decode_str(&url.path);
            return fs::copy(&src, path).map_err(|e| e.to_string());
        }
        let mut sink = ToFile {
            f: None,
            path: path.to_string(),
            n: 0,
            status: 0,
        };
        let compression = self.client.opts.compression;
        self.client.opts.compression = false;
        let r = self
            .client
            .send_streaming(Request::get(url), Context::USER, &mut sink);
        self.client.opts.compression = compression;
        r.map_err(|e| e.to_string())?;
        if sink.status >= 400 {
            return Err(format!("server returned {}", sink.status));
        }
        Ok(sink.n)
    }
}
