//! kapitest: exercises the kernel's event, timer, thread and scheduling
//! system calls and prints one PASS/FAIL line per check.

#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use rustos_rt::prelude::*;
use rustos_rt::sys::syscall;
use rustos_rt::time;

rustos_rt::entry!(main);

mod nr {
    pub const READ: usize = 0;
    pub const WRITE: usize = 1;
    pub const CLOSE: usize = 3;
    pub const MMAP: usize = 9;
    pub const MUNMAP: usize = 11;
    pub const MSYNC: usize = 26;
    pub const OPEN: usize = 2;
    pub const PREAD64: usize = 17;
    pub const PWRITE64: usize = 18;
    pub const FTRUNCATE: usize = 77;
    pub const IOCTL: usize = 16;
    pub const RT_SIGPROCMASK: usize = 14;
    pub const PIPE2: usize = 293;
    pub const CLONE: usize = 56;
    pub const FORK: usize = 57;
    pub const EXIT: usize = 60;
    pub const WAIT4: usize = 61;
    pub const KILL: usize = 62;
    pub const GETPID: usize = 39;
    pub const GETITIMER: usize = 36;
    pub const SETITIMER: usize = 38;
    pub const GETPRIORITY: usize = 140;
    pub const SETPRIORITY: usize = 141;
    pub const FUTEX: usize = 202;
    pub const SCHED_SETAFFINITY: usize = 203;
    pub const SCHED_GETAFFINITY: usize = 204;
    pub const EPOLL_CTL: usize = 233;
    pub const EPOLL_WAIT: usize = 232;
    pub const TIMERFD_CREATE: usize = 283;
    pub const TIMERFD_SETTIME: usize = 286;
    pub const TIMERFD_GETTIME: usize = 287;
    pub const SIGNALFD4: usize = 289;
    pub const EVENTFD2: usize = 290;
    pub const EPOLL_CREATE1: usize = 291;
    pub const POLL: usize = 7;
    pub const SELECT: usize = 23;
    pub const SOCKETPAIR: usize = 53;
    pub const FSYNC: usize = 74;
    pub const UNLINK: usize = 87;
    pub const GETRUSAGE: usize = 98;
    pub const TIMES: usize = 100;
    pub const CLOCK_GETTIME: usize = 228;
}

const EAGAIN: isize = -11;
const O_NONBLOCK: usize = 0o4000;
const EPOLLIN: u32 = 1;
const EPOLLONESHOT: u32 = 1 << 30;
const SIGUSR1: usize = 10;
const SIGALRM: usize = 14;

struct Report {
    passed: u32,
    failed: u32,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: String) {
        if ok {
            self.passed += 1;
            println!("PASS {}", name);
        } else {
            self.failed += 1;
            println!("FAIL {}: {}", name, detail);
        }
    }
}

fn sc(n: usize, a: &[usize]) -> isize {
    syscall(n, a)
}

fn read_u64(fd: isize) -> isize {
    let mut v = 0u64;
    let r = sc(nr::READ, &[fd as usize, &mut v as *mut u64 as usize, 8]);
    if r < 0 { r } else { v as isize }
}

fn write_u64(fd: isize, v: u64) -> isize {
    sc(nr::WRITE, &[fd as usize, &v as *const u64 as usize, 8])
}

fn epoll_add(ep: isize, fd: isize, events: u32, data: u64) -> isize {
    let mut ev = [0u8; 12];
    ev[0..4].copy_from_slice(&events.to_ne_bytes());
    ev[4..12].copy_from_slice(&data.to_ne_bytes());
    sc(
        nr::EPOLL_CTL,
        &[ep as usize, 1, fd as usize, ev.as_ptr() as usize],
    )
}

/// epoll_wait: (count, first event's data).
fn epoll_wait(ep: isize, timeout_ms: i32) -> (isize, u64) {
    let mut evs = [0u8; 12 * 8];
    let n = sc(
        nr::EPOLL_WAIT,
        &[
            ep as usize,
            evs.as_mut_ptr() as usize,
            8,
            timeout_ms as isize as usize,
        ],
    );
    (n, u64::from_ne_bytes(evs[4..12].try_into().unwrap()))
}

fn eventfd(r: &mut Report) {
    let fd = sc(nr::EVENTFD2, &[0, O_NONBLOCK]);
    r.check("eventfd create", fd >= 0, format!("{}", fd));
    let empty = read_u64(fd);
    r.check(
        "eventfd empty read is EAGAIN",
        empty == EAGAIN,
        format!("{}", empty),
    );
    write_u64(fd, 3);
    write_u64(fd, 4);
    let v = read_u64(fd);
    r.check("eventfd counter", v == 7, format!("read {}", v));
    let sem = sc(nr::EVENTFD2, &[2, 1 | O_NONBLOCK]);
    let (a, b, c) = (read_u64(sem), read_u64(sem), read_u64(sem));
    r.check(
        "eventfd semaphore",
        (a, b, c) == (1, 1, EAGAIN),
        format!("{} {} {}", a, b, c),
    );
    sc(nr::CLOSE, &[fd as usize]);
    sc(nr::CLOSE, &[sem as usize]);
}

