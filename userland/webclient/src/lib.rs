//! HTTP(S) client glue shared by `wget`, `netcheck`, `wifi` and `browse`.
//!
//! * [`Net`]: an `httpc` connector that opens TCP connections (IPv6 or
//!   IPv4, every resolved address in turn) and wraps them in TLS for
//!   `https` (rustls via `nettls`, certificates checked against the CA
//!   bundle unless disabled);
//! * CA bundle discovery, cookie-jar persistence;
//! * [`portal`]: captive-portal detection.

#![no_std]

extern crate alloc;

pub mod portal;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
pub use httpc;
pub use httpc::client::Client;
use httpc::client::{Connector, Stream};
use httpc::{Error, Url};
pub use nettls;
use nettls::{TlsError, TlsStream, Transport, Versions};
use rustos_rt::net::Socket;
use rustos_rt::{fs, process, time};

/// CA bundles, first match wins (storage partition first so users can
/// install their own).
pub const CA_BUNDLES: &[&str] = &[
    "/storage/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/cert.pem",
];

fn random(buf: &mut [u8]) -> bool {
    process::getrandom(buf);
    true
}

fn now() -> u64 {
    time::now()
}

/// Install the platform hooks for TLS. Call once at program start.
pub fn init() {
    nettls::provider::set_random_source(random);
    nettls::set_time_source(now);
}

/// Load trust anchors from the first CA bundle found.
pub fn load_roots() -> Option<(Arc<nettls::rustls::RootCertStore>, &'static str)> {
    for path in CA_BUNDLES {
        if let Ok(pem) = fs::read_to_string(path) {
            let (store, _) = nettls::root_store(&pem);
            if !store.is_empty() {
                return Some((Arc::new(store), path));
            }
        }
    }
    None
}

struct SockIo(Socket);

impl Transport for SockIo {
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        self.0.recv(buf).map_err(|e| match e.0 {
            11 | 110 => String::from("timed out"),
            _ => e.to_string(),
        })
    }
    fn send_all(&mut self, data: &[u8]) -> Result<(), String> {
        self.0.send_all(data).map_err(|e| e.to_string())
    }
}

struct Plain(Socket);

impl Stream for Plain {
    fn read(&mut self, buf: &mut [u8]) -> httpc::Result<usize> {
        self.0.recv(buf).map_err(|e| match e.0 {
            11 | 110 => Error::Timeout,
            _ => Error::Io(e.to_string()),
        })
    }
    fn write_all(&mut self, data: &[u8]) -> httpc::Result<()> {
        self.0.send_all(data).map_err(|e| Error::Io(e.to_string()))
    }
    fn fd(&self) -> i32 {
        self.0.fd()
    }
    fn set_timeout(&mut self, ms: u64) {
        let _ = self.0.set_timeout(ms);
    }
}

struct Tls(TlsStream<SockIo>);

impl Stream for Tls {
    fn read(&mut self, buf: &mut [u8]) -> httpc::Result<usize> {
        self.0.read(buf).map_err(|e| Error::Io(e.to_string()))
    }
    fn write_all(&mut self, data: &[u8]) -> httpc::Result<()> {
        self.0.write_all(data).map_err(|e| Error::Io(e.to_string()))
    }
    fn fd(&self) -> i32 {
        self.0.transport_ref().0.fd()
    }
    fn set_timeout(&mut self, ms: u64) {
        let _ = self.0.transport().0.set_timeout(ms);
    }
}

