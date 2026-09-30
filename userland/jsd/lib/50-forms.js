// Forms: control state (value, checked, selectedness) mirrored to the
// browser with value/checked/selected ops, form.elements, submission,
// reset, FormData and constraint validation.
"use strict";
(function (G) {
  const J = G.__jsd;
  const { HTMLElement } = G;
  const { refStr, refBool, refNum } = J;

  const LISTED = new Set(["button", "fieldset", "input", "object", "output", "select", "textarea"]);
  const SUBMITTABLE = new Set(["button", "input", "select", "textarea"]);

  // The owner form: form="id" or the nearest ancestor form.
  function formOwner(el) {
    const f = el.getAttribute("form");
    if (f !== null) {
      const e = el.isConnected ? el.ownerDocument.getElementById(f) : null;
      return e && e._tag === "form" ? e : null;
    }
    for (let n = el.parentNode; n; n = n.parentNode) if (n._tag === "form") return n;
    return null;
  }

  function disabled(el) {
    if (el.hasAttribute("disabled")) return true;
    for (let n = el.parentNode; n && n.nodeType === 1; n = n.parentNode) {
      if (n._tag === "fieldset" && n.hasAttribute("disabled")) {
        // The first legend's contents stay enabled.
        const legend = n._c.find((c) => c._tag === "legend");
        if (!legend || !legend.contains(el)) return true;
      }
    }
    return false;
  }

  class ValidityState {
    constructor(el) { this._el = el; }
    get valueMissing() { return this._el._check().valueMissing; }
    get typeMismatch() { return this._el._check().typeMismatch; }
    get patternMismatch() { return this._el._check().patternMismatch; }
    get tooLong() { return this._el._check().tooLong; }
    get tooShort() { return this._el._check().tooShort; }
    get rangeUnderflow() { return this._el._check().rangeUnderflow; }
    get rangeOverflow() { return this._el._check().rangeOverflow; }
    get stepMismatch() { return this._el._check().stepMismatch; }
    get badInput() { return false; }
    get customError() { return !!this._el._custom; }
    get valid() { const c = this._el._check(); return !Object.values(c).some(Boolean) && !this._el._custom; }
  }

  // Shared control behaviour.
  const Control = (Base) => class extends Base {
    get form() { return formOwner(this); }
    get disabled() { return this.hasAttribute("disabled"); }
    set disabled(v) { this.toggleAttribute("disabled", !!v); }
    get name() { return this.getAttribute("name") || ""; }
    set name(v) { this.setAttribute("name", v); }
    get labels() {
      const d = this.ownerDocument;
      return J.staticList(J.descendants(d).filter((l) => l._tag === "label" && l.control === this));
    }
    get validity() { return new ValidityState(this); }
    get validationMessage() {
      if (this._custom) return this._custom;
      const c = this._check();
      if (c.valueMissing) return "Please fill out this field.";
      if (c.typeMismatch) return this.type === "email" ? "Please enter an email address." : "Please enter a URL.";
      if (c.patternMismatch) return "Please match the requested format.";
      if (c.tooLong) return "Please shorten this text.";
      if (c.tooShort) return "Please lengthen this text to " + this.minLength + " characters or more.";
      if (c.rangeUnderflow) return "Value must be greater than or equal to " + this.min + ".";
      if (c.rangeOverflow) return "Value must be less than or equal to " + this.max + ".";
      if (c.stepMismatch) return "Please enter a valid value.";
      return "";
    }
    get willValidate() { return !disabled(this) && !this.hasAttribute("readonly") && !/^(hidden|button|reset)$/.test(this.type || ""); }
    setCustomValidity(m) { this._custom = String(m); }
    checkValidity() {
      if (!this.willValidate || this.validity.valid) return true;
      J.dispatchEvent(this, new G.Event("invalid", { cancelable: true }));
      return false;
    }
    reportValidity() {
      const ok = this.checkValidity();
      if (!ok) J.queue({ t: "invalid", id: this.__id, message: this.validationMessage });
      return ok;
    }
    _check() { return {}; }
  };

  // ---- input ----
  const TEXTLIKE = /^(text|search|url|tel|email|password|number|date|time|datetime-local|month|week|color|range)$/;
  const EMAIL = /^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$/;
  class HTMLInputElement extends Control(HTMLElement) {
    get type() {
      const t = (this.getAttribute("type") || "text").toLowerCase();
      return /^(hidden|text|search|tel|url|email|password|date|month|week|time|datetime-local|number|range|color|checkbox|radio|file|submit|image|reset|button)$/.test(t) ? t : "text";
    }
    set type(v) { this.setAttribute("type", v); }
    get value() {
      const t = this.type;
      if (t === "checkbox" || t === "radio") return this.getAttribute("value") ?? "on";
      if (t === "file") return this._files && this._files.length ? "C:\\fakepath\\" + this._files[0].name : "";
      if (this._value !== undefined) return this._value;
      return this._sanitize(this.getAttribute("value") ?? "");
    }
    set value(v) {
      const t = this.type;
      v = v === null ? "" : String(v);
      if (t === "checkbox" || t === "radio" || /^(submit|image|reset|button|hidden)$/.test(t)) { this.setAttribute("value", v); return; }
      if (t === "file") { if (v === "") this._files = []; return; }
      v = this._sanitize(v);
      if (v === this._value) return;
      this._value = v;
      this._selStart = this._selEnd = v.length;
      if (!this._local) J.ops.push({ op: "value", id: this.__id, value: v });
    }
    _sanitize(v) {
      const t = this.type;
      if (TEXTLIKE.test(t) && t !== "password" && t !== "text" && t !== "search" && t !== "tel") v = v.replace(/[\r\n]/g, "");
      if (t === "url" || t === "email") v = v.trim();
      if (t === "number" && v !== "" && !/^-?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/.test(v)) v = "";
      if (t === "color") v = /^#[0-9a-fA-F]{6}$/.test(v) ? v.toLowerCase() : "#000000";
      if (t === "range") {
        const n = parseFloat(v), mn = parseFloat(this.min) || 0, mx = parseFloat(this.max);
        const hi = Number.isFinite(mx) ? mx : 100;
        v = String(Number.isFinite(n) ? Math.min(Math.max(n, mn), hi) : (mn + hi) / 2);
      }
      return v;
    }
    get defaultValue() { return this.getAttribute("value") ?? ""; }
    set defaultValue(v) { this.setAttribute("value", v); }
    get valueAsNumber() { const n = parseFloat(this.value); return Number.isFinite(n) ? n : NaN; }
    set valueAsNumber(n) { this.value = Number.isFinite(n) ? String(n) : ""; }
    get valueAsDate() { const d = new Date(this.value); return isNaN(d) ? null : d; }
    set valueAsDate(d) { this.value = d ? d.toISOString().slice(0, 10) : ""; }
    get checked() { return this._checked !== undefined ? this._checked : this.hasAttribute("checked"); }
    set checked(v) {
      v = !!v;
      if (v === this.checked && this._checked !== undefined) return;
      this._checked = v;
      if (!this._local) J.ops.push({ op: "checked", id: this.__id, checked: v });
      if (v && this.type === "radio") for (const r of this._radioGroup()) if (r !== this && r.checked) r.checked = false;
    }
    get defaultChecked() { return this.hasAttribute("checked"); }
    set defaultChecked(v) { this.toggleAttribute("checked", !!v); }
    get indeterminate() { return !!this._indet; }
    set indeterminate(v) { this._indet = !!v; }
    get files() { return this.type === "file" ? (this._fl || (this._fl = J.fileList(this))) : null; }
    set files(v) { if (this.type === "file") this._files = Array.from(v || []); }
    get list() { const i = this.getAttribute("list"); return i ? this.ownerDocument.getElementById(i) : null; }
    _radioGroup() {
      const name = this.name;
      if (!name) return [this];
      const root = this.form || this.getRootNode();
      return J.descendants(root).filter((e) => e._tag === "input" && e.type === "radio" && e.name === name && e.form === this.form);
    }
    // Selection (tracked for scripts; the browser owns the caret).
    get selectionStart() { return this._selStart ?? this.value.length; }
    set selectionStart(v) { this._selStart = v; }
    get selectionEnd() { return this._selEnd ?? this.value.length; }
    set selectionEnd(v) { this._selEnd = v; }
    get selectionDirection() { return "none"; }
    select() { this._selStart = 0; this._selEnd = this.value.length; J.dispatchEvent(this, new G.Event("select", { bubbles: true })); }
    setSelectionRange(a, b) { this._selStart = a; this._selEnd = b; }
    setRangeText(r, a, b) {
      a = a ?? this.selectionStart; b = b ?? this.selectionEnd;
      const v = this.value;
      this.value = v.slice(0, a) + r + v.slice(b);
    }
    stepUp(n = 1) { this.valueAsNumber = (this.valueAsNumber || 0) + n * (parseFloat(this.step) || 1); }
    stepDown(n = 1) { this.stepUp(-n); }
    showPicker() {}
    _check() {
      const t = this.type, v = this.value;
      const r = { valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false };
      if (this.required) {
        if (t === "checkbox") r.valueMissing = !this.checked;
        else if (t === "radio") r.valueMissing = !this._radioGroup().some((x) => x.checked);
        else if (t === "file") r.valueMissing = !(this._files && this._files.length);
        else r.valueMissing = v === "";
      }
      if (v === "") return r;
      if (t === "email") r.typeMismatch = !(this.multiple ? v.split(",").every((x) => EMAIL.test(x.trim())) : EMAIL.test(v));
      if (t === "url") { try { new G.URL(v); } catch (e) { r.typeMismatch = true; } }
      const pat = this.getAttribute("pattern");
      if (pat !== null && /^(text|search|url|tel|email|password)$/.test(t)) {
        try { r.patternMismatch = !new RegExp("^(?:" + pat + ")$", "v").test(v); } catch (e) {
          try { r.patternMismatch = !new RegExp("^(?:" + pat + ")$", "u").test(v); } catch (e2) {}
        }
      }
      if (this._value !== undefined) {
        const mx = this.maxLength, mn = this.minLength;
        if (mx >= 0 && v.length > mx) r.tooLong = true;
        if (mn >= 0 && v.length < mn) r.tooShort = true;
      }
      if (t === "number" || t === "range") {
        const n = parseFloat(v), mn = parseFloat(this.min), mx = parseFloat(this.max);
        if (Number.isFinite(mn) && n < mn) r.rangeUnderflow = true;
        if (Number.isFinite(mx) && n > mx) r.rangeOverflow = true;
        const st = this.step;
        if (st !== "any") {
          const s = parseFloat(st) || 1, base = Number.isFinite(mn) ? mn : 0;
          const q = (n - base) / s;
          if (Math.abs(q - Math.round(q)) > 1e-9) r.stepMismatch = true;
        }
      }
      return r;
    }
    _activation(ev) {
      if (disabled(this)) return;
      const t = this.type;
      const f = this.form;
      if ((t === "submit" || t === "image") && f) f._submit(this);
      else if (t === "reset" && f) f.reset();
      void ev;
    }
  }
  for (const p of ["accept", "alt", "autocomplete", "dirName", "max", "min", "placeholder", "src", "step", "formAction", "formEnctype", "formMethod", "formTarget", "inputMode"]) refStr(HTMLInputElement.prototype, p);
  for (const p of ["autofocus", "multiple", "readOnly", "required", "formNoValidate"]) refBool(HTMLInputElement.prototype, p);
  refNum(HTMLInputElement.prototype, "maxLength", "maxlength", -1);
  refNum(HTMLInputElement.prototype, "minLength", "minlength", -1);
  refNum(HTMLInputElement.prototype, "size", "size", 20);
  G.HTMLInputElement = HTMLInputElement;
  J.tagClass.input = HTMLInputElement;

  // ---- textarea ----
  class HTMLTextAreaElement extends Control(HTMLElement) {
    get type() { return "textarea"; }
    get value() { return this._value !== undefined ? this._value : this.defaultValue; }
    set value(v) {
      v = v === null ? "" : String(v).replace(/\r\n?/g, "\n");
      if (v === this._value) return;
      this._value = v;
      if (!this._local) J.ops.push({ op: "value", id: this.__id, value: v });
    }
    get defaultValue() { return this.textContent; }
    set defaultValue(v) { this.textContent = v; }
    get textLength() { return this.value.length; }
    select() { this._selStart = 0; this._selEnd = this.value.length; }
    setSelectionRange(a, b) { this._selStart = a; this._selEnd = b; }
    get selectionStart() { return this._selStart ?? this.value.length; }
    set selectionStart(v) { this._selStart = v; }
    get selectionEnd() { return this._selEnd ?? this.value.length; }
    set selectionEnd(v) { this._selEnd = v; }
    setRangeText(r, a, b) { a = a ?? this.selectionStart; b = b ?? this.selectionEnd; const v = this.value; this.value = v.slice(0, a) + r + v.slice(b); }
    _check() {
      const v = this.value;
      const r = { valueMissing: this.required && v === "" };
      if (this._value !== undefined && v !== "") {
        if (this.maxLength >= 0 && v.length > this.maxLength) r.tooLong = true;
        if (this.minLength >= 0 && v.length < this.minLength) r.tooShort = true;
      }
      return r;
    }
  }
  for (const p of ["placeholder", "wrap", "autocomplete", "dirName"]) refStr(HTMLTextAreaElement.prototype, p);
  for (const p of ["autofocus", "readOnly", "required"]) refBool(HTMLTextAreaElement.prototype, p);
  refNum(HTMLTextAreaElement.prototype, "maxLength", "maxlength", -1);
  refNum(HTMLTextAreaElement.prototype, "minLength", "minlength", -1);
  refNum(HTMLTextAreaElement.prototype, "rows", "rows", 2);
  refNum(HTMLTextAreaElement.prototype, "cols", "cols", 20);
  G.HTMLTextAreaElement = HTMLTextAreaElement;
  J.tagClass.textarea = HTMLTextAreaElement;

  // ---- select / option ----
  class HTMLOptionElement extends HTMLElement {
    get select() { for (let n = this.parentNode; n && n.nodeType === 1; n = n.parentNode) if (n._tag === "select") return n; return null; }
    get form() { const s = this.select; return s ? s.form : null; }
    get value() { return this.getAttribute("value") ?? this.text; }
    set value(v) { this.setAttribute("value", v); }
    get text() { return this.textContent.replace(/\s+/g, " ").trim(); }
    set text(v) { this.textContent = v; }
    get label() { return this.getAttribute("label") ?? this.text; }
    set label(v) { this.setAttribute("label", v); }
    get index() { const s = this.select; return s ? s._options().indexOf(this) : 0; }
    get defaultSelected() { return this.hasAttribute("selected"); }
    set defaultSelected(v) { this.toggleAttribute("selected", !!v); }
    get selected() {
      if (this._sel !== undefined) return this._sel;
      const s = this.select;
      if (s && !s.multiple) return s._selectedOption() === this;
      return this.hasAttribute("selected");
    }
    set selected(v) {
      const s = this.select;
      if (s && !s.multiple && v) for (const o of s._options()) o._sel = false;
      else if (s && !s.multiple) for (const o of s._options()) if (o._sel === undefined) o._sel = o.selected;
      this._sel = !!v;
      if (s) s._sync();
    }
    get disabled() { return this.hasAttribute("disabled"); }
    set disabled(v) { this.toggleAttribute("disabled", !!v); }
  }
  G.HTMLOptionElement = HTMLOptionElement;
  J.tagClass.option = HTMLOptionElement;
  class HTMLOptGroupElement extends HTMLElement {}
  refStr(HTMLOptGroupElement.prototype, "label");
  refBool(HTMLOptGroupElement.prototype, "disabled");
  G.HTMLOptGroupElement = HTMLOptGroupElement;
  J.tagClass.optgroup = HTMLOptGroupElement;
  // new Option(text, value, defaultSelected, selected)
  G.Option = function Option(text, value, defSel, sel) {
    const o = G.document.createElement("option");
    if (text !== undefined) o.text = text;
    if (value !== undefined) o.value = value;
    if (defSel) o.defaultSelected = true;
    if (sel) o.selected = true;
    return o;
  };
  G.Option.prototype = HTMLOptionElement.prototype;

  class HTMLOptionsCollection extends G.HTMLCollection {
    constructor(sel) { super(() => sel._options()); Object.defineProperty(this, "_s", { value: sel }); }
    add(o, before) { this._s.add(o, before); }
    remove(i) { this._s.remove(i); }
    get selectedIndex() { return this._s.selectedIndex; }
    set selectedIndex(i) { this._s.selectedIndex = i; }
    set length(n) {
      const o = this._s._options();
      for (let i = o.length - 1; i >= n; i--) o[i].remove();
      for (let i = o.length; i < n; i++) this._s.appendChild(G.document.createElement("option"));
    }
    get length() { return this._s._options().length; }
  }

  class HTMLSelectElement extends Control(HTMLElement) {
    _options() { return J.descendants(this).filter((e) => e._tag === "option"); }
    // Without explicit selectedness, the first selected option (or the
    // first enabled option in a single select).
    _selectedOption() {
      const opts = this._options();
      const exp = opts.filter((o) => o._sel !== undefined);
      if (exp.length) return opts.find((o) => o._sel) || null;
      const marked = opts.filter((o) => o.hasAttribute("selected"));
      if (marked.length) return marked[marked.length - 1];
      if (this.multiple || this.size > 1) return null;
      return opts.find((o) => !o.disabled) || null;
    }
    _sync() {
      if (this._local) return;
      const opts = this._options();
      if (this.multiple) J.ops.push({ op: "selected", id: this.__id, indices: opts.map((o, i) => (o.selected ? i : -1)).filter((i) => i >= 0) });
      else J.ops.push({ op: "selected", id: this.__id, index: this.selectedIndex });
    }
    get type() { return this.multiple ? "select-multiple" : "select-one"; }
    get options() { return J.liveList(() => this._options(), HTMLOptionsCollection.bind(null, this)); }
    get length() { return this._options().length; }
    set length(n) { this.options.length = n; }
    item(i) { return this._options()[i] || null; }
    namedItem(n) { return this._options().find((o) => o.id === n || o.getAttribute("name") === n) || null; }
    get selectedOptions() { return J.liveList(() => this._options().filter((o) => o.selected), G.HTMLCollection); }
    get selectedIndex() {
      const o = this._options();
      if (this.multiple) return o.findIndex((x) => x.selected);
      return o.indexOf(this._selectedOption());
    }
    set selectedIndex(i) {
      const o = this._options();
      for (const [k, x] of o.entries()) x._sel = k === i;
      this._sync();
    }
    get value() { const o = this.multiple ? this._options().find((x) => x.selected) : this._selectedOption(); return o ? o.value : ""; }
    set value(v) {
      const o = this._options();
      let hit = false;
      for (const x of o) { x._sel = !hit && x.value === String(v); if (x._sel) hit = true; }
      this._sync();
    }
    add(o, before) {
      const b = typeof before === "number" ? this._options()[before] : before;
      if (b) b.parentNode.insertBefore(o, b); else this.appendChild(o);
    }
    remove(i) {
      if (i === undefined) { Element.prototype.remove.call(this); return; }
      const o = this._options()[i];
      if (o) o.remove();
    }
    _check() { return { valueMissing: this.required && (this.selectedIndex < 0 || this.value === "") }; }
  }
  const Element = G.Element;
  refBool(HTMLSelectElement.prototype, "multiple");
  refBool(HTMLSelectElement.prototype, "required");
  refBool(HTMLSelectElement.prototype, "autofocus");
  refNum(HTMLSelectElement.prototype, "size", "size", 0);
  G.HTMLSelectElement = HTMLSelectElement;
  J.tagClass.select = HTMLSelectElement;

  // ---- button, output, fieldset ----
  class HTMLButtonElement extends Control(HTMLElement) {
    get type() { const t = (this.getAttribute("type") || "").toLowerCase(); return t === "reset" || t === "button" ? t : "submit"; }
    set type(v) { this.setAttribute("type", v); }
    get value() { return this.getAttribute("value") || ""; }
    set value(v) { this.setAttribute("value", v); }
    get willValidate() { return false; }
    _activation() {
      if (disabled(this)) return;
      const f = this.form;
      if (!f) return;
      if (this.type === "submit") f._submit(this);
      else if (this.type === "reset") f.reset();
    }
  }
  for (const p of ["formAction", "formEnctype", "formMethod", "formTarget"]) refStr(HTMLButtonElement.prototype, p);
  refBool(HTMLButtonElement.prototype, "formNoValidate");
  refBool(HTMLButtonElement.prototype, "autofocus");
  G.HTMLButtonElement = HTMLButtonElement;
  J.tagClass.button = HTMLButtonElement;

  class HTMLOutputElement extends Control(HTMLElement) {
    get type() { return "output"; }
    get value() { return this.textContent; }
    set value(v) { this.textContent = v; }
    get defaultValue() { return this._def ?? this.textContent; }
    set defaultValue(v) { this._def = String(v); }
    get htmlFor() { return new G.DOMTokenList(this, "for"); }
    get willValidate() { return false; }
  }
  G.HTMLOutputElement = HTMLOutputElement;
  J.tagClass.output = HTMLOutputElement;
  Object.defineProperty(G.HTMLFieldSetElement.prototype, "elements", {
    get() { return J.liveList(() => J.descendants(this).filter((e) => LISTED.has(e._tag)), G.HTMLCollection); },
  });
  Object.defineProperty(G.HTMLFieldSetElement.prototype, "form", { get() { return formOwner(this); } });
  Object.defineProperty(G.HTMLFieldSetElement.prototype, "type", { get() { return "fieldset"; } });
  Object.defineProperty(G.HTMLLegendElement.prototype, "form", { get() { const p = this.parentNode; return p && p._tag === "fieldset" ? p.form : null; } });

  // ---- form ----
  class HTMLFormControlsCollection extends G.HTMLCollection {
    namedItem(n) {
      const m = this._get().filter((e) => e.id === n || e.getAttribute("name") === n);
      if (m.length > 1) return new G.RadioNodeList(m);
      return m[0] || null;
    }
  }
  class RadioNodeList extends G.NodeList {
    constructor(list) { super(() => list); }
    get value() { const r = this._get().find((e) => e.checked); return r ? r.value : ""; }
    set value(v) { const r = this._get().find((e) => e.value === v); if (r) r.checked = true; }
  }
  G.RadioNodeList = RadioNodeList;
  G.HTMLFormControlsCollection = HTMLFormControlsCollection;

  class HTMLFormElement extends HTMLElement {
    _controls() {
      const root = this.getRootNode();
      return J.descendants(root).filter((e) => LISTED.has(e._tag) && e.form === this && !(e._tag === "input" && e.type === "image"));
    }
    get elements() {
      const coll = J.liveList(() => this._controls(), HTMLFormControlsCollection);
      return new Proxy(coll, {
        get(t, k, r) {
          if (typeof k === "string" && !/^\d+$/.test(k) && !(k in t)) return t.namedItem(k) || undefined;
          return Reflect.get(t, k, r);
        },
      });
    }
    get length() { return this._controls().length; }
    get action() {
      const a = this.getAttribute("action");
      try { return new G.URL(a || "", this.ownerDocument.URL).href; } catch (e) { return a || ""; }
    }
    set action(v) { this.setAttribute("action", v); }
    get method() { const m = (this.getAttribute("method") || "").toLowerCase(); return m === "post" || m === "dialog" ? m : "get"; }
    set method(v) { this.setAttribute("method", v); }
    get enctype() {
      const e = (this.getAttribute("enctype") || "").toLowerCase();
      return e === "multipart/form-data" || e === "text/plain" ? e : "application/x-www-form-urlencoded";
    }
    set enctype(v) { this.setAttribute("enctype", v); }
    get encoding() { return this.enctype; }
    set encoding(v) { this.enctype = v; }
    get relList() { return new G.DOMTokenList(this, "rel"); }
    checkValidity() {
      let ok = true;
      for (const c of this._controls()) if (c.willValidate && !c.checkValidity()) ok = false;
      return ok;
    }
    reportValidity() {
      let ok = true;
      for (const c of this._controls()) {
        if (c.willValidate && !c.checkValidity()) {
          if (ok) { J.queue({ t: "invalid", id: c.__id, message: c.validationMessage }); c.focus(); }
          ok = false;
        }
      }
      return ok;
    }
    submit() { this._navigate(null); }
    requestSubmit(submitter) {
      if (submitter && (submitter.form !== this || !/^(submit|image)$/.test(submitter.type))) throw new TypeError("The specified element is not a submit button");
      this._submit(submitter || null);
    }
    // Interactive submission: validation, the submit event, then
    // navigation.
    _submit(submitter) {
      if (this._submitting) return;
      const novalidate = this.hasAttribute("novalidate") || (submitter && submitter.hasAttribute("formnovalidate"));
      if (!novalidate && !this.reportValidity()) return;
      const ev = new G.SubmitEvent("submit", { bubbles: true, cancelable: true, submitter });
      this._submitting = true;
      let ok;
      try { ok = J.dispatchEvent(this, ev); } finally { this._submitting = false; }
      if (ok) this._navigate(submitter);
    }
    _navigate(submitter) {
      const s = submitter;
      const method = ((s && s.getAttribute("formmethod")) || this.method).toLowerCase();
      if (method === "dialog") {
        const d = this.closest("dialog");
        if (d) d.close(s ? s.value : undefined);
        return;
      }
      const action = s && s.hasAttribute("formaction") ? s.formAction : this.action;
      const enctype = ((s && s.getAttribute("formenctype")) || this.enctype).toLowerCase();
      const target = (s && s.getAttribute("formtarget")) || this.getAttribute("target") || "";
      const entries = J.formEntries(this, s);
      // The browser encodes and sends it, then navigates.
      J.queue({ t: "submit", action, method: method === "post" ? "post" : "get", enctype, target, entries: entries.map(J.entryWire) });
      J.navigating = true;
    }
    reset() {
      const ev = new G.Event("reset", { bubbles: true, cancelable: true });
      if (!J.dispatchEvent(this, ev)) return;
      for (const c of this._controls()) {
        if (c._tag === "input") { c._value = undefined; c._checked = undefined; c.value = c.defaultValue; if (c.type === "checkbox" || c.type === "radio") c.checked = c.defaultChecked; }
        else if (c._tag === "textarea") { c._value = undefined; c.value = c.defaultValue; }
        else if (c._tag === "select") { for (const o of c._options()) o._sel = undefined; c._sync(); }
      }
    }
  }
  for (const p of ["acceptCharset", "autocomplete", "name", "target", "rel"]) refStr(HTMLFormElement.prototype, p, p === "acceptCharset" ? "accept-charset" : undefined);
  refBool(HTMLFormElement.prototype, "noValidate", "novalidate");
  G.HTMLFormElement = HTMLFormElement;
  J.tagClass.form = HTMLFormElement;
  // form["name"] / form[0] access to controls.
  J.formProxy = function (f) {
    return new Proxy(f, {
      get(t, k, r) {
        if (typeof k === "string" && !(k in t)) {
          if (/^\d+$/.test(k)) return r._controls()[Number(k)];
          const m = r.elements.namedItem(k);
          if (m) return m;
        }
        return Reflect.get(t, k, r);
      },
    });
  };

  // The form data set (HTML "constructing the entry list").
  J.formEntries = function (form, submitter) {
    const out = [];
    for (const c of form._controls()) {
      if (!SUBMITTABLE.has(c._tag) || disabled(c) || c.closest("datalist")) continue;
      const t = c.type;
      if ((t === "submit" || t === "button" || t === "reset") && c !== submitter) continue;
      if (c._tag === "button" && c !== submitter) continue;
      const name = c.name;
      if (t === "image" && c === submitter) {
        const p = name ? name + "." : "";
        out.push([p + "x", "0"], [p + "y", "0"]);
        continue;
      }
      if (!name) continue;
      if ((t === "checkbox" || t === "radio") && !c.checked) continue;
      if (c._tag === "select") {
        for (const o of c._options()) if (o.selected && !o.disabled) out.push([name, o.value]);
        continue;
      }
      if (t === "file") {
        const fs = c._files || [];
        if (!fs.length) out.push([name, new G.File([], "", { type: "application/octet-stream" })]);
        for (const f of fs) out.push([name, f]);
        continue;
      }
      if (name === "_charset_" && t === "hidden") { out.push([name, "UTF-8"]); continue; }
      out.push([name, c.value]);
      const dn = c.getAttribute("dirname");
      if (dn) out.push([dn, "ltr"]);
    }
    if (submitter && submitter._tag === "button" && submitter.name) {
      // (buttons are listed controls; kept in document order above)
    }
    return out;
  };
  // Entries on the wire: strings as is; files as {name, type, data(base64)}.
  J.entryWire = function ([k, v]) {
    if (typeof v === "string") return [k, v];
    return [k, { name: v.name, type: v.type, b64: J.b64(v._bytes) }];
  };

  // ---- FormData ----
  class FormData {
    constructor(form, submitter) {
      this._e = [];
      if (form) {
        if (!(form instanceof HTMLFormElement)) throw new TypeError("parameter 1 is not of type 'HTMLFormElement'");
        this._e = J.formEntries(form, submitter || null);
      }
    }
    _v(v, fname) {
      if (v instanceof G.Blob) {
        if (!(v instanceof G.File) || fname !== undefined) return new G.File([v], fname ?? (v.name || "blob"), { type: v.type });
        return v;
      }
      return String(v);
    }
    append(k, v, f) { this._e.push([String(k), this._v(v, f)]); }
    set(k, v, f) {
      k = String(k);
      const i = this._e.findIndex((e) => e[0] === k);
      const nv = [k, this._v(v, f)];
      if (i < 0) this._e.push(nv);
      else { this._e[i] = nv; this._e = this._e.filter((e, j) => j <= i || e[0] !== k); }
    }
    get(k) { const e = this._e.find((x) => x[0] === String(k)); return e ? e[1] : null; }
    getAll(k) { return this._e.filter((x) => x[0] === String(k)).map((x) => x[1]); }
    has(k) { return this._e.some((x) => x[0] === String(k)); }
    delete(k) { this._e = this._e.filter((x) => x[0] !== String(k)); }
    entries() { return this._e.map((e) => [e[0], e[1]])[Symbol.iterator](); }
    keys() { return this._e.map((e) => e[0])[Symbol.iterator](); }
    values() { return this._e.map((e) => e[1])[Symbol.iterator](); }
    forEach(f, t) { for (const [k, v] of this._e) f.call(t, v, k, this); }
    [Symbol.iterator]() { return this.entries(); }
  }
  G.FormData = FormData;
  G.ValidityState = ValidityState;

  // Focusable/submittable helpers for the rest of the library.
  J.formOwner = formOwner;
  J.isDisabled = disabled;
})(globalThis);
