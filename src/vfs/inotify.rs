//! inotify(7): file-system events for watched files and directories.
//!
//! Watches are keyed by inode (filesystem id, inode number). The syscall
//! layer reports what happened by path (`created`, `removed`, `modified`,
//! ...); each report finds the watches on the file itself and on its
//! directory (with the file's name). With no watches anywhere, reports
//! return at once.

use super::*;
use crate::errno::*;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Weak;

pub const IN_ACCESS: u32 = 0x1;
pub const IN_MODIFY: u32 = 0x2;
pub const IN_ATTRIB: u32 = 0x4;
pub const IN_CLOSE_WRITE: u32 = 0x8;
pub const IN_CLOSE_NOWRITE: u32 = 0x10;
pub const IN_OPEN: u32 = 0x20;
pub const IN_MOVED_FROM: u32 = 0x40;
pub const IN_MOVED_TO: u32 = 0x80;
pub const IN_CREATE: u32 = 0x100;
pub const IN_DELETE: u32 = 0x200;
pub const IN_DELETE_SELF: u32 = 0x400;
pub const IN_MOVE_SELF: u32 = 0x800;
const IN_ALL_EVENTS: u32 = 0xfff;
pub const IN_Q_OVERFLOW: u32 = 0x4000;
pub const IN_IGNORED: u32 = 0x8000;
const IN_ONLYDIR: u32 = 0x0100_0000;
const IN_DONT_FOLLOW: u32 = 0x0200_0000;
const IN_EXCL_UNLINK: u32 = 0x0400_0000;
const IN_MASK_CREATE: u32 = 0x1000_0000;
const IN_MASK_ADD: u32 = 0x2000_0000;
pub const IN_ISDIR: u32 = 0x4000_0000;
const IN_ONESHOT: u32 = 0x8000_0000;

const QUEUE_MAX: usize = 16384;

type Key = (usize, u64);

fn key(i: &Arc<dyn Inode>) -> KResult<Key> {
    Ok((i.fs_id(), i.metadata()?.ino))
}

/// One inotify instance (an inotify file descriptor).
pub struct Inotify {
    me: Weak<Inotify>,
    state: Mutex<State>,
    wq: WaitQueue,
}

struct State {
    /// Watch descriptor → (inode, mask).
    watches: BTreeMap<i32, (Key, u32)>,
    next_wd: i32,
    queue: VecDeque<Vec<u8>>,
}

/// Every watch: inode → the instances watching it.
/// The instances (and their watch descriptors) watching one inode.
type Watchers = Vec<(Weak<Inotify>, i32)>;

static WATCHES: Mutex<BTreeMap<Key, Watchers>> = Mutex::new(BTreeMap::new());
static NWATCHES: AtomicU32 = AtomicU32::new(0);
static COOKIE: AtomicU32 = AtomicU32::new(1);

impl Inotify {
    pub fn new() -> Arc<Inotify> {
        Arc::new_cyclic(|me| Inotify {
            me: me.clone(),
            state: Mutex::new(State {
                watches: BTreeMap::new(),
                next_wd: 1,
                queue: VecDeque::new(),
            }),
            wq: WaitQueue::new(),
        })
    }

    /// inotify_add_watch(2) on `path`.
    pub fn add_watch(&self, path: &str, mask: u32) -> KResult<i32> {
        if mask & (IN_ALL_EVENTS) == 0 {
            return Err(EINVAL);
        }
        if mask & IN_MASK_ADD != 0 && mask & IN_MASK_CREATE != 0 {
            return Err(EINVAL);
        }
        let inode = if mask & IN_DONT_FOLLOW != 0 {
            lookup_nofollow(path)?
        } else {
            lookup(path)?
        };
        let meta = inode.metadata()?;
        if mask & IN_ONLYDIR != 0 && meta.kind != FileType::Directory {
            return Err(ENOTDIR);
        }
        let k = (inode.fs_id(), meta.ino);
        let events = mask & (IN_ALL_EVENTS | IN_ONESHOT | IN_EXCL_UNLINK);
        let mut st = self.state.lock();
        if let Some((&wd, w)) = st.watches.iter_mut().find(|(_, (wk, _))| *wk == k) {
            if mask & IN_MASK_CREATE != 0 {
                return Err(EEXIST);
            }
            w.1 = if mask & IN_MASK_ADD != 0 {
                w.1 | events
            } else {
                events
            };
            return Ok(wd);
        }
        let wd = st.next_wd;
        st.next_wd += 1;
        st.watches.insert(wd, (k, events));
        drop(st);
        WATCHES
            .lock()
            .entry(k)
            .or_default()
            .push((self.me.clone(), wd));
        NWATCHES.fetch_add(1, Ordering::SeqCst);
        Ok(wd)
    }

