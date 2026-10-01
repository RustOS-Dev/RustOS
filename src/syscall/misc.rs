//! Time, system-information and miscellaneous syscalls.

use super::SysResult;
use crate::errno::*;
use crate::process::{signal, uaccess};
use crate::sync::Mutex;
use alloc::string::String;

pub static HOSTNAME: Mutex<String> = Mutex::new(String::new());

pub fn hostname() -> String {
    let h = HOSTNAME.lock();
    if h.is_empty() {
        String::from("rustos")
    } else {
        h.clone()
    }
}

fn sleep_ns(ns: u64) -> KResult<()> {
    let t = crate::sched::current();
    // A restarted sleep continues towards its original deadline.
    let deadline = match t
        .restart_deadline
        .swap(0, core::sync::atomic::Ordering::SeqCst)
    {
        0 => crate::time::nanos() + ns,
        d => d,
    };
    crate::sched::sleep_until(deadline);
    if crate::time::nanos() < deadline && signal::has_pending() {
        t.restart_deadline
            .store(deadline, core::sync::atomic::Ordering::SeqCst);
        return Err(EINTR);
    }
    Ok(())
}

pub fn nanosleep(req: u64, _rem: u64) -> SysResult {
    let t: [i64; 2] = uaccess::read_user(req)?;
    if t[0] < 0 || !(0..1_000_000_000).contains(&t[1]) {
        return Err(EINVAL);
    }
    sleep_ns(t[0] as u64 * 1_000_000_000 + t[1] as u64)?;
    Ok(0)
}

pub fn clock_nanosleep(clock: u32, flags: u32, req: u64, rem: u64) -> SysResult {
    const TIMER_ABSTIME: u32 = 1;
    if flags & TIMER_ABSTIME != 0 {
        let t: [i64; 2] = uaccess::read_user(req)?;
        let target = t[0] as u64 * 1_000_000_000 + t[1] as u64;
        let now = if clock == 0 {
            let (s, n) = crate::time::realtime();
            s * 1_000_000_000 + n
        } else {
            crate::time::nanos()
        };
        sleep_ns(target.saturating_sub(now))?;
        return Ok(0);
    }
    nanosleep(req, rem)
}

const CLOCK_PROCESS_CPUTIME_ID: u32 = 2;
const CLOCK_THREAD_CPUTIME_ID: u32 = 3;

/// CPU time in nanoseconds for a CPU-time clock: the two standard ids, or
/// a dynamic clock from `clock_getcpuclockid`/`pthread_getcpuclockid`
/// (negative: `(~pid << 3) | type`, bit 2 = thread, type 1 = user time
/// only).
fn cpu_clock_ns(clock: u32) -> KResult<Option<u64>> {
    use crate::sched::cputime::to_ns;
    let (thread, id, user_only) = match clock {
        CLOCK_PROCESS_CPUTIME_ID => (false, 0, false),
        CLOCK_THREAD_CPUTIME_ID => (true, 0, false),
        c if (c as i32) < 0 => {
            let c = c as i32;
            if c & 3 == 3 {
                return Err(EINVAL);
            }
            (c & 4 != 0, !(c >> 3) as u32, c & 3 == 1)
        }
        _ => return Ok(None),
    };
    let (u, s) = if thread {
        let t = if id == 0 {
            crate::sched::current()
        } else {
            crate::sched::find_thread(id as u64).ok_or(EINVAL)?
        };
        t.cpu_times()
    } else {
        let p = if id == 0 {
            crate::process::current().ok_or(ESRCH)?
        } else {
            crate::process::find(id).ok_or(EINVAL)?
        };
        p.cpu_times()
    };
    Ok(Some(to_ns(if user_only { u } else { u + s })))
}

pub fn clock_gettime(clock: u32, tp: u64) -> SysResult {
    let (s, n) = match clock {
        0 | 8 | 11 => crate::time::realtime(), // REALTIME, *_ALARM, TAI
        _ => {
            let ns = match cpu_clock_ns(clock)? {
                Some(ns) => ns,
                None => crate::time::nanos(),
            };
            (ns / 1_000_000_000, ns % 1_000_000_000)
        }
    };
    uaccess::write_user(tp, &[s as i64, n as i64])?;
    Ok(0)
}

pub fn clock_getres(clock: u32, tp: u64) -> SysResult {
    // CPU-time clocks advance by timer ticks.
    let res = match cpu_clock_ns(clock)? {
        Some(_) => crate::sched::cputime::to_ns(1) as i64,
        None => 1,
    };
    if tp != 0 {
        uaccess::write_user(tp, &[0i64, res])?;
    }
    Ok(0)
}

pub fn gettimeofday(tv: u64) -> SysResult {
    if tv != 0 {
        let (s, n) = crate::time::realtime();
        uaccess::write_user(tv, &[s as i64, (n / 1000) as i64])?;
    }
    Ok(0)
}

fn require_root() -> KResult<()> {
    let uid =
        crate::process::current().map_or(0, |p| p.uid.load(core::sync::atomic::Ordering::Relaxed));
    if uid == 0 { Ok(()) } else { Err(EPERM) }
}

pub fn settimeofday(tv: u64) -> SysResult {
    require_root()?;
    if tv != 0 {
        let v: [i64; 2] = uaccess::read_user(tv)?;
        crate::time::set_unix_time(v[0].max(0) as u64);
    }
    Ok(0)
}

pub fn clock_settime(clock: u32, ts: u64) -> SysResult {
    if clock != 0 {
        return Err(EINVAL);
    }
    require_root()?;
    let v: [i64; 2] = uaccess::read_user(ts)?;
    crate::time::set_unix_time(v[0].max(0) as u64);
    Ok(0)
}

