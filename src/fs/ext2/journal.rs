//! The ext3/ext4 journal (jbd2): replay at mount and write-ahead logging
//! of metadata changes.
//!
//! While mounted read/write, metadata writes do not go to the disk; they
//! collect in the running transaction (block images) and reads see them.
//! A commit (every few seconds, on sync, when the transaction grows large
//! and at unmount) runs in ordered mode:
//! 1. flush file data written directly to the disk,
//! 2. write the transaction to the log and point the journal superblock
//!    at it (from here on a crash is repaired by replaying the log),
//! 3. write the blocks to their home locations (checkpoint),
//! 4. mark the journal empty again.
//!
//! Filesystem operations hold a handle while they run; a commit waits for
//! running operations so it never captures a half-done change.

use super::Ext2Fs;
use crate::errno::*;
use crate::sched::Tid;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use ext4_core::jbd2;

/// Block images by block number.
type Blocks = BTreeMap<u64, Vec<u8>>;

struct Txn {
    blocks: BTreeMap<u64, Vec<u8>>,
    /// Blocks freed in this transaction: not reused until it commits.
    freed: BTreeSet<u64>,
}

pub(super) struct Journal {
    sb: crate::sync::Mutex<jbd2::Super>,
    /// Journal block runs: (first log block, first disk block, length).
    map: Vec<(u32, u64, u32)>,
    txn: crate::sync::Mutex<Txn>,
    /// Blocks of the transaction being committed: still the current
    /// contents until the checkpoint has written them to the device.
    inflight: crate::sync::Mutex<Option<Arc<Blocks>>>,
    /// Operations in progress.
    active: AtomicUsize,
    /// Threads holding handles (handles nest).
    holders: crate::sync::Mutex<BTreeMap<Tid, usize>>,
    committing: AtomicBool,
    want_commit: AtomicBool,
    commit_lock: crate::sched::mutex::Mutex<()>,
    /// Transaction size (blocks) that triggers an early commit.
    limit: usize,
    /// Read-only mount of a dirty journal: the replayed blocks are kept
    /// in `txn` and never written.
    overlay: AtomicBool,
}

/// An operation in progress (see [`Ext2Fs::begin`]).
pub(super) struct Handle<'a> {
    fs: &'a Ext2Fs,
}

impl Drop for Handle<'_> {
    fn drop(&mut self) {
        let Some(j) = self.fs.jnl() else { return };
        let tid = crate::sched::current_tid();
        {
            let mut h = j.holders.lock();
            let n = h.get_mut(&tid).expect("journal handle");
            *n -= 1;
            if *n > 0 {
                return;
            }
            h.remove(&tid);
        }
        if j.active.fetch_sub(1, Ordering::SeqCst) == 1
            && j.want_commit.swap(false, Ordering::SeqCst)
            && let Some(fs) = self.fs.me.get().and_then(|w| w.upgrade())
        {
            // Commit from the kernel worker: the caller may hold locks.
            crate::sched::defer(move || {
                let _ = fs.commit();
            });
        }
    }
}

impl Ext2Fs {
    /// Start a modifying operation: a commit waits until it ends.
    pub(super) fn begin(&self) -> Handle<'_> {
        if let Some(j) = self.jnl() {
            let tid = crate::sched::current_tid();
            if let Some(n) = j.holders.lock().get_mut(&tid) {
                *n += 1;
                return Handle { fs: self };
            }
            loop {
                while j.committing.load(Ordering::SeqCst) {
                    crate::sched::yield_now();
                }
                j.active.fetch_add(1, Ordering::SeqCst);
                if !j.committing.load(Ordering::SeqCst) {
                    break;
                }
                j.active.fetch_sub(1, Ordering::SeqCst);
            }
            j.holders.lock().insert(tid, 1);
        }
        Handle { fs: self }
    }
}

