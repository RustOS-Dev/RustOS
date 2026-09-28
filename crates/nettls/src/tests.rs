extern crate std;

use super::*;
use rustls::{ServerConfig, ServerConnection};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

const RSA_CA: &str = include_str!("../tests/data/rsa-ca.pem");
const EC_CA: &str = include_str!("../tests/data/ec-ca.pem");
const OTHER_CA: &str = include_str!("../tests/data/other-ca.pem");
const SERVER_RSA_CA: &str = include_str!("../tests/data/server-rsa-ca.pem");
const SERVER_EC_CA: &str = include_str!("../tests/data/server-ec-ca.pem");
const SERVER_KEY: &str = include_str!("../tests/data/server.key");

/// 2030-01-01: inside the test certificates' validity.
const T_VALID: u64 = 1_893_456_000;

fn test_random(buf: &mut [u8]) -> bool {
    use std::cell::Cell;
    std::thread_local!(static S: Cell<u64> = Cell::new(0x9E37_79B9_7F4A_7C15 ^ std::process::id() as u64));
    S.with(|s| {
        for b in buf.iter_mut() {
            let mut x = s.get();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.set(x);
            *b = (x >> 32) as u8;
        }
    });
    true
}

// Per thread: tests run in parallel and only the client thread (the test
// thread) checks certificate validity.
std::thread_local!(static CLOCK: std::cell::Cell<u64> = const { std::cell::Cell::new(T_VALID) });
fn test_time() -> u64 {
    CLOCK.with(|c| c.get())
}
fn set_clock(t: u64) {
    CLOCK.with(|c| c.set(t));
}

fn setup() {
    provider::set_random_source(test_random);
    set_time_source(test_time);
}

/// One end of an in-memory byte pipe.
struct Pipe {
    tx: Sender<Vec<u8>>,
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
}

fn pipe_pair() -> (Pipe, Pipe) {
    let (a_tx, b_rx) = channel();
    let (b_tx, a_rx) = channel();
    (
        Pipe {
            tx: a_tx,
            rx: a_rx,
            pending: Vec::new(),
        },
        Pipe {
            tx: b_tx,
            rx: b_rx,
            pending: Vec::new(),
        },
    )
}

impl Transport for Pipe {
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if self.pending.is_empty() {
            match self.rx.recv() {
                Ok(d) => self.pending = d,
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
    fn send_all(&mut self, data: &[u8]) -> Result<(), String> {
        self.tx
            .send(data.to_vec())
            .map_err(|_| String::from("pipe closed"))
    }
}

impl std::io::Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        Transport::recv(self, buf).map_err(std::io::Error::other)
    }
}
impl std::io::Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.send_all(buf).map_err(std::io::Error::other)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Echo server: reads a request line "N\n" and replies with N bytes,
/// then echoes whatever else it receives until close_notify.
fn server(cfg: Arc<ServerConfig>, mut io: Pipe) -> Result<Vec<u8>, String> {
    let mut conn = ServerConnection::new(cfg).map_err(|e| format!("{e}"))?;
    conn.set_buffer_limit(None);
    let mut got = Vec::new();
    let mut replied = false;
    loop {
        while conn.wants_write() {
            conn.write_tls(&mut io).map_err(|e| format!("{e}"))?;
        }
        if conn.read_tls(&mut io).map_err(|e| format!("{e}"))? == 0 {
            return Ok(got);
        }
        let st = conn.process_new_packets().map_err(|e| format!("{e}"))?;
        let mut buf = vec![0u8; st.plaintext_bytes_to_read()];
        use std::io::Read;
        let n = conn.reader().read(&mut buf).unwrap_or(0);
        got.extend_from_slice(&buf[..n]);
        if !replied {
            if let Some(nl) = got.iter().position(|&b| b == b'\n') {
                let len: usize = std::str::from_utf8(&got[..nl]).unwrap().parse().unwrap();
                let body: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
                use std::io::Write;
                conn.writer().write_all(&body).unwrap();
                replied = true;
            }
        }
        if st.peer_has_closed() {
            conn.send_close_notify();
            while conn.wants_write() {
                if conn.write_tls(&mut io).is_err() {
                    break; // the client already hung up
                }
            }
            return Ok(got);
        }
    }
}

