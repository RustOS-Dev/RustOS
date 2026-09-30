# Porting software to RustOS

RustOS speaks the Linux x86-64 system-call ABI, so C programs built
against **musl** run unchanged, statically or dynamically linked. The
repository pins upstream musl (`third_party/musl`, v1.2.5) and builds a
sysroot from it.

## The sysroot and `rustos-cc`

```sh
git submodule update --init third_party/musl
tools/build-musl.sh            # -> target/sysroot (about 20 s; cached)
tools/rustos-cc -O2 -o hello hello.c            # dynamic
tools/rustos-cc -O2 -static -o hello hello.c    # static
```

`tools/rustos-cc` is `gcc` with a specs file (like `musl-gcc`): musl's
headers, `crt1.o`/`crti.o`/`crtn.o`, `libc`, and the dynamic linker path
`/lib/ld-musl-x86_64.so.1`. The sysroot also carries the host's Linux UAPI
headers (`linux/`, `asm/`), which musl does not ship. Every kernel build
installs musl's `libc.so` as `/lib/ld-musl-x86_64.so.1` (musl's libc is
also its dynamic linker); shared libraries are found in `/lib`,
`/usr/local/lib` and `/usr/lib`.

What works: stdio, malloc (including `mremap`), pthreads with
thread-local storage, mutexes and condition variables (futexes),
`dlopen`, signals, `fork`/`exec`/`wait`, files and directories, TCP/UDP
sockets, `poll`/`select`/`epoll`, `eventfd`/`timerfd`/`signalfd`, file
`mmap`, pseudo-terminals. See [SYSCALLS.md](SYSCALLS.md) and
[LIMITATIONS.md](LIMITATIONS.md) for what is missing (e.g. `ptrace`,
System V IPC, `inotify`).

The boot image contains a few test programs built this way:
`musl-hello`, `musl-hello-static`, `musl-threads`, `musl-dlopen` (with
`/usr/lib/libplugin.so`) and `musl-libctest` (a small libc conformance
run); the `musl` boot scenario runs them.

## Ports

Recipes live in `ports/NAME/build.sh`; `tools/install-port.sh` downloads
the pinned source (checked by SHA-256), builds it with `rustos-cc` and
stages the result in `target/ports/NAME` (laid out like `/usr/local`).

| Port | Result |
|------|--------|
| `busybox` | BusyBox 1.36.1, static (`defconfig` minus a few applets needing missing kernel features) |
| `curl` | curl 8.10.1 with mbedTLS 3.6.2, static, HTTPS against `/etc/ssl/certs/ca-certificates.crt` |
| `quickjs` | QuickJS-ng 0.16.2 (from the `rquickjs-sys` crate's vendored copy): `qjs`, `qjsc`, `run-test262`, and `libquickjs.a` + headers for embedding |

The ports named in `ports/default.list` (all three) are built by the
kernel build on first use and installed in the boot image under
`/usr/bin`; later builds reuse them until their `build.sh` changes. Set
`RUSTOS_PORTS=0` to build without them (a port that fails to build, e.g.
without network access for its sources, is left out with a warning).
Downloaded sources are kept in `target/ports/src` (or
`$RUSTOS_PORTS_CACHE`); CI caches that directory. BusyBox applets are run
as `busybox APPLET` so they do not shadow the rbox tools.

```sh
tools/install-port.sh busybox               # stage only
tools/install-port.sh --initramfs busybox   # also put it in the boot image
cargo build                                 # (then boot as usual)
```

The `musl` scenario runs a test262 subset (`tests/test262.list`, pinned
commit, fetched by `tools/fetch-test262.sh`) with `run-test262` on RustOS
and expects the same result as on the host; the known failures of this
QuickJS-ng version are listed in `tests/test262_errors.txt`.

Staged files can instead be copied to `/storage/usr/local` on a RustOS
drive. `--initramfs` adds everything in `target/ports-root` to the next
kernel build; delete that directory to drop the ports again.

A new port needs a `ports/NAME/build.sh` that takes `SRC_DIR BUILD_DIR
DEST_DIR`, uses `fetch URL SHA256 FILE` from `tools/port-lib.sh`, builds
with `CC=tools/rustos-cc` (usually static, `--host=x86_64-linux-musl` for
autoconf) and installs into `DEST_DIR/bin`, `DEST_DIR/lib`, ...

## RustOS's own dynamic linker

Rust and libc-free programs in this repository use `/lib/ld-rustos.so.1`
(`userland/ldso`) instead: eager binding, thread-local storage (static
TLS for the program and its libraries, `__tls_get_addr`), and `dlopen`/
`dlsym`/`dlclose`/`dlerror` (link against the `/lib/libdl.so` stub). The
`dynlink` scenario exercises it (`dyntest`, `tlstest`, `dltest`).
