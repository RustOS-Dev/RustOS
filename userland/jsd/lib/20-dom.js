// The DOM: nodes and the tree, kept in sync with the browser's copy
// through mutation operations (J.ops), which the host sends after each
// task. Node ids are shared with the browser; new nodes get ids from
// J.nextId. Comments and document fragments exist only here.
"use strict";
(function (G) {
  const J = G.__jsd;
  J.nodes = new Map();
  J.version = 0; // bumped on every tree change (live collections)
  J.nextId = 1;

  const ELEMENT_NODE = 1, TEXT_NODE = 3, COMMENT_NODE = 8, DOCUMENT_NODE = 9, DOCUMENT_FRAGMENT_NODE = 11, DOCUMENT_TYPE_NODE = 10;
  const VOID = new Set(["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr", "keygen", "frame"]);
  J.VOID = VOID;

  function register(n, id) {
    n.__id = id;
    J.nodes.set(id, n);
  }

  // ---- mutation records (MutationObserver) ----
  const observers = [];
  J.observers = observers;
  function queueRecord(target, rec) {
    for (const o of observers) {
      for (const reg of o._targets) {
        let hit = reg.node === target;
        if (!hit && reg.opts.subtree) {
          for (let n = target.parentNode; n; n = n.parentNode) if (n === reg.node) { hit = true; break; }
        }
        if (!hit) continue;
        const opts = reg.opts;
        if (rec.type === "childList" && !opts.childList) continue;
        if (rec.type === "attributes") {
          if (!opts.attributes && opts.attributes !== undefined) continue;
          if (!opts.attributes && !opts.attributeOldValue && !opts.attributeFilter) continue;
          if (opts.attributeFilter && !opts.attributeFilter.includes(rec.attributeName)) continue;
        }
        if (rec.type === "characterData" && !opts.characterData && !opts.characterDataOldValue) continue;
        const r = Object.assign({ addedNodes: [], removedNodes: [], previousSibling: null, nextSibling: null, attributeName: null, attributeNamespace: null, oldValue: null }, rec);
        if (!(opts.attributeOldValue || opts.characterDataOldValue)) r.oldValue = null;
        o._records.push(r);
        if (!o._scheduled) {
          o._scheduled = true;
          Promise.resolve().then(() => {
            o._scheduled = false;
            const recs = o.takeRecords();
            if (recs.length) {
              try { o._cb.call(o, recs, o); } catch (e) { G.__onError(e); }
            }
          });
        }
        break;
      }
    }
  }

  class MutationObserver {
    constructor(cb) { this._cb = cb; this._targets = []; this._records = []; this._scheduled = false; }
    observe(node, opts) {
      opts = Object.assign({}, opts);
      if (opts.attributeOldValue || opts.attributeFilter) opts.attributes = true;
      if (opts.characterDataOldValue) opts.characterData = true;
      const ex = this._targets.find((t) => t.node === node);
      if (ex) ex.opts = opts; else this._targets.push({ node, opts });
      if (!observers.includes(this)) observers.push(this);
    }
    disconnect() {
      this._targets = []; this._records = [];
      const i = observers.indexOf(this);
      if (i >= 0) observers.splice(i, 1);
    }
    takeRecords() { return this._records.splice(0); }
  }

  // ---- live lists ----
  function liveList(compute, Kind) {
    let cacheV = -1, cache = [];
    const get = () => { if (cacheV !== J.version) { cache = compute(); cacheV = J.version; } return cache; };
    const base = new Kind(get);
    return new Proxy(base, {
      get(t, k, r) {
        if (typeof k === "string" && k !== "" && /^\d+$/.test(k)) return get()[Number(k)];
        if (k === "length") return get().length;
        if (Kind === HTMLCollection && typeof k === "string" && !(k in t)) {
          return get().find((e) => e.id === k || e.getAttribute("name") === k) || undefined;
        }
        return Reflect.get(t, k, r);
      },
      has(t, k) { return (typeof k === "string" && /^\d+$/.test(k) && Number(k) < get().length) || k in t; },
      ownKeys(t) { return Object.keys(get()).concat(Reflect.ownKeys(t)); },
      getOwnPropertyDescriptor(t, k) {
        if (typeof k === "string" && /^\d+$/.test(k) && Number(k) < get().length) return { value: get()[Number(k)], enumerable: true, configurable: true, writable: false };
        return Reflect.getOwnPropertyDescriptor(t, k);
      },
    });
  }
  class NodeList {
    constructor(get) { Object.defineProperty(this, "_get", { value: get }); }
    item(i) { return this._get()[i] || null; }
    get length() { return this._get().length; }
    forEach(f, t) { this._get().slice().forEach(f, t); }
    entries() { return this._get().slice().entries(); }
    keys() { return this._get().slice().keys(); }
    values() { return this._get().slice().values(); }
    [Symbol.iterator]() { return this._get().slice()[Symbol.iterator](); }
  }
  class HTMLCollection {
    constructor(get) { Object.defineProperty(this, "_get", { value: get }); }
    item(i) { return this._get()[i] || null; }
    namedItem(n) { return this._get().find((e) => e.id === n || e.getAttribute("name") === n) || null; }
    get length() { return this._get().length; }
    [Symbol.iterator]() { return this._get().slice()[Symbol.iterator](); }
  }
  J.liveList = liveList;
  J.staticList = (arr) => liveList(() => arr, NodeList);

  // ---- Node ----
  class Node extends G.EventTarget {
    constructor() {
      super();
      this.parentNode = null;
      this._c = [];
    }
    get ownerDocument() { return this.nodeType === DOCUMENT_NODE ? null : (this._doc || G.document); }
    get childNodes() {
      if (!this._cl) this._cl = liveList(() => this._c.slice(), NodeList);
      return this._cl;
    }
    get firstChild() { return this._c[0] || null; }
    get lastChild() { return this._c[this._c.length - 1] || null; }
    get previousSibling() {
      const p = this.parentNode; if (!p) return null;
      const i = p._c.indexOf(this); return i > 0 ? p._c[i - 1] : null;
    }
    get nextSibling() {
      const p = this.parentNode; if (!p) return null;
      const i = p._c.indexOf(this); return p._c[i + 1] || null;
    }
    get parentElement() { const p = this.parentNode; return p && p.nodeType === ELEMENT_NODE ? p : null; }
    get isConnected() { let n = this; while (n.parentNode) n = n.parentNode; return n.nodeType === DOCUMENT_NODE; }
    hasChildNodes() { return this._c.length > 0; }
    getRootNode() { let n = this; while (n.parentNode) n = n.parentNode; return n; }
    contains(o) { for (let n = o; n; n = n.parentNode) if (n === this) return true; return false; }
    get baseURI() { return G.document ? G.document.baseURI : ""; }
    get nodeValue() { return this.nodeType === TEXT_NODE || this.nodeType === COMMENT_NODE ? this._data : null; }
    set nodeValue(v) { if (this.nodeType === TEXT_NODE || this.nodeType === COMMENT_NODE) this.data = v; }

    get textContent() {
      if (this.nodeType === TEXT_NODE || this.nodeType === COMMENT_NODE) return this._data;
      if (this.nodeType === DOCUMENT_NODE) return null;
      let s = "";
      const walk = (n) => { for (const c of n._c) { if (c.nodeType === TEXT_NODE) s += c._data; else if (c.nodeType === ELEMENT_NODE || c.nodeType === DOCUMENT_FRAGMENT_NODE) walk(c); } };
      walk(this);
      return s;
    }
    set textContent(v) {
      if (this.nodeType === TEXT_NODE || this.nodeType === COMMENT_NODE) { this.data = v; return; }
      v = v == null ? "" : String(v);
      for (const c of this._c.slice()) this.removeChild(c);
      if (v !== "") this.appendChild(this.ownerDocument.createTextNode(v));
    }

    _check(child, before) {
      if (!(child instanceof Node)) throw new TypeError("parameter is not a Node");
      if (child.contains(this)) throw new DOMException("The new child contains the parent", "HierarchyRequestError");
      if (before && before.parentNode !== this) throw new DOMException("The node before which the new node is to be inserted is not a child of this node", "NotFoundError");
    }

    insertBefore(child, before) {
      before = before || null;
      this._check(child, before);
      if (child.nodeType === DOCUMENT_FRAGMENT_NODE) {
        for (const c of child._c.slice()) this.insertBefore(c, before);
        return child;
      }
      if (child === before) return child;
      if (child.parentNode) child.parentNode._remove(child, true);
      // A node from another document (DOMParser, createHTMLDocument)
      // joins this one: the browser learns about it now.
      if (child._local && !this._local && child._foreign) J.materialize(child);
      const i = before ? this._c.indexOf(before) : this._c.length;
      this._c.splice(i, 0, child);
      child.parentNode = this;
      if (this._doc) child._setDoc(this._doc);
      J.version++;
      if (!this._local && !child._local) {
        let b = before;
        while (b && b._local) b = b.nextSibling;
        J.ops.push({ op: "insert", parent: this.__id, child: child.__id, before: b ? b.__id : null });
      }
      queueRecord(this, { type: "childList", target: this, addedNodes: [child], previousSibling: child.previousSibling, nextSibling: child.nextSibling });
      if (child.isConnected) J.connected(child);
      return child;
    }
    appendChild(c) { return this.insertBefore(c, null); }
    _remove(child, moving) {
      const i = this._c.indexOf(child);
      if (i < 0) return;
      const prev = this._c[i - 1] || null, next = this._c[i + 1] || null;
      const wasConnected = child.isConnected;
      this._c.splice(i, 1);
      child.parentNode = null;
      J.version++;
      if (!this._local && !child._local) J.ops.push({ op: "remove", child: child.__id });
      queueRecord(this, { type: "childList", target: this, removedNodes: [child], previousSibling: prev, nextSibling: next });
      if (wasConnected && !moving) J.disconnected(child);
    }
    removeChild(child) {
      if (!child || child.parentNode !== this) throw new DOMException("The node to be removed is not a child of this node", "NotFoundError");
      this._remove(child, false);
      return child;
    }
    replaceChild(nw, old) {
      if (old.parentNode !== this) throw new DOMException("The node to be replaced is not a child of this node", "NotFoundError");
      const next = old.nextSibling === nw ? nw.nextSibling : old.nextSibling;
      this.removeChild(old);
      this.insertBefore(nw, next);
      return old;
    }
    cloneNode(deep) {
      const d = this.ownerDocument || this;
      let c;
      switch (this.nodeType) {
        case ELEMENT_NODE:
          c = d.createElement(this._tag);
          for (const [k, v] of this._attrs) c.setAttribute(k, v);
          if (this._cloneExtra) this._cloneExtra(c);
          break;
        case TEXT_NODE: c = d.createTextNode(this._data); break;
        case COMMENT_NODE: c = d.createComment(this._data); break;
        case DOCUMENT_FRAGMENT_NODE: c = d.createDocumentFragment(); break;
        default: throw new DOMException("cannot clone this node", "NotSupportedError");
      }
      if (deep) for (const k of this._c) c.appendChild(k.cloneNode(true));
      return c;
    }
    isEqualNode(o) {
      if (!o || o.nodeType !== this.nodeType) return false;
      if (this.nodeType === ELEMENT_NODE) {
        if (this._tag !== o._tag || this._attrs.length !== o._attrs.length) return false;
        for (const [k, v] of this._attrs) if (o.getAttribute(k) !== v) return false;
      } else if (this._data !== o._data) return false;
      if (this._c.length !== o._c.length) return false;
      return this._c.every((c, i) => c.isEqualNode(o._c[i]));
    }
    isSameNode(o) { return this === o; }
    compareDocumentPosition(o) {
      if (o === this) return 0;
      const anc = (n) => { const a = []; for (; n; n = n.parentNode) a.unshift(n); return a; };
      const a = anc(this), b = anc(o);
      if (a[0] !== b[0]) return 1 | 32 | (J.nodeOrder(this) < J.nodeOrder(o) ? 4 : 2);
      let i = 0;
      while (i < a.length && i < b.length && a[i] === b[i]) i++;
      if (i === a.length) return 16 | 4; // o is a descendant: contained_by + following
      if (i === b.length) return 8 | 2;  // o is an ancestor
      const p = a[i - 1];
      return p._c.indexOf(a[i]) < p._c.indexOf(b[i]) ? 4 : 2;
    }
    normalize() {
      for (const c of this._c.slice()) {
        if (c.nodeType === TEXT_NODE) {
          if (c._data === "") { this.removeChild(c); continue; }
          let n = c.nextSibling;
          while (n && n.nodeType === TEXT_NODE) { c.appendData(n._data); const x = n.nextSibling; this.removeChild(n); n = x; }
        } else c.normalize();
      }
    }
    _setDoc(d) { this._doc = d; for (const c of this._c) c._setDoc(d); }
    // ChildNode / ParentNode mixins
    _nodes(args) {
      const d = this.ownerDocument || this;
      if (args.length === 1 && args[0] instanceof Node) return args[0];
      const f = d.createDocumentFragment();
      for (const a of args) f.appendChild(a instanceof Node ? a : d.createTextNode(String(a)));
      return f;
    }
    before(...a) { const p = this.parentNode; if (p) p.insertBefore(this._nodes(a), this); }
    after(...a) { const p = this.parentNode; if (p) p.insertBefore(this._nodes(a), this.nextSibling); }
    replaceWith(...a) { const p = this.parentNode; if (p) { const n = this._nodes(a); if (n !== this) p.replaceChild(n, this); } }
    remove() { if (this.parentNode) this.parentNode.removeChild(this); }
    append(...a) { this.appendChild(this._nodes(a)); }
    prepend(...a) { this.insertBefore(this._nodes(a), this.firstChild); }
    replaceChildren(...a) { for (const c of this._c.slice()) this.removeChild(c); this.append(...a); }
    get children() {
      if (!this._ch) this._ch = liveList(() => this._c.filter((c) => c.nodeType === ELEMENT_NODE), HTMLCollection);
      return this._ch;
    }
    get firstElementChild() { return this._c.find((c) => c.nodeType === ELEMENT_NODE) || null; }
    get lastElementChild() { for (let i = this._c.length - 1; i >= 0; i--) if (this._c[i].nodeType === ELEMENT_NODE) return this._c[i]; return null; }
    get childElementCount() { return this._c.filter((c) => c.nodeType === ELEMENT_NODE).length; }
    get previousElementSibling() { let n = this.previousSibling; while (n && n.nodeType !== ELEMENT_NODE) n = n.previousSibling; return n; }
    get nextElementSibling() { let n = this.nextSibling; while (n && n.nodeType !== ELEMENT_NODE) n = n.nextSibling; return n; }
    querySelector(s) { return J.select(this, s, true)[0] || null; }
    querySelectorAll(s) { return J.staticList(J.select(this, s, false)); }
    getElementsByTagName(t) {
      t = String(t).toLowerCase();
      return liveList(() => J.descendants(this).filter((e) => e.nodeType === ELEMENT_NODE && (t === "*" || e._tag === t)), HTMLCollection);
    }
    getElementsByClassName(c) {
      const want = String(c).split(/\s+/).filter(Boolean);
      return liveList(() => J.descendants(this).filter((e) => e.nodeType === ELEMENT_NODE && want.every((w) => e.classList.contains(w))), HTMLCollection);
    }
    getElementsByTagNameNS(ns, t) { return this.getElementsByTagName(t); }
    lookupNamespaceURI() { return "http://www.w3.org/1999/xhtml"; }
  }
  Object.assign(Node, { ELEMENT_NODE, ATTRIBUTE_NODE: 2, TEXT_NODE, CDATA_SECTION_NODE: 4, PROCESSING_INSTRUCTION_NODE: 7, COMMENT_NODE, DOCUMENT_NODE, DOCUMENT_TYPE_NODE, DOCUMENT_FRAGMENT_NODE,
    DOCUMENT_POSITION_DISCONNECTED: 1, DOCUMENT_POSITION_PRECEDING: 2, DOCUMENT_POSITION_FOLLOWING: 4, DOCUMENT_POSITION_CONTAINS: 8, DOCUMENT_POSITION_CONTAINED_BY: 16, DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC: 32 });
  for (const k of Object.keys(Node)) if (/^[A-Z_]+$/.test(k)) Node.prototype[k] = Node[k];

  J.descendants = function (root) {
    const out = [];
    const walk = (n) => { for (const c of n._c) { out.push(c); walk(c); } };
    walk(root);
    return out;
  };
  J.nodeOrder = function (n) { return n.__id || 0; };

  class CharacterData extends Node {
    get data() { return this._data; }
    set data(v) {
      v = v == null ? "" : String(v);
      const old = this._data;
      this._data = v;
      J.version++;
      if (!this._local) J.ops.push({ op: "data", id: this.__id, data: v });
      queueRecord(this, { type: "characterData", target: this, oldValue: old });
    }
    get length() { return this._data.length; }
    appendData(s) { this.data = this._data + s; }
    insertData(o, s) { this.data = this._data.slice(0, o) + s + this._data.slice(o); }
    deleteData(o, n) { this.data = this._data.slice(0, o) + this._data.slice(o + n); }
    replaceData(o, n, s) { this.data = this._data.slice(0, o) + s + this._data.slice(o + n); }
    substringData(o, n) { return this._data.substr(o, n); }
  }
  class Text extends CharacterData {
    constructor(data) {
      super();
      this._data = data === undefined ? "" : String(data);
      register(this, J.nextId++);
      if (J.makeLocal) this._local = true;
      else J.ops.push({ op: "create", id: this.__id, text: this._data });
    }
    get nodeType() { return TEXT_NODE; }
    get nodeName() { return "#text"; }
    get wholeText() { return this._data; }
    splitText(o) {
      const t = new Text(this._data.slice(o));
      this.data = this._data.slice(0, o);
      if (this.parentNode) this.parentNode.insertBefore(t, this.nextSibling);
      return t;
    }
  }
  class Comment extends CharacterData {
    constructor(data) { super(); this._data = data === undefined ? "" : String(data); this._local = true; }
    get nodeType() { return COMMENT_NODE; }
    get nodeName() { return "#comment"; }
  }
  class DocumentFragment extends Node {
    constructor() { super(); this._local = true; }
    get nodeType() { return DOCUMENT_FRAGMENT_NODE; }
    get nodeName() { return "#document-fragment"; }
    getElementById(id) { return J.descendants(this).find((e) => e.nodeType === ELEMENT_NODE && e.id === id) || null; }
    get innerHTML() { return J.serializeChildren(this); }
  }
  class DocumentType extends Node {
    constructor(name) { super(); this.name = name; this.publicId = ""; this.systemId = ""; this._local = true; }
    get nodeType() { return DOCUMENT_TYPE_NODE; }
    get nodeName() { return this.name; }
  }

  class DOMException extends Error {
    constructor(message, name) {
      super(message || "");
      this.name = name || "Error";
      this.code = { IndexSizeError: 1, HierarchyRequestError: 3, WrongDocumentError: 4, InvalidCharacterError: 5, NotFoundError: 8, NotSupportedError: 9, InvalidStateError: 11, SyntaxError: 12, InvalidAccessError: 15, SecurityError: 18, NetworkError: 19, AbortError: 20, QuotaExceededError: 22, TimeoutError: 23 }[this.name] || 0;
    }
  }

  // ---- HTML serialization ----
  function escText(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/ /g, "&nbsp;"); }
  function escAttr(s) { return s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/ /g, "&nbsp;"); }
  J.serialize = function (n) {
    switch (n.nodeType) {
      case TEXT_NODE: {
        const p = n.parentNode && n.parentNode._tag;
        return p && /^(script|style|xmp|iframe|noembed|noframes|plaintext|noscript)$/.test(p) ? n._data : escText(n._data);
      }
      case COMMENT_NODE: return "<!--" + n._data + "-->";
      case DOCUMENT_TYPE_NODE: return "<!DOCTYPE " + n.name + ">";
      case ELEMENT_NODE: {
        let s = "<" + n._tag;
        for (const [k, v] of n._attrs) s += " " + k + '="' + escAttr(v) + '"';
        s += ">";
        if (VOID.has(n._tag)) return s;
        const kids = n._tag === "template" && n._content ? n._content : n;
        return s + J.serializeChildren(kids) + "</" + n._tag + ">";
      }
      default: return J.serializeChildren(n);
    }
  };
  J.serializeChildren = (n) => n._c.map(J.serialize).join("");

  // Parse HTML through the browser: new nodes arrive with fresh ids.
  J.parseFragment = function (html, doc) {
    const first = J.nextId;
    const tree = J.rpc("parseFragment", { html: String(html), first });
    const f = doc.createDocumentFragment();
    for (const t of tree) f.appendChild(J.build(t, doc));
    // Scripts inserted through innerHTML never run.
    for (const n of J.descendants(f)) if (n._tag === "script") n._started = true;
    return f;
  };
  // Build nodes from the wire format ([id, tag, attrs, children] or
  // [id, "#text", data]); ids are kept.
  J.build = function (t, doc) {
    const id = t[0];
    if (id >= J.nextId) J.nextId = id + 1;
    let n;
    if (t[1] === "#text") {
      const save = J.nextId;
      J.nextId = id;
      n = doc.createTextNode(t[2]);
      J.nextId = Math.max(save, id + 1);
    } else {
      n = doc._createElementWithId(t[1], id, t[2]);
      for (const c of t[3]) n.appendChild(J.build(c, doc));
    }
    return n;
  };

  // Connection callbacks (scripts added to the document run; custom
  // elements' connectedCallback).
  J.connected = function (n) {
    if (n.nodeType !== ELEMENT_NODE) return;
    const all = [n].concat(J.descendants(n).filter((x) => x.nodeType === ELEMENT_NODE));
    const main = n.getRootNode() === G.document;
    for (const e of all) {
      if (main) J.exposeNamed(e);
      if (e._connected) e._connected();
      if (typeof e.connectedCallback === "function" && e._customUpgraded) {
        try { e.connectedCallback(); } catch (x) { G.__onError(x); }
      }
    }
  };
  J.disconnected = function (n) {
    if (n.nodeType !== ELEMENT_NODE) return;
    const all = [n].concat(J.descendants(n).filter((x) => x.nodeType === ELEMENT_NODE));
    for (const e of all) {
      if (typeof e.disconnectedCallback === "function" && e._customUpgraded) {
        try { e.disconnectedCallback(); } catch (x) { G.__onError(x); }
      }
    }
  };

  // TreeWalker / NodeIterator.
  const NodeFilter = { FILTER_ACCEPT: 1, FILTER_REJECT: 2, FILTER_SKIP: 3, SHOW_ALL: 0xFFFFFFFF, SHOW_ELEMENT: 1, SHOW_ATTRIBUTE: 2, SHOW_TEXT: 4, SHOW_COMMENT: 0x80, SHOW_DOCUMENT: 0x100, SHOW_DOCUMENT_FRAGMENT: 0x400 };
  function accept(n, what, filter) {
    if (!(what & (1 << (n.nodeType - 1)))) return 3;
    if (!filter) return 1;
    return typeof filter === "function" ? filter(n) : filter.acceptNode(n);
  }
  class TreeWalker {
    constructor(root, what, filter) { this.root = root; this.whatToShow = what === undefined ? NodeFilter.SHOW_ALL : what; this.filter = filter || null; this.currentNode = root; }
    _order() { return J.descendants(this.root); }
    nextNode() {
      const all = this._order();
      let i = this.currentNode === this.root ? -1 : all.indexOf(this.currentNode);
      for (i++; i < all.length; i++) if (accept(all[i], this.whatToShow, this.filter) === 1) return (this.currentNode = all[i]);
      return null;
    }
    previousNode() {
      const all = this._order();
      for (let i = all.indexOf(this.currentNode) - 1; i >= 0; i--) if (accept(all[i], this.whatToShow, this.filter) === 1) return (this.currentNode = all[i]);
      if (this.currentNode !== this.root && accept(this.root, this.whatToShow, this.filter) === 1) return (this.currentNode = this.root);
      return null;
    }
    parentNode() {
      for (let n = this.currentNode.parentNode; n && n !== this.root.parentNode; n = n.parentNode) if (accept(n, this.whatToShow, this.filter) === 1) return (this.currentNode = n);
      return null;
    }
    firstChild() { for (const c of this.currentNode._c) if (accept(c, this.whatToShow, this.filter) === 1) return (this.currentNode = c); return null; }
    lastChild() { for (let i = this.currentNode._c.length - 1; i >= 0; i--) { const c = this.currentNode._c[i]; if (accept(c, this.whatToShow, this.filter) === 1) return (this.currentNode = c); } return null; }
    nextSibling() { for (let n = this.currentNode.nextSibling; n; n = n.nextSibling) if (accept(n, this.whatToShow, this.filter) === 1) return (this.currentNode = n); return null; }
    previousSibling() { for (let n = this.currentNode.previousSibling; n; n = n.previousSibling) if (accept(n, this.whatToShow, this.filter) === 1) return (this.currentNode = n); return null; }
  }
  class NodeIterator extends TreeWalker {
    get referenceNode() { return this.currentNode; }
    detach() {}
  }

  // A minimal Range (for libraries that probe it).
  class Range {
    constructor() { this.startContainer = G.document; this.startOffset = 0; this.endContainer = G.document; this.endOffset = 0; this.collapsed = true; }
    setStart(n, o) { this.startContainer = n; this.startOffset = o; }
    setEnd(n, o) { this.endContainer = n; this.endOffset = o; this.collapsed = false; }
    selectNode(n) { this.startContainer = this.endContainer = n.parentNode; this.startOffset = this.endOffset = 0; }
    selectNodeContents(n) { this.startContainer = this.endContainer = n; this.startOffset = 0; this.endOffset = n._c.length; }
    collapse() { this.collapsed = true; }
    cloneRange() { return Object.assign(new Range(), this); }
    createContextualFragment(html) { return J.parseFragment(html, G.document); }
    getBoundingClientRect() { return new G.DOMRect(0, 0, 0, 0); }
    getClientRects() { return []; }
    toString() { return ""; }
    detach() {}
    deleteContents() {}
    insertNode(n) { if (this.startContainer._c) this.startContainer.insertBefore(n, this.startContainer._c[this.startOffset] || null); }
  }

  Object.assign(G, { Node, CharacterData, Text, Comment, DocumentFragment, DocumentType, DOMException, NodeList, HTMLCollection,
    MutationObserver, WebKitMutationObserver: MutationObserver, TreeWalker, NodeIterator, NodeFilter, Range });
  J.materialize = function (n) {
    if (!n._foreign) return;
    n._foreign = false;
    n._local = false;
    if (n.nodeType === TEXT_NODE) { J.ops.push({ op: "create", id: n.__id, text: n._data }); return; }
    J.ops.push({ op: "create", id: n.__id, tag: n._tag });
    for (const [k, v] of n._attrs) J.ops.push({ op: "attr", id: n.__id, name: k, value: v });
    for (const c of n._c) {
      if (c.nodeType === COMMENT_NODE) continue;
      J.materialize(c);
      J.ops.push({ op: "insert", parent: n.__id, child: c.__id, before: null });
    }
  };
  // Elements with an id (and forms/images/iframes with a name) are
  // reachable as window properties unless a script defines that name.
  const named = new Set();
  J.exposeNamed = function (e) {
    const names = [];
    const id = e.getAttribute("id");
    if (id) names.push(id);
    const name = /^(form|img|iframe|embed|object)$/.test(e._tag) ? e.getAttribute("name") : null;
    if (name) names.push(name);
    for (const k of names) {
      if (named.has(k) || Object.prototype.hasOwnProperty.call(G, k) || k in G) continue;
      named.add(k);
      Object.defineProperty(G, k, {
        configurable: true,
        enumerable: false,
        get() {
          const d = G.document;
          return d.getElementById(k) || d.querySelector('[name="' + k.replace(/["\\]/g, "\\$&") + '"]') || undefined;
        },
        set(v) { Object.defineProperty(G, k, { value: v, writable: true, configurable: true, enumerable: true }); named.delete(k); },
      });
    }
  };
  J.register = register;
  J.queueRecord = queueRecord;
})(globalThis);
