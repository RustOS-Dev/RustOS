//! Running a page's scripts: the browser end of the jsd protocol. The
//! page's jsd sends DOM mutations (applied to the entry's document),
//! requests (answered here: fetch with the same-origin policy and CORS,
//! cookies, storage, layout queries, dialogs) and navigation messages
//! (queued as actions for the browser).

use crate::Entry;
use crate::js::Js;
use crate::load::Loader;
use alloc::collections::{BTreeMap, BTreeSet};
use html::NodeId;
use jsproto::{Json, dom};
use rustos_rt::prelude::*;
use rustos_rt::{fs, time};
use webclient::httpc::client::site;
use webclient::httpc::cookie::Context;
use webclient::httpc::{Headers, Request, Url};

/// What the page asked the browser to do.
pub enum Action {
    Navigate { url: Url, replace: bool },
    Submit(Request),
    Go(i32),
    Close,
}

/// Dialogs and messages, shown by the terminal UI or answered in batch
/// mode.
pub trait Ui {
    fn alert(&mut self, text: &str);
    fn confirm(&mut self, text: &str) -> bool;
    fn prompt(&mut self, text: &str, default: &str) -> Option<String>;
}

/// Form control state set by the user or by scripts, overriding the markup.
#[derive(Clone, Default)]
pub struct Override {
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub selected: Option<usize>,
}

pub struct Script {
    pub js: Js,
    pub loaded: bool,
    pub console: Vec<String>,
    pub clickable: BTreeSet<NodeId>,
    pub actions: Vec<Action>,
    /// Element scripts focused (the browser selects it).
    pub focus: Option<NodeId>,
    /// Scroll position scripts asked for (CSS px).
    pub scroll: Option<f32>,
    /// A message for the status line.
    pub notice: Option<String>,
    /// Audio clips and players of the page's media elements.
    pub media: crate::media::Media,
    seq: i64,
}

impl Script {
    pub fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }
}

/// Session storage, per origin, for the life of the browser.
#[derive(Default)]
pub struct Sessions {
    pub map: BTreeMap<String, BTreeMap<String, String>>,
}

