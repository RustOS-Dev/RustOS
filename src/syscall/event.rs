//! Event file descriptors: epoll, eventfd, timerfd and signalfd.
//!
//! Each object has a wait queue woken when its readiness may have changed
//! (`FileLike::wait_queue`). An epoll instance hooks the queues of the
//! descriptors it watches, so activity on them wakes its own queue.

use super::SysResult;
use crate::errno::*;
use crate::process::{self, signal, uaccess};
use crate::sched::wait::{WakeHook, wait_any};
use crate::sched::{self, TimerTarget, WaitQueue};
use crate::sync::Mutex;
use crate::vfs::{self, File, FileLike, POLLIN, POLLOUT};
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

fn cur() -> KResult<Arc<process::Process>> {
    process::current().ok_or(ESRCH)
}

fn install(obj: Arc<dyn FileLike>, flags: u32, name: &str) -> SysResult {
    let file = File::from_stream(obj, vfs::O_RDWR | (flags & vfs::O_NONBLOCK), name);
    let fd = cur()?
        .files
        .lock()
        .install(file, flags & vfs::O_CLOEXEC != 0)?;
    Ok(fd as i64)
}

/// Run `f` on the object of type `T` behind descriptor `fd`.
fn with_object<T: FileLike + 'static, R>(fd: i32, f: impl FnOnce(&T) -> KResult<R>) -> KResult<R> {
    let file = cur()?.files.lock().get(fd)?;
    let vfs::FileObject::Stream(s) = &file.object else {
        return Err(EINVAL);
    };
    f(s.as_any().downcast_ref::<T>().ok_or(EINVAL)?)
}

/// Block (unless `nonblock`) until `ready()` returns a value, waking when
/// `wq` is woken.
fn block_until<R>(
    wq: &WaitQueue,
    nonblock: bool,
    mut ready: impl FnMut() -> Option<R>,
) -> KResult<R> {
    if let Some(r) = ready() {
        return Ok(r);
    }
    if nonblock {
        return Err(EAGAIN);
    }
    let mut out = None;
    wait_any::<Errno>(
        &[wq],
        None,
        || {
            out = ready();
            Ok(out.is_some() as usize)
        },
        signal::has_pending,
    )?;
    out.ok_or(EINTR)
}

// ---------------------------------------------------------------------------
// eventfd
// ---------------------------------------------------------------------------

const EFD_SEMAPHORE: u32 = 1;

struct EventFd {
    count: Mutex<u64>,
    semaphore: bool,
    wq: WaitQueue,
}