impl Journal {
    /// Locate the journal of `fs` (inode `ino`) and read its superblock.
    pub(super) fn open(fs: &Ext2Fs, ino: u32) -> KResult<Journal> {
        let bs = fs.block_size;
        let inode = fs.inode(ino)?;
        let mut st = inode.st.lock();
        let len = (st.size / bs) as u32;
        let mut map: Vec<(u32, u64, u32)> = Vec::new();
        for l in 0..len {
            let p = inode.bmap(&mut st, l as u64, false)?.ok_or(EIO)?;
            match map.last_mut() {
                Some((s, d, n)) if *s + *n == l && *d + *n as u64 == p => *n += 1,
                _ => map.push((l, p, 1)),
            }
        }
        drop(st);
        let first = map.first().ok_or(EIO)?.1;
        let mut b = vec![0u8; bs as usize];
        fs.dev.read_bytes(first * bs, &mut b)?;
        let sb = jbd2::Super::parse(&b).map_err(|e| {
            crate::println!("[ext4] journal superblock: {:?}", e);
            EINVAL
        })?;
        if sb.block_size as u64 != bs || sb.maxlen > len {
            return Err(EINVAL);
        }
        let limit = ((sb.maxlen - sb.first) as usize / 4).max(16);
        Ok(Journal {
            sb: crate::sync::Mutex::new(sb),
            map,
            txn: crate::sync::Mutex::new(Txn {
                blocks: BTreeMap::new(),
                freed: BTreeSet::new(),
            }),
            inflight: crate::sync::Mutex::new(None),
            active: AtomicUsize::new(0),
            holders: crate::sync::Mutex::new(BTreeMap::new()),
            committing: AtomicBool::new(false),
            want_commit: AtomicBool::new(false),
            commit_lock: crate::sched::mutex::Mutex::new(()),
            limit,
            overlay: AtomicBool::new(false),
        })
    }

    /// Whether the log holds transactions to replay.
    pub(super) fn dirty(&self) -> bool {
        self.sb.lock().start != 0
    }

    pub(super) fn has_overlay(&self) -> bool {
        self.overlay.load(Ordering::SeqCst)
    }

    /// Read-only mount: keep the committed transactions in memory so reads
    /// see the recovered state without writing the device.
    pub(super) fn overlay(&self, fs: &Ext2Fs) -> KResult<()> {
        let sb = self.sb.lock().clone();
        let plan = jbd2::plan_replay(&sb, |l| self.read_log(fs, l));
        let mut t = self.txn.lock();
        for w in &plan.writes {
            let mut d = self.read_log(fs, w.log_block).ok_or(EIO)?;
            jbd2::unescape(&mut d, w);
            t.blocks.insert(w.target, d);
        }
        drop(t);
        self.overlay.store(true, Ordering::SeqCst);
        crate::println!(
            "[ext4] journal needs recovery; read-only: using {} replayed block(s) in memory",
            plan.writes.len()
        );
        Ok(())
    }

    fn phys(&self, l: u32) -> Option<u64> {
        self.map
            .iter()
            .find(|(s, _, n)| l >= *s && l < *s + *n)
            .map(|(s, d, _)| *d + (l - *s) as u64)
    }

    fn read_log(&self, fs: &Ext2Fs, l: u32) -> Option<Vec<u8>> {
        let mut b = vec![0u8; fs.block_size as usize];
        fs.dev
            .read_bytes(self.phys(l)? * fs.block_size, &mut b)
            .ok()?;
        Some(b)
    }

    fn write_sb(&self, fs: &Ext2Fs, start: u32, seq: u32) -> KResult<()> {
        let data = self.sb.lock().encode(start, seq);
        let p = self.phys(0).ok_or(EIO)?;
        fs.dev.write_bytes(p * fs.block_size, &data)?;
        let mut sb = self.sb.lock();
        sb.start = start;
        sb.sequence = seq;
        Ok(())
    }

    /// Replay committed transactions left by a crash. Returns the number
    /// of blocks written.
    pub(super) fn replay(&self, fs: &Ext2Fs) -> KResult<usize> {
        let sb = self.sb.lock().clone();
        let plan = jbd2::plan_replay(&sb, |l| self.read_log(fs, l));
        let n = plan.writes.len();
        for w in &plan.writes {
            let mut d = self.read_log(fs, w.log_block).ok_or(EIO)?;
            jbd2::unescape(&mut d, w);
            fs.dev.write_bytes(w.target * fs.block_size, &d)?;
        }
        fs.dev.sync()?;
        self.write_sb(fs, 0, plan.next_sequence)?;
        fs.dev.sync()?;
        crate::println!(
            "[ext4] journal: replayed {} transaction(s), {} block(s)",
            plan.transactions,
            n
        );
        Ok(n)
    }

