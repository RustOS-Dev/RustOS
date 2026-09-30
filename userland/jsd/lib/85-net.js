// Networking through the browser: Blob/File/FileReader, Headers,
// Request, Response, fetch, AbortController, XMLHttpRequest, WebSocket
// and EventSource. The browser performs every request with its cookie
// jar, cache and TLS, and applies the same-origin policy and CORS.
"use strict";
(function (G) {
  const J = G.__jsd;

  // ---- Blob / File ----
  function partBytes(p) {
    if (p instanceof Blob) return p._bytes;
    if (p instanceof ArrayBuffer || ArrayBuffer.isView(p)) return J.bytesOf(p).slice();
    return J.utf8(String(p));
  }
  function concat(list) {
    const n = list.reduce((a, b) => a + b.length, 0);
    const out = new Uint8Array(n);
    let o = 0;
    for (const b of list) { out.set(b, o); o += b.length; }
    return out;
  }
  J.concatBytes = concat;
  class Blob {
    constructor(parts, opts) {
      const list = [];
      if (parts !== undefined) {
        if (typeof parts !== "object" || parts === null || typeof parts[Symbol.iterator] !== "function") throw new TypeError("Failed to construct 'Blob': The provided value cannot be converted to a sequence.");
        for (const p of parts) {
          let b = partBytes(p);
          if (opts && opts.endings === "native" && typeof p === "string") b = J.utf8(p.replace(/\r?\n/g, "\n"));
          list.push(b);
        }
      }
      this._bytes = concat(list);
      const t = opts && opts.type !== undefined ? String(opts.type) : "";
      this._type = /[^\x20-\x7e]/.test(t) ? "" : t.toLowerCase();
    }
    get size() { return this._bytes.length; }
    get type() { return this._type; }
    slice(a, b, type) {
      const n = this._bytes.length;
      const rel = (x, d) => (x === undefined ? d : x < 0 ? Math.max(n + x, 0) : Math.min(x, n));
      const s = rel(a, 0), e = rel(b, n);
      const r = new Blob([], { type: type === undefined ? "" : type });
      r._bytes = this._bytes.slice(s, Math.max(s, e));
      return r;
    }
    text() { return Promise.resolve(J.fromUtf8(this._bytes)); }
    arrayBuffer() { return Promise.resolve(this._bytes.slice().buffer); }
    bytes() { return Promise.resolve(this._bytes.slice()); }
    stream() { return new G.ReadableStream({ start: (c) => { c.enqueue(this._bytes.slice()); c.close(); } }); }
    get [Symbol.toStringTag]() { return "Blob"; }
  }
  class File extends Blob {
    constructor(parts, name, opts) {
      if (arguments.length < 2) throw new TypeError("Failed to construct 'File': 2 arguments required, but only " + arguments.length + " present.");
      super(parts, opts);
      this._name = String(name);
      this._mtime = opts && opts.lastModified !== undefined ? Number(opts.lastModified) : Date.now();
    }
    get name() { return this._name; }
    get lastModified() { return this._mtime; }
    get webkitRelativePath() { return ""; }
    get [Symbol.toStringTag]() { return "File"; }
  }
  class FileList {
    constructor(el) { Object.defineProperty(this, "_el", { value: el }); }
    get length() { return (this._el._files || []).length; }
    item(i) { return (this._el._files || [])[i] || null; }
    [Symbol.iterator]() { return (this._el._files || []).slice()[Symbol.iterator](); }
  }
  J.fileList = (el) => new Proxy(new FileList(el), { get(t, k, r) { if (typeof k === "string" && /^\d+$/.test(k)) return t.item(Number(k)); return Reflect.get(t, k, r); } });

  const objectURLs = new Map();
  let objSeq = 1;
  J.createObjectURL = function (blob) {
    const u = "blob:" + G.location.origin + "/" + (G.crypto ? G.crypto.randomUUID() : String(objSeq++));
    objectURLs.set(u, blob);
    return u;
  };
  J.revokeObjectURL = (u) => objectURLs.delete(String(u));

  class FileReader extends G.EventTarget {
    constructor() { super(); this.readyState = 0; this.result = null; this.error = null; }
    _read(blob, kind, enc) {
      if (this.readyState === 1) throw new G.DOMException("The object is already busy reading Blobs.", "InvalidStateError");
      this.readyState = 1;
      this.result = null;
      G.setTimeout(() => {
        if (this._aborted) { this._aborted = false; return; }
        const b = blob._bytes;
        if (kind === "text") this.result = new G.TextDecoder(enc && J.encodingOf(enc) ? enc : "utf-8").decode(b);
        else if (kind === "buffer") this.result = b.slice().buffer;
        else if (kind === "binary") { let s = ""; for (const x of b) s += String.fromCharCode(x); this.result = s; }
        else this.result = "data:" + (blob.type || "application/octet-stream") + ";base64," + J.b64(b);
        this.readyState = 2;
        for (const t of ["loadstart", "progress", "load", "loadend"]) {
          const ev = new G.ProgressEvent(t, { lengthComputable: true, loaded: b.length, total: b.length });
          J.dispatchEvent(this, ev);
        }
      }, 0);
    }
    readAsText(b, enc) { this._read(b, "text", enc); }
    readAsArrayBuffer(b) { this._read(b, "buffer"); }
    readAsBinaryString(b) { this._read(b, "binary"); }
    readAsDataURL(b) { this._read(b, "data"); }
    abort() {
      if (this.readyState !== 1) return;
      this._aborted = true;
      this.readyState = 2;
      this.result = null;
      J.dispatchEvent(this, new G.ProgressEvent("abort"));
      J.dispatchEvent(this, new G.ProgressEvent("loadend"));
    }
  }
  FileReader.EMPTY = 0; FileReader.LOADING = 1; FileReader.DONE = 2;
  J.defineHandlerProps(FileReader.prototype, ["loadstart", "progress", "load", "loadend", "error", "abort"]);

  // ---- a minimal ReadableStream (enough for response.body readers) ----
  class ReadableStream {
    constructor(src) {
      this._q = []; this._closed = false; this._waiting = null; this._err = null;
      const c = {
        enqueue: (v) => { if (this._waiting) { const w = this._waiting; this._waiting = null; w[0]({ value: v, done: false }); } else this._q.push(v); },
        close: () => { this._closed = true; if (this._waiting) { const w = this._waiting; this._waiting = null; w[0]({ value: undefined, done: true }); } },
        error: (e) => { this._err = e; if (this._waiting) { const w = this._waiting; this._waiting = null; w[1](e); } },
        desiredSize: 1,
      };
      this._pull = src && src.pull ? () => src.pull(c) : null;
      if (src && src.start) src.start(c);
    }
    get locked() { return !!this._reader; }
    getReader() {
      if (this._reader) throw new TypeError("ReadableStream is locked");
      const s = this;
      this._reader = {
        read() {
          if (s._q.length) return Promise.resolve({ value: s._q.shift(), done: false });
          if (s._err) return Promise.reject(s._err);
          if (s._closed) return Promise.resolve({ value: undefined, done: true });
          if (s._pull) s._pull();
          if (s._q.length) return Promise.resolve({ value: s._q.shift(), done: false });
          return new Promise((a, b) => { s._waiting = [a, b]; });
        },
        releaseLock() { s._reader = null; },
        cancel() { s._closed = true; s._q = []; return Promise.resolve(); },
        closed: Promise.resolve(),
      };
      return this._reader;
    }
    cancel() { this._closed = true; this._q = []; return Promise.resolve(); }
    async *[Symbol.asyncIterator]() {
      const r = this.getReader();
      for (;;) { const { value, done } = await r.read(); if (done) return; yield value; }
    }
  }
  G.ReadableStream = ReadableStream;

  // ---- AbortController ----
  class AbortSignal extends G.EventTarget {
    constructor() { super(); this.aborted = false; this.reason = undefined; this.onabort = null; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    _abort(reason) {
      if (this.aborted) return;
      this.aborted = true;
      this.reason = reason === undefined ? new G.DOMException("signal is aborted without reason", "AbortError") : reason;
      const ev = new G.Event("abort");
      J.dispatchEvent(this, ev);
      if (typeof this.onabort === "function") this.onabort(ev);
    }
    static abort(r) { const s = new AbortSignal(); s._abort(r); return s; }
    static timeout(ms) { const s = new AbortSignal(); G.setTimeout(() => s._abort(new G.DOMException("signal timed out", "TimeoutError")), ms); return s; }
    static any(list) {
      const s = new AbortSignal();
      for (const x of list) { if (x.aborted) { s._abort(x.reason); break; } x.addEventListener("abort", () => s._abort(x.reason)); }
      return s;
    }
  }
  class AbortController {
    constructor() { this.signal = new AbortSignal(); }
    abort(r) { this.signal._abort(r); }
  }

  // ---- Headers ----
  const TOKEN = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
  const normValue = (v) => String(v).replace(/^[\t\n\r ]+|[\t\n\r ]+$/g, "");
  const FORBIDDEN_REQ = /^(accept-charset|accept-encoding|access-control-request-headers|access-control-request-method|connection|content-length|cookie|cookie2|date|dnt|expect|host|keep-alive|origin|referer|set-cookie|te|trailer|transfer-encoding|upgrade|via|proxy-.*|sec-.*)$/i;
  class Headers {
    constructor(init) {
      this._l = [];
      this._guard = "none";
      if (init === undefined || init === null) return;
      if (init instanceof Headers) { for (const [k, v] of init._l) this.append(k, v); return; }
      if (typeof init !== "object") throw new TypeError("Failed to construct 'Headers': The provided value is not of type 'HeadersInit'.");
      if (typeof init[Symbol.iterator] === "function") {
        for (const p of init) {
          const a = Array.from(p);
          if (a.length !== 2) throw new TypeError("Failed to construct 'Headers': Invalid value");
          this.append(a[0], a[1]);
        }
      } else for (const k of Object.keys(init)) this.append(k, init[k]);
    }
    _check(k, v) {
      if (!TOKEN.test(k)) throw new TypeError("Invalid name");
      if (v !== undefined && /[\0\r\n]/.test(v)) throw new TypeError("Invalid value");
      if (this._guard === "immutable") throw new TypeError("Headers are immutable");
    }
    _skip(k) { return this._guard === "request" && FORBIDDEN_REQ.test(k); }
    append(k, v) { k = String(k); v = normValue(v); this._check(k, v); if (this._skip(k)) return; this._l.push([k.toLowerCase(), v]); }
    set(k, v) {
      k = String(k); v = normValue(v); this._check(k, v);
      if (this._skip(k)) return;
      const lk = k.toLowerCase();
      const i = this._l.findIndex((e) => e[0] === lk);
      if (i < 0) this._l.push([lk, v]);
      else { this._l[i][1] = v; this._l = this._l.filter((e, j) => j <= i || e[0] !== lk); }
    }
    delete(k) { k = String(k); this._check(k); this._l = this._l.filter((e) => e[0] !== k.toLowerCase()); }
    get(k) {
      k = String(k); if (!TOKEN.test(k)) throw new TypeError("Invalid name");
      const vs = this._l.filter((e) => e[0] === k.toLowerCase()).map((e) => e[1]);
      return vs.length ? vs.join(", ") : null;
    }
    getSetCookie() { return this._l.filter((e) => e[0] === "set-cookie").map((e) => e[1]); }
    has(k) { k = String(k); if (!TOKEN.test(k)) throw new TypeError("Invalid name"); return this._l.some((e) => e[0] === k.toLowerCase()); }
    _sorted() {
      const names = Array.from(new Set(this._l.map((e) => e[0]))).sort();
      const out = [];
      for (const n of names) {
        if (n === "set-cookie") for (const v of this.getSetCookie()) out.push([n, v]);
        else out.push([n, this.get(n)]);
      }
      return out;
    }
    forEach(f, t) { for (const [k, v] of this._sorted()) f.call(t, v, k, this); }
    entries() { return this._sorted()[Symbol.iterator](); }
    keys() { return this._sorted().map((e) => e[0])[Symbol.iterator](); }
    values() { return this._sorted().map((e) => e[1])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
  }

  // ---- bodies ----
  // Extract a body: [bytes, content type or null].
  function extract(body) {
    if (body === null || body === undefined) return [null, null];
    if (typeof body === "string") return [J.utf8(body), "text/plain;charset=UTF-8"];
    if (body instanceof G.URLSearchParams) return [J.utf8(body.toString()), "application/x-www-form-urlencoded;charset=UTF-8"];
    if (body instanceof Blob) return [body._bytes, body.type || null];
    if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) return [J.bytesOf(body).slice(), null];
    if (body instanceof G.FormData) {
      const boundary = "----rustosFormBoundary" + Math.random().toString(36).slice(2, 14);
      const parts = [];
      const esc = (s) => s.replace(/\n/g, "%0A").replace(/\r/g, "%0D").replace(/"/g, "%22");
      for (const [k, v] of body._e) {
        let h = "--" + boundary + "\r\nContent-Disposition: form-data; name=\"" + esc(k) + "\"";
        if (typeof v === "string") { parts.push(J.utf8(h + "\r\n\r\n" + v.replace(/\r(?!\n)|(?<!\r)\n/g, "\r\n") + "\r\n")); continue; }
        h += "; filename=\"" + esc(v.name) + "\"\r\nContent-Type: " + (v.type || "application/octet-stream") + "\r\n\r\n";
        parts.push(J.utf8(h), v._bytes, J.utf8("\r\n"));
      }
      parts.push(J.utf8("--" + boundary + "--\r\n"));
      return [concat(parts), "multipart/form-data; boundary=" + boundary];
    }
    if (body instanceof ReadableStream) {
      // Only already-queued chunks are sent.
      return [concat(body._q.map((c) => (typeof c === "string" ? J.utf8(c) : J.bytesOf(c)))), null];
    }
    return [J.utf8(String(body)), "text/plain;charset=UTF-8"];
  }
  J.extractBody = extract;

  function parseMultipart(bytes, ctype) {
    const m = /boundary=(?:"([^"]+)"|([^;]+))/i.exec(ctype || "");
    if (!m) throw new TypeError("Failed to fetch: missing boundary");
    const b = "--" + (m[1] || m[2]);
    const text = Array.from(bytes, (x) => String.fromCharCode(x)).join("");
    const fd = new G.FormData();
    for (const part of text.split(b).slice(1)) {
      if (part.startsWith("--")) break;
      const i = part.indexOf("\r\n\r\n");
      if (i < 0) continue;
      const head = part.slice(2, i), data = part.slice(i + 4, part.length - 2);
      const name = /name="([^"]*)"/i.exec(head), fname = /filename="([^"]*)"/i.exec(head), ct = /content-type:\s*([^\r\n]+)/i.exec(head);
      const raw = Uint8Array.from(data, (c) => c.charCodeAt(0));
      if (fname) fd.append(name ? name[1] : "", new File([raw], fname[1], { type: ct ? ct[1] : "" }));
      else fd.append(name ? name[1] : "", J.fromUtf8(raw));
    }
    return fd;
  }

  const Body = (Base) => class extends Base {
    get bodyUsed() { return !!this._used; }
    get body() {
      if (this._bytes === null) return null;
      if (!this._stream) { const b = this._bytes; this._stream = new ReadableStream({ start(c) { if (b.length) c.enqueue(b); c.close(); } }); }
      return this._stream;
    }
    _consume() {
      if (this._used) return Promise.reject(new TypeError("Failed to execute: body stream already read"));
      this._used = true;
      return Promise.resolve(this._bytes || new Uint8Array(0));
    }
    arrayBuffer() { return this._consume().then((b) => b.slice().buffer); }
    bytes() { return this._consume().then((b) => b.slice()); }
    text() { return this._consume().then((b) => J.fromUtf8(b)); }
    json() { return this.text().then((t) => JSON.parse(t)); }
    blob() { return this._consume().then((b) => new Blob([b], { type: this.headers.get("content-type") || "" })); }
    formData() {
      return this._consume().then((b) => {
        const ct = this.headers.get("content-type") || "";
        if (/multipart\/form-data/i.test(ct)) return parseMultipart(b, ct);
        if (/application\/x-www-form-urlencoded/i.test(ct)) {
          const fd = new G.FormData();
          for (const [k, v] of new G.URLSearchParams(J.fromUtf8(b))) fd.append(k, v);
          return fd;
        }
        throw new TypeError("Failed to fetch: not form data");
      });
    }
  };

  class Request extends Body(Object) {
    constructor(input, init) {
      super();
      init = init || {};
      let src = null;
      if (input instanceof Request) src = input;
      const url = src ? src.url : new G.URL(String(input), G.document ? G.document.baseURI : undefined).href;
      if (!src && /^[^:]+:\/\/[^/]*@/.test(url)) throw new TypeError("Failed to construct 'Request': Request cannot be constructed from a URL that includes credentials");
      this.url = url;
      let method = init.method !== undefined ? String(init.method) : src ? src.method : "GET";
      if (!TOKEN.test(method)) throw new TypeError("Failed to construct 'Request': '" + method + "' is not a valid HTTP method.");
      if (/^(connect|trace|track)$/i.test(method)) throw new TypeError("Failed to construct 'Request': '" + method + "' HTTP method is unsupported.");
      if (/^(delete|get|head|options|post|put|patch)$/i.test(method)) method = method.toUpperCase();
      this.method = method;
      this.headers = new Headers(init.headers !== undefined ? init.headers : src ? src.headers : undefined);
      this.headers._guard = "request";
      this.mode = init.mode || (src ? src.mode : "cors");
      if (this.mode === "navigate") throw new TypeError("Failed to construct 'Request': Cannot construct a Request with a RequestInit whose mode member is set as 'navigate'.");
      this.credentials = init.credentials || (src ? src.credentials : "same-origin");
      this.cache = init.cache || (src ? src.cache : "default");
      this.redirect = init.redirect || (src ? src.redirect : "follow");
      this.referrer = init.referrer !== undefined ? String(init.referrer) : "about:client";
      this.referrerPolicy = init.referrerPolicy || "";
      this.integrity = init.integrity || "";
      this.keepalive = !!init.keepalive;
      this.signal = init.signal || (src ? src.signal : new AbortSignal());
      this.destination = "";
      this.duplex = "half";
      let body = init.body;
      if (body !== undefined && body !== null && (this.method === "GET" || this.method === "HEAD")) throw new TypeError("Failed to construct 'Request': Request with GET/HEAD method cannot have body.");
      if (body === undefined && src) {
        if (src._used) throw new TypeError("Failed to construct 'Request': Cannot construct a Request with a Request object that has already been used.");
        this._bytes = src._bytes;
      } else {
        const [bytes, ct] = extract(body);
        this._bytes = bytes;
        if (ct && !this.headers.has("content-type")) this.headers.append("content-type", ct);
      }
    }
    clone() {
      if (this._used) throw new TypeError("Failed to execute 'clone' on 'Request': Request body is already used");
      const r = new Request(this);
      return r;
    }
  }

  const NULL_BODY = new Set([101, 103, 204, 205, 304]);
  class Response extends Body(Object) {
    constructor(body, init) {
      super();
      init = init || {};
      const status = init.status === undefined ? 200 : Number(init.status);
      if (status < 200 || status > 599) throw new RangeError("Failed to construct 'Response': The status provided (" + status + ") is outside the range [200, 599].");
      this.status = status;
      this.statusText = init.statusText === undefined ? "" : String(init.statusText);
      this.headers = new Headers(init.headers);
      this.type = "default";
      this.url = "";
      this.redirected = false;
      if (body !== undefined && body !== null && NULL_BODY.has(status)) throw new TypeError("Failed to construct 'Response': Response with null body status cannot have body");
      const [bytes, ct] = extract(body);
      this._bytes = bytes;
      if (ct && !this.headers.has("content-type")) this.headers.append("content-type", ct);
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    clone() {
      if (this._used) throw new TypeError("Failed to execute 'clone' on 'Response': Response body is already used");
      const r = new Response(null, { status: this.status === 0 ? 200 : this.status, statusText: this.statusText, headers: this.headers });
      r.status = this.status;
      r._bytes = this._bytes;
      r.type = this.type; r.url = this.url; r.redirected = this.redirected;
      r.headers._guard = this.headers._guard;
      return r;
    }
    static error() { const r = new Response(null, { status: 200 }); r.status = 0; r.type = "error"; r.headers._guard = "immutable"; return r; }
    static redirect(url, status = 302) {
      if (![301, 302, 303, 307, 308].includes(status)) throw new RangeError("Failed to execute 'redirect' on 'Response': Invalid status code");
      const r = new Response(null, { status, headers: { location: new G.URL(url, G.document.baseURI).href } });
      r.headers._guard = "immutable";
      return r;
    }
    static json(data, init) {
      const s = JSON.stringify(data);
      if (s === undefined) throw new TypeError("Failed to execute 'json' on 'Response': The data is not JSON serializable");
      const r = new Response(s, init);
      r.headers.set("content-type", "application/json");
      return r;
    }
  }

  // The browser's view of a request.
  function wire(req) {
    return {
      method: req.method, url: req.url, headers: req.headers._l, body: req._bytes ? J.b64(req._bytes) : null,
      mode: req.mode, credentials: req.credentials, redirect: req.redirect, cache: req.cache,
      referrer: req.referrer === "about:client" ? G.document.URL : req.referrer,
    };
  }
  function fromWire(r, req) {
    const res = new Response(null, { status: 200 });
    res.status = r.status;
    res.statusText = r.statusText || "";
    res.headers = new Headers(r.headers || []);
    res.headers._guard = "immutable";
    res.url = r.url || req.url;
    res.redirected = !!r.redirected;
    res.type = r.type || "basic";
    res._bytes = NULL_BODY.has(r.status) || req.method === "HEAD" ? null : J.unb64(r.body || "") || new Uint8Array(0);
    return res;
  }

  G.fetch = function (input, init) {
    let req;
    try { req = new Request(input, init); } catch (e) { return Promise.reject(e); }
    if (req.signal.aborted) return Promise.reject(req.signal.reason);
    const u = new G.URL(req.url);
    if (u.protocol === "data:") return dataURL(u.href).then((r) => fromWire(r, req));
    if (u.protocol === "blob:") {
      const b = objectURLs.get(req.url.split("#")[0]);
      if (!b || req.method !== "GET") return Promise.reject(new TypeError("Failed to fetch"));
      return Promise.resolve(fromWire({ status: 200, statusText: "OK", headers: [["content-type", b.type], ["content-length", String(b.size)]], body: J.b64(b._bytes), url: req.url }, req));
    }
    if (u.protocol !== "http:" && u.protocol !== "https:") return Promise.reject(new TypeError("Failed to fetch"));
    return new Promise((resolve, reject) => {
      const p = J.rpcAsync("fetch", wire(req));
      const onAbort = () => reject(req.signal.reason);
      req.signal.addEventListener("abort", onAbort);
      p.then((r) => {
        req.signal.removeEventListener("abort", onAbort);
        if (req.signal.aborted) return;
        if (r.error) reject(new TypeError("Failed to fetch"));
        else resolve(fromWire(r, req));
        J.fetchDone && J.fetchDone();
      }, (e) => { void e; reject(new TypeError("Failed to fetch")); });
    });
  };
  function dataURL(href) {
    const m = /^data:([^,]*?)(;base64)?,(.*)$/is.exec(href);
    if (!m) return Promise.reject(new TypeError("Failed to fetch"));
    let bytes;
    const body = decodeURIComponent(m[3].replace(/%(?![0-9a-f]{2})/gi, "%25"));
    if (m[2]) { bytes = J.unb64(body); if (!bytes) return Promise.reject(new TypeError("Failed to fetch")); }
    else bytes = Uint8Array.from(body, (c) => c.charCodeAt(0) & 0xff);
    const type = m[1] || "text/plain;charset=US-ASCII";
    return Promise.resolve({ status: 200, statusText: "OK", headers: [["content-type", type]], body: J.b64(bytes), url: href });
  }

  // ---- XMLHttpRequest ----
  class XMLHttpRequestEventTarget extends G.EventTarget {}
  J.defineHandlerProps(XMLHttpRequestEventTarget.prototype, ["loadstart", "progress", "load", "loadend", "error", "abort", "timeout"]);
  class XMLHttpRequestUpload extends XMLHttpRequestEventTarget {}
  class XMLHttpRequest extends XMLHttpRequestEventTarget {
    constructor() {
      super();
      this.readyState = 0; this.status = 0; this.statusText = ""; this.responseURL = "";
      this.timeout = 0; this.withCredentials = false; this.responseType = "";
      this.upload = new XMLHttpRequestUpload();
      this._rh = []; this._res = null; this._send = false; this._gen = 0;
    }
    _state(n) { this.readyState = n; J.dispatchEvent(this, new G.Event("readystatechange")); }
    _fire(t, loaded, total) { J.dispatchEvent(this, new G.ProgressEvent(t, { lengthComputable: total !== undefined, loaded: loaded || 0, total: total || 0 })); }
    open(method, url, async, user, pass) {
      method = String(method);
      if (!TOKEN.test(method)) throw new G.DOMException("'" + method + "' is not a valid HTTP method.", "SyntaxError");
      if (/^(connect|trace|track)$/i.test(method)) throw new G.DOMException("'" + method + "' HTTP method is unsupported.", "SecurityError");
      if (/^(delete|get|head|options|post|put|patch)$/i.test(method)) method = method.toUpperCase();
      let u;
      try { u = new G.URL(String(url), G.document.baseURI); } catch (e) { throw new G.DOMException("Invalid URL", "SyntaxError"); }
      if (user !== undefined && user !== null) u.username = user;
      if (pass !== undefined && pass !== null) u.password = pass;
      this._method = method; this._url = u.href; this._async = async !== false;
      this._rh = []; this._res = null; this._send = false; this._gen++;
      this.status = 0; this.statusText = ""; this.responseURL = "";
      this._state(1);
    }
    setRequestHeader(k, v) {
      if (this.readyState !== 1 || this._send) throw new G.DOMException("The object's state must be OPENED.", "InvalidStateError");
      k = String(k); v = normValue(v);
      if (!TOKEN.test(k)) throw new G.DOMException("'" + k + "' is not a valid HTTP header field name.", "SyntaxError");
      if (FORBIDDEN_REQ.test(k)) return;
      const e = this._rh.find((x) => x[0].toLowerCase() === k.toLowerCase());
      if (e) e[1] += ", " + v; else this._rh.push([k, v]);
    }
    overrideMimeType(m) { this._mime = String(m); }
    send(body) {
      if (this.readyState !== 1 || this._send) throw new G.DOMException("The object's state must be OPENED.", "InvalidStateError");
      if (this._method === "GET" || this._method === "HEAD") body = null;
      let req;
      try {
        req = new Request(this._url, { method: this._method, headers: this._rh, body: body === undefined ? null : body, credentials: this.withCredentials ? "include" : "same-origin" });
      } catch (e) { throw new G.DOMException(e.message, "SyntaxError"); }
      if (body instanceof Document || (body && body.nodeType === 9)) { req._bytes = J.utf8(J.serializeChildren(body)); if (!req.headers.has("content-type")) req.headers.set("content-type", "text/html;charset=UTF-8"); }
      this._send = true;
      const gen = this._gen;
      const w = wire(req);
      if (!this._async) {
        let r;
        try { r = J.rpc("fetch", w); } catch (e) { r = { error: String(e) }; }
        this._finish(r, req, gen, true);
        return;
      }
      this._fire("loadstart", 0);
      if (req._bytes && req._bytes.length) this.upload._fire = XMLHttpRequest.prototype._fire;
      let timer = 0;
      if (this.timeout > 0) timer = G.setTimeout(() => { if (gen === this._gen) { this._gen++; this._error("timeout"); } }, this.timeout);
      J.rpcAsync("fetch", w).then((r) => { G.clearTimeout(timer); this._finish(r, req, gen, false); }, () => { G.clearTimeout(timer); this._finish({ error: "network" }, req, gen, false); });
    }
    _error(kind) {
      this._send = false;
      this._res = null;
      this.status = 0;
      this._state(4);
      this._fire(kind, 0);
      this._fire("loadend", 0);
    }
    _finish(r, req, gen, sync) {
      if (gen !== this._gen) return;
      if (r.error) {
        this._send = false;
        if (sync) { this.readyState = 4; throw new G.DOMException("Failed to execute 'send' on 'XMLHttpRequest': Failed to load '" + req.url + "'.", "NetworkError"); }
        this._error("error");
        return;
      }
      this._res = fromWire(r, req);
      this.status = r.status;
      this.statusText = r.statusText || "";
      this.responseURL = r.url || req.url;
      const n = this._res._bytes ? this._res._bytes.length : 0;
      if (sync) { this.readyState = 4; this._send = false; J.dispatchEvent(this, new G.Event("readystatechange")); this._fire("load", n, n); this._fire("loadend", n, n); return; }
      this._state(2);
      this._state(3);
      this._fire("progress", n, n);
      this._send = false;
      this._state(4);
      this._fire("load", n, n);
      this._fire("loadend", n, n);
    }
    abort() {
      this._gen++;
      if (this._send || this.readyState === 1 && this._send) { this._send = false; this._state(4); this._fire("abort"); this._fire("loadend"); }
      if (this.readyState === 4) { this.readyState = 0; this.status = 0; this.statusText = ""; this._res = null; }
    }
    getResponseHeader(k) {
      if (!this._res || this.readyState < 2) return null;
      k = String(k).toLowerCase();
      if (k === "set-cookie" || k === "set-cookie2") return null;
      return this._res.headers.get(k);
    }
    getAllResponseHeaders() {
      if (!this._res || this.readyState < 2) return "";
      return this._res.headers._sorted().filter((e) => e[0] !== "set-cookie").map(([k, v]) => k + ": " + v + "\r\n").join("");
    }
    _charset() {
      const ct = this._mime || (this._res && this._res.headers.get("content-type")) || "";
      const m = /charset=["']?([^;"']+)/i.exec(ct);
      return m && J.encodingOf(m[1]) ? m[1] : "utf-8";
    }
    get responseText() {
      if (this.responseType !== "" && this.responseType !== "text") throw new G.DOMException("The value is only accessible if the object's 'responseType' is '' or 'text'.", "InvalidStateError");
      if (!this._res || this.readyState < 3) return "";
      return new G.TextDecoder(this._charset()).decode(this._res._bytes || new Uint8Array(0));
    }
    get responseXML() {
      if (!this._res || this.readyState !== 4) return null;
      const ct = (this._mime || this._res.headers.get("content-type") || "").toLowerCase();
      if (this.responseType === "document" || /html|xml/.test(ct)) return new G.DOMParser().parseFromString(this.responseText, "text/html");
      return null;
    }
    get response() {
      const t = this.responseType;
      if (t === "" || t === "text") return this.responseText;
      if (!this._res || this.readyState !== 4) return null;
      const b = this._res._bytes || new Uint8Array(0);
      if (t === "json") { try { return JSON.parse(J.fromUtf8(b)); } catch (e) { return null; } }
      if (t === "arraybuffer") return b.slice().buffer;
      if (t === "blob") return new Blob([b], { type: this._res.headers.get("content-type") || "" });
      if (t === "document") return this.responseXML;
      return null;
    }
  }
  XMLHttpRequest.UNSENT = 0; XMLHttpRequest.OPENED = 1; XMLHttpRequest.HEADERS_RECEIVED = 2; XMLHttpRequest.LOADING = 3; XMLHttpRequest.DONE = 4;
  Object.assign(XMLHttpRequest.prototype, { UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 });
  J.defineHandlerProps(XMLHttpRequest.prototype, ["readystatechange"]);
  const Document = G.Document;

  // ---- WebSocket (RFC 6455, run by the browser) ----
  const sockets = new Map();
  class WebSocket extends G.EventTarget {
    constructor(url, protocols) {
      super();
      let u;
      try { u = new G.URL(String(url), G.document.baseURI); } catch (e) { throw new G.DOMException("The URL '" + url + "' is invalid.", "SyntaxError"); }
      if (u.protocol === "http:") u.protocol = "ws:";
      if (u.protocol === "https:") u.protocol = "wss:";
      if (u.protocol !== "ws:" && u.protocol !== "wss:") throw new G.DOMException("The URL's scheme must be either 'http', 'https', 'ws', or 'wss'.", "SyntaxError");
      if (u.hash) throw new G.DOMException("The URL contains a fragment identifier.", "SyntaxError");
      protocols = protocols === undefined ? [] : typeof protocols === "string" ? [protocols] : Array.from(protocols);
      this.url = u.href;
      this.readyState = 0;
      this.protocol = "";
      this.extensions = "";
      this.bufferedAmount = 0;
      this.binaryType = "blob";
      this._id = J.seq++;
      sockets.set(this._id, this);
      J.queue({ t: "wsOpen", id: this._id, url: u.href, protocols });
    }
    send(data) {
      if (this.readyState === 0) throw new G.DOMException("Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.", "InvalidStateError");
      if (this.readyState !== 1) return;
      if (typeof data === "string") J.queue({ t: "wsSend", id: this._id, text: data });
      else {
        const b = data instanceof Blob ? data._bytes : J.bytesOf(data);
        J.queue({ t: "wsSend", id: this._id, b64: J.b64(b) });
      }
    }
    close(code, reason) {
      if (code !== undefined && code !== 1000 && (code < 3000 || code > 4999)) throw new G.DOMException("The code must be either 1000, or between 3000 and 4999.", "InvalidAccessError");
      if (reason !== undefined && J.utf8(reason).length > 123) throw new G.DOMException("The message must not be greater than 123 bytes.", "SyntaxError");
      if (this.readyState >= 2) return;
      this.readyState = 2;
      J.queue({ t: "wsClose", id: this._id, code: code === undefined ? 1000 : code, reason: reason || "" });
    }
  }
  WebSocket.CONNECTING = 0; WebSocket.OPEN = 1; WebSocket.CLOSING = 2; WebSocket.CLOSED = 3;
  Object.assign(WebSocket.prototype, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
  J.defineHandlerProps(WebSocket.prototype, ["open", "message", "error", "close"]);
  class CloseEvent extends G.Event {
    constructor(t, i) { super(t, i); i = i || {}; this.wasClean = !!i.wasClean; this.code = i.code || 0; this.reason = i.reason || ""; }
  }
  J.handlers.ws = function (m) {
    const s = sockets.get(m.id);
    if (!s) return;
    if (m.kind === "open") { s.readyState = 1; s.protocol = m.protocol || ""; J.dispatchEvent(s, new G.Event("open")); }
    else if (m.kind === "message") {
      let data = m.text;
      if (m.b64 !== undefined) { const b = J.unb64(m.b64); data = s.binaryType === "arraybuffer" ? b.buffer : new Blob([b]); }
      J.dispatchEvent(s, new G.MessageEvent("message", { data, origin: new G.URL(s.url).origin }));
    } else if (m.kind === "close" || m.kind === "error") {
      if (m.kind === "error") J.dispatchEvent(s, new G.Event("error"));
      s.readyState = 3;
      sockets.delete(m.id);
      J.dispatchEvent(s, new CloseEvent("close", { wasClean: m.kind === "close", code: m.code || 1006, reason: m.reason || "" }));
    }
  };

  // ---- EventSource (the browser streams the body in chunks) ----
  const sources = new Map();
  class EventSource extends G.EventTarget {
    constructor(url, init) {
      super();
      this.url = new G.URL(String(url), G.document.baseURI).href;
      this.withCredentials = !!(init && init.withCredentials);
      this.readyState = 0;
      this._buf = ""; this._data = ""; this._event = ""; this._last = ""; this._retry = 3000;
      this._connect();
    }
    _connect() {
      this._id = J.seq++;
      sources.set(this._id, this);
      J.queue({ t: "esOpen", id: this._id, url: this.url, lastEventId: this._last, credentials: this.withCredentials });
    }
    close() { this.readyState = 2; sources.delete(this._id); J.queue({ t: "esClose", id: this._id }); }
    _line(line) {
      if (line === "") {
        if (this._data !== "") {
          const ev = new G.MessageEvent(this._event || "message", { data: this._data.replace(/\n$/, ""), lastEventId: this._last, origin: new G.URL(this.url).origin });
          J.dispatchEvent(this, ev);
        }
        this._data = ""; this._event = "";
        return;
      }
      if (line[0] === ":") return;
      const i = line.indexOf(":");
      const f = i < 0 ? line : line.slice(0, i);
      let v = i < 0 ? "" : line.slice(i + 1);
      if (v[0] === " ") v = v.slice(1);
      if (f === "data") this._data += v + "\n";
      else if (f === "event") this._event = v;
      else if (f === "id" && !v.includes("\0")) this._last = v;
      else if (f === "retry" && /^\d+$/.test(v)) this._retry = Number(v);
    }
  }
  EventSource.CONNECTING = 0; EventSource.OPEN = 1; EventSource.CLOSED = 2;
  J.defineHandlerProps(EventSource.prototype, ["open", "message", "error"]);
  J.handlers.es = function (m) {
    const s = sources.get(m.id);
    if (!s) return;
    if (m.kind === "open") { s.readyState = 1; J.dispatchEvent(s, new G.Event("open")); }
    else if (m.kind === "chunk") {
      s._buf += m.text;
      const lines = s._buf.split(/\r\n|\r|\n/);
      s._buf = lines.pop();
      for (const l of lines) s._line(l);
    } else if (m.kind === "end" || m.kind === "error") {
      sources.delete(m.id);
      if (s.readyState === 2) return;
      if (m.fatal) { s.readyState = 2; J.dispatchEvent(s, new G.Event("error")); return; }
      s.readyState = 0;
      J.dispatchEvent(s, new G.Event("error"));
      G.setTimeout(() => { if (s.readyState !== 2) s._connect(); }, s._retry);
    }
  };

  Object.assign(G, { Blob, File, FileList, FileReader, Headers, Request, Response, AbortController, AbortSignal, XMLHttpRequest,
    XMLHttpRequestUpload, XMLHttpRequestEventTarget, WebSocket, CloseEvent, EventSource });
  void Document;
})(globalThis);
