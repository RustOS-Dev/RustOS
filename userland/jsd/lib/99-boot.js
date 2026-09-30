// Page start-up and the browser's messages: building the document,
// running scripts in order (parser-blocking, async, defer, modules,
// import maps, document.write), load events, and user actions.
"use strict";
(function (G) {
  const J = G.__jsd;

  const JS_TYPES = /^(|text\/javascript|application\/javascript|application\/ecmascript|text\/ecmascript|application\/x-javascript|application\/x-ecmascript|text\/x-javascript|text\/x-ecmascript|text\/jscript|text\/livescript|text\/javascript1\.[0-5])$/;
  function scriptKind(s) {
    const t = (s.getAttribute("type") ?? (s.hasAttribute("language") ? "text/" + s.getAttribute("language") : "")).trim().toLowerCase();
    if (t === "module") return "module";
    if (t === "importmap") return "importmap";
    if (JS_TYPES.test(t.replace(/;.*$/, "").trim())) return "classic";
    return null;
  }

  // ---- fetching scripts ----
  function fetchText(url, kind) {
    const r = J.rpc("fetchScript", { url, kind });
    if (!r || r.error) throw new Error(r ? r.error : "failed");
    return r.text;
  }
  function fetchTextAsync(url, kind) {
    return J.rpcAsync("fetchScript", { url, kind }).then((r) => {
      if (!r || r.error) throw new Error(r ? r.error : "failed");
      return r.text;
    });
  }
  function absolute(s) {
    try { return new G.URL(s.getAttribute("src"), G.document.baseURI).href; } catch (e) { return null; }
  }

  function execClassic(s, code, url) {
    const d = G.document;
    const prev = d._currentScript;
    d._currentScript = s;
    try { J.host.evalScript(code, url); }
    finally { d._currentScript = prev; }
  }
  function execModule(s, code, url) {
    const d = G.document;
    d._currentScript = null;
    J.host.evalModule(code, url);
    void s;
  }
  const fireLoad = (s) => J.dispatchEvent(s, new G.Event("load"));
  const fireError = (s) => J.dispatchEvent(s, new G.Event("error"));

  // ---- import maps and module resolution ----
  const importMap = { imports: {}, scopes: {} };
  function addImportMap(text, base) {
    let m;
    try { m = JSON.parse(text); } catch (e) { G.__onError(new SyntaxError("Failed to parse import map: " + e.message)); return; }
    const norm = (o) => {
      const out = {};
      for (const k of Object.keys(o || {})) {
        try { out[k] = new G.URL(o[k], base).href; } catch (e) {}
      }
      return out;
    };
    Object.assign(importMap.imports, norm(m.imports));
    for (const sc of Object.keys(m.scopes || {})) {
      let key;
      try { key = new G.URL(sc, base).href; } catch (e) { continue; }
      importMap.scopes[key] = Object.assign(importMap.scopes[key] || {}, norm(m.scopes[sc]));
    }
  }
  function mapLookup(map, spec) {
    if (Object.prototype.hasOwnProperty.call(map, spec)) return map[spec];
    let best = null;
    for (const k of Object.keys(map)) if (k.endsWith("/") && spec.startsWith(k) && (!best || k.length > best.length)) best = k;
    return best ? map[best] + spec.slice(best.length) : null;
  }
  G.__resolve = function (base, spec) {
    if (base === "jsd:lib" || !/^[a-z]+:/i.test(base)) base = G.document.baseURI;
    let asURL = null;
    if (/^(\/|\.\/|\.\.\/)/.test(spec)) asURL = new G.URL(spec, base).href;
    else { try { asURL = new G.URL(spec).href; } catch (e) {} }
    const key = asURL || spec;
    const scopes = Object.keys(importMap.scopes).sort((a, b) => b.length - a.length);
    for (const sc of scopes) {
      if (base.startsWith(sc)) { const r = mapLookup(importMap.scopes[sc], key); if (r) return r; }
    }
    const r = mapLookup(importMap.imports, key);
    if (r) return r;
    if (asURL) return asURL;
    throw new TypeError("Failed to resolve module specifier \"" + spec + "\". Relative references must start with either \"/\", \"./\", or \"../\".");
  };
  G.__loadModule = function (url) { return fetchText(url, "module"); };

  // ---- running the parser's scripts in document order ----
  let deferred = [];
  let pendingAsync = 0;
  let loadFired = false;

  function nextScript(after) {
    const all = J.descendants(G.document);
    let i = after ? all.indexOf(after) + 1 : 0;
    for (; i < all.length; i++) {
      const n = all[i];
      if (n._tag === "script" && n._parserInserted && !n._started) return n;
    }
    return null;
  }

  function prepare(s, parser) {
    if (s._started) return;
    const kind = scriptKind(s);
    if (!kind) return;
    if (kind === "classic" && s.hasAttribute("nomodule")) return;
    s._started = true;
    if (!s.isConnected) return;
    const hasSrc = s.hasAttribute("src");
    if (kind === "importmap") { if (!hasSrc) addImportMap(s.textContent, G.document.baseURI); return; }
    if (!hasSrc) {
      const code = s.textContent;
      if (kind === "module") {
        if (parser && !s.hasAttribute("async")) deferred.push({ s, kind, code, url: G.document.URL });
        else G.setTimeout(() => execModule(s, code, G.document.URL), 0);
      } else execClassic(s, code, G.document.URL);
      return;
    }
    const url = absolute(s);
    if (!url || s.getAttribute("src") === "") { G.setTimeout(() => fireError(s), 0); return; }
    const isAsync = s.hasAttribute("async") || (!parser && s._forceAsync !== false);
    if (kind === "module" || (parser && s.hasAttribute("defer") && !s.hasAttribute("async"))) {
      if (isAsync && kind === "module") return runAsync(s, kind, url);
      if (!parser) return runAsync(s, kind, url);
      const job = { s, kind, url, code: null, failed: false };
      job.p = fetchTextAsync(url, kind).then((t) => { job.code = t; }, () => { job.failed = true; });
      deferred.push(job);
      return;
    }
    if (isAsync || !parser) return runAsync(s, kind, url);
    // Parser-blocking.
    let code;
    try { code = fetchText(url, kind); } catch (e) { fireError(s); return; }
    execClassic(s, code, url);
    fireLoad(s);
  }
  function runAsync(s, kind, url) {
    pendingAsync++;
    fetchTextAsync(url, kind).then((code) => {
      if (kind === "module") execModule(s, code, url); else execClassic(s, code, url);
      fireLoad(s);
    }, () => fireError(s)).finally(() => { pendingAsync--; maybeLoad(); });
  }
  // Scripts added by script run when connected.
  J.runInsertedScript = function (s) {
    s._started = false;
    prepare(s, false);
  };
  J.runScriptsIn = function (root) {
    if (!root) return;
    for (const s of J.descendants(root).filter((n) => n._tag === "script")) { s._parserInserted = true; }
    parse();
  };

  // document.write while parsing: the markup goes in after the running
  // script and is parsed with the rest of the document.
  J.documentWrite = function (doc, html) {
    const s = doc._currentScript;
    const frag = J.parseFragment(html, doc);
    for (const n of J.descendants(frag)) if (n._tag === "script") { n._parserInserted = true; n._started = false; }
    let parent = s && s.parentNode ? s.parentNode : doc.body || doc.documentElement;
    let before = s && s.parentNode ? (s._writeAnchor || s.nextSibling) : null;
    // Keep successive writes in order.
    const last = frag.lastChild;
    parent.insertBefore(frag, before);
    if (s && last) s._writeAnchor = last.nextSibling;
  };

  function parse() {
    const d = G.document;
    d._parsing = true;
    let s = nextScript(null);
    while (s) {
      try { prepare(s, true); } catch (e) { G.__onError(e); }
      s = nextScript(s);
    }
    d._parsing = false;
  }

  async function finishParsing() {
    const d = G.document;
    d._ready = "interactive";
    J.dispatchEvent(d, new G.Event("readystatechange"));
    for (const job of deferred.splice(0)) {
      try {
        if (job.p) await job.p;
        if (job.failed) { fireError(job.s); continue; }
        if (job.code !== null && job.code !== undefined) {
          if (job.kind === "module") execModule(job.s, job.code, job.url); else execClassic(job.s, job.code, job.url);
          if (job.p) fireLoad(job.s);
        }
      } catch (e) { G.__onError(e); }
    }
    J.dispatchEvent(d, new G.Event("DOMContentLoaded", { bubbles: true }));
    for (const a of d.querySelectorAll("audio[autoplay]")) a.play().catch(() => {});
    loadFired = false;
    maybeLoad();
  }
  function maybeLoad() {
    if (loadFired || pendingAsync > 0 || G.document._ready === "loading") return;
    loadFired = true;
    G.setTimeout(() => {
      const d = G.document;
      d._ready = "complete";
      J.dispatchEvent(d, new G.Event("readystatechange"));
      G.dispatchEvent(Object.assign(new G.Event("load"), {}));
      G.dispatchEvent(new G.PageTransitionEvent("pageshow", { persisted: false }));
      J.queue({ t: "loaded" });
    }, 0);
  }

  // ---- messages from the browser ----
  J.handlers.init = function (m) {
    J.setViewport(m.width || 640, m.height || 400);
    J.historyBefore = m.historyBefore || 0;
    J.historyAfter = m.historyAfter || 0;
    const d = J.newDocument(false);
    G.document = d;
    d._url = m.url;
    d._referrer = m.referrer || "";
    d._quirks = !!m.quirks;
    d._ready = "loading";
    J.history[0] = { url: m.url, state: null };
    try { G.origin = new G.URL(m.url).origin; } catch (e) {}
    G.isSecureContext = /^https:/.test(m.url) || /^http:\/\/(localhost|127\.)/.test(m.url);
    J.nextId = Math.max(J.nextId, m.next || 1);
    J.frameMs = m.graphical ? 16 : 100;
    // Build the page's tree; the browser already has these nodes.
    d.appendChild(new G.DocumentType("html"));
    J.building = true;
    try { for (const t of m.tree[3]) d.appendChild(J.build(t, d)); } finally { J.building = false; }
    J.ops.length = 0;
    for (const n of J.descendants(d)) if (n._tag === "script") n._parserInserted = true;
    for (const n of J.descendants(d)) if (n.nodeType === 1 && J.customElements.has(n._tag)) J.upgrade(n);
    for (const n of J.descendants(d)) if (n._tag === "img" || n._tag === "link") { if (n._connected) n._connected(); }
    parse();
    finishParsing().catch((e) => G.__onError(e));
  };

  // A user action on element `id`. Default actions of links, buttons,
  // checkboxes and forms run here (they come back as navigate/submit
  // messages and ops); the reply says whether the page cancelled the
  // event, so the browser knows whether to open a text field's editor.
  J.handlers.event = function (m) {
    const el = J.nodes.get(m.id) || G.document.body;
    let ok = true;
    const type = m.type;
    const k = { key: m.key || "", code: m.code || "", keyCode: m.keyCode || 0, which: m.keyCode || 0, bubbles: true, cancelable: true, composed: true, view: G, ctrlKey: !!m.ctrl, altKey: !!m.alt, shiftKey: !!m.shift };
    try {
      if (type === "click") {
        for (const t of ["pointerdown", "mousedown"]) if (!el._disabledCtl()) J.dispatchEvent(el, new G.MouseEvent(t, { bubbles: true, cancelable: true, view: G, button: 0, buttons: 1 }));
        if (el.focus && el.tabIndex >= 0) el.focus();
        for (const t of ["pointerup", "mouseup"]) J.dispatchEvent(el, new G.MouseEvent(t, { bubbles: true, cancelable: true, view: G, button: 0 }));
        if (!el._disabledCtl()) ok = J.activate(el, new G.MouseEvent("click", { bubbles: true, cancelable: true, composed: true, view: G, detail: 1, button: 0 }));
        J.reply(m, !ok);
        return;
      }
      if (type === "focus") { if (el.focus) el.focus(); }
      else if (type === "blur") { if (el.blur) el.blur(); }
      else if (type === "keydown" || type === "keyup" || type === "keypress") ok = J.dispatchEvent(el, new G.KeyboardEvent(type, k));
      else if (type === "submit") {
        // Enter in a text field: implicit submission.
        const f = el._tag === "form" ? el : el.form;
        if (f) {
          const btn = f._controls().find((c) => (c._tag === "button" && c.type === "submit") || (c._tag === "input" && (c.type === "submit" || c.type === "image")));
          if (btn) btn.click(); else f._submit(null);
          J.reply(m, false);
          return;
        }
      } else ok = J.dispatchEvent(el, new G.Event(type, { bubbles: true, cancelable: true }));
    } catch (e) { G.__onError(e); }
    J.reply(m, !ok);
  };
  G.Element.prototype._disabledCtl = function () { return /^(button|input|select|textarea)$/.test(this._tag) && J.isDisabled(this); };
  J.reply = (m, cancelled) => J.queue({ t: "eventDone", seq: m.seq, cancelled: !!cancelled });

  // The user changed a control: update its state, then input/change.
  J.handlers.input = function (m) {
    const el = J.nodes.get(m.id);
    if (!el) return;
    el._local = true; // the browser already has the new state
    try {
      if (m.value !== undefined) el.value = m.value;
      if (m.checked !== undefined) el.checked = m.checked;
      if (m.index !== undefined) el.selectedIndex = m.index;
    } finally { el._local = false; }
    J.ops = J.ops.filter((o) => !(o.id === el.__id && (o.op === "value" || o.op === "checked" || o.op === "selected")));
    try {
      if (m.value !== undefined && el._tag !== "select") J.dispatchEvent(el, new G.InputEvent("input", { bubbles: true, composed: true, inputType: "insertText", data: m.value }));
      else J.dispatchEvent(el, new G.Event("input", { bubbles: true, composed: true }));
      J.dispatchEvent(el, new G.Event("change", { bubbles: true }));
    } catch (e) { G.__onError(e); }
  };

  // Tell the browser which elements have click handlers.
  J.onFlush = function () {
    if (!J.clickDirty) return;
    J.clickDirty = false;
    const ids = [];
    for (const e of J.clickTargets) {
      if (e.isConnected && e.getRootNode() === G.document) ids.push(e.__id);
      else if (!e.isConnected) J.clickTargets.delete(e);
    }
    J.queue({ t: "clickable", ids });
  };

  J.handlers.resize = function (m) {
    J.setViewport(m.width, m.height);
    G.dispatchEvent(new G.UIEvent("resize"));
  };
  J.handlers.scroll = function (m) {
    G.scrollX = G.pageXOffset = m.x || 0;
    G.scrollY = G.pageYOffset = m.y || 0;
    J.dispatchEvent(G.document, new G.Event("scroll", { bubbles: true }));
  };
  J.handlers.unload = function (m) {
    const ev = new G.Event("beforeunload", { cancelable: true });
    G.dispatchEvent(ev);
    G.dispatchEvent(new G.PageTransitionEvent("pagehide", { persisted: false }));
    G.dispatchEvent(new G.Event("unload"));
    J.queue({ t: "unloaded", seq: m.seq });
  };
  J.handlers.eval = function (m) {
    let v;
    try { v = (0, eval)(m.code); v = typeof v === "string" ? v : (() => { try { return JSON.stringify(v) ?? String(v); } catch (e) { return String(v); } })(); }
    catch (e) { v = "Uncaught " + (e && e.name ? e.name + ": " + e.message : String(e)); }
    J.queue({ t: "evalResult", seq: m.seq, v });
  };
  J.handlers.storage = function (m) {
    G.dispatchEvent(new G.StorageEvent("storage", { key: m.key, oldValue: m.oldValue, newValue: m.newValue, url: m.url }));
  };
  J.handlers.online = function (m) {
    G.navigator.onLine = !!m.online;
    G.dispatchEvent(new G.Event(m.online ? "online" : "offline"));
  };
  // Inline handlers of elements, from DOMContentLoaded on: done by
  // J.handlerOf; nothing to do here.
})(globalThis);
