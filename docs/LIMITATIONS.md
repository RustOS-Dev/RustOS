# RustOS Known Limitations and Workarounds

This document describes the known limitations of RustOS and recommended workarounds.

## Shell and Command Limitations

### No Pipe Support

**Limitation:** Commands cannot be chained with pipes (`|`).

```bash
# ✗ NOT SUPPORTED
cat file.txt | grep pattern
ps aux | grep process
```

**Impact:** Cannot combine outputs of multiple commands.

**Workaround:**
Use temporary files to connect commands:
```bash
cat file.txt > /tmp/output.txt
grep pattern /tmp/output.txt
```

**Future Work:** Pipeline support planned for Phase 7.

---

### No Output Redirection

**Limitation:** Standard output redirection (`>`, `>>`, `2>`) not supported.

```bash
# ✗ NOT SUPPORTED
echo "text" > file.txt
command > output.log
command 2> error.log
```

**Impact:** Cannot redirect or append output to files directly.

**Workaround:**
Use the `write` command instead:
```bash
write file.txt "text content"
```

For appending, must read existing content and rewrite:
```bash
# This is not ideal; read-modify-write pattern
cat existing.txt > /tmp/temp.txt
write file.txt "$(cat /tmp/temp.txt)new content"  # Still limited
```

**Future Work:** Output redirection planned.

---

### No Command Substitution

**Limitation:** Command substitution (`$(...)` or backticks) not supported.

```bash
# ✗ NOT SUPPORTED
echo $(whoami)
result=`date`
```

**Impact:** Cannot use output of one command as input to another.

**Workaround:**
Use separate commands and manual variable passing (not available in shell).

**Future Work:** Requires shell environment variable support.

---

### No Variable Expansion

**Limitation:** Shell variables (`$VAR`) not supported.

```bash
# ✗ NOT SUPPORTED
echo $HOME
echo $PATH
set VAR=value
echo $VAR
```

**Impact:** Cannot store or reuse values between commands.

**Workaround:**
Use filesystem to store temporary values:
```bash
write /tmp/myvar.txt "value"
# Later: read it back with cat
```

**Future Work:** Environment variable support planned.

---

### No Glob Expansion

**Limitation:** Wildcard expansion (`*`, `?`, `[...]`) not supported.

```bash
# ✗ NOT SUPPORTED
ls *.txt
rm test_?.log
cp dir/*.c /tmp/
```

**Impact:** Must specify exact filenames; cannot use patterns.

**Workaround:**
Use explicit filenames:
```bash
ls file1.txt file2.txt file3.txt
```

For mass operations, use shell loops (if available):
```bash
# Not available in RustOS shell yet
```

**Future Work:** Glob support in Phase 7+.

---

### No Job Control

**Limitation:** Background processes (`&`), job listing (`jobs`), and foreground/background management not supported.

```bash
# ✗ NOT SUPPORTED
command &
jobs
fg %1
bg %2
```

**Impact:** All commands run synchronously; cannot run multiple tasks concurrently in shell.

**Workaround:**
Write separate system code to handle concurrency (kernel-level multitasking works).

**Future Work:** Job control may be added in future phases.

---

### Limited Argument Handling

**Limitation:** No support for:
- Tilde expansion (`~` for home directory)
- Brace expansion (`{a,b,c}`)
- Word splitting on special characters

```bash
# ✗ NOT SUPPORTED
cd ~
cp ~/file.txt /tmp/
mkdir -p /dir{1,2,3}
```

**Impact:** Must use explicit paths and cannot abbreviate directories.

**Workaround:**
Use absolute paths:
```bash
cp /root/file.txt /tmp/
```

---

## Filesystem Limitations

### FAT32 Filename Length Restriction

**Limitation:** FAT32 write operations limited to 8.3 DOS naming (8 characters + 3-character extension).

```bash
# ✓ WORKS
write /mnt/readme.txt "content"
write /mnt/myfile.doc "content"

# ✗ FAILS
write /mnt/verylongfilename.txt "content"  # Filename too long
write /mnt/file.markdown "content"         # Extension too long (>3 chars)
```

**Impact:**
- Cannot create files with long names on FAT32
- Cannot use modern file extensions (.markdown, .config, etc.)
- RamFS supports longer names (255 characters)

**Workaround:**
1. Use RamFS (in-memory):
   ```bash
   write /test_very_long_filename.txt "content"  # Works in root RamFS
   ```

2. Shorten filenames:
   ```bash
   write /mnt/readme.txt "content"
   write /mnt/config.ini "content"
   ```

