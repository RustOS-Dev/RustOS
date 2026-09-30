//! End-to-end tests of `jsd` against a minimal browser: the page is parsed
//! with `crates/html`, sent to a host build of jsd, and its mutation
//! batches are applied back. Set `JSD_BIN` to the host binary
//! (userland/jsd/host-build.sh); without it the tests are skipped.

use html::{Document, NodeKind};
use jsproto::{Json, dom};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Page {
    doc: Document,
    child: Child,
    tx: ChildStdin,
    rx: BufReader<ChildStdout>,
    net: HashMap<String, (u16, String, String)>,
    console: Vec<String>,
    other: Vec<Json>,
    alerts: Vec<String>,
    storage: HashMap<String, String>,
    cookies: String,
    loaded: bool,
    seq: i64,
}

fn jsd() -> Option<String> {
    std::env::var("JSD_BIN").ok().filter(|p| std::path::Path::new(p).exists())
}

impl Page {
    fn open(src: &str, net: &[(&str, &str)]) -> Page {
        let doc = html::parse(src);
        let mut child = Command::new(jsd().unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn jsd");
        let tx = child.stdin.take().unwrap();
        let rx = BufReader::new(child.stdout.take().unwrap());
        let net = net
            .iter()
            .map(|(u, b)| {
                let ct = if u.ends_with(".js") || u.ends_with(".mjs") { "text/javascript" } else if u.ends_with(".json") { "application/json" } else { "text/html" };
                (u.to_string(), (200, ct.to_string(), b.to_string()))
            })
            .collect();
        let mut p = Page { doc, child, tx, rx, net, console: Vec::new(), other: Vec::new(), alerts: Vec::new(), storage: HashMap::new(), cookies: String::new(), loaded: false, seq: 0 };
        let tree = dom::serialize_document(&p.doc);
        let init = Json::obj([
            ("t", Json::from("init")),
            ("url", Json::from("http://test.example/dir/page.html")),
            ("tree", tree.get("tree").unwrap().clone()),
            ("next", tree.get("next").unwrap().clone()),
            ("width", Json::from(640usize)),
            ("height", Json::from(400usize)),
        ]);
        p.send(&init);
        p.run_until(|p| p.loaded);
        p
    }

    fn send(&mut self, m: &Json) {
        writeln!(self.tx, "{}", m.to_json()).unwrap();
        self.tx.flush().unwrap();
    }

    fn reply(&mut self, id: &Json, v: Json) {
        let m = Json::obj([("t", Json::from("reply")), ("id", id.clone()), ("v", v)]);
        self.send(&m);
    }

    /// Process messages until `done` holds at an idle point.
    fn run_until(&mut self, done: impl Fn(&Page) -> bool) {
        let start = std::time::Instant::now();
        loop {
            assert!(start.elapsed().as_secs() < 20, "timeout; console: {:?}", self.console);
            let mut line = String::new();
            if self.rx.read_line(&mut line).unwrap() == 0 {
                panic!("jsd exited; console: {:?}", self.console);
            }
            let m = Json::parse(line.trim()).unwrap_or_else(|e| panic!("bad line {line:?}: {e:?}"));
            match m.str("t") {
                Some("idle") => {
                    if done(self) {
                        return;
                    }
                }
                Some("mut") => {
                    let ops = m.arr("ops").unwrap().to_vec();
                    let rest: Vec<Json> = dom::apply_ops(&mut self.doc, &ops).into_iter().cloned().collect();
                    self.other.extend(rest);
                }
                Some("console") => self.console.push(format!("{}: {}", m.str("level").unwrap_or(""), m.str("text").unwrap_or(""))),
                Some("loaded") => self.loaded = true,
                Some("rpc") => self.rpc(&m),
                Some("storageSet") => {
                    let k = m.str("k").unwrap().to_string();
                    match m.str("v") {
                        Some(v) => self.storage.insert(k, v.to_string()),
                        None => self.storage.remove(&k),
                    };
                }
                _ => self.other.push(m),
            }
        }
    }

    fn rpc(&mut self, m: &Json) {
        let id = m.get("id").unwrap().clone();
        let a = m.get("a").cloned().unwrap_or(Json::Null);
        let v = match m.str("m").unwrap() {
            "parseFragment" => dom::fragment(a.str("html").unwrap(), a.int("first").unwrap() as usize),
            "parseDocument" => dom::document_tree(a.str("html").unwrap(), a.int("first").unwrap() as usize),
            "fetchScript" => match self.net.get(a.str("url").unwrap()) {
                Some((_, _, body)) => Json::obj([("text", Json::from(body.as_str()))]),
                None => Json::obj([("error", Json::from("404"))]),
            },
            "fetch" => {
                let url = a.str("url").unwrap().to_string();
                let method = a.str("method").unwrap_or("GET").to_string();
                let body = a.str("body").map(|b| String::from_utf8(b64d(b)).unwrap()).unwrap_or_default();
                let key = if method == "GET" { url.clone() } else { format!("{method} {url}") };
                match self.net.get(&key) {
                    Some((st, ct, text)) => {
                        let text = text.replace("{body}", &body);
                        Json::obj([
                            ("status", Json::from(*st as usize)),
                            ("statusText", Json::from("OK")),
                            ("url", Json::from(url.as_str())),
                            ("headers", Json::Arr(vec![Json::Arr(vec![Json::from("content-type"), Json::from(ct.as_str())])])),
                            ("body", Json::from(b64e(text.as_bytes()).as_str())),
                        ])
                    }
                    None => Json::obj([("status", Json::from(404usize)), ("url", Json::from(url.as_str())), ("headers", Json::Arr(vec![])), ("body", Json::from(""))]),
                }
            }
            "cookie" => Json::from(self.cookies.as_str()),
            "setCookie" => {
                let c = a.str("v").unwrap().split(';').next().unwrap().to_string();
                if !self.cookies.is_empty() {
                    self.cookies.push_str("; ");
                }
                self.cookies.push_str(&c);
                Json::Null
            }
            "storageLoad" => Json::Obj(self.storage.iter().map(|(k, v)| (k.clone(), Json::from(v.as_str()))).collect()),
            "alert" => {
                self.alerts.push(a.str("text").unwrap().to_string());
                Json::Null
            }
            "confirm" => Json::Bool(true),
            "prompt" => Json::from("typed"),
            "rects" => Json::Arr(vec![Json::Arr(vec![Json::from(8usize), Json::from(16usize), Json::from(100usize), Json::from(16usize)])]),
            "computedStyle" => Json::obj([("display", Json::from("block"))]),
            "matchMedia" => Json::Bool(false),
            other => panic!("unexpected rpc {other}"),
        };
        self.reply(&id, v);
    }

    fn eval(&mut self, code: &str) -> String {
        self.seq += 1;
        let seq = self.seq;
        self.send(&Json::obj([("t", Json::from("eval")), ("seq", Json::from(seq as usize)), ("code", Json::from(code))]));
        let mut out = None;
        self.run_until(|p| p.other.iter().any(|m| m.str("t") == Some("evalResult") && m.int("seq") == Some(seq)));
        self.other.retain(|m| {
            if m.str("t") == Some("evalResult") && m.int("seq") == Some(seq) {
                out = m.str("v").map(String::from);
                false
            } else {
                true
            }
        });
        out.unwrap_or_default()
    }

    /// Let timers run for `ms` milliseconds.
    fn wait(&mut self, ms: u64) {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        self.eval(&format!("new Promise(r => setTimeout(r, {ms})), 0"));
        while std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.eval("0");
    }

    fn text(&self) -> String {
        let mut s = String::new();
        for n in self.doc.descendants(0) {
            if let NodeKind::Text(t) = &self.doc.nodes[n].kind {
                s.push_str(t);
            }
        }
        s
    }

    fn html_of(&self, tag: &str) -> String {
        let n = self.doc.find(tag).unwrap_or_else(|| panic!("no {tag}: {} {:?}", dom::serialize_document(&self.doc).to_json(), self.console));
        dom::serialize_node(&self.doc, n).to_json()
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn b64e(b: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize] as char);
        s.push(T[(n >> 12 & 63) as usize] as char);
        s.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        s.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    s
}

fn b64d(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
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

macro_rules! need_jsd {
    () => {
        if jsd().is_none() {
            eprintln!("JSD_BIN not set; skipping");
            return;
        }
    };
}

#[test]
fn renders_content_from_script() {
    need_jsd!();
    let mut p = Page::open(
        r#"<body><div id=app></div><script>
            const app = document.getElementById("app");
            const ul = document.createElement("ul");
            for (const f of ["apple", "pear"]) { const li = document.createElement("li"); li.textContent = f; li.className = "fruit"; ul.appendChild(li); }
            app.appendChild(ul);
            app.insertAdjacentHTML("beforeend", "<p class=note>two <b>fruits</b></p>");
            document.title = "Made by JS";
        </script>"#,
        &[],
    );
    assert!(p.text().contains("apple"), "{} {:?}", p.text(), p.console);
    assert!(p.html_of("ul").contains("\"li\",[[\"class\",\"fruit\"]]"), "{}", p.html_of("ul"));
    assert!(p.html_of("p").contains("fruits"));
    assert_eq!(p.eval("document.title"), "Made by JS");
    assert!(p.text().contains("Made by JS"));
    assert_eq!(p.eval("document.querySelectorAll('li.fruit').length"), "2");
    assert_eq!(p.eval("document.querySelector('#app > ul li:last-child').textContent"), "pear");
    assert_eq!(p.eval("app.innerHTML"), "<ul><li class=\"fruit\">apple</li><li class=\"fruit\">pear</li></ul><p class=\"note\">two <b>fruits</b></p>");
    assert!(p.console.is_empty(), "{:?}", p.console);
}

#[test]
fn script_order_write_and_events() {
    need_jsd!();
    let mut p = Page::open(
        r#"<head><script src="a.js"></script><script defer src="d.js"></script><script async src="as.js"></script></head>
        <body><script>log.push("inline"); document.write("<p id=w>written</p><script>log.push('w-script')<\/script>");</script>
        <script type="module">import {v} from "./m.mjs"; log.push("module " + v);</script>
        <script>document.addEventListener("DOMContentLoaded", () => log.push("dcl")); addEventListener("load", () => log.push("load"));</script>"#,
        &[
            ("http://test.example/dir/a.js", "var log = ['a'];"),
            ("http://test.example/dir/d.js", "log.push('defer ' + document.readyState)"),
            ("http://test.example/dir/as.js", "log.push('async')"),
            ("http://test.example/dir/m.mjs", "export const v = 42;"),
        ],
    );
    let log = p.eval("log.join(',')");
    assert!(log.starts_with("a,inline,w-script,"), "{log}");
    assert!(log.contains("defer interactive"), "{log}");
    assert!(log.contains("module 42"), "{log}");
    assert!(log.contains("dcl") && log.ends_with("load"), "{log}");
    assert!(log.contains("async"), "{log}");
    assert!(p.text().contains("written"));
    assert_eq!(p.eval("document.readyState"), "complete");
}

#[test]
fn forms_validation_and_submit() {
    need_jsd!();
    let mut p = Page::open(
        r#"<form id=f action="/login" method=post onsubmit="return check(this)">
            <input name=user required><input type=email name=mail value="bad"><input type=checkbox name=keep checked>
            <select name=s><option>a<option selected>b</select><button id=b>Go</button></form>
            <script>function check(f) { log.push("submit:" + f.user.value); return f.checkValidity(); } var log = [];</script>"#,
        &[],
    );
    assert_eq!(p.eval("f.elements.length"), "5");
    assert_eq!(p.eval("f.s.value + f.s.selectedIndex"), "b1");
    assert_eq!(p.eval("f.mail.validity.typeMismatch"), "true");
    assert_eq!(p.eval("f.user.validity.valueMissing"), "true");
    // Clicking the button: invalid, so no submission.
    let id = p.eval("b.__id");
    p.send(&Json::parse(&format!(r#"{{"t":"event","seq":1,"type":"click","id":{id}}}"#)).unwrap());
    p.run_until(|p| p.other.iter().any(|m| m.str("t") == Some("eventDone")));
    assert!(!p.other.iter().any(|m| m.str("t") == Some("submit")));
    assert!(p.other.iter().any(|m| m.str("t") == Some("invalid")));
    // The user types; then submit works.
    let uid = p.eval("f.user.__id");
    p.send(&Json::parse(&format!(r#"{{"t":"input","id":{uid},"value":"ann"}}"#)).unwrap());
    p.eval("f.mail.value = 'ann@example.com'");
    p.other.clear();
    p.send(&Json::parse(&format!(r#"{{"t":"event","seq":2,"type":"click","id":{id}}}"#)).unwrap());
    p.run_until(|p| p.other.iter().any(|m| m.str("t") == Some("eventDone")));
    let sub = p.other.iter().find(|m| m.str("t") == Some("submit")).expect("submit");
    assert_eq!(sub.str("method"), Some("post"));
    assert_eq!(sub.str("action"), Some("http://test.example/login"));
    let e = sub.get("entries").unwrap().to_json();
    assert_eq!(e, r#"[["user","ann"],["mail","ann@example.com"],["keep","on"],["s","b"]]"#);
    assert_eq!(p.eval("log.join()"), "submit:ann");
}

#[test]
fn fetch_xhr_storage_timers() {
    need_jsd!();
    let mut p = Page::open(
        r#"<div id=out></div><script>
        var got = [];
        fetch("/api/data.json").then(r => r.json()).then(j => { got.push("fetch " + j.n); out.textContent = "n=" + j.n; });
        const x = new XMLHttpRequest(); x.open("GET", "/api/data.json"); x.onload = () => got.push("xhr " + JSON.parse(x.responseText).n); x.send();
        const s = new XMLHttpRequest(); s.open("GET", "/api/data.json", false); s.send(); got.push("sync " + s.status);
        fetch("/api/echo", {method: "POST", body: JSON.stringify({a: 1}), headers: {"Content-Type": "application/json"}}).then(r => r.text()).then(t => got.push("post " + t));
        localStorage.setItem("k", "v1");
        setTimeout(() => got.push("timer"), 30);
        </script>"#,
        &[("http://test.example/api/data.json", r#"{"n":7}"#), ("POST http://test.example/api/echo", "echo:{body}")],
    );
    p.wait(80);
    let got = p.eval("got.sort().join('|')");
    assert_eq!(got, r#"fetch 7|post echo:{"a":1}|sync 200|timer|xhr 7"#);
    assert!(p.text().contains("n=7"));
    assert_eq!(p.storage.get("k").map(String::as_str), Some("v1"));
    assert_eq!(p.eval("localStorage.k + localStorage.length"), "v11");
}

#[test]
fn location_and_history() {
    need_jsd!();
    let mut p = Page::open(r#"<a id=l href="next.html?x=1#top">go</a><script>var h = []; addEventListener("hashchange", () => h.push(location.hash));</script>"#, &[]);
    assert_eq!(p.eval("location.pathname + location.search"), "/dir/page.html");
    assert_eq!(p.eval("l.href"), "http://test.example/dir/next.html?x=1#top");
    p.eval("location.hash = 'sec'");
    p.wait(20);
    assert_eq!(p.eval("h.join()"), "#sec");
    p.eval("history.pushState({a:1}, '', '/other?q')");
    assert_eq!(p.eval("location.href + ' ' + history.state.a"), "http://test.example/other?q 1");
    let id = p.eval("l.__id");
    p.send(&Json::parse(&format!(r#"{{"t":"event","seq":3,"type":"click","id":{id}}}"#)).unwrap());
    p.run_until(|p| p.other.iter().any(|m| m.str("t") == Some("eventDone")));
    let nav = p.other.iter().find(|m| m.str("t") == Some("navigate")).expect("navigate");
    // (relative to the URL set by pushState)
    assert_eq!(nav.str("url"), Some("http://test.example/next.html?x=1#top"));
}

#[test]
fn web_apis() {
    need_jsd!();
    let mut p = Page::open("<p>x</p>", &[]);
    let checks = [
        ("new URL('../a/./b?c=d e#f', 'http://ex.com/x/y/z').href", "http://ex.com/x/a/b?c=d%20e#f"),
        ("new URL('http://[::1]:80/').host", "[::1]"),
        ("new URL('http://0x7f.1/').hostname", "127.0.0.1"),
        ("new URL('https://bücher.de/').hostname", "xn--bcher-kva.de"),
        ("new URLSearchParams('a=1&b=2+3&a=4').getAll('a').join() + new URLSearchParams({x: 'y z'})", "1,4x=y+z"),
        ("btoa('hello') + atob('aGk=')", "aGVsbG8=hi"),
        ("new TextDecoder().decode(new TextEncoder().encode('héllo €'))", "héllo €"),
        ("crypto.randomUUID().length", "36"),
        ("typeof structuredClone(new Map([[1, {a: [1, 2]}]])).get(1).a", "object"),
        ("document.createElement('div').classList.toggle('x')", "true"),
        ("(() => { const d = document.createElement('div'); d.style.backgroundColor = 'red'; d.dataset.fooBar = '1'; return d.outerHTML; })()", "<div style=\"background-color: red;\" data-foo-bar=\"1\"></div>"),
        ("new DOMParser().parseFromString('<p>a<b>b</b></p>', 'text/html').body.innerHTML", "<p>a<b>b</b></p>"),
        ("new Intl.NumberFormat().format(1234567.891)", "1,234,567.891"),
        ("JSON.stringify([...new Headers({'X-A': '1', b: '2'})])", "[[\"b\",\"2\"],[\"x-a\",\"1\"]]"),
        ("document.querySelector('p:is(.a, p):not(:empty)').tagName", "P"),
        ("(() => { let n = 0; const e = document.body; e.addEventListener('x', () => n++, {once: true}); e.dispatchEvent(new Event('x')); e.dispatchEvent(new Event('x')); return n; })()", "1"),
    ];
    for (code, want) in checks {
        assert_eq!(p.eval(code), want, "{code}");
    }
    // SHA-256 of "abc".
    p.eval("crypto.subtle.digest('SHA-256', new TextEncoder().encode('abc')).then(b => window.hash = Array.from(new Uint8Array(b), x => x.toString(16).padStart(2, '0')).join(''))");
    assert_eq!(p.eval("hash"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    p.eval("crypto.subtle.digest('SHA-512', new TextEncoder().encode('abc')).then(b => window.h5 = Array.from(new Uint8Array(b), x => x.toString(16).padStart(2, '0')).join('').slice(0, 16))");
    assert_eq!(p.eval("h5"), "ddaf35a193617aba");
    p.eval("crypto.subtle.digest('SHA-1', new TextEncoder().encode('abc')).then(b => window.h1 = Array.from(new Uint8Array(b), x => x.toString(16).padStart(2, '0')).join(''))");
    assert_eq!(p.eval("h1"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert!(p.console.is_empty(), "{:?}", p.console);
}

#[test]
fn mutation_observer_and_custom_elements() {
    need_jsd!();
    let mut p = Page::open(
        r#"<div id=root></div><x-greet name=Bo></x-greet><script>
        var recs = [];
        new MutationObserver(l => l.forEach(r => recs.push(r.type + ":" + (r.attributeName || r.addedNodes.length)))).observe(root, {childList: true, attributes: true, subtree: true});
        root.setAttribute("data-a", "1"); root.append("t");
        customElements.define("x-greet", class extends HTMLElement {
          static observedAttributes = ["name"];
          connectedCallback() { this.textContent = "Hi " + this.getAttribute("name"); }
          attributeChangedCallback(n, o, v) { if (this.isConnected) this.textContent = "Hi " + v; }
        });
        </script>"#,
        &[],
    );
    assert_eq!(p.eval("recs.join()"), "attributes:data-a,childList:1");
    assert!(p.text().contains("Hi Bo"), "{}", p.text());
    p.eval("document.querySelector('x-greet').setAttribute('name', 'Al')");
    assert!(p.text().contains("Hi Al"));
    assert_eq!(p.eval("(() => { const e = document.createElement('x-greet'); e.setAttribute('name', 'Cy'); document.body.append(e); return e.textContent; })()"), "Hi Cy");
}

#[test]
fn canvas_2d() {
    need_jsd!();
    let mut p = Page::open("<canvas id=c width=20 height=10></canvas>", &[]);
    p.eval("window.x = c.getContext('2d'); x.fillStyle = 'rgb(255,0,0)'; x.fillRect(0, 0, 10, 10); x.fillStyle = '#00f'; x.beginPath(); x.arc(15, 5, 4, 0, Math.PI * 2); x.fill(); 0");
    assert_eq!(p.eval("Array.from(x.getImageData(5, 5, 1, 1).data).join()"), "255,0,0,255");
    assert_eq!(p.eval("Array.from(x.getImageData(15, 5, 1, 1).data).join()"), "0,0,255,255");
    assert_eq!(p.eval("Array.from(x.getImageData(19, 0, 1, 1).data).join()"), "0,0,0,0");
    assert!(p.eval("c.toDataURL().slice(0, 22)").starts_with("data:image/bmp;base64,"));
    assert!(p.console.is_empty(), "{:?}", p.console);
}
