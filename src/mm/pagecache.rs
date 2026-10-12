//! Page cache for file data.
//!
//! `read()` of regular files on disk filesystems and memory-mapped file
//! pages are served from here, one frame per (inode, page index). Misses
//! read a run of pages with one filesystem call (`Inode::read_direct`,
//! which bypasses the block cache), growing a readahead window on
//! sequential access. `write()` goes through to the filesystem (so errors
//! such as ENOSPC are reported at once and ext4's ordered journaling sees
//! the data) and updates the cached pages.
//!
//! Mapped pages live here too. The cache holds one reference on each frame;
//! every mapping holds another (see `mm::frame_share`), so a private
//! mapping's first write copies the page through the normal copy-on-write
//! path and a shared mapping writes the cached page itself. Shared
//! writable pages are marked dirty when mapped and written back on
//! `msync`, `munmap`, `sync` and every few seconds by the flush thread;
//! they stay dirty while mapped (stores through the mapping are not
//! tracked page by page).
//!
//! `read`/`write` on the file stay coherent with mappings: writes update
//! cached pages, and reads see data from dirty pages not yet written back.

use super::{
    FRAME_SIZE, frame_is_shared, frame_release, frame_share, phys_ptr, with_frames, zero_frame,
};
use crate::errno::*;
use crate::sync::Mutex;
use crate::vfs::Inode;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

struct CachedPage {
    phys: u64,
    dirty: bool,
}

/// The cache holds a weak reference so that it does not keep inodes alive
/// (an unlinked ext2 inode is freed when its last reference goes). The
/// weak reference also keeps the inode's address, the cache key, from
/// being reused. Objects whose inode is gone are purged by the flusher.
struct Object {
    inode: Weak<dyn Inode>,
    /// Strong reference while pages are dirty, so they can be written back.
    pin: Option<Arc<dyn Inode>>,
    pages: BTreeMap<u64, CachedPage>,
    /// Page index a sequential reader would read next, and the current
    /// readahead window (pages).
    ra_next: u64,
    ra_window: u64,
    /// GEN when this inode was last written or truncated (or when the
    /// object was created).
    write_gen: u64,
}

impl Object {
    fn new(inode: &Arc<dyn Inode>) -> Object {
        Object {
            inode: Arc::downgrade(inode),
            pin: None,
            pages: BTreeMap::new(),
            ra_next: 0,
            ra_window: 0,
            write_gen: GEN.load(Ordering::SeqCst),
        }
    }
}

static CACHE: Mutex<BTreeMap<usize, Object>> = Mutex::new(BTreeMap::new());

/// Bumped (under the cache lock) by every write and truncate, which stamp
/// the inode's object with the new value. A miss that raced with a write to
/// the same inode may have read old data, so it is not cached. (Writes to
/// other files do not matter: a global check made page faults on a busy
/// system lose every race and fail.)
static GEN: AtomicU64 = AtomicU64::new(0);

/// Largest readahead window, in pages.
const MAX_READAHEAD: u64 = 32;

/// Cached pages above which clean, unmapped pages are evicted.
const SOFT_LIMIT: usize = 8192;

fn key(i: &Arc<dyn Inode>) -> usize {
    Arc::as_ptr(i) as *const () as usize
}

fn page_bytes(phys: u64) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(phys_ptr::<u8>(phys), FRAME_SIZE as usize) }
}

/// The frame holding page `idx` of `inode`, reading it in if needed, with
/// a reference for the caller (`frame_share`, taken under the cache lock so
/// that eviction cannot free the frame first); the cache keeps its own.
pub fn get_page(inode: &Arc<dyn Inode>, idx: u64) -> KResult<u64> {
    for _ in 0..4 {
        if let Some(p) = CACHE
            .lock()
            .get(&key(inode))
            .and_then(|o| o.pages.get(&idx))
        {
            frame_share(p.phys);
            return Ok(p.phys);
        }
        if let Some(phys) = fill(inode, idx, 1, true)? {
            return Ok(phys);
        }
    }
    // Lost to concurrent writes to this file every time.
    Err(EAGAIN)
}

