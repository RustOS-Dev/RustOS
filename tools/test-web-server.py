#!/usr/bin/env python3
"""Test web server for the browser and captive-portal scenarios.

Usage: test-web-server.py PORT LOGFILE

Routes:
  /                 index: headings, list, table, links, a GET search form
  /login  (GET)     login form; sets cookie sid=abc
  /login  (POST)    needs cookie sid=abc and user=alice, pass=secret;
                    302 to /welcome with cookie auth=1
  /welcome          needs cookie auth=1; gzip-compressed when accepted
  /search?q=...     echoes the query
  /refresh          meta refresh (1 s) to /welcome
  /generate_204     204 once logged in to the portal, else 302 to /portal
  /portal (GET)     captive-portal page: terms check box + e-mail
  /portal (POST)    accepts terms=on -> logged in
  /css              a page styled by /site.css (which @imports /base.css):
                    flex navigation, a grid, hidden content, media query
  /js/...           JavaScript pages (see JS_PAGES): rendering, a fetch
                    login, XHR, localStorage, timers, document.write,
                    modules, WebSocket echo (/js/echo), EventSource
                    (/js/events), click handlers, a script-only portal
  /gfx              a page for the graphical browser: colored boxes, a
                    gradient, a PNG image (/gfx/blue.png), a script canvas
  PUT /upload/NAME  stores the body in UPLOAD_DIR (default /tmp/rustos-gfx)
With --js-portal, /portal is a login page that works only with scripts.
Every request is appended to LOGFILE as "METHOD PATH | cookie | body".
"""
import base64
import gzip
import hashlib
import http.server
import json
import struct
import sys
import time
import urllib.parse

PORT = int(sys.argv[1])
LOG = sys.argv[2]
JS_PORTAL = "--js-portal" in sys.argv[3:]
UPLOAD_DIR = "/tmp/rustos-gfx"
state = {"portal_ok": False}

INDEX = """<!DOCTYPE html>
<html><head><title>RustOS test site</title></head><body>
<h1>Welcome to the test site</h1>
<p>This page checks <b>text layout</b> in the RustOS browser.
<ul><li>First item<li>Second item</ul>
<table><tr><th>Name<th>Size</tr><tr><td>kernel<td>4 MiB</tr><tr><td>shell<td>200 KiB</tr></table>
<p><a href="/login">Log in</a> or <a href="/refresh">see a refresh</a>.
<form action="/search"><input name=q value="rust os"><input type=submit value=Search></form>
</body></html>"""

LOGIN = """<html><head><title>Login</title></head><body>
<h1>Login</h1>
<form method="post" action="/login">
User: <input name="user" size="12">
Password: <input type="password" name="pass" size="12">
<input type="hidden" name="token" value="t0k">
<label><input type="checkbox" name="remember"> Remember me</label>
<input type="submit" value="Log in">
</form></body></html>"""

PORTAL = """<html><head><title>Free Wi-Fi</title></head><body>
<h1>Welcome to Free Wi-Fi</h1>
<form method="post" action="/portal">
E-mail: <input name="email" size="20">
<label><input type="checkbox" name="terms"> I accept the terms</label>
<input type="submit" value="Connect">
</form></body></html>"""


CSS_PAGE = """<!DOCTYPE html>
<html><head><title>Styled</title><link rel="stylesheet" href="/site.css">
<style>.inline-hidden { display: none }</style></head><body>
<nav class="top"><a href="/">Home</a><a href="/login">Login</a><a href="/refresh">News</a></nav>
<div class="cards"><div class="card">Alpha card</div><div class="card">Beta card</div><div class="card">Gamma card</div></div>
<p class="secret">SECRET-TEXT</p><p class="inline-hidden">INLINE-HIDDEN</p>
<p class="wide-only">WIDE-ONLY</p><p class="narrow-only">NARROW-ONLY</p>
<p class="upper">shouting text</p><p>END-OF-PAGE</p>
</body></html>"""

SITE_CSS = """@import url("/base.css");
nav.top { display: flex; gap: 3ch; }
.cards { display: grid; grid-template-columns: repeat(3, 1fr); gap: 1ch; }
.narrow-only { display: none }
@media (max-width: 500px) { .wide-only { display: none } .narrow-only { display: block } }
.upper { text-transform: uppercase }
"""

BASE_CSS = """.secret { display: none }"""