3. Use Windows short name aliases:
   - Windows may create hidden .LFN entries
   - Not reliably supported across systems

**Limitation Details:**
- Base name: Max 8 characters (0-9, A-Z, no spaces/special chars)
- Extension: Max 3 characters
- Valid characters: A-Z (uppercase), 0-9, underscore (_), hyphen (-)
- Invalid characters: spaces, dots (except final separator), special symbols

**Future Work:** Long File Name (LFN) write support planned.

---

### Read-Only FAT32 LFN Support

**Limitation:** Long File Names (LFN) can be read but not created.

**Impact:**
- Can read files with names >8.3 created on other systems
- Cannot create such files from RustOS
- Files created in RustOS are always 8.3 DOS names

**Workaround:**
Create files with short names from RustOS, rename on other systems if needed.

**Future Work:** LFN write support in Phase 6+.

---

### No Symlink Support

**Limitation:** Symbolic links not supported.

```bash
# ✗ NOT SUPPORTED
ln -s target linkname
```

**Impact:** Cannot create logical references to files/directories.

**Workaround:**
Use actual files/directories or copy files to multiple locations.

**Future Work:** Symlink support depends on filesystem layer enhancement.

---

### Limited Directory Operations

**Limitation:** No recursive directory copying without explicit support.

```bash
# ✓ SUPPORTED
cp -r dir1 dir2  # Works with -r flag

# ✗ NOT GUARANTEED
rm dir/subdir/  # Only if empty; -r required for non-empty
```

**Impact:** Must explicitly use `-r` for recursive operations.

**Workaround:**
Always use appropriate flags:
```bash
cp -r sourcedir destdir
rm -rf olddir
```

---

### No File Permissions/Ownership

**Limitation:** File permissions (rwx) and ownership (user:group) not implemented.

```bash
# ✗ NOT SUPPORTED
chmod 755 file
chown user:group file
```

**Impact:**
- All files owned by "root"
- All files readable/writable (permission checks not enforced)
- No security isolation between users (single-user system anyway)

**Workaround:**
Not applicable; RustOS is single-user with no permission model.

**Future Work:** Not planned (single-user OS).

---

## Hardware and Networking Limitations

### No Hot-Plug USB Support

**Limitation:** USB devices must be inserted before boot; hot-plugging not supported.

```bash
# ✗ NOT SUPPORTED
# Insert USB drive while RustOS is running
# (Device will not be detected)
```

**Impact:**
- Must have all USB devices connected before booting
- Cannot add/remove USB drives during operation
- `usbscan` does not detect newly inserted drives

**Workaround:**
Insert all USB drives before booting RustOS.

**Future Work:** Hot-plug support planned for Phase 7.

---

### No Networking

**Limitation:** RustOS has no network stack and no network drivers, wired or
wireless. The earlier AX210 WiFi integration via the external `tcp-ip`
submodule was removed.

**Future Work:** An in-tree network stack, wired NIC drivers (virtio-net,
e1000/e1000e/I219, igc, r8169) and Intel AX210 WiFi with WPA2/WPA3 are
planned. See [ROADMAP.md](ROADMAP.md), milestones M5–M7.

---

## Process and Memory Limitations

### No Process Creation from Shell

**Limitation:** Cannot create child processes; only `exec` which replaces current process.

```bash
# ✗ NOT SUPPORTED
background_command &

# ✓ SUPPORTED (replaces shell)
exec /usr/bin/hello
```

**Impact:**
- Cannot run multiple programs concurrently from shell
- Kernel can create processes internally
- Background services must be compiled into kernel

**Workaround:**
Use `exec` to replace the shell (loses shell state).

**Future Work:** `fork` syscall implementation.

---

### Limited Heap Memory (16 MiB)

**Limitation:** Total heap memory limited to 16 MiB for all operations.

**Impact:**
- Cannot load files larger than available heap
- Multiple large operations may fail
- Memory leaks will cause eventual failure

**Workaround:**
1. Check available memory:
   ```bash
   meminfo
   ```

2. Process large files in chunks (not available in shell)
3. Reboot to free memory:
   ```bash
   reboot
   ```

4. Delete large files to free space:
   ```bash
   rm largefile.bin
   ```

**Future Work:**
- Increase heap size (hardware-dependent)
- Implement paging/virtual memory (advanced)

---

### No Memory Protection

**Limitation:** No memory protection or isolation; all code runs in kernel space.

**Impact:**
- Buggy userspace programs can crash kernel
- No privilege separation
- Buffer overflows can corrupt entire system

