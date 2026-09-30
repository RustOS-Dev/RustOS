//! Case-insensitive names for ext4 `casefold` directories: the
//! canonical decomposition (NFD) of the full Unicode case folding, as the
//! kernel's utf8 "nfdicf" tables define it. Both lookups (compare the
//! folded forms) and directory hashes (hash the folded form) use it.

use crate::casefold_data::{CCC, MAP_DATA, MAP_INDEX};
use alloc::string::String;
use alloc::vec::Vec;

/// Superblock `s_encoding` value for UTF-8 (12.1).
pub const ENCODING_UTF8: u16 = 1;
/// `s_encoding_flags`: reject invalid names instead of treating them as
/// opaque bytes.
pub const ENCODING_STRICT: u16 = 1;

fn combining_class(c: char) -> u8 {
    CCC.binary_search_by_key(&(c as u32), |e| e.0)
        .map_or(0, |i| CCC[i].1)
}

/// Push the folded, decomposed form of `c`.
fn fold_char(c: char, out: &mut Vec<char>) {
    const S_BASE: u32 = 0xAC00;
    const L_BASE: u32 = 0x1100;
    const V_BASE: u32 = 0x1161;
    const T_BASE: u32 = 0x11A7;
    const T_COUNT: u32 = 28;
    const N_COUNT: u32 = 588;
    let cp = c as u32;
    if (S_BASE..S_BASE + 11172).contains(&cp) {
        let s = cp - S_BASE;
        let l = L_BASE + s / N_COUNT;
        let v = V_BASE + (s % N_COUNT) / T_COUNT;
        let t = T_BASE + s % T_COUNT;
        out.extend([l, v].into_iter().filter_map(char::from_u32));
        if t != T_BASE {
            out.extend(char::from_u32(t));
        }
        return;
    }
    match MAP_INDEX.binary_search_by_key(&cp, |e| e.0) {
        Ok(i) => {
            let (_, off, len) = MAP_INDEX[i];
            let off = off as usize;
            out.extend(
                MAP_DATA[off..off + len as usize]
                    .iter()
                    .filter_map(|&x| char::from_u32(x)),
            );
        }
        Err(_) => out.push(c),
    }
}

/// The folded form of `name`, or `None` if it is not valid UTF-8.
pub fn casefold(name: &[u8]) -> Option<Vec<u8>> {
    let s = core::str::from_utf8(name).ok()?;
    let mut v: Vec<char> = Vec::with_capacity(s.len());
    for c in s.chars() {
        fold_char(c, &mut v);
    }
    // Canonical ordering: sort each run of combining marks by class
    // (stable, so equal classes keep their order).
    let mut i = 0;
    while i < v.len() {
        if combining_class(v[i]) == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < v.len() && combining_class(v[i]) != 0 {
            i += 1;
        }
        v[start..i].sort_by_key(|&c| combining_class(c));
    }
    Some(v.into_iter().collect::<String>().into_bytes())
}

/// Whether two names match in a casefolded directory. Names that are
/// not valid UTF-8 only match byte for byte.
pub fn names_equal(a: &[u8], b: &[u8]) -> bool {
    if a == b {
        return true;
    }
    match (casefold(a), casefold(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> String {
        String::from_utf8(casefold(s.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn folding() {
        assert_eq!(f("Hello.TXT"), "hello.txt");
        assert_eq!(f("Straße"), "strasse");
        // Precomposed and decomposed forms fold the same.
        assert_eq!(f("\u{C9}t\u{E9}"), "e\u{301}te\u{301}");
        assert!(names_equal("CAFÉ".as_bytes(), "cafe\u{301}".as_bytes()));
        assert_eq!(f("ΣΊΣΥΦΟΣ"), "σι\u{301}συφοσ");
        // Hangul syllables decompose to jamo.
        assert_eq!(f("\u{D55C}"), "\u{1112}\u{1161}\u{11AB}");
        // Canonical ordering of combining marks (dot below 220 before
        // acute 230).
        assert_eq!(f("a\u{301}\u{323}"), "a\u{323}\u{301}");
        assert!(casefold(b"\xFF\xFE").is_none());
        assert!(names_equal(b"\xFF", b"\xFF"));
        assert!(!names_equal(b"\xFF", b"\xFE"));
    }
}
