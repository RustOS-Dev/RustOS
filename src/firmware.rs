//! Firmware file lookup for drivers (e.g. iwlwifi).
//!
//! Searches the initramfs (/lib/firmware), the persistent storage
//! partition and the EFI system partition, like Linux's firmware loader.

use crate::errno::*;
use alloc::format;
use alloc::vec::Vec;

pub const SEARCH_PATHS: &[&str] = &[
    "/lib/firmware",
    "/storage/lib/firmware",
    "/boot/efi/firmware",
    "/boot/efi/EFI/rustos/firmware",
];

/// Load `name` from the first firmware directory that has it.
pub fn load(name: &str) -> KResult<Vec<u8>> {
    for dir in SEARCH_PATHS {
        let path = format!("{}/{}", dir, name);
        if let Ok(data) = crate::vfs::read_all(&path) {
            crate::println!("[firmware] loaded {} ({} bytes)", path, data.len());
            return Ok(data);
        }
    }
    Err(ENOENT)
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
