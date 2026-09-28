extern crate std;

use super::client::{Client, Connector, Sink, Stream};
use super::cookie::{parse_date, Context, Jar, SameSite};
use super::*;
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::ToString;
use alloc::vec;
use core::cell::RefCell;
use std::collections::VecDeque;

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[test]
fn request_serialisation() {
    let mut r = Request::post(
        url("http://[::1]:8080/login?x=1#f"),
        "application/x-www-form-urlencoded",
        b"a=1".to_vec(),
    );
    r.headers.set("User-Agent", "t");
    let w = String::from_utf8(r.serialize()).unwrap();
    assert!(
        w.starts_with("POST /login?x=1 HTTP/1.1\r\nHost: [::1]:8080\r\n"),
        "{}",
        w
    );
    assert!(w.contains("Content-Length: 3\r\n"));
    assert!(w.ends_with("\r\n\r\na=1"));
    let g = String::from_utf8(Request::get(url("http://a/")).serialize()).unwrap();
    assert_eq!(g, "GET / HTTP/1.1\r\nHost: a\r\n\r\n");
}

#[test]
fn response_head_parsing() {
    assert_eq!(
        ResponseHead::parse(b"HTTP/1.1 200 OK\r\nA: b\r\n").unwrap(),
        None
    );
    let raw = b"HTTP/1.1 302 Found\r\nLocation: /x\r\nSet-Cookie: a=1\r\nset-cookie: b=2\r\nX-Long: one\r\n  two\r\n\r\nBODY";
    let (h, n) = ResponseHead::parse(raw).unwrap().unwrap();
    assert_eq!(&raw[n..], b"BODY");
    assert_eq!(h.status, 302);
    assert_eq!(h.reason, "Found");
    assert_eq!(h.headers.get("location"), Some("/x"));
    assert_eq!(
        h.headers.get_all("Set-Cookie").collect::<Vec<_>>(),
        vec!["a=1", "b=2"]
    );
    assert_eq!(h.headers.get("X-Long"), Some("one two"));
    assert!(h.keep_alive());
    let (h, _) =
        ResponseHead::parse(b"HTTP/1.0 200\nContent-Type: text/html; charset=\"ISO-8859-1\"\n\n")
            .unwrap()
            .unwrap();
    assert_eq!(h.status, 200);
    assert!(!h.keep_alive());
    assert_eq!(
        h.content_type(),
        ("text/html".to_string(), Some("iso-8859-1".to_string()))
    );
    assert!(ResponseHead::parse(b"SSH-2.0-OpenSSH\r\n\r\n").is_err());
}

#[test]
fn framing_rules() {
    let head = |s: &str| ResponseHead::parse(s.as_bytes()).unwrap().unwrap().0;
    assert_eq!(
        framing("GET", &head("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")).unwrap(),
        Framing::Length(5)
    );
    assert_eq!(
        framing(
            "HEAD",
            &head("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")
        )
        .unwrap(),
        Framing::None
    );
    assert_eq!(
        framing("GET", &head("HTTP/1.1 204 No Content\r\n\r\n")).unwrap(),
        Framing::None
    );
    assert_eq!(
        framing(
            "GET",
            &head(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\nContent-Length: 9\r\n\r\n"
            )
        )
        .unwrap(),
        Framing::Chunked
    );
    assert_eq!(
        framing("GET", &head("HTTP/1.1 200 OK\r\n\r\n")).unwrap(),
        Framing::UntilClose
    );
}

#[test]
fn chunked_decoding_byte_by_byte() {
    let wire =
        b"4;ext=1\r\nWiki\r\n5\r\npedia\r\nE\r\n in\r\n\r\nchunks.\r\n0\r\nTrailer: x\r\n\r\nNEXT";
    // Whole buffer at once.
    let mut d = BodyDecoder::new(Framing::Chunked);
    let mut out = Vec::new();
    let used = d.feed(wire, &mut out).unwrap();
    assert!(d.is_done());
    assert_eq!(&wire[used..], b"NEXT");
    assert_eq!(out, b"Wikipedia in\r\n\r\nchunks.");
    // One byte at a time.
    let mut d = BodyDecoder::new(Framing::Chunked);
    let mut out = Vec::new();
    let mut i = 0;
    while !d.is_done() {
        i += d.feed(&wire[i..i + 1], &mut out).unwrap();
    }
    assert_eq!(out, b"Wikipedia in\r\n\r\nchunks.");
    assert_eq!(&wire[i..], b"NEXT");
    // Truncated body is an error; read-until-close is not.
    let mut d = BodyDecoder::new(Framing::Length(10));
    d.feed(b"abc", &mut Vec::new()).unwrap();
    assert!(d.finish().is_err());
    let mut d = BodyDecoder::new(Framing::UntilClose);
    assert!(d.finish().is_ok());
    let mut d = BodyDecoder::new(Framing::Chunked);
    assert!(d.feed(b"zz\r\n", &mut Vec::new()).is_err());
}

