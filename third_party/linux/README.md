# Linux sources for LinuxKPI

Files here are copied unmodified from Linux (the tag and commit are in
`VERSION`) by `tools/linux-import.py`, which also records each file's SPDX
licence and sha256 in `MANIFEST`. Untagged files are GPL-2.0-only, per
Linux's `COPYING`. RustOS is GPL-2.0-or-later, so a kernel built with these
files is distributed under GPLv2.

- `generated/`: Kconfig output (`autoconf.h`, `dot-config`) for
  `src/linuxkpi/configs/rustos.config`, plus `bounds.h`, `rq-offsets.h`,
  `asm-offsets.h`, `timeconst.h`, `cpufeaturemasks.h` and the syscall/uapi
  wrappers, generated the way Linux's `make prepare` does.
- Everything else keeps its Linux path. Which `.c` files are compiled is
  set by `src/linuxkpi/groups/*.list` and the Cargo features in
  `build/linuxkpi.rs`.

Never edit these files in place. Changes go in `PATCHES/` as numbered
patches. `tools/linux-import.py check` fails if a file differs from its
`MANIFEST` hash.

Updating (with a Linux checkout at the new tag):

    tools/linux-import.py --linux ~/src/linux config
    tools/linux-import.py --linux ~/src/linux import proof e1000 ...
    tools/linux-import.py check
