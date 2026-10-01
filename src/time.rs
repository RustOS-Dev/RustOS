//! Timekeeping.
//!
//! * A monotonic nanosecond clock derived from the TSC, calibrated at boot
//!   against the HPET, the ACPI PM timer, or PIT channel 2 (in that order).
//! * A periodic local-APIC timer tick (`HZ` per second) driving the scheduler
//!   and timed sleeps.
//! * Wall-clock time from the CMOS RTC, captured at boot.

use crate::arch::x86_64::{acpi, apic, idt};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::instructions::port::Port;

pub const HZ: u64 = 250;

static TSC_HZ: AtomicU64 = AtomicU64::new(0);
static TSC_BOOT: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);
static BOOT_UNIX: AtomicU64 = AtomicU64::new(0);
static TIMER_RUNNING: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Nanoseconds since boot. Before calibration this assumes a 2 GHz TSC.
pub fn nanos() -> u64 {
    let hz = match TSC_HZ.load(Ordering::Relaxed) {
        0 => 2_000_000_000,
        h => h,
    };
    let delta = rdtsc().wrapping_sub(TSC_BOOT.load(Ordering::Relaxed));
    ((delta as u128 * 1_000_000_000) / hz as u128) as u64
}

pub fn micros() -> u64 {
    nanos() / 1_000
}

pub fn millis() -> u64 {
    nanos() / 1_000_000
}

/// Timer ticks since the tick started.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn tsc_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

/// Busy-wait for at least `us` microseconds.
pub fn delay_us(us: u64) {
    let end = nanos() + us * 1_000;
    while nanos() < end {
        crate::arch::x86_64::smp::poll();
        core::hint::spin_loop();
    }
}

/// Sleep for `ms` milliseconds, yielding the CPU when the scheduler runs.
pub fn sleep_ms(ms: u64) {
    if crate::sched::is_running() {
        crate::sched::sleep_until(nanos() + ms * 1_000_000);
    } else {
        delay_us(ms * 1_000);
    }
}

/// A point in time used for driver timeouts.
#[derive(Clone, Copy, Debug)]
pub struct Deadline(u64);

impl Deadline {
    pub fn after_ms(ms: u64) -> Deadline {
        Deadline(nanos() + ms * 1_000_000)
    }
    pub fn after_us(us: u64) -> Deadline {
        Deadline(nanos() + us * 1_000)
    }
    pub fn expired(&self) -> bool {
        nanos() >= self.0
    }
    pub fn nanos(&self) -> u64 {
        self.0
    }
}

