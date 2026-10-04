//! https listener whose only purpose is its untrusted certificate. android's
//! captive sign-in window reacts to such a certificate by offering "continue
//! anyway via browser", which is its one way out into the real browser.
//! whoever accepts the certificate is sent back to plain http.

use oasis_portal::{http, sni};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, Stream};
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 512;

/// reads the self-signed certificate and its key, which the image build
/// puts into `dir`.
pub fn config(dir: &Path) -> Result<Arc<ServerConfig>, Box<dyn Error>> {
    let certs = CertificateDer::pem_slice_iter(&fs::read(dir.join(CERT_FILE))?).collect::<Result<_, _>>()?;
    let key = PrivateKeyDer::from_pem_slice(&fs::read(dir.join(KEY_FILE))?)?;
    Ok(Arc::new(ServerConfig::builder().with_no_client_auth().with_single_cert(certs, key)?))
}

/// completes the handshake, which most clients abort at the certificate,
/// and redirects the rest to `location`.
fn redirect(mut stream: TcpStream, config: &Arc<ServerConfig>, location: &str) -> Result<(), Box<dyn Error>> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut session = ServerConnection::new(config.clone())?;
    let mut tls = Stream::new(&mut session, &mut stream);
    let _ = tls.read(&mut [0u8; MAX_REQUEST_BYTES])?;
    let mut reply = Vec::new();
    http::redirect(&mut reply, location)?;
    tls.write_all(&reply)?;
    session.send_close_notify();
    Ok(session.complete_io(&mut stream).map(drop)?)
}

/// serves one connection at a time, forever. clients that ask for a host
/// other than the portal's `names` are hung up on, see `sni`.
pub fn serve(listener: TcpListener, config: Arc<ServerConfig>, names: Vec<String>, location: String) {
    for stream in listener.incoming().flatten() {
        if sni::addressed_to(&stream, &names).unwrap_or(false) {
            let _ = redirect(stream, &config, &location);
        }
    }
}
