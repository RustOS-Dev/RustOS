//! memfd_create(2): anonymous files with file seals (F_ADD_SEALS /
//! F_GET_SEALS), which Wayland compositors and clients use to share
//! buffers.
//!
//! A memfd's contents are page frames (an `mm::pagecache::ShmObject`), not
//! kernel heap: window buffers are megabytes each. read/write copy to and
//! from the frames, and shared mappings map the same frames
//! (`Backing::Shm`), so both always agree. Seals are enforced on writes,
//! size changes and writable shared mappings (`check_map`); F_SEAL_WRITE is
//! refused while a writable shared mapping exists (`syscall::fdobj::seals`
//! looks for one), as in Linux.

use super::*;
use crate::errno::*;
use crate::mm::FRAME_SIZE;
use crate::mm::pagecache::ShmObject;

pub const F_SEAL_SEAL: u32 = 0x1;
pub const F_SEAL_SHRINK: u32 = 0x2;
pub const F_SEAL_GROW: u32 = 0x4;
pub const F_SEAL_WRITE: u32 = 0x8;
pub const F_SEAL_FUTURE_WRITE: u32 = 0x10;
const ALL_SEALS: u32 = 0x1f;

pub const MFD_CLOEXEC: u32 = 1;
pub const MFD_ALLOW_SEALING: u32 = 2;

/// Identifies memfds as a filesystem (for cross-device checks).
const FS_ID: usize = 0x3E3F_D000;

static NEXT_INO: AtomicU64 = AtomicU64::new(1);

pub struct Memfd {
    pages: Arc<ShmObject>,
    size: Mutex<u64>,
    meta: Mutex<Metadata>,
    seals: AtomicU32,
}

impl Memfd {
    /// A new empty memfd. Without MFD_ALLOW_SEALING it starts sealed
    /// against further seals, as in Linux.
    pub fn new(flags: u32) -> KResult<Arc<Memfd>> {
        let now = crate::time::unix_time();
        let mut meta = Metadata::new(FileType::Regular, 0o600);
        meta.ino = NEXT_INO.fetch_add(1, Ordering::Relaxed);
        meta.nlink = 0;
        meta.atime = now;
        meta.mtime = now;
        meta.ctime = now;
        Ok(Arc::new(Memfd {
            pages: ShmObject::new(),
            size: Mutex::new(0),
            meta: Mutex::new(meta),
            seals: AtomicU32::new(if flags & MFD_ALLOW_SEALING != 0 {
                0
            } else {
                F_SEAL_SEAL
            }),
        }))
    }

    /// The frames shared mappings map.
    pub fn pages(&self) -> &Arc<ShmObject> {
        &self.pages
    }

    pub fn seals(&self) -> u32 {
        self.seals.load(Ordering::SeqCst)
    }

    /// Add `seals`; `mapped_writable` says whether a writable shared
    /// mapping of the file exists (F_SEAL_WRITE is refused then).
    pub fn add_seals(&self, seals: u32, mapped_writable: bool) -> KResult<()> {
        if seals & !ALL_SEALS != 0 {
            return Err(EINVAL);
        }
        if self.seals() & F_SEAL_SEAL != 0 {
            return Err(EPERM);
        }
        if seals & F_SEAL_WRITE != 0 && mapped_writable {
            return Err(EBUSY);
        }
        self.seals.fetch_or(seals, Ordering::SeqCst);
        Ok(())
    }

    /// A shared mapping with `write` access is about to be made.
    pub fn check_map(&self, write: bool) -> KResult<()> {
        if write && self.seals() & (F_SEAL_WRITE | F_SEAL_FUTURE_WRITE) != 0 {
            return Err(EPERM);
        }
        Ok(())
    }

    fn touch(&self) {
        let now = crate::time::unix_time();
        let mut m = self.meta.lock();
        m.mtime = now;
        m.ctime = now;
    }

