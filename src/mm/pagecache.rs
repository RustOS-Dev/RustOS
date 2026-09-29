//! Page cache for memory-mapped files.
//!
//! File pages mapped into address spaces live here, one frame per
//! (inode, page index). The cache holds one reference on each frame;
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

use super::{FRAME_SIZE, frame_is_shared, frame_release, phys_ptr, with_frames, zero_frame};
use crate::errno::*;
use crate::sync::Mutex;
use crate::vfs::Inode;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

struct CachedPage {
    phys: u64,
    dirty: bool,
}

struct Object {
    inode: Arc<dyn Inode>,
    pages: BTreeMap<u64, CachedPage>,
}

static CACHE: Mutex<BTreeMap<usize, Object>> = Mutex::new(BTreeMap::new());

/// Cached pages above which clean, unmapped pages are evicted.
const SOFT_LIMIT: usize = 8192;

fn key(i: &Arc<dyn Inode>) -> usize {
    Arc::as_ptr(i) as *const () as usize
}

fn page_bytes(phys: u64) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(phys_ptr::<u8>(phys), FRAME_SIZE as usize) }
}

/// The frame holding page `idx` of `inode`, reading it in if needed. The
/// cache keeps its own reference; callers that map it must add theirs.
pub fn get_page(inode: &Arc<dyn Inode>, idx: u64) -> KResult<u64> {
    let k = key(inode);
    if let Some(p) = CACHE.lock().get(&k).and_then(|o| o.pages.get(&idx)) {
        return Ok(p.phys);
    }
    // Read outside the lock (the filesystem may sleep).
    let phys = with_frames(|f| f.alloc()).ok_or(ENOMEM)?;
    zero_frame(phys);
    if let Err(e) = inode.read_at(idx * FRAME_SIZE, page_bytes(phys)) {
        with_frames(|f| f.free(phys));
        return Err(e);
    }
    let mut c = CACHE.lock();
    let obj = c.entry(k).or_insert_with(|| Object {
        inode: inode.clone(),
        pages: BTreeMap::new(),
    });
    if let Some(p) = obj.pages.get(&idx) {
        let existing = p.phys;
        drop(c);
        with_frames(|f| f.free(phys));
        return Ok(existing);
    }
    obj.pages.insert(idx, CachedPage { phys, dirty: false });
    let total: usize = c.values().map(|o| o.pages.len()).sum();
    drop(c);
    if total > SOFT_LIMIT {
        evict(total - SOFT_LIMIT / 2);
    }
    Ok(phys)
}

/// Record that page `idx` may have been modified through a mapping.
pub fn mark_dirty(inode: &Arc<dyn Inode>, idx: u64) {
    if let Some(p) = CACHE
        .lock()
        .get_mut(&key(inode))
        .and_then(|o| o.pages.get_mut(&idx))
    {
        p.dirty = true;
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
        o.pages
            .iter_mut()
            .filter(|(_, p)| p.dirty)
            .map(|(i, p)| {
                p.dirty = frame_is_shared(p.phys);
                (*i, p.phys)
            })
            .collect()
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
        .map(|o| o.inode.clone())
        .collect();
    for i in inodes {
        let _ = sync_inode(&i);
    }
}

/// Apply a `write()` of `data` at `off` to the cached pages of `inode`.
pub fn write_through(inode: &Arc<dyn Inode>, off: u64, data: &[u8]) {
    let c = CACHE.lock();
    let Some(o) = c.get(&key(inode)) else { return };
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

/// Overlay dirty cached data on a `read()` result of `n` bytes at `off`.
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
    let Some(o) = c.get_mut(&key(inode)) else {
        return;
    };
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
    let mut freed = Vec::new();
    {
        let mut c = CACHE.lock();
        for o in c.values_mut() {
            let victims: Vec<u64> = o
                .pages
                .iter()
                .filter(|(_, p)| !p.dirty && !frame_is_shared(p.phys))
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
}

impl Drop for ShmObject {
    fn drop(&mut self) {
        for (_, p) in core::mem::take(&mut *self.pages.lock()) {
            frame_release(p);
        }
    }
}
