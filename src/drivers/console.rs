//! Kernel console: routes output to the framebuffer terminal (when present),
//! COM1, and the kernel log ring buffer read by `dmesg`. Also keeps the
//! scrollback history for the framebuffer view.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts;

/// Console colour palette (the classic 16 VGA colours).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

fn to_framebuffer_color(color: Color) -> crate::drivers::framebuffer::Color {
    use crate::drivers::framebuffer::Color as FbColor;
    match color {
        Color::Black => FbColor::BLACK,
        Color::Blue | Color::LightBlue => FbColor::BLUE,
        Color::Green | Color::LightGreen => FbColor::GREEN,
        Color::Cyan | Color::LightCyan => FbColor::CYAN,
        Color::Red | Color::LightRed => FbColor::RED,
        Color::Magenta | Color::Pink => FbColor::MAGENTA,
        Color::Brown | Color::Yellow => FbColor::YELLOW,
        Color::LightGray | Color::DarkGray | Color::White => FbColor::WHITE,
    }
}

const HISTORY_MAX: usize = 1000;
static HISTORY: Mutex<VecDeque<Vec<u8>>> = Mutex::new(VecDeque::new());
static VIEW_OFFSET: AtomicUsize = AtomicUsize::new(0);

/// Record a line that scrolled off the top of the screen.
pub(crate) fn push_history(line: &[u8]) {
    if crate::allocator::heap_size() == 0 {
        return;
    }
    let mut h = HISTORY.lock();
    if h.len() >= HISTORY_MAX {
        h.pop_front();
    }
    let end = line.iter().rposition(|&c| c != b' ').map_or(0, |i| i + 1);
    h.push_back(line[..end].to_vec());
}

pub fn view_is_live() -> bool {
    VIEW_OFFSET.load(Ordering::Relaxed) == 0
}

fn redraw_view() {
    interrupts::without_interrupts(|| {
        let h = HISTORY.lock();
        let lines: Vec<&[u8]> = h.iter().map(|l| l.as_slice()).collect();
        if let Some(c) = crate::drivers::framebuffer::CONSOLE.lock().as_mut() {
            c.show_history(&lines, VIEW_OFFSET.load(Ordering::Relaxed));
        }
    });
}

/// Scroll the framebuffer view back by half a screen.
pub fn scroll_view_up() {
    let rows = crate::drivers::framebuffer::console_size().map_or(25, |s| s.1);
    let max = HISTORY.lock().len();
    let cur = VIEW_OFFSET.load(Ordering::Relaxed);
    VIEW_OFFSET.store((cur + rows / 2).min(max), Ordering::Relaxed);
    redraw_view();
}

/// Scroll the framebuffer view forward (towards the live screen).
pub fn scroll_view_down() {
    let rows = crate::drivers::framebuffer::console_size().map_or(25, |s| s.1);
    let cur = VIEW_OFFSET.load(Ordering::Relaxed);
    VIEW_OFFSET.store(cur.saturating_sub(rows / 2), Ordering::Relaxed);
    redraw_view();
}

/// Return to the live view if scrolled back.
pub fn reset_view() {
    if !view_is_live() {
        VIEW_OFFSET.store(0, Ordering::Relaxed);
        redraw_view();
    }
}

/// Write raw bytes to the framebuffer terminal and serial port (no klog).
pub fn write_bytes(bytes: &[u8]) {
    interrupts::without_interrupts(|| {
        if let Some(c) = crate::drivers::framebuffer::CONSOLE.lock().as_mut() {
            c.write_bytes(bytes);
        }
    });
    crate::drivers::serial::write_bytes(bytes);
}

/// Owner of the print lock (CPU id + 1; 0 = free). A CPU that already
/// holds it (e.g. a panic while printing) prints without waiting.
static PRINT_OWNER: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Formats a message into a buffer so it reaches every sink in one piece
/// (messages from different CPUs never interleave mid-line).
struct LineBuf {
    buf: [u8; 1024],
    len: usize,
}

impl LineBuf {
    fn flush(&mut self) {
        if self.len == 0 {
            return;
        }
        let bytes = &self.buf[..self.len];
        if let Some(c) = crate::drivers::framebuffer::CONSOLE.lock().as_mut() {
            c.write_bytes(bytes);
        }
        crate::klog::write_bytes(bytes);
        crate::drivers::serial::write_bytes(bytes);
        self.len = 0;
    }
}

impl fmt::Write for LineBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut b = s.as_bytes();
        while !b.is_empty() {
            if self.len == self.buf.len() {
                self.flush();
            }
            let n = b.len().min(self.buf.len() - self.len);
            self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
            self.len += n;
            b = &b[n..];
        }
        Ok(())
    }
}

/// Writes formatted output to the framebuffer (if present), serial, and the
/// kernel log.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    use core::sync::atomic::Ordering;
    interrupts::without_interrupts(|| {
        let me = if crate::arch::x86_64::cpu::is_initialized() {
            crate::arch::x86_64::cpu::this().cpu_id as usize + 1
        } else {
            1
        };
        let reentrant = PRINT_OWNER.load(Ordering::Acquire) == me;
        if !reentrant {
            let mut spins = 0u64;
            while PRINT_OWNER
                .compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
                spins += 1;
                // Never wedge the machine on a stuck printer.
                if spins > 50_000_000 {
                    break;
                }
            }
        }
        let mut b = LineBuf {
            buf: [0; 1024],
            len: 0,
        };
        let _ = b.write_fmt(args);
        b.flush();
        if !reentrant {
            let _ = PRINT_OWNER.compare_exchange(me, 0, Ordering::Release, Ordering::Relaxed);
        }
    });
}

/// Clear the framebuffer console (no-op without a framebuffer).
pub fn clear_screen() {
    interrupts::without_interrupts(|| {
        if let Some(c) = crate::drivers::framebuffer::CONSOLE.lock().as_mut() {
            c.clear();
        }
    });
}

/// Set foreground/background colours on the framebuffer console.
pub fn set_color(fg: Color, bg: Color) {
    interrupts::without_interrupts(|| {
        if let Some(c) = crate::drivers::framebuffer::CONSOLE.lock().as_mut() {
            c.set_colors(to_framebuffer_color(fg), to_framebuffer_color(bg));
        }
    });
}

/// Like the `print!` macro in the standard library, but prints to the kernel console.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::drivers::console::_print(format_args!($($arg)*)));
}

/// Like the `println!` macro in the standard library, but prints to the kernel console.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[test_case]
fn test_println_simple() {
    println!("test_println_simple output");
}

#[test_case]
fn test_println_many() {
    for _ in 0..200 {
        println!("test_println_many output");
    }
}

#[test_case]
fn test_println_reaches_klog() {
    println!("klog-marker-7c1f");
    let mut buf = [0u8; 4096];
    let n = crate::klog::read_tail(&mut buf);
    let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
    assert!(text.contains("klog-marker-7c1f"));
}
