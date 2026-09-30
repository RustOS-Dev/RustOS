//! The Security Manager's cryptographic toolbox (Core Vol 3 Part H 2.2):
//! the AES function e, AES-CMAC, f4/f5/f6/g2 for LE Secure Connections,
//! c1/s1 for legacy pairing, ah for resolvable private addresses, and
//! P-256 key agreement.
//!
//! Arguments and results are in wire order (least significant byte
//! first); the specification's functions are defined on the reversed,
//! most-significant-first values, which is what the helpers feed AES.

use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use alloc::vec::Vec;
use cmac::{Cmac, Mac};
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{EncodedPoint, PublicKey, SecretKey};

fn rev<const N: usize>(a: &[u8; N]) -> [u8; N] {
    let mut r = *a;
    r.reverse();
    r
}

/// e(key, plaintext): AES-128 (wire-order in and out).
pub fn e(key: &[u8; 16], pt: &[u8; 16]) -> [u8; 16] {
    let c = Aes128::new(GenericArray::from_slice(&rev(key)));
    let mut b = GenericArray::clone_from_slice(&rev(pt));
    c.encrypt_block(&mut b);
    let mut out: [u8; 16] = b.into();
    out.reverse();
    out
}

/// AES-CMAC over the spec-order concatenation of `parts` (each given in
/// wire order); key and result in wire order.
fn cmac_parts(key: &[u8; 16], parts: &[&[u8]]) -> [u8; 16] {
    let mut m = <Cmac<Aes128> as Mac>::new(GenericArray::from_slice(&rev(key)));
    for p in parts {
        let mut v: Vec<u8> = p.to_vec();
        v.reverse();
        m.update(&v);
    }
    let mut out: [u8; 16] = m.finalize().into_bytes().into();
    out.reverse();
    out
}

/// AES-CMAC with key and message as byte strings (RFC 4493 order).
pub fn aes_cmac(key: &[u8; 16], msg: &[u8]) -> [u8; 16] {
    let mut m = <Cmac<Aes128> as Mac>::new(GenericArray::from_slice(key));
    m.update(msg);
    m.finalize().into_bytes().into()
}

/// An LE address as the 56-bit value f5/f6 take: address, then its type
/// in the most significant byte.
pub fn addr56(a: &crate::Addr, t: crate::AddrType) -> [u8; 7] {
    let mut r = [0u8; 7];
    r[..6].copy_from_slice(&a.0);
    r[6] = t as u8;
    r
}

/// f4: confirm values for LE Secure Connections.
pub fn f4(u: &[u8; 32], v: &[u8; 32], x: &[u8; 16], z: u8) -> [u8; 16] {
    cmac_parts(x, &[u, v, &[z]])
}

/// f5: (MacKey, LTK) from the DHKey.
pub fn f5(
    w: &[u8; 32],
    n1: &[u8; 16],
    n2: &[u8; 16],
    a1: &[u8; 7],
    a2: &[u8; 7],
) -> ([u8; 16], [u8; 16]) {
    let salt = rev(&[
        0x6C, 0x88, 0x83, 0x91, 0xAA, 0xF5, 0xA5, 0x38, 0x60, 0x37, 0x0B, 0xDB, 0x5A, 0x60, 0x83,
        0xBE,
    ]);
    let t = cmac_parts(&salt, &[w]);
    let key_id = 0x6274_6c65u32.to_le_bytes();
    let len = 256u16.to_le_bytes();
    let k = |ctr: u8| cmac_parts(&t, &[&[ctr], &key_id, n1, n2, a1, a2, &len]);
    (k(0), k(1))
}

/// f6: DHKey check values.
pub fn f6(
    w: &[u8; 16],
    n1: &[u8; 16],
    n2: &[u8; 16],
    r: &[u8; 16],
    io_cap: &[u8; 3],
    a1: &[u8; 7],
    a2: &[u8; 7],
) -> [u8; 16] {
    cmac_parts(w, &[n1, n2, r, io_cap, a1, a2])
}

/// g2: the six-digit value for numeric comparison.
pub fn g2(u: &[u8; 32], v: &[u8; 32], x: &[u8; 16], y: &[u8; 16]) -> u32 {
    let r = cmac_parts(x, &[u, v, y]);
    u32::from_le_bytes([r[0], r[1], r[2], r[3]]) % 1_000_000
}

