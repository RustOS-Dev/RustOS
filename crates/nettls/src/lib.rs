//! TLS client for RustOS userland (`wget`, `browse`).
//!
//! rustls (TLS 1.2 and 1.3) with a pure-Rust RustCrypto [`provider`],
//! trust anchors from a PEM CA bundle, certificate checks via webpki, and
//! a blocking [`TlsStream`] that drives rustls's unbuffered API over any
//! byte [`Transport`] (a TCP socket in RustOS, memory pipes in tests).
//!
//! The platform supplies randomness ([`provider::set_random_source`]) and
//! wall-clock time ([`set_time_source`]).

#![no_std]

extern crate alloc;

pub mod provider;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};

pub use pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
pub use rustls;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::UnbufferedClientConnection;
use rustls::crypto::{
    verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms,
};
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};

// ---------------------------------------------------------------------------
// Platform hooks
// ---------------------------------------------------------------------------

static TIME: AtomicUsize = AtomicUsize::new(0);

/// Install the wall-clock source (Unix seconds).
pub fn set_time_source(f: fn() -> u64) {
    TIME.store(f as usize, Ordering::SeqCst);
}

fn now_secs() -> Option<u64> {
    let p = TIME.load(Ordering::SeqCst);
    if p == 0 {
        return None;
    }
    // SAFETY: only `set_time_source` stores a `fn() -> u64` here.
    let f: fn() -> u64 = unsafe { core::mem::transmute(p) };
    Some(f())
}

#[derive(Debug)]
struct Clock;

impl rustls::time_provider::TimeProvider for Clock {
    fn current_time(&self) -> Option<UnixTime> {
        now_secs().map(|s| UnixTime::since_unix_epoch(core::time::Duration::from_secs(s)))
    }
}

// ---------------------------------------------------------------------------
// PEM
// ---------------------------------------------------------------------------

fn b64val(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    } as u32)
}

/// Decode base64, ignoring whitespace; stops at padding.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s.as_bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            break;
        }
        acc = acc << 6 | b64val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// All PEM blocks with the given label (e.g. `CERTIFICATE`).
pub fn pem_blocks(text: &str, label: &str) -> Vec<Vec<u8>> {
    let begin = format!("-----BEGIN {}-----", label);
    let end = format!("-----END {}-----", label);
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(&begin) {
        let after = &rest[i + begin.len()..];
        let Some(j) = after.find(&end) else { break };
        if let Some(der) = base64_decode(&after[..j]) {
            out.push(der);
        }
        rest = &after[j + end.len()..];
    }
    out
}

pub fn pem_certificates(text: &str) -> Vec<CertificateDer<'static>> {
    pem_blocks(text, "CERTIFICATE")
        .into_iter()
        .map(CertificateDer::from)
        .collect()
}

/// First PKCS#8 private key in a PEM text.
pub fn pem_private_key(text: &str) -> Option<PrivateKeyDer<'static>> {
    pem_blocks(text, "PRIVATE KEY")
        .into_iter()
        .next()
        .map(|d| PrivateKeyDer::Pkcs8(d.into()))
}

/// Trust anchors from a CA bundle; returns the store and the number of
/// certificates that were rejected.
pub fn root_store(pem: &str) -> (RootCertStore, usize) {
    let mut store = RootCertStore::empty();
    let (_, bad) = store.add_parsable_certificates(pem_certificates(pem));
    (store, bad)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

pub fn crypto_provider() -> Arc<CryptoProvider> {
    Arc::new(provider::provider())
}

/// Accepts any certificate (still checks handshake signatures, so the
/// connection is encrypted to whoever holds the presented key).
#[derive(Debug)]
struct NoVerify(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// Which protocol versions to offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Versions {
    Both,
    Tls12Only,
    Tls13Only,
}

/// Client configuration. `roots = None` disables certificate checks
/// (`wget -k`, or after the user accepted an untrusted certificate).
pub fn client_config(
    roots: Option<Arc<RootCertStore>>,
    versions: Versions,
) -> Result<Arc<ClientConfig>, TlsError> {
    let provider = crypto_provider();
    let algs = provider.signature_verification_algorithms;
    let vers: &[&rustls::SupportedProtocolVersion] = match versions {
        Versions::Both => &[&rustls::version::TLS13, &rustls::version::TLS12],
        Versions::Tls12Only => &[&rustls::version::TLS12],
        Versions::Tls13Only => &[&rustls::version::TLS13],
    };
    let builder = ClientConfig::builder_with_details(provider, Arc::new(Clock))
        .with_protocol_versions(vers)
        .map_err(TlsError::Tls)?;
    let mut cfg = match roots {
        Some(r) => builder.with_root_certificates(r).with_no_client_auth(),
        None => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(algs)))
            .with_no_client_auth(),
    };
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum TlsError {
    Io(String),
    Tls(rustls::Error),
}

