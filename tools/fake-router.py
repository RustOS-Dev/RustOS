#!/usr/bin/env python3
"""A scripted router for QEMU's `-netdev dgram` (one UDP datagram per
Ethernet frame), used by the ipv6 scenario to exercise network
configuration that QEMU's user-mode network cannot provide:

* ARP, ICMP echo, IPv6 neighbour discovery and ICMPv6 echo;
* DHCPv4 (192.168.50.10/24) with option 114 (captive-portal URI);
* router advertisements with M and O flags, a SLAAC prefix
  (2001:db8:50::/64), RDNSS, DNSSL and the captive-portal option;
* DHCPv6: stateful (IA_NA 2001:db8:50::100) and stateless replies with
  DNS servers, domain list and captive-portal URI;
* DNS (A/AAAA) for *.example.test on 192.168.50.1 and 2001:db8:50::53;
* a minimal one-request HTTP responder on port 80 (IPv4 and IPv6).

Usage: fake-router.py LISTEN_PORT QEMU_PORT
"""
import socket
import struct
import sys
import threading
import time

ROUTER_MAC = bytes.fromhex("525400aabbcc")
V4 = bytes([192, 168, 50, 1])
V4_CLIENT = bytes([192, 168, 50, 10])
LL = bytes.fromhex("fe800000000000000000000000000001")
V6 = bytes.fromhex("20010db8005000000000000000000001")
V6_DNS = bytes.fromhex("20010db8005000000000000000000053")
V6_LEASE = bytes.fromhex("20010db8005000000000000000000100")
PREFIX = bytes.fromhex("20010db8005000000000000000000000")
PORTAL4 = b"http://192.168.50.1/portal"
PORTAL_RA = b"https://portal.example.test/ra"
PORTAL6 = b"https://portal.example.test/dhcpv6"
HOSTS = {
    "v6only.example.test": [(28, V6)],
    "dual.example.test": [(1, V4), (28, V6)],
    "v4only.example.test": [(1, V4)],
}
HTTP_BODY = b"hello from the fake router\n"

listen_port, qemu_port = int(sys.argv[1]), int(sys.argv[2])
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind(("127.0.0.1", listen_port))
peer = ("127.0.0.1", qemu_port)
client_mac = None
log = open("/tmp/fake-router.log", "w", buffering=1)


def say(*a):
    print(time.strftime("%H:%M:%S"), *a, file=log)


def send(dst_mac, ethertype, payload):
    sock.sendto(dst_mac + ROUTER_MAC + struct.pack("!H", ethertype) + payload, peer)


