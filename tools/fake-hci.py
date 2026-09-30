#!/usr/bin/env python3
"""A scripted Bluetooth LE controller with one peripheral behind it: a
BLE HID keyboard, for the `bluetooth` QEMU scenario.

It connects to QEMU's H4 serial port (a TCP chardev) and answers HCI
commands like a controller. The keyboard advertises the HID service,
accepts a connection, pairs with LE Secure Connections (Just Works: its
own SMP responder, P-256, AES-CMAC and f4/f5/f6 written here in pure
Python, so the kernel's crypto is checked against an independent
implementation), checks the LTK the host encrypts with, serves a GATT
database with the HID service, and once notifications are enabled
presses Shift once a second. After a disconnection it advertises again, so the
host's reconnection with the stored key is exercised too.

Usage: fake-hci.py PORT LOG   (or --selftest)
"""

import os
import socket
import struct
import sys
import threading
import time

# ---------------------------------------------------------------- AES-128

SBOX = [0] * 256


def _init_sbox():
    p = q = 1
    while True:
        p = p ^ ((p << 1) & 0xFF) ^ (0x1B if p & 0x80 else 0)
        q ^= q << 1
        q ^= q << 2
        q ^= q << 4
        q &= 0xFF
        if q & 0x80:
            q ^= 0x09
        x = q ^ (q << 1 | q >> 7) ^ (q << 2 | q >> 6) ^ (q << 3 | q >> 5) ^ (q << 4 | q >> 4)
        SBOX[p] = (x ^ 0x63) & 0xFF
        if p == 1:
            break
    SBOX[0] = 0x63


_init_sbox()


def _xtime(a):
    return ((a << 1) ^ 0x1B) & 0xFF if a & 0x80 else a << 1


def aes128(key, block):
    """AES-128 encryption of one 16-byte block (bytes in, bytes out)."""
    w = [list(key[i:i + 4]) for i in range(0, 16, 4)]
    rcon = 1
    for i in range(4, 44):
        t = list(w[i - 1])
        if i % 4 == 0:
            t = [SBOX[b] for b in t[1:] + t[:1]]
            t[0] ^= rcon
            rcon = _xtime(rcon)
        w.append([a ^ b for a, b in zip(w[i - 4], t)])
    s = [b ^ k for b, k in zip(block, sum(w[0:4], []))]
    for rnd in range(1, 11):
        s = [SBOX[b] for b in s]
        s = [s[(i + 4 * (i % 4)) % 16] for i in range(16)]  # ShiftRows
        if rnd != 10:
            m = []
            for c in range(4):
                a = s[4 * c:4 * c + 4]
                t = a[0] ^ a[1] ^ a[2] ^ a[3]
                m += [a[i] ^ t ^ _xtime(a[i] ^ a[(i + 1) % 4]) for i in range(4)]
            s = m
        s = [b ^ k for b, k in zip(s, sum(w[4 * rnd:4 * rnd + 4], []))]
    return bytes(s)


