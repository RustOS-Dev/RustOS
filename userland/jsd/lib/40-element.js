// Elements: attributes, classList, style, dataset, geometry (asked of the
// browser's layout), focus and click, reflected properties, HTML element
// classes including form controls, and custom elements.
"use strict";
(function (G) {
  const J = G.__jsd;
  const { Node } = G;
  const registry = new Map();
  const waiting = new Map();

  // ---- attributes ----
  class Attr {
    constructor(el, name) { this.ownerElement = el; this.name = name; this.localName = name; this.namespaceURI = null; this.prefix = null; this.specified = true; }
    get value() { return this.ownerElement ? this.ownerElement.getAttribute(this.name) : this._v; }
    set value(v) { if (this.ownerElement) this.ownerElement.setAttribute(this.name, v); else this._v = String(v); }
    get nodeName() { return this.name; }
    get nodeValue() { return this.value; }
    get nodeType() { return 2; }
  }
  class NamedNodeMap {
    constructor(el) { Object.defineProperty(this, "_el", { value: el }); }
    get length() { return this._el._attrs.length; }
    item(i) { const a = this._el._attrs[i]; return a ? new Attr(this._el, a[0]) : null; }
    getNamedItem(n) { return this._el.hasAttribute(n) ? new Attr(this._el, String(n).toLowerCase()) : null; }
    setNamedItem(a) { this._el.setAttribute(a.name, a.value); }
    removeNamedItem(n) { const a = this.getNamedItem(n); this._el.removeAttribute(n); return a; }
    [Symbol.iterator]() { return this._el._attrs.map((a) => new Attr(this._el, a[0]))[Symbol.iterator](); }
  }

  class DOMTokenList {
    constructor(el, attr) { this._el = el; this._attr = attr; }
    _get() { return (this._el.getAttribute(this._attr) || "").split(/\s+/).filter(Boolean); }
    _set(list) { this._el.setAttribute(this._attr, list.join(" ")); }
    get length() { return this._get().length; }
    get value() { return this._el.getAttribute(this._attr) || ""; }
    set value(v) { this._el.setAttribute(this._attr, v); }
    item(i) { return this._get()[i] ?? null; }
    contains(t) { return this._get().includes(String(t)); }
    add(...ts) { const l = this._get(); let ch = false; for (const t of ts) if (!l.includes(t)) { l.push(String(t)); ch = true; } if (ch || !this._el.hasAttribute(this._attr)) this._set(l); }
    remove(...ts) { const l = this._get(); const n = l.filter((x) => !ts.includes(x)); if (n.length !== l.length) this._set(n); }
    toggle(t, force) {
      const has = this.contains(t);
      const want = force === undefined ? !has : !!force;
      if (want && !has) this.add(t); else if (!want && has) this.remove(t);
      return want;
    }
    replace(a, b) { const l = this._get(); const i = l.indexOf(a); if (i < 0) return false; l[i] = b; this._set(l); return true; }
    supports() { return true; }
    forEach(f, t) { this._get().forEach(f, t); }
    toString() { return this.value; }
    [Symbol.iterator]() { return this._get()[Symbol.iterator](); }
  }

  // ---- style attribute as CSSStyleDeclaration ----
  const kebab = (p) => p === "cssFloat" ? "float" : p.replace(/^webkit/, "-webkit-").replace(/^moz/, "-moz-").replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
  function parseDecls(text) {
    const out = [];
    let depth = 0, cur = "", q = null;
    for (const c of text || "") {
      if (q) { cur += c; if (c === q) q = null; continue; }
      if (c === '"' || c === "'") { q = c; cur += c; continue; }
      if (c === "(") depth++;
      if (c === ")") depth--;
      if (c === ";" && depth === 0) { out.push(cur); cur = ""; continue; }
      cur += c;
    }
    out.push(cur);
    const m = [];
    for (const d of out) {
      const i = d.indexOf(":");
      if (i < 0) continue;
      const name = d.slice(0, i).trim().toLowerCase();
      let value = d.slice(i + 1).trim();
      let prio = "";
      const im = /!\s*important\s*$/i.exec(value);
      if (im) { prio = "important"; value = value.slice(0, im.index).trim(); }
      if (name) m.push([name.startsWith("--") ? d.slice(0, i).trim() : name, value, prio]);
    }
    return m;
  }
  class CSSStyleDeclaration {
    constructor(el) { Object.defineProperty(this, "_el", { value: el }); }
    _read() { return this._el ? parseDecls(this._el.getAttribute("style")) : (this._m || []); }
    _write(m) {
      const text = m.map(([k, v, p]) => k + ": " + v + (p ? " !important" : "") + ";").join(" ");
      if (this._el) { if (text) this._el.setAttribute("style", text); else this._el.removeAttribute("style"); }
      else this._m = m;
    }
    get cssText() { return this._read().map(([k, v, p]) => k + ": " + v + (p ? " !important" : "") + ";").join(" "); }
    set cssText(t) { this._write(parseDecls(String(t))); }
    get length() { return this._read().length; }
    item(i) { const d = this._read()[i]; return d ? d[0] : ""; }
    getPropertyValue(n) { n = String(n); n = n.startsWith("--") ? n : n.toLowerCase(); const d = this._read().find((x) => x[0] === n); return d ? d[1] : ""; }
    getPropertyPriority(n) { const d = this._read().find((x) => x[0] === n); return d ? d[2] : ""; }
    setProperty(n, v, prio) {
      n = String(n); n = n.startsWith("--") ? n : n.toLowerCase();
      if (v === null || v === undefined || v === "") return this.removeProperty(n);
      const m = this._read().filter((x) => x[0] !== n);
      m.push([n, String(v), prio === "important" ? "important" : ""]);
      this._write(m);
    }
    removeProperty(n) {
      const m = this._read();
      const old = this.getPropertyValue(n);
      const k = m.filter((x) => x[0] !== n);
      if (k.length !== m.length) this._write(k);
      return old;
    }
  }
  function styleProxy(el) {
    const decl = new CSSStyleDeclaration(el);
    return new Proxy(decl, {
      get(t, p, r) {
        if (typeof p !== "string" || p in t || p.startsWith("_")) return Reflect.get(t, p, r);
        if (/^\d+$/.test(p)) return t.item(Number(p));
        return t.getPropertyValue(kebab(p));
      },
      set(t, p, v, r) {
        if (typeof p !== "string" || p in t || p.startsWith("_")) return Reflect.set(t, p, v, r);
        t.setProperty(kebab(p), v);
        return true;
      },
    });
  }
  J.CSSStyleDeclaration = CSSStyleDeclaration;
  J.styleProxy = styleProxy;

  function datasetProxy(el) {
    const toAttr = (k) => "data-" + k.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
    const toKey = (a) => a.slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    return new Proxy({}, {
      get(t, k) { return typeof k === "string" ? el.getAttribute(toAttr(k)) ?? undefined : undefined; },
      set(t, k, v) { el.setAttribute(toAttr(String(k)), String(v)); return true; },
      deleteProperty(t, k) { el.removeAttribute(toAttr(String(k))); return true; },
      has(t, k) { return el.hasAttribute(toAttr(String(k))); },
      ownKeys() { return el._attrs.filter(([a]) => a.startsWith("data-")).map(([a]) => toKey(a)); },
      getOwnPropertyDescriptor(t, k) { const v = el.getAttribute(toAttr(String(k))); return v === null ? undefined : { value: v, enumerable: true, configurable: true, writable: true }; },
    });
  }

  class DOMRect {
    constructor(x = 0, y = 0, w = 0, h = 0) { this.x = x; this.y = y; this.width = w; this.height = h; }
    get left() { return Math.min(this.x, this.x + this.width); }
    get right() { return Math.max(this.x, this.x + this.width); }
    get top() { return Math.min(this.y, this.y + this.height); }
    get bottom() { return Math.max(this.y, this.y + this.height); }
    toJSON() { return { x: this.x, y: this.y, width: this.width, height: this.height, top: this.top, right: this.right, bottom: this.bottom, left: this.left }; }
    static fromRect(r) { r = r || {}; return new DOMRect(r.x, r.y, r.width, r.height); }
  }

  // Layout boxes of an element, from the browser (cached per DOM version).
  function rects(el) {
    if (!el.isConnected) return [];
    if (el._rv !== J.version) {
      J.flushOps();
      el._rects = J.rpc("rects", { id: el.__id }) || [];
      el._rv = J.version;
    }
    return el._rects;
  }

  // ---- Element ----
  class Element extends Node {
    // Elements are made by document.createElement (J.making holds the tag
    // and id), by `new` on a registered custom element class, or when an
    // existing element is upgraded (J.upgrading).
    constructor() {
      if (J.upgrading) { const e = J.upgrading; J.upgrading = null; return e; }
      let m = J.making;
      J.making = null;
      if (!m) {
        const name = G.customElements.getName(new.target);
        if (!name) throw new TypeError("Illegal constructor");
        m = { tag: name, id: 0 };
      }
      super();
      this._tag = m.tag;
      this._attrs = [];
      J.register(this, m.id || J.nextId++);
      if (m.local) { this._local = true; this._foreign = true; }
      else J.ops.push({ op: "create", id: this.__id, tag: m.tag });
      if (!m.id && registry.has(m.tag)) this._customUpgraded = true;
    }
    get nodeType() { return 1; }
    get nodeName() { return this.tagName; }
    get tagName() { return this._tag.toUpperCase(); }
    get localName() { return this._tag; }
    get namespaceURI() { return this._ns || "http://www.w3.org/1999/xhtml"; }
    get prefix() { return null; }
    get attributes() { return this._am || (this._am = new NamedNodeMap(this)); }
    hasAttributes() { return this._attrs.length > 0; }
    getAttributeNames() { return this._attrs.map((a) => a[0]); }
    hasAttribute(n) { n = String(n).toLowerCase(); return this._attrs.some((a) => a[0] === n); }
    getAttribute(n) { n = String(n).toLowerCase(); const a = this._attrs.find((x) => x[0] === n); return a ? a[1] : null; }
    getAttributeNS(ns, n) { return this.getAttribute(n); }
    setAttribute(n, v) {
      n = String(n).toLowerCase();
      if (!/^[^\s"'>\/=\x00-\x1f]+$/.test(n)) throw new G.DOMException("'" + n + "' is not a valid attribute name", "InvalidCharacterError");
      v = String(v);
      const a = this._attrs.find((x) => x[0] === n);
      const old = a ? a[1] : null;
      if (a) a[1] = v; else this._attrs.push([n, v]);
      this._attrChanged(n, old, v);
    }
    setAttributeNS(ns, n, v) { this.setAttribute(n.includes(":") ? n.split(":")[1] : n, v); }
    removeAttribute(n) {
      n = String(n).toLowerCase();
      const i = this._attrs.findIndex((x) => x[0] === n);
      if (i < 0) return;
      const old = this._attrs[i][1];
      this._attrs.splice(i, 1);
      this._attrChanged(n, old, null);
    }
    removeAttributeNS(ns, n) { this.removeAttribute(n); }
    toggleAttribute(n, force) {
      const has = this.hasAttribute(n);
      const want = force === undefined ? !has : !!force;
      if (want && !has) this.setAttribute(n, ""); else if (!want && has) this.removeAttribute(n);
      return want;
    }
    getAttributeNode(n) { return this.attributes.getNamedItem(n); }
    setAttributeNode(a) { this.setAttribute(a.name, a.value); }
    _attrChanged(n, old, v) {
      J.version++;
      if (!this._local) J.ops.push({ op: "attr", id: this.__id, name: n, value: v });
      J.queueAttrRecord(this, n, old);
      if (this._customUpgraded && typeof this.attributeChangedCallback === "function") {
        const obs = this.constructor.observedAttributes || [];
        if (obs.includes(n)) {
          try { this.attributeChangedCallback(n, old, v); } catch (e) { G.__onError(e); }
        }
      }
      if (this._onAttr) this._onAttr(n, v);
      if ((n === "id" || n === "name") && v && this.getRootNode() === G.document) J.exposeNamed(this);
    }
    get id() { return this.getAttribute("id") || ""; }
    set id(v) { this.setAttribute("id", v); }
    get className() { return this.getAttribute("class") || ""; }
    set className(v) { this.setAttribute("class", v); }
    get classList() { return this._cls || (this._cls = new DOMTokenList(this, "class")); }
    set classList(v) { this.setAttribute("class", v); }
    get slot() { return this.getAttribute("slot") || ""; }
    matches(s) { return J.matches(this, s, this); }
    webkitMatchesSelector(s) { return this.matches(s); }
    msMatchesSelector(s) { return this.matches(s); }
    closest(s) { for (let e = this; e && e.nodeType === 1; e = e.parentNode) if (J.matches(e, s, null)) return e; return null; }
    get innerHTML() { return J.serializeChildren(this._tag === "template" ? this.content : this); }
    set innerHTML(html) {
      const target = this._tag === "template" ? this.content : this;
      for (const c of target._c.slice()) target.removeChild(c);
      if (html !== "" && html != null) target.appendChild(J.parseFragment(String(html), this.ownerDocument));
    }
    get outerHTML() { return J.serialize(this); }
    set outerHTML(html) {
      const p = this.parentNode;
      if (!p) return;
      p.insertBefore(J.parseFragment(String(html), this.ownerDocument), this);
      p.removeChild(this);
    }
    setHTMLUnsafe(html) { this.innerHTML = html; }
    getHTML() { return this.innerHTML; }
    insertAdjacentHTML(pos, html) { this._adjacent(pos, J.parseFragment(String(html), this.ownerDocument)); }
    insertAdjacentElement(pos, el) { return this._adjacent(pos, el); }
    insertAdjacentText(pos, t) { this._adjacent(pos, this.ownerDocument.createTextNode(t)); }
    _adjacent(pos, n) {
      switch (String(pos).toLowerCase()) {
        case "beforebegin": if (this.parentNode) this.parentNode.insertBefore(n, this); break;
        case "afterbegin": this.insertBefore(n, this.firstChild); break;
        case "beforeend": this.appendChild(n); break;
        case "afterend": if (this.parentNode) this.parentNode.insertBefore(n, this.nextSibling); break;
        default: throw new G.DOMException("bad position", "SyntaxError");
      }
      return n;
    }
    get innerText() { return J.innerText(this); }
    set innerText(v) { this.textContent = v; }
    get outerText() { return this.innerText; }
    // Geometry (CSS px of the browser's layout).
    getBoundingClientRect() {
      const r = rects(this);
      if (!r.length) return new DOMRect();
      let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
      for (const q of r) { x0 = Math.min(x0, q[0]); y0 = Math.min(y0, q[1]); x1 = Math.max(x1, q[0] + q[2]); y1 = Math.max(y1, q[1] + q[3]); }
      return new DOMRect(x0 - G.scrollX, y0 - G.scrollY, x1 - x0, y1 - y0);
    }
    getClientRects() { return rects(this).map((q) => new DOMRect(q[0] - G.scrollX, q[1] - G.scrollY, q[2], q[3])); }
    get offsetWidth() { return Math.round(this.getBoundingClientRect().width); }
    get offsetHeight() { return Math.round(this.getBoundingClientRect().height); }
    get offsetTop() { return Math.round(this.getBoundingClientRect().top + G.scrollY); }
    get offsetLeft() { return Math.round(this.getBoundingClientRect().left + G.scrollX); }
    get offsetParent() { return this.isConnected ? this.ownerDocument.body : null; }
    get clientWidth() { return this.offsetWidth; }
    get clientHeight() { return this.offsetHeight; }
    get clientTop() { return 0; }
    get clientLeft() { return 0; }
    get scrollWidth() { return this.offsetWidth; }
    get scrollHeight() { return this.offsetHeight; }
    get scrollTop() { return 0; }
    set scrollTop(v) {}
    get scrollLeft() { return 0; }
    set scrollLeft(v) {}
    scrollIntoView() { if (this.isConnected) J.queue({ t: "scrollTo", id: this.__id }); }
    scrollTo() {}
    scrollBy() {}
    scroll() {}
    focus() {
      const d = this.ownerDocument;
      if (!this.isConnected || d._active === this) return;
      const old = d._active;
      if (old) old.blur();
      d._active = this;
      J.queue({ t: "focus", id: this.__id });
      J.dispatchEvent(this, new G.FocusEvent("focus", {}));
      J.dispatchEvent(this, new G.FocusEvent("focusin", { bubbles: true }));
    }
    blur() {
      const d = this.ownerDocument;
      if (d._active !== this) return;
      d._active = null;
      J.dispatchEvent(this, new G.FocusEvent("blur", {}));
      J.dispatchEvent(this, new G.FocusEvent("focusout", { bubbles: true }));
    }
    click() {
      if (this._clicking) return;
      this._clicking = true;
      try { J.activate(this, new G.MouseEvent("click", { bubbles: true, cancelable: true, composed: true, view: G.window, detail: 1 })); }
      finally { this._clicking = false; }
    }
    attachShadow(init) {
      const root = this.ownerDocument.createDocumentFragment();
      root.host = this;
      root.mode = init && init.mode;
      this._shadow = root;
      return root;
    }
    get shadowRoot() { return this._shadow && this._shadow.mode === "open" ? this._shadow : null; }
    animate() { return { finished: Promise.resolve(), cancel() {}, play() {}, pause() {}, onfinish: null, addEventListener() {} }; }
    getAnimations() { return []; }
    requestFullscreen() { return Promise.reject(new TypeError("not supported")); }
    setPointerCapture() {}
    releasePointerCapture() {}
    hasPointerCapture() { return false; }
    checkVisibility() { return rects(this).length > 0; }
  }

  // Activation behavior of a click: checkboxes toggle first (undone if
  // cancelled); links navigate, buttons submit and labels forward the
  // click when not cancelled.
  J.activate = function (el, ev) {
    let undo = null;
    if (el._tag === "input" && (el.type === "checkbox" || el.type === "radio") && !el.disabled) {
      const was = el.checked;
      if (el.type === "checkbox") el.checked = !was;
      else if (!was) {
        const others = el._radioGroup();
        const prev = others.find((r) => r.checked);
        el.checked = true;
        undo = () => { el.checked = false; if (prev) prev.checked = true; };
      }
      if (!undo) undo = () => { el.checked = was; };
    }
    const ok = J.dispatchEvent(el, ev);
    if (!ok) { if (undo) undo(); return false; }
    if (undo && el.type !== undefined) {
      J.dispatchEvent(el, new G.Event("input", { bubbles: true, composed: true }));
      J.dispatchEvent(el, new G.Event("change", { bubbles: true }));
    }
    // Default actions, from the target outward.
    for (let n = el; n && n.nodeType === 1; n = n.parentNode) {
      if (n._activation) { n._activation(ev); break; }
    }
    return true;
  };

  J.queueAttrRecord = function (el, name, old) {
    J.queueRecord(el, { type: "attributes", target: el, attributeName: name, oldValue: old });
  };

  // innerText: text with line breaks at block boundaries (approximate,
  // from the elements' display types in the UA style).
  const BLOCK = new Set(["address", "article", "aside", "blockquote", "details", "dialog", "dd", "div", "dl", "dt", "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hgroup", "hr", "li", "main", "nav", "ol", "p", "pre", "section", "table", "tr", "ul", "summary"]);
  J.innerText = function (el) {
    let out = "";
    const walk = (n) => {
      for (const c of n._c) {
        if (c.nodeType === 3) out += c._data.replace(/\s+/g, " ");
        else if (c.nodeType === 1) {
          if (/^(script|style|template|head|noscript)$/.test(c._tag) || c.hidden || (c.getAttribute("style") || "").replace(/\s/g, "").includes("display:none")) continue;
          if (c._tag === "br") { out += "\n"; continue; }
          const block = BLOCK.has(c._tag);
          if (block && out && !out.endsWith("\n")) out += "\n";
          if (c._tag === "td" || c._tag === "th") { if (out && !out.endsWith("\n") && !out.endsWith("\t")) out += "\t"; }
          walk(c);
          if (block && !out.endsWith("\n")) out += "\n";
        }
      }
    };
    walk(el);
    return out.replace(/ *\n */g, "\n").replace(/\n{3,}/g, "\n\n").replace(/^\n+|\n+$/g, "");
  };

  // ---- reflection helpers ----
  function refStr(proto, prop, attr, def) {
    attr = attr || prop.toLowerCase();
    Object.defineProperty(proto, prop, { configurable: true, get() { return this.getAttribute(attr) ?? (def || ""); }, set(v) { this.setAttribute(attr, v); } });
  }
  function refBool(proto, prop, attr) {
    attr = attr || prop.toLowerCase();
    Object.defineProperty(proto, prop, { configurable: true, get() { return this.hasAttribute(attr); }, set(v) { this.toggleAttribute(attr, !!v); } });
  }
  function refNum(proto, prop, attr, def) {
    attr = attr || prop.toLowerCase();
    Object.defineProperty(proto, prop, { configurable: true, get() { const v = parseInt(this.getAttribute(attr), 10); return Number.isFinite(v) ? v : def; }, set(v) { this.setAttribute(attr, String(Math.trunc(v))); } });
  }
  function refUrl(proto, prop, attr) {
    attr = attr || prop.toLowerCase();
    Object.defineProperty(proto, prop, {
      configurable: true,
      get() { const v = this.getAttribute(attr); if (v === null) return ""; try { return new G.URL(v, this.ownerDocument.baseURI).href; } catch (e) { return v; } },
      set(v) { this.setAttribute(attr, v); },
    });
  }
  J.refStr = refStr; J.refBool = refBool; J.refNum = refNum; J.refUrl = refUrl;

  class HTMLElement extends Element {
    get style() { return this._st || (this._st = styleProxy(this)); }
    set style(v) { this.setAttribute("style", v); }
    get dataset() { return this._ds || (this._ds = datasetProxy(this)); }
    get hidden() { return this.hasAttribute("hidden"); }
    set hidden(v) { this.toggleAttribute("hidden", !!v); }
    get tabIndex() { const v = parseInt(this.getAttribute("tabindex"), 10); return Number.isFinite(v) ? v : (/^(a|button|input|select|textarea)$/.test(this._tag) ? 0 : -1); }
    set tabIndex(v) { this.setAttribute("tabindex", String(v)); }
    get isContentEditable() { return this.getAttribute("contenteditable") === "true" || this.getAttribute("contenteditable") === ""; }
    get contentEditable() { return this.getAttribute("contenteditable") ?? "inherit"; }
    set contentEditable(v) { this.setAttribute("contenteditable", v); }
    get inert() { return this.hasAttribute("inert"); }
    set inert(v) { this.toggleAttribute("inert", !!v); }
    showPopover() { this.setAttribute("popover-open", ""); }
    hidePopover() { this.removeAttribute("popover-open"); }
    togglePopover() { this.toggleAttribute("popover-open"); }
  }
  for (const p of ["title", "lang", "dir", "accessKey", "translate", "autocapitalize", "enterKeyHint", "inputMode", "nonce", "popover"]) refStr(HTMLElement.prototype, p);
  refBool(HTMLElement.prototype, "draggable");
  refBool(HTMLElement.prototype, "spellcheck");
  refBool(HTMLElement.prototype, "autofocus");

  const classes = {};
  function def(name, tags, body) {
    classes[name] = body;
    for (const t of tags) J.tagClass[t] = body;
    G[name] = body;
  }
  J.tagClass = Object.create(null);

  // ---- links, images, media ----
  class HTMLAnchorElement extends HTMLElement {
    _activation(ev) {
      if (ev.defaultPrevented) return;
      const href = this.getAttribute("href");
      if (href === null) return;
      J.navigate(this.href, { target: this.target, download: this.hasAttribute("download") });
    }
    toString() { return this.href; }
    get text() { return this.textContent; }
    set text(v) { this.textContent = v; }
    get relList() { return new DOMTokenList(this, "rel"); }
  }
  refUrl(HTMLAnchorElement.prototype, "href");
  for (const p of ["target", "rel", "download", "hreflang", "type", "ping", "referrerPolicy", "name"]) refStr(HTMLAnchorElement.prototype, p);
  // URL parts (protocol, host, ...) from href.
  for (const part of ["protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin", "username", "password"]) {
    Object.defineProperty(HTMLAnchorElement.prototype, part, {
      configurable: true,
      get() { try { return new G.URL(this.href)[part]; } catch (e) { return ""; } },
      set(v) { try { const u = new G.URL(this.href); u[part] = v; this.href = u.href; } catch (e) {} },
    });
  }
  def("HTMLAnchorElement", ["a"], HTMLAnchorElement);
  class HTMLAreaElement extends HTMLAnchorElement {}
  def("HTMLAreaElement", ["area"], HTMLAreaElement);

  class HTMLImageElement extends HTMLElement {
    get complete() { return true; }
    get naturalWidth() { return parseInt(this.getAttribute("width"), 10) || 0; }
    get naturalHeight() { return parseInt(this.getAttribute("height"), 10) || 0; }
    get width() { return parseInt(this.getAttribute("width"), 10) || this.offsetWidth; }
    set width(v) { this.setAttribute("width", String(v)); }
    get height() { return parseInt(this.getAttribute("height"), 10) || this.offsetHeight; }
    set height(v) { this.setAttribute("height", String(v)); }
    get currentSrc() { return this.src; }
    decode() { return Promise.resolve(); }
    _onAttr(n) {
      if (n === "src" && this.isConnected) J.fireLoad(this);
    }
    _connected() { if (this.hasAttribute("src")) J.fireLoad(this); }
  }
  refUrl(HTMLImageElement.prototype, "src");
  for (const p of ["alt", "srcset", "sizes", "crossOrigin", "useMap", "loading", "decoding", "referrerPolicy", "fetchPriority"]) refStr(HTMLImageElement.prototype, p);
  refBool(HTMLImageElement.prototype, "isMap");
  def("HTMLImageElement", ["img"], HTMLImageElement);
  J.fireLoad = function (el) {
    if (el._loadQueued) return;
    el._loadQueued = true;
    G.setTimeout(() => { el._loadQueued = false; J.dispatchEvent(el, new G.Event("load")); }, 0);
  };

  class HTMLMediaElement extends HTMLElement {
    play() { this._paused = false; J.dispatchEvent(this, new G.Event("play")); return Promise.resolve(); }
    pause() { this._paused = true; J.dispatchEvent(this, new G.Event("pause")); }
    load() {}
    canPlayType() { return ""; }
    get paused() { return this._paused !== false; }
    get currentTime() { return 0; }
    set currentTime(v) {}
    get duration() { return NaN; }
    get readyState() { return 0; }
    get volume() { return 1; }
    set volume(v) {}
    get muted() { return this.hasAttribute("muted"); }
    set muted(v) { this.toggleAttribute("muted", !!v); }
  }
  refUrl(HTMLMediaElement.prototype, "src");
  for (const p of ["autoplay", "controls", "loop", "playsInline"]) refBool(HTMLMediaElement.prototype, p);
  def("HTMLMediaElement", [], HTMLMediaElement);
  def("HTMLVideoElement", ["video"], class HTMLVideoElement extends HTMLMediaElement {});
  def("HTMLAudioElement", ["audio"], class HTMLAudioElement extends HTMLMediaElement {});

  class HTMLCanvasElement extends HTMLElement {
    getContext(kind) { return J.canvasContext ? J.canvasContext(this, kind) : null; }
    toDataURL() { return "data:,"; }
    toBlob(cb) { cb(null); }
    get width() { return parseInt(this.getAttribute("width"), 10) || 300; }
    set width(v) { this.setAttribute("width", String(v)); }
    get height() { return parseInt(this.getAttribute("height"), 10) || 150; }
    set height(v) { this.setAttribute("height", String(v)); }
  }
  def("HTMLCanvasElement", ["canvas"], HTMLCanvasElement);

  class HTMLIFrameElement extends HTMLElement {
    get contentWindow() { return null; }
    get contentDocument() { return null; }
  }
  refUrl(HTMLIFrameElement.prototype, "src");
  for (const p of ["name", "srcdoc", "allow", "width", "height", "loading", "sandbox"]) refStr(HTMLIFrameElement.prototype, p);
  def("HTMLIFrameElement", ["iframe"], HTMLIFrameElement);

  // ---- scripts, styles, links, meta ----
  class HTMLScriptElement extends HTMLElement {
    get text() { return this.textContent; }
    set text(v) { this.textContent = v; }
    _connected() {
      // Scripts inserted by script run once connected.
      if (this._parserInserted || this._started || J.building || this.getRootNode() !== G.document) return;
      this._started = true;
      J.runInsertedScript(this);
    }
  }
  refUrl(HTMLScriptElement.prototype, "src");
  for (const p of ["type", "charset", "crossOrigin", "integrity", "referrerPolicy", "event", "htmlFor"]) refStr(HTMLScriptElement.prototype, p);
  refBool(HTMLScriptElement.prototype, "defer");
  refBool(HTMLScriptElement.prototype, "noModule");
  Object.defineProperty(HTMLScriptElement.prototype, "async", {
    get() { return this.hasAttribute("async") || (!this._parserInserted && this._forceAsync !== false); },
    set(v) { this._forceAsync = false; this.toggleAttribute("async", !!v); },
  });
  def("HTMLScriptElement", ["script"], HTMLScriptElement);

  class HTMLStyleElement extends HTMLElement {
    get sheet() { return J.styleSheetFor ? J.styleSheetFor(this) : null; }
  }
  refStr(HTMLStyleElement.prototype, "media");
  refStr(HTMLStyleElement.prototype, "type");
  refBool(HTMLStyleElement.prototype, "disabled");
  def("HTMLStyleElement", ["style"], HTMLStyleElement);
  class HTMLLinkElement extends HTMLElement {
    get relList() { return new DOMTokenList(this, "rel"); }
    get sheet() { return J.styleSheetFor ? J.styleSheetFor(this) : null; }
    _connected() { if (/stylesheet|preload/.test(this.rel)) J.fireLoad(this); }
  }
  refUrl(HTMLLinkElement.prototype, "href");
  for (const p of ["rel", "media", "type", "as", "crossOrigin", "integrity", "hreflang", "sizes", "imageSrcset", "fetchPriority"]) refStr(HTMLLinkElement.prototype, p);
  refBool(HTMLLinkElement.prototype, "disabled");
  def("HTMLLinkElement", ["link"], HTMLLinkElement);
  class HTMLMetaElement extends HTMLElement {}
  for (const p of ["name", "content", "httpEquiv", "charset", "media"]) refStr(HTMLMetaElement.prototype, p, p === "httpEquiv" ? "http-equiv" : undefined);
  def("HTMLMetaElement", ["meta"], HTMLMetaElement);
  class HTMLBaseElement extends HTMLElement {}
  refUrl(HTMLBaseElement.prototype, "href");
  refStr(HTMLBaseElement.prototype, "target");
  def("HTMLBaseElement", ["base"], HTMLBaseElement);
  class HTMLTitleElement extends HTMLElement {
    get text() { return this.textContent; }
    set text(v) { this.textContent = v; }
  }
  def("HTMLTitleElement", ["title"], HTMLTitleElement);

  class HTMLTemplateElement extends HTMLElement {
    get content() {
      if (!this._content) {
        this._content = this.ownerDocument.createDocumentFragment();
        // Move parsed children into the template contents.
        for (const c of this._c.slice()) this._content.appendChild(c);
      }
      return this._content;
    }
    _cloneExtra(c) { c._content = this.content.cloneNode(true); }
  }
  def("HTMLTemplateElement", ["template"], HTMLTemplateElement);

  class HTMLSlotElement extends HTMLElement {
    assignedNodes() { return []; }
    assignedElements() { return []; }
  }
  refStr(HTMLSlotElement.prototype, "name");
  def("HTMLSlotElement", ["slot"], HTMLSlotElement);

  // ---- interactive elements ----
  class HTMLDialogElement extends HTMLElement {
    get open() { return this.hasAttribute("open"); }
    set open(v) { this.toggleAttribute("open", !!v); }
    show() { this.setAttribute("open", ""); }
    showModal() { this._modal = true; this.setAttribute("open", ""); }
    close(rv) {
      if (!this.open) return;
      if (rv !== undefined) this.returnValue = String(rv);
      this._modal = false;
      this.removeAttribute("open");
      G.setTimeout(() => J.dispatchEvent(this, new G.Event("close")), 0);
    }
    requestClose(rv) { this.close(rv); }
  }
  HTMLDialogElement.prototype.returnValue = "";
  def("HTMLDialogElement", ["dialog"], HTMLDialogElement);
  class HTMLDetailsElement extends HTMLElement {
    get open() { return this.hasAttribute("open"); }
    set open(v) { this.toggleAttribute("open", !!v); }
    _onAttr(n) { if (n === "open") G.setTimeout(() => J.dispatchEvent(this, new G.Event("toggle")), 0); }
  }
  def("HTMLDetailsElement", ["details"], HTMLDetailsElement);
  class HTMLSummaryElement extends HTMLElement {
    _activation() { const d = this.parentNode; if (d && d._tag === "details") d.open = !d.open; }
  }
  def("HTMLSummaryElement", ["summary"], HTMLSummaryElement);
  class HTMLLabelElement extends HTMLElement {
    get htmlFor() { return this.getAttribute("for") || ""; }
    set htmlFor(v) { this.setAttribute("for", v); }
    get control() {
      const f = this.getAttribute("for");
      if (f !== null) return this.ownerDocument.getElementById(f);
      return this.querySelector("input,select,textarea,button");
    }
    get form() { const c = this.control; return c ? c.form : null; }
    _activation(ev) {
      const c = this.control;
      if (c && ev.target !== c && !c.contains(ev.target)) { c.focus(); c.click(); }
    }
  }
  def("HTMLLabelElement", ["label"], HTMLLabelElement);

  // Generic elements by tag.
  const generic = {
    HTMLDivElement: ["div"], HTMLSpanElement: ["span"], HTMLParagraphElement: ["p"], HTMLHeadingElement: ["h1", "h2", "h3", "h4", "h5", "h6"],
    HTMLUListElement: ["ul"], HTMLOListElement: ["ol"], HTMLLIElement: ["li"], HTMLDListElement: ["dl"], HTMLPreElement: ["pre", "listing", "xmp"],
    HTMLQuoteElement: ["blockquote", "q"], HTMLBRElement: ["br"], HTMLHRElement: ["hr"], HTMLTableElement: ["table"], HTMLTableRowElement: ["tr"],
    HTMLTableCellElement: ["td", "th"], HTMLTableSectionElement: ["thead", "tbody", "tfoot"], HTMLTableCaptionElement: ["caption"],
    HTMLTableColElement: ["col", "colgroup"], HTMLBodyElement: ["body"], HTMLHeadElement: ["head"], HTMLHtmlElement: ["html"],
    HTMLModElement: ["ins", "del"], HTMLPictureElement: ["picture"], HTMLSourceElement: ["source"], HTMLTrackElement: ["track"],
    HTMLEmbedElement: ["embed"], HTMLObjectElement: ["object"], HTMLParamElement: ["param"], HTMLMapElement: ["map"],
    HTMLTimeElement: ["time"], HTMLDataElement: ["data"], HTMLMenuElement: ["menu"], HTMLDataListElement: ["datalist"],
    HTMLMeterElement: ["meter"], HTMLProgressElement: ["progress"], HTMLLegendElement: ["legend"], HTMLFieldSetElement: ["fieldset"],
  };
  for (const [name, tags] of Object.entries(generic)) {
    const cls = { [name]: class extends HTMLElement {} }[name];
    def(name, tags, cls);
  }
  G.HTMLTableElement.prototype.insertRow = function (i) {
    const body = this.querySelector("tbody") || this;
    const tr = this.ownerDocument.createElement("tr");
    const rows = body.querySelectorAll(":scope > tr");
    body.insertBefore(tr, i === undefined || i < 0 || i >= rows.length ? null : rows[i]);
    return tr;
  };
  Object.defineProperty(G.HTMLTableElement.prototype, "rows", { get() { return J.liveList(() => J.descendants(this).filter((e) => e._tag === "tr"), G.HTMLCollection); } });
  Object.defineProperty(G.HTMLTableElement.prototype, "tBodies", { get() { return J.liveList(() => this._c.filter((e) => e._tag === "tbody"), G.HTMLCollection); } });
  G.HTMLTableRowElement.prototype.insertCell = function (i) {
    const td = this.ownerDocument.createElement("td");
    const cells = this._c.filter((e) => e._tag === "td" || e._tag === "th");
    this.insertBefore(td, i === undefined || i < 0 || i >= cells.length ? null : cells[i]);
    return td;
  };
  Object.defineProperty(G.HTMLTableRowElement.prototype, "cells", { get() { return J.liveList(() => this._c.filter((e) => e._tag === "td" || e._tag === "th"), G.HTMLCollection); } });
  refNum(G.HTMLTableCellElement.prototype, "colSpan", "colspan", 1);
  refNum(G.HTMLTableCellElement.prototype, "rowSpan", "rowspan", 1);
  refNum(G.HTMLOListElement.prototype, "start", "start", 1);
  refBool(G.HTMLOListElement.prototype, "reversed");
  refStr(G.HTMLTimeElement.prototype, "dateTime", "datetime");
  refStr(G.HTMLDataElement.prototype, "value");
  refBool(G.HTMLFieldSetElement.prototype, "disabled");
  for (const p of ["value", "max", "min", "low", "high", "optimum"]) {
    Object.defineProperty(G.HTMLMeterElement.prototype, p, { get() { return Number(this.getAttribute(p)) || 0; }, set(v) { this.setAttribute(p, String(v)); } });
  }
  Object.defineProperty(G.HTMLProgressElement.prototype, "value", { get() { return Number(this.getAttribute("value")) || 0; }, set(v) { this.setAttribute("value", String(v)); } });
  Object.defineProperty(G.HTMLProgressElement.prototype, "max", { get() { return Number(this.getAttribute("max")) || 1; }, set(v) { this.setAttribute("max", String(v)); } });

  // ---- custom elements ----
  J.customElements = registry;
  G.customElements = {
    define(name, cls, opts) {
      name = String(name).toLowerCase();
      if (registry.has(name)) throw new G.DOMException("'" + name + "' is already defined", "NotSupportedError");
      if (!/^[a-z][a-z0-9._·-]*-[a-z0-9._·-]*$/.test(name)) throw new G.DOMException("'" + name + "' is not a valid custom element name", "SyntaxError");
      registry.set(name, cls);
      // Upgrade existing elements.
      if (G.document) for (const e of J.descendants(G.document).filter((n) => n.nodeType === 1 && n._tag === name)) J.upgrade(e);
      const w = waiting.get(name);
      if (w) { waiting.delete(name); w.forEach((r) => r(cls)); }
      void opts;
    },
    get(name) { return registry.get(String(name).toLowerCase()); },
    getName(cls) { for (const [n, c] of registry) if (c === cls) return n; return null; },
    whenDefined(name) {
      name = String(name).toLowerCase();
      if (registry.has(name)) return Promise.resolve(registry.get(name));
      return new Promise((r) => { const w = waiting.get(name) || []; w.push(r); waiting.set(name, w); });
    },
    upgrade(root) { for (const e of J.descendants(root)) if (e.nodeType === 1) J.upgrade(e); },
  };
  J.upgrade = function (e) {
    const cls = registry.get(e._tag);
    if (!cls || e._customUpgraded) return;
    Object.setPrototypeOf(e, cls.prototype);
    J.upgrading = e;
    try { new cls(); } catch (x) { G.__onError(x); }
    J.upgrading = null;
    e._customUpgraded = true;
    const obs = cls.observedAttributes || [];
    if (typeof e.attributeChangedCallback === "function") {
      for (const [k, v] of e._attrs) if (obs.includes(k)) { try { e.attributeChangedCallback(k, null, v); } catch (x) { G.__onError(x); } }
    }
    if (e.isConnected && typeof e.connectedCallback === "function") { try { e.connectedCallback(); } catch (x) { G.__onError(x); } }
  };

  Object.assign(G, { Element, HTMLElement, Attr, NamedNodeMap, DOMTokenList, CSSStyleDeclaration, DOMRect, DOMRectReadOnly: DOMRect });
  J.HTMLElement = HTMLElement;
})(globalThis);
