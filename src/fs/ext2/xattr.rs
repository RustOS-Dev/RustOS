//! Extended attributes (read side): in-inode attributes, the shared
//! attribute block, and values stored in their own inodes (ea_inode).
//! Deleting a file drops its references to such value inodes.

use super::inline::{IbodyAttr, parse_ibody};
use super::*;

const BLOCK_MAGIC: u32 = 0xEA02_0000;
/// Inode flag of an inode holding an attribute value.
const FL_EA_INODE: u32 = 0x20_0000;

fn prefix(index: u8) -> Option<&'static str> {
    Some(match index {
        1 => "user.",
        2 => "system.posix_acl_access",
        3 => "system.posix_acl_default",
        4 => "trusted.",
        6 => "security.",
        8 => "system.richacl",
        _ => return None, // 7 = system.data (inline data) stays hidden
    })
}

fn full_name(a: &IbodyAttr) -> Option<String> {
    let p = prefix(a.index)?;
    Some(alloc::format!("{}{}", p, String::from_utf8_lossy(&a.name)))
}

/// Attributes in an attribute block (entries from byte 32, value
/// offsets from the start of the block).
fn parse_block(b: &[u8]) -> Vec<IbodyAttr> {
    let mut out = Vec::new();
    if b.len() < 32 || u32le(b, 0) != BLOCK_MAGIC {
        return out;
    }
    let mut o = 32;
    while o + 16 <= b.len() && u32le(b, o) != 0 {
        let nl = b[o] as usize;
        let voff = u16le(b, o + 2) as usize;
        let value_inum = u32le(b, o + 4);
        let vsize = u32le(b, o + 8) as usize;
        if o + 16 + nl > b.len() {
            break;
        }
        let value = if value_inum == 0 {
            match b.get(voff..voff + vsize) {
                Some(v) => v.to_vec(),
                None => break,
            }
        } else {
            Vec::new()
        };
        out.push(IbodyAttr {
            index: b[o + 1],
            name: b[o + 16..o + 16 + nl].to_vec(),
            value,
            value_inum,
            value_size: vsize as u32,
            hash: u32le(b, o + 12),
        });
        o += (16 + nl).next_multiple_of(4);
    }
    out
}

impl Ext2Inode {
    fn all_xattrs(&self) -> KResult<Vec<IbodyAttr>> {
        let (raw, xb) = {
            let st = self.st.lock();
            (st.raw.clone(), st.xattr_block())
        };
        let mut v = parse_ibody(&raw);
        if xb != 0 && xb < self.fs.blocks_count {
            let mut b = vec![0u8; self.fs.block_size as usize];
            self.fs.read_block(xb, &mut b)?;
            v.extend(parse_block(&b));
        }
        Ok(v)
    }

    /// The value of an attribute, reading ea_inode values from their inode.
    fn xattr_value(&self, a: &IbodyAttr) -> KResult<Vec<u8>> {
        if a.value_inum == 0 {
            return Ok(a.value.clone());
        }
        let ea = self.fs.inode(a.value_inum)?;
        let mut st = ea.st.lock();
        if st.flags & FL_EA_INODE == 0 {
            return Err(EIO);
        }
        let mut v = vec![0u8; a.value_size as usize];
        let n = ea.read_data(&mut st, 0, &mut v, false)?;
        v.truncate(n);
        Ok(v)
    }

    pub(super) fn xattr_get(&self, name: &str) -> KResult<Vec<u8>> {
        for a in self.all_xattrs()? {
            if full_name(&a).as_deref() == Some(name) {
                return self.xattr_value(&a);
            }
        }
        Err(ENODATA)
    }

    pub(super) fn xattr_list(&self) -> KResult<Vec<String>> {
        Ok(self.all_xattrs()?.iter().filter_map(full_name).collect())
    }

    /// Drop one reference to each value inode in `attrs` (the count is
    /// kept in the value inode's ctime and i_version); a value inode
    /// whose count reaches zero is deleted.
    pub(super) fn put_ea_inodes(&self, attrs: &[IbodyAttr]) -> KResult<()> {
        for a in attrs.iter().filter(|a| a.value_inum != 0) {
            let ea = self.fs.inode(a.value_inum)?;
            let last = {
                let mut st = ea.st.lock();
                if st.flags & FL_EA_INODE == 0 {
                    continue;
                }
                let r = ((st.ctime as u64) << 32) | u32le(&st.raw, 0x24) as u64;
                let r = r.saturating_sub(1);
                st.ctime = (r >> 32) as u32;
                put32(&mut st.raw, 0x24, r as u32);
                ea.save(&st)?;
                r == 0
            };
            if last {
                ea.change_links(-1)?;
            }
        }
        Ok(())
    }

    /// Value inodes referenced from the inode body.
    pub(super) fn ibody_ea_refs(st: &RawInode) -> Vec<IbodyAttr> {
        parse_ibody(&st.raw)
            .into_iter()
            .filter(|a| a.value_inum != 0)
            .collect()
    }

    /// Value inodes referenced from attribute block contents `b`.
    pub(super) fn block_ea_refs(b: &[u8]) -> Vec<IbodyAttr> {
        parse_block(b)
            .into_iter()
            .filter(|a| a.value_inum != 0)
            .collect()
    }
}
