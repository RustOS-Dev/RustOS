//! Futexes: user-space locks' kernel side.
//!
//! Waiters are keyed by the physical address of the futex word, so private
//! and shared (MAP_SHARED, across fork) futexes both work. Each waiter
//! holds a flag that a waker sets; all waiters sleep on one wait queue and
//! re-check their own flag, which makes requeueing a list operation.

use crate::errno::*;
use crate::process::{signal, uaccess};
use crate::sched::WaitQueue;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

pub const FUTEX_WAIT: u32 = 0;
pub const FUTEX_WAKE: u32 = 1;
pub const FUTEX_REQUEUE: u32 = 3;
pub const FUTEX_CMP_REQUEUE: u32 = 4;
pub const FUTEX_WAKE_OP: u32 = 5;
pub const FUTEX_WAIT_BITSET: u32 = 9;
pub const FUTEX_WAKE_BITSET: u32 = 10;
const FUTEX_CLOCK_REALTIME: u32 = 256;
const BITSET_ANY: u32 = u32::MAX;

struct Waiter {
    woken: Arc<AtomicBool>,
    bitset: u32,
}

/// Waiters per futex key (physical address).
static TABLE: Mutex<BTreeMap<u64, Vec<Waiter>>> = Mutex::new(BTreeMap::new());
static WQ: WaitQueue = WaitQueue::new();

fn irqsave<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}

/// Physical address of the user word at `addr` in the current address
/// space (faulting it in first).
fn key(addr: u64) -> KResult<u64> {
    if addr & 3 != 0 {
        return Err(EINVAL);
    }
    let _: u32 = uaccess::read_user(addr)?;
    let (cr3, _) = x86_64::registers::control::Cr3::read_raw();
    let mut table = cr3.start_address().as_u64();
    for level in (0..4).rev() {
        let idx = (addr >> (12 + 9 * level)) & 0x1FF;
        let e: u64 = unsafe { *crate::mm::phys_ptr::<u64>(table + idx * 8) };
        if e & 1 == 0 {
            return Err(EFAULT);
        }
        let phys = e & 0x000F_FFFF_FFFF_F000;
        if level == 0 || (level < 3 && e & 0x80 != 0) {
            let page_mask = (1u64 << (12 + 9 * level)) - 1;
            return Ok((phys & !page_mask) | (addr & page_mask));
        }
        table = phys;
    }
    Err(EFAULT)
}

/// Wake up to `n` waiters on `k` whose bitset intersects `bits`.
fn wake_key(k: u64, n: u32, bits: u32) -> u32 {
    let mut woken = 0;
    irqsave(|| {
        let mut t = TABLE.lock();
        if let Some(list) = t.get_mut(&k) {
            list.retain(|w| {
                if woken < n && w.bitset & bits != 0 {
                    w.woken.store(true, Ordering::SeqCst);
                    woken += 1;
                    false
                } else {
                    true
                }
            });
            if list.is_empty() {
                t.remove(&k);
            }
        }
    });
    if woken > 0 {
        WQ.wake_all();
    }
    woken
}

/// Wait while `*addr == val`, until woken, a signal, or `deadline` (ns on
/// the monotonic clock).
fn wait(addr: u64, val: u32, bits: u32, deadline: Option<u64>) -> KResult<i64> {
    if bits == 0 {
        return Err(EINVAL);
    }
    let k = key(addr)?;
    let flag = Arc::new(AtomicBool::new(false));
    // Check the value and queue ourselves atomically with respect to wakers
    // (they take the table lock after changing the word).
    let queued = irqsave(|| -> KResult<bool> {
        let mut t = TABLE.lock();
        let cur: u32 = uaccess::read_user(addr)?;
        if cur != val {
            return Ok(false);
        }
        t.entry(k).or_default().push(Waiter {
            woken: flag.clone(),
            bitset: bits,
        });
        Ok(true)
    })?;
    if !queued {
        return Err(EAGAIN);
    }
    let cond = || flag.load(Ordering::SeqCst) || signal::has_pending();
    match deadline {
        Some(d) => {
            let now = crate::time::nanos();
            let ms = d.saturating_sub(now).div_ceil(1_000_000);
            if ms > 0 {
                WQ.wait_timeout(ms, cond);
            }
        }
        None => WQ.wait_until(cond),
    }
    if flag.load(Ordering::SeqCst) {
        return Ok(0);
    }
    // Not woken: take ourselves off whatever list we are on (we may have
    // been requeued to another key).
    let removed = irqsave(|| {
        let mut t = TABLE.lock();
        let mut found = false;
        t.retain(|_, list| {
            list.retain(|w| {
                let me = Arc::ptr_eq(&w.woken, &flag);
                found |= me;
                !me
            });
            !list.is_empty()
        });
        found
    });
    if !removed && flag.load(Ordering::SeqCst) {
        return Ok(0); // woken in the meantime
    }
    if signal::has_pending() {
        Err(EINTR)
    } else {
        Err(ETIMEDOUT)
    }
}

