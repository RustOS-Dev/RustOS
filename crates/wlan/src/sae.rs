//! WPA3 Simultaneous Authentication of Equals (IEEE 802.11-2020 §12.4) for
//! ECC group 19 (NIST P-256), with both password element derivations:
//! hunting-and-pecking and hash-to-element (H2E, SSWU).

use crate::Mac;
use crate::crypto::{self, hmac_sha256, kdf_sha256};
use alloc::vec::Vec;
use p256::elliptic_curve::ff::{Field, PrimeField};
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{AffinePoint, EncodedPoint, FieldBytes, FieldElement, ProjectivePoint, Scalar};

pub const GROUP_19: u16 = 19;

const P: [u8; 32] = hex32("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff");
const Q_MINUS_1: [u8; 32] =
    hex32("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632550");
const B: [u8; 32] = hex32("5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b");
/// 2^256 mod p.
const R256: [u8; 32] = hex32("00000000fffffffeffffffffffffffffffffffff000000000000000000000001");

const fn hex32(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        let h = (b[2 * i] as char).to_digit(16).unwrap() as u8;
        let l = (b[2 * i + 1] as char).to_digit(16).unwrap() as u8;
        out[i] = (h << 4) | l;
        i += 1;
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaeError {
    /// Malformed or unsupported commit/confirm.
    BadMessage,
    UnsupportedGroup,
    /// The peer's element or scalar is invalid (or a reflection of ours).
    BadPeer,
    /// Confirm did not verify: wrong password.
    ConfirmMismatch,
    /// No password element could be derived.
    NoPwe,
}

/// Big-endian 256-bit a - b (a >= b assumed).
fn sub256(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut v = a[i] as i16 - b[i] as i16 - borrow;
        borrow = 0;
        if v < 0 {
            v += 256;
            borrow = 1;
        }
        out[i] = v as u8;
    }
    out
}

fn fe(b: &[u8; 32]) -> Option<FieldElement> {
    Option::from(FieldElement::from_bytes(FieldBytes::from_slice(b)))
}

/// Reduce a big-endian integer of up to 48 bytes modulo p.
fn fe_reduce(v: &[u8]) -> FieldElement {
    let (hi, lo) = if v.len() > 32 {
        v.split_at(v.len() - 32)
    } else {
        (&[][..], v)
    };
    let mut lo32 = [0u8; 32];
    lo32[32 - lo.len()..].copy_from_slice(lo);
    if lo32 >= P {
        lo32 = sub256(&lo32, &P);
    }
    let mut hi32 = [0u8; 32];
    hi32[32 - hi.len()..].copy_from_slice(hi);
    fe(&lo32).unwrap() + fe(&hi32).unwrap() * fe(&R256).unwrap()
}

fn is_odd(f: &FieldElement) -> bool {
    f.to_bytes()[31] & 1 == 1
}

/// y² = x³ - 3x + b
fn curve_rhs(x: &FieldElement) -> FieldElement {
    let b = fe(&B).unwrap();
    x.square() * x - (*x + *x + *x) + b
}

fn point(x: &FieldElement, y: &FieldElement) -> Option<AffinePoint> {
    let ep = EncodedPoint::from_affine_coordinates(&x.to_bytes(), &y.to_bytes(), false);
    Option::from(AffinePoint::from_encoded_point(&ep))
}

fn encode_point(p: &AffinePoint) -> [u8; 64] {
    let ep = p.to_encoded_point(false);
    let mut out = [0u8; 64];
    out.copy_from_slice(&ep.as_bytes()[1..65]);
    out
}

fn decode_point(b: &[u8]) -> Option<AffinePoint> {
    if b.len() != 64 {
        return None;
    }
    let x: [u8; 32] = b[..32].try_into().ok()?;
    let y: [u8; 32] = b[32..].try_into().ok()?;
    point(&fe(&x)?, &fe(&y)?)
}

fn scalar_from(b: &[u8; 32]) -> Option<Scalar> {
    Option::from(Scalar::from_repr(*FieldBytes::from_slice(b)))
}

fn scalar_bytes(s: &Scalar) -> [u8; 32] {
    s.to_repr().into()
}

/// (max, min) of the two MAC addresses.
fn macs(a: &Mac, b: &Mac) -> Vec<u8> {
    let (lo, hi) = crypto::min_max(a, b);
    let mut v = hi.to_vec();
    v.extend_from_slice(lo);
    v
}

