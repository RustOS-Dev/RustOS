#!/usr/bin/env python3
"""Check that a WAV file (as QEMU's wav audio backend records it) holds a
tone: check-wav.py FILE FREQ [MIN_RMS]. Finds the loud part, then compares
the Goertzel power at FREQ with neighbouring frequencies."""
import math, struct, sys

path, freq = sys.argv[1], float(sys.argv[2])
min_rms = float(sys.argv[3]) if len(sys.argv) > 3 else 500
data = open(path, "rb").read()
if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
    sys.exit("not a WAV file")
o, rate, ch, bits, body = 12, 44100, 2, 16, b""
while o + 8 <= len(data):
    cid, clen = data[o:o + 4], struct.unpack_from("<I", data, o + 4)[0]
    if cid == b"fmt ":
        _, ch, rate, _, _, bits = struct.unpack_from("<HHIIHH", data, o + 8)
    if cid == b"data":
        body = data[o + 8:]  # QEMU fixes the length only at exit: take the rest
        break
    o += 8 + clen + (clen & 1)
if bits != 16:
    sys.exit("expected 16-bit samples, got %d" % bits)
n = len(body) // (2 * ch)
mono = [sum(struct.unpack_from("<%dh" % ch, body, i * 2 * ch)) / ch for i in range(n)]
# The loud part: 10 ms windows above a threshold.
win = rate // 100
loud = [i for i in range(0, n - win, win)
        if math.sqrt(sum(x * x for x in mono[i:i + win]) / win) > min_rms / 2]
if not loud:
    sys.exit("no signal (%d frames, %.1f s)" % (n, n / rate))
start = loud[len(loud) // 4]
seg = mono[start:start + min(rate // 2, loud[-1] - start + win)]
rms = math.sqrt(sum(x * x for x in seg) / len(seg))

def goertzel(s, f):
    c = 2 * math.cos(2 * math.pi * f / rate)
    s1 = s2 = 0.0
    for x in s:
        s1, s2 = x + c * s1 - s2, s1
    return s1 * s1 + s2 * s2 - c * s1 * s2

p = goertzel(seg, freq)
others = [goertzel(seg, freq * k) for k in (0.5, 0.75, 1.25, 1.5, 2.0)]
ratio = p / max(max(others), 1e-9)
print("%s: %d Hz, %d ch, %.2f s loud, rms %.0f, %.0f Hz power ratio %.1f"
      % (path, rate, ch, len(loud) * win / rate, rms, freq, ratio))
if rms < min_rms or ratio < 20:
    sys.exit("tone check failed")
