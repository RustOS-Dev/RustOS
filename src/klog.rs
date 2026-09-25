//! Kernel log ring buffer.
//!
//! Every byte written to the console is appended here so `dmesg` can replay
//! boot and driver messages even after they scrolled off screen. The buffer
//! is a fixed static array so it works before the heap exists.

use core::fmt;
use spin::Mutex;

const KLOG_SIZE: usize = 64 * 1024;

struct Ring {
    buf: [u8; KLOG_SIZE],
    /// Total bytes ever written; `head % KLOG_SIZE` is the next write slot.
    head: usize,
}

static KLOG: Mutex<Ring> = Mutex::new(Ring {
    buf: [0; KLOG_SIZE],
    head: 0,
});

/// Append raw bytes to the log.
pub fn write_bytes(bytes: &[u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut ring = KLOG.lock();
        for &b in bytes {
            let idx = ring.head % KLOG_SIZE;
            ring.buf[idx] = b;
            ring.head = ring.head.wrapping_add(1);
        }
    });
}

/// Copy the most recent `out.len()` bytes (or fewer) into `out`, oldest
/// first. Returns the number of bytes copied.
pub fn read_tail(out: &mut [u8]) -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let ring = KLOG.lock();
        let avail = ring.head.min(KLOG_SIZE);
        let n = avail.min(out.len());
        let start = ring.head - n;
        for (i, slot) in out.iter_mut().take(n).enumerate() {
            *slot = ring.buf[(start + i) % KLOG_SIZE];
        }
        n
    })
}

/// Total number of bytes currently retained.
pub fn len() -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| KLOG.lock().head.min(KLOG_SIZE))
}

/// `fmt::Write` adapter that appends to the kernel log.
pub struct KlogWriter;

impl fmt::Write for KlogWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        write_bytes(s.as_bytes());
        Ok(())
    }
}
