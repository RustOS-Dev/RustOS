//! Directory index (htree) name hashes: legacy, half-MD4 and TEA, signed
//! and unsigned variants (fs/ext4/hash.c).

pub const LEGACY: u8 = 0;
pub const HALF_MD4: u8 = 1;
pub const TEA: u8 = 2;
pub const LEGACY_UNSIGNED: u8 = 3;
pub const HALF_MD4_UNSIGNED: u8 = 4;
pub const TEA_UNSIGNED: u8 = 5;

/// The end-of-directory hash value, never used for names.
const EOF_32BIT: u32 = 0x7FFF_FFFF;

fn ch(c: u8, unsigned: bool) -> u32 {
    if unsigned {
        c as u32
    } else {
        c as i8 as i32 as u32
    }
}

fn legacy(name: &[u8], unsigned: bool) -> u32 {
    let (mut h0, mut h1) = (0x12A3_FE2Du32, 0x37AB_E8F9u32);
    for &c in name {
        let mut h = h1.wrapping_add(h0 ^ ch(c, unsigned).wrapping_mul(7_152_373));
        if h & 0x8000_0000 != 0 {
            h = h.wrapping_sub(0x7FFF_FFFF);
        }
        h1 = h0;
        h0 = h;
    }
    h0 << 1
}

fn str2hashbuf(msg: &[u8], out: &mut [u32], unsigned: bool) {
    let num = out.len();
    let len = msg.len() as u32;
    let mut pad = len | (len << 8);
    pad |= pad << 16;
    let mut val = pad;
    let n = msg.len().min(num * 4);
    let mut k = 0;
    for (i, &c) in msg[..n].iter().enumerate() {
        val = ch(c, unsigned).wrapping_add(val << 8);
        if i % 4 == 3 {
            out[k] = val;
            k += 1;
            val = pad;
        }
    }
    if k < num {
        out[k] = val;
        k += 1;
    }
    while k < num {
        out[k] = pad;
        k += 1;
    }
}

fn tea_transform(buf: &mut [u32; 4], inp: &[u32; 4]) {
    let mut sum = 0u32;
    let (mut b0, mut b1) = (buf[0], buf[1]);
    let (a, b, c, d) = (inp[0], inp[1], inp[2], inp[3]);
    for _ in 0..16 {
        sum = sum.wrapping_add(0x9E37_79B9);
        b0 = b0.wrapping_add(
            ((b1 << 4).wrapping_add(a)) ^ b1.wrapping_add(sum) ^ ((b1 >> 5).wrapping_add(b)),
        );
        b1 = b1.wrapping_add(
            ((b0 << 4).wrapping_add(c)) ^ b0.wrapping_add(sum) ^ ((b0 >> 5).wrapping_add(d)),
        );
    }
    buf[0] = buf[0].wrapping_add(b0);
    buf[1] = buf[1].wrapping_add(b1);
}

fn half_md4_transform(buf: &mut [u32; 4], x: &[u32; 8]) {
    let f = |x: u32, y: u32, z: u32| z ^ (x & (y ^ z));
    let g = |x: u32, y: u32, z: u32| (x & y).wrapping_add((x ^ y) & z);
    let h = |x: u32, y: u32, z: u32| x ^ y ^ z;
    const K2: u32 = 0o13240474631;
    const K3: u32 = 0o15666365641;
    let (mut a, mut b, mut c, mut d) = (buf[0], buf[1], buf[2], buf[3]);
    macro_rules! r {
        ($f:ident, $a:ident, $b:ident, $c:ident, $d:ident, $x:expr, $s:expr) => {
            $a = $a
                .wrapping_add($f($b, $c, $d))
                .wrapping_add($x)
                .rotate_left($s);
        };
    }
    r!(f, a, b, c, d, x[0], 3);
    r!(f, d, a, b, c, x[1], 7);
    r!(f, c, d, a, b, x[2], 11);
    r!(f, b, c, d, a, x[3], 19);
    r!(f, a, b, c, d, x[4], 3);
    r!(f, d, a, b, c, x[5], 7);
    r!(f, c, d, a, b, x[6], 11);
    r!(f, b, c, d, a, x[7], 19);
    r!(g, a, b, c, d, x[1].wrapping_add(K2), 3);
    r!(g, d, a, b, c, x[3].wrapping_add(K2), 5);
    r!(g, c, d, a, b, x[5].wrapping_add(K2), 9);
    r!(g, b, c, d, a, x[7].wrapping_add(K2), 13);
    r!(g, a, b, c, d, x[0].wrapping_add(K2), 3);
    r!(g, d, a, b, c, x[2].wrapping_add(K2), 5);
    r!(g, c, d, a, b, x[4].wrapping_add(K2), 9);
    r!(g, b, c, d, a, x[6].wrapping_add(K2), 13);
    r!(h, a, b, c, d, x[3].wrapping_add(K3), 3);
    r!(h, d, a, b, c, x[7].wrapping_add(K3), 9);
    r!(h, c, d, a, b, x[2].wrapping_add(K3), 11);
    r!(h, b, c, d, a, x[6].wrapping_add(K3), 15);
    r!(h, a, b, c, d, x[1].wrapping_add(K3), 3);
    r!(h, d, a, b, c, x[5].wrapping_add(K3), 9);
    r!(h, c, d, a, b, x[0].wrapping_add(K3), 11);
    r!(h, b, c, d, a, x[4].wrapping_add(K3), 15);
    buf[0] = buf[0].wrapping_add(a);
    buf[1] = buf[1].wrapping_add(b);
    buf[2] = buf[2].wrapping_add(c);
    buf[3] = buf[3].wrapping_add(d);
}

