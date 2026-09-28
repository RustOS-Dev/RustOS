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
Every request is appended to LOGFILE as "METHOD PATH | cookie | body".
"""
import gzip
import http.server
import sys
import urllib.parse

PORT = int(sys.argv[1])
LOG = sys.argv[2]
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

    def do_GET(self):
        self.record()
        u = urllib.parse.urlparse(self.path)
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
            self.reply(200, PORTAL)
        else:
            self.reply(404, "<h1>Not found</h1>")

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n)
        self.record(body)
        form = urllib.parse.parse_qs(body.decode())
        u = urllib.parse.urlparse(self.path)
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