/// Move up to `n` waiters from `from` to `to`; returns how many moved.
fn requeue(from: u64, to: u64, n: u32) -> u32 {
    irqsave(|| {
        let mut t = TABLE.lock();
        let Some(mut list) = t.remove(&from) else {
            return 0;
        };
        let take = (n as usize).min(list.len());
        let moved: Vec<Waiter> = list.drain(..take).collect();
        if !list.is_empty() {
            t.insert(from, list);
        }
        let count = moved.len() as u32;
        if count > 0 {
            t.entry(to).or_default().extend(moved);
        }
        count
    })
}

fn timespec_ns(p: u64) -> KResult<u64> {
    let t: [i64; 2] = uaccess::read_user(p)?;
    if t[0] < 0 || !(0..1_000_000_000).contains(&t[1]) {
        return Err(EINVAL);
    }
    Ok(t[0] as u64 * 1_000_000_000 + t[1] as u64)
}

/// futex(2).
pub fn futex(addr: u64, op: u32, val: u32, timeout: u64, addr2: u64, val3: u32) -> KResult<i64> {
    let cmd = op & 0x7F;
    match cmd {
        FUTEX_WAIT => {
            let deadline = if timeout != 0 {
                Some(crate::time::nanos() + timespec_ns(timeout)?)
            } else {
                None
            };
            wait(addr, val, BITSET_ANY, deadline)
        }
        FUTEX_WAIT_BITSET => {
            // Absolute timeout on CLOCK_MONOTONIC (or CLOCK_REALTIME).
            let deadline = if timeout != 0 {
                let abs = timespec_ns(timeout)?;
                Some(if op & FUTEX_CLOCK_REALTIME != 0 {
                    let now_real = crate::time::realtime_nanos();
                    // Convert to the monotonic clock.
                    crate::time::nanos() + abs.saturating_sub(now_real)
                } else {
                    abs
                })
            } else {
                None
            };
            wait(addr, val, val3, deadline)
        }
        FUTEX_WAKE => Ok(wake_key(key(addr)?, val, BITSET_ANY) as i64),
        FUTEX_WAKE_BITSET => {
            if val3 == 0 {
                return Err(EINVAL);
            }
            Ok(wake_key(key(addr)?, val, val3) as i64)
        }
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            let k1 = key(addr)?;
            let k2 = key(addr2)?;
            if cmd == FUTEX_CMP_REQUEUE {
                let cur: u32 = uaccess::read_user(addr)?;
                if cur != val3 {
                    return Err(EAGAIN);
                }
            }
            let woken = wake_key(k1, val, BITSET_ANY);
            // `timeout` carries val2: how many to requeue.
            let moved = requeue(k1, k2, timeout as u32);
            Ok((woken + if cmd == FUTEX_CMP_REQUEUE { moved } else { 0 }) as i64)
        }
        FUTEX_WAKE_OP => {
            let k1 = key(addr)?;
            let k2 = key(addr2)?;
            let op_ = (val3 >> 28) & 7;
            let cmp = (val3 >> 24) & 0xF;
            let mut oparg = (val3 >> 12) & 0xFFF;
            let cmparg = val3 & 0xFFF;
            if (val3 >> 28) & 8 != 0 {
                oparg = 1u32.checked_shl(oparg).unwrap_or(0);
            }
            let old: u32 = irqsave(|| -> KResult<u32> {
                let _t = TABLE.lock();
                let old: u32 = uaccess::read_user(addr2)?;
                let new = match op_ {
                    0 => oparg,
                    1 => old.wrapping_add(oparg),
                    2 => old | oparg,
                    3 => old & !oparg,
                    4 => old ^ oparg,
                    _ => return Err(ENOSYS),
                };
                uaccess::write_user(addr2, &new)?;
                Ok(old)
            })?;
            let mut n = wake_key(k1, val, BITSET_ANY);
            let (o, c) = (old as i32, cmparg as i32);
            let hit = match cmp {
                0 => o == c,
                1 => o != c,
                2 => o < c,
                3 => o <= c,
                4 => o > c,
                5 => o >= c,
                _ => return Err(ENOSYS),
            };
            if hit {
                n += wake_key(k2, timeout as u32, BITSET_ANY);
            }
            Ok(n as i64)
        }
        _ => Err(ENOSYS),
    }
}

/// Thread exit with a `clear_child_tid` address: zero it and wake one
/// waiter (pthread_join).
pub fn clear_tid_and_wake(addr: u64) {
    if addr == 0 {
        return;
    }
    if uaccess::write_user(addr, &0u32).is_ok()
        && let Ok(k) = key(addr)
    {
        wake_key(k, 1, BITSET_ANY);
    }
}
