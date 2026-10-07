#!/usr/bin/env python3
"""Check pixels of a PNG (8-bit RGB/RGBA, any filters) or a binary PPM.

usage: check-png.py FILE CHECK...
  X,Y=R,G,B[~TOL]     the pixel at (X, Y) is that color (default tolerance 24)
  count:R,G,B>=N      at least N pixels are that color (tolerance 24)
  count:R,G,B<=N      at most N pixels are that color
  size=WxH            the image size
"""
import struct
import sys
import zlib


def load(path):
    data = open(path, "rb").read()
    if data[:2] == b"P6":
        # QEMU screendump: binary PPM.
        parts = data.split(maxsplit=4)
        w, h = int(parts[1]), int(parts[2])
        pix = parts[4]
        return w, h, lambda x, y: tuple(pix[(y * w + x) * 3:(y * w + x) * 3 + 3])
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    p, idat = 8, b""
    while p < len(data):
        n = struct.unpack(">I", data[p:p + 4])[0]
        t = data[p + 4:p + 8]
        d = data[p + 8:p + 8 + n]
        if t == b"IHDR":
            w, h, depth, ctype = struct.unpack(">IIBB", d[:10])
        elif t == b"IDAT":
            idat += d
        p += 12 + n
    assert depth == 8 and ctype in (2, 6), "unsupported PNG"
    bpp = 3 if ctype == 2 else 4
    raw = zlib.decompress(idat)
    stride = w * bpp
    rows, prev, o = [], bytearray(stride), 0
    for _ in range(h):
        f = raw[o]
        line = bytearray(raw[o + 1:o + 1 + stride])
        o += 1 + stride
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if f == 1:
                line[i] = (line[i] + a) & 255
            elif f == 2:
                line[i] = (line[i] + b) & 255
            elif f == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif f == 4:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 255
        rows.append(line)
        prev = line
    px = lambda x, y: tuple(rows[y][x * bpp:x * bpp + 3])
    return w, h, px


def close(a, b, tol):
    return all(abs(x - y) <= tol for x, y in zip(a, b))


def main():
    w, h, px = load(sys.argv[1])
    ok = True
    for chk in sys.argv[2:]:
        if chk.startswith("size="):
            want = chk[5:]
            good = want == "%dx%d" % (w, h)
            print("%s %s (got %dx%d)" % ("ok" if good else "FAIL", chk, w, h))
        elif chk.startswith("count:"):
            at_most = "<=" in chk
            col, n = chk[6:].split("<=" if at_most else ">=")
            col = tuple(int(v) for v in col.split(","))
            got = sum(1 for y in range(h) for x in range(w) if close(px(x, y), col, 24))
            good = got <= int(n) if at_most else got >= int(n)
            print("%s %s (got %d)" % ("ok" if good else "FAIL", chk, got))
        else:
            pos, col = chk.split("=")
            tol = 24
            if "~" in col:
                col, tol = col.split("~")
                tol = int(tol)
            x, y = (int(v) for v in pos.split(","))
            col = tuple(int(v) for v in col.split(","))
            got = px(x, y)
            good = close(got, col, tol)
            print("%s %s (got %s)" % ("ok" if good else "FAIL", chk, got))
        ok &= good
    sys.exit(0 if ok else 1)


main()