fn timerfd(r: &mut Report) {
    let fd = sc(nr::TIMERFD_CREATE, &[1, 0]);
    r.check("timerfd create", fd >= 0, format!("{}", fd));
    // First expiry after 50 ms, then every 20 ms.
    let spec: [i64; 4] = [0, 20_000_000, 0, 50_000_000];
    let t0 = time::millis();
    let s = sc(
        nr::TIMERFD_SETTIME,
        &[fd as usize, 0, spec.as_ptr() as usize, 0],
    );
    let n = read_u64(fd);
    let dt = time::millis() - t0;
    r.check(
        "timerfd first expiry",
        s == 0 && n >= 1 && (40..500).contains(&dt),
        format!("n={} after {} ms", n, dt),
    );
    time::sleep_ms(110);
    let n = read_u64(fd);
    r.check(
        "timerfd periodic",
        (3..=8).contains(&n),
        format!("{} expirations in 110 ms", n),
    );
    let mut cur = [0i64; 4];
    sc(
        nr::TIMERFD_GETTIME,
        &[fd as usize, cur.as_mut_ptr() as usize],
    );
    r.check(
        "timerfd gettime",
        cur[1] == 20_000_000 && cur[3] > 0,
        format!("{:?}", cur),
    );
    let off = [0i64; 4];
    sc(
        nr::TIMERFD_SETTIME,
        &[fd as usize, 0, off.as_ptr() as usize, 0],
    );
    sc(nr::CLOSE, &[fd as usize]);
}

fn epoll(r: &mut Report) {
    let ep = sc(nr::EPOLL_CREATE1, &[0]);
    r.check("epoll create", ep >= 0, format!("{}", ep));
    let efd = sc(nr::EVENTFD2, &[0, O_NONBLOCK]);
    let mut p = [0i32; 2];
    sc(nr::PIPE2, &[p.as_mut_ptr() as usize, O_NONBLOCK]);
    epoll_add(ep, efd, EPOLLIN, 111);
    epoll_add(ep, p[0] as isize, EPOLLIN | EPOLLONESHOT, 222);
    let (n, _) = epoll_wait(ep, 0);
    r.check("epoll nothing ready", n == 0, format!("{}", n));
    write_u64(efd, 1);
    let (n, d) = epoll_wait(ep, 1000);
    r.check(
        "epoll eventfd ready",
        n == 1 && d == 111,
        format!("n={} data={}", n, d),
    );
    read_u64(efd);
    sc(nr::WRITE, &[p[1] as usize, b"x".as_ptr() as usize, 1]);
    let (n, d) = epoll_wait(ep, 1000);
    r.check(
        "epoll pipe ready",
        n == 1 && d == 222,
        format!("n={} data={}", n, d),
    );
    let (n, _) = epoll_wait(ep, 50);
    r.check("epoll one-shot disarmed", n == 0, format!("{}", n));
    // A blocking wait woken by a timer.
    let tfd = sc(nr::TIMERFD_CREATE, &[1, 0]);
    let spec: [i64; 4] = [0, 0, 0, 30_000_000];
    sc(
        nr::TIMERFD_SETTIME,
        &[tfd as usize, 0, spec.as_ptr() as usize, 0],
    );
    epoll_add(ep, tfd, EPOLLIN, 333);
    let t0 = time::millis();
    let (n, d) = epoll_wait(ep, 2000);
    let dt = time::millis() - t0;
    r.check(
        "epoll wakes on timerfd",
        n == 1 && d == 333 && dt < 1000,
        format!("n={} data={} after {} ms", n, d, dt),
    );
    for fd in [ep, efd, tfd, p[0] as isize, p[1] as isize] {
        sc(nr::CLOSE, &[fd as usize]);
    }
}

fn sigmask_block(sigs: u64) -> u64 {
    let mut old = 0u64;
    sc(
        nr::RT_SIGPROCMASK,
        &[
            0,
            &sigs as *const u64 as usize,
            &mut old as *mut u64 as usize,
            8,
        ],
    );
    old
}

fn signalfd_read(fd: isize) -> isize {
    let mut info = [0u8; 128];
    let n = sc(nr::READ, &[fd as usize, info.as_mut_ptr() as usize, 128]);
    if n < 0 {
        n
    } else {
        u32::from_ne_bytes(info[0..4].try_into().unwrap()) as isize
    }
}

fn signals(r: &mut Report) {
    let mask = (1u64 << (SIGUSR1 - 1)) | (1u64 << (SIGALRM - 1));
    let old = sigmask_block(mask);
    let fd = sc(
        nr::SIGNALFD4,
        &[usize::MAX, &mask as *const u64 as usize, 8, 0],
    );
    r.check("signalfd create", fd >= 0, format!("{}", fd));
    let pid = sc(nr::GETPID, &[]);
    sc(nr::KILL, &[pid as usize, SIGUSR1]);
    let s = signalfd_read(fd);
    r.check(
        "signalfd reads SIGUSR1",
        s == SIGUSR1 as isize,
        format!("{}", s),
    );
    // ITIMER_REAL: 40 ms one-shot, delivered through the signalfd.
    let itv: [i64; 4] = [0, 0, 0, 40_000];
    let t0 = time::millis();
    sc(nr::SETITIMER, &[0, itv.as_ptr() as usize, 0]);
    let mut cur = [0i64; 4];
    sc(nr::GETITIMER, &[0, cur.as_mut_ptr() as usize]);
    let s = signalfd_read(fd);
    let dt = time::millis() - t0;
    r.check(
        "setitimer SIGALRM",
        s == SIGALRM as isize && (30..1000).contains(&dt) && cur[3] > 0,
        format!("sig {} after {} ms (remaining {:?})", s, dt, cur),
    );
    sc(nr::CLOSE, &[fd as usize]);
    let mut o = old;
    sc(nr::RT_SIGPROCMASK, &[2, &mut o as *mut u64 as usize, 0, 8]);
}

// ---------------------------------------------------------------------------
// Threads
// ---------------------------------------------------------------------------

const CLONE_VM: usize = 0x100;
const CLONE_FS: usize = 0x200;
const CLONE_FILES: usize = 0x400;
const CLONE_SIGHAND: usize = 0x800;
const CLONE_THREAD: usize = 0x10000;
const CLONE_SYSVSEM: usize = 0x40000;
const CLONE_PARENT_SETTID: usize = 0x100000;
const CLONE_CHILD_CLEARTID: usize = 0x200000;

