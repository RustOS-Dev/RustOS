//! WebSocket (RFC 6455) and EventSource connections opened by page
//! scripts. They belong to the page's entry; the browser's event loop
//! polls their sockets and forwards what arrives to jsd.

use crate::Entry;
use crate::load::Loader;
use crate::script::{b64d, b64e};
use alloc::collections::BTreeMap;
use jsproto::Json;
use rustos_rt::prelude::*;
use rustos_rt::time;
use webclient::httpc::client::{Connector, Stream, site};
use webclient::httpc::cookie::Context;
use webclient::httpc::{self, BodyDecoder, ResponseHead, Url};

pub struct Conn {
    id: i64,
    ws: bool,
    stream: Box<dyn Stream>,
    buf: Vec<u8>,
    /// EventSource body decoding (chunked or not).
    body: Option<BodyDecoder>,
    /// WebSocket message being reassembled (opcode, data).
    frag: Option<(u8, Vec<u8>)>,
    closing: bool,
}

fn msg(kind: &str, pairs: Vec<(&str, Json)>) -> Json {
    let mut m = BTreeMap::new();
    m.insert(String::from("kind"), Json::from(kind));
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Json::Obj(m)
}

fn send_js(e: &mut Entry, t: &str, id: i64, m: Json) {
    let Some(s) = e.script.as_mut() else { return };
    let Json::Obj(mut o) = m else { return };
    o.insert(String::from("t"), Json::from(t));
    o.insert(String::from("id"), Json::from(id));
    s.js.send(&Json::Obj(o));
}

// ---- SHA-1 (for Sec-WebSocket-Accept) ----

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut m = data.to_vec();
    let bits = (data.len() as u64) * 8;
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&bits.to_be_bytes());
    for block in m.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// The Sec-WebSocket-Accept value for `key`.
pub fn ws_accept(key: &str) -> String {
    b64e(&sha1(
        format!("{}258EAFA5-E914-47DA-95CA-C5AB0DC85B11", key).as_bytes(),
    ))
}

// ---- opening ----

/// Read the response head (the socket's timeout applies).
fn read_head(stream: &mut Box<dyn Stream>, buf: &mut Vec<u8>) -> Option<ResponseHead> {
    let start = time::millis();
    loop {
        if let Ok(Some((h, n))) = ResponseHead::parse(buf) {
            buf.drain(..n);
            return Some(h);
        }
        if time::millis() - start > 15_000 || buf.len() > 65536 {
            return None;
        }
        let mut b = [0u8; 4096];
        match stream.read(&mut b) {
            Ok(0) => return None,
            Ok(n) => buf.extend_from_slice(&b[..n]),
            Err(httpc::Error::Timeout) => {}
            Err(_) => return None,
        }
    }
}

