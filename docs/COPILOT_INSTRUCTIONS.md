# Guide for AI coding assistants

Read these first: [ARCHITECTURE.md](ARCHITECTURE.md) (structure),
[DEVELOPMENT.md](DEVELOPMENT.md) (build, test, conventions),
[LIMITATIONS.md](LIMITATIONS.md) (what does not exist yet),
[ROADMAP.md](ROADMAP.md) (plan and status).

## Facts that are easy to get wrong

* The kernel is a `no_std` Rust PIE for the custom soft-float target
  `x86_64-rustos.json`; nightly toolchain pinned in `rust-toolchain.toml`.
  No SSE/AVX in kernel or userland code (crypto crates use portable
  backends).
* Boot is UEFI only (bootloader 0.11 + OVMF in QEMU); there is no BIOS
  path, no `bootimage`, no VGA text mode.
* User programs run in ring 3 with the Linux x86-64 system-call ABI
  (`syscall`, Linux numbers, `-errno` results). The shell and commands are
  separate programs in `userland/` using `crates/rustos-rt`; there is no
  in-kernel shell and no libc.
* SMP is on: several CPUs run the scheduler concurrently. Use proper
  locks; do not assume disabling interrupts gives exclusivity; do not spin
  for long with interrupts off.
* Networking is in-tree (`src/net/`, smoltcp); there is no `tcp-ip`
  submodule anymore. Wi-Fi is `src/drivers/wifi/iwlwifi` + `crates/wlan`.
* Filesystems: FAT (rw), ext2 (rw), ext3/4 (ro), tmpfs root from the
  initramfs; persistent data on the `RUSTOS_ROOT` partition at `/storage`.

## Working on the code

* Put pure logic in host-testable crates (`crates/*`) with unit tests.
* Match surrounding style: small modules with a `//!` header explaining
  the hardware/protocol, constants named after the specification.
* Validate with `cargo fmt --check`, `cargo clippy -- -D warnings`, host
  crate tests, `cargo test`, and the relevant `tests/scenarios/*`
  (`tools/run-scenarios.sh`). Add a scenario for user-visible behaviour.
* Keep docs truthful: update LIMITATIONS/HARDWARE when support changes and
  never mark hardware as tested unless it ran on that hardware.