/// Read pages `idx..idx+count` that are not cached yet (stopping at the
/// first cached one) with a single filesystem read, and cache them. With
/// `want`, returns page `idx` with a reference for the caller (None when
/// the read raced with a write and was dropped).
fn fill(inode: &Arc<dyn Inode>, idx: u64, count: u64, want: bool) -> KResult<Option<u64>> {
    let k = key(inode);
    let (seen, count) = {
        let mut c = CACHE.lock();
        // The object exists from here on, so writes racing with the read
        // below stamp it.
        let o = c.entry(k).or_insert_with(|| Object::new(inode));
        let mut n = 1;
        while n < count && !o.pages.contains_key(&(idx + n)) {
            n += 1;
        }
        (o.write_gen, n)
    };
    // Read outside the lock (the filesystem may sleep).
    let mut buf = alloc::vec![0u8; (count * FRAME_SIZE) as usize];
    inode.read_direct(idx * FRAME_SIZE, &mut buf)?;
    let mut frames = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let Some(phys) = with_frames(|f| f.alloc()) else {
            for p in frames {
                with_frames(|f| f.free(p));
            }
            return Err(ENOMEM);
        };
        page_bytes(phys)
            .copy_from_slice(&buf[i * FRAME_SIZE as usize..(i + 1) * FRAME_SIZE as usize]);
        frames.push(phys);
    }
    let mut unused = Vec::new();
    let mut got = None;
    let total = {
        let mut c = CACHE.lock();
        let obj = c.entry(k).or_insert_with(|| Object::new(inode));
        if obj.write_gen != seen {
            unused = frames; // raced with a write: may be stale
        } else {
            for (i, phys) in frames.into_iter().enumerate() {
                match obj.pages.entry(idx + i as u64) {
                    alloc::collections::btree_map::Entry::Occupied(_) => unused.push(phys),
                    alloc::collections::btree_map::Entry::Vacant(v) => {
                        v.insert(CachedPage { phys, dirty: false });
                    }
                }
            }
            if want && let Some(p) = obj.pages.get(&idx) {
                frame_share(p.phys);
                got = Some(p.phys);
            }
        }
        c.values().map(|o| o.pages.len()).sum::<usize>()
    };
    for p in unused {
        with_frames(|f| f.free(p));
    }
    if total > SOFT_LIMIT {
        // Not the pages just read: a reader copies them next (page faults
        // that found their page evicted every time failed with EAGAIN once
        // mapped libraries kept the cache above the limit).
        evict_except(total - SOFT_LIMIT / 2, Some((k, idx..idx + count)));
    }
    Ok(got)
}

/// `read()` through the cache: up to `buf.len()` bytes at `off`, clipped
/// to the file size.
pub fn read(inode: &Arc<dyn Inode>, off: u64, buf: &mut [u8]) -> KResult<usize> {
    let size = inode.metadata()?.size;
    if off >= size || buf.is_empty() {
        return Ok(0);
    }
    let len = (buf.len() as u64).min(size - off) as usize;
    let k = key(inode);
    let first = off / FRAME_SIZE;
    let last = (off + len as u64 - 1) / FRAME_SIZE;
    let eof_page = (size - 1) / FRAME_SIZE;
    // Sequential access grows the readahead window.
    let window = {
        let mut c = CACHE.lock();
        let obj = c.entry(k).or_insert_with(|| Object::new(inode));
        obj.ra_window = if first == obj.ra_next && first != 0 {
            (obj.ra_window * 2).clamp(4, MAX_READAHEAD)
        } else if first == 0 {
            4
        } else {
            0
        };
        obj.ra_next = last + 1;
        obj.ra_window
    };
    let mut done = 0;
    let mut idx = first;
    let mut misses = 0;
    while done < len {
        let pstart = idx * FRAME_SIZE;
        let from = (off + done as u64).max(pstart);
        let to = (off + len as u64).min(pstart + FRAME_SIZE);
        let hit = {
            let c = CACHE.lock();
            c.get(&k).and_then(|o| o.pages.get(&idx)).map(|p| {
                buf[(from - off) as usize..(to - off) as usize].copy_from_slice(
                    &page_bytes(p.phys)[(from - pstart) as usize..(to - pstart) as usize],
                );
            })
        };
        if hit.is_some() {
            done = (to - off) as usize;
            idx += 1;
            continue;
        }
        misses += 1;
        if misses > 4 {
            // Losing races with writers: read this part directly.
            let n = inode.read_at(from, &mut buf[(from - off) as usize..(to - off) as usize])?;
            done = (from - off) as usize + n;
            idx += 1;
            continue;
        }
        let want = (last - idx + 1 + window)
            .min(eof_page - idx + 1)
            .min(MAX_READAHEAD * 2);
        fill(inode, idx, want.max(1), false)?;
    }
    Ok(len)
}