**Workaround:**
1. Use simple, well-tested programs
2. Be careful with file operations
3. Avoid programs with known issues

**Future Work:**
- Ring 3 userspace with paging
- Memory protection via MMU

---

## Development and Build Limitations

### Rust Nightly Required

**Limitation:** Requires Rust nightly toolchain; not stable-compatible.

```bash
# ✓ REQUIRED
rustup default nightly
cargo +nightly build

# ✗ WON'T WORK
cargo build  # (with stable toolchain)
```

**Impact:**
- Build may break with new nightly versions
- Need to update nightly regularly
- Not suitable for stable releases yet

**Workaround:**
1. Keep Rust nightly updated:
   ```bash
   rustup update nightly
   ```

2. Pin to specific nightly if needed:
   ```bash
   rustup override set nightly-2024-01-15
   ```

**Future Work:** Move to stable Rust when features stabilize.

---

### Limited ELF Loader Capabilities

**Limitation:** ELF loader supports only basic binaries; no dynamic linking, ASLR, or advanced features.

**Supported:**
- Static-linked ELF binaries
- x86_64 architecture
- Simple relocations

**Not Supported:**
- Dynamic linking (libc, shared objects)
- Position-independent executable (PIE)
- Address space layout randomization (ASLR)
- Debugging symbols (stripped binaries only)

**Impact:**
- Userspace programs must be fully static
- Difficult to use standard C libraries
- Larger binary sizes

**Workaround:**
Use `rustos-rt` crate for Rust userspace programs:
```bash
cargo new --lib myapp
# Add rustos-rt dependency
```

**Future Work:** Dynamic linker support in Phase 7+.

---

### No stdin Redirection for Exec

**Limitation:** Programs invoked via `exec` have limited stdin access.

```bash
# ✗ NOT FULLY SUPPORTED
exec /bin/program < input.txt

# ✓ WORKS
exec /bin/program
# (Limited keyboard input available)
```

**Impact:**
- Programs cannot read from pre-existing files via stdin
- Must read files explicitly within program

**Workaround:**
Programs should use explicit file reading:
```c
FILE *f = fopen("input.txt", "r");
// Read from file instead of stdin
```

**Future Work:** Full stdin redirection in later phases.

---

## Known Bugs and Issues

### FAT32 Long File Names (LFN) Read-Only

**Status:** ✅ Working (read), ❌ Not working (write)

The FAT32 filesystem can read existing long filenames but cannot create them.

**Workaround:**
Create files with 8.3 names from RustOS.

---

### NVMe Sector Read Unimplemented

**Status:** ✅ Infrastructure ready, ❌ Sector read not implemented

NVMe support exists but actual sector reading not yet implemented.

**Impact:**
- NVMe SSDs may not be usable for storage
- USB devices work fine (XHCI)

**Workaround:**
Use USB mass storage devices for file access.

---

### Deep Path Recursion Stack Usage

**Status:** ⚠️ Not tested

FAT32 path resolution may use significant stack space for very deep directories (100+ levels).

**Impact:**
- May cause stack overflow with deeply nested paths
- Normal usage (< 20 levels) should be fine

**Workaround:**
Keep directory structures reasonably shallow.

---

## Comparison with Linux

| Feature | RustOS | Linux |
|---------|--------|-------|
| Pipes | ✗ No | ✓ Yes |
| Redirection | ✗ No | ✓ Yes |
| Variables | ✗ No | ✓ Yes |
| Permissions | ✗ No | ✓ Yes |
| Symlinks | ✗ No | ✓ Yes |
| Hot-plug | ✗ No | ✓ Yes |
| Multitasking | ✓ Yes (kernel) | ✓ Yes (both) |
| Networking | ✗ No | ✓ Full |
| ELF Loader | ✓ Basic | ✓ Full |
| Memory Protection | ✗ No | ✓ Yes |

---

## Timeline for Limitation Fixes

The phased timeline now lives in [ROADMAP.md](ROADMAP.md).

---

## Contributing Fixes

Community contributions welcome! Areas needing work:

1. **Easy:** Documentation, examples, tests
2. **Medium:** Shell improvements, additional drivers
3. **Hard:** Memory protection, process isolation, IPv6

See [DEVELOPMENT.md](DEVELOPMENT.md) for contribution guidelines.

---

## Related Documentation

- [SHELL_COMMANDS.md](SHELL_COMMANDS.md) - Supported commands
- [SYSCALLS.md](SYSCALLS.md) - Available syscalls
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) - Common issues
- [DEVELOPMENT.md](DEVELOPMENT.md) - Development guide