const STACK: usize = 64 * 1024;

/// Start a thread running `f(arg)` on a fresh stack; `tid` receives its
/// id and is cleared (with a futex wake) when it exits.
fn spawn_thread(f: extern "C" fn(usize), arg: usize, tid: &AtomicU32) -> isize {
    let stack = sc(nr::MMAP, &[0, STACK, 3, 0x22, usize::MAX, 0]);
    if stack < 0 {
        return stack;
    }
    let top = (stack as usize + STACK) & !15;
    unsafe {
        *((top - 16) as *mut usize) = f as usize;
        *((top - 8) as *mut usize) = arg;
    }
    let flags = CLONE_VM
        | CLONE_FS
        | CLONE_FILES
        | CLONE_SIGHAND
        | CLONE_THREAD
        | CLONE_SYSVSEM
        | CLONE_PARENT_SETTID
        | CLONE_CHILD_CLEARTID;
    let tidp = tid.as_ptr() as usize;
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "syscall",
            "test rax, rax",
            "jnz 2f",
            // Child: fn and arg are on the new stack.
            "pop rax",
            "pop rdi",
            "and rsp, -16",
            "call rax",
            "mov eax, 60",
            "xor edi, edi",
            "syscall",
            "ud2",
            "2:",
            inlateout("rax") nr::CLONE => ret,
            in("rdi") flags,
            in("rsi") top - 16,
            in("rdx") tidp,
            in("r10") tidp,
            in("r8") 0usize,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }
    ret
}

fn futex_wait(word: &AtomicU32, val: u32, timeout_ms: u64) -> isize {
    let ts: [i64; 2] = [
        (timeout_ms / 1000) as i64,
        ((timeout_ms % 1000) * 1_000_000) as i64,
    ];
    sc(
        nr::FUTEX,
        &[
            word.as_ptr() as usize,
            0,
            val as usize,
            ts.as_ptr() as usize,
        ],
    )
}

fn futex_wake(word: &AtomicU32, n: u32) -> isize {
    sc(nr::FUTEX, &[word.as_ptr() as usize, 1, n as usize])
}

/// Wait for a thread started with `spawn_thread` to exit.
fn join(tid: &AtomicU32) -> bool {
    let t0 = time::millis();
    loop {
        let v = tid.load(Ordering::SeqCst);
        if v == 0 {
            return true;
        }
        if time::millis() - t0 > 5000 {
            return false;
        }
        futex_wait(tid, v, 500);
    }
}

static GATE: AtomicU32 = AtomicU32::new(0);
static WOKEN: AtomicU32 = AtomicU32::new(0);

extern "C" fn waiter(_arg: usize) {
    while GATE.load(Ordering::SeqCst) == 0 {
        futex_wait(&GATE, 0, 2000);
    }
    WOKEN.fetch_add(1, Ordering::SeqCst);
}

static COUNTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
static STOP: AtomicU32 = AtomicU32::new(0);

extern "C" fn spinner(i: usize) {
    while STOP.load(Ordering::Relaxed) == 0 {
        for _ in 0..1000 {
            core::hint::spin_loop();
        }
        COUNTS[i].fetch_add(1, Ordering::Relaxed);
    }
}

fn threads(r: &mut Report) {
    let tids = [AtomicU32::new(0), AtomicU32::new(0)];
    for t in &tids {
        let rc = spawn_thread(waiter, 0, t);
        r.check(
            "clone thread",
            rc > 0 && t.load(Ordering::SeqCst) == rc as u32,
            format!("rc={}", rc),
        );
    }
    time::sleep_ms(50);
    r.check(
        "futex waiters sleeping",
        WOKEN.load(Ordering::SeqCst) == 0,
        String::from("woke early"),
    );
    GATE.store(1, Ordering::SeqCst);
    let n = futex_wake(&GATE, u32::MAX);
    let joined = tids.iter().all(join);
    r.check(
        "futex wake + CLEARTID join",
        joined && WOKEN.load(Ordering::SeqCst) == 2,
        format!(
            "wake returned {}, woken {}, joined {}",
            n,
            WOKEN.load(Ordering::SeqCst),
            joined
        ),
    );
    let word = AtomicU32::new(5);
    let t0 = time::millis();
    let rc = futex_wait(&word, 5, 60);
    let dt = time::millis() - t0;
    r.check(
        "futex wait timeout",
        rc == -110 && dt >= 50,
        format!("rc={} after {} ms", rc, dt),
    );
    r.check(
        "futex value mismatch",
        futex_wait(&word, 6, 10) == EAGAIN,
        String::new(),
    );

    // Fairness: four busy threads share the CPUs.
    let tids = [const { AtomicU32::new(0) }; 4];
    for (i, t) in tids.iter().enumerate() {
        spawn_thread(spinner, i, t);
    }
    time::sleep_ms(600);
    STOP.store(1, Ordering::SeqCst);
    let all = tids.iter().all(join);
    let c: Vec<u64> = COUNTS.iter().map(|c| c.load(Ordering::SeqCst)).collect();
    let (lo, hi) = (*c.iter().min().unwrap(), *c.iter().max().unwrap());
    r.check(
        "scheduler fairness (4 threads)",
        all && lo > 0 && lo * 4 >= hi,
        format!("counts {:?}", c),
    );
}

