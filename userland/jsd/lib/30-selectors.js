// Selectors for querySelector(All), matches() and closest(): a parser to
// an AST and a right-to-left matcher (type, id, class, attributes,
// combinators, :is/:where/:not/:has, structural and state
// pseudo-classes).
"use strict";
(function (G) {
  const J = G.__jsd;
  const cache = new Map();

  function syntax(s) { return new G.DOMException("'" + s + "' is not a valid selector", "SyntaxError"); }

  // Tokens: ident, #hash, .class, [attr], :pseudo(args), combinators, ","
  function parseList(src) {
    const s = String(src);
    let i = 0;
    const ws = () => { while (i < s.length && /\s/.test(s[i])) i++; };
    const ident = () => {
      let out = "";
      while (i < s.length) {
        const c = s[i];
        if (/[\w\- -￿]/.test(c)) { out += c; i++; }
        else if (c === "\\" && i + 1 < s.length) {
          i++;
          const m = /^[0-9a-fA-F]{1,6} ?/.exec(s.slice(i));
          if (m) { out += String.fromCodePoint(parseInt(m[0], 16)); i += m[0].length; }
          else { out += s[i]; i++; }
        } else break;
      }
      return out;
    };
    const string = () => {
      const q = s[i++];
      let out = "";
      while (i < s.length && s[i] !== q) { if (s[i] === "\\") i++; out += s[i++]; }
      i++;
      return out;
    };
    // Balanced parentheses content.
    const args = () => {
      let depth = 1, start = i;
      while (i < s.length && depth) {
        if (s[i] === "(") depth++;
        else if (s[i] === ")") depth--;
        else if (s[i] === '"' || s[i] === "'") { string(); continue; }
        i++;
      }
      return s.slice(start, i - 1);
    };
    function compound() {
      const parts = [];
      for (;;) {
        const c = s[i];
        if (c === "*") { i++; parts.push({ k: "any" }); }
        else if (c === "#") { i++; parts.push({ k: "id", v: ident() }); }
        else if (c === ".") { i++; const v = ident(); if (!v) throw syntax(s); parts.push({ k: "class", v }); }
        else if (c === "&") { i++; parts.push({ k: "scope" }); }
        else if (c === "[") {
          i++; ws();
          let name = ident();
          if (s[i] === "|") { i++; name = ident(); }
          ws();
          let op = null, val = null, ci = false;
          if (s[i] !== "]") {
            const m = /^([~|^$*]?=)/.exec(s.slice(i));
            if (!m) throw syntax(s);
            op = m[1]; i += m[1].length; ws();
            val = s[i] === '"' || s[i] === "'" ? string() : ident();
            ws();
            if (/[is]/i.test(s[i] || "") && s[i + 1] !== undefined) { ci = s[i].toLowerCase() === "i"; i++; ws(); }
          }
          if (s[i] !== "]") throw syntax(s);
          i++;
          parts.push({ k: "attr", name: name.toLowerCase(), op, val, ci });
        } else if (c === ":") {
          i++;
          let pe = false;
          if (s[i] === ":") { i++; pe = true; }
          const name = ident().toLowerCase();
          let arg = null;
          if (s[i] === "(") { i++; arg = args(); }
          if (pe || /^(before|after|first-line|first-letter)$/.test(name)) parts.push({ k: "pseudoEl", v: name });
          else parts.push(pseudo(name, arg));
        } else if (/[\w\-\\ -￿]/.test(c || "")) {
          const t = ident();
          if (s[i] === "|") { i++; parts.push({ k: "tag", v: ident().toLowerCase() }); }
          else parts.push({ k: "tag", v: t.toLowerCase() });
        } else break;
      }
      return parts;
    }
    function pseudo(name, arg) {
      switch (name) {
        case "not": case "is": case "where": case "matches": case "-webkit-any": case "any":
          return { k: name === "not" ? "not" : "is", list: parseList(arg), forgiving: name !== "not" };
        case "has": return { k: "has", list: parseList(arg).map((sel) => sel) };
        case "nth-child": case "nth-last-child": case "nth-of-type": case "nth-last-of-type": {
          let ab = arg.trim(), of = null;
          const m = /\s+of\s+/i.exec(ab);
          if (m) { of = parseList(ab.slice(m.index + m[0].length)); ab = ab.slice(0, m.index); }
          return { k: name, ab: nth(ab), of };
        }
        case "lang": return { k: "lang", v: String(arg).trim().toLowerCase() };
        case "dir": return { k: "never" };
        default: return { k: "state", v: name };
      }
    }
    function nth(t) {
      t = t.replace(/\s+/g, "").toLowerCase();
      if (t === "odd") return [2, 1];
      if (t === "even") return [2, 0];
      const m = /^([+-]?\d*)n([+-]\d+)?$/.exec(t);
      if (m) return [m[1] === "" || m[1] === "+" ? 1 : m[1] === "-" ? -1 : Number(m[1]), m[2] ? Number(m[2]) : 0];
      if (/^[+-]?\d+$/.test(t)) return [0, Number(t)];
      throw syntax(t);
    }
    // Complex selectors separated by commas.
    const list = [];
    ws();
    for (;;) {
      const sel = [];
      let comb = null;
      ws();
      if (/[>+~]/.test(s[i] || "")) { comb = s[i]; i++; ws(); sel.relative = comb; }
      for (;;) {
        const parts = compound();
        if (!parts.length) throw syntax(s);
        sel.push({ comb: comb || " ", parts });
        const before = i;
        ws();
        if (i >= s.length || s[i] === "," || s[i] === ")") break;
        if (/[>+~]/.test(s[i])) { comb = s[i]; i++; ws(); }
        else if (i > before) comb = " ";
        else throw syntax(s);
      }
      list.push(sel);
      ws();
      if (s[i] === ",") { i++; continue; }
      if (i < s.length) throw syntax(s);
      break;
    }
    return list;
  }

  function nthOk([a, b], pos) {
    if (a === 0) return pos === b;
    const d = pos - b;
    return d % a === 0 && d / a >= 0;
  }

  function sibIndex(e, fromEnd, pred) {
    let n = 1;
    let s = fromEnd ? e.nextElementSibling : e.previousElementSibling;
    while (s) { if (pred(s)) n++; s = fromEnd ? s.nextElementSibling : s.previousElementSibling; }
    return n;
  }

  function stateMatches(e, v, scope) {
    const tag = e._tag;
    switch (v) {
      case "root": return e === e.ownerDocument.documentElement;
      case "scope": return scope ? e === scope : e === e.ownerDocument.documentElement;
      case "empty": return e._c.every((c) => c.nodeType === 8 || (c.nodeType === 3 && c._data === ""));
      case "first-child": return !e.previousElementSibling;
      case "last-child": return !e.nextElementSibling;
      case "only-child": return !e.previousElementSibling && !e.nextElementSibling;
      case "first-of-type": return sibIndex(e, false, (s) => s._tag === tag) === 1;
      case "last-of-type": return sibIndex(e, true, (s) => s._tag === tag) === 1;
      case "only-of-type": return sibIndex(e, false, (s) => s._tag === tag) === 1 && sibIndex(e, true, (s) => s._tag === tag) === 1;
      case "link": case "any-link": case "-webkit-any-link": return (tag === "a" || tag === "area") && e.hasAttribute("href");
      case "visited": return false;
      case "checked": return (tag === "input" && (e.type === "checkbox" || e.type === "radio") && e.checked) || (tag === "option" && e.selected);
      case "disabled": return !!e.disabled && /^(input|button|select|textarea|option|optgroup|fieldset)$/.test(tag);
      case "enabled": return /^(input|button|select|textarea|option|optgroup|fieldset)$/.test(tag) && !e.disabled;
      case "required": return !!e.required;
      case "optional": return /^(input|select|textarea)$/.test(tag) && !e.required;
      case "read-only": return !(/^(input|textarea)$/.test(tag) && !e.readOnly && !e.disabled);
      case "read-write": return /^(input|textarea)$/.test(tag) && !e.readOnly && !e.disabled;
      case "placeholder-shown": return /^(input|textarea)$/.test(tag) && e.hasAttribute("placeholder") && e.value === "";
      case "focus": return e.ownerDocument.activeElement === e;
      case "focus-visible": return e.ownerDocument.activeElement === e;
      case "focus-within": { const a = e.ownerDocument.activeElement; return !!a && e.contains(a); }
      case "hover": case "active": return false;
      case "target": return !!e.id && G.location && G.location.hash === "#" + e.id;
      case "defined": return true;
      case "valid": return typeof e.checkValidity === "function" ? e.checkValidity() : true;
      case "invalid": return typeof e.checkValidity === "function" ? !e.checkValidity() : false;
      case "indeterminate": return !!e.indeterminate;
      case "default": return (tag === "input" && e.hasAttribute("checked")) || (tag === "option" && e.hasAttribute("selected"));
      case "open": return e.hasAttribute("open");
      case "modal": return !!e._modal;
      default: return false;
    }
  }

  function attrOk(e, p) {
    const v = e.getAttribute(p.name);
    if (v === null) return false;
    if (!p.op) return true;
    const a = p.ci ? v.toLowerCase() : v, w = p.ci ? p.val.toLowerCase() : p.val;
    switch (p.op) {
      case "=": return a === w;
      case "~=": return w !== "" && a.split(/\s+/).includes(w);
      case "|=": return a === w || a.startsWith(w + "-");
      case "^=": return w !== "" && a.startsWith(w);
      case "$=": return w !== "" && a.endsWith(w);
      case "*=": return w !== "" && a.includes(w);
    }
    return false;
  }

  function compoundOk(e, parts, scope) {
    for (const p of parts) {
      switch (p.k) {
        case "any": break;
        case "tag": if (e._tag !== p.v) return false; break;
        case "id": if (e.getAttribute("id") !== p.v) return false; break;
        case "class": if (!e.classList.contains(p.v)) return false; break;
        case "attr": if (!attrOk(e, p)) return false; break;
        case "scope": if (scope && e !== scope) return false; break;
        case "not": if (listOk(e, p.list, scope)) return false; break;
        case "is": if (!listOk(e, p.list, scope)) return false; break;
        case "has": if (!p.list.some((sel) => hasOk(e, sel))) return false; break;
        case "nth-child": case "nth-last-child": {
          if (p.of && !listOk(e, p.of, scope)) return false;
          const pred = p.of ? (s) => listOk(s, p.of, scope) : () => true;
          if (!nthOk(p.ab, sibIndex(e, p.k === "nth-last-child", pred))) return false;
          break;
        }
        case "nth-of-type": case "nth-last-of-type":
          if (!nthOk(p.ab, sibIndex(e, p.k === "nth-last-of-type", (s) => s._tag === e._tag))) return false;
          break;
        case "lang": {
          let n = e, l = null;
          while (n && n.nodeType === 1 && l === null) { l = n.getAttribute("lang"); n = n.parentNode; }
          l = (l || "").toLowerCase();
          if (!(l === p.v || l.startsWith(p.v + "-"))) return false;
          break;
        }
        case "state": if (!stateMatches(e, p.v, scope)) return false; break;
        case "pseudoEl": return false; // elements never match a pseudo-element
        default: return false;
      }
    }
    return true;
  }

  // Match complex selector `sel` ending at index k against element e.
  function complexOk(e, sel, k, scope) {
    if (!compoundOk(e, sel[k].parts, scope)) return false;
    if (k === 0) return true;
    const comb = sel[k].comb;
    if (comb === ">") { const p = e.parentElement; return !!p && complexOk(p, sel, k - 1, scope); }
    if (comb === " ") { for (let p = e.parentElement; p; p = p.parentElement) if (complexOk(p, sel, k - 1, scope)) return true; return false; }
    if (comb === "+") { const s = e.previousElementSibling; return !!s && complexOk(s, sel, k - 1, scope); }
    if (comb === "~") { for (let s = e.previousElementSibling; s; s = s.previousElementSibling) if (complexOk(s, sel, k - 1, scope)) return true; return false; }
    return false;
  }

  function listOk(e, list, scope) {
    return list.some((sel) => complexOk(e, sel, sel.length - 1, scope));
  }

  // :has(relative selector): match `sel` with its leftmost compound
  // related to the anchor element by the selector's leading combinator.
  function relOk(e, sel, k, anchor) {
    if (!compoundOk(e, sel[k].parts, null)) return false;
    if (k === 0) {
      const rel = sel.relative || " ";
      if (rel === ">") return e.parentElement === anchor;
      if (rel === " ") return e !== anchor && anchor.contains(e);
      if (rel === "+") return e.previousElementSibling === anchor;
      for (let s = e.previousElementSibling; s; s = s.previousElementSibling) if (s === anchor) return true;
      return false;
    }
    const comb = sel[k].comb;
    if (comb === ">") { const p = e.parentElement; return !!p && relOk(p, sel, k - 1, anchor); }
    if (comb === " ") { for (let p = e.parentElement; p; p = p.parentElement) if (relOk(p, sel, k - 1, anchor)) return true; return false; }
    if (comb === "+") { const s = e.previousElementSibling; return !!s && relOk(s, sel, k - 1, anchor); }
    for (let s = e.previousElementSibling; s; s = s.previousElementSibling) if (relOk(s, sel, k - 1, anchor)) return true;
    return false;
  }

  function hasOk(e, sel) {
    const rel = sel.relative || " ";
    const scope = rel === " " || rel === ">" ? e : e.parentNode;
    if (!scope) return false;
    return J.descendants(scope).some((c) => c.nodeType === 1 && relOk(c, sel, sel.length - 1, e));
  }

  function parse(s) {
    let l = cache.get(s);
    if (!l) {
      l = parseList(s);
      if (cache.size > 500) cache.clear();
      cache.set(s, l);
    }
    return l;
  }

  J.matches = (e, s, scope) => listOk(e, parse(s), scope || null);
  J.select = function (root, s, first) {
    const list = parse(s);
    const scope = root.nodeType === 1 ? root : null;
    const out = [];
    const walk = (n) => {
      for (const c of n._c) {
        if (c.nodeType === 1) {
          if (listOk(c, list, scope)) { out.push(c); if (first) return true; }
          if (walk(c)) return true;
        } else if (c.nodeType === 11 && walk(c)) return true;
      }
      return false;
    };
    walk(root);
    return out;
  };
  J.parseSelector = parse;
})(globalThis);
