// Document: node factories, lookups, collections, cookie, title,
// document.write, focus, DOMParser and XMLSerializer. The page's
// document is `document`; others (DOMParser, createHTMLDocument) are
// local to jsd and never reach the browser until their nodes are
// inserted into the page.
"use strict";
(function (G) {
  const J = G.__jsd;
  const { Node } = G;

  class DOMImplementation {
    constructor(doc) { this._doc = doc; }
    createHTMLDocument(title) {
      const d = J.newDocument(true);
      const html = d.createElement("html");
      const head = d.createElement("head");
      html.appendChild(head);
      if (title !== undefined) { const t = d.createElement("title"); t.textContent = title; head.appendChild(t); }
      html.appendChild(d.createElement("body"));
      d.appendChild(new G.DocumentType("html"));
      d.appendChild(html);
      return d;
    }
    createDocument() { return this.createHTMLDocument(); }
    createDocumentType(name) { return new G.DocumentType(name); }
    hasFeature() { return true; }
  }

  class Document extends Node {
    constructor() {
      super();
      this._localDoc = true;
      this._url = "about:blank";
      this._ready = "complete";
    }
    get nodeType() { return 9; }
    get nodeName() { return "#document"; }
    get implementation() { return this._impl || (this._impl = new DOMImplementation(this)); }
    get documentElement() { return this._c.find((c) => c.nodeType === 1) || null; }
    get doctype() { return this._c.find((c) => c.nodeType === 10) || null; }
    get head() { const h = this.documentElement; return h ? h._c.find((c) => c._tag === "head") || null : null; }
    get body() { const h = this.documentElement; return h ? h._c.find((c) => c._tag === "body" || c._tag === "frameset") || null : null; }
    set body(b) {
      const h = this.documentElement;
      const old = this.body;
      if (old) h.replaceChild(b, old); else if (h) h.appendChild(b);
    }
    get scrollingElement() { return this.documentElement; }
    get URL() { return this._url; }
    get documentURI() { return this._url; }
    get baseURI() {
      const b = J.descendants(this).find((e) => e._tag === "base" && e.hasAttribute("href"));
      if (b) { try { return new G.URL(b.getAttribute("href"), this._url).href; } catch (e) {} }
      return this._url;
    }
    get compatMode() { return this._quirks ? "BackCompat" : "CSS1Compat"; }
    get characterSet() { return "UTF-8"; }
    get charset() { return "UTF-8"; }
    get inputEncoding() { return "UTF-8"; }
    get contentType() { return "text/html"; }
    get readyState() { return this._ready; }
    get defaultView() { return this._localDoc ? null : G.window; }
    get location() { return this._localDoc ? null : G.location; }
    set location(v) { if (!this._localDoc) G.location.href = v; }
    get referrer() { return this._referrer || ""; }
    get lastModified() { const d = new Date(); const p = (n) => String(n).padStart(2, "0"); return p(d.getMonth() + 1) + "/" + p(d.getDate()) + "/" + d.getFullYear() + " " + p(d.getHours()) + ":" + p(d.getMinutes()) + ":" + p(d.getSeconds()); }
    get domain() { try { return new G.URL(this._url).hostname; } catch (e) { return ""; } }
    set domain(v) {}
    get visibilityState() { return "visible"; }
    get hidden() { return false; }
    get fullscreenEnabled() { return false; }
    get fullscreenElement() { return null; }
    get pictureInPictureEnabled() { return false; }
    get designMode() { return "off"; }
    set designMode(v) {}
    get dir() { const h = this.documentElement; return h ? h.getAttribute("dir") || "" : ""; }
    set dir(v) { const h = this.documentElement; if (h) h.setAttribute("dir", v); }
    get title() {
      const t = J.descendants(this).find((e) => e._tag === "title");
      return t ? t.textContent.replace(/[\t\n\f\r ]+/g, " ").trim() : "";
    }
    set title(v) {
      let t = J.descendants(this).find((e) => e._tag === "title");
      if (!t) {
        const h = this.head;
        if (!h) return;
        t = this.createElement("title");
        h.appendChild(t);
      }
      t.textContent = v;
    }
    get cookie() { return this._localDoc ? "" : J.rpc("cookie", null) || ""; }
    set cookie(v) { if (!this._localDoc) J.rpc("setCookie", { v: String(v) }); }
    get activeElement() {
      const a = this._active;
      return a && a.isConnected ? a : this.body;
    }
    hasFocus() { return !this._localDoc; }
    get currentScript() { return this._currentScript || null; }
    get styleSheets() { return J.styleSheets ? J.styleSheets(this) : []; }
    get fonts() { return J.fonts || (J.fonts = { ready: Promise.resolve(), status: "loaded", check: () => true, load: () => Promise.resolve([]), add() {}, forEach() {}, addEventListener() {} }); }
    get timeline() { return { currentTime: G.performance.now() }; }

    // ---- collections ----
    _all(pred) { return J.liveList(() => J.descendants(this).filter((e) => e.nodeType === 1 && pred(e)), G.HTMLCollection); }
    get forms() { return this._all((e) => e._tag === "form"); }
    get images() { return this._all((e) => e._tag === "img"); }
    get links() { return this._all((e) => (e._tag === "a" || e._tag === "area") && e.hasAttribute("href")); }
    get anchors() { return this._all((e) => e._tag === "a" && e.hasAttribute("name")); }
    get scripts() { return this._all((e) => e._tag === "script"); }
    get embeds() { return this._all((e) => e._tag === "embed"); }
    get plugins() { return this.embeds; }
    get applets() { return this._all(() => false); }
    get all() {
      const coll = this._all(() => true);
      return coll;
    }
    getElementById(id) {
      id = String(id);
      if (!id) return null;
      const walk = (n) => {
        for (const c of n._c) {
          if (c.nodeType !== 1) continue;
          if (c.getAttribute("id") === id) return c;
          const r = walk(c);
          if (r) return r;
        }
        return null;
      };
      return walk(this);
    }
    getElementsByName(n) { return J.liveList(() => J.descendants(this).filter((e) => e.nodeType === 1 && e.getAttribute("name") === String(n)), G.NodeList); }

    // ---- factories ----
    // (J.build uses this to create parsed elements with the browser's ids)
    _createElementWithId(tag, id, attrs) {
      tag = String(tag).toLowerCase();
      const custom = J.customElements.get(tag);
      const Cls = custom || J.tagClass[tag] || (tag.includes("-") ? G.HTMLElement : (KNOWN.has(tag) ? G.HTMLElement : G.HTMLUnknownElement));
      J.making = { tag, id, local: this._localDoc };
      let e;
      try { e = new Cls(); } finally { J.making = null; }
      if (custom) e._customUpgraded = true;
      e._doc = this;
      for (const [k, v] of attrs || []) {
        e._attrs.push([String(k).toLowerCase(), String(v)]);
        if (!e._local) J.ops.push({ op: "attr", id: e.__id, name: String(k).toLowerCase(), value: String(v) });
      }
      if (tag === "form") {
        const p = J.formProxy(e);
        J.nodes.set(e.__id, p);
        return p;
      }
      return e;
    }
    createElement(tag, opts) {
      tag = String(tag);
      if (!/^[A-Za-z][^\s/>\x00]*$/.test(tag)) throw new G.DOMException("The tag name provided ('" + tag + "') is not a valid name.", "InvalidCharacterError");
      const is = opts && typeof opts === "object" ? opts.is : undefined;
      const e = this._createElementWithId(tag, 0, is ? [["is", is]] : []);
      return e;
    }
    createElementNS(ns, q) { const e = this.createElement(q.includes(":") ? q.split(":")[1] : q); if (ns && ns !== "http://www.w3.org/1999/xhtml") e._ns = ns; return e; }
    createTextNode(s) {
      J.makeLocal = this._localDoc;
      let t;
      try { t = new G.Text(s); } finally { J.makeLocal = false; }
      t._doc = this;
      if (this._localDoc) t._foreign = true;
      return t;
    }
    createComment(s) { const c = new G.Comment(s); c._doc = this; return c; }
    createCDATASection(s) { return this.createTextNode(s); }
    createProcessingInstruction(t, d) { return this.createComment("?" + t + " " + d + "?"); }
    createDocumentFragment() { const f = new G.DocumentFragment(); f._doc = this; return f; }
    createAttribute(n) { const a = new G.Attr(null, String(n).toLowerCase()); a._v = ""; return a; }
    createEvent(kind) {
      const k = String(kind).toLowerCase().replace(/s$/, "");
      const map = { event: G.Event, htmlevent: G.Event, uievent: G.UIEvent, mouseevent: G.MouseEvent, keyboardevent: G.KeyboardEvent, customevent: G.CustomEvent, focusevent: G.FocusEvent, messageevent: G.MessageEvent, hashchangeevent: G.HashChangeEvent };
      const C = map[k];
      if (!C) throw new G.DOMException("The provided event type ('" + kind + "') is invalid.", "NotSupportedError");
      const e = new C("");
      e._uninit = true;
      return e;
    }
    createRange() { return new G.Range(); }
    createTreeWalker(root, what, filter) { return new G.TreeWalker(root, what, filter); }
    createNodeIterator(root, what, filter) { return new G.NodeIterator(root, what, filter); }
    importNode(n, deep) { return this.adoptNode(n.cloneNode(!!deep)); }
    adoptNode(n) {
      if (n.parentNode) n.parentNode.removeChild(n);
      n._setDoc(this);
      return n;
    }
    // ---- document.write ----
    open() {
      if (this._localDoc) return this;
      if (this._parsing) return this;
      // After load: replace the document with what is written.
      for (const c of this._c.slice()) this.removeChild(c);
      this._reopened = "";
      return this;
    }
    close() {
      if (this._reopened !== undefined && this._reopened !== null) {
        const html = this._reopened;
        this._reopened = null;
        J.replaceDocument(this, html);
      }
    }
    write(...parts) {
      const html = parts.join("");
      if (this._localDoc) return;
      if (this._reopened !== undefined && this._reopened !== null) { this._reopened += html; return; }
      if (this._parsing) { J.documentWrite(this, html); return; }
      // Writing after load implicitly reopens the document.
      this.open();
      this._reopened += html;
      G.setTimeout(() => this.close(), 0);
    }
    writeln(...parts) { this.write(...parts, "\n"); }
    // ---- misc ----
    elementFromPoint(x, y) {
      if (this._localDoc) return null;
      const id = J.rpc("hitTest", { x, y });
      return (id && J.nodes.get(id)) || this.body;
    }
    elementsFromPoint(x, y) { const e = this.elementFromPoint(x, y); const out = []; for (let n = e; n && n.nodeType === 1; n = n.parentNode) out.push(n); return out; }
    caretRangeFromPoint() { return null; }
    getSelection() { return G.getSelection(); }
    execCommand() { return false; }
    queryCommandSupported() { return false; }
    queryCommandEnabled() { return false; }
    exitFullscreen() { return Promise.resolve(); }
    startViewTransition(cb) { const p = Promise.resolve().then(() => cb && cb()); return { finished: p, ready: p, updateCallbackDone: p, skipTransition() {} }; }
  }
  const KNOWN = new Set("a abbr address area article aside audio b base bdi bdo blockquote body br button canvas caption cite code col colgroup data datalist dd del details dfn dialog div dl dt em embed fieldset figcaption figure footer form h1 h2 h3 h4 h5 h6 head header hgroup hr html i iframe img input ins kbd label legend li link main map mark menu meta meter nav noscript object ol optgroup option output p param picture pre progress q rp rt ruby s samp script search section select slot small source span strong style sub summary sup table tbody td template textarea tfoot th thead time title tr track u ul var video wbr center font big tt strike nobr noframes frame frameset marquee acronym basefont listing xmp plaintext noembed svg math".split(" "));
  class HTMLUnknownElement extends G.HTMLElement {}
  G.HTMLUnknownElement = HTMLUnknownElement;

  class HTMLDocument extends Document {}

  J.newDocument = function (local) {
    const d = new HTMLDocument();
    d._localDoc = !!local;
    d._doc = d;
    J.register(d, local ? J.nextId++ : 0);
    if (local) d._local = true;
    return d;
  };

  // Rebuild a document from HTML (document.open/write/close after load;
  // DOMParser).
  J.replaceDocument = function (doc, html) {
    const tree = J.rpc("parseDocument", { html: String(html), first: J.nextId });
    for (const c of doc._c.slice()) doc.removeChild(c);
    const save = J.making;
    for (const t of tree) doc.appendChild(J.build(t, doc));
    J.making = save;
    if (!doc._localDoc) J.runScriptsIn && J.runScriptsIn(doc.documentElement);
  };

  class DOMParser {
    parseFromString(s, type) {
      const d = J.newDocument(true);
      type = String(type || "text/html").toLowerCase();
      if (type !== "text/html") d._xml = type;
      const tree = J.rpc("parseDocument", { html: String(s), first: J.nextId });
      for (const t of tree) d.appendChild(J.build(t, d));
      return d;
    }
  }
  class XMLSerializer {
    serializeToString(n) { return n.nodeType === 9 ? J.serializeChildren(n) : J.serialize(n); }
  }

  // A basic Selection: the browser's selection is not exposed.
  const selection = {
    anchorNode: null, anchorOffset: 0, focusNode: null, focusOffset: 0, isCollapsed: true, rangeCount: 0, type: "None",
    toString: () => "", removeAllRanges() {}, addRange() {}, getRangeAt() { return new G.Range(); }, collapse() {}, collapseToEnd() {},
    collapseToStart() {}, selectAllChildren() {}, empty() {}, containsNode: () => false, extend() {}, setBaseAndExtent() {},
  };
  G.getSelection = () => selection;

  Object.assign(G, { Document, HTMLDocument, DOMImplementation, DOMParser, XMLSerializer });
})(globalThis);
