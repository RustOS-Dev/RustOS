//! Firmware file lookup for drivers (e.g. iwlwifi).
//!
//! Searches the initramfs (/lib/firmware), the persistent storage
//! partition and the EFI system partition, like Linux's firmware loader.
//! A directory's `.links` file stands in for linux-firmware's symlinks,
//! which FAT32 cannot hold: each line is `NAME TARGET` (relative to the
//! firmware directory), NAME being a file or a directory prefix.

use crate::errno::*;
use alloc::format;
use alloc::vec::Vec;

pub const SEARCH_PATHS: &[&str] = &[
    "/lib/firmware",
    "/storage/lib/firmware",
    "/boot/efi/firmware",
    "/boot/efi/EFI/rustos/firmware",
];

/// The path of firmware `name` in the first firmware directory that has
/// it (following `.links`).
pub fn find(name: &str) -> Option<alloc::string::String> {
    for dir in SEARCH_PATHS {
        let mut name = alloc::string::String::from(name);
        let mut links = None;
        // Follow up to a few links (gb205/... -> gb202/... -> ga102/...).
        for _ in 0..4 {
            let path = format!("{}/{}", dir, name);
            if crate::vfs::lookup(&path).is_ok() {
                return Some(path);
            }
            let table = links.get_or_insert_with(|| {
                crate::vfs::read_all(&format!("{}/.links", dir)).unwrap_or_default()
            });
            match follow_link(table, &name) {
                Some(next) => name = next,
                None => break,
            }
        }
    }
    None
}

/// Load `name` from the first firmware directory that has it.
pub fn load(name: &str) -> KResult<Vec<u8>> {
    let path = find(name).ok_or(ENOENT)?;
    let data = crate::vfs::read_all(&path)?;
    crate::println!("[firmware] loaded {} ({} bytes)", path, data.len());
    Ok(data)
}

/// `name` with the first matching link in `table` (a `.links` file)
/// applied, if one matches.
fn follow_link(table: &[u8], name: &str) -> Option<alloc::string::String> {
    let text = core::str::from_utf8(table).ok()?;
    text.lines().find_map(|line| {
        let (link, target) = line.split_once(' ')?;
        let target = target.trim();
        if name == link {
            Some(target.into())
        } else {
            let rest = name.strip_prefix(link)?.strip_prefix('/')?;
            Some(format!("{}/{}", target, rest))
        }
    })
}

/// Why this image has no stock firmware (firmware/stock.list), if it was
/// built without it: then a missing file is the build's fault, not the
/// user's.
pub fn stock_missing() -> Option<alloc::string::String> {
    crate::vfs::read_all("/lib/firmware/.stock-missing")
        .ok()
        .map(|d| alloc::string::String::from_utf8_lossy(&d).trim().into())
}

/// What to tell the user when `name` (a stock firmware file) is missing.
pub fn missing_hint(name: &str) -> alloc::string::String {
    match stock_missing() {
        Some(why) => format!(
            "this image was built without its firmware ({}); rebuild it with network access, or copy {} into /storage/lib/firmware",
            why, name
        ),
        None => format!(
            "{} is missing from /lib/firmware (it should ship in the image: please report this); copy it into /storage/lib/firmware",
            name
        ),
    }
}

/// Load the first available file among `names` (newest API first).
pub fn load_any(names: &[&str]) -> KResult<(usize, Vec<u8>)> {
    for (i, n) in names.iter().enumerate() {
        if let Ok(d) = load(n) {
            return Ok((i, d));
        }
    }
    Err(ENOENT)
}
