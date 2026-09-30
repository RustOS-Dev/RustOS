//! Blocking HTTP/1.1 client over caller-supplied connections.
//!
//! The [`Connector`] opens byte streams (plain TCP, or TLS for `https`),
//! so this module stays independent of the socket and TLS layers and can
//! be tested with scripted responses.

use crate::cookie::{Context, Jar};
use crate::encoding;
use crate::{
    framing, redirect_target, BodyDecoder, Error, Framing, Request, ResponseHead, Result, Url,
};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A bidirectional byte stream (one TCP or TLS connection).
pub trait Stream {
    /// Read into `buf`; `Ok(0)` means the peer closed the connection.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
    fn write_all(&mut self, data: &[u8]) -> Result<()>;
    /// The underlying socket (for poll), if any.
    fn fd(&self) -> i32 {
        -1
    }
    /// Change the read timeout (ms).
    fn set_timeout(&mut self, _ms: u64) {}
}

pub trait Connector {
    /// Open a connection to the origin of `url` (TLS for `https`).
    fn connect(&mut self, url: &Url) -> Result<Box<dyn Stream>>;
    /// Current Unix time in seconds (cookie expiry).
    fn now(&self) -> u64;
}

/// Receives a response while it is read.
pub trait Sink {
    /// Called with the final response head (after redirects).
    fn head(&mut self, _head: &ResponseHead, _url: &Url) -> Result<()> {
        Ok(())
    }
    /// Raw body bytes (content coding not removed).
    fn data(&mut self, data: &[u8]) -> Result<()>;
}

struct Discard;
impl Sink for Discard {
    fn data(&mut self, _: &[u8]) -> Result<()> {
        Ok(())
    }
}

struct Collect {
    body: Vec<u8>,
    limit: usize,
}
impl Sink for Collect {
    fn data(&mut self, d: &[u8]) -> Result<()> {
        if self.body.len() + d.len() > self.limit {
            return Err(Error::TooLarge);
        }
        self.body.extend_from_slice(d);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub user_agent: String,
    pub accept: String,
    pub accept_language: String,
    pub max_redirects: usize,
    /// Largest body [`Client::send`] buffers (after decoding).
    pub max_body: usize,
    /// Ask for and undo gzip/deflate in [`Client::send`].
    pub compression: bool,
    /// Reuse connections (HTTP/1.1 keep-alive).
    pub keep_alive: bool,
    /// Use and update the cookie jar.
    pub cookies: bool,
    /// Follow redirects (otherwise a 3xx response is returned as is).
    pub follow_redirects: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            user_agent: String::from("RustOS/1.0"),
            accept: String::from("*/*"),
            accept_language: String::from("en"),
            max_redirects: 10,
            max_body: 32 << 20,
            compression: true,
            keep_alive: true,
            cookies: true,
            follow_redirects: true,
        }
    }
}

/// A complete (buffered, decoded) response.
#[derive(Debug, Clone)]
pub struct Response {
    /// Final URL after redirects.
    pub url: Url,
    pub head: ResponseHead,
    /// Body with the content coding removed.
    pub body: Vec<u8>,
    /// URLs that redirected here, in order.
    pub redirects: Vec<Url>,
}

impl Response {
    pub fn status(&self) -> u16 {
        self.head.status
    }
    /// Body decoded as text using the header's charset.
    pub fn text(&self) -> String {
        let (_, cs) = self.head.content_type();
        encoding::decode_text(&self.body, cs.as_deref())
    }
}