fn scheduling(r: &mut Report) {
    let mut mask = 0u64;
    let n = sc(
        nr::SCHED_GETAFFINITY,
        &[0, 8, &mut mask as *mut u64 as usize],
    );
    r.check(
        "sched_getaffinity",
        n == 8 && mask != 0,
        format!("n={} mask={:#x}", n, mask),
    );
    let one = 1u64;
    let s = sc(nr::SCHED_SETAFFINITY, &[0, 8, &one as *const u64 as usize]);
    let mut got = 0u64;
    sc(
        nr::SCHED_GETAFFINITY,
        &[0, 8, &mut got as *mut u64 as usize],
    );
    r.check(
        "sched_setaffinity",
        s == 0 && got == 1,
        format!("rc={} mask={:#x}", s, got),
    );
    sc(nr::SCHED_SETAFFINITY, &[0, 8, &mask as *const u64 as usize]);
    let bad = 0u64;
    r.check(
        "sched_setaffinity empty mask",
        sc(nr::SCHED_SETAFFINITY, &[0, 8, &bad as *const u64 as usize]) == -22,
        String::new(),
    );
    sc(nr::SETPRIORITY, &[0, 0, 5]);
    let p = sc(nr::GETPRIORITY, &[0, 0]);
    r.check("setpriority/getpriority", p == 15, format!("{}", p));
    sc(nr::SETPRIORITY, &[0, 0, 0]);
}

fn fork_exit(r: &mut Report) {
    // A child process exits normally and is reaped.
    let pid = sc(nr::FORK, &[]);
    if pid == 0 {
        sc(nr::EXIT, &[7]);
    }
    let mut st = 0i32;
    let w = sc(
        nr::WAIT4,
        &[pid as usize, &mut st as *mut i32 as usize, 0, 0],
    );
    r.check(
        "fork + exit status",
        w == pid && (st >> 8) & 0xFF == 7,
        format!("wait {} status {:#x}", w, st),
    );
}

const CLOCK_PROCESS_CPUTIME_ID: usize = 2;
const CLOCK_THREAD_CPUTIME_ID: usize = 3;

fn clock_ns(id: usize) -> u64 {
    let mut ts = [0i64; 2];
    sc(nr::CLOCK_GETTIME, &[id, ts.as_mut_ptr() as usize]);
    ts[0] as u64 * 1_000_000_000 + ts[1] as u64
}

/// Spin in user mode for `ms` milliseconds of wall time.
fn busy(ms: u64) {
    let end = time::millis() + ms;
    let mut x = 1u64;
    while time::millis() < end {
        for i in 0..200_000u64 {
            x = core::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
        }
    }
    core::hint::black_box(x);
}

/// getrusage: (user, system) time in microseconds.
fn rusage(who: isize) -> (u64, u64) {
    let mut ru = [0i64; 18];
    sc(nr::GETRUSAGE, &[who as usize, ru.as_mut_ptr() as usize]);
    (
        (ru[0] * 1_000_000 + ru[1]) as u64,
        (ru[2] * 1_000_000 + ru[3]) as u64,
    )
}

/// The numeric fields of a /proc/stat "cpu" line.
fn cpu_fields(line: &str) -> Option<Vec<u64>> {
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|f| f.parse().ok())
        .collect::<Option<_>>()?;
    (v.len() == 10).then_some(v)
}

fn cputime(r: &mut Report) {
    let (p0, t0, w0) = (
        clock_ns(CLOCK_PROCESS_CPUTIME_ID),
        clock_ns(CLOCK_THREAD_CPUTIME_ID),
        time::millis(),
    );
    let (u0, _) = rusage(0);
    busy(400);
    let wall = (time::millis() - w0) * 1_000_000;
    let (p1, t1) = (
        clock_ns(CLOCK_PROCESS_CPUTIME_ID),
        clock_ns(CLOCK_THREAD_CPUTIME_ID),
    );
    let (u1, _) = rusage(0);
    // Another CPU-bound task may share this CPU: ask for a quarter.
    r.check(
        "CPU time advances under a busy loop",
        p1 - p0 >= wall / 4 && p1 - p0 <= wall + 20_000_000 && t1 - t0 >= wall / 4,
        format!(
            "process {} ns, thread {} ns, wall {} ns",
            p1 - p0,
            t1 - t0,
            wall
        ),
    );
    r.check(
        "getrusage(RUSAGE_SELF) user time",
        u1 - u0 >= wall / 4000,
        format!("{} -> {} us", u0, u1),
    );
    let mut tms = [0u64; 4];
    sc(nr::TIMES, &[tms.as_mut_ptr() as usize]);
    r.check("times() user ticks", tms[0] >= 5, format!("{:?}", tms));
    let stat = rustos_rt::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = stat
        .rfind(')')
        .map_or(Vec::new(), |i| stat[i + 2..].split_whitespace().collect());
    let utime = f.get(11).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    r.check(
        "/proc/self/stat utime",
        f.len() == 50 && utime >= 5,
        format!("{} fields, utime {}", f.len() + 2, utime),
    );

    // Sleeping takes no CPU time.
    let s0 = clock_ns(CLOCK_PROCESS_CPUTIME_ID);
    time::sleep_ms(300);
    let s1 = clock_ns(CLOCK_PROCESS_CPUTIME_ID);
    r.check(
        "sleeping uses no CPU time",
        s1 - s0 < 50_000_000,
        format!("{} ns", s1 - s0),
    );

    // A reaped child's time goes to wait4's rusage and RUSAGE_CHILDREN.
    let (c0, _) = rusage(-1);
    let pid = sc(nr::FORK, &[]);
    if pid == 0 {
        busy(300);
        sc(nr::EXIT, &[0]);
    }
    let mut st = 0i32;
    let mut ru = [0i64; 18];
    sc(
        nr::WAIT4,
        &[
            pid as usize,
            &mut st as *mut i32 as usize,
            0,
            ru.as_mut_ptr() as usize,
        ],
    );
    let (c1, _) = rusage(-1);
    let child_us = (ru[0] * 1_000_000 + ru[1]) as u64;
    sc(nr::TIMES, &[tms.as_mut_ptr() as usize]);
    r.check(
        "children CPU time (wait4, RUSAGE_CHILDREN, times)",
        child_us >= 50_000 && c1 - c0 >= child_us && tms[2] >= 5,
        format!(
            "wait4 {} us, children {} -> {} us, tms {:?}",
            child_us, c0, c1, tms
        ),
    );

    // /proc/stat: aggregate and per-CPU lines that advance.
    let read = || rustos_rt::fs::read_to_string("/proc/stat").unwrap_or_default();
    let a = read();
    time::sleep_ms(200);
    let b = read();
    let total = |s: &str| {
        s.lines()
            .next()
            .filter(|l| l.starts_with("cpu "))
            .and_then(cpu_fields)
            .map_or(0, |v| v.iter().sum::<u64>())
    };
    let ncpu = b
        .lines()
        .filter(|l| l.starts_with("cpu") && !l.starts_with("cpu "))
        .filter(|l| cpu_fields(l).is_some())
        .count();
    let cpus = rustos_rt::fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("processor"))
        .count();
    let keys = [
        "intr ",
        "ctxt ",
        "btime ",
        "processes ",
        "procs_running ",
        "procs_blocked ",
    ];
    r.check(
        "/proc/stat parses",
        total(&a) > 0
            && total(&b) > total(&a)
            && ncpu == cpus
            && keys.iter().all(|k| b.lines().any(|l| l.starts_with(k))),
        format!(
            "{} -> {}, {} cpu lines, {} cpus",
            total(&a),
            total(&b),
            ncpu,
            cpus
        ),
    );
    let up = rustos_rt::fs::read_to_string("/proc/uptime").unwrap_or_default();
    let idle_ok = up
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.split_once('.'))
        .and_then(|(i, f)| Some(i.parse::<u64>().ok()? * 100 + f.parse::<u64>().ok()?))
        .is_some_and(|idle| idle > 0);
    r.check("/proc/uptime idle time", idle_ok, up.trim().to_string());
    let la = rustos_rt::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let f: Vec<&str> = la.split_whitespace().collect();
    let ok = f.len() == 5
        && f[..3].iter().all(|v| {
            v.split_once('.').is_some_and(|(i, d)| {
                i.parse::<u64>().is_ok() && d.len() == 2 && d.parse::<u64>().is_ok()
            })
        })
        && f[3].split_once('/').is_some_and(|(a, b)| {
            a.parse::<u64>().is_ok_and(|a| a >= 1) && b.parse::<u64>().is_ok()
        })
        && f[4].parse::<u64>().is_ok();
    r.check("/proc/loadavg parses", ok, la.trim().to_string());
}

