//! tells whether a tls client is asking for the portal, by reading the
//! server name of its first message without taking part in tls.
//!
//! the device's dns answers every name with its own address, so its https
//! port also receives connections meant for the internet: the connectivity
//! checks of phones and the background traffic of their apps. none of them
//! accept our certificate, and each handshake costs the device seconds of
//! cpu and tens of KB of heap. such clients are hung up on instead.

use std::io;
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

/// more than any client hello takes, including large post-quantum key shares
const MAX_HELLO_BYTES: usize = 8192;
const RECORD_HEADER_BYTES: usize = 5;
const RECORD_HANDSHAKE: u8 = 22;
const HANDSHAKE_CLIENT_HELLO: u8 = 1;
/// handshake type and length, client version, and random
const HELLO_FIXED_BYTES: usize = 4 + 2 + 32;
const EXTENSION_SERVER_NAME: usize = 0;
const NAME_TYPE_HOST: u8 = 0;
/// a hello arrives in one or two packets. the wait for more of it ends
/// after this many steps without any
const WAIT_STEP: Duration = Duration::from_millis(20);
const STALL_STEPS: u32 = 10;
/// a silent client must not hold up a listener that serves one at a time
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq)]
pub enum Hello {
    /// more bytes are needed
    Incomplete,
    /// a client hello, with the server name it asks for, if any
    Name(Option<String>),
    /// not a tls client hello
    Invalid,
}

/// walks a message front to back. every step returns None past the end.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(count)?;
        self.0 = rest;
        Some(taken)
    }

    /// a big endian number of `width` bytes.
    fn number(&mut self, width: usize) -> Option<usize> {
        Some(self.take(width)?.iter().fold(0, |number, byte| number << 8 | usize::from(*byte)))
    }

    /// a field that is preceded by its length in `width` bytes.
    fn field(&mut self, width: usize) -> Option<Cursor<'a>> {
        let length = self.number(width)?;
        self.take(length).map(Cursor)
    }
}

/// the host name in the server name extension of a client hello body.
/// a hello that is not `whole` was cut short somewhere in its extensions,
/// which are then read as far as they go.
fn name_in(mut hello: Cursor, whole: bool) -> Option<Option<String>> {
    if hello.take(1)? != [HANDSHAKE_CLIENT_HELLO] {
        return None;
    }
    hello.take(HELLO_FIXED_BYTES - 1)?;
    // session id, cipher suites, compression methods
    for width in [1, 2, 1] {
        hello.field(width)?;
    }
    let mut extensions = match (whole, hello.field(2)) {
        (_, Some(extensions)) => extensions,
        (true, None) => return Some(None),
        (false, None) => hello,
    };
    while !extensions.0.is_empty() {
        let (Some(kind), Some(mut body)) = (extensions.number(2), extensions.field(2)) else {
            return (!whole).then_some(None);
        };
        if kind == EXTENSION_SERVER_NAME {
            let mut names = body.field(2)?;
            let host = names.take(1)? == [NAME_TYPE_HOST];
            let name = names.field(2)?.0;
            return Some(host.then(|| String::from_utf8_lossy(name).into_owned()));
        }
    }
    Some(None)
}

/// reads the server name from the start of a tls connection.
pub fn server_name(data: &[u8]) -> Hello {
    let Some(header) = data.get(..RECORD_HEADER_BYTES) else {
        return if data.first().is_none_or(|kind| *kind == RECORD_HANDSHAKE) {
            Hello::Incomplete
        } else {
            Hello::Invalid
        };
    };
    let length = usize::from(u16::from_be_bytes([header[3], header[4]]));
    if header[0] != RECORD_HANDSHAKE || RECORD_HEADER_BYTES + length > MAX_HELLO_BYTES {
        return Hello::Invalid;
    }
    match data.get(RECORD_HEADER_BYTES..RECORD_HEADER_BYTES + length) {
        Some(record) => name_in(Cursor(record), true).map_or(Hello::Invalid, Hello::Name),
        None => Hello::Incomplete,
    }
}

/// the server name in the part of a client hello that has arrived. None
/// when that part holds no name, which does not mean that the hello has none.
fn name_so_far(data: &[u8]) -> Option<String> {
    name_in(Cursor(data.get(RECORD_HEADER_BYTES..)?), false).flatten()
}

/// waits for the client hello on `stream`, without consuming it, and tells
/// whether it is addressed to us: without a server name, as when connecting
/// to an ip address, or with one of `names`.
///
/// lwip only lets us peek at the first packet, and the hello of a browser
/// spans two. once no more of it shows up, the client is turned away only
/// for a foreign name in the part that did.
pub fn addressed_to(stream: &TcpStream, names: &[String]) -> io::Result<bool> {
    let ours = |name: &str| names.iter().any(|ours| ours.eq_ignore_ascii_case(name));
    let mut hello = vec![0u8; MAX_HELLO_BYTES];
    stream.set_read_timeout(Some(FIRST_BYTE_TIMEOUT))?;
    let (mut seen, mut stalled) = (0, 0);
    while stalled < STALL_STEPS {
        let length = stream.peek(&mut hello)?;
        match server_name(&hello[..length]) {
            Hello::Name(name) => return Ok(name.is_none_or(|name| ours(&name))),
            Hello::Invalid => return Ok(false),
            Hello::Incomplete if length == 0 => return Ok(false),
            Hello::Incomplete => thread::sleep(WAIT_STEP),
        }
        stalled = if length == seen { stalled + 1 } else { 0 };
        seen = length;
    }
    Ok(seen > RECORD_HEADER_BYTES && name_so_far(&hello[..seen]).is_none_or(|name| ours(&name)))
}
