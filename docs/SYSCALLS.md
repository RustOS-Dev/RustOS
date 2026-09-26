# RustOS System Calls

RustOS uses the **Linux x86-64 system-call ABI**: the same numbers,
argument registers and error convention, so small Linux-style programs
(including libc-free C built with `-nostdlib`) run unchanged. Userland in
this repository uses the wrappers in `crates/rustos-rt`.

## Calling convention

* Instruction: `syscall` (preferred) or `int 0x80` (same numbers).
* Number in `rax`; arguments in `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`.
* Result in `rax`; errors are returned as `-errno` (values from
  `src/errno.rs`, identical to Linux).
* `rcx` and `r11` are clobbered by `syscall`.
* Blocking calls are interruptible by signals (`EINTR`, with
  `SA_RESTART` restarting where Linux does).

```asm
mov rax, 1          ; write
mov rdi, 1          ; fd
lea rsi, [rel msg]  ; buffer
mov rdx, len
syscall
```

## Process start-up

`execve` builds the System V initial stack: `argc`, `argv[]`, `envp[]`,
and an auxiliary vector with `AT_PHDR`, `AT_PHENT`, `AT_PHNUM`,
`AT_PAGESZ`, `AT_BASE`, `AT_ENTRY`, `AT_UID`/`AT_EUID`/`AT_GID`/`AT_EGID`,
`AT_SECURE`, `AT_RANDOM` and `AT_EXECFN`. Static, static-PIE and
dynamically linked (`PT_INTERP` → `/lib/ld-rustos.so.1`) ELF64 programs and
`#!` scripts are accepted.

## Implemented calls


### Files and descriptors

| # | Name |
|---|------|
| 0 | `read` |
| 1 | `write` |
| 2 | `open` |
| 3 | `close` |
| 7 | `poll` |
| 8 | `lseek` |
| 16 | `ioctl` |
| 17 | `pread64` |
| 18 | `pwrite64` |
| 19 | `readv` |
| 20 | `writev` |
| 22 | `pipe` |
| 23 | `select` |
| 32 | `dup` |
| 33 | `dup2` |
| 40 | `sendfile` |
| 72 | `fcntl` |
| 73 | `flock` |
| 74 | `fsync` |
| 75 | `fdatasync` |
| 76 | `truncate` |
| 77 | `ftruncate` |
| 85 | `creat` |
| 162 | `sync` |
| 257 | `openat` |
| 270 | `pselect6` |
| 271 | `ppoll` |
| 292 | `dup3` |
| 293 | `pipe2` |

### Metadata and directories

| # | Name |
|---|------|
| 4 | `stat` |
| 5 | `fstat` |
| 6 | `lstat` |
| 21 | `access` |
| 78 | `getdents` |
| 79 | `getcwd` |
| 80 | `chdir` |
| 81 | `fchdir` |
| 82 | `rename` |
| 83 | `mkdir` |
| 84 | `rmdir` |
| 86 | `link` |
| 87 | `unlink` |
| 88 | `symlink` |
| 89 | `readlink` |
| 90 | `chmod` |
| 91 | `fchmod` |
| 92 | `chown` |
| 93 | `fchown` |
| 94 | `lchown` |
| 95 | `umask` |
| 133 | `mknod` |
| 137 | `statfs` |
| 138 | `fstatfs` |
| 165 | `mount` |
| 166 | `umount2` |
| 217 | `getdents64` |
| 235 | `utimes` |
| 258 | `mkdirat` |
| 260 | `fchownat` |
| 262 | `newfstatat` |
| 263 | `unlinkat` |
| 264 | `renameat` |
| 265 | `linkat` |
| 266 | `symlinkat` |
| 267 | `readlinkat` |
| 268 | `fchmodat` |
| 269 | `faccessat` |
| 280 | `utimensat` |
| 316 | `renameat2` |
| 332 | `statx` |

### Memory

