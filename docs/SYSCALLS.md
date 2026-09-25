# RustOS Syscall Reference

This document provides comprehensive documentation for all system calls available in RustOS.

## Syscall Invocation

Syscalls are invoked using the `int 0x80` interrupt from userspace or in-kernel processes.

**Register Convention (x86_64):**
- `rax`: Syscall number
- `rdi`: First argument (arg1)
- `rsi`: Second argument (arg2)
- `rdx`: Third argument (arg3)
- **Return value**: In `rax` (negative values indicate errors)

**Example (in assembly):**
```asm
mov rax, 1         ; SYS_WRITE
mov rdi, 1         ; stdout fd
mov rsi, msg       ; message pointer
mov rdx, len       ; message length
int 0x80           ; invoke syscall
```

---

## File I/O Syscalls (0-99)

These syscalls handle basic file operations and process lifecycle.

### SYS_READ (0)

Read data from a file descriptor.

**Signature:**
```c
ssize_t read(int fd, void *buf, size_t count);
```

**Arguments:**
- `rdi` (arg1): File descriptor (0 = stdin, 1 = stdout, 2 = stderr, ≥3 = VFS files)
- `rsi` (arg2): Pointer to buffer (must be writable)
- `rdx` (arg3): Number of bytes to read

**Return Value:**
- `≥0`: Number of bytes read (may be less than requested)
- `-22`: EINVAL (invalid arguments)
- `-9`: EBADF (bad file descriptor)

**Behavior:**
- Reads from keyboard input when fd=0 (stdin)
- Non-blocking: returns immediately if no input available
- Stops at newline character (`\n` or `\r`)

**Examples:**
```c
char buf[256];
ssize_t n = read(0, buf, 256);  // Read from stdin
```

**Known Limitations:**
- Cannot read from files opened via SYS_OPEN (stateless file access only)
- Only stdin (fd=0) implemented; fd ≥3 returns EBADF

---

### SYS_WRITE (1)

Write data to a file descriptor.

**Signature:**
```c
ssize_t write(int fd, const void *buf, size_t count);
```

**Arguments:**
- `rdi` (arg1): File descriptor (1 = stdout, 2 = stderr, ≥3 = VFS files)
- `rsi` (arg2): Pointer to buffer (must be readable)
- `rdx` (arg3): Number of bytes to write

**Return Value:**
- `≥0`: Number of bytes written (equal to count on success)
- `-22`: EINVAL (invalid UTF-8 or arguments)
- `-9`: EBADF (bad file descriptor)

**Behavior:**
- fd=1: Writes to framebuffer (visible output)
- fd=2: Writes to serial debug output
- Only UTF-8 text supported; invalid UTF-8 returns EINVAL

**Examples:**
```c
const char *msg = "Hello\n";
ssize_t n = write(1, msg, 6);  // Print to stdout
```

**Known Limitations:**
- Only stdout/stderr supported; fd ≥3 returns EBADF
- Non-UTF-8 data returns error instead of partial write

---

### SYS_OPEN (2)

Check if a file exists and obtain a file descriptor.

**Signature:**
```c
int open(const char *pathname);
```

**Arguments:**
- `rdi` (arg1): Pointer to null-terminated path string

**Return Value:**
- `3`: File exists (or is a built-in `/bin` command)
- `-2`: ENOENT (file not found)
- `-22`: EINVAL (null pointer or unterminated string)

**Behavior:**
- Returns fd=3 for any existing file or `/bin` command path
- Does not actually open the file (stateless operation)
- Virtual `/bin` commands are treated as existing files

**Examples:**
```c
int fd = open("/etc/hosts");     // Returns 3 if exists, -2 if not
int fd = open("/bin/echo");      // Returns 3 (built-in command exists)
```

**Known Limitations:**
- Does not allocate a file descriptor; always returns 3 or error
- Cannot open files for read/write; only existence check
- No support for file modes or flags (read-only check)

---

### SYS_CLOSE (3)

Close a file descriptor (no-op).

**Signature:**
```c
int close(int fd);
```

**Arguments:**
- `rdi` (arg1): File descriptor to close

**Return Value:**
- `0`: Always succeeds (no-op)

**Behavior:**
- Currently a no-op; always returns success
- Exists for POSIX compatibility

**Examples:**
```c
close(3);  // Returns 0 (no effect)
```

---

### SYS_EXEC (59)

Execute a program from a file path.

**Signature:**
```c
int exec(const char *pathname);
```

**Arguments:**
- `rdi` (arg1): Pointer to null-terminated path string

**Return Value:**
- On success: Does not return (process replaced)
- `-2`: ENOENT (file not found)
- `-8`: ENOEXEC (file is not a valid ELF binary)
- `-22`: EINVAL (null pointer or unterminated string)

**Behavior:**
- Loads ELF binary from specified path
- Replaces current process image
- Searches both `/bin` virtual commands and filesystem
- Returns to shell with exit code on failure

**Examples:**
```c
exec("/bin/hello");        // Execute built-in command
exec("/usr/app/myapp");    // Execute file from VFS
```

