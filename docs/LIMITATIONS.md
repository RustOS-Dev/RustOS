# RustOS Known Limitations

What RustOS does **not** do (yet), by area. Everything not listed here is
expected to work; see [ROADMAP.md](ROADMAP.md) for what was built and
[HARDWARE.md](HARDWARE.md) for which drivers have run on real machines.

## Hardware validation

* Most drivers are verified only under QEMU. The Intel AX210 Wi-Fi,
  I219/I225/RTL8168 Ethernet and CDC NCM drivers are written against
  reference drivers and specifications but have not yet run on hardware.
* No GPU acceleration, audio, Bluetooth, Thunderbolt management, or
  suspend/resume (S3/S0ix). Power-off (S5) and reboot are supported.
* ATAPI optical drives are detected but not usable.

## Kernel

* No real-time scheduling classes; `nice` only scales time slices; load
  balancing is by idle CPUs stealing work.
* TLB shootdowns flush the whole TLB; a CPU that keeps interrupts disabled
  for more than 500 ms is skipped with a warning (seen only under heavily
  overloaded emulators).
* No swap, no huge pages; writes through shared file mappings beyond the
  end of the file are not stored (Linux raises SIGBUS there). The page
  cache serves mappings only; `read`/`write` go to the filesystem.
* No kernel modules; drivers are built in.
* Missing system calls: `inotify`, `ptrace`, System V IPC,
  namespaces/cgroups, `ITIMER_VIRTUAL`/`PROF` (see
  [SYSCALLS.md](SYSCALLS.md)). poll/epoll waiters share one wake-up
  queue (sources that do not notify are re-checked every 50 ms).

## Filesystems and storage

* ext2/ext3/ext4 are read/write (journal replay, metadata journaling in
  ordered mode, extents, metadata checksums, htree inserts). ext4 images
  with inline_data, bigalloc, quotas, meta_bg or an external journal are
  mounted read-only (the kernel log names the features); directory
  lookups scan linearly instead of using the htree index; only metadata
  is journaled (no `data=journal`).
* FAT has no Unix permissions (files appear as root-owned 0755/0644);
  timestamps have 2-second resolution.
* No NTFS, exFAT, btrfs, XFS; no software RAID or LVM; no disk
  encryption.
* The root filesystem is a tmpfs populated from the initramfs; persistent
  data lives on the `RUSTOS_ROOT` storage partition mounted at `/storage`.

## Networking

* IPv6: no privacy (temporary) addresses, no DHCPv6 prefix delegation,
  no multicast group management (MLD) beyond what SLAAC needs.
* No IP forwarding, NAT, firewall, VLANs, bridges or `AF_PACKET` sockets.
* TLS: client only (TLS 1.2 and 1.3, ECDHE with AES-GCM or
  ChaCha20-Poly1305); no session resumption, client certificates, OCSP or
  revocation checks. HTTPS needs a CA bundle (shipped from the build host)
  and a correct clock.
* Captive portals are detected, but pages that need JavaScript to log in
  cannot be used from the text browser.

## Wi-Fi

* Station mode only; open, WPA2-Personal and WPA3-Personal networks.
  No WPA-Enterprise, WEP or TKIP-only networks, AP/monitor/P2P modes.
* 802.11n/ac/ax with A-MPDU aggregation is implemented but unverified on
  hardware (fallbacks: `iwlwifi.mode=`, `iwlwifi.agg=0` in kernel.conf).
  A-MSDUs are received, not sent. No 6 GHz, no power save.
* Intel AX210/AX211/AX201 only, and the firmware files must be installed
  (see [WIFI.md](WIFI.md)).

## USB

* No isochronous transfers (webcams, audio), no USB serial adapters,
  printers or USB Wi-Fi/Bluetooth.
* UAS needs SuperSpeed bulk streams (otherwise Bulk-Only is used when the
  device offers it) and keeps one command in flight.
* HID: report descriptors are parsed (keyboards incl. NKRO and media
  keys, mice, tablets/touch screens, game pads); multi-touch contacts are
  not tracked separately, and `/dev/input/event0` merges all devices and
  has no EVIOCG* ioctls; no output reports (keyboard LEDs).

## Userland

* There is no libc; programs use `rustos-rt` (Rust) or raw system calls.
  The dynamic linker handles `DT_NEEDED` libraries and the common
  relocations but not thread-local storage, `dlopen` or lazy binding.
* The shell (`sh`) is POSIX-like (pipes, redirections, variables,
  globbing, command substitution, functions, job control) but not fully
  POSIX: integer-only arithmetic, `trap` can only ignore or restore
  signals (no handler commands), no here-strings (`<<<`).
* Virtual consoles other than the visible one keep only their last
  32 KiB of output (replayed when shown); logins are optional and there
  is a single user (root) by default.
