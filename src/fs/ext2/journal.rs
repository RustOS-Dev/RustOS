//! The ext3/ext4 journal (jbd2): replay at mount and write-ahead logging
//! of metadata changes.
//!
//! While mounted read/write, metadata writes do not go to the disk; they
//! collect in the running transaction (block images) and reads see them.
//! A commit (every few seconds, on sync, when the transaction grows large
//! and at unmount):
//! 1. flushes file data written directly to the disk (data=ordered; not
//!    with data=writeback; with data=journal file data is logged too),
//! 2. appends the transaction to the log (descriptor and data blocks, a
//!    flush unless checksums or async_commit make the commit block
//!    self-validating, the commit block) and, if the log was empty, points
//!    the journal superblock at it. From here on a crash is repaired by
//!    replaying the log.
//!
//! Committed blocks wait in memory (reads see them) until a checkpoint
//! writes them to their home locations and empties the log: when the log
//! is half full, when many blocks are waiting, when a block freed by the
//! transaction still has a logged image (it must not be replayed over a
//! new owner), and at unmount.
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

/// Where the log stands: transactions from `start` (sequence
/// `start_seq`) to `head` are committed but not checkpointed.
#[derive(Clone, Copy, Default)]
struct LogState {
    /// First log block of the oldest live transaction (0: log empty).
    start: u32,
    head: u32,
    next_seq: u32,
    used: u32,
}

/// Committed blocks waiting for the checkpoint beyond this many trigger it.
const CKPT_MAX_BLOCKS: usize = 2048;