/// (hash, minor hash) of `name` for hash `version` with the filesystem's
/// `s_hash_seed` (all zero: the default seed).
pub fn dx_hash(name: &[u8], version: u8, seed: [u32; 4]) -> (u32, u32) {
    let mut buf = if seed == [0; 4] {
        [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476]
    } else {
        seed
    };
    let unsigned = matches!(version, LEGACY_UNSIGNED | HALF_MD4_UNSIGNED | TEA_UNSIGNED);
    let (hash, minor) = match version {
        HALF_MD4 | HALF_MD4_UNSIGNED => {
            let mut p = name;
            loop {
                let mut inp = [0u32; 8];
                str2hashbuf(p, &mut inp, unsigned);
                half_md4_transform(&mut buf, &inp);
                if p.len() <= 32 {
                    break;
                }
                p = &p[32..];
            }
            (buf[1], buf[2])
        }
        TEA | TEA_UNSIGNED => {
            let mut p = name;
            loop {
                let mut inp = [0u32; 4];
                str2hashbuf(p, &mut inp, unsigned);
                tea_transform(&mut buf, &inp);
                if p.len() <= 16 {
                    break;
                }
                p = &p[16..];
            }
            (buf[0], buf[1])
        }
        _ => (legacy(name, unsigned), 0),
    };
    let mut hash = hash & !1;
    if hash == EOF_32BIT << 1 {
        hash = (EOF_32BIT - 1) << 1;
    }
    (hash, minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: [u32; 4] = [0xA12A_392A, 0xFA4F_EAC6, 0x0447_0891, 0xF928_4707];

    // From `debugfs -R "dx_hash -h ALG -s SEED NAME"` (e2fsprogs 1.47).
    #[test]
    fn debugfs_vectors() {
        let cases: &[(u8, &str, u32, u32, u32, u32)] = &[
            (LEGACY, "hello", 0x32252546, 0, 0x32252546, 0),
            (LEGACY, "lost+found", 0x5e2aba24, 0, 0x5e2aba24, 0),
            (LEGACY, "Ünïcode", 0xf0364b18, 0, 0xf0364b18, 0),
            (
                HALF_MD4, "hello", 0x1746da32, 0x420013b5, 0x8f6d4bc8, 0xfcdd28cf,
            ),
            (
                HALF_MD4, "a", 0xd5fa7d7a, 0xacb48187, 0x3add2bbc, 0x69198da7,
            ),
            (
                HALF_MD4,
                "lost+found",
                0x591de422,
                0x6ffc56e0,
                0x04f6a618,
                0x28c684f1,
            ),
            (
                HALF_MD4,
                "file_with_a_longer_name_0123456789abcdef",
                0xa0f7c4fc,
                0x01425578,
                0x05308b4a,
                0xbd844611,
            ),
            (
                HALF_MD4,
                "Ünïcode",
                0xf634dd7e,
                0x29fc0b6d,
                0x0bb5faee,
                0x1a3043b0,
            ),
            (TEA, "hello", 0x6f5bb1a8, 0x231917c2, 0x27fd732a, 0xece36f4e),
            (TEA, "a", 0x6d0ea4c0, 0xc18922df, 0x76bff9c4, 0xa7b2d4d9),
            (
                TEA,
                "file_with_a_longer_name_0123456789abcdef",
                0xd32615bc,
                0xb90884e3,
                0x52c07ae8,
                0x26053bf2,
            ),
            (
                TEA,
                "Ünïcode",
                0x10ea1a1e,
                0xaf9ffb30,
                0xf12c5f8c,
                0xebfc8642,
            ),
        ];
        for &(v, name, h0, m0, h1, m1) in cases {
            assert_eq!(
                dx_hash(name.as_bytes(), v, [0; 4]),
                (h0, m0),
                "{} zero seed v{}",
                name,
                v
            );
            assert_eq!(
                dx_hash(name.as_bytes(), v, SEED),
                (h1, m1),
                "{} seed v{}",
                name,
                v
            );
        }
    }
}