const PROT_RW: usize = 3;
const MAP_SHARED: usize = 1;
const MAP_PRIVATE: usize = 2;
const MAP_ANON: usize = 0x20;

fn mmap(len: usize, flags: usize, fd: isize, off: usize) -> isize {
    sc(nr::MMAP, &[0, len, PROT_RW, flags, fd as usize, off])
}

fn memory(r: &mut Report) {
    // Shared anonymous memory stays shared with a forked child, even for
    // pages first touched after the fork.
    let a = mmap(8192, MAP_SHARED | MAP_ANON, -1, 0);
    r.check("mmap shared anonymous", a > 0, format!("{}", a));
    let pid = sc(nr::FORK, &[]);
    if pid == 0 {
        unsafe {
            *(a as *mut u32) = 0xC0FFEE;
            *((a as usize + 4096) as *mut u32) = 0xBEEF;
        }
        sc(nr::EXIT, &[0]);
    }
    let mut st = 0i32;
    sc(
        nr::WAIT4,
        &[pid as usize, &mut st as *mut i32 as usize, 0, 0],
    );
    let (x, y) = unsafe { (*(a as *const u32), *((a as usize + 4096) as *const u32)) };
    r.check(
        "MAP_SHARED|MAP_ANONYMOUS across fork",
        x == 0xC0FFEE && y == 0xBEEF,
        format!("{:#x} {:#x}", x, y),
    );
    sc(nr::MUNMAP, &[a as usize, 8192]);

    // File mappings.
    let path = b"/tmp/kapitest.map\0";
    let fd = sc(nr::OPEN, &[path.as_ptr() as usize, 0o102 | 0o1000, 0o644]); // O_RDWR|O_CREAT|O_TRUNC
    let data = [b'a'; 6000];
    sc(
        nr::PWRITE64,
        &[fd as usize, data.as_ptr() as usize, data.len(), 0],
    );
    let sh = mmap(8192, MAP_SHARED, fd, 0);
    let pv = mmap(8192, MAP_PRIVATE, fd, 0);
    r.check("mmap file", sh > 0 && pv > 0, format!("{} {}", sh, pv));
    let first = unsafe { *(sh as *const u8) };
    r.check(
        "file mapping reads file data",
        first == b'a',
        format!("{}", first),
    );
    unsafe {
        *((sh as usize + 10) as *mut u8) = b'S';
        *((pv as usize + 20) as *mut u8) = b'P';
    }
    let mut buf = [0u8; 32];
    sc(
        nr::PREAD64,
        &[fd as usize, buf.as_mut_ptr() as usize, 32, 0],
    );
    r.check(
        "shared mapping write visible to read()",
        buf[10] == b'S',
        format!("{:?}", &buf[..24]),
    );
    r.check(
        "private mapping write stays private",
        buf[20] == b'a',
        format!("{:?}", &buf[..24]),
    );
    let seen = unsafe { *((pv as usize + 10) as *const u8) };
    r.check(
        "private mapping sees shared data",
        seen == b'S',
        format!("{}", seen),
    );
    // write() updates the mapped page.
    sc(nr::PWRITE64, &[fd as usize, b"W".as_ptr() as usize, 1, 30]);
    let w = unsafe { *((sh as usize + 30) as *const u8) };
    r.check("write() visible in mapping", w == b'W', format!("{}", w));
    let rc = sc(nr::MSYNC, &[sh as usize, 8192, 4]);
    r.check("msync", rc == 0, format!("{}", rc));
    sc(nr::MUNMAP, &[sh as usize, 8192]);
    sc(nr::MUNMAP, &[pv as usize, 8192]);
    // Reopen: the data reached the file.
    sc(nr::CLOSE, &[fd as usize]);
    let fd = sc(nr::OPEN, &[path.as_ptr() as usize, 2, 0]);
    let mut buf = [0u8; 32];
    sc(
        nr::PREAD64,
        &[fd as usize, buf.as_mut_ptr() as usize, 32, 0],
    );
    r.check(
        "msync/munmap wrote the file",
        buf[10] == b'S' && buf[30] == b'W',
        format!("{:?}", &buf[..32]),
    );
    sc(nr::FTRUNCATE, &[fd as usize, 0]);
    sc(nr::CLOSE, &[fd as usize]);
}

