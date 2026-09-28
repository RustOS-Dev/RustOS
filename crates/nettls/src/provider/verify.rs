//! Signature verification algorithms for certificates and handshakes.

use pki_types::{alg_id, AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::SignatureScheme;
use sha2::{Digest, Sha256, Sha384, Sha512};
use signature::hazmat::PrehashVerifier;

#[derive(Debug, Clone, Copy)]
enum H {
    S256,
    S384,
    S512,
}

fn digest(h: H, m: &[u8]) -> alloc::vec::Vec<u8> {
    match h {
        H::S256 => Sha256::digest(m).to_vec(),
        H::S384 => Sha384::digest(m).to_vec(),
        H::S512 => Sha512::digest(m).to_vec(),
    }
}

#[derive(Debug)]
enum Kind {
    EcdsaP256,
    EcdsaP384,
    RsaPkcs1,
    RsaPss,
    Ed25519,
}

#[derive(Debug)]
pub struct Alg {
    kind: Kind,
    hash: H,
    pk: AlgorithmIdentifier,
    sig: AlgorithmIdentifier,
}

impl SignatureVerificationAlgorithm for Alg {
    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        match self.kind {
            Kind::EcdsaP256 => {
                let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
                    .map_err(|_| InvalidSignature)?;
                let sig =
                    p256::ecdsa::Signature::from_der(signature).map_err(|_| InvalidSignature)?;
                key.verify_prehash(&digest(self.hash, message), &sig)
                    .map_err(|_| InvalidSignature)
            }
            Kind::EcdsaP384 => {
                let key = p384::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
                    .map_err(|_| InvalidSignature)?;
                let sig =
                    p384::ecdsa::Signature::from_der(signature).map_err(|_| InvalidSignature)?;
                key.verify_prehash(&digest(self.hash, message), &sig)
                    .map_err(|_| InvalidSignature)
            }
            Kind::RsaPkcs1 | Kind::RsaPss => {
                use rsa::pkcs1::DecodeRsaPublicKey;
                use rsa::traits::PublicKeyParts;
                let key =
                    rsa::RsaPublicKey::from_pkcs1_der(public_key).map_err(|_| InvalidSignature)?;
                let bits = key.n().bits();
                if !(2048..=8192).contains(&bits) {
                    return Err(InvalidSignature);
                }
                let hashed = digest(self.hash, message);
                let r = match (&self.kind, self.hash) {
                    (Kind::RsaPkcs1, H::S256) => {
                        key.verify(rsa::Pkcs1v15Sign::new::<Sha256>(), &hashed, signature)
                    }
                    (Kind::RsaPkcs1, H::S384) => {
                        key.verify(rsa::Pkcs1v15Sign::new::<Sha384>(), &hashed, signature)
                    }
                    (Kind::RsaPkcs1, H::S512) => {
                        key.verify(rsa::Pkcs1v15Sign::new::<Sha512>(), &hashed, signature)
                    }
                    (_, H::S256) => key.verify(rsa::Pss::new::<Sha256>(), &hashed, signature),
                    (_, H::S384) => key.verify(rsa::Pss::new::<Sha384>(), &hashed, signature),
                    (_, H::S512) => key.verify(rsa::Pss::new::<Sha512>(), &hashed, signature),
                };
                r.map_err(|_| InvalidSignature)
            }
            Kind::Ed25519 => {
                let pk: [u8; 32] = public_key.try_into().map_err(|_| InvalidSignature)?;
                let key =
                    ed25519_dalek::VerifyingKey::from_bytes(&pk).map_err(|_| InvalidSignature)?;
                let sig = ed25519_dalek::Signature::from_slice(signature)
                    .map_err(|_| InvalidSignature)?;
                key.verify_strict(message, &sig)
                    .map_err(|_| InvalidSignature)
            }
        }
    }
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        self.pk
    }
    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        self.sig
    }
}

macro_rules! alg {
    ($name:ident, $kind:ident, $hash:ident, $pk:ident, $sig:ident) => {
        pub static $name: Alg = Alg {
            kind: Kind::$kind,
            hash: H::$hash,
            pk: alg_id::$pk,
            sig: alg_id::$sig,
        };
    };
}

alg!(ECDSA_P256_SHA256, EcdsaP256, S256, ECDSA_P256, ECDSA_SHA256);
alg!(ECDSA_P256_SHA384, EcdsaP256, S384, ECDSA_P256, ECDSA_SHA384);
alg!(ECDSA_P384_SHA256, EcdsaP384, S256, ECDSA_P384, ECDSA_SHA256);
alg!(ECDSA_P384_SHA384, EcdsaP384, S384, ECDSA_P384, ECDSA_SHA384);
alg!(
    RSA_PKCS1_SHA256,
    RsaPkcs1,
    S256,
    RSA_ENCRYPTION,
    RSA_PKCS1_SHA256
);
alg!(
    RSA_PKCS1_SHA384,
    RsaPkcs1,
    S384,
    RSA_ENCRYPTION,
    RSA_PKCS1_SHA384
);
alg!(
    RSA_PKCS1_SHA512,
    RsaPkcs1,
    S512,
    RSA_ENCRYPTION,
    RSA_PKCS1_SHA512
);
alg!(RSA_PSS_SHA256, RsaPss, S256, RSA_ENCRYPTION, RSA_PSS_SHA256);
alg!(RSA_PSS_SHA384, RsaPss, S384, RSA_ENCRYPTION, RSA_PSS_SHA384);
alg!(RSA_PSS_SHA512, RsaPss, S512, RSA_ENCRYPTION, RSA_PSS_SHA512);
alg!(ED25519, Ed25519, S512, ED25519, ED25519);

pub static ALGORITHMS: WebPkiSupportedAlgorithms = WebPkiSupportedAlgorithms {
    all: &[
        &ECDSA_P256_SHA256,
        &ECDSA_P256_SHA384,
        &ECDSA_P384_SHA256,
        &ECDSA_P384_SHA384,
        &ED25519,
        &RSA_PSS_SHA256,
        &RSA_PSS_SHA384,
        &RSA_PSS_SHA512,
        &RSA_PKCS1_SHA256,
        &RSA_PKCS1_SHA384,
        &RSA_PKCS1_SHA512,
    ],
    mapping: &[
        (
            SignatureScheme::ECDSA_NISTP384_SHA384,
            &[&ECDSA_P384_SHA384, &ECDSA_P256_SHA384],
        ),
        (
            SignatureScheme::ECDSA_NISTP256_SHA256,
            &[&ECDSA_P256_SHA256, &ECDSA_P384_SHA256],
        ),
        (SignatureScheme::ED25519, &[&ED25519]),
        (SignatureScheme::RSA_PSS_SHA512, &[&RSA_PSS_SHA512]),
        (SignatureScheme::RSA_PSS_SHA384, &[&RSA_PSS_SHA384]),
        (SignatureScheme::RSA_PSS_SHA256, &[&RSA_PSS_SHA256]),
        (SignatureScheme::RSA_PKCS1_SHA512, &[&RSA_PKCS1_SHA512]),
        (SignatureScheme::RSA_PKCS1_SHA384, &[&RSA_PKCS1_SHA384]),
        (SignatureScheme::RSA_PKCS1_SHA256, &[&RSA_PKCS1_SHA256]),
    ],
};
