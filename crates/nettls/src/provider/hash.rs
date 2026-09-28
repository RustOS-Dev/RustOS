use alloc::boxed::Box;
use rustls::crypto::hash::{Context, Hash, HashAlgorithm, Output};
use sha2::Digest;

pub struct Sha<D>(core::marker::PhantomData<D>, HashAlgorithm);

pub static SHA256: Sha<sha2::Sha256> = Sha(core::marker::PhantomData, HashAlgorithm::SHA256);
pub static SHA384: Sha<sha2::Sha384> = Sha(core::marker::PhantomData, HashAlgorithm::SHA384);

// PhantomData<D> is only a type marker.
unsafe impl<D> Sync for Sha<D> {}
unsafe impl<D> Send for Sha<D> {}

impl<D: Digest + Clone + Send + Sync + 'static> Hash for Sha<D> {
    fn start(&self) -> Box<dyn Context> {
        Box::new(Ctx(D::new()))
    }
    fn hash(&self, data: &[u8]) -> Output {
        Output::new(&D::digest(data))
    }
    fn output_len(&self) -> usize {
        <D as Digest>::output_size()
    }
    fn algorithm(&self) -> HashAlgorithm {
        self.1
    }
}

struct Ctx<D>(D);

impl<D: Digest + Clone + Send + Sync + 'static> Context for Ctx<D> {
    fn fork_finish(&self) -> Output {
        Output::new(&self.0.clone().finalize())
    }
    fn fork(&self) -> Box<dyn Context> {
        Box::new(Ctx(self.0.clone()))
    }
    fn finish(self: Box<Self>) -> Output {
        Output::new(&self.0.finalize())
    }
    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
}