    /// inotify_rm_watch(2).
    pub fn rm_watch(&self, wd: i32) -> KResult<()> {
        let (k, _) = self.state.lock().watches.remove(&wd).ok_or(EINVAL)?;
        unregister(k, &self.me, wd);
        self.push(wd, IN_IGNORED, 0, None);
        Ok(())
    }

    fn push(&self, wd: i32, mask: u32, cookie: u32, name: Option<&str>) {
        // struct inotify_event { int wd; u32 mask, cookie, len; char name[]; }
        // with the name NUL-padded to a multiple of the header size.
        let len = name.map_or(0, |n| (n.len() + 1).next_multiple_of(16));
        let mut ev = Vec::with_capacity(16 + len);
        ev.extend_from_slice(&wd.to_ne_bytes());
        ev.extend_from_slice(&mask.to_ne_bytes());
        ev.extend_from_slice(&cookie.to_ne_bytes());
        ev.extend_from_slice(&(len as u32).to_ne_bytes());
        if let Some(n) = name {
            ev.extend_from_slice(n.as_bytes());
            ev.resize(16 + len, 0);
        }
        {
            let mut st = self.state.lock();
            if st.queue.len() >= QUEUE_MAX {
                if st
                    .queue
                    .back()
                    .map(|e| e[4..8] != IN_Q_OVERFLOW.to_ne_bytes())
                    != Some(false)
                {
                    let mut o = Vec::with_capacity(16);
                    o.extend_from_slice(&(-1i32).to_ne_bytes());
                    o.extend_from_slice(&IN_Q_OVERFLOW.to_ne_bytes());
                    o.extend_from_slice(&[0; 8]);
                    st.queue.push_back(o);
                }
            } else if st.queue.back() != Some(&ev) {
                // Identical consecutive events coalesce, as in Linux.
                st.queue.push_back(ev);
            }
        }
        self.wq.wake_all();
        notify_poll();
    }

    /// Deliver `mask` to watch `wd` if it asked for it.
    fn deliver(&self, wd: i32, mask: u32, cookie: u32, name: Option<&str>) {
        let wanted = {
            let st = self.state.lock();
            match st.watches.get(&wd) {
                Some((_, m)) => *m,
                None => return,
            }
        };
        if wanted & mask & IN_ALL_EVENTS == 0 {
            return;
        }
        self.push(wd, mask, cookie, name);
        if wanted & IN_ONESHOT != 0
            && let Some((k, _)) = self.state.lock().watches.remove(&wd)
        {
            unregister(k, &self.me, wd);
            self.push(wd, IN_IGNORED, 0, None);
        }
    }

    fn queued_bytes(&self) -> usize {
        self.state.lock().queue.iter().map(|e| e.len()).sum()
    }
}

fn unregister(k: Key, me: &Weak<Inotify>, wd: i32) {
    let mut w = WATCHES.lock();
    if let Some(list) = w.get_mut(&k) {
        let before = list.len();
        list.retain(|(i, d)| !(*d == wd && Weak::ptr_eq(i, me)));
        NWATCHES.fetch_sub((before - list.len()) as u32, Ordering::SeqCst);
        if list.is_empty() {
            w.remove(&k);
        }
    }
}

