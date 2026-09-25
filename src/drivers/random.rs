//! Kernel random numbers: RDRAND/RDSEED when available, mixed into a
//! ChaCha-style state seeded from the TSC and RTC.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

static STATE: Mutex<[u64; 4]> = Mutex::new([
    0x243F_6A88_85A3_08D3,
    0x1319_8A2E_0370_7344,
    0xA409_3822_299F_31D0,
    0x082E_FA98_EC4E_6C89,
]);
static SEEDED: AtomicBool = AtomicBool::new(false);
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn rdrand() -> Option<u64> {
    let ecx = core::arch::x86_64::__cpuid(1).ecx;
    if ecx & (1 << 30) == 0 {
        return None;
    }
    for _ in 0..10 {
        let mut v: u64 = 0;
        if unsafe { core::arch::x86_64::_rdrand64_step(&mut v) } == 1 {
            return Some(v);
        }
    }
    None
}

// xoshiro256** step
fn next(s: &mut [u64; 4]) -> u64 {
    let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
    let t = s[1] << 17;
    s[2] ^= s[0];
    s[3] ^= s[1];
    s[1] ^= s[2];
    s[0] ^= s[3];
    s[2] ^= t;
    s[3] = s[3].rotate_left(45);
    result
}

fn seed(s: &mut [u64; 4]) {
    s[0] ^= crate::time::rdtsc();
    s[1] ^= crate::time::unix_time().rotate_left(32);
    s[2] ^= rdrand().unwrap_or(0x9E37_79B9_7F4A_7C15);
    s[3] ^= rdrand().unwrap_or(crate::time::rdtsc().rotate_left(17));
    for _ in 0..16 {
        next(s);
    }
}

/// Fill `buf` with random bytes.
pub fn fill(buf: &mut [u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut s = STATE.lock();
        if !SEEDED.swap(true, Ordering::SeqCst) {
            seed(&mut s);
        }
        // Stir in fresh entropy regularly.
        if COUNTER.fetch_add(1, Ordering::Relaxed).is_multiple_of(64) {
            s[0] ^= crate::time::rdtsc();
            if let Some(r) = rdrand() {
                s[3] ^= r;
            }
        }
        for chunk in buf.chunks_mut(8) {
            let mut v = next(&mut s);
            if let Some(r) = rdrand() {
                v ^= r;
            }
            chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]);
        }
    });
}

pub fn u64() -> u64 {
    let mut b = [0u8; 8];
    fill(&mut b);
    u64::from_le_bytes(b)
}
