//! Time, system-information and miscellaneous syscalls.

use super::SysResult;
use crate::errno::*;
use crate::process::{signal, uaccess};
use alloc::string::String;
use spin::Mutex;

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

pub fn clock_gettime(clock: u32, tp: u64) -> SysResult {
    let (s, n) = match clock {
        0 | 8 | 11 => crate::time::realtime(), // REALTIME, *_ALARM, TAI
        _ => {
            let ns = crate::time::nanos();
            (ns / 1_000_000_000, ns % 1_000_000_000)
        }
    };
    uaccess::write_user(tp, &[s as i64, n as i64])?;
    Ok(0)
}

pub fn clock_getres(tp: u64) -> SysResult {
    if tp != 0 {
        uaccess::write_user(tp, &[0i64, 1])?;
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

pub fn time(t: u64) -> SysResult {
    let now = crate::time::unix_time() as i64;
    if t != 0 {
        uaccess::write_user(t, &now)?;
    }
    Ok(now)
}

pub fn alarm(secs: u64) -> SysResult {
    let p = crate::process::current().ok_or(ESRCH)?;
    if secs > 0 {
        let pid = p.pid;
        crate::sched::spawn("alarm", move || {
            crate::time::sleep_ms(secs * 1000);
            if let Some(p) = crate::process::find(pid) {
                signal::send(&p, signal::SIGALRM);
            }
        });
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
    crate::vfs::sync_all();
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