fn read_some(fd: isize, want: usize, ms: u64) -> Vec<u8> {
    // Non-blocking reads until `want` bytes or the time runs out.
    let mut out = Vec::new();
    let t0 = time::millis();
    while out.len() < want && time::millis() - t0 < ms {
        let mut b = [0u8; 256];
        let n = sc(nr::READ, &[fd as usize, b.as_mut_ptr() as usize, b.len()]);
        if n > 0 {
            out.extend_from_slice(&b[..n as usize]);
        } else {
            time::sleep_ms(5);
        }
    }
    out
}

fn pty(r: &mut Report) {
    const O_RDWR: usize = 2;
    const O_NOCTTY: usize = 0o400;
    let m = sc(
        nr::OPEN,
        &[
            b"/dev/ptmx\0".as_ptr() as usize,
            O_RDWR | O_NOCTTY | O_NONBLOCK,
            0,
        ],
    );
    r.check("open /dev/ptmx", m >= 0, format!("{}", m));
    let mut n = 0u32;
    sc(
        nr::IOCTL,
        &[m as usize, 0x8004_5430, &mut n as *mut u32 as usize],
    );
    let path = format!("/dev/pts/{}\0", n);
    let locked = sc(nr::OPEN, &[path.as_ptr() as usize, O_RDWR | O_NOCTTY, 0]);
    r.check(
        "slave locked until unlockpt",
        locked == -5,
        format!("{}", locked),
    );
    let zero = 0i32;
    sc(
        nr::IOCTL,
        &[m as usize, 0x4004_5431, &zero as *const i32 as usize],
    );
    let s = sc(
        nr::OPEN,
        &[path.as_ptr() as usize, O_RDWR | O_NOCTTY | O_NONBLOCK, 0],
    );
    r.check(
        "open slave",
        s >= 0,
        format!("{} {}", path.trim_end_matches('\0'), s),
    );
    // Typed input: echoed to the master, delivered as a line to the slave.
    sc(nr::WRITE, &[m as usize, b"hello\r".as_ptr() as usize, 6]);
    let echo = read_some(m, 7, 1000);
    r.check(
        "pty echo",
        echo == b"hello\r\n",
        format!("{:?}", core::str::from_utf8(&echo)),
    );
    let line = read_some(s, 6, 1000);
    r.check(
        "pty canonical input",
        line == b"hello\n",
        format!("{:?}", core::str::from_utf8(&line)),
    );
    // Program output: NL becomes CR NL on the master side.
    sc(nr::WRITE, &[s as usize, b"out\n".as_ptr() as usize, 4]);
    let out = read_some(m, 5, 1000);
    r.check(
        "pty output",
        out == b"out\r\n",
        format!("{:?}", core::str::from_utf8(&out)),
    );
    // Window size set on the master is seen on the slave.
    let ws: [u16; 4] = [30, 100, 0, 0];
    sc(nr::IOCTL, &[m as usize, 0x5414, ws.as_ptr() as usize]);
    let mut got = [0u16; 4];
    sc(nr::IOCTL, &[s as usize, 0x5413, got.as_mut_ptr() as usize]);
    r.check(
        "pty window size",
        got[0] == 30 && got[1] == 100,
        format!("{:?}", got),
    );
    // Closing the master hangs up the slave.
    sc(nr::CLOSE, &[m as usize]);
    let mut b = [0u8; 8];
    let eof = sc(nr::READ, &[s as usize, b.as_mut_ptr() as usize, 8]);
    r.check("pty hangup gives EOF", eof == 0, format!("{}", eof));
    sc(nr::CLOSE, &[s as usize]);
}

static WAKE_FD: AtomicU32 = AtomicU32::new(0);
static WAKE_DELAY: AtomicU32 = AtomicU32::new(0);

/// Thread: sleep, then write one byte to `WAKE_FD`.
extern "C" fn late_writer(_arg: usize) {
    time::sleep_ms(WAKE_DELAY.load(Ordering::SeqCst) as u64);
    let fd = WAKE_FD.load(Ordering::SeqCst) as usize;
    sc(nr::WRITE, &[fd, b"x".as_ptr() as usize, 1]);
}

/// Start `late_writer` on `fd` after `ms`; returns the tid cell to join.
fn write_later(fd: isize, ms: u32, tid: &AtomicU32) {
    WAKE_FD.store(fd as u32, Ordering::SeqCst);
    WAKE_DELAY.store(ms, Ordering::SeqCst);
    spawn_thread(late_writer, 0, tid);
}

fn poll1(fd: isize, timeout_ms: i32) -> isize {
    let mut p = [0u8; 8];
    p[0..4].copy_from_slice(&(fd as i32).to_ne_bytes());
    p[4..6].copy_from_slice(&1i16.to_ne_bytes()); // POLLIN
    sc(
        nr::POLL,
        &[p.as_mut_ptr() as usize, 1, timeout_ms as isize as usize],
    )
}