/// Record that page `idx` may have been modified through a mapping.
pub fn mark_dirty(inode: &Arc<dyn Inode>, idx: u64) {
    if let Some(o) = CACHE.lock().get_mut(&key(inode))
        && let Some(p) = o.pages.get_mut(&idx)
    {
        p.dirty = true;
        o.pin.get_or_insert_with(|| inode.clone());
    }
}

/// Write back the dirty pages of one file (up to its current size).
pub fn sync_inode(inode: &Arc<dyn Inode>) -> KResult<()> {
    let dirty: Vec<(u64, u64)> = {
        let mut c = CACHE.lock();
        let Some(o) = c.get_mut(&key(inode)) else {
            return Ok(());
        };
        // A page still mapped (writable, shared) may be written again
        // without a fault, so it stays dirty until it is unmapped.
        let dirty: Vec<(u64, u64)> = o
            .pages
            .iter_mut()
            .filter(|(_, p)| p.dirty)
            .map(|(i, p)| {
                p.dirty = frame_is_shared(p.phys);
                (*i, p.phys)
            })
            .collect();
        if !o.pages.values().any(|p| p.dirty) {
            o.pin = None;
        }
        dirty
    };
    if dirty.is_empty() {
        return Ok(());
    }
    let size = inode.metadata()?.size;
    for (idx, phys) in dirty {
        let off = idx * FRAME_SIZE;
        if off >= size {
            continue; // beyond EOF: not part of the file
        }
        let n = (size - off).min(FRAME_SIZE) as usize;
        inode.write_at(off, &page_bytes(phys)[..n])?;
    }
    Ok(())
}

/// Write back every dirty page.
pub fn sync_all() {
    let inodes: Vec<Arc<dyn Inode>> = CACHE
        .lock()
        .values()
        .filter(|o| o.pages.values().any(|p| p.dirty))
        .filter_map(|o| o.pin.clone())
        .collect();
    for i in inodes {
        let _ = sync_inode(&i);
    }
    purge_dead();
}

/// Drop the pages of files that no longer exist.
fn purge_dead() {
    let mut freed = Vec::new();
    CACHE.lock().retain(|_, o| {
        if o.inode.strong_count() > 0 {
            return true;
        }
        freed.extend(o.pages.values().map(|p| p.phys));
        false
    });
    for p in freed {
        frame_release(p);
    }
}

/// Apply a `write()` of `data` at `off` to the cached pages of `inode`.
pub fn write_through(inode: &Arc<dyn Inode>, off: u64, data: &[u8]) {
    let mut c = CACHE.lock();
    let g = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(o) = c.get_mut(&key(inode)) else {
        return;
    };
    o.write_gen = g;
    let (first, last) = (
        off / FRAME_SIZE,
        (off + data.len() as u64).div_ceil(FRAME_SIZE),
    );
    for (&idx, p) in o.pages.range(first..last) {
        let pstart = idx * FRAME_SIZE;
        let from = off.max(pstart);
        let to = (off + data.len() as u64).min(pstart + FRAME_SIZE);
        page_bytes(p.phys)[(from - pstart) as usize..(to - pstart) as usize]
            .copy_from_slice(&data[(from - off) as usize..(to - off) as usize]);
    }
}

/// Overlay dirty cached data (stores through shared mappings) on a
/// `read()` of `n` bytes at `off` that bypassed the cache.
pub fn read_overlay(inode: &Arc<dyn Inode>, off: u64, buf: &mut [u8], n: usize) {
    let c = CACHE.lock();
    let Some(o) = c.get(&key(inode)) else { return };
    let end = off + n as u64;
    for (&idx, p) in o.pages.range(off / FRAME_SIZE..end.div_ceil(FRAME_SIZE)) {
        if !p.dirty {
            continue;
        }
        let pstart = idx * FRAME_SIZE;
        let from = off.max(pstart);
        let to = end.min(pstart + FRAME_SIZE);
        buf[(from - off) as usize..(to - off) as usize]
            .copy_from_slice(&page_bytes(p.phys)[(from - pstart) as usize..(to - pstart) as usize]);
    }
}