JS_PAGES = {
    # <audio>: an MP3 clip (0.5 s of 1 kHz) through new Audio(), and a
    # missing one.
    "/js/audio": """<!DOCTYPE html><title>JS audio</title><p id=s>log:</p>
<script>
const log = (m) => { document.getElementById("s").textContent += " " + m; };
const a = new Audio("/js/tone.mp3");
log("can=" + a.canPlayType("audio/mpeg") + "/" + (a.canPlayType("video/webm") || "no"));
a.addEventListener("loadedmetadata", () => log("dur=" + a.duration.toFixed(1)));
a.addEventListener("ended", () => log("ENDED paused=" + a.paused + " t=" + a.currentTime.toFixed(1)));
a.play().then(() => log("PLAYING"), (e) => log("ERR " + e.name));
const b = document.createElement("audio");
b.src = "/js/missing.mp3";
b.play().catch((e) => log("missing=" + e.name + "/" + (b.error && b.error.code)));
</script>""",
    "/js/render": """<!DOCTYPE html><title>JS render</title><div id=app>LOADING</div>
<noscript>NOSCRIPT-SHOWN</noscript>
<script>
const items = ["alpha", "beta", "gamma"];
document.getElementById("app").innerHTML = "<h2>Rendered by JS</h2><ul>" + items.map(i => "<li>item " + i).join("") + "</ul>";
const p = document.createElement("p");
p.textContent = "Count: " + document.querySelectorAll("#app li").length;
document.body.appendChild(p);
document.title = "JS title " + (6 * 7);
</script>""",
    "/js/login": """<!DOCTYPE html><title>JS login</title><h1>Script login</h1>
<form id=f action="/nojs" method=post>
User: <input name=user id=user size=12>
Password: <input type=password name=pass id=pass size=12>
<button type=submit>Sign in</button></form><p id=err></p>
<script>
f.addEventListener("submit", async (ev) => {
  ev.preventDefault();
  if (!user.value) { err.textContent = "NAME-REQUIRED"; return; }
  const r = await fetch("/js/api/login", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ user: user.value, pass: pass.value }) });
  const j = await r.json();
  if (j.ok) location.href = "/js/home"; else err.textContent = "BAD-LOGIN";
});
</script>""",
    "/js/xhr": """<!DOCTYPE html><title>XHR</title><table id=t><tr><th>Name<th>Qty</table>
<script>
const x = new XMLHttpRequest();
x.open("GET", "/js/api/items");
x.responseType = "json";
x.onload = () => { for (const it of x.response.items) { const r = t.insertRow(); r.insertCell().textContent = it.name; r.insertCell().textContent = String(it.qty); } document.body.insertAdjacentHTML("beforeend", "<p>XHR-DONE " + x.status + "</p>"); };
x.send();
</script>""",
    "/js/storage": """<!DOCTYPE html><title>Storage</title><p id=o></p><script>
localStorage.visits = String((+localStorage.visits || 0) + 1);
sessionStorage.setItem("s", "1");
o.textContent = "VISITS=" + localStorage.getItem("visits") + " SESSION=" + sessionStorage.length;
</script>""",
    "/js/timer": """<!DOCTYPE html><title>Timer</title><p>WAITING</p><script>
setTimeout(() => { location.href = "/js/after?from=timer"; }, 300);
</script>""",
    "/js/after": """<!DOCTYPE html><title>After</title><p>ARRIVED-AFTER-TIMER</p>""",
    "/js/write": """<!DOCTYPE html><title>Write</title><p>before</p>
<script>document.write("<p>WRITTEN-" + (2 + 3) + "</p>");</script><p>after</p>""",
    "/js/module": """<!DOCTYPE html><title>Module</title>
<script type=importmap>{"imports": {"lib": "/js/lib.mjs"}}</script>
<script type=module>import { msg } from "/js/mod.mjs"; import { twice } from "lib"; document.body.append(msg + " " + twice(21));</script>
<p>page</p>""",
    "/js/ws": """<!DOCTYPE html><title>WebSocket</title><p id=o>WS-WAIT</p><script>
const ws = new WebSocket("ws://" + location.host + "/js/echo");
ws.onopen = () => ws.send("hello-ws");
ws.onmessage = (e) => { o.textContent = "ECHO:" + e.data; ws.close(); };
ws.onerror = () => { o.textContent = "WS-ERROR"; };
</script>""",
    "/js/sse": """<!DOCTYPE html><title>SSE</title><p id=o>SSE-WAIT</p><script>
const got = [];
const es = new EventSource("/js/events");
es.onmessage = (e) => { got.push(e.data); if (got.length === 3) { o.textContent = "SSE:" + got.join(","); es.close(); } };
</script>""",
    "/js/click": """<!DOCTYPE html><title>Click</title><div id=b class=btn>Press me</div><p id=o>CLICKS=0</p><script>
let n = 0;
b.addEventListener("click", () => { n++; o.textContent = "CLICKS=" + n; });
</script>""",
    "/js/home": None,
}

JS_FILES = {
    "/js/mod.mjs": 'import { twice } from "./lib.mjs";\nexport const msg = "MODULE-OK-" + twice(2);\n',
    "/js/lib.mjs": "export function twice(x) { return x * 2; }\n",
}