/// Wake-ups come from the object's own wait queue: prompt, and no early
/// return while nothing happens.
fn wakeups(r: &mut Report) {
    let mut fds = [0i32; 2];
    sc(nr::PIPE2, &[fds.as_mut_ptr() as usize, 0]);
    let (rd, wr) = (fds[0] as isize, fds[1] as isize);
    let t0 = time::millis();
    let n = poll1(rd, 200);
    let dt = time::millis() - t0;
    r.check(
        "poll idle pipe sleeps the timeout",
        n == 0 && dt >= 190,
        format!("n={} {}ms", n, dt),
    );

    let tid = AtomicU32::new(0);
    write_later(wr, 30, &tid);
    let t0 = time::millis();
    let n = poll1(rd, 2000);
    let dt = time::millis() - t0;
    join(&tid);
    r.check(
        "poll wakes on pipe write",
        n == 1 && dt < 30 + 25,
        format!("n={} {}ms", n, dt),
    );
    let mut b = [0u8; 8];
    sc(nr::READ, &[rd as usize, b.as_mut_ptr() as usize, 8]);

    // select() on the same pipe.
    write_later(wr, 30, &tid);
    let mut set = [0u64; 1];
    set[0] = 1 << rd;
    let tv: [i64; 2] = [2, 0];
    let t0 = time::millis();
    let n = sc(
        nr::SELECT,
        &[
            rd as usize + 1,
            set.as_mut_ptr() as usize,
            0,
            0,
            tv.as_ptr() as usize,
        ],
    );
    let dt = time::millis() - t0;
    join(&tid);
    r.check(
        "select wakes on pipe write",
        n == 1 && dt < 30 + 25,
        format!("n={} {}ms", n, dt),
    );
    sc(nr::READ, &[rd as usize, b.as_mut_ptr() as usize, 8]);

    // epoll: a socketpair and an eventfd; edge-triggered on the socket.
    let mut sv = [0i32; 2];
    let rc = sc(nr::SOCKETPAIR, &[1, 1, 0, sv.as_mut_ptr() as usize]); // AF_UNIX, SOCK_STREAM
    r.check("socketpair", rc == 0, format!("{}", rc));
    let ep = sc(nr::EPOLL_CREATE1, &[0]);
    let efd = sc(nr::EVENTFD2, &[0, 0]);
    epoll_add(ep, sv[0] as isize, EPOLLIN | (1 << 31), 7); // EPOLLET
    epoll_add(ep, efd, EPOLLIN, 9);
    write_later(sv[1] as isize, 30, &tid);
    let t0 = time::millis();
    let (n, data) = epoll_wait(ep, 2000);
    let dt = time::millis() - t0;
    join(&tid);
    r.check(
        "epoll wakes on socketpair",
        n == 1 && data == 7 && dt < 30 + 25,
        format!("n={} data={} {}ms", n, data, dt),
    );
    let (n, _) = epoll_wait(ep, 100);
    r.check(
        "epoll edge-triggered: no repeat",
        n == 0,
        format!("n={}", n),
    );
    write_later(sv[1] as isize, 10, &tid);
    let (n, data) = epoll_wait(ep, 2000);
    join(&tid);
    r.check(
        "epoll edge-triggered: new data",
        n == 1 && data == 7,
        format!("n={} data={}", n, data),
    );
    // Nested: poll() on the epoll descriptor itself.
    let t0 = time::millis();
    let n = poll1(ep, 300);
    let dt = time::millis() - t0;
    r.check(
        "poll on idle epoll fd",
        n == 0 && dt >= 290,
        format!("n={} {}ms", n, dt),
    );
    write_later(sv[1] as isize, 30, &tid);
    let t0 = time::millis();
    let n = poll1(ep, 2000);
    let dt = time::millis() - t0;
    join(&tid);
    r.check(
        "poll on epoll fd wakes",
        n == 1 && dt < 30 + 25,
        format!("n={} {}ms", n, dt),
    );
    write_u64(efd, 1);
    let n = poll1(ep, 300);
    r.check("poll on epoll fd sees eventfd", n == 1, format!("n={}", n));
    for fd in [rd, wr, sv[0] as isize, sv[1] as isize, ep, efd] {
        sc(nr::CLOSE, &[fd as usize]);
    }
}

/// Graphics mode on the console (KDSETMODE / KDGETMODE / VT_SETMODE).
fn vt(r: &mut Report) {
    const KDSETMODE: usize = 0x4B3A;
    const KDGETMODE: usize = 0x4B3B;
    const VT_GETMODE: usize = 0x5601;
    const VT_SETMODE: usize = 0x5602;
    let mut mode = 9u32;
    let rc = sc(nr::IOCTL, &[0, KDSETMODE, 1]);
    sc(nr::IOCTL, &[0, KDGETMODE, &mut mode as *mut u32 as usize]);
    r.check(
        "KDSETMODE KD_GRAPHICS",
        rc == 0 && mode == 1,
        format!("rc={} mode={}", rc, mode),
    );
    // VT_PROCESS with SIGUSR1/SIGUSR2 for release/acquire.
    let mut vm = [0u8; 8];
    vm[0] = 1;
    vm[2..4].copy_from_slice(&10i16.to_ne_bytes());
    vm[4..6].copy_from_slice(&12i16.to_ne_bytes());
    let rc = sc(nr::IOCTL, &[0, VT_SETMODE, vm.as_ptr() as usize]);
    let mut got = [0u8; 8];
    sc(nr::IOCTL, &[0, VT_GETMODE, got.as_mut_ptr() as usize]);
    r.check(
        "VT_SETMODE VT_PROCESS",
        rc == 0 && got[..6] == vm[..6],
        format!("rc={} {:?}", rc, got),
    );
    vm = [0; 8];
    sc(nr::IOCTL, &[0, VT_SETMODE, vm.as_ptr() as usize]);
    let rc = sc(nr::IOCTL, &[0, KDSETMODE, 0]);
    sc(nr::IOCTL, &[0, KDGETMODE, &mut mode as *mut u32 as usize]);
    r.check(
        "KDSETMODE KD_TEXT",
        rc == 0 && mode == 0,
        format!("rc={} mode={}", rc, mode),
    );
}