**Known Limitations:**
- No support for command-line arguments
- No environment variables passed
- Cannot execute shell scripts (ELF only)
- stdin/stdout limited to simple console I/O

---

### SYS_EXIT (60)

Terminate the current process.

**Signature:**
```c
void exit(int status);
```

**Arguments:**
- `rdi` (arg1): Exit status code (signed 64-bit)

**Return Value:**
- Never returns (exits to kernel or shell)

**Behavior:**
- Prints exit status: `[process exited with code X]`
- Terminates the process and returns control to shell
- Negative status codes are printed as-is

**Examples:**
```c
exit(0);       // Successful exit
exit(1);       // Error exit
exit(-1);      // Negative exit code
```

---

## Process & Memory Syscalls (100-199)

Not implemented yet. The process-model rework (see [ROADMAP.md](ROADMAP.md), milestone M2) will add these with Linux x86_64 syscall numbers.

**Planned syscalls may include:**
- `fork()` / `clone()` (process creation)
- `mmap()` / `munmap()` (memory mapping)
- `brk()` / `sbrk()` (heap management)
- `wait()` / `waitpid()` (process synchronization)

---

## Network Syscalls

RustOS currently has no network stack, so no network syscalls exist. The old
300–310 numbers were removed along with the `tcp-ip` submodule. The planned
BSD socket API will use the Linux x86_64 syscall numbers (`socket` = 41,
`connect` = 42, `accept` = 43, …). See [ROADMAP.md](ROADMAP.md), milestone M5.

---

## Error Codes

Negative return values indicate errors. The error code is the negative of the standard errno value:

| Error | Code | Meaning |
|-------|------|---------|
| EPERM | -1 | Operation not permitted |
| ENOENT | -2 | No such file or directory |
| ESRCH | -3 | No such process |
| EINTR | -4 | Interrupted system call |
| EIO | -5 | I/O error |
| ENXIO | -6 | No such device or address |
| E2BIG | -7 | Argument list too long |
| ENOEXEC | -8 | Exec format error |
| EBADF | -9 | Bad file descriptor |
| ECHILD | -10 | No child processes |
| EAGAIN | -11 | Resource temporarily unavailable |
| ENOMEM | -12 | Cannot allocate memory |
| EACCES | -13 | Permission denied |
| EFAULT | -14 | Bad address |
| EBUSY | -16 | Device or resource busy |
| EEXIST | -17 | File exists |
| ENODEV | -19 | No such device |
| ENOTDIR | -20 | Not a directory |
| EISDIR | -21 | Is a directory |
| EINVAL | -22 | Invalid argument |
| ENFILE | -23 | Too many open files in system |
| EMFILE | -24 | Too many open files |
| ENOTTY | -25 | Not a typewriter |
| EADDRINUSE | -98 | Address already in use |
| ECONNREFUSED | -111 | Connection refused |
| EHOSTUNREACH | -113 | No route to host |
| ENOTCONN | -107 | Socket is not connected |
| ENOSYS | -38 | Function not implemented |

---

## Syscall Table

| Number | Name | Purpose | Status |
|--------|------|---------|--------|
| 0 | read | Read from file descriptor | ✅ Implemented |
| 1 | write | Write to file descriptor | ✅ Implemented |
| 2 | open | Check file existence | ✅ Implemented |
| 3 | close | Close file descriptor | ✅ Implemented (no-op) |
| 59 | execve | Execute program | ✅ Implemented |
| 60 | exit | Exit process | ✅ Implemented |
| 100-299 | (reserved) | Process/memory management | ⏸️ Reserved |
| 300 | socket | Create socket | ✅ Implemented |
| 301 | bind | Bind socket | ✅ Implemented |
| 302 | listen | Listen for connections | ✅ Implemented |
| 303 | connect | Connect to remote | ✅ Implemented |
| 304 | accept | Accept connection | ✅ Implemented |
| 305 | send | Send data | ✅ Implemented |
| 306 | recv | Receive data | ✅ Implemented |
| 307 | setsockopt | Set socket option | ✅ Implemented |
| 308 | getsockopt | Get socket option | ✅ Implemented |
| 309 | shutdown | Shutdown socket | ✅ Implemented |
| 310 | close_socket | Close socket | ✅ Implemented |

---

## Writing Userspace Programs

Use the `rustos-rt` crate to write userspace programs that call these syscalls:

```rust
use rustos_rt::syscall::*;

fn main() {
    let msg = b"Hello from userspace!\n";
    write(FD_STDOUT, msg);
    exit(0);
}
```

The `rustos-rt` crate provides:
- `read()`, `write()`, `open()`, `close()` — File I/O
- `exec()`, `exit()` — Process control
- `socket()`, `bind()`, `listen()`, etc. — Network operations
- Proper errno handling and Rust-friendly wrappers

---

## Related Documentation

- [SHELL_COMMANDS.md](SHELL_COMMANDS.md) - Shell command reference
- [LIMITATIONS.md](LIMITATIONS.md) - Known limitations
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) - Common issues