/// Hunting-and-pecking password element (§12.4.4.2.2), 40 iterations.
pub fn pwe_hunting_and_pecking(password: &[u8], a: &Mac, b: &Mac) -> Option<ProjectivePoint> {
    let key = macs(a, b);
    let mut found: Option<(FieldElement, FieldElement, u8)> = None;
    let mut counter: u8 = 1;
    while counter <= 40 || (found.is_none() && counter < 255) {
        let base = hmac_sha256(&key, &[password, &[counter]]);
        let seed = kdf_sha256(&base, b"SAE Hunting and Pecking", &P, 256);
        let seed: [u8; 32] = seed.try_into().unwrap();
        if seed < P {
            let x = fe(&seed)?;
            let y2 = curve_rhs(&x);
            let y: Option<FieldElement> = y2.sqrt().into();
            if let Some(y) = y
                && found.is_none()
            {
                found = Some((x, y, base[31] & 1));
            }
        }
        counter = counter.wrapping_add(1);
    }
    let (x, y, lsb) = found?;
    let y = if (is_odd(&y) as u8) == lsb { y } else { -y };
    point(&x, &y).map(ProjectivePoint::from)
}

/// Simplified SWU map to P-256 with z = -10 (as specified for SAE H2E).
fn sswu(u: &FieldElement) -> ProjectivePoint {
    let a = -FieldElement::from(3u64);
    let b = fe(&B).unwrap();
    let z = -FieldElement::from(10u64);
    let u2 = u.square();
    let m = z.square() * u2.square() + z * u2;
    let m_zero = bool::from(m.is_zero());
    let t: FieldElement = Option::from(m.invert()).unwrap_or(FieldElement::ZERO);
    let x1 = if m_zero {
        b * Option::<FieldElement>::from((z * a).invert()).unwrap()
    } else {
        (-b) * Option::<FieldElement>::from(a.invert()).unwrap() * (FieldElement::ONE + t)
    };
    let gx1 = x1.square() * x1 + a * x1 + b;
    let x2 = z * u2 * x1;
    let gx2 = x2.square() * x2 + a * x2 + b;
    let (x, v) = if Option::<FieldElement>::from(gx1.sqrt()).is_some() {
        (x1, gx1)
    } else {
        (x2, gx2)
    };
    let mut y: FieldElement = Option::from(v.sqrt()).unwrap();
    if is_odd(u) != is_odd(&y) {
        y = -y;
    }
    ProjectivePoint::from(point(&x, &y).unwrap())
}

/// The H2E password element base PT (independent of the MAC addresses, so
/// it can be computed once per network).
pub fn h2e_pt(ssid: &[u8], password: &[u8], identifier: Option<&[u8]>) -> ProjectivePoint {
    let mut ikm = password.to_vec();
    if let Some(id) = identifier {
        ikm.extend_from_slice(id);
    }
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(ssid), &ikm);
    let mut v1 = [0u8; 48];
    let mut v2 = [0u8; 48];
    hk.expand(b"SAE Hash to Element u1 P1", &mut v1).unwrap();
    hk.expand(b"SAE Hash to Element u2 P2", &mut v2).unwrap();
    sswu(&fe_reduce(&v1)) + sswu(&fe_reduce(&v2))
}

/// PWE = val * PT with val = H(0, max(mac)||min(mac)) mod (q-1) + 1.
pub fn pwe_h2e(pt: &ProjectivePoint, a: &Mac, b: &Mac) -> ProjectivePoint {
    let (prk, _) = hkdf::Hkdf::<sha2::Sha256>::extract(Some(&[0u8; 32]), &macs(a, b));
    let mut v: [u8; 32] = prk.into();
    if v >= Q_MINUS_1 {
        v = sub256(&v, &Q_MINUS_1);
    }
    let val = scalar_from(&v).unwrap() + Scalar::ONE;
    *pt * val
}

fn random_scalar(rng: &mut dyn FnMut(&mut [u8])) -> Scalar {
    loop {
        let mut b = [0u8; 32];
        rng(&mut b);
        if let Some(s) = scalar_from(&b)
            && !bool::from(s.is_zero())
            && s != Scalar::ONE
        {
            return s;
        }
    }
}

/// One side of an SAE exchange.
pub struct Sae {
    pub h2e: bool,
    pwe: ProjectivePoint,
    rand: Scalar,
    scalar: Scalar,
    element: AffinePoint,
    peer_scalar: Option<Scalar>,
    peer_element: Option<AffinePoint>,
    kck: [u8; 32],
    pmk: [u8; 32],
    pmkid: [u8; 16],
    send_confirm: u16,
    /// Anti-clogging token to include in the next commit.
    pub token: Option<Vec<u8>>,
}

