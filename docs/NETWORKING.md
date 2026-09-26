# Networking

RustOS has an in-tree TCP/IP stack: the kernel's network core (`src/net/`)
drives [smoltcp](https://github.com/smoltcp-rs/smoltcp) per interface and
exposes BSD sockets with Linux system-call numbers. Wired, USB and wireless
NICs all plug in through the same `NetDevice` trait.

## Architecture

```
 userland (wget, nc, ping, ip, wifi, ...)          rustos-rt::net
 ───────────────────────── syscalls ─────────────────────────────
 src/net/syscalls.rs   socket/bind/connect/listen/accept4/sendto/
                       recvfrom/sendmsg/recvmsg/shutdown/sockopts,
                       socketpair, SIOC* interface ioctls
 src/net/socket.rs     sockets = (interface, smoltcp handle) slots,
                       blocking + O_NONBLOCK + poll/select
 src/net/mod.rs        interface table, one smoltcp Interface and
                       SocketSet per NIC, DHCPv4 client, routes, ARP,
                       /proc/net/*, "netd" thread (polls on RX / timers)
 ───────────────────────── NetDevice ────────────────────────────
 virtio-net  e1000/e1000e/I21x  igc  r8169  USB ECM/NCM/RNDIS  iwlwifi
```

* **`NetDevice`** (`src/net/mod.rs`): `mac`, `mtu`, `link_up`, `speed`,
  `kind` (Ethernet / Wireless), `transmit`, `receive`, `ioctl` (driver
  specific, used for Wi-Fi), `status`, `shutdown`. Drivers queue received
  frames in an `RxQueue` from their interrupt handler and call
  `net::kick()`; the `netd` thread feeds them to smoltcp.
* **Interfaces** are named `lo`, `eth0..`, `wlan0..` on registration and
  removed again on hot-unplug (USB NICs). Every non-loopback interface
  gets an IPv6 link-local address (EUI-64), configures global IPv6
  addresses and a default route from router advertisements (SLAAC), and
  starts a DHCPv4 client.
* **DHCP** leases configure the address, default gateway and DNS servers
  (written to `/etc/resolv.conf`). A link-down/up cycle restarts DHCP.
* **Routing**: a per-interface route table plus a global default route;
  `route()` picks the interface for a destination.
* **DNS**: `rustos_rt::net::resolve` consults `/etc/hosts`, then the
  resolvers in `/etc/resolv.conf` (UDP, A records).

## Sockets

| Family / type             | Notes                                          |
|---------------------------|------------------------------------------------|
| `AF_INET` `SOCK_STREAM`   | TCP, listen/accept, shutdown, keep-alive       |
| `AF_INET` `SOCK_DGRAM`    | UDP, connected or unconnected                  |
| `AF_INET` `SOCK_RAW`/ICMP | ping (echo request/reply)                      |
| `AF_UNIX` `socketpair`    | stream pairs backed by pipes                   |

Options: `SO_REUSEADDR`, `SO_RCVTIMEO`/`SO_SNDTIMEO`, `SO_KEEPALIVE`,
`TCP_NODELAY`, `SO_ERROR`, `SO_BROADCAST`. Sockets are ordinary file
descriptors (read/write/poll/close/dup/fork).

## Configuration

At boot `/etc/rc` runs `ifup -a -q`, which applies
`/storage/etc/network.conf` (persistent) or `/etc/network.conf`:

```
# IFACE dhcp
# IFACE static ADDRESS/PREFIX [GATEWAY] [DNS...]
# IFACE down
eth0 dhcp
eth1 static 192.168.1.50/24 192.168.1.1 1.1.1.1 9.9.9.9
```

Interfaces not listed keep the kernel's automatic DHCP. Wireless networks
are joined by `wifi auto` from `wifi.conf` (see [WIFI.md](WIFI.md)).

## Tools (`nettools`)

| Command | Purpose |
|---------|---------|
| `ip addr`, `ip -br addr`, `ip link`, `ip route [add|del]` | interfaces, addresses, routes |
| `ifconfig [IFACE [ADDR netmask MASK] [up|down]]` | classic view / static setup |
| `ifup -a | IFACE` | apply `network.conf` |
| `dhcp IFACE [-t SECS]` (`dhclient`) | (re)start DHCP and wait for a lease |
| `route`, `arp` | routing table, neighbour cache |
| `ping [-c N] [-i SECS] HOST` | ICMP echo |
| `nslookup NAME` (`host`) | DNS lookup |
| `netstat [-tuln]` | sockets (`/proc/net/tcp`, `/proc/net/udp`) |
| `nc [-l] [-u] [-p PORT] [-w SECS] [HOST PORT]` | TCP/UDP client and server |
| `wget [-q] [-k] [-O FILE] [-T SECS] URL` | HTTP/1.1 and HTTPS download |
| `httpd [-p PORT] [-d DIR]` | static file server |
| `ntpdate [SERVER]` | set the clock over SNTP |
| `wifi ...` | wireless control ([WIFI.md](WIFI.md)) |

### HTTPS

`wget` speaks TLS 1.3 (embedded-tls: `TLS_AES_128_GCM_SHA256`, P-256 key
exchange, RSA and ECDSA certificates). Server certificates are verified
against the CA bundle at `/storage/etc/ssl/certs/ca-certificates.crt` or
`/etc/ssl/certs/ca-certificates.crt` (the build copies the build host's
bundle into the initramfs; set `RUSTOS_CA_BUNDLE` to choose another file):
the chain must lead to a trusted CA, be within its validity period (keep
the clock right with `ntpdate`) and name the host. `-k` /
`--no-check-certificate` skips verification. TLS 1.2-only servers are not
supported.

## Kernel interfaces

* `/proc/net/dev`, `route`, `tcp`, `udp`, `arp`, `if_addrs`, `wireless`.
* `/sys/class/net/<iface>/{address,operstate,carrier,mtu,speed,type,flags,
  ifindex,driver,statistics/*,wireless/status}`.
* ioctls on any socket: `SIOCGIFCONF`, `SIOCGIFFLAGS`/`SIOCSIFFLAGS`,
  `SIOCGIFADDR`/`SIOCSIFADDR`, `SIOCGIFNETMASK`/`SIOCSIFNETMASK`,
  `SIOCGIFHWADDR`, `SIOCGIFMTU`, `SIOCGIFINDEX`, `SIOCADDRT`/`SIOCDELRT`,
  plus RustOS extensions `SIOCRDHCP` (0x89F0), `SIOCRGATEWAY` (0x89F1),
  `SIOCRDNS` (0x89F2) and the Wi-Fi range 0x89F8–0x89FF.

## Drivers

| Driver | Hardware | Notes |
|--------|----------|-------|
| `virtio_net` | QEMU/KVM virtio-net (legacy + modern) | CI default |
| `e1000` | 82540EM (QEMU `e1000`), 82574L (`e1000e`), I217/I218/I219 (PCH LAN) | ULP/ME handoff for I219 |
| `igc` | I225/I226 2.5 GbE | |
| `r8169` | RTL8111/8168/8125 family | per-revision PHY setup |
| `cdc_ether` | USB CDC ECM, CDC NCM, RNDIS | phone tethering, dongles, hot-plug |
| `iwlwifi` | Intel AX210 / AX211 / AX201 Wi-Fi | [WIFI.md](WIFI.md) |

## Testing

`tools/run-scenarios.sh` boots QEMU and drives the shell: `network`
(virtio-net: DHCP, ping, DNS, TCP client/server via host forwarding, UDP,
loopback, static config, sysfs), `eth-e1000`, `eth-e1000e`,
`eth-virtio-net` (the `network` scenario also checks the SLAAC address),
`eth-usb` (ECM, and RNDIS when built with
`RUSTOS_USB_PREFER_RNDIS`) and `https` (private CA, TLS 1.3-only server,
trust, `-k`, host-name mismatch).

## Limitations

* IPv6: SLAAC and link-local addresses, `AF_INET6` sockets; no DHCPv6,
  and the DNS resolver only queries A records over IPv4.
* No IP forwarding/NAT, no raw `AF_PACKET`, no multicast group management.
* TLS is client-only, TLS 1.3 only, no session resumption.
