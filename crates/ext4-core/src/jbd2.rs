//! The jbd2 journal format: superblock, replay planning (scan, revoke,
//! replay passes) and encoding of a transaction. All journal fields are
//! big-endian. Journal blocks are addressed by their index in the journal
//! (the caller maps them to disk blocks).

use crate::csum::crc32c;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

pub const MAGIC: u32 = 0xC03B_3998;
pub const DESCRIPTOR: u32 = 1;
pub const COMMIT: u32 = 2;
pub const SUPERBLOCK_V1: u32 = 3;
pub const SUPERBLOCK_V2: u32 = 4;
pub const REVOKE: u32 = 5;

pub const INCOMPAT_REVOKE: u32 = 0x1;
pub const INCOMPAT_64BIT: u32 = 0x2;
pub const INCOMPAT_ASYNC_COMMIT: u32 = 0x4;
pub const INCOMPAT_CSUM_V2: u32 = 0x8;
pub const INCOMPAT_CSUM_V3: u32 = 0x10;
pub const INCOMPAT_FAST_COMMIT: u32 = 0x20;
const SUPPORTED_INCOMPAT: u32 =
    INCOMPAT_REVOKE | INCOMPAT_64BIT | INCOMPAT_ASYNC_COMMIT | INCOMPAT_CSUM_V2 | INCOMPAT_CSUM_V3;

const FLAG_ESCAPE: u32 = 1;
const FLAG_SAME_UUID: u32 = 2;
const FLAG_DELETED: u32 = 4;
const FLAG_LAST_TAG: u32 = 8;

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}
fn put_be32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    BadSuperblock,
    Unsupported(u32),
}

/// The journal superblock (journal block 0).
#[derive(Clone, Debug)]
pub struct Super {
    pub block_size: u32,
    pub maxlen: u32,
    pub first: u32,
    pub sequence: u32,
    /// First block of the log; 0 = journal empty (clean).
    pub start: u32,
    pub errno: i32,
    pub incompat: u32,
    pub uuid: [u8; 16],
    raw: Vec<u8>,
}

impl Super {
    pub fn parse(b: &[u8]) -> Result<Super, Error> {
        if b.len() < 1024
            || be32(b, 0) != MAGIC
            || !matches!(be32(b, 4), SUPERBLOCK_V1 | SUPERBLOCK_V2)
        {
            return Err(Error::BadSuperblock);
        }
        let v2 = be32(b, 4) == SUPERBLOCK_V2;
        let incompat = if v2 { be32(b, 0x28) } else { 0 };
        if incompat & !SUPPORTED_INCOMPAT != 0 {
            return Err(Error::Unsupported(incompat & !SUPPORTED_INCOMPAT));
        }
        let s = Super {
            block_size: be32(b, 0xC),
            maxlen: be32(b, 0x10),
            first: be32(b, 0x14),
            sequence: be32(b, 0x18),
            start: be32(b, 0x1C),
            errno: be32(b, 0x20) as i32,
            incompat,
            uuid: b[0x30..0x40].try_into().unwrap(),
            raw: b[..1024].to_vec(),
        };
        if s.block_size < 1024 || s.first == 0 || s.first >= s.maxlen {
            return Err(Error::BadSuperblock);
        }
        Ok(s)
    }

    pub fn csum(&self) -> bool {
        self.incompat & (INCOMPAT_CSUM_V2 | INCOMPAT_CSUM_V3) != 0
    }
    fn csum_v3(&self) -> bool {
        self.incompat & INCOMPAT_CSUM_V3 != 0
    }
    fn is64(&self) -> bool {
        self.incompat & INCOMPAT_64BIT != 0
    }
    pub fn seed(&self) -> u32 {
        crc32c(!0, &self.uuid)
    }
    fn tag_size(&self) -> usize {
        if self.csum_v3() {
            16
        } else if self.is64() {
            12
        } else {
            8
        }
    }
    fn tail(&self) -> usize {
        if self.csum() { 4 } else { 0 }
    }