impl Sae {
    fn with_pwe(pwe: ProjectivePoint, h2e: bool, rng: &mut dyn FnMut(&mut [u8])) -> Sae {
        loop {
            let rand = random_scalar(rng);
            let mask = random_scalar(rng);
            let scalar = rand + mask;
            if bool::from(scalar.is_zero()) || scalar == Scalar::ONE {
                continue;
            }
            return Self::with_rand_mask(pwe, h2e, rand, mask);
        }
    }

    fn with_rand_mask(pwe: ProjectivePoint, h2e: bool, rand: Scalar, mask: Scalar) -> Sae {
        Sae {
            h2e,
            pwe,
            rand,
            scalar: rand + mask,
            element: (-(pwe * mask)).to_affine(),
            peer_scalar: None,
            peer_element: None,
            kck: [0; 32],
            pmk: [0; 32],
            pmkid: [0; 16],
            send_confirm: 0,
            token: None,
        }
    }

    /// Hunting-and-pecking SAE between `own` and `peer`.
    pub fn new(
        password: &[u8],
        own: Mac,
        peer: Mac,
        rng: &mut dyn FnMut(&mut [u8]),
    ) -> Result<Sae, SaeError> {
        let pwe = pwe_hunting_and_pecking(password, &own, &peer).ok_or(SaeError::NoPwe)?;
        Ok(Self::with_pwe(pwe, false, rng))
    }

    /// Hash-to-element SAE using a precomputed PT.
    pub fn new_h2e(
        pt: &ProjectivePoint,
        own: Mac,
        peer: Mac,
        rng: &mut dyn FnMut(&mut [u8]),
    ) -> Sae {
        Self::with_pwe(pwe_h2e(pt, &own, &peer), true, rng)
    }

    /// Authentication status code to use for the commit.
    pub fn commit_status(&self) -> u16 {
        if self.h2e {
            crate::frame::STATUS_SAE_HASH_TO_ELEMENT
        } else {
            crate::frame::STATUS_SUCCESS
        }
    }

    /// Commit message body (after the auth algorithm/seq/status fields).
    pub fn commit(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(100);
        b.extend_from_slice(&GROUP_19.to_le_bytes());
        if let Some(t) = &self.token
            && !self.h2e
        {
            b.extend_from_slice(t);
        }
        b.extend_from_slice(&scalar_bytes(&self.scalar));
        b.extend_from_slice(&encode_point(&self.element));
        if let Some(t) = &self.token
            && self.h2e
        {
            // Anti-Clogging Token Container element.
            b.extend_from_slice(&[0xFF, (t.len() + 1) as u8, 93]);
            b.extend_from_slice(t);
        }
        b
    }

    /// Handle an anti-clogging request (status 76): remember the token.
    pub fn set_token_from(&mut self, body: &[u8]) -> Result<(), SaeError> {
        if body.len() < 2 || u16::from_le_bytes([body[0], body[1]]) != GROUP_19 {
            return Err(SaeError::BadMessage);
        }
        let rest = &body[2..];
        let token = if self.h2e && rest.len() >= 3 && rest[0] == 0xFF && rest[2] == 93 {
            rest[3..(2 + rest[1] as usize).min(rest.len())].to_vec()
        } else {
            rest.to_vec()
        };
        self.token = Some(token);
        Ok(())
    }

    /// Process the peer's commit and derive KCK/PMK.
    pub fn process_commit(&mut self, body: &[u8]) -> Result<(), SaeError> {
        if body.len() < 2 {
            return Err(SaeError::BadMessage);
        }
        if u16::from_le_bytes([body[0], body[1]]) != GROUP_19 {
            return Err(SaeError::UnsupportedGroup);
        }
        // Scalar and element follow the group (an AP never sends a token
        // back to us, but tolerate one before them for HnP).
        if body.len() < 2 + 96 {
            return Err(SaeError::BadMessage);
        }
        let off = if self.h2e { 2 } else { body.len() - 96 };
        let off = if self.h2e && body.len() > 98 && body[98] != 0xFF {
            body.len() - 96
        } else {
            off
        };
        let s: [u8; 32] = body[off..off + 32].try_into().unwrap();
        let peer_scalar = scalar_from(&s).ok_or(SaeError::BadPeer)?;
        if bool::from(peer_scalar.is_zero()) || peer_scalar == Scalar::ONE {
            return Err(SaeError::BadPeer);
        }
        let peer_element = decode_point(&body[off + 32..off + 96]).ok_or(SaeError::BadPeer)?;
        // Reflection attack: peer echoing our own commit.
        if peer_scalar == self.scalar && peer_element == self.element {
            return Err(SaeError::BadPeer);
        }
        let k = (self.pwe * peer_scalar + ProjectivePoint::from(peer_element)) * self.rand;
        let k = k.to_affine();
        if k == AffinePoint::IDENTITY {
            return Err(SaeError::BadPeer);
        }
        let kx = encode_point(&k);
        let keyseed = hmac_sha256(&[0u8; 32], &[&kx[..32]]);
        let ctx = scalar_bytes(&(self.scalar + peer_scalar));
        let kp = kdf_sha256(&keyseed, b"SAE KCK and PMK", &ctx, 512);
        self.kck.copy_from_slice(&kp[..32]);
        self.pmk.copy_from_slice(&kp[32..64]);
        self.pmkid.copy_from_slice(&ctx[..16]);
        self.peer_scalar = Some(peer_scalar);
        self.peer_element = Some(peer_element);
        Ok(())
    }