impl TlsError {
    /// The server's certificate was rejected (the user may choose to
    /// continue anyway).
    pub fn is_certificate_error(&self) -> bool {
        matches!(self, TlsError::Tls(rustls::Error::InvalidCertificate(_)))
    }
}

fn cert_reason(e: &CertificateError) -> String {
    match e {
        CertificateError::UnknownIssuer => "issued by an unknown or untrusted authority".into(),
        CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
            "expired (check the clock with 'date'; fix it with 'ntpdate')".into()
        }
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
            "not valid yet (check the clock with 'date')".into()
        }
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
            "issued for a different host name".into()
        }
        CertificateError::Revoked => "revoked".into(),
        CertificateError::BadSignature => "has a bad signature".into(),
        CertificateError::BadEncoding => "is malformed".into(),
        other => format!("rejected ({:?})", other),
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TlsError::Io(s) => write!(f, "{}", s),
            TlsError::Tls(rustls::Error::InvalidCertificate(c)) => {
                write!(f, "server certificate {}", cert_reason(c))
            }
            TlsError::Tls(rustls::Error::AlertReceived(a)) => {
                write!(f, "TLS alert from server: {:?}", a)
            }
            TlsError::Tls(rustls::Error::PeerIncompatible(p)) => {
                write!(f, "server is incompatible: {:?}", p)
            }
            TlsError::Tls(e) => write!(f, "TLS error: {}", e),
        }
    }
}

// ---------------------------------------------------------------------------
// Streams
// ---------------------------------------------------------------------------

/// A reliable byte stream (e.g. a TCP socket).
pub trait Transport {
    /// Read some bytes; `Ok(0)` at end of stream.
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    fn send_all(&mut self, data: &[u8]) -> Result<(), String>;
}

enum Step {
    /// Made progress; process again.
    Again,
    /// Need more bytes from the peer.
    NeedData,
    /// Handshake done and all received records consumed.
    Idle,
    Closed,
}

/// A TLS client connection over a [`Transport`].
pub struct TlsStream<T: Transport> {
    conn: UnbufferedClientConnection,
    io: T,
    inbuf: Vec<u8>,
    in_used: usize,
    outbuf: Vec<u8>,
    out_used: usize,
    plain: Vec<u8>,
    plain_pos: usize,
    eof: bool,
}

const RECORD: usize = 16 * 1024 + 2048;

impl<T: Transport> TlsStream<T> {
    /// Start a connection to `host` and complete the handshake.
    pub fn connect(config: Arc<ClientConfig>, host: &str, io: T) -> Result<TlsStream<T>, TlsError> {
        let name = ServerName::try_from(host.to_string())
            .map_err(|_| TlsError::Io(format!("invalid server name '{}'", host)))?;
        let conn = UnbufferedClientConnection::new(config, name).map_err(TlsError::Tls)?;
        let mut s = TlsStream {
            conn,
            io,
            inbuf: vec![0u8; RECORD],
            in_used: 0,
            outbuf: vec![0u8; RECORD],
            out_used: 0,
            plain: Vec::new(),
            plain_pos: 0,
            eof: false,
        };
        loop {
            match s.process(None)?.0 {
                Step::Again => {}
                Step::NeedData => s.fill()?,
                Step::Idle => return Ok(s),
                Step::Closed => {
                    return Err(TlsError::Io(
                        "connection closed during the TLS handshake".into(),
                    ))
                }
            }
        }
    }

    /// Negotiated protocol version and cipher suite (for `-v` output).
    pub fn describe(&self) -> String {
        let v = self
            .conn
            .protocol_version()
            .map(|v| format!("{:?}", v))
            .unwrap_or_default();
        let c = self
            .conn
            .negotiated_cipher_suite()
            .map(|c| format!("{:?}", c.suite()))
            .unwrap_or_default();
        format!("{} {}", v, c)
    }

    pub fn transport(&mut self) -> &mut T {
        &mut self.io
    }

    /// Read more TLS bytes from the peer.
    fn fill(&mut self) -> Result<(), TlsError> {
        if self.in_used == self.inbuf.len() {
            self.inbuf.resize(self.inbuf.len() * 2, 0);
        }
        let n = self
            .io
            .recv(&mut self.inbuf[self.in_used..])
            .map_err(TlsError::Io)?;
        if n == 0 {
            self.eof = true;
            return Err(TlsError::Io("connection closed by peer".into()));
        }
        self.in_used += n;
        Ok(())
    }

