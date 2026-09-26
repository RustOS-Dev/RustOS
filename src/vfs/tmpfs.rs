//! In-memory filesystem (root filesystem, /tmp, initramfs contents).

use super::*;
use alloc::collections::BTreeMap;
use alloc::sync::Weak;

static NEXT_FS_ID: AtomicU64 = AtomicU64::new(1);

pub struct TmpFs {
    root: Arc<TmpInode>,
    id: usize,
    next_ino: AtomicU64,
}

enum Content {
    File(Vec<u8>),
    Dir(BTreeMap<String, Arc<TmpInode>>),
    Symlink(String),
    Special,
}

pub struct TmpInode {
    fs: Weak<TmpFs>,
    fs_id: usize,
    ino: u64,
    meta: Mutex<Metadata>,
    content: RwLock<Content>,
    /// Stream object for device/fifo nodes created with mknod.
    special: Mutex<Option<Arc<dyn FileLike>>>,
}

impl TmpFs {
    pub fn new() -> Arc<TmpFs> {
        let id = NEXT_FS_ID.fetch_add(1, Ordering::SeqCst) as usize | (1 << 40);
        Arc::new_cyclic(|weak: &Weak<TmpFs>| {
            let now = crate::time::unix_time();
            let mut meta = Metadata::new(FileType::Directory, 0o755);
            meta.ino = 1;
            meta.nlink = 2;
            meta.atime = now;
            meta.mtime = now;
            meta.ctime = now;
            TmpFs {
                root: Arc::new(TmpInode {
                    fs: weak.clone(),
                    fs_id: id,
                    ino: 1,
                    meta: Mutex::new(meta),
                    content: RwLock::new(Content::Dir(BTreeMap::new())),
                    special: Mutex::new(None),
                }),
                id,
                next_ino: AtomicU64::new(2),
            }
        })
    }

    fn new_inode(&self, kind: FileType, mode: u32, content: Content) -> Arc<TmpInode> {
        let ino = self.next_ino.fetch_add(1, Ordering::SeqCst);
        let now = crate::time::unix_time();
        let mut meta = Metadata::new(kind, mode);
        meta.ino = ino;
        meta.nlink = if kind == FileType::Directory { 2 } else { 1 };
        meta.atime = now;
        meta.mtime = now;
        meta.ctime = now;
        Arc::new(TmpInode {
            fs: self.root.fs.clone(),
            fs_id: self.id,
            ino,
            meta: Mutex::new(meta),
            content: RwLock::new(content),
            special: Mutex::new(None),
        })
    }
}

impl FileSystem for TmpFs {
    fn root(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }
    fn name(&self) -> &'static str {
        "tmpfs"
    }
    fn statfs(&self) -> StatFs {
        let (free, total) = crate::mm::memory_stats();
        StatFs {
            fs_type: 0x0102_1994,
            block_size: 4096,
            blocks: total / 4096,
            blocks_free: free / 4096,
            files: 0,
            files_free: 0,
            name_max: 255,
        }
    }
}

impl TmpInode {
    fn fs(&self) -> KResult<Arc<TmpFs>> {
        self.fs.upgrade().ok_or(EIO)
    }

    fn touch(&self) {
        let now = crate::time::unix_time();
        let mut m = self.meta.lock();
        m.mtime = now;
        m.ctime = now;
    }

    fn child(&self, name: &str) -> KResult<Arc<TmpInode>> {
        match &*self.content.read() {
            Content::Dir(d) => d.get(name).cloned().ok_or(ENOENT),
            _ => Err(ENOTDIR),
        }
    }

    /// Attach a stream object (used by devfs-style nodes and FIFOs).
    pub fn set_special(&self, s: Arc<dyn FileLike>) {
        *self.special.lock() = Some(s);
    }
}

impl Inode for TmpInode {
    fn metadata(&self) -> KResult<Metadata> {
        let mut m = *self.meta.lock();
        if let Content::File(d) = &*self.content.read() {
            m.size = d.len() as u64;
            m.blocks = m.size.div_ceil(512);
        } else if let Content::Symlink(t) = &*self.content.read() {
            m.size = t.len() as u64;
        }
        Ok(m)
    }

    fn lookup(&self, name: &str) -> KResult<Arc<dyn Inode>> {
        self.child(name).map(|c| c as Arc<dyn Inode>)
    }