    fn confirm_hash(
        &self,
        counter: u16,
        s1: &Scalar,
        e1: &AffinePoint,
        s2: &Scalar,
        e2: &AffinePoint,
    ) -> [u8; 32] {
        hmac_sha256(
            &self.kck,
            &[
                &counter.to_le_bytes(),
                &scalar_bytes(s1),
                &encode_point(e1),
                &scalar_bytes(s2),
                &encode_point(e2),
            ],
        )
    }

    /// Confirm message body: send-confirm counter + confirm.
    pub fn confirm(&mut self) -> Vec<u8> {
        self.send_confirm = self.send_confirm.saturating_add(1);
        let (ps, pe) = (self.peer_scalar.unwrap(), self.peer_element.unwrap());
        let c = self.confirm_hash(self.send_confirm, &self.scalar, &self.element, &ps, &pe);
        let mut b = self.send_confirm.to_le_bytes().to_vec();
        b.extend_from_slice(&c);
        b
    }

    /// Verify the peer's confirm.
    pub fn process_confirm(&self, body: &[u8]) -> Result<(), SaeError> {
        if body.len() < 34 {
            return Err(SaeError::BadMessage);
        }
        let (ps, pe) = (
            self.peer_scalar.ok_or(SaeError::BadMessage)?,
            self.peer_element.ok_or(SaeError::BadMessage)?,
        );
        let counter = u16::from_le_bytes([body[0], body[1]]);
        let expect = self.confirm_hash(counter, &ps, &pe, &self.scalar, &self.element);
        if crypto::ct_eq(&expect, &body[2..34]) {
            Ok(())
        } else {
            Err(SaeError::ConfirmMismatch)
        }
    }

