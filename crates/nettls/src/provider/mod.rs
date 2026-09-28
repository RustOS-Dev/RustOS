//! A rustls [`CryptoProvider`] built from RustCrypto crates.
//!
//! Pure Rust with portable (non-SIMD) backends, so it runs in RustOS's
//! soft-float userland. Offers:
//! * TLS 1.3: AES-128/256-GCM, ChaCha20-Poly1305;
//! * TLS 1.2: ECDHE-ECDSA/RSA with AES-128/256-GCM and ChaCha20-Poly1305;
//! * key exchange: X25519, secp256r1, secp384r1;
//! * signatures: ECDSA P-256/P-384 (SHA-256/384), Ed25519, RSA PKCS#1 v1.5
//!   and PSS (SHA-256/384/512, 2048–8192 bits);
//! * signing with ECDSA P-256 PKCS#8 keys (tests, client certificates).

mod aead;
mod hash;
mod hmac;
mod kx;
mod sign;
mod verify;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use rustls::crypto::tls12::PrfUsingHmac;
use rustls::crypto::tls13::HkdfUsingHmac;
use rustls::crypto::{
    CipherSuiteCommon, CryptoProvider, GetRandomFailed, KeyExchangeAlgorithm, KeyProvider,
    SecureRandom,
};
use rustls::{
    CipherSuite, SignatureScheme, SupportedCipherSuite, Tls12CipherSuite, Tls13CipherSuite,
};

/// Source of cryptographically secure random bytes (the platform's
/// `getrandom`). Must be set with [`set_random_source`] before use.
static RANDOM: AtomicUsize = AtomicUsize::new(0);

/// Install the random-byte source; it returns false on failure.
pub fn set_random_source(f: fn(&mut [u8]) -> bool) {
    RANDOM.store(f as usize, Ordering::SeqCst);
}

pub(crate) fn fill_random(buf: &mut [u8]) -> bool {
    let p = RANDOM.load(Ordering::SeqCst);
    if p == 0 {
        return false;
    }
    // SAFETY: only `set_random_source` stores into RANDOM, always a valid
    // `fn(&mut [u8]) -> bool`.
    let f: fn(&mut [u8]) -> bool = unsafe { core::mem::transmute(p) };
    f(buf)
}

/// `rand_core` adapter over the random source (for key generation).
pub(crate) struct Rng;

impl rand_core::RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        assert!(fill_random(dest), "no random source");
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        if fill_random(dest) {
            Ok(())
        } else {
            Err(rand_core::Error::from(
                core::num::NonZeroU32::new(rand_core::Error::CUSTOM_START).unwrap(),
            ))
        }
    }
}
impl rand_core::CryptoRng for Rng {}

#[derive(Debug)]
struct Provider;

impl SecureRandom for Provider {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        if fill_random(buf) {
            Ok(())
        } else {
            Err(GetRandomFailed)
        }
    }
}

impl KeyProvider for Provider {
    fn load_private_key(
        &self,
        key: pki_types::PrivateKeyDer<'static>,
    ) -> Result<Arc<dyn rustls::sign::SigningKey>, rustls::Error> {
        sign::load(key)
    }
}

pub(crate) static HMAC_SHA256: hmac::Hmac<sha2::Sha256> = hmac::Hmac::new(32);
pub(crate) static HMAC_SHA384: hmac::Hmac<sha2::Sha384> = hmac::Hmac::new(48);

const TLS12_ECDSA: &[SignatureScheme] = &[
    SignatureScheme::ED25519,
    SignatureScheme::ECDSA_NISTP521_SHA512,
    SignatureScheme::ECDSA_NISTP384_SHA384,
    SignatureScheme::ECDSA_NISTP256_SHA256,
];
const TLS12_RSA: &[SignatureScheme] = &[
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PKCS1_SHA512,
    SignatureScheme::RSA_PKCS1_SHA384,
    SignatureScheme::RSA_PKCS1_SHA256,
];