/// read()/write()/mmap coherency and fsync on a file under `dir`.
fn files(r: &mut Report, dir: &str) {
    let path = format!("{}/kapitest.io\0", dir);
    let fd = sc(nr::OPEN, &[path.as_ptr() as usize, 0o102 | 0o1000, 0o644]);
    r.check("create test file", fd >= 0, format!("{} {}", fd, dir));
    if fd < 0 {
        return;
    }
    // 1 MiB of a position-dependent pattern, written in odd-sized chunks.
    let pat = |i: usize| (i * 7 + i / 4096) as u8;
    let data: Vec<u8> = (0..1 << 20).map(pat).collect();
    let mut off = 0;
    while off < data.len() {
        let n = (data.len() - off).min(12345);
        sc(
            nr::PWRITE64,
            &[fd as usize, data[off..].as_ptr() as usize, n, off],
        );
        off += n;
    }
    let mut back = alloc::vec![0u8; data.len()];
    let mut off = 0;
    while off < back.len() {
        let n = sc(
            nr::PREAD64,
            &[fd as usize, back[off..].as_mut_ptr() as usize, 65536, off],
        );
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
    let bad = back.iter().zip(&data).position(|(a, b)| a != b);
    r.check(
        "read() returns written data",
        off == data.len() && bad.is_none(),
        format!("got {} first diff {:?}", off, bad),
    );
    // Overwrite inside cached pages; read sees it.
    sc(
        nr::PWRITE64,
        &[fd as usize, b"HELLO".as_ptr() as usize, 5, 4094],
    );
    let mut b = [0u8; 8];
    sc(
        nr::PREAD64,
        &[fd as usize, b.as_mut_ptr() as usize, 8, 4093],
    );
    r.check(
        "overwrite visible to read()",
        &b[1..6] == b"HELLO" && b[0] == pat(4093),
        format!("{:?}", b),
    );
    // Shared mapping store -> read(); write() -> mapping.
    let m = mmap(8192, MAP_SHARED, fd, 0);
    unsafe { *((m as usize + 100) as *mut u8) = b'M' };
    sc(nr::PREAD64, &[fd as usize, b.as_mut_ptr() as usize, 1, 100]);
    r.check(
        "mapping store visible to read()",
        b[0] == b'M',
        format!("{}", b[0]),
    );
    sc(nr::PWRITE64, &[fd as usize, b"w".as_ptr() as usize, 1, 200]);
    let w = unsafe { *((m as usize + 200) as *const u8) };
    r.check("write() visible in mapping", w == b'w', format!("{}", w));
    sc(nr::MUNMAP, &[m as usize, 8192]);
    // Truncate: reads stop at the new size; growing again reads zeros.
    sc(nr::FTRUNCATE, &[fd as usize, 5000]);
    let n = sc(
        nr::PREAD64,
        &[fd as usize, b.as_mut_ptr() as usize, 8, 4998],
    );
    r.check("truncate shortens reads", n == 2, format!("{}", n));
    sc(nr::FTRUNCATE, &[fd as usize, 9000]);
    let n = sc(
        nr::PREAD64,
        &[fd as usize, b.as_mut_ptr() as usize, 8, 5000],
    );
    r.check(
        "truncate-extend reads zeros",
        n == 8 && b == [0; 8],
        format!("{} {:?}", n, b),
    );
    let rc = sc(nr::FSYNC, &[fd as usize]);
    r.check("fsync", rc == 0, format!("{}", rc));
    sc(nr::CLOSE, &[fd as usize]);
    // Keep a file for the scenario to check after a remount.
    let keep = format!("{}/kapitest.keep\0", dir);
    let fd = sc(nr::OPEN, &[keep.as_ptr() as usize, 0o102 | 0o1000, 0o644]);
    sc(
        nr::PWRITE64,
        &[fd as usize, data.as_ptr() as usize, 300_000, 0],
    );
    sc(nr::FSYNC, &[fd as usize]);
    sc(nr::CLOSE, &[fd as usize]);
    sc(nr::UNLINK, &[path.as_ptr() as usize]);
}

fn main(args: Vec<String>) -> i32 {
    let only = args.get(1).cloned();
    let mut r = Report {
        passed: 0,
        failed: 0,
    };
    if only.as_deref() == Some("files") {
        files(&mut r, args.get(2).map_or("/tmp", |s| s.as_str()));
        println!("kapitest: {} passed, {} failed", r.passed, r.failed);
        return (r.failed != 0) as i32;
    }
    let tests: [(&str, fn(&mut Report)); 12] = [
        ("eventfd", eventfd),
        ("timerfd", timerfd),
        ("epoll", epoll),
        ("signals", signals),
        ("threads", threads),
        ("sched", scheduling),
        ("process", fork_exit),
        ("cputime", cputime),
        ("memory", memory),
        ("pty", pty),
        ("wakeups", wakeups),
        ("vt", vt),
    ];
    for (name, f) in tests {
        if only.as_deref().is_none_or(|o| o == name) {
            f(&mut r);
        }
    }
    println!("kapitest: {} passed, {} failed", r.passed, r.failed);
    (r.failed != 0) as i32
}