impl Drop for Tls {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Connector for `httpc::Client`.
pub struct Net {
    /// Connect/read timeout per operation (ms).
    pub timeout_ms: u64,
    /// Accept any server certificate (`wget -k`).
    pub insecure: bool,
    /// Hosts whose certificate the user chose to accept anyway.
    pub trusted_hosts: Vec<String>,
    pub versions: Versions,
    /// Print connection details to stderr.
    pub verbose: bool,
    roots: Option<Option<(Arc<nettls::rustls::RootCertStore>, &'static str)>>,
    strict: Option<Arc<nettls::rustls::ClientConfig>>,
    loose: Option<Arc<nettls::rustls::ClientConfig>>,
    /// The last TLS failure (to tell certificate problems apart).
    pub last_tls_error: Option<TlsError>,
    /// Protocol and cipher of the last TLS connection.
    pub last_tls: String,
}

impl Default for Net {
    fn default() -> Net {
        Net::new()
    }
}

impl Net {
    pub fn new() -> Net {
        init();
        Net {
            timeout_ms: 20_000,
            insecure: false,
            trusted_hosts: Vec::new(),
            versions: Versions::Both,
            verbose: false,
            roots: None,
            strict: None,
            loose: None,
            last_tls_error: None,
            last_tls: String::new(),
        }
    }

    /// The CA bundle in use (loaded on first HTTPS connection).
    pub fn ca_bundle(&mut self) -> Option<&'static str> {
        self.roots
            .get_or_insert_with(load_roots)
            .as_ref()
            .map(|r| r.1)
    }

    fn config(&mut self, host: &str) -> Result<Arc<nettls::rustls::ClientConfig>, Error> {
        let trust_any = self.insecure || self.trusted_hosts.iter().any(|h| h == host);
        let map = |e: TlsError| Error::Io(e.to_string());
        if trust_any {
            if self.loose.is_none() {
                self.loose = Some(nettls::client_config(None, self.versions).map_err(map)?);
            }
            return Ok(self.loose.clone().unwrap());
        }
        if self.strict.is_none() {
            let Some((roots, _)) = self.roots.get_or_insert_with(load_roots).clone() else {
                return Err(Error::Io(String::from(
                    "no CA certificates (install /etc/ssl/certs/ca-certificates.crt or use -k)",
                )));
            };
            self.strict = Some(nettls::client_config(Some(roots), self.versions).map_err(map)?);
        }
        Ok(self.strict.clone().unwrap())
    }
}

impl Connector for Net {
    fn connect(&mut self, url: &Url) -> httpc::Result<Box<dyn Stream>> {
        let host = url.host_str().to_string();
        let port = url.port_or_default().unwrap_or(80);
        let (sock, ip) = rustos_rt::net::connect_host(&host, port, self.timeout_ms)
            .map_err(|e| Error::Io(format!("{}:{}: {}", host, port, e)))?;
        let _ = sock.set_timeout(self.timeout_ms);
        if self.verbose {
            rustos_rt::eprintln!("* connected to {} ({}) port {}", host, ip, port);
        }
        if url.scheme != "https" {
            return Ok(Box::new(Plain(sock)));
        }
        let cfg = self.config(&host)?;
        self.last_tls_error = None;
        match TlsStream::connect(cfg, &host, SockIo(sock)) {
            Ok(t) => {
                self.last_tls = t.describe();
                if self.verbose {
                    rustos_rt::eprintln!("* TLS: {}", self.last_tls);
                }
                Ok(Box::new(Tls(t)))
            }
            Err(e) => {
                let msg = e.to_string();
                self.last_tls_error = Some(e);
                Err(Error::Io(msg))
            }
        }
    }

    fn now(&self) -> u64 {
        time::now()
    }
}

/// Default cookie file: on the storage partition when mounted.
pub fn cookie_file() -> &'static str {
    if fs::is_dir("/storage/etc") {
        "/storage/etc/cookies.txt"
    } else {
        "/tmp/cookies.txt"
    }
}

pub fn load_cookies(jar: &mut httpc::cookie::Jar, path: &str) {
    if let Ok(t) = fs::read_to_string(path) {
        jar.load(&t, time::now());
    }
}

pub fn save_cookies(jar: &httpc::cookie::Jar, path: &str, session: bool) -> bool {
    fs::write(path, jar.save(session).as_bytes()).is_ok()
}

/// A client with the RustOS user agent.
pub fn client(net: Net) -> Client<Net> {
    let mut c = Client::new(net);
    c.opts.user_agent = String::from("RustOS/1.0 (x86_64)");
    c
}