// ---- base64 ----

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64e(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        s.push(B64[(n >> 18) as usize] as char);
        s.push(B64[(n >> 12 & 63) as usize] as char);
        s.push(if c.len() > 1 {
            B64[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            B64[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    s
}

pub fn b64d(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut buf, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        buf = buf << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

fn jstr(s: &str) -> Json {
    Json::from(s)
}

fn origin_of(u: &Url) -> String {
    u.origin()
}

// ---- starting ----

/// Start the scripts of an entry's page: spawn jsd and send it the
/// document. `js` is off for non-HTML and generated pages.
pub fn start(e: &mut Entry, referrer: &str, cols: usize, rows: usize) -> bool {
    let Some(js) = Js::spawn() else { return false };
    let tree = dom::serialize_document(&e.doc);
    let init = Json::obj([
        ("t", jstr("init")),
        ("url", jstr(&e.loaded.url.to_string())),
        ("referrer", jstr(referrer)),
        ("tree", tree.get("tree").cloned().unwrap_or(Json::Null)),
        ("next", tree.get("next").cloned().unwrap_or(Json::Null)),
        ("width", Json::from(cols * 8)),
        ("height", Json::from(rows * 16)),
    ]);
    let mut s = Script {
        js,
        loaded: false,
        console: Vec::new(),
        clickable: BTreeSet::new(),
        actions: Vec::new(),
        focus: None,
        scroll: None,
        notice: None,
        media: crate::media::Media::default(),
        seq: 0,
    };
    s.js.send(&init);
    e.script = Some(s);
    true
}

/// Process jsd's messages until `until` accepts one (returned), jsd goes
/// idle with `stop_idle`, or `timeout_ms` passes (-1: no limit).
pub fn pump(
    e: &mut Entry,
    loader: &mut Loader,
    sessions: &mut Sessions,
    ui: &mut dyn Ui,
    until: &mut dyn FnMut(&Json) -> bool,
    stop_idle: bool,
    timeout_ms: i32,
) -> Option<Json> {
    let start = time::millis();
    loop {
        let left = if timeout_ms < 0 {
            -1
        } else {
            (timeout_ms as i64 - (time::millis() - start) as i64).max(0) as i32
        };
        let m = {
            let s = e.script.as_mut()?;
            if s.js.dead {
                return None;
            }
            s.js.recv(left)?
        };
        if until(&m) {
            return Some(m);
        }
        let idle = m.str("t") == Some("idle");
        handle(e, loader, sessions, ui, &m);
        if idle && stop_idle {
            return None;
        }
        if timeout_ms >= 0 && time::millis() - start >= timeout_ms as u64 {
            return None;
        }
    }
}

/// Handle messages already waiting, without blocking.
pub fn drain(e: &mut Entry, loader: &mut Loader, sessions: &mut Sessions, ui: &mut dyn Ui) {
    pump(e, loader, sessions, ui, &mut |_| false, false, 0);
}

fn reply(e: &mut Entry, id: &Json, v: Result<Json, String>) {
    let Some(s) = e.script.as_mut() else { return };
    let m = match v {
        Ok(v) => Json::obj([("t", jstr("reply")), ("id", id.clone()), ("v", v)]),
        Err(err) => Json::obj([
            ("t", jstr("reply")),
            ("id", id.clone()),
            ("error", jstr(&err)),
        ]),
    };
    s.js.send(&m);
}

fn handle(e: &mut Entry, loader: &mut Loader, sessions: &mut Sessions, ui: &mut dyn Ui, m: &Json) {
    match m.str("t").unwrap_or("") {
        "mut" => {
            let ops = m.arr("ops").unwrap_or(&[]);
            let other: Vec<Json> = dom::apply_ops(&mut e.doc, ops)
                .into_iter()
                .cloned()
                .collect();
            for op in other {
                let Some(id) = op.int("id").map(|i| i as NodeId) else {
                    continue;
                };
                let o = e.overrides.entry(id).or_default();
                match op.str("op") {
                    Some("value") => o.value = op.str("value").map(String::from),
                    Some("checked") => o.checked = op.bool("checked"),
                    Some("selected") => {
                        if let Some(i) = op.int("index") {
                            o.selected = (i >= 0).then_some(i as usize);
                        } else if let Some(l) = op.arr("indices") {
                            o.selected = l.first().and_then(|x| x.as_i64()).map(|i| i as usize);
                        }
                    }
                    _ => {}
                }
            }
            e.dirty = true;
        }
        "console" => {
            if let Some(s) = e.script.as_mut() {
                let line = format!(
                    "{}: {}",
                    m.str("level").unwrap_or("log"),
                    m.str("text").unwrap_or("")
                );
                s.console.push(line);
                if s.console.len() > 500 {
                    s.console.remove(0);
                }
            }
        }
        "loaded" => {
            if let Some(s) = e.script.as_mut() {
                s.loaded = true;
            }
        }
        "rpc" => {
            let id = m.get("id").cloned().unwrap_or(Json::Null);
            let a = m.get("a").cloned().unwrap_or(Json::Null);
            let r = rpc(e, loader, sessions, ui, m.str("m").unwrap_or(""), &a);
            reply(e, &id, r);
        }
        "navigate" => {
            let Some(url) = m.str("url").and_then(|u| Url::parse(u).ok()) else {
                return;
            };
            let replace = m.bool("replace").unwrap_or(false);
            if let Some(s) = e.script.as_mut() {
                s.actions.push(Action::Navigate { url, replace });
            }
        }
        "submit" => {
            if let Some(req) = submission(m) {
                if let Some(s) = e.script.as_mut() {
                    s.actions.push(Action::Submit(req));
                }
            }
        }
        "fragment" | "history" => {
            // Same-document navigation (a fragment, pushState).
            if let Some(url) = m.str("url").and_then(|u| Url::parse(u).ok()) {
                if m.str("t") == Some("fragment") {
                    if let Some(f) = &url.fragment {
                        if let Some(&(_, line)) = e.page.anchors.iter().find(|(n, _)| n == f) {
                            e.top = line;
                        }
                    }
                }
                e.loaded.url = url;
            }
        }
        "go" => {
            let d = m.int("delta").unwrap_or(0) as i32;
            if let Some(s) = e.script.as_mut() {
                s.actions.push(Action::Go(d));
            }
        }
        "close" => {
            if let Some(s) = e.script.as_mut() {
                s.actions.push(Action::Close);
            }
        }
        "scroll" => {
            if let Some(s) = e.script.as_mut() {
                s.scroll = m.num("y").map(|y| y as f32);
            }
        }
        "scrollTo" | "focus" => {
            if let Some(s) = e.script.as_mut() {
                s.focus = m.int("id").map(|i| i as NodeId);
            }
        }
        "clickable" => {
            if let Some(s) = e.script.as_mut() {
                s.clickable = m
                    .arr("ids")
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|x| x.as_i64())
                    .map(|x| x as NodeId)
                    .collect();
            }
            e.dirty = true;
        }
        "invalid" => {
            if let Some(s) = e.script.as_mut() {
                s.notice = Some(
                    m.str("message")
                        .unwrap_or("Please check this field.")
                        .to_string(),
                );
                s.focus = m.int("id").map(|i| i as NodeId);
            }
        }
        "storageSet" | "storageClear" => {
            let origin = origin_of(&e.loaded.url);
            let kind = m.str("kind").unwrap_or("local");
            let mut map = if kind == "local" {
                load_storage(&origin)
            } else {
                sessions.map.get(&origin).cloned().unwrap_or_default()
            };
            if m.str("t") == Some("storageClear") {
                map.clear();
            } else if let Some(k) = m.str("k") {
                match m.str("v") {
                    Some(v) => {
                        map.insert(k.to_string(), v.to_string());
                    }
                    None => {
                        map.remove(k);
                    }
                }
            }
            if kind == "local" {
                save_storage(&origin, &map);
            } else {
                sessions.map.insert(origin, map);
            }
        }
        "wsOpen" | "esOpen" => {
            // WebSocket and EventSource connections run in the browser's
            // event loop (see sockets.rs).
            crate::sockets::open(e, loader, m);
        }
        "wsSend" | "wsClose" | "esClose" => crate::sockets::command(e, m),
        "clipboard" | "idle" | "eventDone" | "evalResult" | "unloaded" => {}
        _ => {}
    }
}

/// A form submission from jsd: {action, method, enctype, entries}.
fn submission(m: &Json) -> Option<Request> {
    let url = Url::parse(m.str("action")?).ok()?;
    let method = m.str("method").unwrap_or("get");
    let enctype = m
        .str("enctype")
        .unwrap_or("application/x-www-form-urlencoded");
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut files: Vec<(String, String, String, Vec<u8>)> = Vec::new();
    for en in m.arr("entries").unwrap_or(&[]) {
        let Some(a) = en.as_arr() else { continue };
        let k = a.first().and_then(|x| x.as_str()).unwrap_or("").to_string();
        match a.get(1) {
            Some(Json::Str(v)) => pairs.push((k, v.clone())),
            Some(f @ Json::Obj(_)) => files.push((
                k,
                f.str("name").unwrap_or("").to_string(),
                f.str("type").unwrap_or("").to_string(),
                b64d(f.str("b64").unwrap_or("")),
            )),
            _ => {}
        }
    }
    crate::form::build_request(url, method, enctype, &pairs, &files).ok()
}

// ---- storage files ----

fn storage_dir() -> &'static str {
    if fs::is_dir("/storage") {
        "/storage/var/browser/localstorage"
    } else {
        "/tmp/browser-localstorage"
    }
}

fn storage_file(origin: &str) -> String {
    let name: String = origin
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}/{}.json", storage_dir(), name)
}

pub fn load_storage(origin: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Ok(t) = fs::read_to_string(&storage_file(origin)) {
        if let Ok(Json::Obj(m)) = Json::parse(&t) {
            for (k, v) in m {
                if let Json::Str(s) = v {
                    out.insert(k, s);
                }
            }
        }
    }
    out
}

fn save_storage(origin: &str, map: &BTreeMap<String, String>) {
    let _ = fs::create_dir_all(storage_dir());
    let j = Json::Obj(
        map.iter()
            .map(|(k, v)| (k.clone(), Json::from(v.as_str())))
            .collect(),
    );
    let _ = fs::write(&storage_file(origin), j.to_json().as_bytes());
}

// ---- requests ----

fn rpc(
    e: &mut Entry,
    loader: &mut Loader,
    sessions: &mut Sessions,
    ui: &mut dyn Ui,
    m: &str,
    a: &Json,
) -> Result<Json, String> {
    match m {
        "parseFragment" => Ok(dom::fragment(
            a.str("html").unwrap_or(""),
            a.int("first").unwrap_or(1) as usize,
        )),
        "parseDocument" => Ok(dom::document_tree(
            a.str("html").unwrap_or(""),
            a.int("first").unwrap_or(1) as usize,
        )),
        "fetchScript" => {
            let url = e
                .loaded
                .url
                .join(a.str("url").unwrap_or(""))
                .map_err(|x| x.to_string())?;
            if e.loaded.url.is_secure() && url.scheme == "http" {
                return Ok(Json::obj([("error", jstr("blocked: mixed content"))]));
            }
            let ctx = Context {
                same_site: site(url.host_str()) == site(e.loaded.url.host_str()),
                top_level_safe: false,
            };
            match loader.fetch(Request::get(url), ctx) {
                Ok(l) if l.status < 400 => Ok(Json::obj([("text", Json::from(l.text))])),
                Ok(l) => Ok(Json::obj([(
                    "error",
                    Json::from(format!("HTTP {}", l.status)),
                )])),
                Err(_) => Ok(Json::obj([("error", jstr("network error"))])),
            }
        }
        "fetch" => Ok(fetch(e, loader, a)),
        "audioLoad" => {
            let err = |s: &str| Ok(Json::obj([("error", jstr(s))]));
            let Ok(url) = e.loaded.url.join(a.str("url").unwrap_or("")) else {
                return err("bad URL");
            };
            if e.loaded.url.is_secure() && url.scheme == "http" {
                return err("blocked: mixed content");
            }
            let referrer = e.loaded.url.clone();
            let Some((bytes, _)) = loader.fetch_bytes(&url, &referrer, crate::media::MAX_BYTES)
            else {
                return err("network error");
            };
            let Some(s) = e.script.as_mut() else {
                return err("no script");
            };
            match s.media.add(&bytes) {
                Ok((id, dur)) => Ok(Json::obj([
                    ("id", Json::from(id as i64)),
                    ("duration", Json::from(dur)),
                ])),
                Err(m) => err(m),
            }
        }
        "audioPlay" => {
            let s = e.script.as_mut().ok_or("no script")?;
            s.media
                .play(
                    a.int("key").unwrap_or(0),
                    a.int("id").unwrap_or(-1) as usize,
                    a.num("from").unwrap_or(0.0),
                    a.num("volume").unwrap_or(1.0),
                )
                .map(|_| Json::Null)
                .map_err(String::from)
        }
        "audioStop" => {
            if let Some(s) = e.script.as_mut() {
                s.media.stop(a.int("key").unwrap_or(0));
            }
            Ok(Json::Null)
        }
        "cookie" => {
            let now = time::now();
            let ctx = Context {
                same_site: true,
                top_level_safe: true,
            };
            let v: Vec<String> = loader
                .client
                .jar
                .matching(&e.loaded.url, now, ctx)
                .into_iter()
                .filter(|c| !c.http_only)
                .map(|c| {
                    if c.name.is_empty() {
                        c.value.clone()
                    } else {
                        format!("{}={}", c.name, c.value)
                    }
                })
                .collect();
            Ok(Json::from(v.join("; ")))
        }
        "setCookie" => {
            let v = a.str("v").unwrap_or("");
            // Scripts cannot set HttpOnly cookies.
            if !v
                .split(';')
                .skip(1)
                .any(|p| p.trim().eq_ignore_ascii_case("httponly"))
            {
                loader.client.jar.store(&e.loaded.url, v, time::now());
                loader.save_cookies();
            }
            Ok(Json::Null)
        }
        "storageLoad" => {
            let origin = origin_of(&e.loaded.url);
            let map = if a.str("kind") == Some("session") {
                sessions.map.get(&origin).cloned().unwrap_or_default()
            } else {
                load_storage(&origin)
            };
            Ok(Json::Obj(
                map.into_iter().map(|(k, v)| (k, Json::from(v))).collect(),
            ))
        }
        "alert" => {
            ui.alert(a.str("text").unwrap_or(""));
            Ok(Json::Null)
        }
        "confirm" => Ok(Json::Bool(ui.confirm(a.str("text").unwrap_or("")))),
        "prompt" => Ok(
            match ui.prompt(a.str("text").unwrap_or(""), a.str("def").unwrap_or("")) {
                Some(s) => Json::from(s),
                None => Json::Null,
            },
        ),
        "rects" => {
            crate::relayout(e, loader);
            let id = a.int("id").unwrap_or(0) as NodeId;
            let r = layout::rects_of(&e.page, &e.doc, id);
            Ok(Json::Arr(
                r.iter()
                    .map(|q| Json::Arr(q.iter().map(|&x| Json::from(x as f64)).collect()))
                    .collect(),
            ))
        }
        "hitTest" => {
            crate::relayout(e, loader);
            let (x, y) = (
                a.num("x").unwrap_or(0.0) as f32,
                a.num("y").unwrap_or(0.0) as f32 + e.top as f32 * 16.0,
            );
            Ok(layout::hit_test(&e.page, &e.doc, x, y).map_or(Json::Null, Json::from))
        }
        "computedStyle" => {
            let id = a.int("id").unwrap_or(0) as NodeId;
            let opts = crate::render_opts(e.width, true);
            let v = layout::computed_style(&e.doc, &opts, &e.sheets, id);
            Ok(Json::Obj(
                v.into_iter().map(|(k, v)| (k, Json::from(v))).collect(),
            ))
        }
        "matchMedia" => {
            let dev = layout::cell_device(&crate::render_opts(e.width, true));
            let q = a.str("q").unwrap_or("");
            Ok(Json::Bool(css::media::matches(
                &css::parser::component_values(q),
                &dev,
            )))
        }
        "cssSupports" => {
            let q = a.str("q").unwrap_or("");
            Ok(Json::Bool(css::media::supports(
                &css::parser::component_values(q),
                &css::style::supported,
            )))
        }
        "cssRules" => Ok(Json::Arr(
            split_rules(a.str("text").unwrap_or(""))
                .into_iter()
                .map(Json::from)
                .collect(),
        )),
        _ => Err(format!("unknown request {}", m)),
    }
}

/// Top-level rules of a style sheet, as text.
fn split_rules(css: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut quote) = (0i32, 0usize, None::<char>);
    let b: Vec<(usize, char)> = css.char_indices().collect();
    let mut i = 0;
    while i < b.len() {
        let (pos, c) = b[i];
        if let Some(q) = quote {
            if c == '\\' {
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == '/' && b.get(i + 1).map(|x| x.1) == Some('*') {
            while i + 1 < b.len() && !(b[i].1 == '*' && b[i + 1].1 == '/') {
                i += 1;
            }
            i += 1;
        } else if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                out.push(css[start..pos + 1].trim().to_string());
                start = pos + 1;
            }
        } else if c == ';' && depth == 0 {
            out.push(css[start..pos + 1].trim().to_string());
            start = pos + 1;
        }
        i += 1;
    }
    out.retain(|r| !r.is_empty());
    out
}

/// fetch()/XMLHttpRequest: the same-origin policy, CORS (with preflight
/// for non-simple requests), mixed-content blocking and cookies.
fn fetch(e: &mut Entry, loader: &mut Loader, a: &Json) -> Json {
    let err = |s: &str| Json::obj([("error", jstr(s))]);
    let Some(url) = a.str("url").and_then(|u| Url::parse(u).ok()) else {
        return err("bad URL");
    };
    let page = e.loaded.url.clone();
    if page.is_secure() && url.scheme == "http" {
        return err("mixed content");
    }
    let method = a.str("method").unwrap_or("GET").to_string();
    let mode = a.str("mode").unwrap_or("cors");
    let creds = a.str("credentials").unwrap_or("same-origin");
    let origin = origin_of(&page);
    let same_origin = origin_of(&url) == origin;
    if !same_origin && mode == "same-origin" {
        return err("cross-origin request in same-origin mode");
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    for h in a.arr("headers").unwrap_or(&[]) {
        if let Some(p) = h.as_arr() {
            headers.push((
                p.first().and_then(|x| x.as_str()).unwrap_or("").to_string(),
                p.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string(),
            ));
        }
    }
    let body = a.str("body").map(b64d).unwrap_or_default();
    let cors = !same_origin && mode == "cors";
    let send_cookies = creds == "include" || (creds == "same-origin" && same_origin);
    // CORS preflight for non-simple requests.
    if cors {
        let simple_method = matches!(method.as_str(), "GET" | "HEAD" | "POST");
        let simple_headers = headers.iter().all(|(k, v)| {
            let k = k.to_ascii_lowercase();
            matches!(
                k.as_str(),
                "accept" | "accept-language" | "content-language"
            ) || (k == "content-type" && {
                let t = v
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                matches!(
                    t.as_str(),
                    "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
                )
            })
        });
        if !simple_method || !simple_headers {
            let mut pre = Request::get(url.clone());
            pre.method = String::from("OPTIONS");
            pre.headers.set("Origin", &origin);
            pre.headers.set("Access-Control-Request-Method", &method);
            let names: Vec<String> = headers.iter().map(|h| h.0.to_ascii_lowercase()).collect();
            if !names.is_empty() {
                pre.headers
                    .set("Access-Control-Request-Headers", &names.join(","));
            }
            let saved = loader.client.opts.cookies;
            loader.client.opts.cookies = false;
            let r = loader.client.send(
                pre,
                Context {
                    same_site: false,
                    top_level_safe: false,
                },
            );
            loader.client.opts.cookies = saved;
            let ok = match &r {
                Ok(r) => {
                    r.head.status < 300
                        && cors_allows(&r.head.headers, &origin, creds == "include")
                        && {
                            let allowed = r
                                .head
                                .headers
                                .get("Access-Control-Allow-Methods")
                                .unwrap_or("")
                                .to_ascii_uppercase();
                            simple_method
                                || allowed
                                    .split(',')
                                    .any(|m| m.trim() == method || m.trim() == "*")
                        }
                        && {
                            let allowed = r
                                .head
                                .headers
                                .get("Access-Control-Allow-Headers")
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            names.iter().all(|n| {
                                allowed.split(',').any(|x| x.trim() == n || x.trim() == "*")
                                    || matches!(
                                        n.as_str(),
                                        "accept"
                                            | "accept-language"
                                            | "content-language"
                                            | "content-type"
                                    )
                            })
                        }
                }
                Err(_) => false,
            };
            if !ok {
                return err("CORS preflight failed");
            }
        }
    }
    let mut req = Request::get(url.clone());
    req.method = method.clone();
    for (k, v) in &headers {
        req.headers.set(k, v);
    }
    if !same_origin {
        req.headers.set("Origin", &origin);
    }
    if method != "GET" && method != "HEAD" {
        req.body = body;
    }
    if let Some(r) = a.str("referrer") {
        if !r.is_empty() && !(page.is_secure() && url.scheme == "http") {
            req.headers.set(
                "Referer",
                &Url::parse(r)
                    .map(|u| u.without_fragment().to_string())
                    .unwrap_or_default(),
            );
        }
    }
    let follow = a.str("redirect").unwrap_or("follow") != "manual";
    let (saved_cookies, saved_follow) = (
        loader.client.opts.cookies,
        loader.client.opts.follow_redirects,
    );
    loader.client.opts.cookies = send_cookies;
    loader.client.opts.follow_redirects = follow;
    let ctx = Context {
        same_site: site(url.host_str()) == site(page.host_str()),
        top_level_safe: false,
    };
    let r = loader.client.send(req, ctx);
    loader.client.opts.cookies = saved_cookies;
    loader.client.opts.follow_redirects = saved_follow;
    let r = match r {
        Ok(r) => r,
        Err(x) => return err(&x.to_string()),
    };
    if send_cookies && r.head.headers.contains("Set-Cookie") {
        loader.save_cookies();
    }
    let final_same = origin_of(&r.url) == origin;
    let mut ty = "basic";
    if !final_same {
        if mode == "no-cors" {
            // Opaque: nothing is visible to the page.
            return Json::obj([
                ("status", Json::from(0usize)),
                ("type", jstr("opaque")),
                ("headers", Json::Arr(Vec::new())),
                ("body", jstr("")),
                ("url", jstr("")),
            ]);
        }
        if !cors_allows(&r.head.headers, &origin, creds == "include") {
            return err("CORS: response not allowed");
        }
        ty = "cors";
    }
    let visible = |k: &str| {
        let k = k.to_ascii_lowercase();
        if k == "set-cookie" || k == "set-cookie2" {
            return false;
        }
        if ty == "basic" {
            return true;
        }
        let exposed = r
            .head
            .headers
            .get("Access-Control-Expose-Headers")
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(
            k.as_str(),
            "cache-control"
                | "content-language"
                | "content-length"
                | "content-type"
                | "expires"
                | "last-modified"
                | "pragma"
        ) || exposed.split(',').any(|x| x.trim() == k || x.trim() == "*")
    };
    let hs: Vec<Json> = r
        .head
        .headers
        .0
        .iter()
        .filter(|(k, _)| visible(k))
        .map(|(k, v)| {
            Json::Arr(vec![
                Json::from(k.to_ascii_lowercase()),
                Json::from(v.as_str()),
            ])
        })
        .collect();
    Json::obj([
        ("status", Json::from(r.head.status as usize)),
        ("statusText", Json::from(r.head.reason.as_str())),
        ("url", Json::from(r.url.to_string())),
        ("redirected", Json::Bool(!r.redirects.is_empty())),
        ("type", jstr(ty)),
        ("headers", Json::Arr(hs)),
        ("body", Json::from(b64e(&r.body))),
    ])
}

fn cors_allows(h: &Headers, origin: &str, credentials: bool) -> bool {
    let acao = h.get("Access-Control-Allow-Origin").unwrap_or("").trim();
    if credentials {
        acao == origin
            && h.get("Access-Control-Allow-Credentials")
                .is_some_and(|v| v.trim() == "true")
    } else {
        acao == "*" || acao == origin
    }
}

/// Send a user event to the page's scripts and wait for the result:
/// Some(true) if a handler cancelled it; None without scripts.
pub fn event(
    e: &mut Entry,
    loader: &mut Loader,
    sessions: &mut Sessions,
    ui: &mut dyn Ui,
    kind: &str,
    id: NodeId,
) -> Option<bool> {
    let s = e.script.as_mut()?;
    if s.js.dead {
        return None;
    }
    let seq = s.next_seq();
    s.js.send(&Json::obj([
        ("t", jstr("event")),
        ("seq", Json::from(seq)),
        ("type", jstr(kind)),
        ("id", Json::from(id)),
    ]));
    let r = pump(
        e,
        loader,
        sessions,
        ui,
        &mut |m| m.str("t") == Some("eventDone") && m.int("seq") == Some(seq),
        false,
        15_000,
    )?;
    // Let the task's mutations arrive.
    pump(e, loader, sessions, ui, &mut |_| false, true, 200);
    Some(r.bool("cancelled").unwrap_or(false))
}

/// The user changed a control's value.
pub fn input(e: &mut Entry, id: NodeId, what: Json) {
    let Some(s) = e.script.as_mut() else { return };
    let mut m = match what {
        Json::Obj(o) => o,
        _ => return,
    };
    m.insert(String::from("t"), jstr("input"));
    m.insert(String::from("id"), Json::from(id));
    s.js.send(&Json::Obj(m));
}

/// Evaluate code in the page (the `J` console).
pub fn eval(
    e: &mut Entry,
    loader: &mut Loader,
    sessions: &mut Sessions,
    ui: &mut dyn Ui,
    code: &str,
) -> String {
    let Some(s) = e.script.as_mut() else {
        return String::from("(scripts are not running)");
    };
    let seq = s.next_seq();
    s.js.send(&Json::obj([
        ("t", jstr("eval")),
        ("seq", Json::from(seq)),
        ("code", jstr(code)),
    ]));
    match pump(
        e,
        loader,
        sessions,
        ui,
        &mut |m| m.str("t") == Some("evalResult") && m.int("seq") == Some(seq),
        false,
        15_000,
    ) {
        Some(r) => r.str("v").unwrap_or("").to_string(),
        None => String::from("(no answer)"),
    }
}

/// Tell the page it is being left (beforeunload/pagehide/unload).
pub fn unload(e: &mut Entry, loader: &mut Loader, sessions: &mut Sessions, ui: &mut dyn Ui) {
    let Some(s) = e.script.as_mut() else { return };
    let seq = s.next_seq();
    s.js.send(&Json::obj([
        ("t", jstr("unload")),
        ("seq", Json::from(seq)),
    ]));
    pump(
        e,
        loader,
        sessions,
        ui,
        &mut |m| m.str("t") == Some("unloaded"),
        false,
        1000,
    );
}
