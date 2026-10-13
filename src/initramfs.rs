//! The embedded initial RAM filesystem (a cpio "newc" archive built from
//! `userland/` by build.rs), unpacked into the root tmpfs at boot.

use crate::vfs;

// The archive is assembled in with .incbin rather than include_bytes!: rustc
// turns include_bytes! into a constant it holds several copies of, and a few
// hundred MiB of ports took more than 13 GiB to compile. rustc does not see
// the .incbin input, so the crate depends on build.rs's stamp of the archive
// (its size and hash) instead, and the stamp's hash is part of the assembly:
// an incremental build would otherwise keep the object holding the old
// archive, the assembly text being unchanged.
const ARCHIVE_STAMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/initramfs.stamp"));
const ARCHIVE_ID: u32 = {
    let mut h: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < ARCHIVE_STAMP.len() {
        h = (h ^ ARCHIVE_STAMP[i] as u32).wrapping_mul(0x0100_0193);
        i += 1;
    }
    h
};
core::arch::global_asm!(
    ".section .rodata.initramfs,\"a\"",
    ".balign 4096",
    ".set rustos_initramfs_id, {id}",
    ".global rustos_initramfs_start",
    "rustos_initramfs_start:",
    concat!(".incbin \"", env!("OUT_DIR"), "/initramfs.cpio\""),
    ".global rustos_initramfs_end",
    "rustos_initramfs_end:",
    ".previous",
    id = const ARCHIVE_ID,
);

unsafe extern "C" {
    static rustos_initramfs_start: u8;
    static rustos_initramfs_end: u8;
}

fn archive() -> &'static [u8] {
    let start = &raw const rustos_initramfs_start;
    let end = &raw const rustos_initramfs_end;
    // SAFETY: the two symbols bracket the archive in read-only data.
    unsafe { core::slice::from_raw_parts(start, end as usize - start as usize) }
}

fn hex(b: &[u8]) -> Option<u32> {
    u32::from_str_radix(core::str::from_utf8(b).ok()?, 16).ok()
}

/// Unpack the archive into the VFS root. Returns the number of entries.
pub fn unpack() -> usize {
    unpack_archive(archive())
}

/// Regular files keep pointing into `data` (tmpfs copies one only when it
/// is written), so the archive costs no kernel heap.
pub fn unpack_archive(data: &'static [u8]) -> usize {
    let mut off = 0usize;
    let mut count = 0;
    while off + 110 <= data.len() {
        let h = &data[off..off + 110];
        if &h[..6] != b"070701" {
            break;
        }
        let field = |i: usize| hex(&h[6 + i * 8..6 + (i + 1) * 8]).unwrap_or(0);
        let mode = field(1);
        let filesize = field(6) as usize;
        let namesize = field(11) as usize;
        let name_start = off + 110;
        let name_end = name_start + namesize.saturating_sub(1);
        if name_end > data.len() {
            break;
        }
        let name = core::str::from_utf8(&data[name_start..name_end]).unwrap_or("");
        let data_start = (name_start + namesize).next_multiple_of(4);
        let data_end = data_start + filesize;
        if data_end > data.len() {
            break;
        }
        if name == "TRAILER!!!" {
            break;
        }
        let path = alloc::format!("/{}", name.trim_start_matches("./").trim_start_matches('/'));
        let body = &data[data_start..data_end];
        match mode & 0o170000 {
            0o040000 => {
                let _ = vfs::mkdir_p(&path);
            }
            0o120000 => {
                if let Ok(target) = core::str::from_utf8(body) {
                    let _ = vfs::unlink(&path);
                    let _ = vfs::symlink(target, &path);
                }
            }
            0o100000 => {
                if let Some(parent) = path.rfind('/').map(|i| &path[..i])
                    && !parent.is_empty()
                {
                    let _ = vfs::mkdir_p(parent);
                }
                if vfs::write_all(&path, &[]).is_ok()
                    && let Ok(i) = vfs::lookup(&path)
                {
                    match i.as_any().downcast_ref::<vfs::tmpfs::TmpInode>() {
                        Some(t) => t.set_static(body),
                        None => {
                            let _ = vfs::write_all(&path, body);
                        }
                    }
                    let _ = i.chmod(mode & 0o7777);
                }
            }
            _ => {}
        }
        count += 1;
        off = data_end.next_multiple_of(4);
    }
    count
}