/// Spin until `cond` holds or `ms` elapse. Returns whether `cond` held.
pub fn wait_until(ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let d = Deadline::after_ms(ms);
    loop {
        if cond() {
            return true;
        }
        if d.expired() {
            return cond();
        }
        crate::arch::x86_64::smp::poll();
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// Calibration
// ---------------------------------------------------------------------------

/// Measure the TSC frequency. Call once, after ACPI parsing.
pub fn calibrate() {
    TSC_BOOT.store(rdtsc(), Ordering::SeqCst);
    let hz = cpuid_tsc_hz()
        .or_else(calibrate_hpet)
        .or_else(calibrate_pm_timer)
        .unwrap_or_else(calibrate_pit);
    TSC_HZ.store(hz, Ordering::SeqCst);
    BOOT_UNIX.store(crate::arch::x86_64::rtc::read_unix_time(), Ordering::SeqCst);
    crate::println!("[time] TSC {} MHz", hz / 1_000_000);
}

fn cpuid_tsc_hz() -> Option<u64> {
    let max = core::arch::x86_64::__cpuid(0).eax;
    if max < 0x15 {
        return None;
    }
    let r = core::arch::x86_64::__cpuid(0x15);
    if r.eax == 0 || r.ebx == 0 || r.ecx == 0 {
        return None;
    }
    Some(r.ecx as u64 * r.ebx as u64 / r.eax as u64)
}

fn calibrate_hpet() -> Option<u64> {
    let phys = acpi::platform().hpet_base?;
    let base = crate::mm::map_mmio(phys, 0x400);
    let read64 = |off: u64| unsafe { core::ptr::read_volatile((base + off) as *const u64) };
    let write64 =
        |off: u64, v: u64| unsafe { core::ptr::write_volatile((base + off) as *mut u64, v) };
    let period_fs = read64(0) >> 32; // femtoseconds per tick
    if period_fs == 0 || period_fs > 100_000_000 {
        return None;
    }
    write64(0x10, read64(0x10) | 1); // enable main counter
    let ticks_10ms = 10_000_000_000_000u64 / period_fs;
    let h0 = read64(0xF0);
    let t0 = rdtsc();
    while read64(0xF0).wrapping_sub(h0) < ticks_10ms {
        core::hint::spin_loop();
    }
    let t1 = rdtsc();
    let h1 = read64(0xF0);
    let elapsed_fs = (h1.wrapping_sub(h0)) as u128 * period_fs as u128;
    Some(((t1 - t0) as u128 * 1_000_000_000_000_000 / elapsed_fs) as u64)
}

fn calibrate_pm_timer() -> Option<u64> {
    const PM_HZ: u64 = 3_579_545;
    let port = acpi::platform().pm_timer_port?;
    let mut p = Port::<u32>::new(port);
    let read = |p: &mut Port<u32>| unsafe { p.read() } & 0x00FF_FFFF;
    let target = PM_HZ / 100; // 10 ms
    let s = read(&mut p);
    let t0 = rdtsc();
    loop {
        let now = read(&mut p);
        if (now.wrapping_sub(s) & 0x00FF_FFFF) as u64 >= target {
            break;
        }
    }
    let t1 = rdtsc();
    Some((t1 - t0) * 100)
}

fn calibrate_pit() -> u64 {
    // PIT channel 2 one-shot of 10 ms, gated through port 0x61.
    const PIT_HZ: u64 = 1_193_182;
    let count = (PIT_HZ / 100) as u16;
    unsafe {
        let mut p61 = Port::<u8>::new(0x61);
        let mut cmd = Port::<u8>::new(0x43);
        let mut ch2 = Port::<u8>::new(0x42);
        let v = p61.read();
        p61.write((v & !0x02) | 0x01);
        cmd.write(0b1011_0000); // channel 2, lo/hi, mode 0
        ch2.write(count as u8);
        ch2.write((count >> 8) as u8);
        let v = p61.read();
        p61.write(v & !0x01);
        p61.write(v | 0x01);
        let t0 = rdtsc();
        while p61.read() & 0x20 == 0 {
            core::hint::spin_loop();
        }
        let t1 = rdtsc();
        (t1 - t0) * 100
    }
}

// ---------------------------------------------------------------------------
// Periodic tick
// ---------------------------------------------------------------------------

static APIC_TICKS_PER_PERIOD: AtomicU64 = AtomicU64::new(0);

/// Start the periodic local-APIC timer on the current CPU.
pub fn start_tick() {
    if APIC_TICKS_PER_PERIOD.load(Ordering::SeqCst) == 0 {
        // Calibrate: count APIC timer ticks over 10 ms (divide by 16).
        apic::write(apic::REG_TIMER_DIV, 0x3);
        apic::write(apic::REG_LVT_TIMER, apic::LVT_MASKED);
        apic::write(apic::REG_TIMER_INIT, u32::MAX);
        delay_us(10_000);
        let elapsed = u32::MAX - apic::read(apic::REG_TIMER_CURRENT);
        apic::write(apic::REG_TIMER_INIT, 0);
        let per_sec = elapsed as u64 * 100;
        APIC_TICKS_PER_PERIOD.store((per_sec / HZ).max(1), Ordering::SeqCst);
        idt::register(idt::VEC_TIMER, timer_interrupt);
    }
    apic::write(apic::REG_TIMER_DIV, 0x3);
    apic::write(
        apic::REG_LVT_TIMER,
        idt::VEC_TIMER as u32 | apic::LVT_TIMER_PERIODIC,
    );
    apic::write(
        apic::REG_TIMER_INIT,
        APIC_TICKS_PER_PERIOD.load(Ordering::SeqCst) as u32,
    );
    TIMER_RUNNING.store(true, Ordering::SeqCst);
}

/// A SysRq request the TTY input thread has not taken within two seconds
/// (it may be the stuck one): dump from the timer interrupt instead.
fn sysrq_watchdog() {
    use crate::drivers::serial::{SYSRQ, SYSRQ_SINCE};
    let since = SYSRQ_SINCE.load(Ordering::SeqCst);
    if since == 0 || millis().saturating_sub(since) < 2000 || crate::allocator::heap_locked() {
        return;
    }
    if SYSRQ.swap(false, Ordering::SeqCst) {
        SYSRQ_SINCE.store(0, Ordering::SeqCst);
        crate::drivers::serial::write_unlocked(
            b"[sysrq] (from the timer: the input thread is stuck)\n",
        );
        crate::tty::debug_dump();
    }
}

fn timer_interrupt(frame: &mut idt::TrapFrame) {
    crate::sched::cputime::account_tick(frame.from_user());
    let cpu = crate::arch::x86_64::cpu::this();
    cpu.ticks.fetch_add(1, Ordering::Relaxed);
    if cpu.cpu_id == 0 {
        TICKS.fetch_add(1, Ordering::Relaxed);
    }
    apic::eoi();
    if cpu.cpu_id == 0 {
        sysrq_watchdog();
    }
    crate::sched::timer_tick();
}

// ---------------------------------------------------------------------------
// Wall clock
// ---------------------------------------------------------------------------

/// Seconds since the Unix epoch.
pub fn unix_time() -> u64 {
    BOOT_UNIX.load(Ordering::Relaxed) + nanos() / 1_000_000_000
}

/// Set the wall clock (seconds since the Unix epoch).
pub fn set_unix_time(secs: u64) {
    BOOT_UNIX.store(
        secs.saturating_sub(nanos() / 1_000_000_000),
        Ordering::Relaxed,
    );
}

/// (seconds, nanoseconds) since the Unix epoch.
pub fn realtime() -> (u64, u64) {
    let n = nanos();
    (
        BOOT_UNIX.load(Ordering::Relaxed) + n / 1_000_000_000,
        n % 1_000_000_000,
    )
}

/// Nanoseconds since the Unix epoch.
pub fn realtime_nanos() -> u64 {
    let (s, ns) = realtime();
    s * 1_000_000_000 + ns
}

/// Break a Unix timestamp into (year, month, day, hour, minute, second).
pub fn civil_from_unix(t: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = (t / 86_400) as i64;
    let secs = t % 86_400;
    // Howard Hinnant's days_from_civil inverse.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        y as u32,
        m as u32,
        d as u32,
        (secs / 3600) as u32,
        ((secs / 60) % 60) as u32,
        (secs % 60) as u32,
    )
}

/// Days since the epoch for a civil date.
pub fn unix_from_civil(y: u32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> u64 {
    let y = y as i64 - if m <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days as u64) * 86_400 + hh as u64 * 3600 + mm as u64 * 60 + ss as u64
}

#[test_case]
fn test_civil_round_trip() {
    let t = unix_from_civil(2026, 9, 25, 20, 5, 52);
    assert_eq!(civil_from_unix(t), (2026, 9, 25, 20, 5, 52));
    assert_eq!(unix_from_civil(1970, 1, 1, 0, 0, 0), 0);
}
