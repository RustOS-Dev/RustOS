//! TLS 1.3 client transport for wget (embedded-tls), with X.509 chain
//! verification against the system CA bundle.

use embedded_tls::blocking::TlsConnection;
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CertificateRef, CertificateVerifyRef, CryptoProvider, NoVerify, TlsClock,
    TlsConfig, TlsContext, TlsError, TlsVerifier,
};
use rustos_rt::net::Socket;
use rustos_rt::prelude::*;

pub const CA_BUNDLES: &[&str] = &[
    "/storage/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/cert.pem",
];

const RECORD_BUF: usize = 16640;
/// Largest leaf certificate kept for the CertificateVerify check.
const CERT_SIZE: usize = 8192;

type Suite = Aes128GcmSha256;

// ---------------------------------------------------------------------------
// Plumbing: socket I/O, randomness, clock
// ---------------------------------------------------------------------------

pub struct SockIo(pub Socket);

#[derive(Debug)]
pub struct IoErr;

impl core::fmt::Display for IoErr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("socket error")
    }
}

impl core::error::Error for IoErr {}

impl embedded_io::Error for IoErr {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

impl embedded_io::ErrorType for SockIo {
    type Error = IoErr;
}

impl embedded_io::Read for SockIo {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoErr> {
        self.0.recv(buf).map_err(|_| IoErr)
    }
}

impl embedded_io::Write for SockIo {
    fn write(&mut self, buf: &[u8]) -> Result<usize, IoErr> {
        self.0.send(buf).map_err(|_| IoErr)
    }
    fn flush(&mut self) -> Result<(), IoErr> {
        Ok(())
    }
}

pub struct Rng;

impl rand_core::RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        rustos_rt::process::getrandom(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        rustos_rt::process::getrandom(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rustos_rt::process::getrandom(dest);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        rustos_rt::process::getrandom(dest);
        Ok(())
    }
}

impl rand_core::CryptoRng for Rng {}

pub struct Clock;

impl TlsClock for Clock {
    fn now() -> Option<u64> {
        let t = rustos_rt::time::now();
        // Before the clock is set (1970) validity checks would fail anyway.
        (t > 1_600_000_000).then_some(t)
    }
}

// ---------------------------------------------------------------------------
// CA bundle and DER helpers
// ---------------------------------------------------------------------------

fn b64_val(c: u8) -> Option<u8> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

fn base64_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in s.as_bytes() {
        let Some(v) = b64_val(c) else { continue };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// DER certificates from a PEM bundle.
pub fn load_pem(text: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line == "-----BEGIN CERTIFICATE-----" {
            cur = Some(String::new());
        } else if line == "-----END CERTIFICATE-----" {
            if let Some(b) = cur.take() {
                out.push(base64_decode(&b));
            }
        } else if let Some(b) = cur.as_mut() {
            b.push_str(line);
        }
    }
    out
}

/// One DER TLV at `d[0..]`: (tag, header length, content length).
fn tlv(d: &[u8]) -> Option<(u8, usize, usize)> {
    let tag = *d.first()?;
    let l0 = *d.get(1)? as usize;
    if l0 < 0x80 {
        return Some((tag, 2, l0));
    }
    let n = l0 & 0x7F;
    if n == 0 || n > 4 {
        return None;
    }
    let mut len = 0usize;
    for i in 0..n {
        len = (len << 8) | *d.get(2 + i)? as usize;
    }
    Some((tag, 2 + n, len))
}

/// Raw DER of the issuer and subject Names of a certificate.
fn issuer_subject(cert: &[u8]) -> Option<(&[u8], &[u8])> {
    let (_, h, _) = tlv(cert)?; // Certificate SEQUENCE
    let tbs = &cert[h..];
    let (_, th, tlen) = tlv(tbs)?; // TBSCertificate SEQUENCE
    let mut p = &tbs[th..th + tlen];
    let mut fields: Vec<&[u8]> = Vec::new();
    while !p.is_empty() && fields.len() < 7 {
        let (tag, hh, l) = tlv(p)?;
        let whole = p.get(..hh + l)?;
        if !(fields.is_empty() && tag == 0xA0) {
            fields.push(whole); // skip the optional [0] version
        }
        p = &p[hh + l..];
    }
    // serial, signature algorithm, issuer, validity, subject
    Some((fields.get(2)?, fields.get(4)?))
}

// ---------------------------------------------------------------------------
// Verifier over a CA bundle
// ---------------------------------------------------------------------------

pub struct BundleVerifier {
    cas: Vec<&'static [u8]>,
    host: String,
    inner: Option<CertVerifier<'static, Suite, Clock, CERT_SIZE>>,
}

impl BundleVerifier {
    pub fn new(cas: Vec<Vec<u8>>) -> BundleVerifier {
        BundleVerifier {
            cas: cas.into_iter().map(|c| &*Box::leak(c.into_boxed_slice())).collect(),
            host: String::new(),
            inner: None,
        }
    }

