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

/// Total bytes ever written (a position usable with [`read_from`]).
pub fn head() -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| KLOG.lock().head)
}

/// Copy bytes written since position `pos` into `out`. Returns the new
/// position and the number of bytes copied; bytes that were already
/// overwritten in the ring are skipped.
pub fn read_from(pos: usize, out: &mut [u8]) -> (usize, usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let ring = KLOG.lock();
        let start = pos.max(ring.head.saturating_sub(KLOG_SIZE));
        let n = (ring.head - start.min(ring.head)).min(out.len());
        for (i, slot) in out.iter_mut().take(n).enumerate() {
            *slot = ring.buf[(start + i) % KLOG_SIZE];
        }
        (start + n, n)
    })
}

const PERSIST_DIR: &str = "/storage/log";
const PERSIST_FILE: &str = "/storage/log/kernel.log";
const PERSIST_MAX: u64 = 4 << 20;

/// Mirror the log to `/storage/log/kernel.log` (rotated at 4 MiB to
/// `kernel.log.1`), flushing to disk every two seconds so the tail
/// survives a hang or crash (`log.persist=1`).
pub fn start_persist() {
    if !crate::vfs::is_dir("/storage") {
        crate::println!("[klog] log.persist: /storage is not mounted");
        return;
    }
    let _ = crate::vfs::mkdir_p(PERSIST_DIR);
    if crate::vfs::stat(PERSIST_FILE).is_ok_and(|m| m.size > PERSIST_MAX) {
        let _ = crate::vfs::rename(PERSIST_FILE, "/storage/log/kernel.log.1");
    }
    crate::sched::spawn("klogd", || {
        let banner = alloc::format!(
            "\n===== boot at unix time {} (uptime {} ms) =====\n",
            crate::time::unix_time(),
            crate::time::millis()
        );
        let _ = crate::vfs::append(PERSIST_FILE, banner.as_bytes());
        let mut pos = 0usize;
        let mut buf = alloc::vec![0u8; 16 * 1024];
        loop {
            let mut wrote = false;
            loop {
                let (next, n) = read_from(pos, &mut buf);
                pos = next;
                if n == 0 {
                    break;
                }
                if crate::vfs::append(PERSIST_FILE, &buf[..n]).is_err() {
                    break;
                }
                wrote = true;
            }
            if wrote {
                crate::vfs::sync_all();
            }
            crate::time::sleep_ms(2000);
        }
    });
}