GFX_PAGE = """<!DOCTYPE html><html><head><title>GFX test</title><style>
body { margin: 0; background: #ffffff; font-family: sans-serif }
#red { width: 200px; height: 100px; background: rgb(220, 20, 20) }
#grad { width: 200px; height: 40px; background: linear-gradient(to right, #000000, #ffffff) }
#round { width: 100px; height: 60px; background: rgb(20, 160, 20); border-radius: 30px; margin-top: 10px }
h1 { color: rgb(0, 0, 200); font-size: 32px; margin: 8px 0 }
</style></head><body>
<div id=red></div><div id=grad></div>
<img src="/gfx/blue.png" width=64 height=64 alt=blue style="display:block">
<div id=round></div>
<h1>GFX-TEXT</h1>
<canvas id=c width=120 height=60 style="display:block"></canvas>
<script>
const x = document.getElementById("c").getContext("2d");
x.fillStyle = "rgb(250, 200, 0)"; x.fillRect(0, 0, 120, 60);
x.fillStyle = "#8000ff"; x.beginPath(); x.arc(60, 30, 20, 0, Math.PI * 2); x.fill();
</script>
</body></html>"""


def png_bytes(w, h, rgb):
    import zlib
    raw = b"".join(b"\0" + bytes(rgb) * w for _ in range(h))
    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


