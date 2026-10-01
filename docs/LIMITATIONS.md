# RustOS Known Limitations

What RustOS does **not** do (yet), by area. Everything not listed here is
expected to work; see [ROADMAP.md](ROADMAP.md) for what was built and
[HARDWARE.md](HARDWARE.md) for which drivers have run on real machines.

## Hardware validation

* Most drivers are verified only under QEMU. The Intel AX210 Wi-Fi,
  I219/I225/RTL8168 Ethernet and CDC NCM drivers are written against
  reference drivers and specifications but have not yet run on hardware.
* No GPU acceleration, Thunderbolt management, or
  suspend/resume (S3/S0ix). Power-off (S5) and reboot are supported.
* ATAPI optical drives are detected but not usable.

## Kernel

* No real-time scheduling classes; `nice` only scales time slices; load
  balancing is by idle CPUs stealing work.
* CPU time is sampled per timer tick (4 ms), so short-lived processes
  may show no CPU time and the CPU-time clocks advance in 4 ms steps.
  `/proc/stat` always reports 0 for iowait, steal and guest time;
  "uninterruptible" (for the load average and `procs_blocked`) means a
  user thread in a non-interruptible kernel wait such as disk I/O or a
  sleeping mutex. `/proc/[pid]/stat` has no fault counts, `tty_nr` or
  memory-layout addresses (they read 0).
* TLB shootdowns flush the whole TLB; a CPU that keeps interrupts disabled
  for more than 500 ms is skipped with a warning (seen only under heavily
  overloaded emulators).
* No swap, no huge pages; writes through shared file mappings beyond the
  end of the file are not stored (Linux raises SIGBUS there). `write()`
  goes through to the filesystem (write-through, then the cached pages
  are updated) rather than dirtying the page cache.
* No kernel modules; drivers are built in.
* Signals: real-time signals do not queue (one pending instance per
  signal, like standard signals) and carry no value; no
  `rt_sigqueueinfo`/`rt_tgsigqueueinfo` or POSIX timers (`timer_create`),
  so no `SI_QUEUE`/`SI_TIMER`. Signal frames hold the 512-byte FXSAVE
  state only (the kernel does not enable XSAVE/AVX). A process-directed
  signal goes to whichever thread not blocking it returns to user mode
  first, not preferably the main thread. SIGCONT does not send the
  parent a `CLD_CONTINUED` SIGCHLD; `int3` in user mode is logged rather
  than raising SIGTRAP; no core dumps.
* Missing system calls: `inotify`, `ptrace`, System V IPC,
  namespaces/cgroups, `ITIMER_VIRTUAL`/`PROF` (see
  [SYSCALLS.md](SYSCALLS.md)).

## Filesystems and storage

* ext2/ext3/ext4 are read/write: journal replay (including fast
  commits) and journaling in data=ordered, writeback or journal mode;
  extents, metadata checksums, htree (large_dir, casefold), inline_data,
  bigalloc, meta_bg, quotas, ea_inode. Filesystems with encrypt, verity
  or an external journal mount read-only (encrypted files are not
  decrypted; verity files are not verified).
* ext4 limits: this kernel never writes fast commits (it always does full
  commits); extended attributes can be read but not set; inline files
  are converted to extents on their first change; casefolding uses the
  Unicode 14 tables (Linux uses 12.1, so names made of characters added
  since may compare differently); quota limits are not applied to files
  owned by root, and grace periods are not enforced.
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

* Isochronous transfers are used only for audio playback: no webcams,
  no USB audio capture. No USB serial adapters, printers or USB
  Wi-Fi/Bluetooth.
* UAS queues up to 8 commands with SuperSpeed bulk streams; on USB 2 it
  runs one command at a time (READ/WRITE READY).
* HID: report descriptors are parsed (keyboards incl. NKRO and media
  keys, mice, tablets/touch screens, game pads); multi-touch contacts are
  not tracked separately. Keyboard LEDs are the only output reports.

## Bluetooth

* Keyboards and mice only (LE HID over GATT, BR/EDR HID). No audio
  (A2DP/SCO), no file transfer or tethering, no legacy PIN pairing.
* BR/EDR and the Intel AX210 firmware path are untested on hardware so
  far. See [BLUETOOTH.md](BLUETOOTH.md).

## Audio

* HD Audio in legacy (non-DSP) mode only: laptops whose speakers or
  microphones sit behind an Intel SOF DSP stay silent. No HDMI/DP audio;
  the headphone jack is checked when playback starts.
* USB audio: playback only, no asynchronous (feedback endpoint) devices.
* Everything is mixed at 48 kHz 16-bit stereo; MP3 and WAV only (no
  Ogg/Vorbis, AAC or FLAC). See [AUDIO.md](AUDIO.md).

## Userland

* There is no libc; programs use `rustos-rt` (Rust) or raw system calls.
  The dynamic linker handles `DT_NEEDED` libraries and the common
  relocations but not thread-local storage, `dlopen` or lazy binding.
* The shell (`sh`) is POSIX-like (pipes, redirections, variables,
  globbing, command substitution, functions, job control) but not fully
  POSIX: integer-only arithmetic, `trap` can only ignore or restore
  signals (no handler commands), no here-strings (`<<<`).
* The service manager has no readiness notification, socket activation
  or resource limits (see [SERVICES.md](SERVICES.md)).
* Virtual consoles other than the visible one keep only their last
  32 KiB of output (replayed when shown); logins are optional and there
  is a single user (root) by default.
