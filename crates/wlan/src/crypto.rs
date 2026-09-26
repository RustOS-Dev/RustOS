//! Key derivation and integrity primitives from IEEE 802.11-2020 §12.7.

use alloc::vec::Vec;
use hmac::{Hmac, Mac as _};
use sha1::Sha1;
use sha2::Sha256;

type HmacSha1 = Hmac<Sha1>;
type HmacSha256 = Hmac<Sha256>;

pub fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
    let mut m = HmacSha1::new_from_slice(key).expect("hmac key");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac key");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// PSK = PBKDF2-HMAC-SHA1(passphrase, SSID, 4096, 256 bits).
pub fn wpa_psk(passphrase: &[u8], ssid: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase, ssid, 4096, &mut out);
    out
}

/// PRF-n from 802.11i (HMAC-SHA1 based).
pub fn prf_sha1(key: &[u8], label: &[u8], data: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 20);
    let mut i = 0u8;
    while out.len() < len {
        out.extend_from_slice(&hmac_sha1(key, &[label, &[0], data, &[i]]));
        i += 1;
    }
    out.truncate(len);
    out
}

/// KDF-SHA-256 with an output length of `bits` (§12.7.1.7.2).
pub fn kdf_sha256(key: &[u8], label: &[u8], context: &[u8], bits: usize) -> Vec<u8> {
    let len = bits.div_ceil(8);
    let mut out = Vec::with_capacity(len + 32);
    let mut i: u16 = 1;
    while out.len() < len {
        out.extend_from_slice(&hmac_sha256(
            key,
            &[&i.to_le_bytes(), label, context, &(bits as u16).to_le_bytes()],
        ));
        i += 1;
    }
    out.truncate(len);
    out
}

/// AES-128-CMAC.
pub fn aes_cmac(key: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let mut m = <cmac::Cmac<aes::Aes128> as cmac::Mac>::new_from_slice(key).expect("cmac key");
    cmac::Mac::update(&mut m, data);
    cmac::Mac::finalize(m).into_bytes().into()
}

/// AES key wrap (RFC 3394) with a 128-bit KEK.
pub fn aes_wrap(kek: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    let kek = aes_kw::KekAes128::from(*kek);
    kek.wrap_vec(data).ok()
}

pub fn aes_unwrap(kek: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    let kek = aes_kw::KekAes128::from(*kek);
    kek.unwrap_vec(data).ok()
}

/// Orders two byte strings (min, max) as the key derivations require.
pub fn min_max<'a>(a: &'a [u8], b: &'a [u8]) -> (&'a [u8], &'a [u8]) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Constant-time equality.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn psk_ieee_annex_j() {
        // IEEE 802.11-2020 Annex J.4.2 test vectors.
        assert_eq!(
            wpa_psk(b"password", b"IEEE").to_vec(),
            hex("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")
        );
        assert_eq!(
            wpa_psk(b"ThisIsAPassword", b"ThisIsASSID").to_vec(),
            hex("0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af")
        );
    }

    #[test]
    fn prf_rfc2202_style() {
        // 802.11 Annex J.3 PRF test vector 1: key 0x0b*20, "prefix", "Hi There".
        let out = prf_sha1(&[0x0b; 20], b"prefix", b"Hi There", 64);
        assert_eq!(
            out,
            hex("bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606e17d8da35402ffee75df78c3d31e0f889f012120c0862beb67753e7439ae242edb8373698356cf5a")
        );
    }

    #[test]
    fn cmac_rfc4493() {
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        assert_eq!(aes_cmac(&key, b"").to_vec(), hex("bb1d6929e95937287fa37d129b756746"));
    }

    #[test]
    fn keywrap_rfc3394() {
        let kek: [u8; 16] = hex("000102030405060708090A0B0C0D0E0F").try_into().unwrap();
        let data = hex("00112233445566778899AABBCCDDEEFF");
        let w = aes_wrap(&kek, &data).unwrap();
        assert_eq!(w, hex("1FA68B0A8112B447AEF34BD8FB5A7B829D3E862371D2CFE5"));
        assert_eq!(aes_unwrap(&kek, &w).unwrap(), data);
    }
}