#[test]
fn redirects() {
    let base = url("http://a/form#top");
    let head = |s: &str| ResponseHead::parse(s.as_bytes()).unwrap().unwrap().0;
    let (u, get) = redirect_target(
        "POST",
        &head("HTTP/1.1 302 Found\r\nLocation: /done\r\n\r\n"),
        &base,
    )
    .unwrap();
    assert_eq!(u.to_string(), "http://a/done#top");
    assert!(get);
    let (_, get) = redirect_target(
        "POST",
        &head("HTTP/1.1 307 Temporary\r\nLocation: /x\r\n\r\n"),
        &base,
    )
    .unwrap();
    assert!(!get);
    let (_, get) = redirect_target(
        "GET",
        &head("HTTP/1.1 303 See Other\r\nLocation: https://b/\r\n\r\n"),
        &base,
    )
    .unwrap();
    assert!(get);
    assert!(redirect_target("GET", &head("HTTP/1.1 304 Not Modified\r\n\r\n"), &base).is_none());
    assert!(redirect_target("GET", &head("HTTP/1.1 301 Moved\r\n\r\n"), &base).is_none());
}

// ---------------------------------------------------------------------------
// Encodings
// ---------------------------------------------------------------------------

#[test]
fn gzip_and_deflate() {
    // gzip of "hello hello hello\n" (Python gzip, mtime 0)
    let gz = [
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0x57, 0xc8, 0x40, 0x90, 0x5c, 0x00, 0x3b, 0x7c, 0x8a, 0xdf, 0x12, 0x00, 0x00, 0x00,
    ];
    assert_eq!(
        encoding::gunzip(&gz, 1 << 20).unwrap(),
        b"hello hello hello\n"
    );
    assert_eq!(
        encoding::decode_content("gzip", gz.to_vec(), 1 << 20).unwrap(),
        b"hello hello hello\n"
    );
    // zlib-wrapped and raw deflate of "abcabcabc"
    let zlib = miniz_oxide::deflate::compress_to_vec_zlib(b"abcabcabc", 6);
    let raw = miniz_oxide::deflate::compress_to_vec(b"abcabcabc", 6);
    assert_eq!(
        encoding::decode_content("deflate", zlib, 1 << 20).unwrap(),
        b"abcabcabc"
    );
    assert_eq!(
        encoding::decode_content("deflate", raw, 1 << 20).unwrap(),
        b"abcabcabc"
    );
    assert_eq!(
        encoding::decode_content("identity", b"x".to_vec(), 10).unwrap(),
        b"x"
    );
    assert!(encoding::decode_content("br", b"x".to_vec(), 10).is_err());
    // Size limit (a gzip bomb stays bounded).
    let big = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 100_000], 9);
    assert_eq!(
        encoding::decode_content("deflate", big, 1000),
        Err(Error::TooLarge)
    );
}

#[test]
fn charsets() {
    assert_eq!(
        encoding::decode_text(b"caf\xe9 \x80 \x93q\x94", Some("iso-8859-1")),
        "café € “q”"
    );
    assert_eq!(encoding::decode_text(b"\xa4", Some("iso-8859-15")), "€");
    assert_eq!(encoding::decode_text("naïve".as_bytes(), None), "naïve");
    assert_eq!(
        encoding::decode_text(b"\xef\xbb\xbfbom", Some("latin1")),
        "bom"
    );
    assert_eq!(encoding::decode_text(b"\xff\xfeh\x00i\x00", None), "hi");
    assert_eq!(
        encoding::decode_text(b"bad \xff", Some("utf-8")),
        "bad \u{FFFD}"
    );
}

// ---------------------------------------------------------------------------
// Cookies
// ---------------------------------------------------------------------------

#[test]
fn cookie_dates() {
    assert_eq!(parse_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784111777));
    assert_eq!(
        parse_date("Sunday, 06-Nov-94 08:49:37 GMT"),
        Some(784111777)
    );
    assert_eq!(parse_date("Sun Nov  6 08:49:37 1994"), Some(784111777));
    assert_eq!(
        parse_date("Wed, 09 Jun 2021 10:18:14 GMT"),
        Some(1623233894)
    );
    assert_eq!(parse_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
    assert_eq!(parse_date("garbage"), None);
    assert_eq!(parse_date("Mon, 32 Jan 2024 00:00:00 GMT"), None);
}

