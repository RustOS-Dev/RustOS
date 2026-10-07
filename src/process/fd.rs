//! Per-process file descriptor tables.

use crate::errno::*;
use crate::vfs::File;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub const MAX_FDS: usize = 1024;

#[derive(Clone)]
pub struct FdEntry {
    pub file: Arc<File>,
    pub cloexec: bool,
}

#[derive(Clone, Default)]
pub struct FdTable {
    entries: Vec<Option<FdEntry>>,
}

impl FdTable {
    pub fn new() -> FdTable {
        FdTable {
            entries: Vec::new(),
        }
    }

    pub fn get(&self, fd: i32) -> KResult<Arc<File>> {
        if fd < 0 {
            return Err(EBADF);
        }
        self.entries
            .get(fd as usize)
            .and_then(|e| e.as_ref())
            .map(|e| e.file.clone())
            .ok_or(EBADF)
    }

    pub fn entry(&self, fd: i32) -> KResult<&FdEntry> {
        if fd < 0 {
            return Err(EBADF);
        }
        self.entries
            .get(fd as usize)
            .and_then(|e| e.as_ref())
            .ok_or(EBADF)
    }

    pub fn set_cloexec(&mut self, fd: i32, v: bool) -> KResult<()> {
        let e = self
            .entries
            .get_mut(fd as usize)
            .and_then(|e| e.as_mut())
            .ok_or(EBADF)?;
        e.cloexec = v;
        Ok(())
    }

    /// Install at the lowest free descriptor >= `min`.
    pub fn install_from(&mut self, min: usize, file: Arc<File>, cloexec: bool) -> KResult<i32> {
        let idx = (min..MAX_FDS)
            .find(|&i| self.entries.get(i).is_none_or(|e| e.is_none()))
            .ok_or(EMFILE)?;
        self.install_at(idx, file, cloexec);
        Ok(idx as i32)
    }

    pub fn install(&mut self, file: Arc<File>, cloexec: bool) -> KResult<i32> {
        self.install_from(0, file, cloexec)
    }

    /// Install at exactly `fd`, replacing (and closing) whatever was there.
    pub fn install_at(&mut self, fd: usize, file: Arc<File>, cloexec: bool) {
        if self.entries.len() <= fd {
            self.entries.resize(fd + 1, None);
        }
        self.entries[fd] = Some(FdEntry { file, cloexec });
    }

    pub fn close(&mut self, fd: i32) -> KResult<Arc<File>> {
        if fd < 0 {
            return Err(EBADF);
        }
        self.entries
            .get_mut(fd as usize)
            .and_then(|e| e.take())
            .map(|e| e.file)
            .ok_or(EBADF)
    }

    pub fn close_on_exec(&mut self) {
        for e in self.entries.iter_mut() {
            if e.as_ref().is_some_and(|x| x.cloexec) {
                *e = None;
            }
        }
    }

    /// One past the highest descriptor slot.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|e| e.is_none())
    }

    pub fn close_all(&mut self) {
        self.entries.clear();
    }

    /// (fd, file) pairs of all open descriptors.
    pub fn list(&self) -> Vec<(i32, Arc<File>)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|e| (i as i32, e.file.clone())))
            .collect()
    }
}