/// wsOpen / esOpen from jsd.
pub fn open(e: &mut Entry, loader: &mut Loader, m: &Json) {
    let id = m.int("id").unwrap_or(0);
    let ws = m.str("t") == Some("wsOpen");
    let (t, fail) = if ws {
        ("ws", msg("error", vec![]))
    } else {
        ("es", msg("error", vec![("fatal", Json::Bool(true))]))
    };
    let Some(mut url) = m.str("url").and_then(|u| Url::parse(u).ok()) else {
        return send_js(e, t, id, fail);
    };
    let page = e.loaded.url.clone();
    let secure_scheme = if ws {
        url.scheme == "wss"
    } else {
        url.scheme == "https"
    };
    if page.is_secure() && !secure_scheme {
        return send_js(e, t, id, fail); // mixed content
    }
    if ws {
        url.scheme = String::from(if url.scheme == "wss" { "https" } else { "http" });
    }
    let mut stream = match loader.client.connector.connect(&url) {
        Ok(s) => s,
        Err(_) => return send_js(e, t, id, fail),
    };
    let origin = page.origin();
    let now = time::now();
    let ctx = Context {
        same_site: site(url.host_str()) == site(page.host_str()),
        top_level_safe: false,
    };
    let cookies = loader.client.jar.header(&url, now, ctx);
    let mut req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nOrigin: {}\r\n",
        url.request_target(),
        url.authority(),
        loader.client.opts.user_agent,
        origin
    );
    if let Some(c) = &cookies {
        req.push_str(&format!("Cookie: {}\r\n", c));
    }
    let key = {
        let mut k = [0u8; 16];
        rustos_rt::process::getrandom(&mut k);
        b64e(&k)
    };
    if ws {
        req.push_str(&format!("Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n", key));
        let protos: Vec<String> = m
            .arr("protocols")
            .unwrap_or(&[])
            .iter()
            .filter_map(|p| p.as_str().map(String::from))
            .collect();
        if !protos.is_empty() {
            req.push_str(&format!(
                "Sec-WebSocket-Protocol: {}\r\n",
                protos.join(", ")
            ));
        }
    } else {
        req.push_str("Accept: text/event-stream\r\nCache-Control: no-cache\r\n");
        if let Some(l) = m.str("lastEventId").filter(|l| !l.is_empty()) {
            req.push_str(&format!("Last-Event-ID: {}\r\n", l));
        }
    }
    req.push_str("\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return send_js(e, t, id, fail);
    }
    let mut buf = Vec::new();
    let Some(head) = read_head(&mut stream, &mut buf) else {
        return send_js(e, t, id, fail);
    };
    loader
        .client
        .jar
        .store_all(&url, head.headers.get_all("Set-Cookie"), now);
    if ws {
        let ok = head.status == 101
            && head
                .headers
                .get("Upgrade")
                .is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
            && head.headers.get("Sec-WebSocket-Accept").map(str::trim)
                == Some(ws_accept(&key).as_str());
        if !ok {
            return send_js(e, t, id, fail);
        }
        let proto = head
            .headers
            .get("Sec-WebSocket-Protocol")
            .unwrap_or("")
            .to_string();
        send_js(e, t, id, msg("open", vec![("protocol", Json::from(proto))]));
    } else {
        let ct = head.headers.get("Content-Type").unwrap_or("");
        if head.status != 200 || !ct.to_ascii_lowercase().starts_with("text/event-stream") {
            return send_js(e, t, id, fail);
        }
        send_js(e, t, id, msg("open", vec![]));
    }
    stream.set_timeout(1);
    let body = if ws {
        None
    } else {
        httpc::framing("GET", &head).ok().map(BodyDecoder::new)
    };
    let mut c = Conn {
        id,
        ws,
        stream,
        buf,
        body,
        frag: None,
        closing: false,
    };
    // Bytes that came with the head.
    let pending = core::mem::take(&mut c.buf);
    e.sockets.push(c);
    let i = e.sockets.len() - 1;
    if !pending.is_empty() && !feed(e, i, &pending) {
        e.sockets.remove(i);
    }
}

// ---- jsd commands ----

fn frame(op: u8, data: &[u8]) -> Vec<u8> {
    let mut f = vec![0x80 | op];
    let n = data.len();
    if n < 126 {
        f.push(0x80 | n as u8);
    } else if n < 65536 {
        f.push(0x80 | 126);
        f.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        f.push(0x80 | 127);
        f.extend_from_slice(&(n as u64).to_be_bytes());
    }
    let mut mask = [0u8; 4];
    rustos_rt::process::getrandom(&mut mask);
    f.extend_from_slice(&mask);
    f.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

pub fn command(e: &mut Entry, m: &Json) {
    let id = m.int("id").unwrap_or(0);
    let Some(i) = e.sockets.iter().position(|c| c.id == id) else {
        return;
    };
    match m.str("t") {
        Some("wsSend") => {
            let f = match (m.str("text"), m.str("b64")) {
                (Some(t), _) => frame(1, t.as_bytes()),
                (None, Some(b)) => frame(2, &b64d(b)),
                _ => return,
            };
            if e.sockets[i].stream.write_all(&f).is_err() {
                close(e, i, 1006, "", false);
            }
        }
        Some("wsClose") => {
            let code = m.int("code").unwrap_or(1000) as u16;
            let mut p = code.to_be_bytes().to_vec();
            p.extend_from_slice(m.str("reason").unwrap_or("").as_bytes());
            let c = &mut e.sockets[i];
            c.closing = true;
            let _ = c.stream.write_all(&frame(8, &p));
        }
        Some("esClose") => {
            e.sockets.remove(i);
        }
        _ => {}
    }
}

fn close(e: &mut Entry, i: usize, code: u16, reason: &str, clean: bool) {
    let c = e.sockets.remove(i);
    let t = if c.ws { "ws" } else { "es" };
    let kind = if c.ws && clean {
        "close"
    } else if c.ws {
        "error"
    } else {
        "end"
    };
    send_js(
        e,
        t,
        c.id,
        msg(
            kind,
            vec![
                ("code", Json::from(code as usize)),
                ("reason", Json::from(reason)),
            ],
        ),
    );
}

// ---- incoming data ----

/// Sockets to poll.
pub fn fds(e: &Entry) -> Vec<i32> {
    e.sockets
        .iter()
        .map(|c| c.stream.fd())
        .filter(|&f| f >= 0)
        .collect()
}

/// Read whatever the connections have and pass it on.
pub fn service(e: &mut Entry) {
    let mut i = 0;
    while i < e.sockets.len() {
        let mut data = Vec::new();
        let mut eof = false;
        loop {
            let mut b = [0u8; 16384];
            match e.sockets[i].stream.read(&mut b) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    data.extend_from_slice(&b[..n]);
                    if data.len() > 1 << 20 {
                        break;
                    }
                }
                Err(httpc::Error::Timeout) => break,
                Err(_) => {
                    eof = true;
                    break;
                }
            }
        }
        let alive = data.is_empty() || feed(e, i, &data);
        if !alive {
            continue; // removed
        }
        if eof {
            close(e, i, 1006, "", false);
            continue;
        }
        i += 1;
    }
}

