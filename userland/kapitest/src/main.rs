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

fn main(args: Vec<String>) -> i32 {
    let only = args.get(1).cloned();
    let mut r = Report {
        passed: 0,
        failed: 0,
    };
    let tests: [(&str, fn(&mut Report)); 9] = [
        ("eventfd", eventfd),
        ("timerfd", timerfd),
        ("epoll", epoll),
        ("signals", signals),
        ("threads", threads),
        ("sched", scheduling),
        ("process", fork_exit),
        ("memory", memory),
        ("pty", pty),
    ];
    for (name, f) in tests {
        if only.as_deref().is_none_or(|o| o == name) {
            f(&mut r);
        }
    }
    println!("kapitest: {} passed, {} failed", r.passed, r.failed);
    (r.failed != 0) as i32
}
