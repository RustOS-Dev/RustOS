#!/usr/bin/env python3
"""Turn an ext4 image made with `mke2fs -O fast_commit -d SEED` (where SEED
has small.txt, medium.txt, big.bin and a hard link medium.txt -> alias)
into one that needs fast-commit recovery, as if Linux had crashed after a
fast commit. The fast commit, for the transaction after the last full one:
  - creates fc-new.txt ("fast commit!\\n") in /,
  - appends a second block ("SECOND BLOCK\\n" at 4096) to small.txt and
    links it as small-link,
  - drops blocks 2-4 of big.bin (size 8192),
  - unlinks alias (medium.txt keeps one link).
Usage: fc-inject.py IMAGE"""
import struct, sys

CRC_TABLE = []
for i in range(256):
    c = i
    for _ in range(8):
        c = (c >> 1) ^ 0x82F63B78 if c & 1 else c >> 1
    CRC_TABLE.append(c)

def crc32c(crc, data):
    for b in data:
        crc = CRC_TABLE[(crc ^ b) & 0xFF] ^ (crc >> 8)
    return crc

img = open(sys.argv[1], "r+b")
def rd(off, n):
    img.seek(off)
    return img.read(n)
def wr(off, data):
    img.seek(off)
    img.write(data)

sb = bytearray(rd(1024, 1024))
bs = 1024 << struct.unpack_from("<I", sb, 24)[0]
bpg, ipg = struct.unpack_from("<I", sb, 32)[0], struct.unpack_from("<I", sb, 40)[0]
isz = struct.unpack_from("<H", sb, 88)[0]
first_ino = struct.unpack_from("<I", sb, 84)[0]
incompat = struct.unpack_from("<I", sb, 0x60)[0]
fdb = struct.unpack_from("<I", sb, 20)[0]
dsz = struct.unpack_from("<H", sb, 254)[0] if incompat & 0x80 else 32
blocks_count = struct.unpack_from("<I", sb, 4)[0]

def desc(g):
    return rd((fdb + 1) * bs + g * dsz, dsz)
def itable(g):
    d = desc(g)
    lo = struct.unpack_from("<I", d, 8)[0]
    hi = struct.unpack_from("<I", d, 0x28)[0] if dsz >= 64 else 0
    return lo | hi << 32
def ibitmap(g):
    d = desc(g)
    lo = struct.unpack_from("<I", d, 4)[0]
    hi = struct.unpack_from("<I", d, 0x24)[0] if dsz >= 64 else 0
    return lo | hi << 32
def bbitmap(g):
    d = desc(g)
    lo = struct.unpack_from("<I", d, 0)[0]
    hi = struct.unpack_from("<I", d, 0x20)[0] if dsz >= 64 else 0
    return lo | hi << 32
def inode_off(ino):
    g, i = (ino - 1) // ipg, (ino - 1) % ipg
    return itable(g) * bs + i * isz
def raw_inode(ino):
    return bytearray(rd(inode_off(ino), isz))

def extents(raw):
    """(lblk, len, pblk) of a depth-0/1 extent tree."""
    def node(b):
        magic, n, _, depth = struct.unpack_from("<HHHH", b, 0)
        assert magic == 0xF30A
        out = []
        for i in range(n):
            e = b[12 + 12 * i: 24 + 12 * i]
            if depth == 0:
                l, ln, hi, lo = struct.unpack("<IHHI", e)
                out.append((l, ln if ln <= 32768 else ln - 32768, lo | hi << 32))
            else:
                l, lo, hi, _ = struct.unpack("<IIHH", e)
                out += node(rd((lo | hi << 32) * bs, bs))
        return out
    return node(bytes(raw[40:100]))

def bmap(raw, l):
    for s, n, p in extents(raw):
        if s <= l < s + n:
            return p + l - s
    raise SystemExit("unmapped block %d" % l)

def lookup(name):
    root = raw_inode(2)
    for s, n, p in extents(root):
        for k in range(n):
            b = rd((p + k) * bs, bs)
            o = 0
            while o < bs - 8:
                ino, rec, nl = struct.unpack_from("<IHB", b, o)
                if rec < 8:
                    break
                if ino and b[o + 8:o + 8 + nl] == name:
                    return ino
                o += rec
    raise SystemExit("no " + name.decode())