#[test]
fn cookie_jar_rules() {
    let now = 1_700_000_000;
    let mut j = Jar::new();
    let u = url("http://www.example.com/app/login");
    assert!(j.store(&u, "sid=abc; Path=/; HttpOnly", now));
    assert!(j.store(&u, "pref=1", now)); // default path /app
    assert!(j.store(&u, "wide=1; Domain=.example.com; Path=/", now));
    assert!(!j.store(&u, "evil=1; Domain=other.com", now));
    assert!(!j.store(&u, "tld=1; Domain=com", now));
    assert!(!j.store(&url("http://x.co.uk/"), "ps=1; Domain=co.uk", now));
    assert!(!j.store(&u, "sec=1; Secure", now)); // Secure from http
    assert!(!j.store(
        &url("https://www.example.com/"),
        "__Host-x=1; Secure; Domain=example.com",
        now
    ));
    assert!(j.store(
        &url("https://www.example.com/"),
        "__Host-y=1; Secure; Path=/",
        now
    ));
    assert!(j.store(&u, "old=1; Expires=Thu, 01 Jan 1970 00:00:00 GMT", now));
    assert!(j.store(&u, "short=1; Max-Age=10; Path=/", now));

    let h = |j: &Jar, s: &str, t: u64| j.header(&url(s), t, Context::USER).unwrap_or_default();
    assert_eq!(
        h(&j, "http://www.example.com/app/x", now),
        "pref=1; sid=abc; wide=1; short=1"
    );
    assert_eq!(
        h(&j, "http://www.example.com/", now),
        "sid=abc; wide=1; short=1"
    );
    assert_eq!(h(&j, "http://api.example.com/", now), "wide=1");
    assert_eq!(
        h(&j, "http://www.example.com/application", now),
        "sid=abc; wide=1; short=1"
    );
    assert_eq!(
        h(&j, "https://www.example.com/", now),
        "sid=abc; wide=1; __Host-y=1; short=1"
    );
    assert_eq!(
        h(&j, "http://www.example.com/", now + 20),
        "sid=abc; wide=1"
    );

    // Overwrite and delete.
    j.store(&u, "sid=new; Path=/", now);
    assert!(h(&j, "http://www.example.com/", now).starts_with("sid=new"));
    j.store(&u, "sid=; Path=/; Max-Age=0", now);
    assert!(!h(&j, "http://www.example.com/", now).contains("sid="));
}

#[test]
fn cookie_samesite() {
    let now = 100;
    let mut j = Jar::new();
    let u = url("https://bank.example/");
    j.store(&u, "s=1; SameSite=Strict; Secure", now);
    j.store(&u, "l=1; SameSite=Lax; Secure", now);
    j.store(&u, "n=1; SameSite=None; Secure", now);
    let cross_post = Context {
        same_site: false,
        top_level_safe: false,
    };
    let cross_get = Context {
        same_site: false,
        top_level_safe: true,
    };
    assert_eq!(j.header(&u, now, cross_post).unwrap(), "n=1");
    assert_eq!(j.header(&u, now, cross_get).unwrap(), "l=1; n=1");
    assert_eq!(j.header(&u, now, Context::USER).unwrap(), "s=1; l=1; n=1");
    assert_eq!(j.cookies[0].same_site, SameSite::Strict);
}

#[test]
fn cookie_file_roundtrip() {
    let now = 1000;
    let mut j = Jar::new();
    j.store(
        &url("https://a.example/"),
        "keep=1; Max-Age=500; Domain=a.example; Secure; HttpOnly",
        now,
    );
    j.store(&url("http://b.example/p/q"), "sess=2", now);
    let txt = j.save(false);
    assert!(txt.contains("#HttpOnly_.a.example\tTRUE\t/\tTRUE\t1500\tkeep\t1"));
    assert!(!txt.contains("sess"));
    let mut k = Jar::new();
    k.load(&txt, now);
    assert_eq!(
        k.header(&url("https://www.a.example/x"), now, Context::USER)
            .unwrap(),
        "keep=1"
    );
    let mut expired = Jar::new();
    expired.load(&txt, 2000);
    assert!(expired.cookies.is_empty());
    let all = j.save(true);
    assert!(all.contains("b.example\tFALSE\t/p\tFALSE\t0\tsess\t2"));
}

