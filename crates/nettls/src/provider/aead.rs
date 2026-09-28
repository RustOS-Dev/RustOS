//! Record protection with AES-GCM and ChaCha20-Poly1305 (RustCrypto).

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use alloc::boxed::Box;
use chacha20poly1305::ChaCha20Poly1305;
use rustls::crypto::cipher::{
    make_tls12_aad, make_tls13_aad, AeadKey, InboundOpaqueMessage, InboundPlainMessage, Iv,
    KeyBlockShape, MessageDecrypter, MessageEncrypter, Nonce, OutboundOpaqueMessage,
    OutboundPlainMessage, PrefixedPayload, Tls12AeadAlgorithm, Tls13AeadAlgorithm,
    UnsupportedOperationError, NONCE_LEN,
};
use rustls::{ConnectionTrafficSecrets, ContentType, Error, ProtocolVersion};

const TAG_LEN: usize = 16;
const GCM_EXPLICIT: usize = 8;
const MAX_FRAGMENT_LEN: usize = 16384;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Aes128Gcm,
    Aes256Gcm,
    ChaCha20Poly1305,
}

pub struct Alg(pub Kind);

pub static AES128_GCM: Alg = Alg(Kind::Aes128Gcm);
pub static AES256_GCM: Alg = Alg(Kind::Aes256Gcm);
pub static CHACHA20_POLY1305: Alg = Alg(Kind::ChaCha20Poly1305);

/// A keyed AEAD instance.
enum Cipher {
    A128(Aes128Gcm),
    A256(Aes256Gcm),
    ChaCha(ChaCha20Poly1305),
}

impl Cipher {
    fn new(kind: Kind, key: &[u8]) -> Cipher {
        match kind {
            Kind::Aes128Gcm => Cipher::A128(Aes128Gcm::new_from_slice(key).unwrap()),
            Kind::Aes256Gcm => Cipher::A256(Aes256Gcm::new_from_slice(key).unwrap()),
            Kind::ChaCha20Poly1305 => {
                Cipher::ChaCha(ChaCha20Poly1305::new_from_slice(key).unwrap())
            }
        }
    }

    fn seal(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        buf: &mut [u8],
    ) -> Result<[u8; TAG_LEN], Error> {
        let n = aes_gcm::Nonce::from_slice(nonce);
        let tag = match self {
            Cipher::A128(c) => c.encrypt_in_place_detached(n, aad, buf),
            Cipher::A256(c) => c.encrypt_in_place_detached(n, aad, buf),
            Cipher::ChaCha(c) => {
                c.encrypt_in_place_detached(chacha20poly1305::Nonce::from_slice(nonce), aad, buf)
            }
        }
        .map_err(|_| Error::EncryptError)?;
        let mut t = [0u8; TAG_LEN];
        t.copy_from_slice(&tag);
        Ok(t)
    }

    /// Decrypt `buf` = ciphertext || tag in place; returns plaintext length.
    fn open(&self, nonce: &[u8; NONCE_LEN], aad: &[u8], buf: &mut [u8]) -> Result<usize, Error> {
        if buf.len() < TAG_LEN {
            return Err(Error::DecryptError);
        }
        let plen = buf.len() - TAG_LEN;
        let (ct, tag) = buf.split_at_mut(plen);
        let tag = aes_gcm::Tag::clone_from_slice(tag);
        let n = aes_gcm::Nonce::from_slice(nonce);
        match self {
            Cipher::A128(c) => c.decrypt_in_place_detached(n, aad, ct, &tag),
            Cipher::A256(c) => c.decrypt_in_place_detached(n, aad, ct, &tag),
            Cipher::ChaCha(c) => c.decrypt_in_place_detached(
                chacha20poly1305::Nonce::from_slice(nonce),
                aad,
                ct,
                &tag,
            ),
        }
        .map_err(|_| Error::DecryptError)?;
        Ok(plen)
    }
}

// ---------------------------------------------------------------------------
// TLS 1.3
// ---------------------------------------------------------------------------

impl Tls13AeadAlgorithm for Alg {
    fn encrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageEncrypter> {
        Box::new(Tls13Enc(Cipher::new(self.0, key.as_ref()), iv))
    }
    fn decrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageDecrypter> {
        Box::new(Tls13Dec(Cipher::new(self.0, key.as_ref()), iv))
    }
    fn key_len(&self) -> usize {
        match self.0 {
            Kind::Aes128Gcm => 16,
            _ => 32,
        }
    }
    fn extract_keys(
        &self,
        key: AeadKey,
        iv: Iv,
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok(match self.0 {
            Kind::Aes128Gcm => ConnectionTrafficSecrets::Aes128Gcm { key, iv },
            Kind::Aes256Gcm => ConnectionTrafficSecrets::Aes256Gcm { key, iv },
            Kind::ChaCha20Poly1305 => ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv },
        })
    }
}

struct Tls13Enc(Cipher, Iv);
struct Tls13Dec(Cipher, Iv);

impl MessageEncrypter for Tls13Enc {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total);
        payload.extend_from_chunks(&msg.payload);
        payload.extend_from_slice(&msg.typ.to_array());
        let nonce = Nonce::new(&self.1, seq).0;
        let aad = make_tls13_aad(total);
        let tag = self.0.seal(&nonce, &aad, payload.as_mut())?;
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(
            ContentType::ApplicationData,
            ProtocolVersion::TLSv1_2,
            payload,
        ))
    }
    fn encrypted_payload_len(&self, len: usize) -> usize {
        len + 1 + TAG_LEN
    }
}

