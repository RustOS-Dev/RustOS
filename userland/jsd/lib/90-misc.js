// Utilities: crypto (random values, UUIDs, SHA digests), structuredClone,
// a small Intl, CSS namespace, Image/Audio constructors and CSSOM views.
"use strict";
(function (G) {
  const J = G.__jsd;

  // ---- crypto ----
  function getRandomValues(a) {
    if (!ArrayBuffer.isView(a) || a instanceof Float32Array || a instanceof Float64Array || a instanceof DataView) throw new G.DOMException("The provided ArrayBufferView is not an integer array type.", "TypeMismatchError");
    if (a.byteLength > 65536) throw new G.DOMException("The ArrayBufferView's byte length (" + a.byteLength + ") exceeds the number of bytes of entropy available via this API (65536).", "QuotaExceededError");
    const b = new Uint8Array(a.buffer, a.byteOffset, a.byteLength);
    for (let i = 0; i < b.length; i += 4) {
      const r = J.random32();
      for (let k = 0; k < 4 && i + k < b.length; k++) b[i + k] = (r >>> (k * 8)) & 0xff;
    }
    return a;
  }
  function randomUUID() {
    const b = getRandomValues(new Uint8Array(16));
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    const h = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
    return h.slice(0, 8) + "-" + h.slice(8, 12) + "-" + h.slice(12, 16) + "-" + h.slice(16, 20) + "-" + h.slice(20);
  }

  // SHA-1 / SHA-256 on 32-bit words; SHA-384/512 with BigInt.
  function pad(bytes, blk, lenBytes) {
    const n = bytes.length;
    const total = Math.ceil((n + 1 + lenBytes) / blk) * blk;
    const m = new Uint8Array(total);
    m.set(bytes);
    m[n] = 0x80;
    const bits = BigInt(n) * 8n;
    for (let i = 0; i < 8; i++) m[total - 1 - i] = Number((bits >> BigInt(8 * i)) & 0xffn);
    return m;
  }
  function sha1(bytes) {
    const m = pad(bytes, 64, 8);
    let h0 = 0x67452301, h1 = 0xefcdab89, h2 = 0x98badcfe, h3 = 0x10325476, h4 = 0xc3d2e1f0;
    const w = new Int32Array(80);
    const dv = new DataView(m.buffer);
    for (let o = 0; o < m.length; o += 64) {
      for (let i = 0; i < 16; i++) w[i] = dv.getInt32(o + i * 4);
      for (let i = 16; i < 80; i++) { const x = w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]; w[i] = (x << 1) | (x >>> 31); }
      let a = h0, b = h1, c = h2, d = h3, e = h4;
      for (let i = 0; i < 80; i++) {
        const f = i < 20 ? (b & c) | (~b & d) : i < 40 ? b ^ c ^ d : i < 60 ? (b & c) | (b & d) | (c & d) : b ^ c ^ d;
        const k = i < 20 ? 0x5a827999 : i < 40 ? 0x6ed9eba1 : i < 60 ? 0x8f1bbcdc : 0xca62c1d6;
        const t = (((a << 5) | (a >>> 27)) + f + e + k + w[i]) | 0;
        e = d; d = c; c = (b << 30) | (b >>> 2); b = a; a = t;
      }
      h0 = (h0 + a) | 0; h1 = (h1 + b) | 0; h2 = (h2 + c) | 0; h3 = (h3 + d) | 0; h4 = (h4 + e) | 0;
    }
    const out = new DataView(new ArrayBuffer(20));
    [h0, h1, h2, h3, h4].forEach((h, i) => out.setInt32(i * 4, h));
    return out.buffer;
  }
  const K256 = [0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2];
  function sha256(bytes) {
    const m = pad(bytes, 64, 8);
    const h = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    const w = new Int32Array(64);
    const dv = new DataView(m.buffer);
    const r = (x, n) => (x >>> n) | (x << (32 - n));
    for (let o = 0; o < m.length; o += 64) {
      for (let i = 0; i < 16; i++) w[i] = dv.getInt32(o + i * 4);
      for (let i = 16; i < 64; i++) {
        const s0 = r(w[i - 15], 7) ^ r(w[i - 15], 18) ^ (w[i - 15] >>> 3);
        const s1 = r(w[i - 2], 17) ^ r(w[i - 2], 19) ^ (w[i - 2] >>> 10);
        w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
      }
      let [a, b, c, d, e, f, g, hh] = h;
      for (let i = 0; i < 64; i++) {
        const t1 = (hh + (r(e, 6) ^ r(e, 11) ^ r(e, 25)) + ((e & f) ^ (~e & g)) + K256[i] + w[i]) | 0;
        const t2 = ((r(a, 2) ^ r(a, 13) ^ r(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) | 0;
        hh = g; g = f; f = e; e = (d + t1) | 0; d = c; c = b; b = a; a = (t1 + t2) | 0;
      }
      [a, b, c, d, e, f, g, hh].forEach((x, i) => { h[i] = (h[i] + x) | 0; });
    }
    const out = new DataView(new ArrayBuffer(32));
    h.forEach((x, i) => out.setInt32(i * 4, x));
    return out.buffer;
  }
  let K512 = null;
  function sha512(bytes, is384) {
    if (!K512) {
      K512 = ["428a2f98d728ae22", "7137449123ef65cd", "b5c0fbcfec4d3b2f", "e9b5dba58189dbbc", "3956c25bf348b538", "59f111f1b605d019", "923f82a4af194f9b", "ab1c5ed5da6d8118",
        "d807aa98a3030242", "12835b0145706fbe", "243185be4ee4b28c", "550c7dc3d5ffb4e2", "72be5d74f27b896f", "80deb1fe3b1696b1", "9bdc06a725c71235", "c19bf174cf692694",
        "e49b69c19ef14ad2", "efbe4786384f25e3", "0fc19dc68b8cd5b5", "240ca1cc77ac9c65", "2de92c6f592b0275", "4a7484aa6ea6e483", "5cb0a9dcbd41fbd4", "76f988da831153b5",
        "983e5152ee66dfab", "a831c66d2db43210", "b00327c898fb213f", "bf597fc7beef0ee4", "c6e00bf33da88fc2", "d5a79147930aa725", "06ca6351e003826f", "142929670a0e6e70",
        "27b70a8546d22ffc", "2e1b21385c26c926", "4d2c6dfc5ac42aed", "53380d139d95b3df", "650a73548baf63de", "766a0abb3c77b2a8", "81c2c92e47edaee6", "92722c851482353b",
        "a2bfe8a14cf10364", "a81a664bbc423001", "c24b8b70d0f89791", "c76c51a30654be30", "d192e819d6ef5218", "d69906245565a910", "f40e35855771202a", "106aa07032bbd1b8",
        "19a4c116b8d2d0c8", "1e376c085141ab53", "2748774cdf8eeb99", "34b0bcb5e19b48a8", "391c0cb3c5c95a63", "4ed8aa4ae3418acb", "5b9cca4f7763e373", "682e6ff3d6b2b8a3",
        "748f82ee5defb2fc", "78a5636f43172f60", "84c87814a1f0ab72", "8cc702081a6439ec", "90befffa23631e28", "a4506cebde82bde9", "bef9a3f7b2c67915", "c67178f2e372532b",
        "ca273eceea26619c", "d186b8c721c0c207", "eada7dd6cde0eb1e", "f57d4f7fee6ed178", "06f067aa72176fba", "0a637dc5a2c898a6", "113f9804bef90dae", "1b710b35131c471b",
        "28db77f523047d84", "32caab7b40c72493", "3c9ebe0a15c9bebc", "431d67c49c100d4c", "4cc5d4becb3e42b6", "597f299cfc657e2a", "5fcb6fab3ad6faec", "6c44198c4a475817"].map((x) => BigInt("0x" + x));
    }
    const M = (1n << 64n) - 1n;
    const m = pad(bytes, 128, 16);
    const h = (is384
      ? ["cbbb9d5dc1059ed8", "629a292a367cd507", "9159015a3070dd17", "152fecd8f70e5939", "67332667ffc00b31", "8eb44a8768581511", "db0c2e0d64f98fa7", "47b5481dbefa4fa4"]
      : ["6a09e667f3bcc908", "bb67ae8584caa73b", "3c6ef372fe94f82b", "a54ff53a5f1d36f1", "510e527fade682d1", "9b05688c2b3e6c1f", "1f83d9abfb41bd6b", "5be0cd19137e2179"]).map((x) => BigInt("0x" + x));
    const r = (x, n) => ((x >> n) | (x << (64n - n))) & M;
    const dv = new DataView(m.buffer);
    const w = new Array(80);
    for (let o = 0; o < m.length; o += 128) {
      for (let i = 0; i < 16; i++) w[i] = dv.getBigUint64(o + i * 8);
      for (let i = 16; i < 80; i++) {
        const s0 = r(w[i - 15], 1n) ^ r(w[i - 15], 8n) ^ (w[i - 15] >> 7n);
        const s1 = r(w[i - 2], 19n) ^ r(w[i - 2], 61n) ^ (w[i - 2] >> 6n);
        w[i] = (w[i - 16] + s0 + w[i - 7] + s1) & M;
      }
      let [a, b, c, d, e, f, g, hh] = h;
      for (let i = 0; i < 80; i++) {
        const t1 = (hh + (r(e, 14n) ^ r(e, 18n) ^ r(e, 41n)) + ((e & f) ^ (~e & M & g)) + K512[i] + w[i]) & M;
        const t2 = ((r(a, 28n) ^ r(a, 34n) ^ r(a, 39n)) + ((a & b) ^ (a & c) ^ (b & c))) & M;
        hh = g; g = f; f = e; e = (d + t1) & M; d = c; c = b; b = a; a = (t1 + t2) & M;
      }
      [a, b, c, d, e, f, g, hh].forEach((x, i) => { h[i] = (h[i] + x) & M; });
    }
    const out = new DataView(new ArrayBuffer(64));
    h.forEach((x, i) => out.setBigUint64(i * 8, x));
    return is384 ? out.buffer.slice(0, 48) : out.buffer;
  }
  J.sha256 = (b) => new Uint8Array(sha256(b));
  function hmac(hash, blk, key, msg) {
    if (key.length > blk) key = new Uint8Array(hash(key));
    const k = new Uint8Array(blk); k.set(key);
    const ip = k.map((x) => x ^ 0x36), op = k.map((x) => x ^ 0x5c);
    const inner = new Uint8Array(hash(J.concatBytes([ip, msg])));
    return hash(J.concatBytes([op, inner]));
  }
  const HASH = { "SHA-1": [sha1, 64], "SHA-256": [sha256, 64], "SHA-384": [(b) => sha512(b, true), 128], "SHA-512": [(b) => sha512(b, false), 128] };
  const hashName = (a) => { const n = (typeof a === "string" ? a : a && a.name || "").toUpperCase(); if (!HASH[n]) throw new G.DOMException("Algorithm: Unrecognized name", "NotSupportedError"); return n; };
  class CryptoKey { constructor(alg, usages, raw) { this.type = "secret"; this.extractable = false; this.algorithm = alg; this.usages = usages; this._raw = raw; } }
  const subtle = {
    digest(alg, data) {
      try { const n = hashName(alg); return Promise.resolve(HASH[n][0](J.bytesOf(data).slice())); } catch (e) { return Promise.reject(e); }
    },
    importKey(fmt, key, alg, ext, usages) {
      try {
        if (fmt !== "raw" || !alg || String(alg.name).toUpperCase() !== "HMAC") throw new G.DOMException("Only raw HMAC keys are supported", "NotSupportedError");
        return Promise.resolve(new CryptoKey({ name: "HMAC", hash: { name: hashName(alg.hash) } }, usages, J.bytesOf(key).slice()));
      } catch (e) { return Promise.reject(e); }
    },
    sign(alg, key, data) {
      try { const h = HASH[key.algorithm.hash.name]; return Promise.resolve(hmac(h[0], h[1], key._raw, J.bytesOf(data).slice())); } catch (e) { return Promise.reject(e); }
    },
    verify(alg, key, sig, data) {
      return subtle.sign(alg, key, data).then((s) => { const a = new Uint8Array(s), b = J.bytesOf(sig); return a.length === b.length && a.every((x, i) => x === b[i]); });
    },
  };
  G.crypto = { getRandomValues, randomUUID, subtle };
  G.Crypto = function Crypto() { throw new TypeError("Illegal constructor"); };
  G.CryptoKey = CryptoKey;
  G.SubtleCrypto = function SubtleCrypto() { throw new TypeError("Illegal constructor"); };

  // ---- structuredClone ----
  G.structuredClone = function (v, opts) {
    const seen = new Map();
    void opts;
    const clone = (x) => {
      if (x === null || (typeof x !== "object" && typeof x !== "function")) {
        if (typeof x === "symbol") throw new G.DOMException(String(x) + " could not be cloned.", "DataCloneError");
        return x;
      }
      if (typeof x === "function") throw new G.DOMException("function could not be cloned.", "DataCloneError");
      if (seen.has(x)) return seen.get(x);
      let out;
      if (x instanceof Date) out = new Date(x.getTime());
      else if (x instanceof RegExp) out = new RegExp(x.source, x.flags);
      else if (x instanceof ArrayBuffer) out = x.slice(0);
      else if (ArrayBuffer.isView(x)) {
        const buf = clone(x.buffer);
        out = x instanceof DataView ? new DataView(buf, x.byteOffset, x.byteLength) : new x.constructor(buf, x.byteOffset, x.length);
      } else if (x instanceof Map) { out = new Map(); seen.set(x, out); for (const [k, val] of x) out.set(clone(k), clone(val)); return out; }
      else if (x instanceof Set) { out = new Set(); seen.set(x, out); for (const k of x) out.add(clone(k)); return out; }
      else if (G.File && x instanceof G.File) out = new G.File([x], x.name, { type: x.type, lastModified: x.lastModified });
      else if (G.Blob && x instanceof G.Blob) out = x.slice(0, x.size, x.type);
      else if (x instanceof Error) { out = new (G[x.name] && G[x.name].prototype instanceof Error ? G[x.name] : Error)(x.message); }
      else if (x instanceof Boolean || x instanceof Number || x instanceof String) out = Object(x.valueOf());
      else if (x instanceof G.Node || x instanceof Promise || x instanceof WeakMap || x instanceof WeakSet || (G.EventTarget && x instanceof G.EventTarget)) {
        throw new G.DOMException((x.constructor && x.constructor.name || "object") + " object could not be cloned.", "DataCloneError");
      } else if (Array.isArray(x)) {
        out = new Array(x.length);
        seen.set(x, out);
        for (const k of Object.keys(x)) out[k] = clone(x[k]);
        return out;
      } else {
        out = {};
        seen.set(x, out);
        for (const k of Object.keys(x)) out[k] = clone(x[k]);
        return out;
      }
      seen.set(x, out);
      return out;
    };
    return clone(v);
  };

  // ---- Intl (English only, when the engine has none) ----
  if (typeof G.Intl === "undefined") {
    const group = (s) => s.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
    class NumberFormat {
      constructor(loc, o) { this._o = Object.assign({ style: "decimal", useGrouping: true }, o || {}); }
      format(n) {
        const o = this._o;
        n = Number(n);
        if (!Number.isFinite(n)) return Number.isNaN(n) ? "NaN" : (n < 0 ? "-∞" : "∞");
        if (o.style === "percent") n *= 100;
        let min = o.minimumFractionDigits, max = o.maximumFractionDigits;
        if (o.style === "currency") { min = min ?? 2; max = max ?? Math.max(2, min); }
        min = min ?? 0; max = max ?? Math.max(min, o.style === "percent" ? 0 : 3);
        let s = Math.abs(n).toFixed(max);
        if (s.includes(".")) { s = s.replace(/0+$/, ""); const f = s.split(".")[1] || ""; if (f.length < min) s += "0".repeat(min - f.length); if (s.endsWith(".")) s = s.slice(0, -1); }
        if (min > 0 && !s.includes(".")) s += "." + "0".repeat(min);
        let [i, f] = s.split(".");
        if (o.useGrouping !== false) i = group(i);
        s = f ? i + "." + f : i;
        const sign = n < 0 ? "-" : "";
        if (o.style === "percent") return sign + s + "%";
        if (o.style === "currency") { const sym = { USD: "$", EUR: "€", GBP: "£", JPY: "¥" }[o.currency] || (o.currency + " "); return sign + sym + s; }
        return sign + s;
      }
      formatToParts(n) { return [{ type: "literal", value: this.format(n) }]; }
      resolvedOptions() { return Object.assign({ locale: "en-US", numberingSystem: "latn" }, this._o); }
      static supportedLocalesOf() { return ["en-US"]; }
    }
    const MON = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    const DAY = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    class DateTimeFormat {
      constructor(loc, o) { this._o = o || {}; }
      format(d) {
        d = d === undefined ? new Date() : new Date(d);
        const o = this._o;
        const utc = o.timeZone === "UTC";
        const g = (k) => d[(utc ? "getUTC" : "get") + k]();
        const hasDate = o.year || o.month || o.day || o.weekday || o.dateStyle;
        const hasTime = o.hour || o.minute || o.second || o.timeStyle;
        const parts = [];
        if (hasDate || !hasTime) {
          let s;
          if (o.month === "long" || o.month === "short" || o.dateStyle === "long" || o.dateStyle === "medium") {
            const mn = MON[g("Month")];
            s = (o.month === "short" || o.dateStyle === "medium" ? mn.slice(0, 3) : mn) + " " + g("Date") + ", " + g("FullYear");
          } else s = (g("Month") + 1) + "/" + g("Date") + "/" + g("FullYear");
          if (o.weekday) s = (o.weekday === "long" ? DAY[g("Day")] : DAY[g("Day")].slice(0, 3)) + ", " + s;
          parts.push(s);
        }
        if (hasTime) {
          const h = g("Hours");
          const h12 = o.hour12 !== false && o.hourCycle !== "h23";
          let s = (h12 ? (h % 12 || 12) : String(h).padStart(2, "0")) + ":" + String(g("Minutes")).padStart(2, "0");
          if (o.second || o.timeStyle === "medium" || o.timeStyle === "long") s += ":" + String(g("Seconds")).padStart(2, "0");
          if (h12) s += h < 12 ? " AM" : " PM";
          parts.push(s);
        }
        return parts.join(", ");
      }
      formatToParts(d) { return [{ type: "literal", value: this.format(d) }]; }
      formatRange(a, b) { return this.format(a) + " – " + this.format(b); }
      resolvedOptions() { return Object.assign({ locale: "en-US", calendar: "gregory", numberingSystem: "latn", timeZone: "UTC" }, this._o); }
      static supportedLocalesOf() { return ["en-US"]; }
    }
    class Collator {
      constructor(loc, o) { this._o = o || {}; }
      compare(a, b) {
        a = String(a); b = String(b);
        if (this._o.sensitivity === "base" || this._o.sensitivity === "accent") { a = a.toLowerCase(); b = b.toLowerCase(); }
        if (this._o.numeric) { const na = parseFloat(a), nb = parseFloat(b); if (!isNaN(na) && !isNaN(nb) && na !== nb) return na < nb ? -1 : 1; }
        return a < b ? -1 : a > b ? 1 : 0;
      }
      resolvedOptions() { return { locale: "en-US" }; }
    }
    class PluralRules {
      constructor(loc, o) { this._o = o || {}; }
      select(n) {
        if (this._o.type === "ordinal") { const t = n % 10, h = n % 100; return t === 1 && h !== 11 ? "one" : t === 2 && h !== 12 ? "two" : t === 3 && h !== 13 ? "few" : "other"; }
        return n === 1 ? "one" : "other";
      }
      resolvedOptions() { return { locale: "en-US" }; }
    }
    class RelativeTimeFormat {
      constructor(loc, o) { this._o = o || {}; }
      format(v, unit) {
        unit = String(unit).replace(/s$/, "");
        if (this._o.numeric === "auto" && unit === "day" && Math.abs(v) <= 1) return v === 0 ? "today" : v > 0 ? "tomorrow" : "yesterday";
        const n = Math.abs(v), u = unit + (n === 1 ? "" : "s");
        return v < 0 || Object.is(v, -0) ? n + " " + u + " ago" : "in " + n + " " + u;
      }
    }
    class ListFormat {
      constructor(loc, o) { this._o = o || {}; }
      format(l) {
        l = Array.from(l);
        const w = this._o.type === "disjunction" ? "or" : "and";
        if (l.length < 3) return l.join(" " + w + " ");
        return l.slice(0, -1).join(", ") + ", " + w + " " + l[l.length - 1];
      }
    }
    class Segmenter {
      segment(s) { return Array.from(String(s), (c, i) => ({ segment: c, index: i, input: s })); }
    }
    G.Intl = { NumberFormat, DateTimeFormat, Collator, PluralRules, RelativeTimeFormat, ListFormat, Segmenter, getCanonicalLocales: () => ["en-US"], supportedValuesOf: () => [] };
    const nf = new NumberFormat();
    Number.prototype.toLocaleString = function (loc, o) { return (o ? new NumberFormat(loc, o) : nf).format(this.valueOf()); };
    Date.prototype.toLocaleDateString = function (loc, o) { return new DateTimeFormat(loc, Object.assign({ year: "numeric" }, o)).format(this); };
    Date.prototype.toLocaleTimeString = function (loc, o) { return new DateTimeFormat(loc, Object.assign({ hour: "numeric", second: "numeric" }, o)).format(this); };
    Date.prototype.toLocaleString = function (loc, o) { return new DateTimeFormat(loc, o || { year: "numeric", hour: "numeric", second: "numeric" }).format(this); };
    String.prototype.localeCompare = function (b, loc, o) { return new Collator(loc, o).compare(String(this), b); };
  }

  // ---- CSS namespace ----
  G.CSS = {
    escape(s) {
      s = String(s);
      let out = "";
      for (let i = 0; i < s.length; i++) {
        const c = s.charCodeAt(i), ch = s[i];
        if (c === 0) out += "�";
        else if ((c >= 1 && c <= 0x1f) || c === 0x7f || (i === 0 && c >= 0x30 && c <= 0x39) || (i === 1 && c >= 0x30 && c <= 0x39 && s[0] === "-")) out += "\\" + c.toString(16) + " ";
        else if (i === 0 && ch === "-" && s.length === 1) out += "\\-";
        else if (c >= 0x80 || ch === "-" || ch === "_" || /[0-9A-Za-z]/.test(ch)) out += ch;
        else out += "\\" + ch;
      }
      return out;
    },
    supports(a, b) {
      const q = b === undefined ? String(a) : "(" + a + ": " + b + ")";
      try { return !!J.rpc("cssSupports", { q }); } catch (e) { return false; }
    },
    registerProperty() {},
    px: (n) => n + "px",
  };

  // ---- Image / Audio constructors ----
  G.Image = function Image(w, h) {
    const i = G.document.createElement("img");
    if (w !== undefined) i.width = w;
    if (h !== undefined) i.height = h;
    return i;
  };
  G.Image.prototype = G.HTMLImageElement.prototype;
  G.Audio = function Audio(src) {
    const a = G.document.createElement("audio");
    a.preload = "auto";
    if (src !== undefined) a.src = src;
    return a;
  };
  G.Audio.prototype = G.HTMLAudioElement.prototype;

  // ---- CSSOM: read-only style sheets ----
  class CSSRule { constructor(text) { this.cssText = text; this.type = 1; } }
  class CSSStyleSheet {
    constructor(owner, text) { this.ownerNode = owner || null; this._text = text || ""; this.disabled = false; this.href = owner && owner.href || null; this.media = { mediaText: owner && owner.getAttribute("media") || "" }; this.type = "text/css"; this.title = null; }
    get cssRules() {
      if (!this._rules) this._rules = (J.rpc("cssRules", { text: this._text }) || []).map((t) => new CSSRule(t));
      return this._rules;
    }
    get rules() { return this.cssRules; }
    insertRule(r, i) { const l = this.cssRules; i = i === undefined ? 0 : i; l.splice(i, 0, new CSSRule(r)); this._apply(); return i; }
    deleteRule(i) { this.cssRules.splice(i, 1); this._apply(); }
    addRule(sel, st, i) { return this.insertRule(sel + " { " + st + " }", i === undefined ? this.cssRules.length : i); }
    removeRule(i) { this.deleteRule(i); }
    replaceSync(t) { this._text = String(t); this._rules = null; this._apply(); }
    replace(t) { this.replaceSync(t); return Promise.resolve(this); }
    _apply() {
      // Rules changed by script: rewrite the <style> contents.
      if (this.ownerNode && this.ownerNode._tag === "style") {
        this.ownerNode._sheet = this;
        this.ownerNode.textContent = this._rules.map((r) => r.cssText).join("\n");
      }
    }
  }
  J.styleSheetFor = function (el) {
    if (el._tag === "style") return el._sheet || (el._sheet = new CSSStyleSheet(el, el.textContent));
    if (el._tag === "link" && /\bstylesheet\b/i.test(el.rel)) return el._sheet || (el._sheet = new CSSStyleSheet(el, ""));
    return null;
  };
  J.styleSheets = function (doc) {
    return J.descendants(doc).filter((e) => e.nodeType === 1 && (e._tag === "style" || (e._tag === "link" && /\bstylesheet\b/i.test(e.getAttribute("rel") || "")))).map(J.styleSheetFor);
  };
  G.CSSStyleSheet = CSSStyleSheet;
  G.CSSRule = CSSRule;
  G.StyleSheet = CSSStyleSheet;
})(globalThis);
