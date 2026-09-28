//! Key exchange: X25519, secp256r1 and secp384r1 (ephemeral ECDH).

use super::{fill_random, Rng};
use alloc::boxed::Box;
use alloc::vec::Vec;
use rustls::crypto::{ActiveKeyExchange, SharedSecret, SupportedKxGroup};
use rustls::{Error, NamedGroup, PeerMisbehaved};

fn bad_share() -> Error {
    Error::PeerMisbehaved(PeerMisbehaved::InvalidKeyShare)
}

#[derive(Debug)]
pub struct X25519;

struct X25519Kx {
    secret: x25519_dalek::StaticSecret,
    public: [u8; 32],
}

impl SupportedKxGroup for X25519 {
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
        let mut sk = [0u8; 32];
        if !fill_random(&mut sk) {
            return Err(Error::FailedToGetRandomBytes);
        }
        let secret = x25519_dalek::StaticSecret::from(sk);
        let public = x25519_dalek::PublicKey::from(&secret).to_bytes();
        Ok(Box::new(X25519Kx { secret, public }))
    }
    fn name(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

impl ActiveKeyExchange for X25519Kx {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
        let peer: [u8; 32] = peer.try_into().map_err(|_| bad_share())?;
        let shared = self
            .secret
            .diffie_hellman(&x25519_dalek::PublicKey::from(peer));
        if !shared.was_contributory() {
            return Err(bad_share());
        }
        Ok(SharedSecret::from(&shared.as_bytes()[..]))
    }
    fn pub_key(&self) -> &[u8] {
        &self.public
    }
    fn group(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

macro_rules! nist_group {
    ($group:ident, $kx:ident, $curve:ident, $name:ident) => {
        #[derive(Debug)]
        pub struct $group;

        struct $kx {
            secret: $curve::ecdh::EphemeralSecret,
            public: Vec<u8>,
        }

        impl SupportedKxGroup for $group {
            fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
                let secret = $curve::ecdh::EphemeralSecret::random(&mut Rng);
                let public = $curve::EncodedPoint::from(secret.public_key())
                    .as_bytes()
                    .to_vec();
                Ok(Box::new($kx { secret, public }))
            }
            fn name(&self) -> NamedGroup {
                NamedGroup::$name
            }
        }

        impl ActiveKeyExchange for $kx {
            fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
                let pk = $curve::PublicKey::from_sec1_bytes(peer).map_err(|_| bad_share())?;
                let shared = self.secret.diffie_hellman(&pk);
                Ok(SharedSecret::from(&shared.raw_secret_bytes()[..]))
            }
            fn pub_key(&self) -> &[u8] {
                &self.public
            }
            fn group(&self) -> NamedGroup {
                NamedGroup::$name
            }
        }
    };
}

nist_group!(Secp256r1, P256Kx, p256, secp256r1);
nist_group!(Secp384r1, P384Kx, p384, secp384r1);

pub static ALL: &[&dyn SupportedKxGroup] = &[&X25519, &Secp256r1, &Secp384r1];
