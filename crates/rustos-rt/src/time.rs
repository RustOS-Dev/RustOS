//! Clocks and sleeping.

use crate::sys::{self, nr};

/// (seconds, nanoseconds) of the given clock (0 = realtime, 1 = monotonic).
pub fn clock(id: usize) -> (u64, u64) {
    let mut ts = [0i64; 2];
    sys::syscall(nr::CLOCK_GETTIME, &[id, ts.as_mut_ptr() as usize]);
    (ts[0] as u64, ts[1] as u64)
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    clock(0).0
}

/// Monotonic milliseconds.
pub fn millis() -> u64 {
    let (s, n) = clock(1);
    s * 1000 + n / 1_000_000
}

/// Monotonic microseconds.
pub fn micros() -> u64 {
    let (s, n) = clock(1);
    s * 1_000_000 + n / 1000
}

pub fn sleep_ms(ms: u64) {
    let ts = [(ms / 1000) as i64, ((ms % 1000) * 1_000_000) as i64];
    sys::syscall(nr::NANOSLEEP, &[ts.as_ptr() as usize, 0]);
}

/// Break a Unix timestamp into (year, month, day, hour, minute, second).
pub fn civil(t: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = (t / 86_400) as i64;
    let secs = t % 86_400;
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

pub const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
pub const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