/// Handle bytes from connection `i`; false if it was closed (removed).
fn feed(e: &mut Entry, i: usize, data: &[u8]) -> bool {
    if !e.sockets[i].ws {
        let c = &mut e.sockets[i];
        let mut out = Vec::new();
        if let Some(d) = c.body.as_mut() {
            if d.feed(data, &mut out).is_err() {
                close(e, i, 0, "", false);
                return false;
            }
        } else {
            out.extend_from_slice(data);
        }
        let done = c.body.as_ref().is_some_and(|d| d.is_done());
        let id = c.id;
        if !out.is_empty() {
            send_js(
                e,
                "es",
                id,
                msg(
                    "chunk",
                    vec![(
                        "text",
                        Json::from(String::from_utf8_lossy(&out).into_owned()),
                    )],
                ),
            );
        }
        if done {
            close(e, i, 0, "", false);
            return false;
        }
        return true;
    }
    e.sockets[i].buf.extend_from_slice(data);
    loop {
        let c = &mut e.sockets[i];
        let b = &c.buf;
        if b.len() < 2 {
            return true;
        }
        let fin = b[0] & 0x80 != 0;
        let op = b[0] & 0x0f;
        let masked = b[1] & 0x80 != 0;
        let (mut len, mut pos) = ((b[1] & 0x7f) as usize, 2);
        if len == 126 {
            if b.len() < 4 {
                return true;
            }
            len = u16::from_be_bytes([b[2], b[3]]) as usize;
            pos = 4;
        } else if len == 127 {
            if b.len() < 10 {
                return true;
            }
            len = u64::from_be_bytes(b[2..10].try_into().unwrap()) as usize;
            pos = 10;
        }
        let mask = if masked {
            if b.len() < pos + 4 {
                return true;
            }
            let m = [b[pos], b[pos + 1], b[pos + 2], b[pos + 3]];
            pos += 4;
            Some(m)
        } else {
            None
        };
        if len > 64 << 20 {
            close(e, i, 1009, "message too big", false);
            return false;
        }
        if b.len() < pos + len {
            return true;
        }
        let mut payload = b[pos..pos + len].to_vec();
        if let Some(m) = mask {
            for (k, x) in payload.iter_mut().enumerate() {
                *x ^= m[k % 4];
            }
        }
        c.buf.drain(..pos + len);
        let id = c.id;
        match op {
            0 | 1 | 2 => {
                let (kind, mut all) = match (op, c.frag.take()) {
                    (0, Some((k, d))) => (k, d),
                    (0, None) => {
                        close(e, i, 1002, "", false);
                        return false;
                    }
                    (k, _) => (k, Vec::new()),
                };
                all.extend_from_slice(&payload);
                if !fin {
                    c.frag = Some((kind, all));
                    continue;
                }
                let m = if kind == 1 {
                    msg(
                        "message",
                        vec![(
                            "text",
                            Json::from(String::from_utf8_lossy(&all).into_owned()),
                        )],
                    )
                } else {
                    msg("message", vec![("b64", Json::from(b64e(&all)))])
                };
                send_js(e, "ws", id, m);
            }
            8 => {
                let code = if payload.len() >= 2 {
                    u16::from_be_bytes([payload[0], payload[1]])
                } else {
                    1005
                };
                let reason = String::from_utf8_lossy(payload.get(2..).unwrap_or(&[])).into_owned();
                if !c.closing {
                    let _ = c
                        .stream
                        .write_all(&frame(8, &payload[..payload.len().min(2)]));
                }
                close(e, i, code, &reason, true);
                return false;
            }
            9 => {
                let _ = c.stream.write_all(&frame(10, &payload));
            }
            _ => {}
        }
    }
}