/// ah: the hash part of a resolvable private address.
pub fn ah(irk: &[u8; 16], prand: &[u8; 3]) -> [u8; 3] {
    let mut p = [0u8; 16];
    p[..3].copy_from_slice(prand);
    let r = e(irk, &p);
    [r[0], r[1], r[2]]
}

/// Whether `a` (a resolvable private address) was made with `irk`.
pub fn rpa_matches(irk: &[u8; 16], a: &crate::Addr) -> bool {
    a.is_rpa() && ah(irk, &[a.0[3], a.0[4], a.0[5]]) == [a.0[0], a.0[1], a.0[2]]
}

/// c1: legacy pairing confirm value. `preq`/`pres` are the 7-byte pairing
/// request and response PDUs as sent.
#[allow(clippy::too_many_arguments)]
pub fn c1(
    k: &[u8; 16],
    r: &[u8; 16],
    preq: &[u8; 7],
    pres: &[u8; 7],
    iat: u8,
    rat: u8,
    ia: &crate::Addr,
    ra: &crate::Addr,
) -> [u8; 16] {
    // p1 = pres || preq || rat' || iat' (most significant first).
    let mut p1 = [0u8; 16];
    p1[0] = iat;
    p1[1] = rat;
    p1[2..9].copy_from_slice(preq);
    p1[9..16].copy_from_slice(pres);
    // p2 = padding || ia || ra.
    let mut p2 = [0u8; 16];
    p2[..6].copy_from_slice(&ra.0);
    p2[6..12].copy_from_slice(&ia.0);
    let mut x = [0u8; 16];
    for i in 0..16 {
        x[i] = r[i] ^ p1[i];
    }
    let mut y = e(k, &x);
    for i in 0..16 {
        y[i] ^= p2[i];
    }
    e(k, &y)
}

/// s1: legacy pairing short-term key.
pub fn s1(k: &[u8; 16], r1: &[u8; 16], r2: &[u8; 16]) -> [u8; 16] {
    let mut r = [0u8; 16];
    r[..8].copy_from_slice(&r2[..8]);
    r[8..].copy_from_slice(&r1[..8]);
    e(k, &r)
}

/// A P-256 key pair for LE Secure Connections.
pub struct KeyPair {
    secret: SecretKey,
    /// Public key X and Y in wire order (as in the Pairing Public Key PDU).
    pub x: [u8; 32],
    pub y: [u8; 32],
}

impl KeyPair {
    /// From 32 random bytes; None if they are not a valid scalar (retry
    /// with fresh randomness).
    pub fn from_random(seed: &[u8; 32]) -> Option<KeyPair> {
        let secret = SecretKey::from_bytes(GenericArray::from_slice(seed)).ok()?;
        let p = secret.public_key().to_encoded_point(false);
        let x: [u8; 32] = p.x()?.as_slice().try_into().ok()?;
        let y: [u8; 32] = p.y()?.as_slice().try_into().ok()?;
        Some(KeyPair {
            secret,
            x: rev(&x),
            y: rev(&y),
        })
    }