def aes_cmac(key, msg):
    """RFC 4493 AES-CMAC."""
    def shift(b):
        v = int.from_bytes(b, "big") << 1
        r = (v & ((1 << 128) - 1)).to_bytes(16, "big")
        return bytes(r[:-1]) + bytes([r[-1] ^ (0x87 if v >> 128 else 0)])
    l = aes128(key, bytes(16))
    k1 = shift(l)
    k2 = shift(k1)
    n = max(1, (len(msg) + 15) // 16)
    last = msg[16 * (n - 1):]
    if len(last) == 16:
        last = bytes(a ^ b for a, b in zip(last, k1))
    else:
        last = last + b"\x80" + bytes(15 - len(last))
        last = bytes(a ^ b for a, b in zip(last, k2))
    x = bytes(16)
    for i in range(n - 1):
        x = aes128(key, bytes(a ^ b for a, b in zip(x, msg[16 * i:16 * i + 16])))
    return aes128(key, bytes(a ^ b for a, b in zip(x, last)))


# SMP functions on wire-order (little-endian) values.
def _r(b):
    return bytes(reversed(b))


def cmac_le(key, *parts):
    return _r(aes_cmac(_r(key), b"".join(_r(p) for p in parts)))


def f4(u, v, x, z):
    return cmac_le(x, u, v, bytes([z]))


def f5(w, n1, n2, a1, a2):
    salt = _r(bytes.fromhex("6C888391AAF5A53860370BDB5A6083BE"))
    t = cmac_le(salt, w)
    kid = struct.pack("<I", 0x62746C65)
    ln = struct.pack("<H", 256)
    return (cmac_le(t, b"\x00", kid, n1, n2, a1, a2, ln),
            cmac_le(t, b"\x01", kid, n1, n2, a1, a2, ln))


def f6(w, n1, n2, r, io, a1, a2):
    return cmac_le(w, n1, n2, r, io, a1, a2)


# ---------------------------------------------------------------- P-256

P = 0xFFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFF
A = P - 3
GX = 0x6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296
GY = 0x4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5
N = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551


def ec_add(p1, p2):
    if p1 is None:
        return p2
    if p2 is None:
        return p1
    if p1[0] == p2[0]:
        if (p1[1] + p2[1]) % P == 0:
            return None
        lam = (3 * p1[0] * p1[0] + A) * pow(2 * p1[1], P - 2, P) % P
    else:
        lam = (p2[1] - p1[1]) * pow(p2[0] - p1[0], P - 2, P) % P
    x = (lam * lam - p1[0] - p2[0]) % P
    return (x, (lam * (p1[0] - x) - p1[1]) % P)


def ec_mul(k, pt):
    r = None
    while k:
        if k & 1:
            r = ec_add(r, pt)
        pt = ec_add(pt, pt)
        k >>= 1
    return r


def on_curve(pt):
    x, y = pt
    return (y * y - (x * x * x + A * x + 0x5AC635D8AA3A93E7B3EBBD55769886BC651D06B0CC53B0F63BCE3C3E27D2604B)) % P == 0


# ---------------------------------------------------------------- self-test


def selftest():
    k = bytes.fromhex("2b7e151628aed2a6abf7158809cf4f3c")
    assert aes_cmac(k, b"").hex() == "bb1d6929e95937287fa37d129b756746"
    assert aes_cmac(k, bytes.fromhex("6bc1bee22e409f96e93d7e117393172a")).hex() == \
        "070a16b46b4d4144f79bdd9dd04a287c"
    le = lambda s: _r(bytes.fromhex(s.replace(" ", "")))
    w = le("ec0234a3 57c8ad05 341010a6 0a397d9b 99796b13 b4f866f1 868d34f3 73bfa698")
    n1 = le("d5cb8454 d177733e ffffb2ec 712baeab")
    n2 = le("a6e8e7cc 25a75f6e 216583f7 ff3dc4cf")
    a1, a2 = le("00561237 37bfce"), le("00a71370 2dcfc1")
    mac, ltk = f5(w, n1, n2, a1, a2)
    assert mac == le("2965f176 a1084a02 fd3f6a20 ce636e20")
    assert ltk == le("69867911 69d7cd23 980522b5 94750a38")
    assert f6(mac, n1, n2, le("12a3343b b453bb54 08da42d2 0c2d0fc8"), le("010102"), a1, a2) == \
        le("e3c47398 9cd0e8c5 d26c0b09 da958f61")
    d = 0x3f49f6d4a3c55f3874c9b3e3d2103f504aff607beb40b7995899b8a6cd3c1abd
    q = ec_mul(d, (GX, GY))
    assert q[0] == 0x20b003d2f297be2c5e2c83a7e9f9a5b9eff49111acf4fddbcc0301480e359de6
    assert on_curve(q)


# ---------------------------------------------------------------- controller

HOST_ADDR = bytes.fromhex("1371DA7D1A00")        # 00:1A:7D:DA:71:13
KB_ADDR = bytes.fromhex("554433221" "1C0")         # C0:11:22:33:44:55 (random static)
HANDLE = 0x0040
NAME = b"RustOS Keys"

# A keyboard report map with report ID 1.
REPORT_MAP = bytes([
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01,
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01,
    0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02,
    0x95, 0x01, 0x75, 0x03, 0x91, 0x01,
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00,
    0xC0,
])


class Gatt:
    """The keyboard's attribute database."""

    def __init__(self):
        self.attrs = []  # (handle, type, value, needs_encryption)
        self.service(0x1800)
        self.char(0x2A00, 0x02, NAME)
        self.service(0x180A)
        self.char(0x2A50, 0x02, bytes([2, 0x34, 0x12, 0x78, 0x56, 0x00, 0x01]))
        self.service(0x1812)
        self.char(0x2A4A, 0x02, bytes([0x11, 0x01, 0x00, 0x02]))
        self.char(0x2A4B, 0x02, REPORT_MAP, secure=True)
        self.char(0x2A4E, 0x06, b"\x01")
        self.report = self.char(0x2A4D, 0x12, bytes(8))
        self.cccd = self.add(0x2902, b"\x00\x00", secure=True)
        self.add(0x2908, bytes([1, 1]))
        self.led = self.char(0x2A4D, 0x0E, b"\x00")
        self.add(0x2908, bytes([1, 2]))
        self.char(0x2A4C, 0x04, b"\x00")

    def add(self, ty, value, secure=False):
        h = len(self.attrs) + 1
        self.attrs.append([h, ty, value, secure])
        return h

    def service(self, uuid):
        return self.add(0x2800, struct.pack("<H", uuid))

    def char(self, uuid, props, value, secure=False):
        h = len(self.attrs) + 1
        self.add(0x2803, bytes([props]) + struct.pack("<HH", h + 1, uuid))
        return self.add(uuid, value, secure)

    def group_end(self, i):
        for a in self.attrs[i + 1:]:
            if a[1] == 0x2800:
                return a[0] - 1
        return len(self.attrs)


class Fake:
    def __init__(self, sock, log):
        self.sock = sock
        self.log = log
        self.lock = threading.Lock()
        self.scanning = False
        self.advertising = True
        self.connected = False
        self.encrypted = False
        self.gatt = Gatt()
        self.mtu = 23
        self.ltk = None           # the bond
        self.pending_ltk = None   # derived during pairing
        self.smp = {}
        self.notify = False
        self.typing = 0

    def say(self, *a):
        self.log.write(" ".join(str(x) for x in a) + "\n")
        self.log.flush()

    # ---- transport ----

    def send(self, kind, pkt):
        with self.lock:
            self.sock.sendall(bytes([kind]) + pkt)

    def event(self, code, params):
        self.send(4, bytes([code, len(params)]) + params)

    def complete(self, op, ret):
        self.event(0x0E, bytes([1]) + struct.pack("<H", op) + ret)

    def status(self, op, st=0):
        self.event(0x0F, bytes([st, 1]) + struct.pack("<H", op))

    def acl(self, cid, payload):
        frame = struct.pack("<HH", len(payload), cid) + payload
        self.send(2, struct.pack("<HH", HANDLE, len(frame)) + frame)

    def recv_exact(self, n):
        b = b""
        while len(b) < n:
            c = self.sock.recv(n - len(b))
            if not c:
                raise EOFError
            b += c
        return b

    def run(self):
        threading.Thread(target=self.advertiser, daemon=True).start()
        partial = b""
        while True:
            kind = self.recv_exact(1)[0]
            if kind == 1:
                hdr = self.recv_exact(3)
                op, n = struct.unpack("<HB", hdr)
                self.command(op, self.recv_exact(n))
            elif kind == 2:
                hdr = self.recv_exact(4)
                h, n = struct.unpack("<HH", hdr)
                data = self.recv_exact(n)
                self.event(0x13, bytes([1]) + struct.pack("<HH", h & 0xFFF, 1))
                partial = data if (h >> 12) & 3 != 1 else partial + data
                if len(partial) >= 4:
                    ln, cid = struct.unpack("<HH", partial[:4])
                    if len(partial) >= 4 + ln:
                        self.l2cap(cid, partial[4:4 + ln])
                        partial = b""
            else:
                self.say("bad packet type", kind)

    # ---- HCI commands ----

    def command(self, op, p):
        ok = bytes([0])
        if op == 0x1001:    # Read Local Version: 5.3, Linux Foundation
            self.complete(op, ok + bytes([12]) + struct.pack("<HBHH", 1, 12, 0x05F1, 1))
        elif op == 0x1009:  # Read BD_ADDR
            self.complete(op, ok + HOST_ADDR)
        elif op == 0x1003:  # features: LE, no BR/EDR
            self.complete(op, ok + bytes([0, 0, 0, 0, 0x60, 0, 0, 0]))
        elif op == 0x1005:  # Read Buffer Size
            self.complete(op, ok + struct.pack("<HBHH", 1021, 0, 8, 0))
        elif op == 0x2002:  # LE Read Buffer Size
            self.complete(op, ok + struct.pack("<HB", 251, 8))
        elif op == 0x200C:  # LE Set Scan Enable
            self.scanning = p[0] == 1
            self.complete(op, ok)
        elif op == 0x200D:  # LE Create Connection
            peer = p[6:12]
            self.status(op)
            if peer == KB_ADDR and self.advertising:
                threading.Thread(target=self.connect, daemon=True).start()
        elif op == 0x200E:  # Create Connection Cancel
            self.complete(op, ok)
        elif op == 0x0406:  # Disconnect
            self.status(op)
            self.disconnect(0x16)
        elif op == 0x2019:  # LE Start Encryption
            self.status(op)
            ltk = p[12:28]
            want = self.pending_ltk or self.ltk
            if ltk == want:
                self.encrypted = True
                if self.pending_ltk:
                    self.ltk, self.pending_ltk = self.pending_ltk, None
                    self.say("paired; LTK", _r(self.ltk).hex())
                    self.event(0x08, bytes([0]) + struct.pack("<HB", HANDLE, 1))
                    # Key distribution: our identity.
                    self.acl(6, bytes([0x08]) + bytes([0x5A] * 16))
                    self.acl(6, bytes([0x09, 1]) + KB_ADDR)
                else:
                    self.say("encrypted with the stored LTK")
                    self.event(0x08, bytes([0]) + struct.pack("<HB", HANDLE, 1))
            else:
                self.say("wrong LTK", ltk.hex())
                self.event(0x08, bytes([0x06]) + struct.pack("<HB", HANDLE, 0))
        elif op == 0x2013:  # LE Connection Update
            self.status(op)
            self.event(0x3E, bytes([0x03, 0]) + struct.pack("<HHHH", HANDLE, 0x18, 0, 0x1F4))
        elif (op >> 10) == 0x3F:
            self.complete(op, bytes([0x01]))  # vendor: unknown
        else:
            self.complete(op, ok)

    def advertiser(self):
        adv = bytes([2, 1, 6, 3, 3, 0x12, 0x18, 3, 0x19, 0xC1, 0x03, len(NAME) + 1, 9]) + NAME
        while True:
            time.sleep(0.2)
            if self.scanning and self.advertising and not self.connected:
                rep = bytes([0x02, 1, 0x00, 1]) + KB_ADDR + bytes([len(adv)]) + adv + bytes([0xC4])
                self.event(0x3E, rep)

    def connect(self):
        time.sleep(0.1)
        self.connected, self.advertising, self.encrypted = True, False, False
        self.notify, self.mtu, self.smp = False, 23, {}
        self.say("connected")
        self.event(0x3E, bytes([0x01, 0]) + struct.pack("<HBB", HANDLE, 0, 1) + KB_ADDR +
                   struct.pack("<HHHB", 0x28, 0, 0x1F4, 0))

    def disconnect(self, reason):
        if not self.connected:
            return
        self.connected = False
        self.notify = False
        self.say("disconnected")
        self.event(0x05, bytes([0]) + struct.pack("<HB", HANDLE, reason))

        def readvertise():
            time.sleep(1.0)
            self.advertising = True
        threading.Thread(target=readvertise, daemon=True).start()

    # ---- L2CAP ----

    def l2cap(self, cid, p):
        if cid == 4:
            self.att(p)
        elif cid == 6:
            self.smp_pdu(p)

    # ---- SMP (peripheral, LE Secure Connections Just Works) ----

    def smp_pdu(self, p):
        s = self.smp
        code = p[0]
        if code == 0x01:
            s["preq"] = p[:7]
            s["pres"] = bytes([0x02, 0x03, 0, 0x09, 16, p[5] & 0x02, p[6] & 0x02])
            self.acl(6, s["pres"])
        elif code == 0x0C:
            pkax, pkay = int.from_bytes(p[1:33], "little"), int.from_bytes(p[33:65], "little")
            if not on_curve((pkax, pkay)):
                self.acl(6, bytes([0x05, 0x0B]))
                return
            s["pka"] = (pkax, pkay)
            s["d"] = int.from_bytes(os.urandom(32), "big") % (N - 1) + 1
            q = ec_mul(s["d"], (GX, GY))
            s["pkb"] = q
            self.acl(6, bytes([0x0C]) + q[0].to_bytes(32, "little") + q[1].to_bytes(32, "little"))
            s["nb"] = os.urandom(16)
            cb = f4(q[0].to_bytes(32, "little"), p[1:33], s["nb"], 0)
            self.acl(6, bytes([0x03]) + cb)
        elif code == 0x04:
            s["na"] = p[1:17]
            self.acl(6, bytes([0x04]) + s["nb"])
        elif code == 0x0D:
            dh = ec_mul(s["d"], s["pka"])[0].to_bytes(32, "little")
            a = HOST_ADDR + b"\x00"
            b = KB_ADDR + b"\x01"
            mac, ltk = f5(dh, s["na"], s["nb"], a, b)
            ioa = bytes([s["preq"][1], s["preq"][2], s["preq"][3]])
            iob = bytes([s["pres"][1], s["pres"][2], s["pres"][3]])
            if f6(mac, s["na"], s["nb"], bytes(16), ioa, a, b) != p[1:17]:
                self.say("DHKey check failed")
                self.acl(6, bytes([0x05, 0x0B]))
                return
            self.acl(6, bytes([0x0D]) + f6(mac, s["nb"], s["na"], bytes(16), iob, b, a))
            self.pending_ltk = ltk
        elif code in (0x08, 0x09):
            self.say("host identity", p.hex())

    # ---- ATT server ----

    def att(self, p):
        op = p[0]
        h = struct.unpack("<H", p[1:3])[0] if len(p) >= 3 else 0
        err = lambda code: self.acl(4, bytes([0x01, op]) + struct.pack("<H", h) + bytes([code]))
        attrs = self.gatt.attrs
        if op == 0x02:
            self.mtu = min(struct.unpack("<H", p[1:3])[0], 247)
            self.acl(4, bytes([0x03]) + struct.pack("<H", 247))
        elif op in (0x04, 0x08, 0x10):
            end = struct.unpack("<H", p[3:5])[0]
            want = struct.unpack("<H", p[5:7])[0] if op != 0x04 else None
            out, elen = b"", 0
            for i, (ah, ty, val, _) in enumerate(attrs):
                if ah < h or ah > end or (want is not None and ty != want):
                    continue
                if op == 0x10:
                    e = struct.pack("<HH", ah, self.gatt.group_end(i)) + val
                elif op == 0x08:
                    e = struct.pack("<H", ah) + val
                else:
                    e = struct.pack("<HH", ah, ty)
                elen = elen or len(e)
                if len(e) != elen or 2 + len(out) + len(e) > self.mtu:
                    break
                out += e
            if not out:
                return err(0x0A)
            self.acl(4, bytes([op + 1, 1 if op == 0x04 else elen]) + out)
        elif op in (0x0A, 0x0C):
            a = next((a for a in attrs if a[0] == h), None)
            if a is None:
                return err(0x01)
            if a[3] and not self.encrypted:
                return err(0x0F)
            off = struct.unpack("<H", p[3:5])[0] if op == 0x0C else 0
            self.acl(4, bytes([op + 1]) + a[2][off:off + self.mtu - 1])
        elif op in (0x12, 0x52):
            a = next((a for a in attrs if a[0] == h), None)
            if a is not None and a[3] and not self.encrypted:
                return err(0x0F) if op == 0x12 else None
            if h == self.gatt.cccd and p[3:5] == b"\x01\x00":
                self.say("notifications on")
                self.notify = True
                # One typist per subscription; an older one stops.
                self.typing += 1
                threading.Thread(target=self.type_keys, args=(self.typing,), daemon=True).start()
            elif h == self.gatt.led:
                self.say("LEDs", p[3:].hex())
            if op == 0x12:
                self.acl(4, bytes([0x13]))
        elif op in (0x1E, 0x1B):
            pass
        else:
            err(0x06)

    def type_keys(self, gen):
        time.sleep(1.5)
        while self.connected and self.notify and self.typing == gen:
            # Left Shift: a real key event that types nothing into the
            # shell the test is driving.
            for report in (bytes([0x02, 0, 0, 0, 0, 0, 0, 0]), bytes(8)):
                self.acl(4, bytes([0x1B]) + struct.pack("<H", self.gatt.report) + report)
                time.sleep(0.1)
            time.sleep(0.9)


def main():
    selftest()
    if sys.argv[1:2] == ["--selftest"]:
        print("fake-hci: crypto self-test OK")
        return
    port, log = int(sys.argv[1]), open(sys.argv[2], "w")
    deadline = time.time() + 120
    while True:
        try:
            s = socket.create_connection(("127.0.0.1", port))
            break
        except OSError:
            if time.time() > deadline:
                sys.exit("fake-hci: cannot connect")
            time.sleep(0.2)
    s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    f = Fake(s, log)
    f.say("connected to QEMU")
    try:
        f.run()
    except EOFError:
        f.say("QEMU closed the port")


if __name__ == "__main__":
    main()