fn server_config(
    chain_pem: &str,
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> Arc<ServerConfig> {
    let certs = pem_certificates(chain_pem);
    let key = pem_private_key(SERVER_KEY).unwrap();
    Arc::new(
        ServerConfig::builder_with_provider(crypto_provider())
            .with_protocol_versions(versions)
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap(),
    )
}

/// Connect, fetch `n` bytes, send some data, close. Returns the client
/// description or the error.
fn exchange(
    client_cfg: Arc<ClientConfig>,
    server_cfg: Arc<ServerConfig>,
    host: &str,
    n: usize,
) -> Result<String, TlsError> {
    setup();
    let (c, s) = pipe_pair();
    let srv = thread::spawn(move || {
        setup();
        server(server_cfg, s)
    });
    let r = (|| {
        let mut t = TlsStream::connect(client_cfg, host, c)?;
        t.write_all(format!("{}\n", n).as_bytes())?;
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while got.len() < n {
            let k = t.read(&mut buf)?;
            if k == 0 {
                break;
            }
            got.extend_from_slice(&buf[..k]);
        }
        assert_eq!(got.len(), n);
        assert!(got.iter().enumerate().all(|(i, &b)| b == (i % 251) as u8));
        let big: Vec<u8> = (0..50_000u32).map(|i| i as u8).collect();
        t.write_all(&big)?;
        t.close();
        Ok(t.describe())
    })();
    let got = srv.join().unwrap();
    if r.is_ok() {
        let got = got.unwrap();
        assert_eq!(got.len(), format!("{}\n", n).len() + 50_000);
    }
    r
}

fn roots(pem: &str) -> Arc<RootCertStore> {
    let (r, bad) = root_store(pem);
    assert_eq!(bad, 0);
    Arc::new(r)
}

/// A strict client offering only `suite`.
fn with_suites(suite: rustls::CipherSuite) -> Arc<ClientConfig> {
    let mut p = provider::provider();
    p.cipher_suites.retain(|s| s.suite() == suite);
    let fresh = ClientConfig::builder_with_details(Arc::new(p), Arc::new(Clock))
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .unwrap()
        .with_root_certificates(roots(&(String::from(RSA_CA) + EC_CA)))
        .with_no_client_auth();
    Arc::new(fresh)
}

#[test]
fn pem_and_roots() {
    let certs = pem_certificates(&(String::from(RSA_CA) + EC_CA + "junk"));
    assert_eq!(certs.len(), 2);
    let (store, bad) = root_store(&(String::from(RSA_CA) + OTHER_CA));
    assert_eq!((store.len(), bad), (2, 0));
    assert!(pem_private_key(SERVER_KEY).is_some());
    assert_eq!(base64_decode("aGVs\nbG8=").unwrap(), b"hello");
}

#[test]
fn tls13_all_suites() {
    for suite in [
        rustls::CipherSuite::TLS13_AES_128_GCM_SHA256,
        rustls::CipherSuite::TLS13_AES_256_GCM_SHA384,
        rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    ] {
        let cfg = with_suites(suite);
        let sc = server_config(SERVER_RSA_CA, &[&rustls::version::TLS13]);
        let d = exchange(cfg, sc, "localhost", 70_000).unwrap();
        assert!(d.contains("TLSv1_3"), "{d}");
        assert!(d.contains(&format!("{:?}", suite)), "{d}");
    }
}

#[test]
fn tls12_all_ecdsa_suites() {
    for suite in [
        rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
        rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
        rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    ] {
        let cfg = with_suites(suite);
        let d = exchange(
            cfg,
            server_config(SERVER_EC_CA, &[&rustls::version::TLS12]),
            "test.local",
            200_000,
        )
        .unwrap();
        assert!(d.contains("TLSv1_2"), "{d}");
    }
}

#[test]
fn key_exchange_groups() {
    for g in [
        rustls::NamedGroup::X25519,
        rustls::NamedGroup::secp256r1,
        rustls::NamedGroup::secp384r1,
    ] {
        let mut p = provider::provider();
        p.kx_groups.retain(|k| k.name() == g);
        let cfg = ClientConfig::builder_with_details(Arc::new(p), Arc::new(Clock))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots(RSA_CA))
            .with_no_client_auth();
        exchange(
            Arc::new(cfg),
            server_config(SERVER_RSA_CA, &[&rustls::version::TLS13]),
            "localhost",
            10,
        )
        .unwrap();
    }
}

#[test]
fn verification() {
    let ok = client_config(Some(roots(&(String::from(RSA_CA) + EC_CA))), Versions::Both).unwrap();
    exchange(
        ok.clone(),
        server_config(SERVER_RSA_CA, &[&rustls::version::TLS13]),
        "localhost",
        5,
    )
    .unwrap();
    exchange(
        ok.clone(),
        server_config(SERVER_EC_CA, &[&rustls::version::TLS12]),
        "127.0.0.1",
        5,
    )
    .unwrap();

    let untrusted = client_config(Some(roots(OTHER_CA)), Versions::Both).unwrap();
    let e = exchange(
        untrusted,
        server_config(SERVER_RSA_CA, &[&rustls::version::TLS13]),
        "localhost",
        5,
    )
    .unwrap_err();
    assert!(e.is_certificate_error());
    assert!(
        format!("{e}").contains("unknown or untrusted authority"),
        "{e}"
    );

    let e = exchange(
        ok.clone(),
        server_config(SERVER_RSA_CA, &[&rustls::version::TLS13]),
        "example.com",
        5,
    )
    .unwrap_err();
    assert!(format!("{e}").contains("different host name"), "{e}");

    set_clock(7_258_118_400); // 2200
    let e = exchange(
        ok.clone(),
        server_config(SERVER_RSA_CA, &[&rustls::version::TLS13]),
        "localhost",
        5,
    )
    .unwrap_err();
    set_clock(1_577_836_800); // 2020
    let e2 = exchange(
        ok.clone(),
        server_config(SERVER_RSA_CA, &[&rustls::version::TLS13]),
        "localhost",
        5,
    )
    .unwrap_err();
    set_clock(T_VALID);
    assert!(format!("{e}").contains("expired"), "{e}");
    assert!(format!("{e2}").contains("not valid yet"), "{e2}");

    // Insecure mode accepts anything but still completes the handshake.
    let insecure = client_config(None, Versions::Tls12Only).unwrap();
    exchange(
        insecure,
        server_config(
            SERVER_RSA_CA,
            &[&rustls::version::TLS12, &rustls::version::TLS13],
        ),
        "wrong.name",
        5,
    )
    .unwrap();
}

#[test]
fn version_mismatch_is_reported() {
    let only13 = client_config(None, Versions::Tls13Only).unwrap();
    let e = exchange(
        only13,
        server_config(SERVER_EC_CA, &[&rustls::version::TLS12]),
        "localhost",
        5,
    )
    .unwrap_err();
    assert!(matches!(e, TlsError::Tls(_) | TlsError::Io(_)), "{e}");
}