    /// Copy `len` bytes between `buf` and the file at `off` (into the
    /// file if `write`).
    fn copy(&self, off: u64, buf: *mut u8, len: usize, write: bool) -> KResult<()> {
        let mut done = 0usize;
        while done < len {
            let pos = off + done as u64;
            let (idx, in_page) = (pos / FRAME_SIZE, (pos % FRAME_SIZE) as usize);
            let n = (len - done).min(FRAME_SIZE as usize - in_page);
            let frame = if write {
                Some(self.pages.page(idx)?)
            } else {
                self.pages.existing_page(idx)
            };
            match frame {
                Some(f) => {
                    let p = unsafe { crate::mm::phys_ptr::<u8>(f).add(in_page) };
                    unsafe {
                        if write {
                            core::ptr::copy_nonoverlapping(buf.add(done), p, n);
                        } else {
                            core::ptr::copy_nonoverlapping(p, buf.add(done), n);
                        }
                    }
                }
                // Never written: reads as zeros.
                None => unsafe { core::ptr::write_bytes(buf.add(done), 0, n) },
            }
            done += n;
        }
        Ok(())
    }

    fn resize(&self, new: u64) {
        let mut size = self.size.lock();
        if new < *size {
            let keep = new.div_ceil(FRAME_SIZE);
            self.pages.truncate_pages(keep);
            if !new.is_multiple_of(FRAME_SIZE) {
                self.pages
                    .zero_tail(new / FRAME_SIZE, (new % FRAME_SIZE) as usize);
            }
        }
        *size = new;
    }
}

impl Inode for Memfd {
    fn metadata(&self) -> KResult<Metadata> {
        let mut m = *self.meta.lock();
        m.size = *self.size.lock();
        m.blocks = m.size.div_ceil(512);
        m.blksize = FRAME_SIZE as u32;
        Ok(m)
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        let size = *self.size.lock();
        if off >= size {
            return Ok(0);
        }
        let n = buf.len().min((size - off) as usize);
        self.copy(off, buf.as_mut_ptr(), n, false)?;
        Ok(n)
    }
    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        let seals = self.seals();
        if seals & (F_SEAL_WRITE | F_SEAL_FUTURE_WRITE) != 0 {
            return Err(EPERM);
        }
        let end = off.checked_add(buf.len() as u64).ok_or(EFBIG)?;
        if seals & F_SEAL_GROW != 0 && end > *self.size.lock() {
            return Err(EPERM);
        }
        // `copy` only reads from `buf` when writing into the file.
        self.copy(off, buf.as_ptr() as *mut u8, buf.len(), true)?;
        let mut size = self.size.lock();
        if end > *size {
            *size = end;
        }
        drop(size);
        self.touch();
        Ok(buf.len())
    }
    fn truncate(&self, new: u64) -> KResult<()> {
        let seals = self.seals();
        let cur = *self.size.lock();
        if (new < cur && seals & F_SEAL_SHRINK != 0) || (new > cur && seals & F_SEAL_GROW != 0) {
            return Err(EPERM);
        }
        self.resize(new);
        self.touch();
        Ok(())
    }
    fn fallocate(&self, mode: u32, off: u64, len: u64) -> KResult<()> {
        const KEEP_SIZE: u32 = 1;
        let end = off.checked_add(len).ok_or(EFBIG)?;
        if mode & !KEEP_SIZE != 0 {
            return Err(EOPNOTSUPP);
        }
        if mode & KEEP_SIZE == 0 && end > *self.size.lock() {
            if self.seals() & F_SEAL_GROW != 0 {
                return Err(EPERM);
            }
            self.resize(end);
        }
        Ok(())
    }
    fn chmod(&self, mode: u32) -> KResult<()> {
        self.meta.lock().mode = mode & 0o7777;
        Ok(())
    }
    fn set_times(&self, atime: Option<u64>, mtime: Option<u64>) -> KResult<()> {
        let mut m = self.meta.lock();
        if let Some(a) = atime {
            m.atime = a;
        }
        if let Some(t) = mtime {
            m.mtime = t;
        }
        Ok(())
    }
    fn fs_id(&self) -> usize {
        FS_ID
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