    fn create(&self, name: &str, kind: FileType, mode: u32) -> KResult<Arc<dyn Inode>> {
        let fs = self.fs()?;
        let mut content = self.content.write();
        let Content::Dir(d) = &mut *content else {
            return Err(ENOTDIR);
        };
        if d.contains_key(name) {
            return Err(EEXIST);
        }
        let c = match kind {
            FileType::Directory => Content::Dir(BTreeMap::new()),
            FileType::Regular => Content::File(Vec::new()),
            FileType::Symlink => Content::Symlink(String::new()),
            _ => Content::Special,
        };
        let node = fs.new_inode(kind, mode, c);
        if kind == FileType::Fifo {
            node.set_special(super::pipe::Pipe::new_fifo());
        }
        d.insert(name.to_string(), node.clone());
        drop(content);
        if kind == FileType::Directory {
            self.meta.lock().nlink += 1;
        }
        self.touch();
        Ok(node)
    }

    fn link(&self, name: &str, target: &Arc<dyn Inode>) -> KResult<()> {
        let t = target.as_any().downcast_ref::<TmpInode>().ok_or(EXDEV)?;
        if t.meta.lock().kind == FileType::Directory {
            return Err(EPERM);
        }
        let mut content = self.content.write();
        let Content::Dir(d) = &mut *content else {
            return Err(ENOTDIR);
        };
        if d.contains_key(name) {
            return Err(EEXIST);
        }
        // Re-find the Arc for the target through the source directory is not
        // possible here, so hard links share metadata by pointer: rebuild an
        // Arc from the trait object.
        let arc: Arc<TmpInode> = unsafe {
            let raw = Arc::into_raw(target.clone()) as *const TmpInode;
            Arc::from_raw(raw)
        };
        arc.meta.lock().nlink += 1;
        d.insert(name.to_string(), arc);
        Ok(())
    }

    fn unlink(&self, name: &str) -> KResult<()> {
        let mut content = self.content.write();
        let Content::Dir(d) = &mut *content else {
            return Err(ENOTDIR);
        };
        let n = d.get(name).ok_or(ENOENT)?;
        if n.meta.lock().kind == FileType::Directory {
            return Err(EISDIR);
        }
        let n = d.remove(name).unwrap();
        let mut m = n.meta.lock();
        m.nlink = m.nlink.saturating_sub(1);
        drop(m);
        drop(content);
        self.touch();
        Ok(())
    }

    fn rmdir(&self, name: &str) -> KResult<()> {
        let mut content = self.content.write();
        let Content::Dir(d) = &mut *content else {
            return Err(ENOTDIR);
        };
        let n = d.get(name).ok_or(ENOENT)?;
        match &*n.content.read() {
            Content::Dir(c) if !c.is_empty() => return Err(ENOTEMPTY),
            Content::Dir(_) => {}
            _ => return Err(ENOTDIR),
        }
        d.remove(name);
        drop(content);
        self.meta.lock().nlink -= 1;
        self.touch();
        Ok(())
    }

    fn rename(&self, old: &str, new_dir: &Arc<dyn Inode>, new: &str) -> KResult<()> {
        let nd = new_dir.as_any().downcast_ref::<TmpInode>().ok_or(EXDEV)?;
        let node = self.child(old)?;
        let same = core::ptr::eq(self, nd);
        // Replace an existing target (must be compatible).
        if let Ok(existing) = nd.child(new) {
            if Arc::ptr_eq(&existing, &node) {
                return Ok(());
            }
            let ek = existing.meta.lock().kind;
            let nk = node.meta.lock().kind;
            if ek == FileType::Directory {
                if nk != FileType::Directory {
                    return Err(EISDIR);
                }
                nd.rmdir(new)?;
            } else {
                if nk == FileType::Directory {
                    return Err(ENOTDIR);
                }
                nd.unlink(new)?;
            }
        }
        if same {
            let mut content = self.content.write();
            let Content::Dir(d) = &mut *content else {
                return Err(ENOTDIR);
            };
            let n = d.remove(old).ok_or(ENOENT)?;
            d.insert(new.to_string(), n);
        } else {
            let n = {
                let mut content = self.content.write();
                let Content::Dir(d) = &mut *content else {
                    return Err(ENOTDIR);
                };
                d.remove(old).ok_or(ENOENT)?
            };
            let is_dir = n.meta.lock().kind == FileType::Directory;
            {
                let mut content = nd.content.write();
                let Content::Dir(d) = &mut *content else {
                    return Err(ENOTDIR);
                };
                d.insert(new.to_string(), n);
            }
            if is_dir {
                self.meta.lock().nlink -= 1;
                nd.meta.lock().nlink += 1;
            }
        }
        self.touch();
        nd.touch();
        Ok(())
    }

