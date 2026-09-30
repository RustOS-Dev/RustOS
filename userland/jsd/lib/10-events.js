// DOM events: EventTarget, Event and its common subclasses.
"use strict";
(function (G) {
  const J = G.__jsd;

  class Event {
    constructor(type, init) {
      init = init || {};
      this.type = String(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = !!init.cancelable;
      this.composed = !!init.composed;
      this.defaultPrevented = false;
      this.isTrusted = false;
      this.timeStamp = G.performance.now();
      this.target = null;
      this.currentTarget = null;
      this.eventPhase = 0;
      this._stop = false;
      this._stopNow = false;
      this._passive = false;
    }
    preventDefault() {
      if (this.cancelable && !this._passive) this.defaultPrevented = true;
    }
    stopPropagation() { this._stop = true; }
    stopImmediatePropagation() { this._stop = true; this._stopNow = true; }
    get returnValue() { return !this.defaultPrevented; }
    set returnValue(v) { if (!v) this.preventDefault(); }
    get cancelBubble() { return this._stop; }
    set cancelBubble(v) { if (v) this._stop = true; }
    get srcElement() { return this.target; }
    composedPath() { return this._path ? this._path.slice() : []; }
    initEvent(type, bubbles, cancelable) {
      this.type = String(type); this.bubbles = !!bubbles; this.cancelable = !!cancelable;
    }
  }
  Event.NONE = 0; Event.CAPTURING_PHASE = 1; Event.AT_TARGET = 2; Event.BUBBLING_PHASE = 3;

  class UIEvent extends Event {
    constructor(type, init) { super(type, init); init = init || {}; this.view = init.view || null; this.detail = init.detail || 0; }
  }
  class MouseEvent extends UIEvent {
    constructor(type, init) {
      super(type, init); init = init || {};
      for (const k of ["screenX", "screenY", "clientX", "clientY", "button", "buttons"]) this[k] = init[k] || 0;
      for (const k of ["ctrlKey", "shiftKey", "altKey", "metaKey"]) this[k] = !!init[k];
      this.relatedTarget = init.relatedTarget || null;
    }
    get pageX() { return this.clientX; }
    get pageY() { return this.clientY; }
    get offsetX() { return this.clientX; }
    get offsetY() { return this.clientY; }
    get x() { return this.clientX; }
    get y() { return this.clientY; }
    getModifierState(k) { return !!this[{ Control: "ctrlKey", Shift: "shiftKey", Alt: "altKey", Meta: "metaKey" }[k]]; }
  }
  class PointerEvent extends MouseEvent {
    constructor(type, init) { super(type, init); init = init || {}; this.pointerId = init.pointerId || 1; this.pointerType = init.pointerType || "mouse"; this.isPrimary = true; }
  }
  class WheelEvent extends MouseEvent {
    constructor(type, init) { super(type, init); init = init || {}; this.deltaX = init.deltaX || 0; this.deltaY = init.deltaY || 0; this.deltaZ = 0; this.deltaMode = 0; }
  }
  class KeyboardEvent extends UIEvent {
    constructor(type, init) {
      super(type, init); init = init || {};
      this.key = init.key || ""; this.code = init.code || ""; this.location = 0; this.repeat = !!init.repeat;
      for (const k of ["ctrlKey", "shiftKey", "altKey", "metaKey"]) this[k] = !!init[k];
      this.isComposing = false;
      this.keyCode = init.keyCode || (this.key.length === 1 ? this.key.toUpperCase().charCodeAt(0) : { Enter: 13, Escape: 27, Backspace: 8, Tab: 9, ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40, Delete: 46 }[this.key] || 0);
      this.which = this.keyCode;
      this.charCode = this.key.length === 1 ? this.key.charCodeAt(0) : 0;
    }
    getModifierState(k) { return !!this[{ Control: "ctrlKey", Shift: "shiftKey", Alt: "altKey", Meta: "metaKey" }[k]]; }
  }
  class FocusEvent extends UIEvent {
    constructor(type, init) { super(type, init); this.relatedTarget = (init && init.relatedTarget) || null; }
  }
  class InputEvent extends UIEvent {
    constructor(type, init) { super(type, init); init = init || {}; this.data = init.data ?? null; this.inputType = init.inputType || "insertText"; this.isComposing = false; }
  }
  class CustomEvent extends Event {
    constructor(type, init) { super(type, init); this.detail = init && init.detail !== undefined ? init.detail : null; }
    initCustomEvent(type, b, c, detail) { this.initEvent(type, b, c); this.detail = detail; }
  }
  class SubmitEvent extends Event {
    constructor(type, init) { super(type, init); this.submitter = (init && init.submitter) || null; }
  }
  class ErrorEvent extends Event {
    constructor(type, init) { super(type, init); init = init || {}; this.message = init.message || ""; this.error = init.error; this.filename = init.filename || ""; this.lineno = 0; this.colno = 0; }
  }
  class MessageEvent extends Event {
    constructor(type, init) { super(type, init); init = init || {}; this.data = init.data; this.origin = init.origin || ""; this.lastEventId = ""; this.source = init.source || null; this.ports = []; }
  }
  class ProgressEvent extends Event {
    constructor(type, init) { super(type, init); init = init || {}; this.lengthComputable = !!init.lengthComputable; this.loaded = init.loaded || 0; this.total = init.total || 0; }
  }
  class HashChangeEvent extends Event {
    constructor(type, init) { super(type, init); init = init || {}; this.oldURL = init.oldURL || ""; this.newURL = init.newURL || ""; }
  }
  class PopStateEvent extends Event {
    constructor(type, init) { super(type, init); this.state = init ? init.state : null; }
  }
  class PageTransitionEvent extends Event {
    constructor(type, init) { super(type, init); this.persisted = false; }
  }
  class StorageEvent extends Event {
    constructor(type, init) { super(type, init); Object.assign(this, { key: null, oldValue: null, newValue: null, url: "", storageArea: null }, init || {}); }
  }

  // Inline handler attributes (onclick="...") compiled on first use.
  function compileHandler(target, name, code) {
    let f;
    try {
      const form = target.form || null;
      const doc = target.ownerDocument || G.document;
      // Scope chain: element, form, document (as in browsers).
      f = new Function("__el", "__form", "__doc",
        "return function (event) { with (__doc) { with (__form || {}) { with (__el) {\n" + code + "\n} } } };")(target, form, doc);
    } catch (e) {
      G.__onError(e);
      f = null;
    }
    return f;
  }
  J.compileHandler = compileHandler;

  // Elements with these listeners are offered to the user as clickable.
  const CLICKY = new Set(["click", "mousedown", "mouseup", "pointerdown", "pointerup", "touchstart"]);
  J.clickTargets = new Set();
  J.markClickable = (el) => { J.clickTargets.add(el); J.clickDirty = true; };

  class EventTarget {
    constructor() { this._ls = null; }
    addEventListener(type, cb, opts) {
      if (!cb) return;
      const capture = typeof opts === "boolean" ? opts : !!(opts && opts.capture);
      const once = !!(opts && typeof opts === "object" && opts.once);
      const passive = !!(opts && typeof opts === "object" && opts.passive);
      const signal = opts && typeof opts === "object" ? opts.signal : null;
      if (signal && signal.aborted) return;
      if (!this._ls) this._ls = Object.create(null);
      const list = this._ls[type] || (this._ls[type] = []);
      if (list.some((l) => l.cb === cb && l.capture === capture)) return;
      const l = { cb, capture, once, passive, removed: false };
      list.push(l);
      if (this.nodeType === 1 && CLICKY.has(type)) J.markClickable(this);
      if (signal) signal.addEventListener("abort", () => this.removeEventListener(type, cb, { capture }));
    }
    removeEventListener(type, cb, opts) {
      const capture = typeof opts === "boolean" ? opts : !!(opts && opts.capture);
      const list = this._ls && this._ls[type];
      if (!list) return;
      const i = list.findIndex((l) => l.cb === cb && l.capture === capture);
      if (i >= 0) { list[i].removed = true; list.splice(i, 1); }
    }
    dispatchEvent(ev) {
      if (!(ev instanceof Event)) throw new TypeError("not an Event");
      return J.dispatchEvent(this, ev);
    }
  }

  // The propagation path: target, its ancestors, the document, the window.
  function path(t) {
    const p = [];
    for (let n = t; n; n = n.parentNode) p.push(n);
    if (p.length && p[p.length - 1].nodeType === 9 && G.window) p.push(G.window);
    return p;
  }

  function invoke(node, ev, phase) {
    ev.currentTarget = node;
    ev.eventPhase = phase;
    const list = node._ls && node._ls[ev.type];
    if (list) {
      for (const l of list.slice()) {
        if (l.removed) continue;
        if (phase === 1 && !l.capture) continue;
        if (phase === 3 && l.capture) continue;
        if (l.once) node.removeEventListener(ev.type, l.cb, { capture: l.capture });
        ev._passive = l.passive;
        try {
          if (typeof l.cb === "function") l.cb.call(node, ev);
          else if (l.cb && typeof l.cb.handleEvent === "function") l.cb.handleEvent(ev);
        } catch (e) { G.__onError(e); }
        ev._passive = false;
        if (ev._stopNow) return;
      }
    }
    // on<type> handler (property or attribute), in the bubbling/target
    // phases.
    if (phase !== 1) {
      const h = J.handlerOf(node, ev.type);
      if (h) {
        let r;
        try { r = h.call(node, ev); } catch (e) { G.__onError(e); }
        if (r === false && ev.type !== "mouseover" && ev.type !== "error") ev.preventDefault();
        else if (ev.type === "beforeunload" && typeof r === "string") ev.preventDefault();
      }
    }
  }

  J.handlerOf = function (node, type) {
    const key = "on" + type;
    const own = node._on && node._on[key];
    if (own !== undefined) return typeof own === "function" ? own : null;
    if (node.nodeType === 1 && node.hasAttribute(key)) {
      const code = node.getAttribute(key);
      if (!node._compiled) node._compiled = Object.create(null);
      const c = node._compiled[key];
      if (c && c.code === code) return c.fn;
      const fn = compileHandler(node, key, code);
      node._compiled[key] = { code, fn };
      return fn;
    }
    if (node === G.window && G.document && G.document.body && type in bodyForwarded) {
      return J.handlerOf(G.document.body, type) || null;
    }
    return null;
  };
  const bodyForwarded = { load: 1, unload: 1, beforeunload: 1, hashchange: 1, popstate: 1, message: 1, resize: 1, error: 1, storage: 1, online: 1, offline: 1 };

  J.dispatchEvent = function (target, ev) {
    ev.target = target;
    ev._stop = false; ev._stopNow = false;
    const p = path(target);
    ev._path = p;
    for (let i = p.length - 1; i > 0 && !ev._stop; i--) invoke(p[i], ev, 1);
    if (!ev._stop) {
      // At target: capture listeners then bubble listeners.
      invoke(p[0], ev, 1);
      if (!ev._stopNow) invoke(p[0], ev, 3);
    }
    if (ev.bubbles) for (let i = 1; i < p.length && !ev._stop; i++) invoke(p[i], ev, 3);
    ev.currentTarget = null;
    ev.eventPhase = 0;
    return !ev.defaultPrevented;
  };

  // `el.onclick = fn` style handler properties on every event target.
  J.defineHandlerProps = function (proto, types) {
    for (const t of types) {
      const key = "on" + t;
      Object.defineProperty(proto, key, {
        configurable: true,
        get() { return (this._on && this._on[key] !== undefined) ? this._on[key] : (this.nodeType === 1 && this.hasAttribute(key) ? J.handlerOf(this, t) : null); },
        set(v) {
          if (!this._on) this._on = Object.create(null);
          this._on[key] = typeof v === "function" ? v : null;
          if (this.nodeType === 1 && CLICKY.has(t) && typeof v === "function") J.markClickable(this);
        },
      });
    }
  };
  J.eventTypes = ["abort", "blur", "cancel", "change", "click", "close", "contextmenu", "dblclick", "error", "focus", "focusin", "focusout",
    "input", "invalid", "keydown", "keypress", "keyup", "load", "mousedown", "mouseenter", "mouseleave", "mousemove", "mouseout", "mouseover",
    "mouseup", "wheel", "reset", "resize", "scroll", "select", "submit", "toggle", "pointerdown", "pointerup", "pointermove", "pointerover",
    "pointerout", "pointerenter", "pointerleave", "touchstart", "touchend", "touchmove", "animationend", "transitionend", "beforeinput", "search",
    "unload", "beforeunload", "hashchange", "popstate", "message", "storage", "online", "offline", "pageshow", "pagehide", "DOMContentLoaded", "readystatechange", "visibilitychange"];
  J.defineHandlerProps(EventTarget.prototype, J.eventTypes);

  Object.assign(G, { Event, UIEvent, MouseEvent, PointerEvent, WheelEvent, KeyboardEvent, FocusEvent, InputEvent, CustomEvent,
    SubmitEvent, ErrorEvent, MessageEvent, ProgressEvent, HashChangeEvent, PopStateEvent, PageTransitionEvent, StorageEvent, EventTarget });
})(globalThis);