    /// Next log block after `b`, wrapping inside [first, maxlen).
    fn next(&self, b: u32) -> u32 {
        if b + 1 >= self.maxlen {
            self.first
        } else {
            b + 1
        }
    }

    /// The superblock bytes (1024) with `start`/`sequence` updated and the
    /// checksum recomputed.
    pub fn encode(&self, start: u32, sequence: u32) -> Vec<u8> {
        let mut b = self.raw.clone();
        put_be32(&mut b, 0x18, sequence);
        put_be32(&mut b, 0x1C, start);
        if self.csum() {
            put_be32(&mut b, 0xFC, 0);
            let c = crc32c(!0, &b);
            put_be32(&mut b, 0xFC, c);
        }
        b
    }
}

/// One block to copy from the journal to the filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayWrite {
    pub target: u64,
    pub log_block: u32,
    /// The block began with the journal magic, which was zeroed.
    pub escaped: bool,
    pub sequence: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replay {
    pub writes: Vec<ReplayWrite>,
    /// Sequence number for the next transaction.
    pub next_sequence: u32,
    pub transactions: u32,
}

struct Tag {
    target: u64,
    flags: u32,
    csum: u32,
}

fn tags(sb: &Super, block: &[u8]) -> Vec<Tag> {
    let mut out = Vec::new();
    let ts = sb.tag_size();
    let end = block.len() - sb.tail();
    let mut o = 12;
    while o + ts <= end {
        let (lo, flags, hi, csum) = if sb.csum_v3() {
            (
                be32(block, o),
                be32(block, o + 4),
                be32(block, o + 8),
                be32(block, o + 12),
            )
        } else {
            let flags = u16::from_be_bytes([block[o + 6], block[o + 7]]) as u32;
            let hi = if sb.is64() { be32(block, o + 8) } else { 0 };
            (
                be32(block, o),
                flags,
                hi,
                u16::from_be_bytes([block[o + 4], block[o + 5]]) as u32,
            )
        };
        let target = lo as u64 | if sb.is64() { (hi as u64) << 32 } else { 0 };
        out.push(Tag {
            target,
            flags,
            csum,
        });
        o += ts;
        if flags & FLAG_SAME_UUID == 0 {
            o += 16;
        }
        if flags & FLAG_LAST_TAG != 0 {
            break;
        }
    }
    out
}

fn tail_ok(sb: &Super, block: &[u8]) -> bool {
    if !sb.csum() {
        return true;
    }
    let n = block.len();
    let mut b = block.to_vec();
    b[n - 4..].fill(0);
    crc32c(sb.seed(), &b) == be32(block, n - 4)
}

fn commit_ok(sb: &Super, block: &[u8]) -> bool {
    if !sb.csum() {
        return true;
    }
    let mut b = block.to_vec();
    put_be32(&mut b, 16, 0);
    crc32c(sb.seed(), &b) == be32(block, 16)
}