/// Approximate "site" (registrable domain): the last two labels.
pub fn site(host: &str) -> String {
    let labels: Vec<&str> = host.rsplitn(3, '.').collect();
    if labels.len() >= 2 && !host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        format!("{}.{}", labels[1], labels[0])
    } else {
        host.to_string()
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

pub struct Client<C: Connector> {
    pub connector: C,
    pub jar: Jar,
    pub opts: Options,
    idle: Option<(String, Box<dyn Stream>)>,
}

impl<C: Connector> Client<C> {
    pub fn new(connector: C) -> Client<C> {
        Client {
            connector,
            jar: Jar::new(),
            opts: Options::default(),
            idle: None,
        }
    }

    /// Drop any kept-alive connection.
    pub fn close_idle(&mut self) {
        self.idle = None;
    }

    fn prepare(&self, req: &mut Request, ctx: Context, compression: bool) {
        let h = &mut req.headers;
        if !h.contains("User-Agent") {
            h.set("User-Agent", &self.opts.user_agent);
        }
        if !h.contains("Accept") {
            h.set("Accept", &self.opts.accept);
        }
        if !h.contains("Accept-Language") && !self.opts.accept_language.is_empty() {
            h.set("Accept-Language", &self.opts.accept_language);
        }
        if compression && !h.contains("Accept-Encoding") {
            h.set("Accept-Encoding", encoding::ACCEPT_ENCODING);
        }
        if !compression {
            h.remove("Accept-Encoding");
        }
        h.set(
            "Connection",
            if self.opts.keep_alive {
                "keep-alive"
            } else {
                "close"
            },
        );
        if let Some((u, p)) = req.url.credentials() {
            if !h.contains("Authorization") {
                h.set(
                    "Authorization",
                    &format!("Basic {}", base64(format!("{}:{}", u, p).as_bytes())),
                );
            }
        }
        if self.opts.cookies && !h.contains("Cookie") {
            if let Some(c) = self.jar.header(&req.url, self.connector.now(), ctx) {
                h.set("Cookie", &c);
            }
        } else if !self.opts.cookies {
            h.remove("Cookie");
        }
    }

    /// One request/response on a (possibly reused) connection. The body
    /// goes to `sink` unless `deliver` returns false for the head.
    fn exchange(
        &mut self,
        req: &Request,
        deliver: &mut dyn FnMut(&ResponseHead) -> bool,
        sink: &mut dyn Sink,
    ) -> Result<ResponseHead> {
        let origin = req.url.origin();
        let wire = req.serialize();
        let mut reused = false;
        let mut conn: Box<dyn Stream> = match self.idle.take() {
            Some((o, c)) if o == origin => {
                reused = true;
                c
            }
            _ => self.connector.connect(&req.url)?,
        };
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = alloc::vec![0u8; 16 * 1024];
        // Send and read the head; a stale kept-alive connection fails
        // before any response byte arrives, so retry once on a new one.
        let head = loop {
            let attempt = (|| -> Result<Option<(ResponseHead, usize)>> {
                conn.write_all(&wire)?;
                loop {
                    if let Some((h, n)) = ResponseHead::parse(&buf)? {
                        if h.status / 100 == 1 && h.status != 101 {
                            buf.drain(..n);
                            continue;
                        }
                        return Ok(Some((h, n)));
                    }
                    let n = conn.read(&mut chunk)?;
                    if n == 0 {
                        return Ok(None);
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            })();
            match attempt {
                Ok(Some(h)) => break h,
                Ok(None) | Err(Error::Io(_)) if reused && buf.is_empty() => {
                    reused = false;
                    conn = self.connector.connect(&req.url)?;
                }
                Ok(None) => return Err(Error::Protocol("connection closed without a response")),
                Err(e) => return Err(e),
            }
        };
        let (head, used) = head;
        buf.drain(..used);
        let fr = framing(&req.method, &head)?;
        let mut dec = BodyDecoder::new(fr);
        let want = deliver(&head);
        if want {
            sink.head(&head, &req.url)?;
        }
        let mut discard = Discard;
        let target: &mut dyn Sink = if want { sink } else { &mut discard };
        let mut out = Vec::new();
        let mut pending = buf;
        loop {
            if !pending.is_empty() {
                let n = dec.feed(&pending, &mut out)?;
                pending.drain(..n);
                if !out.is_empty() {
                    target.data(&out)?;
                    out.clear();
                }
                if dec.is_done() {
                    break;
                }
            }
            if dec.is_done() {
                break;
            }
            let n = conn.read(&mut chunk)?;
            if n == 0 {
                dec.finish()?;
                break;
            }
            pending.extend_from_slice(&chunk[..n]);
        }
        if self.opts.keep_alive
            && head.keep_alive()
            && fr != Framing::UntilClose
            && pending.is_empty()
        {
            self.idle = Some((origin, conn));
        }
        Ok(head)
    }

    /// Send a request, following redirects and handling cookies, and
    /// stream the final response body (raw content coding) to `sink`.
    /// Returns the final URL, head and the redirect chain.
    pub fn send_streaming(
        &mut self,
        mut req: Request,
        ctx: Context,
        sink: &mut dyn Sink,
    ) -> Result<(Url, ResponseHead, Vec<Url>)> {
        let mut redirects = Vec::new();
        let mut ctx = ctx;
        let first_site = site(req.url.host_str());
        let compression = self.opts.compression;
        let follow = self.opts.follow_redirects;
        loop {
            if !matches!(req.url.scheme.as_str(), "http" | "https") {
                return Err(Error::UnsupportedScheme(req.url.scheme.clone()));
            }
            self.prepare(&mut req, ctx, compression);
            let method = req.method.clone();
            let url = req.url.clone();
            let mut is_final = false;
            let head = {
                let mut deliver = |h: &ResponseHead| {
                    is_final = !follow || redirect_target(&method, h, &url).is_none();
                    is_final
                };
                self.exchange(&req, &mut deliver, sink)?
            };
            if self.opts.cookies {
                let now = self.connector.now();
                self.jar
                    .store_all(&req.url, head.headers.get_all("Set-Cookie"), now);
            }
            if is_final {
                return Ok((req.url, head, redirects));
            }
            let (next, to_get) = redirect_target(&req.method, &head, &req.url).unwrap();
            if redirects.len() >= self.opts.max_redirects {
                return Err(Error::TooManyRedirects);
            }
            redirects.push(req.url.clone());
            if to_get {
                req.method = String::from("GET");
                req.body.clear();
                req.headers.remove("Content-Type");
                req.headers.remove("Content-Length");
            }
            // Credentials and cookies are recomputed for the new origin.
            if next.origin() != req.url.origin() {
                req.headers.remove("Authorization");
            }
            req.headers.remove("Cookie");
            req.headers.remove("Host");
            ctx = Context {
                same_site: ctx.same_site && site(next.host_str()) == first_site,
                top_level_safe: matches!(req.method.as_str(), "GET" | "HEAD"),
            };
            req.url = next;
        }
    }

    /// Send a request and return the decoded, buffered response.
    pub fn send(&mut self, req: Request, ctx: Context) -> Result<Response> {
        let mut c = Collect {
            body: Vec::new(),
            limit: self.opts.max_body,
        };
        let (url, head, redirects) = self.send_streaming(req, ctx, &mut c)?;
        let body = match head.headers.get("Content-Encoding") {
            Some(ce) => encoding::decode_content(ce, c.body, self.opts.max_body)?,
            None => c.body,
        };
        Ok(Response {
            url,
            head,
            body,
            redirects,
        })
    }

    pub fn get(&mut self, url: Url) -> Result<Response> {
        self.send(Request::get(url), Context::USER)
    }

    /// Submit `application/x-www-form-urlencoded` data.
    pub fn post_form(
        &mut self,
        url: Url,
        pairs: &[(&str, &str)],
        ctx: Context,
    ) -> Result<Response> {
        let body = crate::form::serialize(pairs.iter().copied()).into_bytes();
        self.send(
            Request::post(url, "application/x-www-form-urlencoded", body),
            ctx,
        )
    }
}