pub(super) struct Journal {
    sb: crate::sync::Mutex<jbd2::Super>,
    log: crate::sync::Mutex<LogState>,
    /// Committed images not yet written home.
    ckpt: crate::sync::Mutex<BTreeMap<u64, Arc<Vec<u8>>>>,
    /// Blocks freed by the transaction being committed: not reusable
    /// until it is safely in the log (and, if logged before, checkpointed).
    held: crate::sync::Mutex<BTreeSet<u64>>,
    /// Skip the flush before file data is logged (data=writeback).
    pub(super) writeback: AtomicBool,
    /// Fast-commit records found by the replay, applied once the
    /// filesystem is up.
    fc_tags: crate::sync::Mutex<Vec<ext4_core::fastcommit::Tag>>,
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

/// The sequence number in a journal block header.
fn be_seq(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[8..12].try_into().unwrap())
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
            log: crate::sync::Mutex::new(LogState::default()),
            ckpt: crate::sync::Mutex::new(BTreeMap::new()),
            held: crate::sync::Mutex::new(BTreeSet::new()),
            writeback: AtomicBool::new(false),
            fc_tags: crate::sync::Mutex::new(Vec::new()),
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
        // Fast commits of the transaction after the last full one.
        if sb.fc_first < sb.fc_end {
            let blocks: Vec<Vec<u8>> = (sb.fc_first..sb.fc_end)
                .map_while(|l| self.read_log(fs, l))
                .collect();
            let tags = ext4_core::fastcommit::scan(&blocks, plan.next_sequence);
            if !tags.is_empty() {
                *self.fc_tags.lock() = tags;
            }
        }
        self.write_sb(fs, 0, plan.next_sequence)?;
        fs.dev.sync()?;
        crate::println!(
            "[ext4] journal: replayed {} transaction(s), {} block(s)",
            plan.transactions,
            n
        );
        Ok(n)
    }

    /// Fast-commit records left to apply (taken once).
    pub(super) fn take_fc_tags(&self) -> Vec<ext4_core::fastcommit::Tag> {
        core::mem::take(&mut *self.fc_tags.lock())
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
        if let Some(d) = inflight.as_ref().and_then(|m| m.get(&b)) {
            buf.copy_from_slice(d);
            return true;
        }
        let c = self.ckpt.lock().get(&b).cloned();
        match c {
            Some(d) => {
                buf.copy_from_slice(&d);
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
        self.txn.lock().freed.contains(&b) || self.held.lock().contains(&b)
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
            let freed = core::mem::take(&mut t.freed);
            self.held.lock().extend(freed);
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
        let r = if blocks.is_empty() {
            Ok(())
        } else {
            self.write_out(fs, &blocks)
        };
        // A freed block whose old image is still logged must be
        // checkpointed before anyone can reuse it.
        let held: Vec<u64> = core::mem::take(&mut *self.held.lock())
            .into_iter()
            .collect();
        let r = r.and_then(|_| {
            let c = self.ckpt.lock();
            let clash = held.iter().any(|b| c.contains_key(b));
            let big = c.len() > CKPT_MAX_BLOCKS;
            drop(c);
            let sb = self.sb.lock().clone();
            let half = self.log.lock().used > (sb.maxlen - sb.first) / 2;
            if clash || big || half {
                self.checkpoint_locked(fs)
            } else {
                Ok(())
            }
        });
        *self.inflight.lock() = None;
        r
    }

    /// Write every committed block home and empty the log (at unmount,
    /// or to make room).
    pub(super) fn checkpoint(&self, fs: &Ext2Fs) -> KResult<()> {
        if self.has_overlay() {
            return Ok(());
        }
        let _serial = self.commit_lock.lock();
        self.checkpoint_locked(fs)
    }

    fn checkpoint_locked(&self, fs: &Ext2Fs) -> KResult<()> {
        let blocks: Vec<(u64, Arc<Vec<u8>>)> = self
            .ckpt
            .lock()
            .iter()
            .map(|(b, d)| (*b, d.clone()))
            .collect();
        let st = *self.log.lock();
        if blocks.is_empty() && st.start == 0 {
            return Ok(());
        }
        for (b, d) in &blocks {
            fs.dev.write_bytes(b * fs.block_size, d)?;
        }
        fs.dev.sync()?;
        if st.start != 0 {
            self.write_sb(fs, 0, st.next_seq)?;
            fs.dev.sync()?;
        }
        let mut c = self.ckpt.lock();
        for (b, d) in &blocks {
            // Keep images a newer commit replaced meanwhile.
            if c.get(b).is_some_and(|x| Arc::ptr_eq(x, d)) {
                c.remove(b);
            }
        }
        *self.log.lock() = LogState::default();
        Ok(())
    }

    /// Append one transaction to the log; its blocks then wait for the
    /// checkpoint.
    fn write_out(&self, fs: &Ext2Fs, blocks: &BTreeMap<u64, Vec<u8>>) -> KResult<()> {
        let bs = fs.block_size;
        let sb = self.sb.lock().clone();
        let cap = sb.maxlen - sb.first;
        let list: Vec<(u64, &[u8])> = blocks.iter().map(|(b, d)| (*b, &d[..])).collect();
        let mut st = *self.log.lock();
        let seq = |st: &LogState| {
            if st.start == 0 {
                self.sb.lock().sequence
            } else {
                st.next_seq
            }
        };
        let Some(log) = jbd2::build_transaction(&sb, seq(&st), &list, crate::time::unix_time())
        else {
            crate::println!("[ext4] journal: transaction too large; writing in place");
            self.checkpoint_locked(fs)?;
            fs.dev.sync()?;
            for (b, d) in blocks.iter() {
                fs.dev.write_bytes(b * bs, d)?;
            }
            return fs.dev.sync();
        };
        if st.start != 0 && st.used + log.len() as u32 + 1 > cap {
            self.checkpoint_locked(fs)?;
            st = *self.log.lock();
        }
        let seq = seq(&st);
        // The sequence may have moved with a checkpoint: rebuild if so.
        let log = if seq == be_seq(&log[0]) {
            log
        } else {
            jbd2::build_transaction(&sb, seq, &list, crate::time::unix_time()).ok_or(EIO)?
        };
        // 1. data=ordered: file data reaches the disk before the metadata
        //    that points at it.
        if !self.writeback.load(Ordering::Relaxed) {
            fs.dev.sync()?;
        }
        // 2. The transaction. Without per-block checksums (or
        //    async_commit), a flush orders the commit block after it.
        let self_checking = sb.csum() || sb.incompat & jbd2::INCOMPAT_ASYNC_COMMIT != 0;
        let head0 = if st.start == 0 { sb.first } else { st.head };
        let mut p = head0;
        let n = log.len();
        for (i, blk) in log.iter().enumerate() {
            if i + 1 == n && !self_checking {
                fs.dev.sync()?;
            }
            fs.dev.write_bytes(self.phys(p).ok_or(EIO)? * bs, blk)?;
            p = if p + 1 >= sb.maxlen { sb.first } else { p + 1 };
        }
        fs.dev.sync()?;
        if st.start == 0 {
            self.write_sb(fs, head0, seq)?;
            fs.dev.sync()?;
            st.start = head0;
            st.used = 0;
        }
        st.head = p;
        st.used += n as u32;
        st.next_seq = seq.wrapping_add(1);
        *self.log.lock() = st;
        let mut c = self.ckpt.lock();
        for (b, d) in blocks.iter() {
            c.insert(*b, Arc::new(d.clone()));
        }
        Ok(())
    }
}