| # | Name |
|---|------|
| 9 | `mmap` |
| 10 | `mprotect` |
| 11 | `munmap` |
| 12 | `brk` |
| 28 | `madvise` |

### Processes and threads

| # | Name |
|---|------|
| 24 | `sched_yield` |
| 39 | `getpid` |
| 56 | `clone` |
| 57 | `fork` |
| 58 | `vfork` |
| 59 | `execve` |
| 60 | `exit` |
| 61 | `wait4` |
| 97 | `getrlimit` |
| 98 | `getrusage` |
| 100 | `times` |
| 102 | `getuid` |
| 104 | `getgid` |
| 105 | `setuid` |
| 106 | `setgid` |
| 107 | `geteuid` |
| 108 | `getegid` |
| 109 | `setpgid` |
| 110 | `getppid` |
| 111 | `getpgrp` |
| 112 | `setsid` |
| 115 | `getgroups` |
| 121 | `getpgid` |
| 124 | `getsid` |
| 157 | `prctl` |
| 158 | `arch_prctl` |
| 186 | `gettid` |
| 202 | `futex` |
| 218 | `set_tid_address` |
| 231 | `exit_group` |
| 273 | `set_robust_list` |
| 302 | `prlimit64` |

### Signals

| # | Name |
|---|------|
| 13 | `rt_sigaction` |
| 14 | `rt_sigprocmask` |
| 15 | `rt_sigreturn` |
| 34 | `pause` |
| 37 | `alarm` |
| 62 | `kill` |
| 127 | `rt_sigpending` |
| 130 | `rt_sigsuspend` |
| 131 | `sigaltstack` |
| 200 | `tkill` |
| 234 | `tgkill` |

### Time

| # | Name |
|---|------|
| 35 | `nanosleep` |
| 96 | `gettimeofday` |
| 164 | `settimeofday` |
| 201 | `time` |
| 227 | `clock_settime` |
| 228 | `clock_gettime` |
| 229 | `clock_getres` |
| 230 | `clock_nanosleep` |

### Sockets

| # | Name |
|---|------|
| 41 | `socket` |
| 42 | `connect` |
| 43 | `accept` |
| 44 | `sendto` |
| 45 | `recvfrom` |
| 46 | `sendmsg` |
| 47 | `recvmsg` |
| 48 | `shutdown` |
| 49 | `bind` |
| 50 | `listen` |
| 51 | `getsockname` |
| 52 | `getpeername` |
| 53 | `socketpair` |
| 54 | `setsockopt` |
| 55 | `getsockopt` |
| 288 | `accept4` |

### System

| # | Name |
|---|------|
| 63 | `uname` |
| 99 | `sysinfo` |
| 103 | `syslog` |
| 169 | `reboot` |
| 170 | `sethostname` |
| 318 | `getrandom` |

## RustOS-specific interfaces

These use standard system calls with RustOS-defined requests:

| Interface | Request | Purpose |
|-----------|---------|---------|
| `ioctl(sock, 0x89F0, ifreq)` | `SIOCRDHCP` | start (1) / stop (0) DHCP on an interface |
| `ioctl(sock, 0x89F1, ifreq)` | `SIOCRGATEWAY` | set the default gateway |
| `ioctl(sock, 0x89F2, ifreq)` | `SIOCRDNS` | set DNS servers |
| `ioctl(sock, 0x89F8..0x89FF, ifreq)` | `SIOCRWIFI` | Wi-Fi status, scan, results, connect, disconnect (see [WIFI.md](WIFI.md)) |
| `reboot(magic1, magic2, cmd)` | `LINUX_REBOOT_CMD_POWER_OFF` / `RESTART` | ACPI power-off / reset |

## Not implemented

Calls outside this list return `-ENOSYS`. Notable gaps: `epoll`,
`inotify`, `timerfd`/`eventfd`/`signalfd`, `io_uring`, shared-memory IPC
(`shmget`, `memfd_create`), `ptrace`, namespaces and cgroups, `setitimer`,
thread-local storage relocations in the dynamic linker.
