//! Kernel log ring buffer.
//!
//! Every byte written to the console is appended here so `dmesg` can replay
//! boot and driver messages even after they scrolled off screen. The buffer
//! is a fixed static array so it works before the heap exists.

use crate::sync::Mutex;
use core::fmt;

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
pub fn is_locked() -> bool {
    KLOG.is_locked()
}

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
    static STARTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if !crate::vfs::is_dir("/storage") {
        crate::println!("[klog] log.persist: /storage is not mounted");
        return;
    }
    if STARTED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let _ = crate::vfs::mkdir_p(PERSIST_DIR);
    if crate::vfs::stat(PERSIST_FILE).is_ok_and(|m| m.size > PERSIST_MAX) {
        let _ = crate::vfs::rename(PERSIST_FILE, "/storage/log/kernel.log.1");
    }
    let banner = alloc::format!(
        "\n===== boot at unix time {} (uptime {} ms) =====\n",
        crate::time::unix_time(),
        crate::time::millis()
    );
    let _ = crate::vfs::append(PERSIST_FILE, banner.as_bytes());
    PERSISTING.store(true, core::sync::atomic::Ordering::SeqCst);
    crate::sched::spawn("klogd", || {
        loop {
            sync_persist();
            crate::time::sleep_ms(2000);
        }
    });
}

/// Set once log.persist has started.
static PERSISTING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Log position written out so far (held across the disk writes).
static PERSISTED: crate::sched::mutex::Mutex<usize> = crate::sched::mutex::Mutex::new(0);

/// Write what the log gained since the last write to kernel.log and sync
/// it to disk now, when log.persist runs (klogd does every two seconds;
/// `linux.initcall_debug` before each Linux initcall and probe, so the
/// last line on disk names the one a machine stopped in).
pub fn sync_persist() {
    if !PERSISTING.load(core::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let mut pos = PERSISTED.lock();
    let mut buf = alloc::vec![0u8; 16 * 1024];
    let mut wrote = false;
    loop {
        let (next, n) = read_from(*pos, &mut buf);
        if n == 0 || crate::vfs::append(PERSIST_FILE, &buf[..n]).is_err() {
            break;
        }
        *pos = next;
        wrote = true;
    }
    if wrote {
        crate::vfs::sync_all();
    }
}