/// Plan the recovery of a journal. `read(i)` returns log block `i`.
/// Only transactions with a valid commit block are replayed; blocks
/// revoked by a later (or the same) transaction are skipped.
pub fn plan_replay(sb: &Super, mut read: impl FnMut(u32) -> Option<Vec<u8>>) -> Replay {
    let mut replay = Replay {
        next_sequence: sb.sequence,
        ..Default::default()
    };
    if sb.start == 0 {
        return replay;
    }
    // Pass 1: find the committed transactions, collect revokes and the
    // candidate writes of each transaction.
    let mut revoked: BTreeMap<u64, u32> = BTreeMap::new();
    let mut committed: Vec<ReplayWrite> = Vec::new();
    let mut pending: Vec<ReplayWrite> = Vec::new();
    let mut seq = sb.sequence;
    let mut blk = sb.start;
    let mut steps = 0u32;
    while steps < sb.maxlen {
        let Some(b) = read(blk) else { break };
        if be32(&b, 0) != MAGIC || be32(&b, 8) != seq {
            break;
        }
        match be32(&b, 4) {
            DESCRIPTOR => {
                if !tail_ok(sb, &b) {
                    break;
                }
                let mut data = blk;
                for t in tags(sb, &b) {
                    data = sb.next(data);
                    steps += 1;
                    if t.flags & FLAG_DELETED != 0 {
                        continue;
                    }
                    if sb.csum()
                        && let Some(d) = read(data)
                    {
                        let c = crc32c(crc32c(sb.seed(), &seq.to_be_bytes()), &d);
                        let want = if sb.csum_v3() { c } else { c & 0xFFFF };
                        if want != t.csum {
                            // A torn transaction: its commit will not
                            // match either; stop at it.
                            pending.clear();
                        }
                    }
                    pending.push(ReplayWrite {
                        target: t.target,
                        log_block: data,
                        escaped: t.flags & FLAG_ESCAPE != 0,
                        sequence: seq,
                    });
                }
                blk = data;
            }
            COMMIT => {
                if !commit_ok(sb, &b) {
                    break;
                }
                committed.append(&mut pending);
                seq = seq.wrapping_add(1);
                replay.transactions += 1;
            }
            REVOKE => {
                if !tail_ok(sb, &b) {
                    break;
                }
                let used = (be32(&b, 12) as usize).min(b.len() - sb.tail());
                let w = if sb.is64() { 8 } else { 4 };
                let mut o = 16;
                while o + w <= used {
                    let t = if w == 8 {
                        u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
                    } else {
                        be32(&b, o) as u64
                    };
                    let e = revoked.entry(t).or_insert(seq);
                    *e = (*e).max(seq);
                    o += w;
                }
            }
            _ => break,
        }
        blk = sb.next(blk);
        steps += 1;
    }
    replay.next_sequence = seq;
    replay.writes = committed
        .into_iter()
        .filter(|w| revoked.get(&w.target).is_none_or(|&r| r < w.sequence))
        .collect();
    replay
}

/// Restore an escaped data block.
pub fn unescape(block: &mut [u8], w: &ReplayWrite) {
    if w.escaped {
        block[..4].copy_from_slice(&MAGIC.to_be_bytes());
    }
}