    /// One `process_tls_records` round. With `write`, encrypt and send (a
    /// prefix of) it when the connection is writable; returns the number
    /// of plaintext bytes sent.
    fn process(&mut self, write: Option<&[u8]>) -> Result<(Step, usize), TlsError> {
        let status = self
            .conn
            .process_tls_records(&mut self.inbuf[..self.in_used]);
        let mut discard = status.discard;
        let mut sent = 0;
        let step = match status.state.map_err(TlsError::Tls)? {
            ConnectionState::ReadTraffic(mut st) => {
                while let Some(rec) = st.next_record() {
                    let rec = rec.map_err(TlsError::Tls)?;
                    discard += rec.discard;
                    self.plain.extend_from_slice(rec.payload);
                }
                Step::Again
            }
            ConnectionState::EncodeTlsData(mut st) => {
                loop {
                    match st.encode(&mut self.outbuf[self.out_used..]) {
                        Ok(n) => {
                            self.out_used += n;
                            break;
                        }
                        Err(EncodeError::InsufficientSize(e)) => {
                            let need = self.out_used + e.required_size;
                            self.outbuf.resize(need, 0);
                        }
                        Err(_) => break,
                    }
                }
                Step::Again
            }
            ConnectionState::TransmitTlsData(st) => {
                self.io
                    .send_all(&self.outbuf[..self.out_used])
                    .map_err(TlsError::Io)?;
                self.out_used = 0;
                st.done();
                Step::Again
            }
            ConnectionState::BlockedHandshake => Step::NeedData,
            ConnectionState::WriteTraffic(mut wt) => {
                match write {
                    Some(data) if !data.is_empty() => {
                        let chunk = &data[..data.len().min(16 * 1024)];
                        let n = loop {
                            match wt.encrypt(chunk, &mut self.outbuf) {
                                Ok(n) => break n,
                                Err(EncryptError::InsufficientSize(e)) => {
                                    self.outbuf.resize(e.required_size, 0)
                                }
                                Err(e) => {
                                    return Err(TlsError::Io(format!("TLS encrypt: {:?}", e)))
                                }
                            }
                        };
                        self.io.send_all(&self.outbuf[..n]).map_err(TlsError::Io)?;
                        sent = chunk.len();
                    }
                    Some(_) => {
                        // Empty write: send close_notify.
                        if let Ok(n) = wt.queue_close_notify(&mut self.outbuf) {
                            let _ = self.io.send_all(&self.outbuf[..n]);
                        }
                    }
                    None => {}
                }
                Step::Idle
            }
            ConnectionState::PeerClosed | ConnectionState::Closed => Step::Closed,
            _ => Step::Again,
        };
        if discard > 0 {
            self.inbuf.copy_within(discard..self.in_used, 0);
            self.in_used -= discard;
        }
        Ok((step, sent))
    }

    /// Read decrypted application data; `Ok(0)` at end of stream (a TCP
    /// close without close_notify also counts, as browsers accept).
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, TlsError> {
        loop {
            if self.plain_pos < self.plain.len() {
                let n = buf.len().min(self.plain.len() - self.plain_pos);
                buf[..n].copy_from_slice(&self.plain[self.plain_pos..self.plain_pos + n]);
                self.plain_pos += n;
                if self.plain_pos == self.plain.len() {
                    self.plain.clear();
                    self.plain_pos = 0;
                }
                return Ok(n);
            }
            if self.eof {
                return Ok(0);
            }
            match self.process(None)?.0 {
                Step::Again => {}
                Step::Closed => {
                    self.eof = true;
                }
                Step::NeedData | Step::Idle => match self.fill() {
                    Ok(()) => {}
                    Err(_) if self.eof => {}
                    Err(e) => return Err(e),
                },
            }
        }
    }

    pub fn write_all(&mut self, mut data: &[u8]) -> Result<(), TlsError> {
        while !data.is_empty() {
            match self.process(Some(data))? {
                (Step::Idle, n) => data = &data[n..],
                (Step::Again, _) => {}
                (Step::NeedData, _) => self.fill()?,
                (Step::Closed, _) => return Err(TlsError::Io("TLS connection closed".into())),
            }
        }
        Ok(())
    }

    /// Send close_notify.
    pub fn close(&mut self) {
        let _ = self.process(Some(&[]));
    }
}

#[cfg(test)]
mod tests;