/// The file was truncated to `size`: drop whole pages past it and zero the
/// tail of the last one.
pub fn truncate(inode: &Arc<dyn Inode>, size: u64) {
    let mut c = CACHE.lock();
    let g = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(o) = c.get_mut(&key(inode)) else {
        return;
    };
    o.write_gen = g;
    let keep = size.div_ceil(FRAME_SIZE);
    let gone: Vec<u64> = o.pages.range(keep..).map(|(i, _)| *i).collect();
    for i in gone {
        let p = o.pages.remove(&i).unwrap();
        frame_release(p.phys);
    }
    if !size.is_multiple_of(FRAME_SIZE)
        && let Some(p) = o.pages.get(&(size / FRAME_SIZE))
    {
        page_bytes(p.phys)[(size % FRAME_SIZE) as usize..].fill(0);
    }
}

/// Drop up to `n` clean pages that no address space maps.
pub fn evict(n: usize) -> usize {
    evict_except(n, None)
}

/// `evict`, sparing the pages `keep` names (an object key and page range).
fn evict_except(n: usize, keep: Option<(usize, core::ops::Range<u64>)>) -> usize {
    let mut freed = Vec::new();
    {
        let mut c = CACHE.lock();
        for (&k, o) in c.iter_mut() {
            let spare = match &keep {
                Some((kk, r)) if *kk == k => r.clone(),
                _ => 0..0,
            };
            let victims: Vec<u64> = o
                .pages
                .iter()
                .filter(|(i, p)| !p.dirty && !frame_is_shared(p.phys) && !spare.contains(i))
                .map(|(i, _)| *i)
                .take(n - freed.len())
                .collect();
            for i in victims {
                freed.push(o.pages.remove(&i).unwrap().phys);
            }
            if freed.len() >= n {
                break;
            }
        }
        c.retain(|_, o| !o.pages.is_empty());
    }
    let count = freed.len();
    for p in freed {
        frame_release(p);
    }
    count
}

/// (cached pages, dirty pages) for /proc/meminfo.
pub fn stats() -> (usize, usize) {
    let c = CACHE.lock();
    c.values().fold((0, 0), |(a, d), o| {
        (
            a + o.pages.len(),
            d + o.pages.values().filter(|p| p.dirty).count(),
        )
    })
}

/// Anonymous shared memory (`MAP_SHARED | MAP_ANONYMOUS`): pages shared by
/// every address space that maps the object, including across fork.
pub struct ShmObject {
    pages: Mutex<BTreeMap<u64, u64>>,
}

impl ShmObject {
    pub fn new() -> Arc<ShmObject> {
        Arc::new(ShmObject {
            pages: Mutex::new(BTreeMap::new()),
        })
    }

    /// Frame of page `idx` (zeroed on first use). The object keeps a
    /// reference; mappings add theirs.
    pub fn page(&self, idx: u64) -> KResult<u64> {
        let mut pages = self.pages.lock();
        if let Some(&p) = pages.get(&idx) {
            return Ok(p);
        }
        let p = with_frames(|f| f.alloc()).ok_or(ENOMEM)?;
        zero_frame(p);
        pages.insert(idx, p);
        Ok(p)
    }

    /// Frame of page `idx` if it was ever touched.
    pub fn existing_page(&self, idx: u64) -> Option<u64> {
        self.pages.lock().get(&idx).copied()
    }

    /// Drop the object's pages from `idx` on (mappings keep their own
    /// references to frames they map).
    pub fn truncate_pages(&self, idx: u64) {
        let gone = self.pages.lock().split_off(&idx);
        for (_, p) in gone {
            frame_release(p);
        }
    }

    /// Zero bytes `from..` of page `idx` (the tail of a shrunk file).
    pub fn zero_tail(&self, idx: u64, from: usize) {
        if let Some(p) = self.existing_page(idx) {
            let ptr = crate::mm::phys_ptr::<u8>(p);
            unsafe { core::ptr::write_bytes(ptr.add(from), 0, FRAME_SIZE as usize - from) };
        }
    }
}

impl Drop for ShmObject {
    fn drop(&mut self) {
        for (_, p) in core::mem::take(&mut *self.pages.lock()) {
            frame_release(p);
        }
    }
}
