//! memfd_create(2): anonymous files on a private tmpfs, with file seals
//! (F_ADD_SEALS / F_GET_SEALS) that Wayland compositors and clients use to
//! share buffers safely.
//!
//! A memfd is a tmpfs inode that was never linked into a directory; a
//! wrapper inode adds the seals and enforces them on writes, size changes
//! and writable shared mappings (`check_map`).
//! F_SEAL_WRITE is refused while a writable shared mapping exists
//! (`syscall::fdobj::seals` looks for one), as in Linux.

use super::*;
use crate::errno::*;

pub const F_SEAL_SEAL: u32 = 0x1;
pub const F_SEAL_SHRINK: u32 = 0x2;
pub const F_SEAL_GROW: u32 = 0x4;
pub const F_SEAL_WRITE: u32 = 0x8;
pub const F_SEAL_FUTURE_WRITE: u32 = 0x10;
const ALL_SEALS: u32 = 0x1f;

pub const MFD_CLOEXEC: u32 = 1;
pub const MFD_ALLOW_SEALING: u32 = 2;

/// The memfd filesystem: one tmpfs instance holding no names.
static MEMFD_FS: spin::Once<Arc<tmpfs::TmpFs>> = spin::Once::new();

pub struct Memfd {
    inner: Arc<dyn Inode>,
    seals: AtomicU32,
}

impl Memfd {
    /// A new empty memfd. Without MFD_ALLOW_SEALING it starts sealed
    /// against further seals, as in Linux.
    pub fn new(flags: u32) -> KResult<Arc<Memfd>> {
        let fs = MEMFD_FS.call_once(tmpfs::TmpFs::new);
        let root = fs.root();
        // Create and unlink at once: the inode lives as long as its files.
        let name = alloc::format!(
            "memfd.{}",
            NEXT_MEMFD.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
        );
        let inner = root.create(&name, FileType::Regular, 0o600)?;
        root.unlink(&name)?;
        Ok(Arc::new(Memfd {
            inner,
            seals: AtomicU32::new(if flags & MFD_ALLOW_SEALING != 0 {
                0
            } else {
                F_SEAL_SEAL
            }),
        }))
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
        let cur = self.seals();
        if cur & F_SEAL_SEAL != 0 {
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
}

static NEXT_MEMFD: AtomicU64 = AtomicU64::new(1);

impl Inode for Memfd {
    fn metadata(&self) -> KResult<Metadata> {
        self.inner.metadata()
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        self.inner.read_at(off, buf)
    }
    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        let seals = self.seals();
        if seals & (F_SEAL_WRITE | F_SEAL_FUTURE_WRITE) != 0 {
            return Err(EPERM);
        }
        if seals & F_SEAL_GROW != 0 && off + buf.len() as u64 > self.inner.metadata()?.size {
            return Err(EPERM);
        }
        self.inner.write_at(off, buf)
    }
    fn cacheable(&self) -> bool {
        self.inner.cacheable()
    }
    fn read_direct(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        self.inner.read_direct(off, buf)
    }
    fn truncate(&self, size: u64) -> KResult<()> {
        let seals = self.seals();
        let cur = self.inner.metadata()?.size;
        if (size < cur && seals & F_SEAL_SHRINK != 0) || (size > cur && seals & F_SEAL_GROW != 0) {
            return Err(EPERM);
        }
        self.inner.truncate(size)
    }
    fn fallocate(&self, mode: u32, off: u64, len: u64) -> KResult<()> {
        if self.seals() & F_SEAL_GROW != 0 && off + len > self.inner.metadata()?.size {
            return Err(EPERM);
        }
        self.inner.fallocate(mode, off, len)
    }
    fn chmod(&self, mode: u32) -> KResult<()> {
        self.inner.chmod(mode)
    }
    fn set_times(&self, atime: Option<u64>, mtime: Option<u64>) -> KResult<()> {
        self.inner.set_times(atime, mtime)
    }
    fn fs_id(&self) -> usize {
        self.inner.fs_id()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
