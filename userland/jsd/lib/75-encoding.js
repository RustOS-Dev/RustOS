// Encoding: TextEncoder/TextDecoder (UTF-8, UTF-16, windows-1252),
// atob/btoa and base64 helpers.
"use strict";
(function (G) {
  const J = G.__jsd;

  function utf8Encode(s) {
    s = String(s);
    const out = [];
    for (let i = 0; i < s.length; i++) {
      let c = s.charCodeAt(i);
      if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) {
        const d = s.charCodeAt(i + 1);
        if (d >= 0xdc00 && d <= 0xdfff) { c = 0x10000 + ((c - 0xd800) << 10) + (d - 0xdc00); i++; } else c = 0xfffd;
      } else if (c >= 0xd800 && c <= 0xdfff) c = 0xfffd;
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
      else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
      else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    }
    return new Uint8Array(out);
  }

  // UTF-8 decoding with the WHATWG error handling (maximal subparts
  // become U+FFFD); `st` carries an incomplete sequence across calls.
  function utf8Decode(b, st, fatal, flush) {
    let s = "";
    let need = st.need, cp = st.cp, seen = st.seen, lo = st.lo, hi = st.hi;
    const chunk = [];
    const push = (c) => { chunk.push(c); if (chunk.length > 8192) { s += String.fromCodePoint(...chunk); chunk.length = 0; } };
    const bad = () => { if (fatal) throw new TypeError("The encoded data was not valid for encoding utf-8"); push(0xfffd); };
    for (let i = 0; i < b.length; i++) {
      const x = b[i];
      if (need === 0) {
        if (x < 0x80) push(x);
        else if (x >= 0xc2 && x <= 0xdf) { need = 1; cp = x & 0x1f; }
        else if (x >= 0xe0 && x <= 0xef) { if (x === 0xe0) lo = 0xa0; if (x === 0xed) hi = 0x9f; need = 2; cp = x & 0xf; }
        else if (x >= 0xf0 && x <= 0xf4) { if (x === 0xf0) lo = 0x90; if (x === 0xf4) hi = 0x8f; need = 3; cp = x & 7; }
        else bad();
        continue;
      }
      if (x < lo || x > hi) {
        need = cp = seen = 0; lo = 0x80; hi = 0xbf;
        bad();
        i--;
        continue;
      }
      lo = 0x80; hi = 0xbf;
      cp = (cp << 6) | (x & 0x3f);
      seen++;
      if (seen === need) { push(cp); need = cp = seen = 0; }
    }
    if (flush && need) { need = cp = seen = 0; lo = 0x80; hi = 0xbf; bad(); }
    Object.assign(st, { need, cp, seen, lo, hi });
    if (chunk.length) s += String.fromCodePoint(...chunk);
    return s;
  }

  const W1252 = [0x20ac, 0x81, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030, 0x160, 0x2039, 0x152, 0x8d, 0x17d, 0x8f,
    0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x2dc, 0x2122, 0x161, 0x203a, 0x153, 0x9d, 0x17e, 0x178];
  const LABELS = {
    "utf-8": ["unicode-1-1-utf-8", "unicode11utf8", "unicode20utf8", "utf-8", "utf8", "x-unicode20utf8"],
    "utf-16le": ["csunicode", "iso-10646-ucs-2", "ucs-2", "unicode", "unicodefeff", "utf-16", "utf-16le"],
    "utf-16be": ["unicodefffe", "utf-16be"],
    "windows-1252": ["ansi_x3.4-1968", "ascii", "cp1252", "cp819", "csisolatin1", "ibm819", "iso-8859-1", "iso-ir-100", "iso8859-1", "iso88591", "iso_8859-1", "iso_8859-1:1987", "l1", "latin1", "us-ascii", "windows-1252", "x-cp1252"],
  };
  function encodingOf(label) {
    label = String(label).trim().toLowerCase();
    for (const [name, ls] of Object.entries(LABELS)) if (ls.includes(label)) return name;
    return null;
  }
  J.encodingOf = encodingOf;

  function bytesOf(input) {
    if (input === undefined) return new Uint8Array(0);
    if (input instanceof ArrayBuffer) return new Uint8Array(input);
    if (typeof SharedArrayBuffer !== "undefined" && input instanceof SharedArrayBuffer) return new Uint8Array(input);
    if (ArrayBuffer.isView(input)) return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
    throw new TypeError("The provided value is not of type '(ArrayBuffer or ArrayBufferView)'");
  }
  J.bytesOf = bytesOf;

  class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(s) { return utf8Encode(s === undefined ? "" : s); }
    encodeInto(s, dest) {
      const b = utf8Encode(s);
      // Only whole characters are written.
      let n = Math.min(b.length, dest.length);
      while (n > 0 && n < b.length && (b[n] & 0xc0) === 0x80) n--;
      dest.set(b.subarray(0, n));
      let read = 0;
      const str = String(s);
      for (let i = 0, bytes = 0; i < str.length; i++) {
        const c = str.codePointAt(i);
        const len = c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
        if (bytes + len > n) break;
        bytes += len;
        read += c >= 0x10000 ? 2 : 1;
        if (c >= 0x10000) i++;
      }
      return { read, written: n };
    }
  }

  class TextDecoder {
    constructor(label = "utf-8", opts = {}) {
      const e = encodingOf(label);
      if (!e) throw new RangeError("The encoding label provided ('" + label + "') is invalid.");
      this._e = e;
      this._fatal = !!opts.fatal;
      this._ignoreBOM = !!opts.ignoreBOM;
      this._reset();
    }
    _reset() { this._st = { need: 0, cp: 0, seen: 0, lo: 0x80, hi: 0xbf }; this._bomSeen = false; this._pend = []; }
    get encoding() { return this._e; }
    get fatal() { return this._fatal; }
    get ignoreBOM() { return this._ignoreBOM; }
    decode(input, opts) {
      const stream = !!(opts && opts.stream);
      let b = bytesOf(input);
      let s;
      if (this._e === "utf-8") s = utf8Decode(b, this._st, this._fatal, !stream);
      else if (this._e === "windows-1252") { s = ""; for (const x of b) s += String.fromCharCode(x >= 0x80 && x < 0xa0 ? W1252[x - 0x80] : x); }
      else {
        const all = this._pend.concat(Array.from(b));
        const even = all.length & ~1;
        const be = this._e === "utf-16be";
        const units = [];
        for (let i = 0; i < even; i += 2) units.push(be ? (all[i] << 8) | all[i + 1] : all[i] | (all[i + 1] << 8));
        this._pend = all.slice(even);
        s = String.fromCharCode(...units);
        // Lone surrogates become U+FFFD.
        s = s.replace(/[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/g, (m, off) => (stream && off === s.length - 1 && m >= "\ud800" && m <= "\udbff" ? m : "�"));
        if (!stream && this._pend.length) { if (this._fatal) throw new TypeError("incomplete"); s += "�"; this._pend = []; }
      }
      if (!this._ignoreBOM && !this._bomSeen && s.length) {
        this._bomSeen = true;
        if (s.charCodeAt(0) === 0xfeff) s = s.slice(1);
      }
      if (!stream) this._reset();
      return s;
    }
  }

  // ---- base64 ----
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  J.b64 = function (bytes) {
    let s = "";
    let i = 0;
    for (; i + 2 < bytes.length; i += 3) {
      const n = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2];
      s += B64[n >> 18] + B64[(n >> 12) & 63] + B64[(n >> 6) & 63] + B64[n & 63];
    }
    if (i < bytes.length) {
      const n = (bytes[i] << 16) | ((bytes[i + 1] || 0) << 8);
      s += B64[n >> 18] + B64[(n >> 12) & 63] + (i + 1 < bytes.length ? B64[(n >> 6) & 63] : "=") + "=";
    }
    return s;
  };
  J.unb64 = function (s) {
    s = String(s).replace(/[\t\n\f\r ]/g, "");
    if (s.length % 4 === 0) s = s.replace(/==?$/, "");
    if (s.length % 4 === 1 || /[^A-Za-z0-9+/]/.test(s)) return null;
    const out = [];
    let buf = 0, bits = 0;
    for (const ch of s) {
      buf = (buf << 6) | B64.indexOf(ch);
      bits += 6;
      if (bits >= 8) { bits -= 8; out.push((buf >> bits) & 0xff); }
    }
    return new Uint8Array(out);
  };
  G.btoa = function (s) {
    s = String(s);
    const b = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c > 255) throw new G.DOMException("Failed to execute 'btoa' on 'Window': The string to be encoded contains characters outside of the Latin1 range.", "InvalidCharacterError");
      b[i] = c;
    }
    return J.b64(b);
  };
  G.atob = function (s) {
    const b = J.unb64(s);
    if (!b) throw new G.DOMException("Failed to execute 'atob' on 'Window': The string to be decoded is not correctly encoded.", "InvalidCharacterError");
    let out = "";
    for (let i = 0; i < b.length; i += 8192) out += String.fromCharCode(...b.subarray(i, i + 8192));
    return out;
  };
  J.utf8 = utf8Encode;
  J.fromUtf8 = (b) => utf8Decode(b, { need: 0, cp: 0, seen: 0, lo: 0x80, hi: 0xbf }, false, true);
  Object.assign(G, { TextEncoder, TextDecoder });
})(globalThis);