impl FileLike for EventFd {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        let v = block_until(&self.wq, nonblock, || {
            let mut c = self.count.lock();
            if *c == 0 {
                return None;
            }
            let v = if self.semaphore { 1 } else { *c };
            *c -= v;
            Some(v)
        })?;
        buf[..8].copy_from_slice(&v.to_ne_bytes());
        self.wq.wake_all();
        Ok(8)
    }
    fn write(&self, buf: &[u8], nonblock: bool) -> KResult<usize> {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        let v = u64::from_ne_bytes(buf[..8].try_into().unwrap());
        if v == u64::MAX {
            return Err(EINVAL);
        }
        block_until(&self.wq, nonblock, || {
            let mut c = self.count.lock();
            (*c <= u64::MAX - 1 - v).then(|| *c += v)
        })?;
        self.wq.wake_all();
        Ok(8)
    }
    fn poll(&self) -> u16 {
        let c = *self.count.lock();
        let mut r = 0;
        if c > 0 {
            r |= POLLIN;
        }
        if c < u64::MAX - 1 {
            r |= POLLOUT;
        }
        r
    }
    fn wait_queue(&self) -> &WaitQueue {
        &self.wq
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn eventfd2(initval: u64, flags: u32) -> SysResult {
    let obj = Arc::new(EventFd {
        count: Mutex::new(initval & 0xFFFF_FFFF),
        semaphore: flags & EFD_SEMAPHORE != 0,
        wq: WaitQueue::new(),
    });
    install(obj, flags, "anon_inode:[eventfd]")
}

// ---------------------------------------------------------------------------
// timerfd
// ---------------------------------------------------------------------------

const TFD_TIMER_ABSTIME: u32 = 1;
const CLOCK_REALTIME: u64 = 0;

struct TimerFd {
    me: Weak<TimerFd>,
    realtime: bool,
    /// Next expiry on the monotonic clock (0 = disarmed).
    deadline: AtomicU64,
    interval: AtomicU64,
    expirations: AtomicU64,
    wq: WaitQueue,
}

impl TimerTarget for TimerFd {
    fn fire(self: Arc<Self>, now: u64) {
        let d = self.deadline.load(Ordering::SeqCst);
        if d == 0 || now < d {
            return;
        }
        let iv = self.interval.load(Ordering::SeqCst);
        let mut n = 1;
        if let Some(missed) = (now - d).checked_div(iv) {
            n += missed;
            let next = d + n * iv;
            self.deadline.store(next, Ordering::SeqCst);
            sched::add_timer(next, self.clone());
        } else {
            self.deadline.store(0, Ordering::SeqCst);
        }
        self.expirations.fetch_add(n, Ordering::SeqCst);
        self.wq.wake_all();
    }
}

impl FileLike for TimerFd {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        if buf.len() < 8 {
            return Err(EINVAL);
        }
        let v = block_until(&self.wq, nonblock, || {
            match self.expirations.swap(0, Ordering::SeqCst) {
                0 => None,
                v => Some(v),
            }
        })?;
        buf[..8].copy_from_slice(&v.to_ne_bytes());
        Ok(8)
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn poll(&self) -> u16 {
        if self.expirations.load(Ordering::SeqCst) > 0 {
            POLLIN
        } else {
            0
        }
    }
    fn close(&self) {
        self.deadline.store(0, Ordering::SeqCst);
    }
    fn wait_queue(&self) -> &WaitQueue {
        &self.wq
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn timerfd_create(clock: u64, flags: u32) -> SysResult {
    if clock > 1 && clock != 7 && clock != 8 {
        return Err(EINVAL); // REALTIME, MONOTONIC, BOOTTIME(_ALARM)
    }
    let obj = Arc::new_cyclic(|me| TimerFd {
        me: me.clone(),
        realtime: clock == CLOCK_REALTIME,
        deadline: AtomicU64::new(0),
        interval: AtomicU64::new(0),
        expirations: AtomicU64::new(0),
        wq: WaitQueue::new(),
    });
    install(obj, flags, "anon_inode:[timerfd]")
}

fn read_timespec(p: u64) -> KResult<u64> {
    let t: [i64; 2] = uaccess::read_user(p)?;
    if t[0] < 0 || !(0..1_000_000_000).contains(&t[1]) {
        return Err(EINVAL);
    }
    Ok(t[0] as u64 * 1_000_000_000 + t[1] as u64)
}

fn timespec(ns: u64) -> [i64; 2] {
    [(ns / 1_000_000_000) as i64, (ns % 1_000_000_000) as i64]
}

fn itimerspec(t: &TimerFd) -> [i64; 4] {
    let d = t.deadline.load(Ordering::SeqCst);
    let rem = if d == 0 {
        0
    } else {
        d.saturating_sub(crate::time::nanos()).max(1)
    };
    let (i, v) = (timespec(t.interval.load(Ordering::SeqCst)), timespec(rem));
    [i[0], i[1], v[0], v[1]]
}

pub fn timerfd_settime(fd: i32, flags: u32, new: u64, old: u64) -> SysResult {
    let interval = read_timespec(new)?;
    let value = read_timespec(new + 16)?;
    let prev = with_object::<TimerFd, _>(fd, |t| {
        let prev = itimerspec(t);
        t.interval.store(interval, Ordering::SeqCst);
        t.expirations.store(0, Ordering::SeqCst);
        if value == 0 {
            t.deadline.store(0, Ordering::SeqCst);
            return Ok(prev);
        }
        let now = crate::time::nanos();
        let deadline = if flags & TFD_TIMER_ABSTIME != 0 {
            if t.realtime {
                now + value.saturating_sub(crate::time::realtime_nanos())
            } else {
                value
            }
        } else {
            now + value
        }
        .max(1);
        t.deadline.store(deadline, Ordering::SeqCst);
        if let Some(me) = t.me.upgrade() {
            sched::add_timer(deadline, me);
        }
        Ok(prev)
    })?;
    if old != 0 {
        uaccess::write_user(old, &prev)?;
    }
    Ok(0)
}

pub fn timerfd_gettime(fd: i32, out: u64) -> SysResult {
    let v = with_object::<TimerFd, _>(fd, |t| Ok(itimerspec(t)))?;
    uaccess::write_user(out, &v)?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// signalfd
// ---------------------------------------------------------------------------

struct SignalFd {
    mask: AtomicU64,
    process: Weak<process::Process>,
}

impl SignalFd {
    fn take(&self) -> Option<u32> {
        let p = self.process.upgrade()?;
        let mask = self.mask.load(Ordering::SeqCst);
        let pend = p.signals.pending.load(Ordering::SeqCst) & mask;
        if pend == 0 {
            return None;
        }
        let sig = pend.trailing_zeros() + 1;
        let bit = 1u64 << (sig - 1);
        (p.signals.pending.fetch_and(!bit, Ordering::SeqCst) & bit != 0).then_some(sig)
    }
}

impl FileLike for SignalFd {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        const SIZE: usize = 128;
        if buf.len() < SIZE {
            return Err(EINVAL);
        }
        let mut n = 0;
        while buf.len() - n >= SIZE {
            let sig = if n == 0 {
                block_until(&signal::SIGNAL_WQ, nonblock, || self.take())?
            } else {
                match self.take() {
                    Some(s) => s,
                    None => break,
                }
            };
            // struct signalfd_siginfo: ssi_signo, ssi_errno, ssi_code, ...
            let rec = &mut buf[n..n + SIZE];
            rec.fill(0);
            rec[0..4].copy_from_slice(&sig.to_ne_bytes());
            n += SIZE;
        }
        Ok(n)
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn poll(&self) -> u16 {
        let Some(p) = self.process.upgrade() else {
            return 0;
        };
        if p.signals.pending.load(Ordering::SeqCst) & self.mask.load(Ordering::SeqCst) != 0 {
            POLLIN
        } else {
            0
        }
    }
    fn wait_queue(&self) -> &WaitQueue {
        &signal::SIGNAL_WQ
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn signalfd4(fd: i32, mask_ptr: u64, size: u64, flags: u32) -> SysResult {
    if size != 8 {
        return Err(EINVAL);
    }
    let mut mask: u64 = uaccess::read_user(mask_ptr)?;
    mask &= !((1 << (signal::SIGKILL - 1)) | (1 << (signal::SIGSTOP - 1)));
    if fd != -1 {
        with_object::<SignalFd, _>(fd, |s| {
            s.mask.store(mask, Ordering::SeqCst);
            Ok(())
        })?;
        return Ok(fd as i64);
    }
    let obj = Arc::new(SignalFd {
        mask: AtomicU64::new(mask),
        process: Arc::downgrade(&cur()?),
    });
    install(obj, flags, "anon_inode:[signalfd]")
}

// ---------------------------------------------------------------------------
// epoll
// ---------------------------------------------------------------------------

const EPOLL_CTL_ADD: u32 = 1;
const EPOLL_CTL_DEL: u32 = 2;
const EPOLL_CTL_MOD: u32 = 3;
const EPOLLET: u32 = 1 << 31;
const EPOLLONESHOT: u32 = 1 << 30;
const EPOLLERR: u32 = 0x008;
const EPOLLHUP: u32 = 0x010;

struct Interest {
    file: Weak<File>,
    events: u32,
    data: u64,
    /// Readiness last reported (edge-triggered entries).
    last: u32,
    /// `hook.activity` at the last report (edge-triggered entries).
    seen: u64,
    /// One-shot entry already reported: disabled until EPOLL_CTL_MOD.
    disabled: bool,
    hook: Arc<InterestHook>,
}

/// Hooked on a watched file's wait queue: counts activity and wakes the
/// epoll instance (and so anything polling the epoll descriptor). Hooks of
/// a closed epoll instance drop out on their queue's next wake-up.
struct InterestHook {
    ep: Weak<Epoll>,
    activity: AtomicU64,
}

impl WakeHook for InterestHook {
    fn woken(&self) -> bool {
        let Some(ep) = self.ep.upgrade() else {
            return false;
        };
        self.activity.fetch_add(1, Ordering::SeqCst);
        ep.wq.wake_all();
        true
    }
}

impl Interest {
    fn unhook(&self) {
        if let Some(f) = self.file.upgrade()
            && let Some(q) = f.wait_queue()
        {
            let h: Arc<dyn WakeHook> = self.hook.clone();
            q.remove_hook(&h);
        }
    }
}

struct Epoll {
    /// Keyed by (fd, file address) like Linux's (fd, file) pair.
    items: Mutex<BTreeMap<(i32, usize), Interest>>,
    wq: WaitQueue,
}

impl Epoll {
    /// Collect up to `max` ready events.
    fn collect(&self, max: usize, consume: bool) -> Vec<(u32, u64)> {
        let mut out = Vec::new();
        let mut items = self.items.lock();
        items.retain(|_, it| it.file.strong_count() > 0);
        for it in items.values_mut() {
            if out.len() >= max {
                break;
            }
            if it.disabled {
                continue;
            }
            let Some(f) = it.file.upgrade() else { continue };
            let ready = f.poll() as u32 & (it.events | EPOLLERR | EPOLLHUP);
            if ready == 0 {
                it.last = 0;
                continue;
            }
            if it.events & EPOLLET != 0 {
                // Report rising edges, and again after any new activity.
                let activity = it.hook.activity.load(Ordering::SeqCst);
                let fresh = ready & !it.last != 0 || activity != it.seen;
                if !fresh {
                    continue;
                }
                if consume {
                    it.last = ready;
                    it.seen = activity;
                }
            }
            if consume && it.events & EPOLLONESHOT != 0 {
                it.disabled = true;
            }
            out.push((ready, it.data));
        }
        out
    }
}

impl FileLike for Epoll {
    fn read(&self, _buf: &mut [u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn poll(&self) -> u16 {
        if self.collect(1, false).is_empty() {
            0
        } else {
            POLLIN
        }
    }
    fn wait_queue(&self) -> &WaitQueue {
        &self.wq
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Every epoll instance with its creator, for state dumps.
static EPOLLS: Mutex<Vec<(u32, Weak<Epoll>)>> = Mutex::new(Vec::new());

pub fn epoll_create1(flags: u32) -> SysResult {
    if flags & !vfs::O_CLOEXEC != 0 {
        return Err(EINVAL);
    }
    let ep = Arc::new(Epoll {
        items: Mutex::new(BTreeMap::new()),
        wq: WaitQueue::new(),
    });
    {
        let mut all = EPOLLS.lock();
        all.retain(|(_, w)| w.strong_count() > 0);
        all.push((cur()?.pid, Arc::downgrade(&ep)));
    }
    install(ep, flags & vfs::O_CLOEXEC, "anon_inode:[eventpoll]")
}

/// State dump: each epoll instance's interests, with their current
/// readiness and whether the interest's wake hook is still registered.
/// A ready interest of an epoll its owner sleeps on is a lost wake-up.
pub fn dump_epolls() {
    let Some(all) = EPOLLS.try_lock() else {
        crate::serial_println!("[sysrq] epoll list locked");
        return;
    };
    for (pid, w) in all.iter() {
        let Some(ep) = w.upgrade() else { continue };
        let Some(items) = ep.items.try_lock() else {
            crate::serial_println!("[sysrq] epoll of pid {} locked", pid);
            continue;
        };
        let mut line = alloc::format!("[sysrq] epoll pid {} waiters-hooks:", pid);
        for ((fd, _), it) in items.iter() {
            let ready = it
                .file
                .upgrade()
                .map(|f| f.poll() as u32 & (it.events | EPOLLERR | EPOLLHUP))
                .unwrap_or(0);
            line.push_str(&alloc::format!(
                " {}:ev{:#x}/r{:#x}/h{}{}",
                fd,
                it.events & 0xffff,
                ready,
                Arc::strong_count(&it.hook) - 1,
                if it.disabled { "/off" } else { "" }
            ));
        }
        crate::serial_println!("{}", line);
    }
}

pub fn epoll_ctl(epfd: i32, op: u32, fd: i32, event: u64) -> SysResult {
    let target = cur()?.files.lock().get(fd)?;
    if epfd == fd {
        return Err(EINVAL);
    }
    let (events, data) = if op == EPOLL_CTL_DEL {
        (0, 0)
    } else {
        let raw: [u8; 12] = uaccess::read_user(event)?;
        (
            u32::from_ne_bytes(raw[0..4].try_into().unwrap()),
            u64::from_ne_bytes(raw[4..12].try_into().unwrap()),
        )
    };
    let key = (fd, Arc::as_ptr(&target) as usize);
    let epfile = cur()?.files.lock().get(epfd)?;
    let vfs::FileObject::Stream(eps) = &epfile.object else {
        return Err(EINVAL);
    };
    if eps.as_any().downcast_ref::<Epoll>().is_none() {
        return Err(EINVAL);
    }
    // An `Arc<Epoll>` for the hooks' back references.
    let ep_arc: Arc<Epoll> = {
        let raw = Arc::into_raw(eps.clone()) as *const Epoll;
        // SAFETY: the object was just checked to be an `Epoll`.
        unsafe { Arc::from_raw(raw) }
    };
    if let vfs::FileObject::Stream(t) = &target.object
        && let Some(inner) = t.as_any().downcast_ref::<Epoll>()
        && core::ptr::eq(inner, &*ep_arc)
    {
        return Err(ELOOP);
    }
    {
        let ep = &*ep_arc;
        let mut items = ep.items.lock();
        match op {
            EPOLL_CTL_ADD => {
                if items.contains_key(&key) {
                    return Err(EEXIST);
                }
                let hook = Arc::new(InterestHook {
                    ep: Arc::downgrade(&ep_arc),
                    activity: AtomicU64::new(0),
                });
                if let Some(q) = target.wait_queue() {
                    q.add_hook(hook.clone());
                }
                items.insert(
                    key,
                    Interest {
                        file: Arc::downgrade(&target),
                        events,
                        data,
                        last: 0,
                        seen: 0,
                        disabled: false,
                        hook,
                    },
                );
            }
            EPOLL_CTL_MOD => {
                let it = items.get_mut(&key).ok_or(ENOENT)?;
                it.events = events;
                it.data = data;
                it.last = 0;
                it.disabled = false;
            }
            EPOLL_CTL_DEL => {
                items.remove(&key).ok_or(ENOENT)?.unhook();
            }
            _ => return Err(EINVAL),
        }
    }
    ep_arc.wq.wake_all();
    Ok(0)
}

pub fn epoll_wait(epfd: i32, events: u64, max: i32, timeout_ms: i64) -> SysResult {
    if max <= 0 || max > 65536 {
        return Err(EINVAL);
    }
    let timeout = (timeout_ms >= 0).then(|| timeout_ms as u64 * 1_000_000);
    epoll_wait_ns(epfd, events, max, timeout)
}

pub fn epoll_pwait2(epfd: i32, events: u64, max: i32, ts: u64) -> SysResult {
    let timeout = if ts == 0 {
        None
    } else {
        Some(read_timespec(ts)?)
    };
    epoll_wait_ns(epfd, events, max, timeout)
}

fn epoll_wait_ns(epfd: i32, events: u64, max: i32, timeout: Option<u64>) -> SysResult {
    if max <= 0 {
        return Err(EINVAL);
    }
    let file = cur()?.files.lock().get(epfd)?;
    let vfs::FileObject::Stream(s) = &file.object else {
        return Err(EINVAL);
    };
    let ep = s.as_any().downcast_ref::<Epoll>().ok_or(EINVAL)?;
    let mut got = Vec::new();
    super::fs::wait_ready(timeout, core::slice::from_ref(&file), || {
        got = ep.collect(max as usize, true);
        Ok(got.len())
    })?;
    for (i, (ev, data)) in got.iter().enumerate() {
        let mut raw = [0u8; 12];
        raw[0..4].copy_from_slice(&ev.to_ne_bytes());
        raw[4..12].copy_from_slice(&data.to_ne_bytes());
        uaccess::write_user(events + 12 * i as u64, &raw)?;
    }
    Ok(got.len() as i64)
}