impl FileLike for Inotify {
    fn read(&self, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        loop {
            {
                let mut st = self.state.lock();
                if let Some(first) = st.queue.front() {
                    if buf.len() < first.len() {
                        return Err(EINVAL);
                    }
                    let mut n = 0;
                    while let Some(e) = st.queue.front() {
                        if n + e.len() > buf.len() {
                            break;
                        }
                        buf[n..n + e.len()].copy_from_slice(e);
                        n += e.len();
                        st.queue.pop_front();
                    }
                    return Ok(n);
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            crate::sched::wait::wait_any::<Errno>(
                &[&self.wq],
                None,
                || Ok(!self.state.lock().queue.is_empty() as usize),
                crate::process::signal::has_pending,
            )?;
        }
    }
    fn write(&self, _buf: &[u8], _nonblock: bool) -> KResult<usize> {
        Err(EINVAL)
    }
    fn poll(&self) -> u16 {
        if self.state.lock().queue.is_empty() {
            0
        } else {
            POLLIN
        }
    }
    fn wait_queue(&self) -> &WaitQueue {
        &self.wq
    }
    fn ioctl(&self, cmd: u64, arg: u64) -> KResult<i64> {
        const FIONREAD: u64 = 0x541B;
        if cmd == FIONREAD {
            crate::process::uaccess::write_user(arg, &(self.queued_bytes() as i32))?;
            return Ok(0);
        }
        Err(ENOTTY)
    }
    fn close(&self) {
        let watches = core::mem::take(&mut self.state.lock().watches);
        for (wd, (k, _)) in watches {
            unregister(k, &self.me, wd);
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Instances watching inode `k`.
fn watchers(k: Key) -> Vec<(Arc<Inotify>, i32)> {
    WATCHES
        .lock()
        .get(&k)
        .map(|l| {
            l.iter()
                .filter_map(|(i, wd)| i.upgrade().map(|i| (i, *wd)))
                .collect()
        })
        .unwrap_or_default()
}

fn active() -> bool {
    NWATCHES.load(Ordering::Relaxed) != 0
}

fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (".", path),
    }
}

/// Report `mask` for `inode` (absolute `path`): to its own watches, and
/// to its directory's with its name.
fn report(path: &str, inode: Option<&Arc<dyn Inode>>, mask: u32, cookie: u32) {
    let is_dir = inode
        .and_then(|i| i.metadata().ok())
        .is_some_and(|m| m.kind == FileType::Directory);
    let mask = mask | if is_dir { IN_ISDIR } else { 0 };
    if let Some(i) = inode
        && let Ok(k) = key(i)
    {
        for (w, wd) in watchers(k) {
            w.deliver(wd, mask, cookie, None);
        }
    }
    let (dir, name) = split(path);
    if !name.is_empty()
        && let Ok(d) = lookup(dir)
        && let Ok(k) = key(&d)
    {
        for (w, wd) in watchers(k) {
            w.deliver(wd, mask, cookie, Some(name));
        }
    }
}

/// `path` was created (a file, directory, link or node).
pub fn created(path: &str) {
    if active() {
        let i = lookup_nofollow(path).ok();
        report(path, None, IN_CREATE | dir_bit(i.as_ref()), 0);
    }
}

fn dir_bit(i: Option<&Arc<dyn Inode>>) -> u32 {
    if i.and_then(|i| i.metadata().ok())
        .is_some_and(|m| m.kind == FileType::Directory)
    {
        IN_ISDIR
    } else {
        0
    }
}

/// `inode`, which was at `path`, is about to be removed (looked up before
/// the removal, so its watches can be told).
pub fn removing(path: &str) -> Option<(String, Arc<dyn Inode>)> {
    if !active() {
        return None;
    }
    lookup_nofollow(path).ok().map(|i| (path.to_string(), i))
}

/// The removal `removing` announced happened.
pub fn removed(r: Option<(String, Arc<dyn Inode>)>) {
    let Some((path, inode)) = r else { return };
    let isdir = dir_bit(Some(&inode));
    let (dir, name) = split(&path);
    if let Ok(d) = lookup(dir)
        && let Ok(k) = key(&d)
    {
        for (w, wd) in watchers(k) {
            w.deliver(wd, IN_DELETE | isdir, 0, Some(name));
        }
    }
    // The inode itself goes away when its last link does.
    let gone = isdir != 0 || inode.metadata().map_or(true, |m| m.nlink == 0);
    if gone && let Ok(k) = key(&inode) {
        for (w, wd) in watchers(k) {
            w.deliver(wd, IN_DELETE_SELF, 0, None);
            if let Some((k, _)) = w.state.lock().watches.remove(&wd) {
                unregister(k, &w.me, wd);
            }
            w.push(wd, IN_IGNORED, 0, None);
        }
    }
}

/// `from` was renamed to `to`.
pub fn renamed(from: &str, to: &str) {
    if !active() {
        return;
    }
    let i = lookup_nofollow(to).ok();
    let cookie = COOKIE.fetch_add(1, Ordering::Relaxed);
    let isdir = dir_bit(i.as_ref());
    for (path, mask) in [(from, IN_MOVED_FROM), (to, IN_MOVED_TO)] {
        let (dir, name) = split(path);
        if let Ok(d) = lookup(dir)
            && let Ok(k) = key(&d)
        {
            for (w, wd) in watchers(k) {
                w.deliver(wd, mask | isdir, cookie, Some(name));
            }
        }
    }
    if let Some(i) = &i
        && let Ok(k) = key(i)
    {
        for (w, wd) in watchers(k) {
            w.deliver(wd, IN_MOVE_SELF, 0, None);
        }
    }
}

/// An event on the open file `path` (IN_MODIFY, IN_OPEN, IN_CLOSE_*,
/// IN_ACCESS, IN_ATTRIB).
pub fn file_event(path: &str, inode: Option<&Arc<dyn Inode>>, mask: u32) {
    if active() && path.starts_with('/') {
        report(path, inode, mask, 0);
    }
}

/// An attribute of `path` changed (chmod, chown, utimes).
pub fn attrib(path: &str) {
    if active() {
        let i = lookup_nofollow(path).ok();
        report(path, i.as_ref(), IN_ATTRIB, 0);
    }
}
