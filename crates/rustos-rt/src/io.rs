//! Standard streams and buffered output.

use crate::sys::{self, nr};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::fmt;

pub const STDIN: i32 = 0;
pub const STDOUT: i32 = 1;
pub const STDERR: i32 = 2;

pub fn read(fd: i32, buf: &mut [u8]) -> crate::Result<usize> {
    sys::check(sys::syscall(
        nr::READ,
        &[fd as usize, buf.as_mut_ptr() as usize, buf.len()],
    ))
}

pub fn write(fd: i32, buf: &[u8]) -> crate::Result<usize> {
    sys::check(sys::syscall(
        nr::WRITE,
        &[fd as usize, buf.as_ptr() as usize, buf.len()],
    ))
}

/// Write the whole buffer (retrying short writes and EINTR).
pub fn write_all(fd: i32, mut buf: &[u8]) -> crate::Result<()> {
    while !buf.is_empty() {
        match write(fd, buf) {
            Ok(0) => return Err(crate::Error(5)),
            Ok(n) => buf = &buf[n..],
            Err(crate::Error(4)) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub trait Read {
    fn read(&mut self, buf: &mut [u8]) -> crate::Result<usize>;

    fn read_to_end(&mut self, out: &mut Vec<u8>) -> crate::Result<usize> {
        let mut buf = [0u8; 4096];
        let mut total = 0;
        loop {
            match self.read(&mut buf) {
                Ok(0) => return Ok(total),
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    total += n;
                }
                Err(crate::Error(4)) => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn read_to_string(&mut self, out: &mut String) -> crate::Result<usize> {
        let mut v = Vec::new();
        let n = self.read_to_end(&mut v)?;
        out.push_str(&String::from_utf8_lossy(&v));
        Ok(n)
    }
}

pub trait Write {
    fn write(&mut self, buf: &[u8]) -> crate::Result<usize>;
    fn write_all(&mut self, mut buf: &[u8]) -> crate::Result<()> {
        while !buf.is_empty() {
            let n = self.write(buf)?;
            if n == 0 {
                return Err(crate::Error(5));
            }
            buf = &buf[n..];
        }
        Ok(())
    }
    fn flush(&mut self) -> crate::Result<()> {
        Ok(())
    }
}

struct OutBuf {
    buf: UnsafeCell<Vec<u8>>,
}
unsafe impl Sync for OutBuf {}

static STDOUT_BUF: OutBuf = OutBuf {
    buf: UnsafeCell::new(Vec::new()),
};

/// Whether stdout is a terminal (line-buffered) or not (fully buffered).
static mut STDOUT_TTY: Option<bool> = None;

pub fn isatty(fd: i32) -> bool {
    let mut t = [0u8; 64];
    sys::syscall(nr::IOCTL, &[fd as usize, 0x5401, t.as_mut_ptr() as usize]) == 0
}

fn stdout_is_tty() -> bool {
    unsafe {
        match STDOUT_TTY {
            Some(v) => v,
            None => {
                let v = isatty(STDOUT);
                STDOUT_TTY = Some(v);
                v
            }
        }
    }
}

/// Flush buffered stdout.
pub fn flush() {
    let b = unsafe { &mut *STDOUT_BUF.buf.get() };
    if !b.is_empty() {
        let _ = write_all(STDOUT, b);
        b.clear();
    }
}

/// Forget buffered output (used in a forked child before exec).
pub fn discard_buffered() {
    unsafe { (*STDOUT_BUF.buf.get()).clear() };
}

fn stdout_write(data: &[u8]) {
    let b = unsafe { &mut *STDOUT_BUF.buf.get() };
    b.extend_from_slice(data);
    if b.len() >= 8192 || (stdout_is_tty() && data.contains(&b'\n')) {
        flush();
    }
}

pub struct Stdout;
pub struct Stderr;
pub struct Stdin;

impl fmt::Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        stdout_write(s.as_bytes());
        Ok(())
    }
}

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> crate::Result<usize> {
        stdout_write(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> crate::Result<()> {
        flush();
        Ok(())
    }
}

impl fmt::Write for Stderr {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        flush();
        write_all(STDERR, s.as_bytes()).map_err(|_| fmt::Error)
    }
}

impl Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> crate::Result<usize> {
        flush();
        read(STDIN, buf)
    }
}

pub fn stdout() -> Stdout {
    Stdout
}

pub fn stdin() -> Stdin {
    Stdin
}

pub fn write_fmt_fd(fd: i32, args: fmt::Arguments) -> fmt::Result {
    use fmt::Write as _;
    if fd == STDOUT {
        Stdout.write_fmt(args)
    } else {
        struct Fd(i32);
        impl fmt::Write for Fd {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                write_all(self.0, s.as_bytes()).map_err(|_| fmt::Error)
            }
        }
        flush();
        Fd(fd).write_fmt(args)
    }
}

/// Read one line from stdin (without the newline). Returns None at EOF.
pub fn read_line() -> Option<String> {
    flush();
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match read(STDIN, &mut b) {
            Ok(0) => {
                return if line.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(&line).into_owned())
                };
            }
            Ok(_) => {
                if b[0] == b'\n' {
                    return Some(String::from_utf8_lossy(&line).into_owned());
                }
                line.push(b[0]);
            }
            Err(crate::Error(4)) => {}
            Err(_) => return None,
        }
    }
}

/// Buffered line reader over any file descriptor.
pub struct LineReader {
    fd: i32,
    buf: Vec<u8>,
    eof: bool,
}

impl LineReader {
    pub fn new(fd: i32) -> LineReader {
        LineReader {
            fd,
            buf: Vec::new(),
            eof: false,
        }
    }

    /// Next line including no trailing newline; None at EOF.
    pub fn next_line(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                return Some(line[..line.len() - 1].to_vec());
            }
            if self.eof {
                if self.buf.is_empty() {
                    return None;
                }
                return Some(core::mem::take(&mut self.buf));
            }
            let mut tmp = [0u8; 4096];
            match read(self.fd, &mut tmp) {
                Ok(0) => self.eof = true,
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(crate::Error(4)) => {}
                Err(_) => self.eof = true,
            }
        }
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{ let _ = $crate::io::write_fmt_fd(1, format_args!($($arg)*)); }};
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => {{ let _ = $crate::io::write_fmt_fd(1, format_args!("{}\n", format_args!($($arg)*))); }};
}

#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => {{ let _ = $crate::io::write_fmt_fd(2, format_args!($($arg)*)); }};
}

#[macro_export]
macro_rules! eprintln {
    () => ($crate::eprint!("\n"));
    ($($arg:tt)*) => {{ let _ = $crate::io::write_fmt_fd(2, format_args!("{}\n", format_args!($($arg)*))); }};
}