# Free inode and blocks (group 0 bitmaps; high blocks of the last group).
ib = rd(ibitmap(0) * bs, bs)
new_ino = next(i + 1 for i in range(first_ino - 1, ipg) if not ib[i // 8] >> (i % 8) & 1)
last_g = (blocks_count - fdb - 1) // bpg
bb = rd(bbitmap(last_g) * bs, bs)
base = fdb + last_g * bpg
nb = min(bpg, blocks_count - base)
free = [base + i for i in range(nb - 1, 0, -1) if not bb[i // 8] >> (i % 8) & 1]
q, p = free[0], free[1]

small, medium, big = lookup(b"small.txt"), lookup(b"medium.txt"), lookup(b"big.bin")
wr(q * bs, b"fast commit!\n".ljust(bs, b"\0"))
wr(p * bs, b"SECOND BLOCK\n".ljust(bs, b"\0"))

def tlv(tag, val):
    return struct.pack("<HH", tag, len(val)) + val
def add_range(ino, l, n, pb):
    return tlv(1, struct.pack("<I", ino) + struct.pack("<IHHI", l, n, pb >> 32, pb & 0xFFFFFFFF))
def dentry(tag, parent, ino, name):
    return tlv(tag, struct.pack("<II", parent, ino) + name)
def inode_tag(ino, raw):
    return tlv(6, struct.pack("<I", ino) + bytes(raw))

# The journal: its superblock and the fast-commit area.
jraw = raw_inode(8)
jsb_off = bmap(jraw, 0) * bs
jsb = bytearray(rd(jsb_off, 1024))
maxlen, first, seq = struct.unpack_from(">III", jsb, 0x10)
nfc = struct.unpack_from(">I", jsb, 0x54)[0] or 256
fc_first = maxlen - nfc + 1
tid = seq

# A new regular file (extent tree filled by its ADD_RANGE).
nr = bytearray(isz)
struct.pack_into("<HHI", nr, 0, 0o100644, 0, 13)
now = 1700000000
struct.pack_into("<III", nr, 8, now, now, now)
struct.pack_into("<H", nr, 26, 1)
struct.pack_into("<I", nr, 32, 0x80000)
struct.pack_into("<HHHHI", nr, 40, 0xF30A, 0, 4, 0, 0)
if isz > 128:
    struct.pack_into("<H", nr, 128, 32)

s_raw = raw_inode(small)
struct.pack_into("<I", s_raw, 4, 4096 + 13)
struct.pack_into("<H", s_raw, 26, 2)
b_raw = raw_inode(big)
struct.pack_into("<I", b_raw, 4, 8192)
m_raw = raw_inode(medium)
struct.pack_into("<H", m_raw, 26, 1)

body = tlv(9, struct.pack("<II", 0, tid))
body += inode_tag(new_ino, nr) + add_range(new_ino, 0, 1, q) + dentry(3, 2, new_ino, b"fc-new.txt")
body += add_range(small, 1, 1, p) + dentry(4, 2, small, b"small-link")
body += tlv(2, struct.pack("<III", big, 2, 3))
body += dentry(5, 2, medium, b"alias")
body += inode_tag(small, s_raw) + inode_tag(big, b_raw) + inode_tag(medium, m_raw)
tail = tlv(8, struct.pack("<II", tid, 0))
crc = crc32c(crc32c(0, body), tail[:8])
tail = tail[:8] + struct.pack("<I", crc)
blk = (body + tail)
blk += tlv(7, b"\0" * (bs - len(blk) - 4))
assert len(blk) == bs
wr(bmap(jraw, fc_first) * bs, blk)

# The journal needs recovery (no full transaction in the log) and runs
# with fast commits.
wr(bmap(jraw, first) * bs, b"\0" * bs)
struct.pack_into(">I", jsb, 0x1C, first)
struct.pack_into(">I", jsb, 0x28, struct.unpack_from(">I", jsb, 0x28)[0] | 0x20)
wr(jsb_off, bytes(jsb))
struct.pack_into("<I", sb, 0x60, incompat | 0x4)
if struct.unpack_from("<I", sb, 0x64)[0] & 0x400:  # metadata_csum
    struct.pack_into("<I", sb, 0x3FC, crc32c(0xFFFFFFFF, sb[:0x3FC]))
wr(1024, bytes(sb))
img.close()
print("fast commit: new inode %d, blocks %d %d" % (new_ino, q, p))
