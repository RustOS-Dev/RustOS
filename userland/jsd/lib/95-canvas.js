// <canvas> 2D context: a software rasterizer over an RGBA buffer
// (el._pixels). The graphical browser reads the pixels and shows them
// as the canvas element's image. Paths are flattened to polygons and
// filled with nonzero scanlines (4x vertical supersampling); strokes
// become one quad per segment. Text is not rasterized in jsd (measureText
// estimates widths).
"use strict";
(function (G) {
  const J = G.__jsd;
  G.__jsd_b64 = (u8) => J.b64(u8);

  const NAMED = { black: [0, 0, 0], white: [255, 255, 255], red: [255, 0, 0], green: [0, 128, 0], blue: [0, 0, 255], yellow: [255, 255, 0],
    orange: [255, 165, 0], purple: [128, 0, 128], gray: [128, 128, 128], grey: [128, 128, 128], lime: [0, 255, 0], cyan: [0, 255, 255],
    magenta: [255, 0, 255], navy: [0, 0, 128], teal: [0, 128, 128], maroon: [128, 0, 0], silver: [192, 192, 192], pink: [255, 192, 203],
    brown: [165, 42, 42], gold: [255, 215, 0], transparent: [0, 0, 0, 0] };
  function parseColor(s) {
    if (typeof s !== "string") return null;
    s = s.trim().toLowerCase();
    if (NAMED[s]) { const c = NAMED[s]; return [c[0], c[1], c[2], c.length > 3 ? c[3] : 255]; }
    let m = /^#([0-9a-f]{3,8})$/.exec(s);
    if (m) {
      let h = m[1];
      if (h.length === 3 || h.length === 4) h = h.split("").map((c) => c + c).join("");
      const n = (i) => parseInt(h.slice(i, i + 2), 16);
      return [n(0), n(2), n(4), h.length === 8 ? n(6) : 255];
    }
    m = /^rgba?\(([^)]*)\)$/.exec(s);
    if (m) {
      const p = m[1].split(/[\s,/]+/).filter(Boolean).map((x) => (x.endsWith("%") ? parseFloat(x) * 2.55 : parseFloat(x)));
      return [p[0] | 0, p[1] | 0, p[2] | 0, p.length > 3 ? Math.round((m[1].includes("%") && p[3] > 1 ? p[3] / 255 : p[3]) * 255) : 255];
    }
    m = /^hsla?\(([^)]*)\)$/.exec(s);
    if (m) {
      const p = m[1].split(/[\s,/]+/).filter(Boolean).map(parseFloat);
      const h = ((p[0] % 360) + 360) % 360 / 360, sat = p[1] / 100, l = p[2] / 100;
      const q = l < 0.5 ? l * (1 + sat) : l + sat - l * sat, pp = 2 * l - q;
      const f = (t) => { t = (t + 1) % 1; return t < 1 / 6 ? pp + (q - pp) * 6 * t : t < 0.5 ? q : t < 2 / 3 ? pp + (q - pp) * (2 / 3 - t) * 6 : pp; };
      return [Math.round(f(h + 1 / 3) * 255), Math.round(f(h) * 255), Math.round(f(h - 1 / 3) * 255), p.length > 3 ? Math.round(p[3] * 255) : 255];
    }
    return null;
  }

  class ImageData {
    constructor(a, b, c) {
      if (a instanceof Uint8ClampedArray) { this.data = a; this.width = b; this.height = c === undefined ? a.length / 4 / b : c; }
      else { this.width = a; this.height = b; this.data = new Uint8ClampedArray(a * b * 4); }
    }
  }
  class CanvasGradient {
    constructor(kind, args) { this._kind = kind; this._args = args; this._stops = []; }
    addColorStop(o, c) { this._stops.push([o, parseColor(c) || [0, 0, 0, 255]]); this._stops.sort((a, b) => a[0] - b[0]); }
    _at(x, y) {
      let t;
      if (this._kind === "linear") {
        const [x0, y0, x1, y1] = this._args;
        const dx = x1 - x0, dy = y1 - y0, l = dx * dx + dy * dy || 1;
        t = ((x - x0) * dx + (y - y0) * dy) / l;
      } else {
        const [x0, y0, r0, x1, y1, r1] = this._args;
        void x0; void y0; void r0;
        t = (Math.hypot(x - x1, y - y1)) / (r1 || 1);
      }
      t = Math.max(0, Math.min(1, t));
      const s = this._stops;
      if (!s.length) return [0, 0, 0, 0];
      if (t <= s[0][0]) return s[0][1];
      for (let i = 1; i < s.length; i++) {
        if (t <= s[i][0]) {
          const k = (t - s[i - 1][0]) / ((s[i][0] - s[i - 1][0]) || 1);
          return s[i][1].map((v, j) => Math.round(s[i - 1][1][j] + (v - s[i - 1][1][j]) * k));
        }
      }
      return s[s.length - 1][1];
    }
  }

  class CanvasRenderingContext2D {
    constructor(canvas) {
      this.canvas = canvas;
      this._st = { fill: [0, 0, 0, 255], stroke: [0, 0, 0, 255], fillSrc: "#000000", strokeSrc: "#000000", alpha: 1, lineWidth: 1, m: [1, 0, 0, 1, 0, 0], font: "10px sans-serif", textAlign: "start", textBaseline: "alphabetic", composite: "source-over" };
      this._stack = [];
      this._path = [];
      this._sub = null;
      this.imageSmoothingEnabled = true;
      this.lineCap = "butt"; this.lineJoin = "miter"; this.miterLimit = 10; this.shadowBlur = 0; this.shadowColor = "transparent"; this.shadowOffsetX = 0; this.shadowOffsetY = 0;
      this._ensure();
    }
    _ensure() {
      const c = this.canvas, w = c.width | 0, h = c.height | 0;
      if (!c._pixels || c._pw !== w || c._ph !== h) { c._pixels = new Uint8ClampedArray(w * h * 4); c._pw = w; c._ph = h; }
      return c._pixels;
    }
    get fillStyle() { return this._st.fillSrc; }
    set fillStyle(v) { if (v instanceof CanvasGradient) { this._st.fill = v; this._st.fillSrc = v; return; } const c = parseColor(String(v)); if (c) { this._st.fill = c; this._st.fillSrc = String(v); } }
    get strokeStyle() { return this._st.strokeSrc; }
    set strokeStyle(v) { if (v instanceof CanvasGradient) { this._st.stroke = v; this._st.strokeSrc = v; return; } const c = parseColor(String(v)); if (c) { this._st.stroke = c; this._st.strokeSrc = String(v); } }
    get globalAlpha() { return this._st.alpha; }
    set globalAlpha(v) { v = +v; if (v >= 0 && v <= 1) this._st.alpha = v; }
    get lineWidth() { return this._st.lineWidth; }
    set lineWidth(v) { v = +v; if (v > 0) this._st.lineWidth = v; }
    get font() { return this._st.font; }
    set font(v) { this._st.font = String(v); }
    get textAlign() { return this._st.textAlign; }
    set textAlign(v) { this._st.textAlign = String(v); }
    get textBaseline() { return this._st.textBaseline; }
    set textBaseline(v) { this._st.textBaseline = String(v); }
    get globalCompositeOperation() { return this._st.composite; }
    set globalCompositeOperation(v) { this._st.composite = String(v); }
    save() { this._stack.push(Object.assign({}, this._st, { m: this._st.m.slice() })); }
    restore() { const s = this._stack.pop(); if (s) this._st = s; }
    // ---- transforms ----
    _tp(x, y) { const m = this._st.m; return [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]]; }
    transform(a, b, c, d, e, f) { const m = this._st.m; this._st.m = [m[0] * a + m[2] * b, m[1] * a + m[3] * b, m[0] * c + m[2] * d, m[1] * c + m[3] * d, m[0] * e + m[2] * f + m[4], m[1] * e + m[3] * f + m[5]]; }
    setTransform(a, b, c, d, e, f) { if (typeof a === "object" && a) { this._st.m = [a.a, a.b, a.c, a.d, a.e, a.f]; return; } this._st.m = [a, b, c, d, e, f]; }
    resetTransform() { this._st.m = [1, 0, 0, 1, 0, 0]; }
    getTransform() { const m = this._st.m; return { a: m[0], b: m[1], c: m[2], d: m[3], e: m[4], f: m[5] }; }
    translate(x, y) { this.transform(1, 0, 0, 1, x, y); }
    scale(x, y) { this.transform(x, 0, 0, y, 0, 0); }
    rotate(a) { const c = Math.cos(a), s = Math.sin(a); this.transform(c, s, -s, c, 0, 0); }
    // ---- pixels ----
    _blend(i, c, cov) {
      const px = this._ensure();
      const a = (c[3] / 255) * this._st.alpha * cov;
      if (a <= 0) return;
      if (this._st.composite === "copy" || a >= 1) { px[i] = c[0]; px[i + 1] = c[1]; px[i + 2] = c[2]; px[i + 3] = Math.round(Math.max(a, px[i + 3] / 255 * (1 - a) + a) * 255); if (a >= 1) px[i + 3] = 255; return; }
      const da = px[i + 3] / 255, oa = a + da * (1 - a);
      for (let k = 0; k < 3; k++) px[i + k] = Math.round((c[k] * a + px[i + k] * da * (1 - a)) / (oa || 1));
      px[i + 3] = Math.round(oa * 255);
    }
    _fillPoly(polys, style) {
      const w = this.canvas.width | 0, h = this.canvas.height | 0;
      let miny = Infinity, maxy = -Infinity;
      for (const p of polys) for (const [, y] of p) { miny = Math.min(miny, y); maxy = Math.max(maxy, y); }
      miny = Math.max(0, Math.floor(miny)); maxy = Math.min(h, Math.ceil(maxy));
      const S = 4, acc = new Float32Array(w);
      for (let py = miny; py < maxy; py++) {
        acc.fill(0);
        for (let s = 0; s < S; s++) {
          const sy = py + (s + 0.5) / S, xs = [];
          for (const p of polys) {
            for (let i = 0; i < p.length; i++) {
              const a = p[i], b = p[(i + 1) % p.length];
              if ((a[1] <= sy && b[1] > sy) || (b[1] <= sy && a[1] > sy)) xs.push([a[0] + (sy - a[1]) * (b[0] - a[0]) / (b[1] - a[1]), b[1] > a[1] ? 1 : -1]);
            }
          }
          xs.sort((p, q) => p[0] - q[0]);
          let wind = 0;
          for (let k = 0; k < xs.length - 1; k++) {
            wind += xs[k][1];
            if (wind === 0) continue;
            let x0 = Math.max(0, xs[k][0]), x1 = Math.min(w, xs[k + 1][0]);
            for (let x = x0; x < x1;) {
              const pxl = Math.floor(x), seg = Math.min(pxl + 1, x1) - x;
              acc[pxl] += seg / S;
              x = pxl + 1;
            }
          }
        }
        for (let x = 0; x < w; x++) {
          if (acc[x] > 0) {
            const c = style instanceof CanvasGradient ? style._at(x + 0.5, py + 0.5) : style;
            this._blend((py * w + x) * 4, c, Math.min(1, acc[x]));
          }
        }
      }
    }
    fillRect(x, y, w, h) { this._fillPoly([[this._tp(x, y), this._tp(x + w, y), this._tp(x + w, y + h), this._tp(x, y + h)]], this._st.fill); }
    clearRect(x, y, w, h) {
      const px = this._ensure(), cw = this.canvas.width | 0, ch = this.canvas.height | 0;
      const [ax, ay] = this._tp(x, y), [bx, by] = this._tp(x + w, y + h);
      for (let j = Math.max(0, Math.round(Math.min(ay, by))); j < Math.min(ch, Math.round(Math.max(ay, by))); j++)
        for (let i = Math.max(0, Math.round(Math.min(ax, bx))); i < Math.min(cw, Math.round(Math.max(ax, bx))); i++) px.fill(0, (j * cw + i) * 4, (j * cw + i) * 4 + 4);
    }
    strokeRect(x, y, w, h) { this.beginPath(); this.rect(x, y, w, h); this.stroke(); this.beginPath(); }
    // ---- paths ----
    beginPath() { this._path = []; this._sub = null; }
    moveTo(x, y) { this._sub = [this._tp(x, y)]; this._path.push(this._sub); }
    lineTo(x, y) { if (!this._sub) return this.moveTo(x, y); this._sub.push(this._tp(x, y)); }
    closePath() { if (this._sub && this._sub.length) { this._sub.closed = true; const f = this._sub[0]; this._sub = [f]; this._path.push(this._sub); } }
    rect(x, y, w, h) { this.moveTo(x, y); this.lineTo(x + w, y); this.lineTo(x + w, y + h); this.lineTo(x, y + h); this.closePath(); }
    roundRect(x, y, w, h, r) { r = Math.min(+(Array.isArray(r) ? r[0] : r) || 0, w / 2, h / 2); this.moveTo(x + r, y); this.arcTo(x + w, y, x + w, y + h, r); this.arcTo(x + w, y + h, x, y + h, r); this.arcTo(x, y + h, x, y, r); this.arcTo(x, y, x + w, y, r); this.closePath(); }
    arc(cx, cy, r, a0, a1, ccw) {
      let sweep = a1 - a0;
      if (!ccw && sweep < 0) sweep = (sweep % (2 * Math.PI)) + 2 * Math.PI;
      if (ccw && sweep > 0) sweep = (sweep % (2 * Math.PI)) - 2 * Math.PI;
      if (Math.abs(a1 - a0) >= 2 * Math.PI) sweep = ccw ? -2 * Math.PI : 2 * Math.PI;
      const n = Math.max(8, Math.ceil(Math.abs(sweep) * Math.max(r, 1) / 2));
      for (let i = 0; i <= n; i++) {
        const a = a0 + sweep * i / n, x = cx + r * Math.cos(a), y = cy + r * Math.sin(a);
        if (i === 0 && !this._sub) this.moveTo(x, y); else this.lineTo(x, y);
      }
    }
    ellipse(cx, cy, rx, ry, rot, a0, a1, ccw) { this.save(); this.translate(cx, cy); this.rotate(rot); this.scale(rx, ry); this.arc(0, 0, 1, a0, a1, ccw); this.restore(); }
    arcTo(x1, y1, x2, y2, r) { this.lineTo(x1, y1); void x2; void y2; void r; }
    quadraticCurveTo(cx, cy, x, y) {
      const inv = this._inv(), p0 = this._sub ? this._sub[this._sub.length - 1] : [x, y];
      const [sx, sy] = inv(p0[0], p0[1]);
      for (let i = 1; i <= 12; i++) { const t = i / 12, u = 1 - t; this.lineTo(u * u * sx + 2 * u * t * cx + t * t * x, u * u * sy + 2 * u * t * cy + t * t * y); }
    }
    bezierCurveTo(c1x, c1y, c2x, c2y, x, y) {
      const inv = this._inv(), p0 = this._sub ? this._sub[this._sub.length - 1] : [x, y];
      const [sx, sy] = inv(p0[0], p0[1]);
      for (let i = 1; i <= 16; i++) { const t = i / 16, u = 1 - t; this.lineTo(u * u * u * sx + 3 * u * u * t * c1x + 3 * u * t * t * c2x + t * t * t * x, u * u * u * sy + 3 * u * u * t * c1y + 3 * u * t * t * c2y + t * t * t * y); }
    }
    _inv() {
      const [a, b, c, d, e, f] = this._st.m, det = a * d - b * c || 1;
      return (x, y) => [(d * (x - e) - c * (y - f)) / det, (-b * (x - e) + a * (y - f)) / det];
    }
    fill() { this._fillPoly(this._path.filter((p) => p.length > 2), this._st.fill); }
    stroke() {
      const hw = this._st.lineWidth * Math.hypot(this._st.m[0], this._st.m[1]) / 2, quads = [];
      for (const p of this._path) {
        const pts = p.closed ? p.concat([p[0]]) : p;
        for (let i = 0; i + 1 < pts.length; i++) {
          const [x0, y0] = pts[i], [x1, y1] = pts[i + 1], l = Math.hypot(x1 - x0, y1 - y0);
          if (l < 1e-6) continue;
          const nx = -(y1 - y0) / l * hw, ny = (x1 - x0) / l * hw;
          // Square caps on joints keep corners filled.
          const ex = (x1 - x0) / l * hw, ey = (y1 - y0) / l * hw;
          quads.push([[x0 + nx - ex, y0 + ny - ey], [x1 + nx + ex, y1 + ny + ey], [x1 - nx + ex, y1 - ny + ey], [x0 - nx - ex, y0 - ny - ey]]);
        }
      }
      for (const q of quads) this._fillPoly([q], this._st.stroke);
    }
    clip() {}
    isPointInPath(x, y) {
      for (const p of this._path) {
        let inside = false;
        for (let i = 0, j = p.length - 1; i < p.length; j = i++) if ((p[i][1] > y) !== (p[j][1] > y) && x < (p[j][0] - p[i][0]) * (y - p[i][1]) / (p[j][1] - p[i][1]) + p[i][0]) inside = !inside;
        if (inside) return true;
      }
      return false;
    }
    // ---- text (measured, not drawn) ----
    _size() { const m = /(\d+(?:\.\d+)?)px/.exec(this._st.font); return m ? parseFloat(m[1]) : 10; }
    measureText(t) { const w = String(t).length * this._size() * 0.55; return { width: w, actualBoundingBoxAscent: this._size() * 0.8, actualBoundingBoxDescent: this._size() * 0.2, fontBoundingBoxAscent: this._size() * 0.8, fontBoundingBoxDescent: this._size() * 0.2, actualBoundingBoxLeft: 0, actualBoundingBoxRight: w }; }
    fillText() {}
    strokeText() {}
    // ---- images ----
    createImageData(w, h) { if (w instanceof ImageData) return new ImageData(w.width, w.height); return new ImageData(Math.abs(w | 0), Math.abs(h | 0)); }
    getImageData(sx, sy, sw, sh) {
      const px = this._ensure(), cw = this.canvas.width | 0, ch = this.canvas.height | 0, out = new ImageData(sw, sh);
      for (let j = 0; j < sh; j++) for (let i = 0; i < sw; i++) {
        const x = sx + i, y = sy + j;
        if (x < 0 || y < 0 || x >= cw || y >= ch) continue;
        out.data.set(px.subarray((y * cw + x) * 4, (y * cw + x) * 4 + 4), (j * sw + i) * 4);
      }
      return out;
    }
    putImageData(d, dx, dy) {
      const px = this._ensure(), cw = this.canvas.width | 0, ch = this.canvas.height | 0;
      for (let j = 0; j < d.height; j++) for (let i = 0; i < d.width; i++) {
        const x = dx + i, y = dy + j;
        if (x < 0 || y < 0 || x >= cw || y >= ch) continue;
        px.set(d.data.subarray((j * d.width + i) * 4, (j * d.width + i) * 4 + 4), (y * cw + x) * 4);
      }
    }
    drawImage(src, ...a) {
      const sp = src && src._pixels;
      if (!sp) return; // only canvases (images are not decoded in jsd)
      const sw0 = src.width | 0, sh0 = src.height | 0;
      let [sx, sy, sw, sh, dx, dy, dw, dh] = a.length === 2 ? [0, 0, sw0, sh0, a[0], a[1], sw0, sh0] : a.length === 4 ? [0, 0, sw0, sh0, a[0], a[1], a[2], a[3]] : a;
      const cw = this.canvas.width | 0;
      for (let j = 0; j < dh; j++) for (let i = 0; i < dw; i++) {
        const x = Math.floor(sx + i * sw / dw), y = Math.floor(sy + j * sh / dh);
        if (x < 0 || y < 0 || x >= sw0 || y >= sh0) continue;
        const [tx, ty] = this._tp(dx + i, dy + j);
        const X = Math.floor(tx), Y = Math.floor(ty);
        if (X < 0 || Y < 0 || X >= cw || Y >= (this.canvas.height | 0)) continue;
        const k = (y * sw0 + x) * 4;
        this._blend((Y * cw + X) * 4, [sp[k], sp[k + 1], sp[k + 2], sp[k + 3]], 1);
      }
    }
    createLinearGradient(x0, y0, x1, y1) { const a = this._tp(x0, y0), b = this._tp(x1, y1); return new CanvasGradient("linear", [a[0], a[1], b[0], b[1]]); }
    createRadialGradient(x0, y0, r0, x1, y1, r1) { const a = this._tp(x0, y0), b = this._tp(x1, y1); return new CanvasGradient("radial", [a[0], a[1], r0, b[0], b[1], r1]); }
    createPattern() { return null; }
    setLineDash() {}
    getLineDash() { return []; }
  }

  J.canvasContext = function (el, kind) {
    if (kind !== "2d") return null;
    return el._ctx || (el._ctx = new CanvasRenderingContext2D(el));
  };
  // toDataURL: a PNG made by the browser is not available here; return
  // an uncompressed BMP data URL (decodable by browsers and by browse).
  G.HTMLCanvasElement.prototype.toDataURL = function () {
    const w = this.width | 0, h = this.height | 0, px = this._pixels || new Uint8ClampedArray(w * h * 4);
    const size = 54 + w * h * 4, b = new Uint8Array(size), dv = new DataView(b.buffer);
    b[0] = 66; b[1] = 77; dv.setUint32(2, size, true); dv.setUint32(10, 54, true); dv.setUint32(14, 40, true);
    dv.setInt32(18, w, true); dv.setInt32(22, -h, true); dv.setUint16(26, 1, true); dv.setUint16(28, 32, true);
    for (let i = 0; i < w * h; i++) { b[54 + i * 4] = px[i * 4 + 2]; b[55 + i * 4] = px[i * 4 + 1]; b[56 + i * 4] = px[i * 4]; b[57 + i * 4] = px[i * 4 + 3]; }
    return "data:image/bmp;base64," + J.b64(b);
  };
  Object.assign(G, { CanvasRenderingContext2D, CanvasGradient, ImageData });
})(globalThis);
