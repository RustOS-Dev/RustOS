use alloc::boxed::Box;
use hmac::{Mac, SimpleHmac};
use rustls::crypto::hmac::{Key, Tag};

/// HMAC over digest `D` (output length given explicitly for `const`).
pub struct Hmac<D>(core::marker::PhantomData<D>, usize);

impl<D> Hmac<D> {
    pub const fn new(out_len: usize) -> Hmac<D> {
        Hmac(core::marker::PhantomData, out_len)
    }
}

unsafe impl<D> Sync for Hmac<D> {}
unsafe impl<D> Send for Hmac<D> {}

impl<D> rustls::crypto::hmac::Hmac for Hmac<D>
where
    D: sha2::Digest + hmac::digest::core_api::BlockSizeUser + Clone + Send + Sync + 'static,
{
    fn with_key(&self, key: &[u8]) -> Box<dyn Key> {
        Box::new(HmacKey(
            <SimpleHmac<D> as hmac::digest::KeyInit>::new_from_slice(key)
                .expect("HMAC accepts any key length"),
            self.1,
        ))
    }
    fn hash_output_len(&self) -> usize {
        self.1
    }
}

struct HmacKey<D: sha2::Digest + hmac::digest::core_api::BlockSizeUser>(SimpleHmac<D>, usize);

impl<D> Key for HmacKey<D>
where
    D: sha2::Digest + hmac::digest::core_api::BlockSizeUser + Clone + Send + Sync + 'static,
{
    fn sign_concat(&self, first: &[u8], middle: &[&[u8]], last: &[u8]) -> Tag {
        let mut m = self.0.clone();
        m.update(first);
        for x in middle {
            m.update(x);
        }
        m.update(last);
        Tag::new(&m.finalize().into_bytes())
    }
    fn tag_len(&self) -> usize {
        self.1
    }
}