/// Encode one transaction (`seq`) holding `blocks` (target, contents) as
/// log blocks starting at the journal's first block: descriptor blocks
/// followed by their data, then the commit block. Returns the log blocks
/// in order (to write at first, first+1, ...), or None if they do not fit.
pub fn build_transaction(
    sb: &Super,
    seq: u32,
    blocks: &[(u64, &[u8])],
    commit_time: u64,
) -> Option<Vec<Vec<u8>>> {
    let bs = sb.block_size as usize;
    let ts = sb.tag_size();
    let room = bs - 12 - sb.tail() - 16; // first tag carries the UUID
    let per_desc = room / ts;
    let mut out: Vec<Vec<u8>> = Vec::new();
    for chunk in blocks.chunks(per_desc.max(1)) {
        let mut desc = vec![0u8; bs];
        put_be32(&mut desc, 0, MAGIC);
        put_be32(&mut desc, 4, DESCRIPTOR);
        put_be32(&mut desc, 8, seq);
        let mut o = 12;
        let mut datas = Vec::with_capacity(chunk.len());
        for (i, (target, data)) in chunk.iter().enumerate() {
            if !sb.is64() && *target > u32::MAX as u64 {
                return None;
            }
            let mut d = data.to_vec();
            d.resize(bs, 0);
            let mut flags = 0;
            if be32(&d, 0) == MAGIC {
                d[..4].fill(0);
                flags |= FLAG_ESCAPE;
            }
            if i > 0 {
                flags |= FLAG_SAME_UUID;
            }
            if i + 1 == chunk.len() {
                flags |= FLAG_LAST_TAG;
            }
            // Tag checksums cover the block as stored in the journal.
            let c = crc32c(crc32c(sb.seed(), &seq.to_be_bytes()), &d);
            if sb.csum_v3() {
                put_be32(&mut desc, o, *target as u32);
                put_be32(&mut desc, o + 4, flags);
                put_be32(&mut desc, o + 8, (*target >> 32) as u32);
                put_be32(&mut desc, o + 12, c);
            } else {
                put_be32(&mut desc, o, *target as u32);
                let c16 = if sb.csum() { c as u16 } else { 0 };
                desc[o + 4..o + 6].copy_from_slice(&c16.to_be_bytes());
                desc[o + 6..o + 8].copy_from_slice(&(flags as u16).to_be_bytes());
                if sb.is64() {
                    put_be32(&mut desc, o + 8, (*target >> 32) as u32);
                }
            }
            o += ts;
            if i == 0 {
                desc[o..o + 16].copy_from_slice(&sb.uuid);
                o += 16;
            }
            datas.push(d);
        }
        if sb.csum() {
            let c = crc32c(sb.seed(), &desc);
            put_be32(&mut desc, bs - 4, c);
        }
        out.push(desc);
        out.extend(datas);
    }
    let mut commit = vec![0u8; bs];
    put_be32(&mut commit, 0, MAGIC);
    put_be32(&mut commit, 4, COMMIT);
    put_be32(&mut commit, 8, seq);
    commit[0x30..0x38].copy_from_slice(&commit_time.to_be_bytes());
    if sb.csum() {
        let c = crc32c(sb.seed(), &commit);
        put_be32(&mut commit, 16, c);
    }
    out.push(commit);
    (out.len() as u32 <= sb.maxlen - sb.first).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sb(incompat: u32) -> Super {
        let mut b = vec![0u8; 1024];
        put_be32(&mut b, 0, MAGIC);
        put_be32(&mut b, 4, SUPERBLOCK_V2);
        put_be32(&mut b, 0xC, 1024);
        put_be32(&mut b, 0x10, 64);
        put_be32(&mut b, 0x14, 1);
        put_be32(&mut b, 0x18, 7);
        put_be32(&mut b, 0x28, incompat);
        b[0x30..0x40].copy_from_slice(&[9; 16]);
        Super::parse(&b).unwrap()
    }

    fn round_trip(incompat: u32) {
        let s = sb(incompat);
        let mut magic_block = vec![0x55u8; 1024];
        magic_block[..4].copy_from_slice(&MAGIC.to_be_bytes());
        let a = vec![0xAAu8; 1024];
        let blocks: Vec<(u64, &[u8])> = vec![(100, &a), (200, &magic_block)];
        let log = build_transaction(&s, 7, &blocks, 1).unwrap();
        assert_eq!(log.len(), 4); // descriptor, 2 data, commit
        // As the journal would look on disk after the commit.
        let mut journal = vec![vec![0u8; 1024]; 64];
        for (i, b) in log.iter().enumerate() {
            journal[1 + i] = b.clone();
        }
        let mut s2 = s.clone();
        s2.start = 1;
        let r = plan_replay(&s2, |i| journal.get(i as usize).cloned());
        assert_eq!(r.transactions, 1);
        assert_eq!(r.next_sequence, 8);
        assert_eq!(
            r.writes.iter().map(|w| w.target).collect::<Vec<_>>(),
            [100, 200]
        );
        let mut d = journal[r.writes[1].log_block as usize].clone();
        unescape(&mut d, &r.writes[1]);
        assert_eq!(d, magic_block);
        // Without its commit block the transaction is ignored.
        journal[4] = vec![0; 1024];
        assert_eq!(
            plan_replay(&s2, |i| journal.get(i as usize).cloned())
                .writes
                .len(),
            0
        );
    }

    #[test]
    fn transactions_round_trip() {
        round_trip(0);
        round_trip(INCOMPAT_64BIT);
        round_trip(INCOMPAT_CSUM_V3 | INCOMPAT_64BIT);
        round_trip(INCOMPAT_CSUM_V2);
    }

    #[test]
    fn encode_superblock() {
        let s = sb(INCOMPAT_CSUM_V3);
        let b = s.encode(1, 9);
        let p = Super::parse(&b).unwrap();
        assert_eq!((p.start, p.sequence), (1, 9));
        let mut z = b.clone();
        put_be32(&mut z, 0xFC, 0);
        assert_eq!(crc32c(!0, &z), be32(&b, 0xFC));
    }
}