def csum(data):
    if len(data) % 2:
        data += b"\0"
    s = sum(struct.unpack("!%dH" % (len(data) // 2), data))
    while s >> 16:
        s = (s & 0xFFFF) + (s >> 16)
    return (~s) & 0xFFFF


# ---------------------------------------------------------------- IPv4

def ipv4(src, dst, proto, payload):
    hdr = struct.pack("!BBHHHBBH4s4s", 0x45, 0, 20 + len(payload), 0, 0, 64, proto, 0, src, dst)
    hdr = hdr[:10] + struct.pack("!H", csum(hdr)) + hdr[12:]
    return hdr + payload


def udp4(src, dst, sport, dport, data):
    u = struct.pack("!HHHH", sport, dport, 8 + len(data), 0) + data
    pseudo = src + dst + struct.pack("!BBH", 0, 17, len(u))
    c = csum(pseudo + u) or 0xFFFF
    return ipv4(src, dst, 17, u[:6] + struct.pack("!H", c) + u[8:])


def dhcp4(req, mtype):
    xid = req[4:8]
    chaddr = req[28:44]
    b = struct.pack("!BBBB4sHH4s4s4s4s", 2, 1, 6, 0, xid, 0, 0, b"\0" * 4, V4_CLIENT, V4, b"\0" * 4)
    b += chaddr + b"\0" * 192 + bytes([99, 130, 83, 99])
    opts = bytes([53, 1, mtype, 54, 4]) + V4 + bytes([51, 4]) + struct.pack("!I", 3600)
    opts += bytes([1, 4, 255, 255, 255, 0, 3, 4]) + V4 + bytes([6, 4]) + V4
    opts += bytes([114, len(PORTAL4)]) + PORTAL4 + bytes([255])
    return b + opts


def handle_ipv4(frame):
    ip = frame[14:]
    ihl = (ip[0] & 15) * 4
    proto, src, dst = ip[9], ip[12:16], ip[16:20]
    p = ip[ihl:]
    if proto == 17:
        sport, dport = struct.unpack("!HH", p[:4])
        data = p[8:]
        if dport == 67:
            opts = data[240:]
            mtype = None
            i = 0
            while i < len(opts) and opts[i] != 255:
                if opts[i] == 0:
                    i += 1
                    continue
                if opts[i] == 53:
                    mtype = opts[i + 2]
                i += 2 + opts[i + 1]
            reply = {1: 2, 3: 5}.get(mtype)
            if reply:
                say("dhcp4", mtype, "->", reply)
                send(b"\xff" * 6, 0x0800, udp4(V4, b"\xff" * 4, 67, 68, dhcp4(data, reply)))
        elif dport == 53 and dst == V4:
            ans = dns_answer(data)
            if ans:
                send(frame[6:12], 0x0800, udp4(V4, src, 53, sport, ans))
    elif proto == 1 and p[0] == 8 and dst == V4:
        r = bytes([0, 0, 0, 0]) + p[4:]
        r = r[:2] + struct.pack("!H", csum(r)) + r[4:]
        send(frame[6:12], 0x0800, ipv4(V4, src, 1, r))
    elif proto == 6 and dst == V4:
        tcp_handle(frame[6:12], src, V4, p, v6=False)


# ---------------------------------------------------------------- IPv6

def ipv6(src, dst, nh, payload, hop=64):
    return struct.pack("!IHBB", 0x60000000, len(payload), nh, hop) + src + dst + payload


def l4sum6(src, dst, nh, data):
    return csum(src + dst + struct.pack("!IxxxB", len(data), nh) + data)


def icmp6(src, dst, body, hop=255):
    c = l4sum6(src, dst, 58, body)
    return ipv6(src, dst, 58, body[:2] + struct.pack("!H", c) + body[4:], hop)


def udp6(src, dst, sport, dport, data):
    u = struct.pack("!HHHH", sport, dport, 8 + len(data), 0) + data
    c = l4sum6(src, dst, 17, u) or 0xFFFF
    return ipv6(src, dst, 17, u[:6] + struct.pack("!H", c) + u[8:])


def pad8(opt_type, body):
    total = 2 + len(body)
    total += (-total) % 8
    return bytes([opt_type, total // 8]) + body + b"\0" * (total - 2 - len(body))


def router_advert():
    body = struct.pack("!BBHBBHII", 134, 0, 0, 64, 0xC0, 1800, 0, 0)
    body += pad8(1, ROUTER_MAC)
    body += struct.pack("!BBBBIII", 3, 4, 64, 0xC0, 3600, 1800, 0) + PREFIX
    body += pad8(25, b"\0\0" + struct.pack("!I", 3600) + V6_DNS)
    body += pad8(31, b"\0\0" + struct.pack("!I", 3600) + b"\x07example\x04test\x00")
    body += pad8(37, PORTAL_RA)
    return icmp6(LL, bytes.fromhex("ff020000000000000000000000000001"), body)


def dhcp6_opt(code, data):
    return struct.pack("!HH", code, len(data)) + data


def handle_dhcp6(mac, src, data):
    mtype, xid = data[0], data[1:4]
    opts = {}
    i = 4
    while i + 4 <= len(data):
        c, l = struct.unpack("!HH", data[i:i + 4])
        opts[c] = data[i + 4:i + 4 + l]
        i += 4 + l
    reply_type = {1: 2, 3: 7, 5: 7, 6: 7, 11: 7}.get(mtype)
    if not reply_type:
        return
    out = bytes([reply_type]) + xid
    out += dhcp6_opt(1, opts.get(1, b""))
    out += dhcp6_opt(2, b"\x00\x03\x00\x01" + ROUTER_MAC)
    if 3 in opts and mtype != 11:
        iaid = opts[3][:4]
        addr = dhcp6_opt(5, V6_LEASE + struct.pack("!II", 60, 120))
        out += dhcp6_opt(3, iaid + struct.pack("!II", 30, 50) + addr)
    out += dhcp6_opt(23, V6_DNS)
    out += dhcp6_opt(24, b"\x07example\x04test\x00")
    out += dhcp6_opt(103, PORTAL6)
    say("dhcp6", mtype, "->", reply_type)
    send(mac, 0x86DD, udp6(LL, src, 547, 546, out))


def handle_ipv6(frame):
    ip = frame[14:]
    nh, src, dst = ip[6], ip[8:24], ip[24:40]
    p = ip[40:]
    mac = frame[6:12]
    if nh == 58:
        t = p[0]
        if t == 133:  # router solicitation
            say("RS")
            send(mac, 0x86DD, router_advert())
        elif t == 135:  # neighbour solicitation
            target = p[8:24]
            if target in (LL, V6, V6_DNS):
                na = struct.pack("!BBHI", 136, 0, 0, 0x60000000) + target + pad8(2, ROUTER_MAC)
                reply_dst = src if src != b"\0" * 16 else bytes.fromhex("ff020000000000000000000000000001")
                send(mac, 0x86DD, icmp6(target, reply_dst, na))
        elif t == 128 and dst in (V6, LL):  # echo request
            body = bytes([129, 0, 0, 0]) + p[4:]
            send(mac, 0x86DD, icmp6(dst, src, body, 64))
    elif nh == 17:
        sport, dport = struct.unpack("!HH", p[:4])
        data = p[8:]
        if dport == 547:
            handle_dhcp6(mac, src, data)
        elif dport == 53 and dst in (V6_DNS, V6):
            ans = dns_answer(data)
            if ans:
                send(mac, 0x86DD, udp6(dst, src, 53, sport, ans))
    elif nh == 6 and dst == V6:
        tcp_handle(mac, src, V6, p, v6=True)


# ---------------------------------------------------------------- DNS

def dns_answer(q):
    qid, flags, qd = struct.unpack("!HHH", q[:6])
    i = 12
    labels = []
    while q[i]:
        labels.append(q[i + 1:i + 1 + q[i]].decode())
        i += 1 + q[i]
    name = ".".join(labels).lower()
    qtype = struct.unpack("!H", q[i + 1:i + 3])[0]
    question = q[12:i + 5]
    recs = [r for r in HOSTS.get(name, []) if r[0] == qtype]
    rcode = 0 if name in HOSTS else 3
    say("dns", name, qtype, "->", len(recs))
    out = struct.pack("!HHHHHH", qid, 0x8180 | rcode, 1, len(recs), 0, 0) + question
    for t, addr in recs:
        out += struct.pack("!HHHIH", 0xC00C, t, 1, 60, len(addr)) + addr
    return out


# ---------------------------------------------------------------- TCP

conns = {}


def tcp_send(mac, src, dst, sport, dport, seq, ack, flags, data, v6):
    hdr = struct.pack("!HHIIBBHHH", dport, sport, seq, ack, 5 << 4, flags, 65535, 0, 0) + data
    if v6:
        c = l4sum6(dst, src, 6, hdr)
        seg = hdr[:16] + struct.pack("!H", c) + hdr[18:]
        send(mac, 0x86DD, ipv6(dst, src, 6, seg))
    else:
        pseudo = dst + src + struct.pack("!BBH", 0, 6, len(hdr))
        c = csum(pseudo + hdr)
        seg = hdr[:16] + struct.pack("!H", c) + hdr[18:]
        send(mac, 0x0800, ipv4(dst, src, 6, seg))


def tcp_handle(mac, src, dst, p, v6):
    sport, dport, seq, ack, off, flags = struct.unpack("!HHIIBB", p[:14])
    data = p[(off >> 4) * 4:]
    key = (src, sport)
    if dport != 80:
        tcp_send(mac, src, dst, sport, dport, 0, seq + 1, 0x14, b"", v6)
        return
    if flags & 0x02:  # SYN
        conns[key] = {"seq": 1000, "rcv": seq + 1, "done": False}
        tcp_send(mac, src, dst, sport, dport, 1000, seq + 1, 0x12, b"", v6)
        return
    c = conns.get(key)
    if not c:
        return
    if data:
        c["rcv"] = seq + len(data)
        if b"\r\n\r\n" in data and not c["done"]:
            resp = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(HTTP_BODY) + HTTP_BODY
            tcp_send(mac, src, dst, sport, dport, c["seq"] + 1, c["rcv"], 0x19, resp, v6)
            c["seq"] += 1 + len(resp)
            c["done"] = True
            say("http", "v6" if v6 else "v4", data.split(b"\r\n")[0])
        else:
            tcp_send(mac, src, dst, sport, dport, c["seq"] + 1, c["rcv"], 0x10, b"", v6)
    if flags & 0x01:  # FIN
        tcp_send(mac, src, dst, sport, dport, c["seq"] + 1, seq + len(data) + 1, 0x10, b"", v6)
        conns.pop(key, None)


# ---------------------------------------------------------------- main

def periodic_ra():
    while True:
        time.sleep(3)
        send(b"\x33\x33\x00\x00\x00\x01", 0x86DD, router_advert())


threading.Thread(target=periodic_ra, daemon=True).start()
say("fake router listening on", listen_port, "->", qemu_port)
while True:
    frame, _ = sock.recvfrom(4096)
    if len(frame) < 14:
        continue
    et = struct.unpack("!H", frame[12:14])[0]
    try:
        if et == 0x0806 and frame[14 + 24:14 + 28] == V4 and frame[21] == 1:
            arp = struct.pack("!HHBBH", 1, 0x0800, 6, 4, 2) + ROUTER_MAC + V4 + frame[22:28] + frame[28:32]
            send(frame[6:12], 0x0806, arp)
        elif et == 0x0800:
            handle_ipv4(frame)
        elif et == 0x86DD:
            handle_ipv6(frame)
    except Exception as e:  # keep serving on malformed input
        say("error", repr(e))
