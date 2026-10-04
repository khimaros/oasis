//! minimal http/1.1 server side: one request per connection, small bounded
//! buffers, urlencoded forms in and json out.

use crate::text::parse_params;
use std::io::{self, Read, Write};
use std::net::IpAddr;

const MAX_HEAD_BYTES: usize = 8192;
const MAX_BODY_BYTES: usize = 16384;
const READ_CHUNK: usize = 512;
const HEAD_END: &[u8] = b"\r\n\r\n";

pub const OK: &str = "200 OK";
pub const NO_CONTENT: &str = "204 No Content";
pub const FOUND: &str = "302 Found";
pub const BAD_REQUEST: &str = "400 Bad Request";
pub const FORBIDDEN: &str = "403 Forbidden";
pub const NOT_FOUND: &str = "404 Not Found";
pub const CONFLICT: &str = "409 Conflict";
pub const TOO_MANY: &str = "429 Too Many Requests";
pub const UNAVAILABLE: &str = "503 Service Unavailable";

pub const HTML: &str = "text/html; charset=utf-8";
pub const JSON: &str = "application/json";
pub const BINARY: &str = "application/octet-stream";
pub const TEXT: &str = "text/plain; charset=utf-8";

pub type Params = Vec<(String, String)>;

pub struct Request {
    pub method: String,
    pub path: String,
    pub host: String,
    pub query: Params,
    pub form: Params,
    pub peer: IpAddr,
}

impl Request {
    /// form field, falling back to the query string. empty when absent.
    pub fn param(&self, key: &str) -> &str {
        let mut pairs = self.form.iter().chain(&self.query);
        pairs.find(|(k, _)| k == key).map_or("", |(_, v)| v)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// reads until the blank line ending the headers. returns the buffer and
/// the offset where the body starts.
fn read_head(stream: &mut impl Read) -> io::Result<(Vec<u8>, usize)> {
    let mut buf = Vec::with_capacity(READ_CHUNK);
    let mut chunk = [0u8; READ_CHUNK];
    loop {
        let len = stream.read(&mut chunk)?;
        if len == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..len]);
        if let Some(pos) = buf.windows(HEAD_END.len()).position(|window| window == HEAD_END) {
            return Ok((buf, pos + HEAD_END.len()));
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(invalid("headers too large"));
        }
    }
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    let matches = |line: &&str| line.split(':').next().is_some_and(|n| n.eq_ignore_ascii_case(name));
    head.lines().skip(1).find(matches).and_then(|line| line.split_once(':')).map(|(_, v)| v.trim())
}

pub fn read_request(stream: &mut impl Read, peer: IpAddr) -> io::Result<Request> {
    let (mut buf, body_start) = read_head(stream)?;
    let head = String::from_utf8_lossy(&buf[..body_start]).into_owned();
    let mut parts = head.lines().next().unwrap_or("").split(' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(invalid("malformed request line"));
    };
    let length = header(&head, "content-length").map_or(Ok(0), str::parse::<usize>);
    let length = length.ok().filter(|len| *len <= MAX_BODY_BYTES).ok_or_else(|| invalid("bad length"))?;
    let have = buf.len() - body_start;
    buf.resize(body_start + length.max(have), 0);
    if length > have {
        stream.read_exact(&mut buf[body_start + have..])?;
    }
    let body = String::from_utf8_lossy(&buf[body_start..body_start + length]);
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Request {
        method: method.into(),
        path: path.into(),
        host: header(&head, "host").unwrap_or("").into(),
        query: parse_params(query),
        form: parse_params(&body),
        peer,
    })
}

/// writes the status line and headers. the body is delimited by closing
/// the connection, which lets handlers stream without knowing its length.
pub fn head(out: &mut impl Write, status: &str, content_type: &str) -> io::Result<()> {
    write!(out, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n")?;
    write!(out, "Cache-Control: no-store\r\nConnection: close\r\n\r\n")
}

pub fn send(out: &mut impl Write, status: &str, content_type: &str, body: &str) -> io::Result<()> {
    head(out, status, content_type)?;
    out.write_all(body.as_bytes())
}

pub fn redirect(out: &mut impl Write, location: &str) -> io::Result<()> {
    write!(out, "HTTP/1.1 {FOUND}\r\nLocation: {location}\r\n")?;
    write!(out, "Cache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}
