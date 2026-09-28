//! SHA-512 based `crypt(3)` (`$6$`), as used in `/etc/shadow`
//! (Ulrich Drepper's "Unix crypt using SHA-256 and SHA-512").

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use sha2::{Digest, Sha512};

const ITOA64: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
pub const DEFAULT_ROUNDS: u32 = 5000;
const MIN_ROUNDS: u32 = 1000;
const MAX_ROUNDS: u32 = 999_999_999;

fn b64(out: &mut String, b2: u8, b1: u8, b0: u8, n: usize) {
    let mut w = ((b2 as u32) << 16) | ((b1 as u32) << 8) | b0 as u32;
    for _ in 0..n {
        out.push(ITOA64[(w & 0x3F) as usize] as char);
        w >>= 6;
    }
}

/// Hash `password` with `salt` (at most 16 characters are used).
pub fn sha512_crypt(password: &[u8], salt: &str, rounds: Option<u32>) -> String {
    let salt = &salt.as_bytes()[..salt.len().min(16)];
    let custom = rounds.is_some();
    let rounds = rounds
        .unwrap_or(DEFAULT_ROUNDS)
        .clamp(MIN_ROUNDS, MAX_ROUNDS);
    let pl = password.len();

    let b = Sha512::new()
        .chain_update(password)
        .chain_update(salt)
        .chain_update(password)
        .finalize();
    let mut a = Sha512::new().chain_update(password).chain_update(salt);
    let mut cnt = pl;
    while cnt > 64 {
        a.update(b);
        cnt -= 64;
    }
    a.update(&b[..cnt]);
    let mut i = pl;
    while i > 0 {
        if i & 1 != 0 {
            a.update(b);
        } else {
            a.update(password);
        }
        i >>= 1;
    }
    let a = a.finalize();

    let mut dp = Sha512::new();
    for _ in 0..pl {
        dp.update(password);
    }
    let dp = dp.finalize();
    let p: Vec<u8> = dp.iter().cycle().take(pl).copied().collect();

    let mut ds = Sha512::new();
    for _ in 0..16 + a[0] as usize {
        ds.update(salt);
    }
    let ds = ds.finalize();
    let s: Vec<u8> = ds.iter().cycle().take(salt.len()).copied().collect();

    let mut c = a;
    for r in 0..rounds {
        let mut h = Sha512::new();
        if r & 1 != 0 {
            h.update(&p);
        } else {
            h.update(c);
        }
        if r % 3 != 0 {
            h.update(&s);
        }
        if r % 7 != 0 {
            h.update(&p);
        }
        if r & 1 != 0 {
            h.update(c);
        } else {
            h.update(&p);
        }
        c = h.finalize();
    }

    let mut out = String::from("$6$");
    if custom {
        out.push_str(&format!("rounds={}$", rounds));
    }
    out.push_str(core::str::from_utf8(salt).unwrap_or(""));
    out.push('$');
    const ORDER: [(usize, usize, usize); 21] = [
        (0, 21, 42),
        (22, 43, 1),
        (44, 2, 23),
        (3, 24, 45),
        (25, 46, 4),
        (47, 5, 26),
        (6, 27, 48),
        (28, 49, 7),
        (50, 8, 29),
        (9, 30, 51),
        (31, 52, 10),
        (53, 11, 32),
        (12, 33, 54),
        (34, 55, 13),
        (56, 14, 35),
        (15, 36, 57),
        (37, 58, 16),
        (59, 17, 38),
        (18, 39, 60),
        (40, 61, 19),
        (62, 20, 41),
    ];
    for (x, y, z) in ORDER {
        b64(&mut out, c[x], c[y], c[z], 4);
    }
    b64(&mut out, 0, 0, c[63], 2);
    out
}

/// Check `password` against a `$6$` hash from /etc/shadow.
pub fn verify(password: &[u8], hash: &str) -> bool {
    let Some(rest) = hash.strip_prefix("$6$") else {
        return false;
    };
    let (rounds, rest) = match rest.strip_prefix("rounds=") {
        Some(r) => {
            let Some((n, rest)) = r.split_once('$') else {
                return false;
            };
            let Ok(n) = n.parse::<u32>() else {
                return false;
            };
            (Some(n), rest)
        }
        None => (None, rest),
    };
    let Some((salt, _)) = rest.split_once('$') else {
        return false;
    };
    let computed = sha512_crypt(password, salt, rounds);
    // Constant-time comparison.
    computed.len() == hash.len()
        && computed
            .bytes()
            .zip(hash.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// A salt of 16 characters from `random` bytes.
pub fn make_salt(random: &[u8; 16]) -> String {
    random
        .iter()
        .map(|b| ITOA64[(*b & 0x3F) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vectors from the specification (the third checked against
    // `openssl passwd -6`).
    #[test]
    fn spec_vectors() {
        assert_eq!(
            sha512_crypt(b"Hello world!", "saltstring", None),
            "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"
        );
        assert_eq!(
            sha512_crypt(b"Hello world!", "saltstringsaltstring", Some(10000)),
            "$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0sbHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v."
        );
        assert_eq!(
            sha512_crypt(
                b"we have a short salt string but not a short password",
                "roundstoolow",
                Some(10)
            ),
            "$6$rounds=1000$roundstoolow$yjTuW7RnC.d35QcVTFIb6uvh/7IQ1.GFtFN3i/.jwmeWEhzjf4uD/OPCb4jRl6atJGYhLst8IyR6YAtTrriMU1"
        );
    }

    #[test]
    fn verify_round_trip() {
        let h = sha512_crypt(b"secret", &make_salt(&[7; 16]), None);
        assert!(verify(b"secret", &h));
        assert!(!verify(b"Secret", &h));
        assert!(!verify(b"secret", "$1$abc$def"));
    }
}
