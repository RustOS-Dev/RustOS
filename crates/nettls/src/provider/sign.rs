//! Signing with ECDSA P-256/P-384 PKCS#8 keys (server side in tests;
//! client certificates).

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use pki_types::PrivateKeyDer;
use rustls::sign::{Signer, SigningKey};
use rustls::{Error, SignatureAlgorithm, SignatureScheme};

#[derive(Debug, Clone)]
enum Key {
    P256(p256::ecdsa::SigningKey),
    P384(p384::ecdsa::SigningKey),
}

#[derive(Debug)]
struct EcdsaKey(Key);

pub fn load(der: PrivateKeyDer<'static>) -> Result<Arc<dyn SigningKey>, Error> {
    use p256::pkcs8::DecodePrivateKey;
    let PrivateKeyDer::Pkcs8(p) = der else {
        return Err(Error::General(
            "only PKCS#8 ECDSA keys are supported".into(),
        ));
    };
    let bytes = p.secret_pkcs8_der();
    if let Ok(k) = p256::ecdsa::SigningKey::from_pkcs8_der(bytes) {
        return Ok(Arc::new(EcdsaKey(Key::P256(k))));
    }
    if let Ok(k) = p384::ecdsa::SigningKey::from_pkcs8_der(bytes) {
        return Ok(Arc::new(EcdsaKey(Key::P384(k))));
    }
    Err(Error::General("unsupported private key".into()))
}

impl EcdsaKey {
    fn scheme(&self) -> SignatureScheme {
        match self.0 {
            Key::P256(_) => SignatureScheme::ECDSA_NISTP256_SHA256,
            Key::P384(_) => SignatureScheme::ECDSA_NISTP384_SHA384,
        }
    }
}

impl SigningKey for EcdsaKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        let s = self.scheme();
        offered
            .contains(&s)
            .then(|| Box::new(EcdsaSigner(self.0.clone(), s)) as Box<dyn Signer>)
    }
    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }
}

#[derive(Debug)]
struct EcdsaSigner(Key, SignatureScheme);

impl Signer for EcdsaSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        use signature::RandomizedSigner;
        let mut rng = super::Rng;
        Ok(match &self.0 {
            Key::P256(k) => {
                let s: p256::ecdsa::Signature = k
                    .try_sign_with_rng(&mut rng, message)
                    .map_err(|_| Error::General("sign".into()))?;
                s.to_der().as_bytes().to_vec()
            }
            Key::P384(k) => {
                let s: p384::ecdsa::Signature = k
                    .try_sign_with_rng(&mut rng, message)
                    .map_err(|_| Error::General("sign".into()))?;
                s.to_der().as_bytes().to_vec()
            }
        })
    }
    fn scheme(&self) -> SignatureScheme {
        self.1
    }
}
