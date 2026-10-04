//! https listener whose only purpose is its untrusted certificate. android's
//! captive sign-in window reacts to such a certificate by offering "continue
//! anyway via browser", which is its one way out into the real browser.
//! whoever accepts the certificate is sent back to plain http.

use esp_idf_svc::sys::EspError;
use esp_idf_svc::tls::{EspTls, ServerConfig, Socket, X509};
use oasis_portal::{http, sni};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::time::Duration;

/// self-signed, created per checkout by `make firmware/tls/cert.pem`
const CERT: &[u8] = concat!(include_str!("../tls/cert.pem"), "\0").as_bytes();
const KEY: &[u8] = concat!(include_str!("../tls/key.pem"), "\0").as_bytes();
const HANDSHAKE_TIMEOUT_MS: u32 = 5000;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 512;
const INVALID_FD: i32 = -1;

/// hands a std socket to esp-tls, which closes it when the session ends.
struct Client(Option<TcpStream>);

impl Socket for Client {
    fn handle(&self) -> i32 {
        self.0.as_ref().map_or(INVALID_FD, AsRawFd::as_raw_fd)
    }

    fn release(&mut self) -> Result<(), EspError> {
        self.0.take().map(IntoRawFd::into_raw_fd);
        Ok(())
    }
}

/// completes the handshake, which most clients abort at the certificate,
/// and redirects the rest to `location`.
fn redirect(stream: TcpStream, location: &str) -> Result<(), EspError> {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let mut tls = EspTls::adopt(Client(Some(stream)))?;
    tls.negotiate_server(&ServerConfig {
        server_cert: Some(X509::pem_until_nul(CERT)),
        server_key: Some(X509::pem_until_nul(KEY)),
        tls_handshake_timeout_ms: HANDSHAKE_TIMEOUT_MS,
        ..ServerConfig::new()
    })?;
    let _ = tls.read(&mut [0u8; MAX_REQUEST_BYTES]);
    let mut reply = Vec::new();
    let _ = http::redirect(&mut reply, location);
    tls.write_all(&reply)
}

/// serves one connection at a time, forever. a handshake needs tens of KB
/// of heap, so they are not run in parallel, and clients that ask for a
/// host other than `names` are hung up on before it starts.
pub fn serve(listener: TcpListener, location: String, names: Vec<String>) {
    for stream in listener.incoming().flatten() {
        if !sni::addressed_to(&stream, &names).unwrap_or(false) {
            continue;
        }
        if let Err(err) = redirect(stream, &location) {
            log::debug!("https: {err}");
        }
    }
}