    /// Current image of metadata block `b` if the running transaction
    /// changed it.
    pub(super) fn cached(&self, b: u64, buf: &mut [u8]) -> bool {
        // Look in the transaction and the commit in progress under the
        // transaction lock, so a commit cannot move the block in between.
        let t = self.txn.lock();
        if let Some(d) = t.blocks.get(&b) {
            buf.copy_from_slice(d);
            return true;
        }
        let inflight = self.inflight.lock().clone();
        drop(t);
        match inflight.as_ref().and_then(|m| m.get(&b)) {
            Some(d) => {
                buf.copy_from_slice(d);
                true
            }
            None => false,
        }
    }

    /// Record a new image of metadata block `b`.
    pub(super) fn log(&self, b: u64, data: Vec<u8>) {
        let mut t = self.txn.lock();
        t.blocks.insert(b, data);
        if t.blocks.len() >= self.limit {
            self.want_commit.store(true, Ordering::SeqCst);
        }
    }

    /// `b` was freed: forget any logged image and hold it back from
    /// reallocation until the transaction commits.
    pub(super) fn freed(&self, b: u64) {
        let mut t = self.txn.lock();
        t.blocks.remove(&b);
        t.freed.insert(b);
    }

    pub(super) fn is_freed(&self, b: u64) -> bool {
        self.txn.lock().freed.contains(&b)
    }

    /// Commit the running transaction (see the module documentation).
    pub(super) fn commit(&self, fs: &Ext2Fs) -> KResult<()> {
        if self.has_overlay() {
            return Ok(());
        }
        if self
            .holders
            .lock()
            .contains_key(&crate::sched::current_tid())
        {
            // Inside an operation: commit when it ends.
            self.want_commit.store(true, Ordering::SeqCst);
            return Ok(());
        }
        let _serial = self.commit_lock.lock();
        self.want_commit.store(false, Ordering::SeqCst);
        // Stop new operations and wait for running ones.
        self.committing.store(true, Ordering::SeqCst);
        while self.active.load(Ordering::SeqCst) != 0 {
            crate::sched::yield_now();
        }
        let blocks = {
            let mut t = self.txn.lock();
            t.freed.clear();
            let b = Arc::new(core::mem::take(&mut t.blocks));
            // Published before the transaction lock drops: a reader must
            // never find a block in neither place (it would read the
            // stale copy on the device).
            if !b.is_empty() {
                *self.inflight.lock() = Some(b.clone());
            }
            b
        };
        self.committing.store(false, Ordering::SeqCst);
        if blocks.is_empty() {
            return Ok(());
        }
        let r = self.write_out(fs, &blocks);
        *self.inflight.lock() = None;
        r
    }

    /// Log, checkpoint and retire one transaction.
    fn write_out(&self, fs: &Ext2Fs, blocks: &BTreeMap<u64, Vec<u8>>) -> KResult<()> {
        let bs = fs.block_size;
        // 1. Ordered mode: file data first.
        fs.dev.sync()?;
        let sb = self.sb.lock().clone();
        let seq = sb.sequence;
        let list: Vec<(u64, &[u8])> = blocks.iter().map(|(b, d)| (*b, &d[..])).collect();
        let logged = match jbd2::build_transaction(&sb, seq, &list, crate::time::unix_time()) {
            Some(log) => {
                // 2. The log, then the superblock pointing at it.
                for (i, blk) in log.iter().enumerate() {
                    let p = self.phys(sb.first + i as u32).ok_or(EIO)?;
                    fs.dev.write_bytes(p * bs, blk)?;
                }
                fs.dev.sync()?;
                self.write_sb(fs, sb.first, seq)?;
                fs.dev.sync()?;
                true
            }
            None => {
                crate::println!("[ext4] journal: transaction too large; writing in place");
                false
            }
        };
        // 3. Checkpoint.
        for (b, d) in blocks.iter() {
            fs.dev.write_bytes(b * bs, d)?;
        }
        fs.dev.sync()?;
        // 4. The journal is empty again.
        if logged {
            self.write_sb(fs, 0, seq.wrapping_add(1))?;
            fs.dev.sync()?;
        }
        Ok(())
    }
}
