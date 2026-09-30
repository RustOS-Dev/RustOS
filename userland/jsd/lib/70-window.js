// Window: location, history, navigator, screen, dialogs, storage,
// getComputedStyle, matchMedia, open/close and postMessage.
"use strict";
(function (G) {
  const J = G.__jsd;

  // `window` is the global object; it dispatches like an EventTarget.
  const ET = G.EventTarget.prototype;
  for (const k of ["addEventListener", "removeEventListener", "dispatchEvent"]) G[k] = ET[k].bind(G);
  G._ls = null;
  G.window = G.self = G.frames = G.globalThis;
  G.top = G.parent = G;
  G.opener = null;
  G.frameElement = null;
  G.closed = false;
  G.name = "";
  G.length = 0;
  G.status = "";
  G.origin = "null";
  G.isSecureContext = false;
  G.crossOriginIsolated = false;
  G.devicePixelRatio = 1;
  G.innerWidth = 640; G.innerHeight = 400; G.outerWidth = 640; G.outerHeight = 400;
  G.scrollX = G.pageXOffset = 0; G.scrollY = G.pageYOffset = 0;
  G.screenX = G.screenLeft = 0; G.screenY = G.screenTop = 0;
  G.scrollTo = G.scroll = function (x, y) {
    if (typeof x === "object" && x) { y = x.top; x = x.left; }
    J.queue({ t: "scroll", x: Number(x) || 0, y: Number(y) || 0 });
  };
  G.scrollBy = function (x, y) {
    if (typeof x === "object" && x) { y = x.top; x = x.left; }
    G.scrollTo(G.scrollX + (Number(x) || 0), G.scrollY + (Number(y) || 0));
  };
  J.defineHandlerProps(G, J.eventTypes);
  G.focus = () => {}; G.blur = () => {}; G.print = () => {}; G.stop = () => {};
  G.moveTo = G.moveBy = G.resizeTo = G.resizeBy = () => {};
  G.captureEvents = G.releaseEvents = () => {};
  G.screen = { width: 640, height: 400, availWidth: 640, availHeight: 400, colorDepth: 24, pixelDepth: 24, orientation: { type: "landscape-primary", angle: 0, addEventListener() {} } };
  J.setViewport = function (w, h) {
    G.innerWidth = G.outerWidth = G.screen.width = G.screen.availWidth = w;
    G.innerHeight = G.outerHeight = G.screen.height = G.screen.availHeight = h;
  };

  // ---- dialogs (shown by the browser; they block the script) ----
  G.alert = (m) => { J.rpc("alert", { text: m === undefined ? "" : String(m) }); };
  G.confirm = (m) => !!J.rpc("confirm", { text: m === undefined ? "" : String(m) });
  G.prompt = (m, d) => { const r = J.rpc("prompt", { text: m === undefined ? "" : String(m), def: d === undefined ? "" : String(d) }); return r === null || r === undefined ? null : String(r); };

  // ---- location ----
  function cur() { return new G.URL(G.document.URL); }
  class Location {
    get href() { return G.document.URL; }
    set href(v) { J.navigate(String(v)); }
    assign(v) { J.navigate(String(v)); }
    replace(v) { J.navigate(String(v), { replace: true }); }
    reload() { J.navigate(G.document.URL, { reload: true }); }
    toString() { return this.href; }
    get ancestorOrigins() { return []; }
  }
  for (const part of ["protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin"]) {
    Object.defineProperty(Location.prototype, part, {
      get() { try { return cur()[part]; } catch (e) { return ""; } },
      set(v) {
        if (part === "origin") return;
        const u = cur();
        u[part] = v;
        J.navigate(u.href);
      },
    });
  }
  const location = new Location();
  Object.defineProperty(G, "location", { get: () => location, set: (v) => { location.href = v; }, configurable: true });

  // Navigation: fragment changes stay in the page; others are handed to
  // the browser, which replaces this jsd.
  J.navigate = function (url, opts) {
    opts = opts || {};
    let u;
    try { u = new G.URL(url, G.document.baseURI); } catch (e) { throw new G.DOMException("'" + url + "' is not a valid URL.", "SyntaxError"); }
    if (u.protocol === "javascript:") {
      const code = decodeURIComponent(u.href.slice(11));
      G.setTimeout(() => { try { (0, eval)(code); } catch (e) { G.__onError(e); } }, 0);
      return;
    }
    const old = cur();
    if (!opts.reload && !opts.download && u.hash !== "" && u.href.split("#")[0] === old.href.split("#")[0] && (opts.target || "_self").match(/^(_self|)$/)) {
      if (u.href === old.href) { J.queue({ t: "fragment", url: u.href }); return; }
      J.setURL(u.href, !opts.replace, null);
      J.queue({ t: "fragment", url: u.href });
      G.setTimeout(() => G.dispatchEvent(new G.HashChangeEvent("hashchange", { oldURL: old.href, newURL: u.href })), 0);
      return;
    }
    J.queue({ t: "navigate", url: u.href, replace: !!opts.replace, target: opts.target || "", download: !!opts.download });
    J.navigating = true;
  };
  J.setURL = function (url, push, state) {
    G.document._url = url;
    if (push) { hist.splice(hpos + 1); hist.push({ url, state }); hpos = hist.length - 1; }
    else hist[hpos] = { url, state };
  };

  // ---- history ----
  const hist = [{ url: "", state: null }];
  let hpos = 0;
  J.history = hist;
  class History {
    get length() { return J.historyBefore + hist.length + J.historyAfter; }
    get state() { return hist[hpos].state; }
    get scrollRestoration() { return "auto"; }
    set scrollRestoration(v) {}
    pushState(state, title, url) { this._change(state, url, true); }
    replaceState(state, title, url) { this._change(state, url, false); }
    _change(state, url, push) {
      let u = G.document.URL;
      if (url !== undefined && url !== null) {
        const n = new G.URL(String(url), G.document.baseURI);
        const o = new G.URL(G.document.URL);
        if (n.origin !== o.origin) throw new G.DOMException("A history state object with URL '" + n.href + "' cannot be created in a document with origin '" + o.origin + "'.", "SecurityError");
        u = n.href;
      }
      state = state === undefined ? null : G.structuredClone(state);
      J.setURL(u, push, state);
      J.queue({ t: "history", url: u, push });
    }
    go(n) {
      n = Number(n) || 0;
      if (n === 0) { location.reload(); return; }
      const to = hpos + n;
      if (to >= 0 && to < hist.length) {
        // Same-document traversal.
        G.setTimeout(() => {
          hpos = to;
          const e = hist[to];
          const oldHash = new G.URL(G.document.URL).hash;
          G.document._url = e.url;
          J.queue({ t: "history", url: e.url, push: false });
          G.dispatchEvent(new G.PopStateEvent("popstate", { state: e.state }));
          if (new G.URL(e.url).hash !== oldHash) G.dispatchEvent(new G.HashChangeEvent("hashchange", { newURL: e.url }));
        }, 0);
      } else J.queue({ t: "go", delta: to < 0 ? to : to - hist.length + 1 });
    }
    back() { this.go(-1); }
    forward() { this.go(1); }
  }
  J.historyBefore = 0;
  J.historyAfter = 0;
  G.history = new History();

  // ---- navigator ----
  const nav = {
    userAgent: "Mozilla/5.0 (X11; RustOS x86_64) browse/0.3", appName: "Netscape", appCodeName: "Mozilla", appVersion: "5.0 (X11)",
    product: "Gecko", productSub: "20030107", vendor: "", vendorSub: "", platform: "Linux x86_64", language: "en-US", languages: ["en-US", "en"],
    onLine: true, cookieEnabled: true, doNotTrack: null, hardwareConcurrency: 1, maxTouchPoints: 0, pdfViewerEnabled: false, webdriver: false,
    plugins: [], mimeTypes: [], javaEnabled: () => false,
    sendBeacon(url, data) { try { G.fetch(url, { method: "POST", body: data, keepalive: true }).catch(() => {}); return true; } catch (e) { return false; } },
    clipboard: { writeText: (t) => { J.queue({ t: "clipboard", text: String(t) }); return Promise.resolve(); }, readText: () => Promise.resolve("") },
    permissions: { query: () => Promise.resolve({ state: "denied", onchange: null, addEventListener() {} }) },
    mediaDevices: { enumerateDevices: () => Promise.resolve([]), getUserMedia: () => Promise.reject(new G.DOMException("not supported", "NotSupportedError")) },
    storage: { estimate: () => Promise.resolve({ quota: 5 << 20, usage: 0 }), persist: () => Promise.resolve(false), persisted: () => Promise.resolve(false) },
    userAgentData: { brands: [], mobile: false, platform: "Linux", getHighEntropyValues: () => Promise.resolve({}) },
    connection: { effectiveType: "4g", downlink: 10, rtt: 50, saveData: false, addEventListener() {} },
    geolocation: { getCurrentPosition(ok, err) { if (err) G.setTimeout(() => err({ code: 1, message: "denied" }), 0); }, watchPosition() { return 0; }, clearWatch() {} },
    vibrate: () => false,
    share: () => Promise.reject(new G.DOMException("not supported", "NotSupportedError")),
    canShare: () => false,
    registerProtocolHandler() {},
    serviceWorker: undefined,
  };
  G.navigator = nav;
  G.clientInformation = nav;

  // ---- storage (per origin, kept by the browser) ----
  function makeStorage(kind) {
    const data = new Map();
    let loaded = false;
    const load = () => {
      if (loaded) return;
      loaded = true;
      const obj = J.rpc("storageLoad", { kind }) || {};
      for (const k of Object.keys(obj)) data.set(k, String(obj[k]));
    };
    let bytes = 0;
    const save = (k, v) => {
      J.queue({ t: "storageSet", kind, k, v });
    };
    const api = {
      get length() { load(); return data.size; },
      key(i) { load(); return Array.from(data.keys())[i] ?? null; },
      getItem(k) { load(); k = String(k); return data.has(k) ? data.get(k) : null; },
      setItem(k, v) {
        load(); k = String(k); v = String(v);
        const old = data.get(k);
        bytes += v.length - (old ? old.length : 0);
        if (bytes > 5 << 20) { bytes -= v.length - (old ? old.length : 0); throw new G.DOMException("The quota has been exceeded.", "QuotaExceededError"); }
        data.set(k, v);
        save(k, v);
      },
      removeItem(k) { load(); k = String(k); if (data.delete(k)) save(k, null); },
      clear() { load(); data.clear(); J.queue({ t: "storageClear", kind }); },
    };
    return new Proxy(api, {
      get(t, k) {
        if (typeof k === "symbol" || k in t) return t[k];
        load();
        return data.has(k) ? data.get(k) : undefined;
      },
      set(t, k, v) { if (k in t) return false; t.setItem(k, v); return true; },
      deleteProperty(t, k) { t.removeItem(k); return true; },
      has(t, k) { load(); return k in t || data.has(k); },
      ownKeys() { load(); return Array.from(data.keys()); },
      getOwnPropertyDescriptor(t, k) { load(); return data.has(k) ? { value: data.get(k), enumerable: true, configurable: true, writable: true } : undefined; },
    });
  }
  let ls, ss;
  Object.defineProperty(G, "localStorage", { get: () => ls || (ls = makeStorage("local")), configurable: true });
  Object.defineProperty(G, "sessionStorage", { get: () => ss || (ss = makeStorage("session")), configurable: true });
  G.indexedDB = undefined;
  G.caches = undefined;

  // ---- computed style and media queries (asked of the browser) ----
  G.getComputedStyle = function (el, pseudo) {
    if (!el || el.nodeType !== 1) throw new TypeError("parameter 1 is not of type 'Element'.");
    let props = {};
    if (el.isConnected) { J.flushOps(); props = J.rpc("computedStyle", { id: el.__id, pseudo: pseudo || null }) || {}; }
    const kebab = (p) => p.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
    const names = Object.keys(props);
    const decl = {
      getPropertyValue: (n) => props[String(n).toLowerCase()] ?? props[String(n)] ?? "",
      getPropertyPriority: () => "",
      item: (i) => names[i] || "",
      get length() { return names.length; },
      get cssText() { return ""; },
      setProperty() { throw new G.DOMException("read-only", "NoModificationAllowedError"); },
      removeProperty() { throw new G.DOMException("read-only", "NoModificationAllowedError"); },
    };
    return new Proxy(decl, {
      get(t, k) {
        if (typeof k !== "string" || k in t) return t[k];
        if (/^\d+$/.test(k)) return names[Number(k)];
        return t.getPropertyValue(k === "cssFloat" ? "float" : kebab(k));
      },
    });
  };
  class MediaQueryList extends G.EventTarget {
    constructor(q) { super(); this.media = q; this.onchange = null; this._m = !!J.rpc("matchMedia", { q }); }
    get matches() { return this._m; }
    addListener(f) { this.addEventListener("change", f); }
    removeListener(f) { this.removeEventListener("change", f); }
  }
  G.matchMedia = (q) => new MediaQueryList(String(q));
  G.MediaQueryList = MediaQueryList;

  // ---- windows ----
  G.open = function (url, target) {
    url = url === undefined || url === "" ? "about:blank" : String(url);
    let u;
    try { u = new G.URL(url, G.document.baseURI).href; } catch (e) { throw new G.DOMException("bad URL", "SyntaxError"); }
    J.queue({ t: "navigate", url: u, target: target || "_blank", replace: false });
    // The new page runs in its own jsd; scripts get a stand-in.
    return { closed: false, close() { this.closed = true; }, focus() {}, blur() {}, postMessage() {}, location: { href: u }, document: null };
  };
  G.close = function () { J.queue({ t: "close" }); };
  G.postMessage = function (data, origin) {
    const ev = new G.MessageEvent("message", { data: G.structuredClone(data), origin: G.location.origin, source: G });
    void origin;
    G.setTimeout(() => G.dispatchEvent(ev), 0);
  };
  G.reportError = (e) => G.__onError(e);
  G.visualViewport = { width: 640, height: 400, scale: 1, offsetLeft: 0, offsetTop: 0, pageLeft: 0, pageTop: 0, addEventListener() {}, removeEventListener() {} };
  G.customElements = G.customElements;
  G.external = { AddSearchProvider() {}, IsSearchProviderInstalled() {} };
  G.speechSynthesis = undefined;

  // Observers the page may probe (resize/intersection report once).
  class ResizeObserver {
    constructor(cb) { this._cb = cb; this._t = []; }
    observe(el) {
      this._t.push(el);
      G.setTimeout(() => {
        const r = el.getBoundingClientRect();
        try { this._cb([{ target: el, contentRect: r, borderBoxSize: [{ inlineSize: r.width, blockSize: r.height }], contentBoxSize: [{ inlineSize: r.width, blockSize: r.height }] }], this); } catch (e) { G.__onError(e); }
      }, 0);
    }
    unobserve(el) { this._t = this._t.filter((x) => x !== el); }
    disconnect() { this._t = []; }
  }
  class IntersectionObserver {
    constructor(cb, opts) { this._cb = cb; this.root = (opts && opts.root) || null; this.rootMargin = (opts && opts.rootMargin) || "0px"; this.thresholds = [0]; }
    observe(el) {
      // Everything counts as visible: lazy loaders then load at once.
      G.setTimeout(() => {
        const r = el.getBoundingClientRect();
        try { this._cb([{ target: el, isIntersecting: true, intersectionRatio: 1, boundingClientRect: r, intersectionRect: r, rootBounds: null, time: G.performance.now() }], this); } catch (e) { G.__onError(e); }
      }, 0);
    }
    unobserve() {}
    disconnect() {}
    takeRecords() { return []; }
  }
  class PerformanceObserver { constructor() {} observe() {} disconnect() {} takeRecords() { return []; } }
  PerformanceObserver.supportedEntryTypes = [];
  Object.assign(G, { ResizeObserver, IntersectionObserver, PerformanceObserver, Location, History });
})(globalThis);