    fn ca_for(&self, issuer: &[u8]) -> Option<&'static [u8]> {
        self.cas
            .iter()
            .copied()
            .find(|ca| issuer_subject(ca).is_some_and(|(_, subject)| subject == issuer))
    }
}

impl TlsVerifier<Suite> for BundleVerifier {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), TlsError> {
        self.host = hostname.into();
        Ok(())
    }

    fn verify_certificate(
        &mut self,
        transcript: &<Suite as embedded_tls::TlsCipherSuite>::Hash,
        mut cert: CertificateRef,
    ) -> Result<(), TlsError> {
        // Find the trust anchor for the top of the chain; drop trailing
        // cross-signed certificates whose issuer we do not have.
        let ca = loop {
            let Some(embedded_tls::CertificateEntryRef::X509(top)) = cert.entries.last() else {
                return Err(TlsError::InvalidCertificate);
            };
            let (issuer, _) = issuer_subject(top).ok_or(TlsError::InvalidCertificate)?;
            if let Some(ca) = self.ca_for(issuer) {
                break ca;
            }
            if cert.entries.len() <= 1 {
                eprintln!("wget: certificate issuer is not a trusted CA");
                return Err(TlsError::InvalidCertificate);
            }
            cert.entries.pop();
        };
        let mut v = CertVerifier::<Suite, Clock, CERT_SIZE>::new(Certificate::X509(ca));
        v.set_hostname_verification(&self.host)?;
        v.verify_certificate(transcript, cert)?;
        self.inner = Some(v);
        Ok(())
    }

    fn verify_signature(&mut self, verify: CertificateVerifyRef) -> Result<(), TlsError> {
        self.inner.as_mut().ok_or(TlsError::InvalidCertificate)?.verify_signature(verify)
    }
}

/// Certificate checking: against the CA bundle, or none (`-k`).
pub enum Verifier {
    Bundle(BundleVerifier),
    None(NoVerify),
}

impl TlsVerifier<Suite> for Verifier {
    fn set_hostname_verification(&mut self, h: &str) -> Result<(), TlsError> {
        match self {
            Verifier::Bundle(v) => v.set_hostname_verification(h),
            Verifier::None(v) => <NoVerify as TlsVerifier<Suite>>::set_hostname_verification(v, h),
        }
    }
    fn verify_certificate(
        &mut self,
        t: &<Suite as embedded_tls::TlsCipherSuite>::Hash,
        c: CertificateRef,
    ) -> Result<(), TlsError> {
        match self {
            Verifier::Bundle(v) => v.verify_certificate(t, c),
            Verifier::None(v) => <NoVerify as TlsVerifier<Suite>>::verify_certificate(v, t, c),
        }
    }
    fn verify_signature(&mut self, s: CertificateVerifyRef) -> Result<(), TlsError> {
        match self {
            Verifier::Bundle(v) => v.verify_signature(s),
            Verifier::None(v) => <NoVerify as TlsVerifier<Suite>>::verify_signature(v, s),
        }
    }
}

pub struct Provider {
    rng: Rng,
    verifier: Verifier,
}

impl CryptoProvider for Provider {
    type CipherSuite = Suite;
    type Signature = &'static [u8];

    fn rng(&mut self) -> impl rand_core::CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Suite>, TlsError> {
        Ok(&mut self.verifier)
    }
}

pub type Tls = TlsConnection<'static, SockIo, Suite>;

/// Open a TLS session over `s`. `cas` = None disables verification.
pub fn connect(s: Socket, host: &str, cas: Option<Vec<Vec<u8>>>) -> Result<Tls, String> {
    let rbuf: &'static mut [u8] = Box::leak(vec![0u8; RECORD_BUF].into_boxed_slice());
    let wbuf: &'static mut [u8] = Box::leak(vec![0u8; RECORD_BUF].into_boxed_slice());
    let host: &'static str = Box::leak(String::from(host).into_boxed_str());
    let config: &'static TlsConfig<'static> =
        Box::leak(Box::new(TlsConfig::new().with_server_name(host)));
    let mut conn = TlsConnection::new(SockIo(s), rbuf, wbuf);
    let provider = Provider {
        rng: Rng,
        verifier: match cas {
            Some(c) => Verifier::Bundle(BundleVerifier::new(c)),
            None => Verifier::None(NoVerify),
        },
    };
    conn.open(TlsContext::new(config, provider))
        .map_err(|e| format!("TLS handshake failed: {:?}", e))?;
    Ok(conn)
}

/// The CA bundle, if one is installed.
pub fn system_cas() -> Option<Vec<Vec<u8>>> {
    CA_BUNDLES.iter().find_map(|p| {
        let text = rustos_rt::fs::read_to_string(p).ok()?;
        let v = load_pem(&text);
        (!v.is_empty()).then_some(v)
    })
}