    /// The DHKey (shared X coordinate, wire order) with a peer's public
    /// key, or None if the peer's point is not on the curve.
    pub fn dhkey(&self, px: &[u8; 32], py: &[u8; 32]) -> Option<[u8; 32]> {
        let ep = EncodedPoint::from_affine_coordinates(
            GenericArray::from_slice(&rev(px)),
            GenericArray::from_slice(&rev(py)),
            false,
        );
        let peer = Option::<PublicKey>::from(PublicKey::from_encoded_point(&ep))?;
        let s = p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), peer.as_affine());
        let x: [u8; 32] = s.raw_secret_bytes().as_slice().try_into().ok()?;
        Some(rev(&x))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{arr, le, msb};
    use crate::{Addr, AddrType};

    const U: &str = "20b003d2 f297be2c 5e2c83a7 e9f9a5b9 eff49111 acf4fddb cc030148 0e359de6";
    const V: &str = "55188b3d 32f6bb9a 900afcfb eed4e72a 59cb9ac2 f19d7cfb 6b4fdd49 f47fc5fd";
    const X: &str = "d5cb8454 d177733e ffffb2ec 712baeab";
    const Y: &str = "a6e8e7cc 25a75f6e 216583f7 ff3dc4cf";

    #[test]
    fn rfc4493() {
        let k = arr(msb("2b7e1516 28aed2a6 abf71588 09cf4f3c"));
        assert_eq!(
            aes_cmac(&k, &[]).to_vec(),
            msb("bb1d6929 e9593728 7fa37d12 9b756746")
        );
        assert_eq!(
            aes_cmac(&k, &msb("6bc1bee2 2e409f96 e93d7e11 7393172a")).to_vec(),
            msb("070a16b4 6b4d4144 f79bdd9d d04a287c")
        );
    }

    #[test]
    fn debug_key_pair() {
        // The SMP debug key pair (Vol 3 Part H 2.3.5.6.1).
        let seed = arr(msb(
            "3f49f6d4 a3c55f38 74c9b3e3 d2103f50 4aff607b eb40b799 5899b8a6 cd3c1abd",
        ));
        let k = KeyPair::from_random(&seed).unwrap();
        assert_eq!(k.x.to_vec(), le(U));
        assert_eq!(
            k.y.to_vec(),
            le("dc809c49 652aeb6d 63329abf 5a52155c 766345c2 8fed3024 741c8ed0 1589d28b")
        );
        // A key agreement with itself is symmetric and deterministic.
        let d = k.dhkey(&k.x, &k.y).unwrap();
        assert_eq!(d, k.dhkey(&k.x, &k.y).unwrap());
        // A point off the curve is refused.
        let mut bad = k.y;
        bad[0] ^= 1;
        assert!(k.dhkey(&k.x, &bad).is_none());
    }

    #[test]
    fn f5_f6_vectors() {
        let w = arr(le(
            "ec0234a3 57c8ad05 341010a6 0a397d9b 99796b13 b4f866f1 868d34f3 73bfa698",
        ));
        let n1 = arr(le(X));
        let n2 = arr(le(Y));
        let a1 = arr(le("00561237 37bfce"));
        let a2 = arr(le("00a71370 2dcfc1"));
        let (mac, ltk) = f5(&w, &n1, &n2, &a1, &a2);
        assert_eq!(mac.to_vec(), le("2965f176 a1084a02 fd3f6a20 ce636e20"));
        assert_eq!(ltk.to_vec(), le("69867911 69d7cd23 980522b5 94750a38"));
        let r = arr(le("12a3343b b453bb54 08da42d2 0c2d0fc8"));
        let io = arr(le("010102"));
        assert_eq!(
            f6(&mac, &n1, &n2, &r, &io, &a1, &a2).to_vec(),
            le("e3c47398 9cd0e8c5 d26c0b09 da958f61")
        );
        // The 56-bit address layout f5/f6 expect.
        let a = Addr::parse("56:12:37:37:BF:CE").unwrap();
        assert_eq!(addr56(&a, AddrType::Public), a1);
    }

    #[test]
    fn g2_vector() {
        let v = g2(&arr(le(U)), &arr(le(V)), &arr(le(X)), &arr(le(Y)));
        assert_eq!(v, 0x2f9ed5ba % 1_000_000);
    }

    #[test]
    fn ah_vector() {
        let irk = arr(le("ec0234a3 57c8ad05 341010a6 0a397d9b"));
        assert_eq!(ah(&irk, &arr(le("708194"))).to_vec(), le("0dfbaa"));
        let rpa = Addr::parse("70:81:94:0D:FB:AA").unwrap();
        assert!(rpa_matches(&irk, &rpa));
        assert!(!rpa_matches(
            &irk,
            &Addr::parse("70:81:94:0D:FB:AB").unwrap()
        ));
    }

    #[test]
    fn legacy_c1_s1() {
        let k = [0u8; 16];
        let r = arr(le("5783D52156AD6F0E6388274EC6702EE0"));
        let preq = arr(le("07071000000101"));
        let pres = arr(le("05000800000302"));
        let ia = Addr::parse("A1:A2:A3:A4:A5:A6").unwrap();
        let ra = Addr::parse("B1:B2:B3:B4:B5:B6").unwrap();
        assert_eq!(
            c1(&k, &r, &preq, &pres, 1, 0, &ia, &ra).to_vec(),
            le("1e1e3fef878988ead2a74dc5bef13b86")
        );
        let r1 = arr(le("000F0E0D0C0B0A091122334455667788"));
        let r2 = arr(le("010203040506070899AABBCCDDEEFF00"));
        assert_eq!(
            s1(&k, &r1, &r2).to_vec(),
            le("9a1fe1f0e8b0f49b5b4216ae796da062")
        );
    }
}
