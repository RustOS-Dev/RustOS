// URL and URLSearchParams: the WHATWG URL parser (basic URL parser state
// machine, host parsing with IPv4/IPv6 and punycode) in JavaScript.
"use strict";
(function (G) {
  const J = G.__jsd;

  const SPECIAL = { "ftp:": 21, "file:": null, "http:": 80, "https:": 443, "ws:": 80, "wss:": 443 };
  const isSpecialScheme = (s) => Object.prototype.hasOwnProperty.call(SPECIAL, s + ":");

  // Percent-encode sets.
  const C0 = (c) => c < 0x20 || c > 0x7e;
  const FRAGMENT = (c) => C0(c) || c === 0x20 || c === 0x22 || c === 0x3c || c === 0x3e || c === 0x60;
  const QUERY = (c) => C0(c) || c === 0x20 || c === 0x22 || c === 0x23 || c === 0x3c || c === 0x3e;
  const SPECIAL_QUERY = (c) => QUERY(c) || c === 0x27;
  const PATH = (c) => QUERY(c) || c === 0x3f || c === 0x5e || c === 0x60 || c === 0x7b || c === 0x7d;
  const USERINFO = (c) => PATH(c) || c === 0x2f || c === 0x3a || c === 0x3b || c === 0x3d || c === 0x40 || (c >= 0x5b && c <= 0x5d) || c === 0x7c;
  const COMPONENT = (c) => USERINFO(c) || (c >= 0x24 && c <= 0x26) || c === 0x2b || c === 0x2c;
  const FORM = (c) => COMPONENT(c) || c === 0x21 || (c >= 0x27 && c <= 0x29) || c === 0x7e;

  const enc = new G.TextEncoder();
  function pct(cp, set) {
    const s = String.fromCodePoint(cp);
    if (cp < 0x80 && !set(cp)) return s;
    let out = "";
    for (const b of enc.encode(s)) out += (!set(b) && b < 0x80) ? String.fromCharCode(b) : "%" + b.toString(16).toUpperCase().padStart(2, "0");
    return out;
  }
  function pctString(s, set, spaceAsPlus) {
    let out = "";
    for (const ch of s) {
      const cp = ch.codePointAt(0);
      if (spaceAsPlus && cp === 0x20) out += "+";
      else out += pct(cp >= 0xd800 && cp <= 0xdfff ? 0xfffd : cp, set);
    }
    return out;
  }
  function pctDecodeBytes(s) {
    const b = enc.encode(s);
    const out = [];
    for (let i = 0; i < b.length; i++) {
      if (b[i] === 0x25 && i + 2 < b.length && /^[0-9a-fA-F]{2}$/.test(String.fromCharCode(b[i + 1], b[i + 2]))) {
        out.push(parseInt(String.fromCharCode(b[i + 1], b[i + 2]), 16));
        i += 2;
      } else out.push(b[i]);
    }
    return new Uint8Array(out);
  }
  const dec = () => new G.TextDecoder();

  // ---- hosts ----
  function parseIPv6(input) {
    const a = [0, 0, 0, 0, 0, 0, 0, 0];
    let piece = 0, compress = null, p = 0;
    const cps = Array.from(input).map((c) => c.codePointAt(0));
    const c = () => cps[p];
    const hex = (x) => x !== undefined && /[0-9a-fA-F]/.test(String.fromCodePoint(x));
    if (c() === 0x3a) {
      if (cps[p + 1] !== 0x3a) return null;
      p += 2; piece++; compress = piece;
    }
    while (c() !== undefined) {
      if (piece === 8) return null;
      if (c() === 0x3a) {
        if (compress !== null) return null;
        p++; piece++; compress = piece; continue;
      }
      let value = 0, len = 0;
      while (len < 4 && hex(c())) { value = value * 16 + parseInt(String.fromCodePoint(c()), 16); p++; len++; }
      if (c() === 0x2e) {
        if (len === 0) return null;
        p -= len;
        if (piece > 6) return null;
        let seen = 0;
        while (c() !== undefined) {
          let v4 = null;
          if (seen > 0) { if (c() === 0x2e && seen < 4) p++; else return null; }
          if (c() === undefined || c() < 0x30 || c() > 0x39) return null;
          while (c() >= 0x30 && c() <= 0x39) {
            const n = c() - 0x30;
            if (v4 === null) v4 = n; else if (v4 === 0) return null; else v4 = v4 * 10 + n;
            if (v4 > 255) return null;
            p++;
          }
          a[piece] = a[piece] * 0x100 + v4;
          seen++;
          if (seen === 2 || seen === 4) piece++;
        }
        if (seen !== 4) return null;
        break;
      } else if (c() === 0x3a) {
        p++;
        if (c() === undefined) return null;
      } else if (c() !== undefined) return null;
      a[piece] = value;
      piece++;
    }
    if (compress !== null) {
      let swaps = piece - compress;
      piece = 7;
      while (piece !== 0 && swaps > 0) {
        const t = a[compress + swaps - 1];
        a[compress + swaps - 1] = a[piece];
        a[piece] = t;
        piece--; swaps--;
      }
    } else if (piece !== 8) return null;
    return a;
  }
  function serializeIPv6(a) {
    // Compress the longest run (length > 1) of zeros.
    let best = -1, bestLen = 1;
    for (let i = 0; i < 8;) {
      if (a[i] !== 0) { i++; continue; }
      let j = i;
      while (j < 8 && a[j] === 0) j++;
      if (j - i > bestLen) { best = i; bestLen = j - i; }
      i = j;
    }
    let out = "", ignore0 = false;
    for (let i = 0; i < 8; i++) {
      if (ignore0 && a[i] === 0) continue;
      ignore0 = false;
      if (best === i) { out += i === 0 ? "::" : ":"; ignore0 = true; continue; }
      out += a[i].toString(16);
      if (i !== 7) out += ":";
    }
    return "[" + out + "]";
  }
  function parseIPv4Number(s) {
    if (s === "") return null;
    let radix = 10;
    if (/^0[xX]/.test(s)) { s = s.slice(2); radix = 16; } else if (s.length > 1 && s[0] === "0") { s = s.slice(1); radix = 8; }
    if (s === "") return 0;
    const re = radix === 10 ? /^[0-9]+$/ : radix === 16 ? /^[0-9a-fA-F]+$/ : /^[0-7]+$/;
    if (!re.test(s)) return null;
    return parseInt(s, radix);
  }
  function endsInNumber(host) {
    const parts = host.split(".");
    if (parts[parts.length - 1] === "") { if (parts.length === 1) return false; parts.pop(); }
    const last = parts[parts.length - 1];
    if (last !== "" && /^[0-9]+$/.test(last)) return true;
    return parseIPv4Number(last) !== null;
  }
  function parseIPv4(host) {
    const parts = host.split(".");
    if (parts[parts.length - 1] === "" && parts.length > 1) parts.pop();
    if (parts.length > 4) return null;
    const nums = [];
    for (const p of parts) { const n = parseIPv4Number(p); if (n === null) return null; nums.push(n); }
    for (let i = 0; i < nums.length - 1; i++) if (nums[i] > 255) return null;
    if (nums[nums.length - 1] >= 256 ** (5 - nums.length)) return null;
    let ip = nums[nums.length - 1];
    for (let i = 0; i < nums.length - 1; i++) ip += nums[i] * 256 ** (3 - i);
    return ip;
  }
  const serializeIPv4 = (n) => [24, 16, 8, 0].map((s) => Math.floor(n / 2 ** s) % 256).join(".");

  // Punycode (RFC 3492) encoding of one label.
  function punycode(label) {
    const cps = Array.from(label).map((c) => c.codePointAt(0));
    let out = cps.filter((c) => c < 0x80).map((c) => String.fromCharCode(c)).join("");
    const b = out.length;
    let h = b;
    if (b > 0) out += "-";
    let n = 128, delta = 0, bias = 72;
    const digit = (d) => String.fromCharCode(d + 22 + 75 * (d < 26));
    const adapt = (d, num, first) => {
      d = first ? Math.floor(d / 700) : d >> 1;
      d += Math.floor(d / num);
      let k = 0;
      while (d > 455) { d = Math.floor(d / 35); k += 36; }
      return k + Math.floor(36 * d / (d + 38));
    };
    while (h < cps.length) {
      const m = Math.min(...cps.filter((c) => c >= n));
      delta += (m - n) * (h + 1);
      n = m;
      for (const c of cps) {
        if (c < n) delta++;
        if (c === n) {
          let q = delta;
          for (let k = 36; ; k += 36) {
            const t = k <= bias ? 1 : k >= bias + 26 ? 26 : k - bias;
            if (q < t) break;
            out += digit(t + (q - t) % (36 - t));
            q = Math.floor((q - t) / (36 - t));
          }
          out += digit(q);
          bias = adapt(delta, h + 1, h === b);
          delta = 0;
          h++;
        }
      }
      delta++; n++;
    }
    return out;
  }
  function domainToASCII(d) {
    // UTS 46 processing, reduced: case fold, map full stops, then punycode.
    d = d.normalize("NFC").toLowerCase().replace(/[。．｡]/g, ".").replace(/[­​⁠﻿͏᠋-᠍︀-️]/g, "");
    const labels = d.split(".").map((l) => (/[^\x00-\x7f]/.test(l) ? "xn--" + punycode(l) : l));
    for (const l of labels) if (l.length > 63 && /[^\x00-\x7f]/.test(d)) return null;
    return labels.join(".");
  }
  const FORBIDDEN_HOST = /[\x00\t\n\r #/:<>?@[\\\]^|]/;
  const FORBIDDEN_DOMAIN = /[\x00-\x1f\t\n\r #%/:<>?@[\\\]^|\x7f]/;
  function parseHost(input, notSpecial) {
    if (input.startsWith("[")) {
      if (!input.endsWith("]")) return null;
      const a = parseIPv6(input.slice(1, -1));
      return a ? serializeIPv6(a) : null;
    }
    if (notSpecial) {
      if (FORBIDDEN_HOST.test(input)) return null;
      return pctString(input, C0);
    }
    let domain;
    try { domain = dec().decode(pctDecodeBytes(input)); } catch (e) { return null; }
    const ascii = domainToASCII(domain);
    if (ascii === null || ascii === "" || FORBIDDEN_DOMAIN.test(ascii)) return null;
    if (endsInNumber(ascii)) {
      const v4 = parseIPv4(ascii);
      return v4 === null ? null : serializeIPv4(v4);
    }
    return ascii;
  }

  // ---- the basic URL parser ----
  const ALPHA = /[A-Za-z]/, ALNUMPLUS = /[A-Za-z0-9+\-.]/;
  const isWinLetter = (s, normalizedOnly) => s.length === 2 && ALPHA.test(s[0]) && (s[1] === ":" || (!normalizedOnly && s[1] === "|"));
  const startsWinLetter = (cps, p) => cps.length - p >= 2 && ALPHA.test(cps[p]) && (cps[p + 1] === ":" || cps[p + 1] === "|") && (cps.length - p === 2 || "/\\?#".includes(cps[p + 2]));
  function shortenPath(u) {
    if (u.scheme === "file" && u.path.length === 1 && isWinLetter(u.path[0], true)) return;
    u.path.pop();
  }
  const isSingleDot = (s) => s === "." || s.toLowerCase() === "%2e";
  const isDoubleDot = (s) => /^(\.|%2e)(\.|%2e)$/i.test(s);

  function parse(input, base, url, stateOverride) {
    if (!url) {
      url = { scheme: "", username: "", password: "", host: null, port: null, path: [], opaque: null, query: null, fragment: null };
      input = input.replace(/^[\x00-\x20]+|[\x00-\x20]+$/g, "");
    }
    input = input.replace(/[\t\n\r]/g, "");
    const cps = Array.from(input);
    let state = stateOverride || "scheme start";
    let buf = "", atSeen = false, insideBrackets = false, passwordSeen = false;
    const special = () => isSpecialScheme(url.scheme);
    for (let p = 0; p <= cps.length; p++) {
      const c = cps[p]; // undefined = EOF
      switch (state) {
        case "scheme start":
          if (c !== undefined && ALPHA.test(c)) { buf += c.toLowerCase(); state = "scheme"; }
          else if (!stateOverride) { state = "no scheme"; p--; }
          else return null;
          break;
        case "scheme":
          if (c !== undefined && ALNUMPLUS.test(c)) buf += c.toLowerCase();
          else if (c === ":") {
            if (stateOverride) {
              if (isSpecialScheme(url.scheme) !== isSpecialScheme(buf)) return url;
              if ((url.username || url.password || url.port !== null) && buf === "file") return url;
              if (url.scheme === "file" && url.host === "") return url;
            }
            url.scheme = buf;
            if (stateOverride) {
              if (url.port === SPECIAL[url.scheme + ":"]) url.port = null;
              return url;
            }
            buf = "";
            if (url.scheme === "file") state = "file";
            else if (special() && base && base.scheme === url.scheme) state = "special relative or authority";
            else if (special()) state = "special authority slashes";
            else if (cps[p + 1] === "/") { state = "path or authority"; p++; }
            else { url.opaque = ""; state = "opaque path"; }
          } else if (!stateOverride) { buf = ""; state = "no scheme"; p = -1; }
          else return null;
          break;
        case "no scheme":
          if (!base || (base.opaque !== null && c !== "#")) return null;
          if (base.opaque !== null && c === "#") {
            url.scheme = base.scheme; url.path = base.path.slice(); url.opaque = base.opaque; url.query = base.query; url.fragment = ""; state = "fragment";
          } else if (base.scheme !== "file") { state = "relative"; p--; }
          else { state = "file"; p--; }
          break;
        case "special relative or authority":
          if (c === "/" && cps[p + 1] === "/") { state = "special authority ignore slashes"; p++; }
          else { state = "relative"; p--; }
          break;
        case "path or authority":
          if (c === "/") state = "authority"; else { state = "path"; p--; }
          break;
        case "relative":
          url.scheme = base.scheme;
          if (c === "/") state = "relative slash";
          else if (special() && c === "\\") state = "relative slash";
          else {
            url.username = base.username; url.password = base.password; url.host = base.host; url.port = base.port; url.path = base.path.slice(); url.query = base.query;
            if (c === "?") { url.query = ""; state = "query"; }
            else if (c === "#") { url.fragment = ""; state = "fragment"; }
            else if (c !== undefined) { url.query = null; shortenPath(url); state = "path"; p--; }
          }
          break;
        case "relative slash":
          if (special() && (c === "/" || c === "\\")) state = "special authority ignore slashes";
          else if (c === "/") state = "authority";
          else { url.username = base.username; url.password = base.password; url.host = base.host; url.port = base.port; state = "path"; p--; }
          break;
        case "special authority slashes":
          if (c === "/" && cps[p + 1] === "/") { state = "special authority ignore slashes"; p++; }
          else { state = "special authority ignore slashes"; p--; }
          break;
        case "special authority ignore slashes":
          if (c !== "/" && c !== "\\") { state = "authority"; p--; }
          break;
        case "authority":
          if (c === "@") {
            if (atSeen) buf = "%40" + buf;
            atSeen = true;
            for (const ch of buf) {
              if (ch === ":" && !passwordSeen) { passwordSeen = true; continue; }
              const e = pct(ch.codePointAt(0), USERINFO);
              if (passwordSeen) url.password += e; else url.username += e;
            }
            buf = "";
          } else if (c === undefined || c === "/" || c === "?" || c === "#" || (special() && c === "\\")) {
            if (atSeen && buf === "") return null;
            p -= Array.from(buf).length + 1;
            buf = "";
            state = "host";
          } else buf += c;
          break;
        case "host":
        case "hostname":
          if (stateOverride && url.scheme === "file") { p--; state = "file host"; }
          else if (c === ":" && !insideBrackets) {
            if (buf === "") return null;
            if (stateOverride === "hostname") return null;
            const h = parseHost(buf, !special());
            if (h === null) return null;
            url.host = h; buf = ""; state = "port";
          } else if (c === undefined || c === "/" || c === "?" || c === "#" || (special() && c === "\\")) {
            p--;
            if (special() && buf === "") return null;
            if (stateOverride && buf === "" && (url.username || url.password || url.port !== null)) return url;
            const h = parseHost(buf, !special());
            if (h === null) return null;
            url.host = h; buf = ""; state = "path start";
            if (stateOverride) return url;
          } else {
            if (c === "[") insideBrackets = true;
            if (c === "]") insideBrackets = false;
            buf += c;
          }
          break;
        case "port":
          if (c !== undefined && /[0-9]/.test(c)) buf += c;
          else if (c === undefined || c === "/" || c === "?" || c === "#" || (special() && c === "\\") || stateOverride) {
            if (buf !== "") {
              const port = parseInt(buf, 10);
              if (port > 65535) return null;
              url.port = port === SPECIAL[url.scheme + ":"] ? null : port;
              buf = "";
              if (stateOverride) return url;
            }
            if (stateOverride) return null;
            state = "path start"; p--;
          } else return null;
          break;
        case "file":
          url.scheme = "file";
          url.host = "";
          if (c === "/" || c === "\\") state = "file slash";
          else if (base && base.scheme === "file") {
            url.host = base.host; url.path = base.path.slice(); url.query = base.query;
            if (c === "?") { url.query = ""; state = "query"; }
            else if (c === "#") { url.fragment = ""; state = "fragment"; }
            else if (c !== undefined) {
              url.query = null;
              if (!startsWinLetter(cps, p)) shortenPath(url);
              else url.path = [];
              state = "path"; p--;
            }
          } else { state = "path"; p--; }
          break;
        case "file slash":
          if (c === "/" || c === "\\") state = "file host";
          else {
            if (base && base.scheme === "file") {
              url.host = base.host;
              if (!startsWinLetter(cps, p) && base.path.length && isWinLetter(base.path[0], true)) url.path.push(base.path[0]);
            }
            state = "path"; p--;
          }
          break;
        case "file host":
          if (c === undefined || c === "/" || c === "\\" || c === "?" || c === "#") {
            p--;
            if (!stateOverride && isWinLetter(buf, false)) state = "path";
            else if (buf === "") {
              url.host = "";
              if (stateOverride) return url;
              state = "path start";
            } else {
              let h = parseHost(buf, false);
              if (h === null) return null;
              if (h === "localhost") h = "";
              url.host = h;
              if (stateOverride) return url;
              buf = ""; state = "path start";
            }
          } else buf += c;
          break;
        case "path start":
          if (special()) { state = "path"; if (c !== "/" && c !== "\\") p--; }
          else if (!stateOverride && c === "?") { url.query = ""; state = "query"; }
          else if (!stateOverride && c === "#") { url.fragment = ""; state = "fragment"; }
          else if (c !== undefined) { state = "path"; if (c !== "/") p--; }
          else if (stateOverride && url.host === null) url.path.push("");
          break;
        case "path":
          if (c === undefined || c === "/" || (special() && c === "\\") || (!stateOverride && (c === "?" || c === "#"))) {
            if (isDoubleDot(buf)) {
              shortenPath(url);
              if (c !== "/" && !(special() && c === "\\")) url.path.push("");
            } else if (isSingleDot(buf) && c !== "/" && !(special() && c === "\\")) url.path.push("");
            else if (!isSingleDot(buf)) {
              if (url.scheme === "file" && url.path.length === 0 && isWinLetter(buf, false)) buf = buf[0] + ":";
              url.path.push(buf);
            }
            buf = "";
            if (c === "?") { url.query = ""; state = "query"; }
            if (c === "#") { url.fragment = ""; state = "fragment"; }
          } else buf += pct(c.codePointAt(0), PATH);
          break;
        case "opaque path":
          if (c === "?") { url.query = ""; state = "query"; }
          else if (c === "#") { url.fragment = ""; state = "fragment"; }
          else if (c === " ") {
            const n = cps[p + 1];
            if (n === "?" || n === "#") url.opaque += "%20"; else url.opaque += " ";
          } else if (c !== undefined) url.opaque += pct(c.codePointAt(0), C0);
          break;
        case "query":
          if ((!stateOverride && c === "#") || c === undefined) {
            url.query += pctString(buf, special() ? SPECIAL_QUERY : QUERY);
            buf = "";
            if (c === "#") { url.fragment = ""; state = "fragment"; }
          } else buf += c;
          break;
        case "fragment":
          if (c !== undefined) url.fragment += pct(c.codePointAt(0), FRAGMENT);
          break;
      }
    }
    if (url.opaque !== null && url.opaque.endsWith(" ") && url.fragment === null && url.query === null) url.opaque = url.opaque.replace(/ +$/, "");
    return url;
  }

  function pathString(u) {
    if (u.opaque !== null) return u.opaque;
    let s = "";
    if (u.host === null && u.path.length > 1 && u.path[0] === "") s = "/.";
    return s + u.path.map((x) => "/" + x).join("");
  }
  function serialize(u, noFragment) {
    let s = u.scheme + ":";
    if (u.host !== null) {
      s += "//";
      if (u.username || u.password) { s += u.username; if (u.password) s += ":" + u.password; s += "@"; }
      s += u.host;
      if (u.port !== null) s += ":" + u.port;
    }
    s += pathString(u);
    if (u.query !== null) s += "?" + u.query;
    if (!noFragment && u.fragment !== null) s += "#" + u.fragment;
    return s;
  }
  function origin(u) {
    if (u.scheme === "blob") {
      try { const inner = parse(pathString(u)); if (inner && (inner.scheme === "http" || inner.scheme === "https")) return origin(inner); } catch (e) {}
      return "null";
    }
    if (isSpecialScheme(u.scheme) && u.scheme !== "file") return u.scheme + "://" + u.host + (u.port !== null ? ":" + u.port : "");
    return "null";
  }

  // ---- application/x-www-form-urlencoded ----
  function formParse(s) {
    const out = [];
    for (const seq of s.split("&")) {
      if (seq === "") continue;
      const i = seq.indexOf("=");
      const k = i < 0 ? seq : seq.slice(0, i), v = i < 0 ? "" : seq.slice(i + 1);
      const d = (x) => dec().decode(pctDecodeBytes(x.replace(/\+/g, " ")));
      out.push([d(k), d(v)]);
    }
    return out;
  }
  const toUSV = (s) => String(s).replace(/[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/g, "�");
  const formSerialize = (list) => list.map(([k, v]) => pctString(toUSV(k), FORM, true) + "=" + pctString(toUSV(v), FORM, true)).join("&");
  J.formUrlencode = formSerialize;

  class URLSearchParams {
    constructor(init) {
      this._l = [];
      this._url = null;
      if (init === undefined || init === null) return;
      if (typeof init === "object" || typeof init === "function") {
        if (init instanceof URLSearchParams) { this._l = init._l.map((e) => e.slice()); return; }
        if (typeof init[Symbol.iterator] === "function") {
          for (const pair of init) {
            const a = Array.from(pair);
            if (a.length !== 2) throw new TypeError("Failed to construct 'URLSearchParams': Sequence initializer must only contain pair elements");
            this._l.push([toUSV(a[0]), toUSV(a[1])]);
          }
        } else for (const k of Object.keys(init)) this._l.push([toUSV(k), toUSV(init[k])]);
        return;
      }
      let s = toUSV(init);
      if (s.startsWith("?")) s = s.slice(1);
      this._l = formParse(s);
    }
    _update() {
      if (!this._url) return;
      const q = formSerialize(this._l);
      this._url._u.query = q === "" ? null : q;
      if (q === "") this._url._stripTrailingSpaces();
    }
    get size() { return this._l.length; }
    append(k, v) { this._l.push([toUSV(k), toUSV(v)]); this._update(); }
    delete(k, v) { k = toUSV(k); this._l = this._l.filter((e) => !(e[0] === k && (v === undefined || e[1] === toUSV(v)))); this._update(); }
    get(k) { k = toUSV(k); const e = this._l.find((x) => x[0] === k); return e ? e[1] : null; }
    getAll(k) { k = toUSV(k); return this._l.filter((x) => x[0] === k).map((x) => x[1]); }
    has(k, v) { k = toUSV(k); return this._l.some((e) => e[0] === k && (v === undefined || e[1] === toUSV(v))); }
    set(k, v) {
      k = toUSV(k); v = toUSV(v);
      const i = this._l.findIndex((e) => e[0] === k);
      if (i < 0) this._l.push([k, v]);
      else { this._l[i][1] = v; this._l = this._l.filter((e, j) => j <= i || e[0] !== k); }
      this._update();
    }
    sort() {
      // Stable sort by UTF-16 code units.
      this._l = this._l.map((e, i) => [e, i]).sort((a, b) => (a[0][0] < b[0][0] ? -1 : a[0][0] > b[0][0] ? 1 : a[1] - b[1])).map((x) => x[0]);
      this._update();
    }
    forEach(f, t) { for (let i = 0; i < this._l.length; i++) f.call(t, this._l[i][1], this._l[i][0], this); }
    entries() { return iter(this, (e) => [e[0], e[1]]); }
    keys() { return iter(this, (e) => e[0]); }
    values() { return iter(this, (e) => e[1]); }
    [Symbol.iterator]() { return this.entries(); }
    toString() { return formSerialize(this._l); }
    get [Symbol.toStringTag]() { return "URLSearchParams"; }
  }
  // Live iterator (sees changes made while iterating).
  function iter(sp, f) {
    let i = 0;
    return { next: () => (i < sp._l.length ? { value: f(sp._l[i++]), done: false } : { value: undefined, done: true }), [Symbol.iterator]() { return this; } };
  }

  class URL {
    constructor(url, base) {
      url = toUSV(url);
      let b = null;
      if (base !== undefined) {
        b = parse(toUSV(base));
        if (!b) throw new TypeError("Failed to construct 'URL': Invalid base URL");
      }
      const u = parse(url, b);
      if (!u) throw new TypeError("Failed to construct 'URL': Invalid URL");
      this._u = u;
      this._sp = new URLSearchParams(u.query || "");
      this._sp._url = this;
    }
    static canParse(url, base) { try { new URL(url, base); return true; } catch (e) { return false; } }
    static parse(url, base) { try { return new URL(url, base); } catch (e) { return null; } }
    _stripTrailingSpaces() {
      const u = this._u;
      if (u.opaque !== null && u.fragment === null && u.query === null) u.opaque = u.opaque.replace(/ +$/, "");
    }
    get href() { return serialize(this._u); }
    set href(v) {
      const u = parse(toUSV(v));
      if (!u) throw new TypeError("Failed to set the 'href' property on 'URL': Invalid URL");
      this._u = u;
      this._sp._l = formParse(u.query || "");
    }
    toString() { return this.href; }
    toJSON() { return this.href; }
    get origin() { return origin(this._u); }
    get protocol() { return this._u.scheme + ":"; }
    set protocol(v) { parse(toUSV(v) + ":", null, this._u, "scheme start"); }
    get username() { return this._u.username; }
    set username(v) {
      const u = this._u;
      if (u.host === null || u.host === "" || u.scheme === "file") return;
      u.username = pctString(toUSV(v), USERINFO);
    }
    get password() { return this._u.password; }
    set password(v) {
      const u = this._u;
      if (u.host === null || u.host === "" || u.scheme === "file") return;
      u.password = pctString(toUSV(v), USERINFO);
    }
    get host() { const u = this._u; if (u.host === null) return ""; return u.host + (u.port !== null ? ":" + u.port : ""); }
    set host(v) { if (this._u.opaque !== null) return; parse(toUSV(v), null, this._u, "host"); }
    get hostname() { return this._u.host === null ? "" : this._u.host; }
    set hostname(v) { if (this._u.opaque !== null) return; parse(toUSV(v), null, this._u, "hostname"); }
    get port() { return this._u.port === null ? "" : String(this._u.port); }
    set port(v) {
      const u = this._u;
      if (u.host === null || u.host === "" || u.scheme === "file") return;
      v = toUSV(v);
      if (v === "") u.port = null; else parse(v, null, u, "port");
    }
    get pathname() { return pathString(this._u); }
    set pathname(v) {
      const u = this._u;
      if (u.opaque !== null) return;
      u.path = [];
      parse(toUSV(v), null, u, "path start");
    }
    get search() { const q = this._u.query; return q === null || q === "" ? "" : "?" + q; }
    set search(v) {
      const u = this._u;
      v = toUSV(v);
      if (v === "") { u.query = null; this._sp._l = []; this._stripTrailingSpaces(); return; }
      if (v[0] === "?") v = v.slice(1);
      u.query = "";
      parse(v, null, u, "query");
      this._sp._l = formParse(v);
    }
    get searchParams() { return this._sp; }
    get hash() { const f = this._u.fragment; return f === null || f === "" ? "" : "#" + f; }
    set hash(v) {
      const u = this._u;
      v = toUSV(v);
      if (v === "") { u.fragment = null; this._stripTrailingSpaces(); return; }
      if (v[0] === "#") v = v.slice(1);
      u.fragment = "";
      parse(v, null, u, "fragment");
    }
    static createObjectURL(blob) { return J.createObjectURL(blob); }
    static revokeObjectURL(u) { J.revokeObjectURL(u); }
    get [Symbol.toStringTag]() { return "URL"; }
  }
  G.URL = URL;
  G.webkitURL = URL;
  G.URLSearchParams = URLSearchParams;
  J.parseURL = parse;
  J.serializeURL = serialize;
})(globalThis);