#[test]
fn multipart_body() {
    let (ct, body) = multipart::encode(
        &[
            ("user", multipart::Part::Text("joe")),
            (
                "up\"load",
                multipart::Part::File {
                    filename: "a.txt",
                    content_type: "text/plain",
                    data: b"hi",
                },
            ),
        ],
        0xabc,
    );
    assert_eq!(
        ct,
        "multipart/form-data; boundary=----RustOSFormBoundary0000000000000abc"
    );
    let s = String::from_utf8(body).unwrap();
    assert_eq!(
        s,
        "------RustOSFormBoundary0000000000000abc\r\nContent-Disposition: form-data; name=\"user\"\r\n\r\njoe\r\n\
         ------RustOSFormBoundary0000000000000abc\r\nContent-Disposition: form-data; name=\"up%22load\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nhi\r\n\
         ------RustOSFormBoundary0000000000000abc--\r\n"
    );
}

// ---------------------------------------------------------------------------
// Client against scripted connections
// ---------------------------------------------------------------------------

/// One scripted connection: bytes the server sends (in pieces).
struct Conn {
    replies: VecDeque<Vec<u8>>,
    log: Rc<RefCell<Vec<String>>>,
    id: usize,
}

impl Stream for Conn {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self.replies.front_mut() {
            None => Ok(0),
            Some(r) => {
                let n = r.len().min(buf.len()).min(7); // small reads
                buf[..n].copy_from_slice(&r[..n]);
                r.drain(..n);
                if r.is_empty() {
                    self.replies.pop_front();
                }
                Ok(n)
            }
        }
    }
    fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.log.borrow_mut().push(std::format!(
            "#{} {}",
            self.id,
            String::from_utf8_lossy(data)
        ));
        Ok(())
    }
}

struct Script {
    /// Per new connection: the replies it will produce.
    conns: VecDeque<Vec<&'static str>>,
    log: Rc<RefCell<Vec<String>>>,
    opened: Rc<RefCell<Vec<String>>>,
    n: usize,
}

impl Connector for Script {
    fn connect(&mut self, url: &Url) -> Result<Box<dyn Stream>> {
        self.opened.borrow_mut().push(url.origin());
        let replies = self.conns.pop_front().ok_or(Error::Io("refused".into()))?;
        self.n += 1;
        Ok(Box::new(Conn {
            replies: replies.into_iter().map(|s| s.as_bytes().to_vec()).collect(),
            log: self.log.clone(),
            id: self.n,
        }))
    }
    fn now(&self) -> u64 {
        1_700_000_000
    }
}

fn client(
    conns: Vec<Vec<&'static str>>,
) -> (
    Client<Script>,
    Rc<RefCell<Vec<String>>>,
    Rc<RefCell<Vec<String>>>,
) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let opened = Rc::new(RefCell::new(Vec::new()));
    let c = Client::new(Script {
        conns: conns.into_iter().collect(),
        log: log.clone(),
        opened: opened.clone(),
        n: 0,
    });
    (c, log, opened)
}

#[test]
fn client_portal_login_flow() {
    // A captive portal: the probe is redirected to a login page that sets
    // a cookie; posting the form (with the cookie) redirects to success.
    let (mut c, log, opened) = client(vec![
        vec!["HTTP/1.1 302 Found\r\nLocation: http://portal.local/login?orig=x\r\nContent-Length: 0\r\n\r\n"],
        vec![
            "HTTP/1.1 200 OK\r\nSet-Cookie: session=s1; Path=/\r\nContent-Type: text/html; charset=utf-8\r\nTransfer-Encoding: chunked\r\n\r\n",
            "5\r\n<form\r\n0\r\n\r\n",
            "HTTP/1.1 303 See Other\r\nLocation: /ok\r\nContent-Length: 2\r\n\r\nxx",
            "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nwelcome",
        ],
    ]);
    let r = c.get(url("http://probe.example/generate_204")).unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.url.to_string(), "http://portal.local/login?orig=x");
    assert_eq!(r.text(), "<form");
    assert_eq!(r.redirects.len(), 1);
    let r = c
        .post_form(
            r.url.clone(),
            &[("user", "a b"), ("pw", "x&y")],
            Context::USER,
        )
        .unwrap();
    assert_eq!(r.text(), "welcome");
    assert_eq!(r.url.to_string(), "http://portal.local/ok");
    let log = log.borrow();
    // The POST and the following GET reuse connection #2 and carry the cookie.
    assert!(
        log[2].starts_with("#2 POST /login?orig=x HTTP/1.1\r\n"),
        "{}",
        log[2]
    );
    assert!(log[2].contains("Cookie: session=s1\r\n"));
    assert!(log[2].ends_with("user=a+b&pw=x%26y"));
    assert!(log[3].starts_with("#2 GET /ok HTTP/1.1\r\n"));
    assert!(log[3].contains("Cookie: session=s1\r\n"));
    assert!(!log[3].contains("Content-Length"));
    assert_eq!(opened.borrow().len(), 2);
}

