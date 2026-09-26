# Developing RustOS

## Toolchain

`rust-toolchain.toml` pins the nightly toolchain (with `rust-src`,
`rustfmt`, `clippy`, `llvm-tools` and the `x86_64-unknown-none` target);
rustup installs it on first use. The kernel builds for the custom target
`x86_64-rustos.json` (soft-float, no SSE, PIE) with `build-std`
(`.cargo/config.toml`).

Host packages: `qemu-system-x86` and `ovmf` to run; `e2fsprogs`,
`openssl`, `python3`, `gcc` and `curl` for the test scenarios.

## Building

```bash
cargo build                  # kernel + userland + initramfs (debug)
cargo build --release
cargo run                    # boot in QEMU (see run-qemu-uefi.sh)
```

What `cargo build` does (`build.rs`):

1. Builds the userland workspace (`userland/`: init, sh, rbox, nettools)
   for `x86_64-unknown-none` as static executables linked at 4 MiB
   (`userland/userland.ld`).
2. Builds the dynamic linker (`userland/ldso`, its own workspace) as a
   static PIE, and — if a host C compiler exists — the libc-free
   dynamic-linking test programs in `userland/dyntest`.
3. Packs everything, plus `userland/root/` (configuration files) and the
   host's CA bundle (`RUSTOS_CA_BUNDLE` overrides), into a cpio initramfs
   that the kernel embeds.

`cargo run` invokes `run-qemu-uefi.sh`, which turns the kernel ELF into a
GPT disk image (`crates/create-image`: EFI system partition with the
bootloader and kernel, plus a FAT32 `RUSTOS_ROOT` storage partition) and
boots it under OVMF.

Useful environment variables:

| Variable | Effect |
|----------|--------|
| `RUSTOS_SMP` | number of QEMU CPUs (default 2) |
| `RUSTOS_QEMU_ARGS` | extra QEMU arguments (devices, `-s -S` for gdb, ...) |
| `RUSTOS_SKIP_USERLAND` | build the kernel without the userland |
| `RUSTOS_NOSMP` (build time) | do not start application processors |
| `RUSTOS_USB_PREFER_RNDIS` (build time) | choose RNDIS over CDC ECM configurations (tests) |
| `RUSTOS_STRACE` (build time) | log every system call |
| `RUSTOS_SMP_DEBUG` (build time) | NMI-dump CPUs that miss a TLB shootdown |
| `RUSTOS_CA_BUNDLE` | CA bundle copied into the initramfs |

## Code map

See [ARCHITECTURE.md](ARCHITECTURE.md). Rules of thumb:

* Pure logic (protocols, parsers, crypto glue) goes into a host-testable
  crate under `crates/` with unit tests; the kernel only wires hardware
  to it (`wlan`, `usb-desc`, `fat-format`).
* Drivers use `mm::dma::DmaBuffer` for DMA memory, `PciDevice::map_bar`
  for MMIO, `enable_msix`/`enable_msi_or_intx` for interrupts, and block
  on `sched::WaitQueue`s woken from interrupt handlers, with
  `time::Deadline` timeouts.
* Never spin for long with interrupts disabled (other CPUs may wait for
  a TLB-shootdown answer); long IRQ-off loops call
  `arch::x86_64::smp::poll()`.
* Kernel locks: `spin::Mutex` for short critical sections (wrap in
  `without_interrupts` if an interrupt handler takes the same lock),
  `sched::mutex::Mutex` (sleeping) for long ones.
* User memory is only touched through `process::uaccess`.
* Userland programs use `crates/rustos-rt`; add a new applet to `rbox` or
  `nettools` and list it in `userland/install.list`.

## Testing

```bash
cargo test                                   # kernel tests in QEMU
tools/run-scenarios.sh                       # all boot scenarios
tools/run-scenarios.sh target/x86_64-rustos/debug/rustos tests/scenarios/network.txt
RUSTOS_SMP=4 tools/run-scenarios.sh          # more CPUs
for c in crates/wlan crates/usb-desc crates/fat-format; do (cd $c && cargo test); done
cargo fmt --check && cargo clippy -- -D warnings
```

A scenario (`tests/scenarios/NAME.txt` + optional `NAME.args` with QEMU
arguments) is a script for `tools/qemu-console-test.py`:

| Line | Meaning |
|------|---------|
| `send TEXT` | type a line on the serial console |
| `wait REGEX [SECS]` | wait for output matching REGEX |
| `sleep SECS` | pause |
| `monitor CMD` | QEMU monitor command (`device_add`, `sendkey`, ...) |
| `hostrun CMD` / `hostbg CMD` | run a host shell command (foreground / background) |
| `# ...` | comment |

Disk images in `.args`: `@EXT2:<MiB>[:label]@`, `@EXT4:<MiB>[:label]@`,
`@BLANK:<MiB>@` (kept with `RUSTOS_KEEP_DISKS=1`). Logs go to
`/tmp/scenario-NAME.log`.

## Debugging

* Serial output mirrors the console; `dmesg` shows the kernel log.
* `RUSTOS_QEMU_ARGS="-s -S" cargo run`, then
  `gdb target/x86_64-rustos/debug/rustos -ex 'target remote :1234'`.
* Kernel panics print the faulting address, CPU and register frame.
* `/proc/interrupts`, `/proc/threads`, `/proc/meminfo`, `/sys/class/net`
  help with driver debugging; `RUSTOS_STRACE=1 cargo build` traces system
  calls.

## Contributing

1. Branch from `main`, keep commits focused, write clear messages.
2. Run formatting, clippy, host tests and the relevant scenarios.
3. Add or extend a scenario for user-visible behaviour, host tests for
   pure logic.
4. Update the docs (ARCHITECTURE, LIMITATIONS, HARDWARE) when behaviour
   or support changes.