pub fn time(t: u64) -> SysResult {
    let now = crate::time::unix_time() as i64;
    if t != 0 {
        uaccess::write_user(t, &now)?;
    }
    Ok(now)
}

pub fn alarm(secs: u64) -> SysResult {
    let p = crate::process::current().ok_or(ESRCH)?;
    let (old, _) = crate::process::itimer::set(p.pid, secs * 1_000_000_000, 0);
    Ok(old.div_ceil(1_000_000_000) as i64)
}

fn timeval_ns(tv: [i64; 2]) -> KResult<u64> {
    if tv[0] < 0 || !(0..1_000_000).contains(&tv[1]) {
        return Err(EINVAL);
    }
    Ok(tv[0] as u64 * 1_000_000_000 + tv[1] as u64 * 1000)
}

fn ns_timeval(ns: u64) -> [i64; 2] {
    [
        (ns / 1_000_000_000) as i64,
        ((ns % 1_000_000_000) / 1000) as i64,
    ]
}

fn itimerval(value: u64, interval: u64) -> [i64; 4] {
    let (i, v) = (ns_timeval(interval), ns_timeval(value));
    [i[0], i[1], v[0], v[1]]
}

/// getitimer(2): only ITIMER_REAL.
pub fn getitimer(which: u64, out: u64) -> SysResult {
    if which != 0 {
        return Err(EINVAL);
    }
    let p = crate::process::current().ok_or(ESRCH)?;
    let (v, i) = crate::process::itimer::get(p.pid);
    uaccess::write_user(out, &itimerval(v, i))?;
    Ok(0)
}

/// setitimer(2): only ITIMER_REAL.
pub fn setitimer(which: u64, new: u64, old: u64) -> SysResult {
    if which != 0 {
        return Err(EINVAL);
    }
    let p = crate::process::current().ok_or(ESRCH)?;
    let n: [i64; 4] = if new != 0 {
        uaccess::read_user(new)?
    } else {
        [0; 4]
    };
    let interval = timeval_ns([n[0], n[1]])?;
    let value = timeval_ns([n[2], n[3]])?;
    let (ov, oi) = crate::process::itimer::set(p.pid, value, interval);
    if old != 0 {
        uaccess::write_user(old, &itimerval(ov, oi))?;
    }
    Ok(0)
}

pub fn uname(buf: u64) -> SysResult {
    let mut u = [[0u8; 65]; 6];
    let fields = [
        "RustOS",
        &hostname(),
        env!("CARGO_PKG_VERSION"),
        concat!("#1 SMP ", env!("CARGO_PKG_VERSION")),
        "x86_64",
        "(none)",
    ];
    for (i, f) in fields.iter().enumerate() {
        let b = f.as_bytes();
        let n = b.len().min(64);
        u[i][..n].copy_from_slice(&b[..n]);
    }
    uaccess::write_user(buf, &u)?;
    Ok(0)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SysInfo {
    uptime: i64,
    loads: [u64; 3],
    totalram: u64,
    freeram: u64,
    sharedram: u64,
    bufferram: u64,
    totalswap: u64,
    freeswap: u64,
    procs: u16,
    _pad: [u8; 6],
    totalhigh: u64,
    freehigh: u64,
    mem_unit: u32,
    _f: [u8; 4],
}

pub fn sysinfo(buf: u64) -> SysResult {
    let (free, total) = crate::mm::memory_stats();
    let si = SysInfo {
        uptime: (crate::time::nanos() / 1_000_000_000) as i64,
        totalram: total,
        freeram: free,
        loads: crate::sched::cputime::loadavg_sysinfo(),
        procs: crate::process::count() as u16,
        mem_unit: 1,
        ..Default::default()
    };
    uaccess::write_user(buf, &si)?;
    Ok(0)
}

pub fn syslog(kind: u32, buf: u64, len: u64) -> SysResult {
    match kind {
        // READ_ALL / READ
        2 | 3 => {
            let mut tmp = alloc::vec![0u8; (len as usize).min(crate::klog::len())];
            let n = crate::klog::read_tail(&mut tmp);
            uaccess::copy_to_user(buf, &tmp[..n])?;
            Ok(n as i64)
        }
        10 => Ok(crate::klog::len() as i64),
        _ => Ok(0),
    }
}

pub fn reboot(magic1: u32, magic2: u32, cmd: u32) -> SysResult {
    if magic1 != 0xfee1_dead || !matches!(magic2, 672274793 | 85072278 | 369367448 | 537993216) {
        return Err(EINVAL);
    }
    crate::drivers::shutdown();
    match cmd {
        0x0123_4567 => crate::acpi::reboot(),
        0x4321_FEDC | 0xCDEF_0123 => crate::acpi::shutdown(),
        _ => Err(EINVAL),
    }
}

pub fn sethostname(name: u64, len: u64) -> SysResult {
    if len > 64 {
        return Err(EINVAL);
    }
    let b = uaccess::read_bytes(name, len as usize)?;
    *HOSTNAME.lock() = String::from_utf8(b).map_err(|_| EINVAL)?;
    Ok(0)
}

pub fn getrandom(buf: u64, len: u64) -> SysResult {
    let len = (len as usize).min(1 << 16);
    let mut tmp = alloc::vec![0u8; len];
    crate::drivers::random::fill(&mut tmp);
    uaccess::copy_to_user(buf, &tmp)?;
    Ok(len as i64)
}