#[test]
fn client_retries_stale_keepalive() {
    let (mut c, log, opened) = client(vec![
        // First connection answers once, then the server silently closes.
        vec!["HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\na"],
        vec!["HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nb"],
    ]);
    assert_eq!(c.get(url("http://h/1")).unwrap().body, b"a");
    assert_eq!(c.get(url("http://h/2")).unwrap().body, b"b");
    assert_eq!(opened.borrow().len(), 2);
    assert!(log.borrow()[1].starts_with("#1 GET /2"));
    assert!(log.borrow()[2].starts_with("#2 GET /2"));
    assert!(log.borrow()[0].contains("Accept-Encoding: gzip, deflate\r\n"));
}

#[test]
fn client_streaming_sink_and_limits() {
    struct S {
        heads: usize,
        data: Vec<u8>,
    }
    impl Sink for S {
        fn head(&mut self, h: &ResponseHead, u: &Url) -> Result<()> {
            assert_eq!(h.status, 200);
            assert_eq!(u.path, "/final");
            self.heads += 1;
            Ok(())
        }
        fn data(&mut self, d: &[u8]) -> Result<()> {
            self.data.extend_from_slice(d);
            Ok(())
        }
    }
    let (mut c, _, _) = client(vec![vec![
        "HTTP/1.1 301 Moved\r\nLocation: /final\r\nContent-Length: 4\r\n\r\njunk",
        "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\n\r\n0123456789",
    ]]);
    let mut s = S {
        heads: 0,
        data: Vec::new(),
    };
    let (u, h, chain) = c
        .send_streaming(Request::get(url("http://h/start")), Context::USER, &mut s)
        .unwrap();
    assert_eq!(
        (u.path.as_str(), h.status, chain.len(), s.heads),
        ("/final", 200, 1, 1)
    );
    assert_eq!(s.data, b"0123456789");

    let (mut c, _, _) = client(vec![vec![
        "HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789",
    ]]);
    c.opts.max_body = 4;
    assert_eq!(c.get(url("http://h/")).unwrap_err(), Error::TooLarge);

    let mut loops = Vec::new();
    for _ in 0..12 {
        loops.push("HTTP/1.1 302 Found\r\nLocation: /again\r\nContent-Length: 0\r\n\r\n");
    }
    let (mut c, _, _) = client(vec![loops]);
    assert_eq!(
        c.get(url("http://h/")).unwrap_err(),
        Error::TooManyRedirects
    );

    let (mut c, _, _) = client(vec![]);
    assert!(matches!(
        c.get(url("ftp://h/")),
        Err(Error::UnsupportedScheme(_))
    ));

    // Not following: the redirect itself is the answer (portal probes).
    let (mut c, _, _) = client(vec![vec![
        "HTTP/1.1 302 Found\r\nLocation: http://portal/\r\nContent-Length: 0\r\n\r\n",
    ]]);
    c.opts.follow_redirects = false;
    let r = c.get(url("http://probe/generate_204")).unwrap();
    assert_eq!(
        (r.status(), r.head.headers.get("Location")),
        (302, Some("http://portal/"))
    );
}

#[test]
fn client_basic_auth_and_cross_site_redirect() {
    let (mut c, log, _) = client(vec![
        vec!["HTTP/1.1 302 Found\r\nLocation: http://other.example/x\r\nSet-Cookie: a=1\r\nContent-Length: 0\r\n\r\n"],
        vec!["HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"],
    ]);
    let mut req = Request::get(url("http://user:p%40ss@site.example/"));
    req.headers.set("X-Test", "1");
    c.send(req, Context::USER).unwrap();
    let log = log.borrow();
    assert!(
        log[0].contains("Authorization: Basic dXNlcjpwQHNz\r\n"),
        "{}",
        log[0]
    );
    assert!(!log[1].contains("Authorization"));
    assert!(log[1].contains("Host: other.example\r\n"));
    assert!(log[1].contains("X-Test: 1\r\n"));
}