impl MessageDecrypter for Tls13Dec {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let payload = &mut msg.payload;
        let nonce = Nonce::new(&self.1, seq).0;
        let aad = make_tls13_aad(payload.len());
        let n = self.0.open(&nonce, &aad, payload)?;
        payload.truncate(n);
        msg.into_tls13_unpadded_message()
    }
}

// ---------------------------------------------------------------------------
// TLS 1.2
// ---------------------------------------------------------------------------

impl Tls12AeadAlgorithm for Alg {
    fn encrypter(&self, key: AeadKey, iv: &[u8], extra: &[u8]) -> Box<dyn MessageEncrypter> {
        let c = Cipher::new(self.0, key.as_ref());
        match self.0 {
            Kind::ChaCha20Poly1305 => Box::new(Tls12ChaChaEnc(c, Iv::copy(iv))),
            _ => {
                let mut n = [0u8; NONCE_LEN];
                n[..4].copy_from_slice(iv);
                n[4..].copy_from_slice(extra);
                Box::new(Tls12GcmEnc(c, Iv::new(n)))
            }
        }
    }
    fn decrypter(&self, key: AeadKey, iv: &[u8]) -> Box<dyn MessageDecrypter> {
        let c = Cipher::new(self.0, key.as_ref());
        match self.0 {
            Kind::ChaCha20Poly1305 => Box::new(Tls12ChaChaDec(c, Iv::copy(iv))),
            _ => {
                let mut salt = [0u8; 4];
                salt.copy_from_slice(iv);
                Box::new(Tls12GcmDec(c, salt))
            }
        }
    }
    fn key_block_shape(&self) -> KeyBlockShape {
        match self.0 {
            Kind::ChaCha20Poly1305 => KeyBlockShape {
                enc_key_len: 32,
                fixed_iv_len: 12,
                explicit_nonce_len: 0,
            },
            k => KeyBlockShape {
                enc_key_len: if k == Kind::Aes128Gcm { 16 } else { 32 },
                fixed_iv_len: 4,
                explicit_nonce_len: 8,
            },
        }
    }
    fn extract_keys(
        &self,
        key: AeadKey,
        iv: &[u8],
        explicit: &[u8],
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        let mut n = [0u8; NONCE_LEN];
        match self.0 {
            Kind::ChaCha20Poly1305 => n.copy_from_slice(iv),
            _ => {
                n[..4].copy_from_slice(iv);
                n[4..].copy_from_slice(explicit);
            }
        }
        let iv = Iv::new(n);
        Ok(match self.0 {
            Kind::Aes128Gcm => ConnectionTrafficSecrets::Aes128Gcm { key, iv },
            Kind::Aes256Gcm => ConnectionTrafficSecrets::Aes256Gcm { key, iv },
            Kind::ChaCha20Poly1305 => ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv },
        })
    }
}

struct Tls12GcmEnc(Cipher, Iv);
struct Tls12GcmDec(Cipher, [u8; 4]);
struct Tls12ChaChaEnc(Cipher, Iv);
struct Tls12ChaChaDec(Cipher, Iv);

impl MessageEncrypter for Tls12GcmEnc {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total);
        let nonce = Nonce::new(&self.1, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, msg.payload.len());
        payload.extend_from_slice(&nonce[4..]);
        payload.extend_from_chunks(&msg.payload);
        let tag = self
            .0
            .seal(&nonce, &aad, &mut payload.as_mut()[GCM_EXPLICIT..])?;
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(msg.typ, msg.version, payload))
    }
    fn encrypted_payload_len(&self, len: usize) -> usize {
        len + GCM_EXPLICIT + TAG_LEN
    }
}

impl MessageDecrypter for Tls12GcmDec {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let payload = &mut msg.payload;
        if payload.len() < GCM_EXPLICIT + TAG_LEN {
            return Err(Error::DecryptError);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..4].copy_from_slice(&self.1);
        nonce[4..].copy_from_slice(&payload[..GCM_EXPLICIT]);
        let aad = make_tls12_aad(
            seq,
            msg.typ,
            msg.version,
            payload.len() - GCM_EXPLICIT - TAG_LEN,
        );
        let n = self.0.open(&nonce, &aad, &mut payload[GCM_EXPLICIT..])?;
        if n > MAX_FRAGMENT_LEN {
            return Err(Error::PeerSentOversizedRecord);
        }
        // Plaintext sits after the explicit nonce: shift it to the front.
        payload.copy_within(GCM_EXPLICIT..GCM_EXPLICIT + n, 0);
        payload.truncate(n);
        Ok(msg.into_plain_message())
    }
}

impl MessageEncrypter for Tls12ChaChaEnc {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        let total = self.encrypted_payload_len(msg.payload.len());
        let mut payload = PrefixedPayload::with_capacity(total);
        let nonce = Nonce::new(&self.1, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, msg.payload.len());
        payload.extend_from_chunks(&msg.payload);
        let tag = self.0.seal(&nonce, &aad, payload.as_mut())?;
        payload.extend_from_slice(&tag);
        Ok(OutboundOpaqueMessage::new(msg.typ, msg.version, payload))
    }
    fn encrypted_payload_len(&self, len: usize) -> usize {
        len + TAG_LEN
    }
}

impl MessageDecrypter for Tls12ChaChaDec {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let payload = &mut msg.payload;
        if payload.len() < TAG_LEN {
            return Err(Error::DecryptError);
        }
        let nonce = Nonce::new(&self.1, seq).0;
        let aad = make_tls12_aad(seq, msg.typ, msg.version, payload.len() - TAG_LEN);
        let n = self.0.open(&nonce, &aad, payload)?;
        if n > MAX_FRAGMENT_LEN {
            return Err(Error::PeerSentOversizedRecord);
        }
        payload.truncate(n);
        Ok(msg.into_plain_message())
    }
}