    pub fn pmk(&self) -> [u8; 32] {
        self.pmk
    }
    pub fn pmkid(&self) -> [u8; 16] {
        self.pmkid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eapol::tests::rng;

    const STA: Mac = [0x02, 0, 0, 0, 0, 1];
    const AP: Mac = [0x02, 0, 0, 0, 0, 2];

    fn exchange(mut a: Sae, mut b: Sae) -> Result<(Sae, Sae), SaeError> {
        a.process_commit(&b.commit())?;
        b.process_commit(&a.commit())?;
        b.process_confirm(&a.confirm())?;
        a.process_confirm(&b.confirm())?;
        Ok((a, b))
    }

    fn h(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// IEEE 802.11-2020 Annex J.10 (as used by hostapd's module tests).
    #[test]
    fn annex_j10_hunting_and_pecking() {
        let a1: Mac = h("4d3f2fffe387").try_into().unwrap();
        let a2: Mac = h("a5d8aa958e3c").try_into().unwrap();
        let pwe = pwe_hunting_and_pecking(b"mekmitasdigoat", &a1, &a2).unwrap();
        let rand = scalar_from(
            &h("992465fd3daa3c60aa6565b7f62a2a7f2e12dd12f198faf4fbed89d7ff1ace94")
                .try_into()
                .unwrap(),
        )
        .unwrap();
        let mask = scalar_from(
            &h("9507a90f777a044d6a0830b91ea3d5dd70bece44e1acffb86983b5e1bf9fb322")
                .try_into()
                .unwrap(),
        )
        .unwrap();
        let mut sae = Sae::with_rand_mask(pwe, false, rand, mask);
        assert_eq!(
            sae.commit(),
            h(
                "13002e2c0f0db52440ad146d967114ce005ce1eab0aa2c2e5c2871b774f6c2575c65d5ad9e00829707aa36ba8b859738fc961d08243505f47c035376d7ac4bc8d7b95083bf43827d0fc31ed778dd3671fd21a46d1091d64b6f9a1e1272621325dbe1"
            )
        );
        sae.process_commit(&h("1300591b96f3397fb945100848e7b550543b6720d88337ee93fc49fd6df7e08b5223e71b9bb048d3873f20556953a96c91536fd8ee6ca9b4a68a148b056a909be03e83ae208f60f8ef5537858074db06687032399862999b511e0a1552a5fea317c2")).unwrap();
        assert_eq!(
            sae.kck.to_vec(),
            h("1e733f6d9bd53256287304338831b09a39406d121017073a5c30db36f36cb81a")
        );
        assert_eq!(
            sae.pmk().to_vec(),
            h("4e4dfab1a2dd8ac1a91790f953faaa452ae5c6873ab75b63605ba663f8a7fe59")
        );
        assert_eq!(sae.pmkid().to_vec(), h("8747a600eea3f9f22475df58ca1e5498"));
    }

    #[test]
    fn annex_j10_hash_to_element() {
        let a1: Mac = h("00095b66ec1e").try_into().unwrap();
        let a2: Mac = h("000b6bd90246").try_into().unwrap();
        let pt = h2e_pt(b"byteme", b"mekmitasdigoat", Some(b"psk4internet"));
        let pwe = encode_point(&pwe_h2e(&pt, &a1, &a2).to_affine());
        assert_eq!(
            pwe[..32].to_vec(),
            h("c93049b9e64000f848201649e999f2b5c22dea69b5632c9df4d633b8aa1f6c1e")
        );
        assert_eq!(
            pwe[32..].to_vec(),
            h("73634e94b53d82e7383a8d258199d9dc1a5ee8269d060382ccbf33e614ff59a0")
        );
    }

    #[test]
    fn field_helpers() {
        // p reduces to 0; 2^256 - 1 to R256 - 1.
        assert_eq!(fe_reduce(&P), FieldElement::ZERO);
        assert_eq!(
            fe_reduce(&[0xFF; 32]),
            fe(&R256).unwrap() - FieldElement::ONE
        );
        // 2^256 (as 33 bytes) reduces to R256.
        let mut two256 = [0u8; 33];
        two256[0] = 1;
        assert_eq!(fe_reduce(&two256), fe(&R256).unwrap());
    }

    #[test]
    fn hunting_and_pecking_agree() {
        let mut r = rng();
        let a = Sae::new(b"mekmitasdigoat", STA, AP, &mut r).unwrap();
        let b = Sae::new(b"mekmitasdigoat", AP, STA, &mut r).unwrap();
        let (a, b) = exchange(a, b).unwrap();
        assert_eq!(a.pmk(), b.pmk());
        assert_eq!(a.pmkid(), b.pmkid());
        assert_ne!(a.pmk(), [0; 32]);
    }

    #[test]
    fn pwe_is_symmetric_and_on_curve() {
        let p1 = pwe_hunting_and_pecking(b"password", &STA, &AP).unwrap();
        let p2 = pwe_hunting_and_pecking(b"password", &AP, &STA).unwrap();
        assert_eq!(p1, p2);
        let pt = h2e_pt(b"byteme", b"mekmitasdigoat", Some(b"psk4internet"));
        assert_eq!(pwe_h2e(&pt, &STA, &AP), pwe_h2e(&pt, &AP, &STA));
    }

    #[test]
    fn h2e_agree() {
        let mut r = rng();
        let pt = h2e_pt(b"home", b"hunter2 is secret", None);
        let a = Sae::new_h2e(&pt, STA, AP, &mut r);
        let b = Sae::new_h2e(&pt, AP, STA, &mut r);
        let (a, b) = exchange(a, b).unwrap();
        assert_eq!(a.pmk(), b.pmk());
    }

    #[test]
    fn wrong_password_fails_confirm() {
        let mut r = rng();
        let a = Sae::new(b"right password", STA, AP, &mut r).unwrap();
        let b = Sae::new(b"wrong password", AP, STA, &mut r).unwrap();
        assert_eq!(exchange(a, b).err(), Some(SaeError::ConfirmMismatch));
    }

    #[test]
    fn reflection_rejected() {
        let mut r = rng();
        let mut a = Sae::new(b"pw", STA, AP, &mut r).unwrap();
        let c = a.commit();
        assert_eq!(a.process_commit(&c), Err(SaeError::BadPeer));
    }

    #[test]
    fn anti_clogging_token() {
        let mut r = rng();
        let mut a = Sae::new(b"pw", STA, AP, &mut r).unwrap();
        let mut req = GROUP_19.to_le_bytes().to_vec();
        req.extend_from_slice(&[9; 20]);
        a.set_token_from(&req).unwrap();
        let c = a.commit();
        assert_eq!(c.len(), 2 + 20 + 96);
        assert_eq!(&c[2..22], &[9; 20]);
    }
}