    fn readdir(&self) -> KResult<Vec<DirEntry>> {
        match &*self.content.read() {
            Content::Dir(d) => Ok(d
                .iter()
                .map(|(name, n)| DirEntry {
                    name: name.clone(),
                    ino: n.ino,
                    kind: n.meta.lock().kind,
                })
                .collect()),
            _ => Err(ENOTDIR),
        }
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> KResult<usize> {
        match &*self.content.read() {
            Content::File(d) => {
                let off = off as usize;
                if off >= d.len() {
                    return Ok(0);
                }
                let n = buf.len().min(d.len() - off);
                buf[..n].copy_from_slice(&d[off..off + n]);
                Ok(n)
            }
            Content::Dir(_) => Err(EISDIR),
            _ => Err(EINVAL),
        }
    }

    fn write_at(&self, off: u64, buf: &[u8]) -> KResult<usize> {
        let mut content = self.content.write();
        match &mut *content {
            Content::File(d) => {
                let off = off as usize;
                let end = off.checked_add(buf.len()).ok_or(EFBIG)?;
                if end > d.len() {
                    grow(d, end)?;
                    d.resize(end, 0);
                }
                d[off..end].copy_from_slice(buf);
                drop(content);
                self.touch();
                Ok(buf.len())
            }
            Content::Dir(_) => Err(EISDIR),
            _ => Err(EINVAL),
        }
    }

    fn truncate(&self, size: u64) -> KResult<()> {
        let mut content = self.content.write();
        match &mut *content {
            Content::File(d) => {
                d.resize(size as usize, 0);
                drop(content);
                self.touch();
                Ok(())
            }
            Content::Dir(_) => Err(EISDIR),
            _ => Err(EINVAL),
        }
    }

    fn symlink(&self, name: &str, target: &str) -> KResult<()> {
        let fs = self.fs()?;
        let mut content = self.content.write();
        let Content::Dir(d) = &mut *content else {
            return Err(ENOTDIR);
        };
        if d.contains_key(name) {
            return Err(EEXIST);
        }
        let node = fs.new_inode(
            FileType::Symlink,
            0o777,
            Content::Symlink(target.to_string()),
        );
        d.insert(name.to_string(), node);
        Ok(())
    }

    fn readlink(&self) -> KResult<String> {
        match &*self.content.read() {
            Content::Symlink(t) => Ok(t.clone()),
            _ => Err(EINVAL),
        }
    }

    fn chmod(&self, mode: u32) -> KResult<()> {
        self.meta.lock().mode = mode & 0o7777;
        Ok(())
    }

    fn chown(&self, uid: u32, gid: u32) -> KResult<()> {
        let mut m = self.meta.lock();
        if uid != u32::MAX {
            m.uid = uid;
        }
        if gid != u32::MAX {
            m.gid = gid;
        }
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

    fn open(&self, flags: u32) -> KResult<Option<Arc<dyn FileLike>>> {
        let special = self.special.lock().clone();
        if let Some(s) = &special
            && let Some(hub) = s.as_any().downcast_ref::<super::pipe::FifoHub>()
        {
            return Ok(Some(hub.open_end(flags)));
        }
        Ok(special)
    }

    fn fs_id(&self) -> usize {
        self.fs_id
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[test_case]
fn test_tmpfs_basic_ops() {
    let fs = TmpFs::new();
    let root = fs.root();
    let f = root.create("a.txt", FileType::Regular, 0o644).unwrap();
    assert_eq!(f.write_at(0, b"hello").unwrap(), 5);
    assert_eq!(f.write_at(10, b"x").unwrap(), 1);
    let mut buf = [0u8; 16];
    assert_eq!(f.read_at(0, &mut buf).unwrap(), 11);
    assert_eq!(&buf[..5], b"hello");
    assert_eq!(buf[7], 0);
    let d = root.create("dir", FileType::Directory, 0o755).unwrap();
    root.rename("a.txt", &d, "b.txt").unwrap();
    assert!(root.lookup("a.txt").is_err());
    assert_eq!(d.lookup("b.txt").unwrap().metadata().unwrap().size, 11);
    assert_eq!(root.rmdir("dir"), Err(ENOTEMPTY));
    d.unlink("b.txt").unwrap();
    root.rmdir("dir").unwrap();
    root.symlink("ln", "/target").unwrap();
    assert_eq!(root.lookup("ln").unwrap().readlink().unwrap(), "/target");
}

/// Grow a file buffer to at least `end` bytes without eating the kernel
/// heap's reserve: tmpfs data lives on the heap, and the rest of the kernel
/// must still be able to allocate when /tmp is full.
fn grow(d: &mut Vec<u8>, end: usize) -> KResult<()> {
    if end <= d.capacity() {
        return Ok(());
    }
    let reserve = (crate::allocator::heap_size() / 8).max(8 << 20);
    let free = crate::allocator::heap_free();
    // A reallocation briefly needs the new block next to the old one.
    let doubled = end.max(d.capacity() * 2);
    if doubled as u64 + reserve <= free {
        return d.try_reserve(end - d.len()).map_err(|_| ENOSPC);
    }
    if end as u64 + reserve <= free {
        return d.try_reserve_exact(end - d.len()).map_err(|_| ENOSPC);
    }
    Err(ENOSPC)
}