static TLS13_AES_128_GCM_SHA256: Tls13CipherSuite = Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_AES_128_GCM_SHA256,
        hash_provider: &hash::SHA256,
        confidentiality_limit: 1 << 24,
    },
    hkdf_provider: &HkdfUsingHmac(&HMAC_SHA256),
    aead_alg: &aead::AES128_GCM,
    quic: None,
};
static TLS13_AES_256_GCM_SHA384: Tls13CipherSuite = Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_AES_256_GCM_SHA384,
        hash_provider: &hash::SHA384,
        confidentiality_limit: 1 << 24,
    },
    hkdf_provider: &HkdfUsingHmac(&HMAC_SHA384),
    aead_alg: &aead::AES256_GCM,
    quic: None,
};
static TLS13_CHACHA20_POLY1305_SHA256: Tls13CipherSuite = Tls13CipherSuite {
    common: CipherSuiteCommon {
        suite: CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
        hash_provider: &hash::SHA256,
        confidentiality_limit: u64::MAX,
    },
    hkdf_provider: &HkdfUsingHmac(&HMAC_SHA256),
    aead_alg: &aead::CHACHA20_POLY1305,
    quic: None,
};

macro_rules! tls12_suite {
    ($name:ident, $suite:ident, $hash:expr, $hmac:expr, $sign:expr, $aead:expr, $limit:expr) => {
        static $name: Tls12CipherSuite = Tls12CipherSuite {
            common: CipherSuiteCommon {
                suite: CipherSuite::$suite,
                hash_provider: $hash,
                confidentiality_limit: $limit,
            },
            kx: KeyExchangeAlgorithm::ECDHE,
            sign: $sign,
            aead_alg: $aead,
            prf_provider: &PrfUsingHmac($hmac),
        };
    };
}

tls12_suite!(
    ECDSA_AES128,
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    &hash::SHA256,
    &HMAC_SHA256,
    TLS12_ECDSA,
    &aead::AES128_GCM,
    1 << 24
);
tls12_suite!(
    ECDSA_AES256,
    TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    &hash::SHA384,
    &HMAC_SHA384,
    TLS12_ECDSA,
    &aead::AES256_GCM,
    1 << 24
);
tls12_suite!(
    ECDSA_CHACHA,
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    &hash::SHA256,
    &HMAC_SHA256,
    TLS12_ECDSA,
    &aead::CHACHA20_POLY1305,
    u64::MAX
);
tls12_suite!(
    RSA_AES128,
    TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    &hash::SHA256,
    &HMAC_SHA256,
    TLS12_RSA,
    &aead::AES128_GCM,
    1 << 24
);
tls12_suite!(
    RSA_AES256,
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    &hash::SHA384,
    &HMAC_SHA384,
    TLS12_RSA,
    &aead::AES256_GCM,
    1 << 24
);
tls12_suite!(
    RSA_CHACHA,
    TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    &hash::SHA256,
    &HMAC_SHA256,
    TLS12_RSA,
    &aead::CHACHA20_POLY1305,
    u64::MAX
);

/// Cipher suites in preference order (AES-128 first: fastest in software).
pub static CIPHER_SUITES: &[SupportedCipherSuite] = &[
    SupportedCipherSuite::Tls13(&TLS13_AES_128_GCM_SHA256),
    SupportedCipherSuite::Tls13(&TLS13_CHACHA20_POLY1305_SHA256),
    SupportedCipherSuite::Tls13(&TLS13_AES_256_GCM_SHA384),
    SupportedCipherSuite::Tls12(&ECDSA_AES128),
    SupportedCipherSuite::Tls12(&RSA_AES128),
    SupportedCipherSuite::Tls12(&ECDSA_CHACHA),
    SupportedCipherSuite::Tls12(&RSA_CHACHA),
    SupportedCipherSuite::Tls12(&ECDSA_AES256),
    SupportedCipherSuite::Tls12(&RSA_AES256),
];

/// The provider.
pub fn provider() -> CryptoProvider {
    CryptoProvider {
        cipher_suites: CIPHER_SUITES.to_vec(),
        kx_groups: kx::ALL.to_vec(),
        signature_verification_algorithms: verify::ALGORITHMS,
        secure_random: &Provider,
        key_provider: &Provider,
    }
}
