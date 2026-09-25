//! Kernel console: routes `print!` output to the UEFI framebuffer when one is
//! present, always mirrors it to COM1, and records it in the kernel log ring
//! buffer read by `dmesg`.
//!
//! Legacy VGA text mode (0xb8000) is not used: bootloader 0.11 boots through
//! UEFI, which never leaves the adapter in text mode.

use core::fmt;
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

// The framebuffer console only has an 8-colour palette, so light/dark variants
// collapse to the nearest base colour.
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

/// Writes formatted output to the framebuffer (if present), serial, and the
/// kernel log.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;

    interrupts::without_interrupts(|| {
        use crate::drivers::framebuffer::FRAMEBUFFER_WRITER;
        if let Some(fb_writer) = FRAMEBUFFER_WRITER.lock().as_mut() {
            let _ = fb_writer.write_fmt(args);
        }
        let _ = crate::klog::KlogWriter.write_fmt(args);
    });

    crate::serial_print!("{}", args);
}

/// Clear the framebuffer console (no-op without a framebuffer).
pub fn clear_screen() {
    interrupts::without_interrupts(|| {
        use crate::drivers::framebuffer::FRAMEBUFFER_WRITER;
        if let Some(fb_writer) = FRAMEBUFFER_WRITER.lock().as_mut() {
            fb_writer.clear_screen();
        }
    });
}

/// Set foreground/background colours on the framebuffer console.
pub fn set_color(fg: Color, bg: Color) {
    interrupts::without_interrupts(|| {
        use crate::drivers::framebuffer::FRAMEBUFFER_WRITER;
        if let Some(fb_writer) = FRAMEBUFFER_WRITER.lock().as_mut() {
            fb_writer.set_colors(to_framebuffer_color(fg), to_framebuffer_color(bg));
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