JS_PORTAL_PAGE = """<!DOCTYPE html><html><head><title>Script Wi-Fi</title></head><body>
<h1>Welcome to Script Wi-Fi</h1>
<p><label><input type=checkbox id=terms> I accept the terms</label></p>
<p><span id=go class=button>Connect now</span></p><p id=msg></p>
<script>
go.addEventListener("click", async () => {
  if (!terms.checked) { msg.textContent = "PLEASE-ACCEPT"; return; }
  const r = await fetch("/js/api/accept", { method: "POST", body: new URLSearchParams({ terms: "yes" }) });
  if (r.ok) location.href = "/js/connected";
});
</script></body></html>"""


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def record(self, body=b""):
        with open(LOG, "a") as f:
            f.write("%s %s | %s | %s\n" % (self.command, self.path, self.headers.get("Cookie", ""), body.decode("latin-1")))

    def reply(self, code, body=b"", ctype="text/html; charset=utf-8", headers=()):
        if isinstance(body, str):
            body = body.encode()
        extra = list(headers)
        if body and "gzip" in self.headers.get("Accept-Encoding", "") and self.path.startswith("/welcome"):
            body = gzip.compress(body)
            extra.append(("Content-Encoding", "gzip"))
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def cookies(self):
        c = {}
        for part in self.headers.get("Cookie", "").split(";"):
            if "=" in part:
                k, v = part.strip().split("=", 1)
                c[k] = v
        return c

    def websocket(self):
        key = self.headers.get("Sec-WebSocket-Key", "")
        acc = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", acc)
        self.end_headers()
        self.wfile.flush()
        f = self.rfile
        while True:
            h = f.read(2)
            if len(h) < 2:
                return
            op, n = h[0] & 15, h[1] & 127
            if n == 126:
                n = struct.unpack(">H", f.read(2))[0]
            elif n == 127:
                n = struct.unpack(">Q", f.read(8))[0]
            mask = f.read(4) if h[1] & 128 else b"\0\0\0\0"
            data = bytes(b ^ mask[i % 4] for i, b in enumerate(f.read(n)))
            if op == 8:
                self.wfile.write(bytes([0x88, len(data[:2])]) + data[:2])
                self.wfile.flush()
                self.close_connection = True
                return
            if op in (1, 2):
                with open(LOG, "a") as lf:
                    lf.write("WS %s\n" % data.decode("latin-1"))
                hdr = bytes([0x80 | op, len(data)]) if len(data) < 126 else bytes([0x80 | op, 126]) + struct.pack(">H", len(data))
                self.wfile.write(hdr + data)
                self.wfile.flush()

    def events(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        for d in ("a", "b", "c"):
            chunk = ("id: %s\ndata: %s\n\n" % (d, d)).encode()
            self.wfile.write(b"%x\r\n%s\r\n" % (len(chunk), chunk))
            self.wfile.flush()
            time.sleep(0.1)
        self.wfile.write(b"0\r\n\r\n")
        self.close_connection = True

    def js_get(self, u):
        if u.path == "/js/tone.mp3":
            import os
            here = os.path.dirname(os.path.abspath(__file__))
            with open(os.path.join(here, "../crates/audio/tests/data/tone1k.mp3"), "rb") as f:
                self.reply(200, f.read(), "audio/mpeg")
        elif u.path == "/js/echo":
            self.websocket()
        elif u.path == "/js/events":
            self.events()
        elif u.path in JS_FILES:
            self.reply(200, JS_FILES[u.path], "text/javascript")
        elif u.path == "/js/api/items":
            self.reply(200, json.dumps({"items": [{"name": "apples", "qty": 3}, {"name": "pears", "qty": 5}]}), "application/json")
        elif u.path == "/js/home":
            if self.cookies().get("jsauth") == "1":
                self.reply(200, "<title>JS home</title><h1>SCRIPT-LOGIN-OK</h1>")
            else:
                self.reply(401, "<h1>Not logged in</h1>")
        elif u.path == "/js/connected":
            self.reply(200, "<title>Connected</title><h1>You are now connected</h1>")
        elif JS_PAGES.get(u.path):
            self.reply(200, JS_PAGES[u.path])
        else:
            self.reply(404, "<h1>Not found</h1>")

    def do_PUT(self):
        import os
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n)
        self.record()
        u = urllib.parse.urlparse(self.path)
        if u.path.startswith("/upload/"):
            os.makedirs(UPLOAD_DIR, exist_ok=True)
            with open(os.path.join(UPLOAD_DIR, os.path.basename(u.path)), "wb") as f:
                f.write(body)
            self.reply(201, "stored", "text/plain")
        else:
            self.reply(404, "")

    def do_GET(self):
        self.record()
        u = urllib.parse.urlparse(self.path)
        if u.path == "/gfx":
            self.reply(200, GFX_PAGE)
            return
        if u.path == "/gfx/blue.png":
            self.reply(200, png_bytes(16, 16, (10, 40, 230)), "image/png")
            return
        if u.path.startswith("/js/"):
            self.js_get(u)
            return
        if u.path == "/css":
            self.reply(200, CSS_PAGE)
            return
        if u.path in ("/site.css", "/base.css"):
            self.reply(200, SITE_CSS if u.path == "/site.css" else BASE_CSS, "text/css")
            return
        if u.path == "/":
            self.reply(200, INDEX)
        elif u.path == "/login":
            self.reply(200, LOGIN, headers=[("Set-Cookie", "sid=abc; Path=/")])
        elif u.path == "/welcome":
            if self.cookies().get("auth") == "1":
                self.reply(200, "<title>Welcome</title><h1>Welcome alice</h1><p>You are logged in.</p>")
            else:
                self.reply(401, "<h1>Not logged in</h1>")
        elif u.path == "/search":
            q = urllib.parse.parse_qs(u.query).get("q", [""])[0]
            self.reply(200, "<title>Search</title><p>Results for <b>%s</b></p>" % q)
        elif u.path == "/refresh":
            self.reply(200, '<meta http-equiv="refresh" content="1; url=/welcome"><p>Redirecting...</p>')
        elif u.path == "/generate_204":
            if state["portal_ok"]:
                self.reply(204)
            else:
                self.reply(302, "", headers=[("Location", "http://10.0.2.2:%d/portal?orig=generate_204" % PORT)])
        elif u.path == "/portal":
            self.reply(200, JS_PORTAL_PAGE if JS_PORTAL else PORTAL)
        else:
            self.reply(404, "<h1>Not found</h1>")

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n)
        self.record(body)
        form = urllib.parse.parse_qs(body.decode())
        u = urllib.parse.urlparse(self.path)
        if u.path == "/js/api/login":
            try:
                j = json.loads(body)
            except ValueError:
                j = {}
            if j.get("user") == "alice" and j.get("pass") == "secret":
                self.reply(200, '{"ok": true}', "application/json", headers=[("Set-Cookie", "jsauth=1; Path=/; HttpOnly")])
            else:
                self.reply(200, '{"ok": false}', "application/json")
            return
        if u.path == "/js/api/accept":
            state["portal_ok"] = form.get("terms") == ["yes"]
            self.reply(200 if state["portal_ok"] else 400, "ok", "text/plain")
            return
        if u.path == "/login":
            ok = self.cookies().get("sid") == "abc" and form.get("user") == ["alice"] and form.get("pass") == ["secret"]
            if ok:
                self.reply(302, "", headers=[("Location", "/welcome"), ("Set-Cookie", "auth=1; Path=/; HttpOnly")])
            else:
                self.reply(403, "<h1>Login failed</h1>")
        elif u.path == "/portal":
            if form.get("terms") == ["on"]:
                state["portal_ok"] = True
                self.reply(200, "<title>Connected</title><h1>You are now connected</h1>")
            else:
                self.reply(200, PORTAL.replace("<h1>", "<p><b>Please accept the terms.</b></p><h1>"))
        else:
            self.reply(404, "")


open(LOG, "w").close()
http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
