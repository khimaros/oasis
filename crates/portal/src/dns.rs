//! captive dns: every A query resolves to the portal so that client
//! connectivity probes land on our http server.

use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::thread;
use std::time::Duration;

pub(crate) const HEADER_LEN: usize = 12;
const MAX_PACKET: usize = 512;
const TTL_SECS: u8 = 60;
pub(crate) const TYPE_A: u16 = 1;
pub(crate) const CLASS_IN: u16 = 1;
pub(crate) const FLAG_RESPONSE: u8 = 0x80;
pub(crate) const FLAG_AUTHORITATIVE: u8 = 0x04;
const FLAG_RECURSION_AVAILABLE: u8 = 0x80;
// opcode and recursion desired are echoed back from the query
const QUERY_FLAGS_KEPT: u8 = 0x79;
pub(crate) const LABEL_POINTER: u8 = 0xC0;
pub(crate) const ERROR_BACKOFF: Duration = Duration::from_millis(100);

fn u16_at(buf: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([buf[pos], buf[pos + 1]])
}

/// offset just past the first question (name, type, class).
fn question_end(query: &[u8]) -> Option<usize> {
    let mut pos = HEADER_LEN;
    loop {
        let len = *query.get(pos)?;
        if len & LABEL_POINTER != 0 {
            return None;
        }
        pos += 1 + len as usize;
        if len == 0 {
            break;
        }
    }
    (query.len() >= pos + 4).then_some(pos + 4)
}

/// builds the reply for a query. A questions get `ip`, anything else gets
/// an empty answer so that clients fall back to A.
pub fn answer(query: &[u8], ip: Ipv4Addr) -> Option<Vec<u8>> {
    if query.len() < HEADER_LEN || query[2] & FLAG_RESPONSE != 0 || u16_at(query, 4) == 0 {
        return None;
    }
    let end = question_end(query)?;
    let is_a = u16_at(query, end - 4) == TYPE_A && u16_at(query, end - 2) == CLASS_IN;
    let mut reply = Vec::with_capacity(end + 16);
    reply.extend_from_slice(&query[..2]);
    reply.push(FLAG_RESPONSE | FLAG_AUTHORITATIVE | (query[2] & QUERY_FLAGS_KEPT));
    reply.push(FLAG_RECURSION_AVAILABLE);
    reply.extend_from_slice(&[0, 1, 0, is_a as u8, 0, 0, 0, 0]);
    reply.extend_from_slice(&query[HEADER_LEN..end]);
    if is_a {
        // name is a pointer back to the question at offset 12
        reply.extend_from_slice(&[LABEL_POINTER, HEADER_LEN as u8, 0, 1, 0, 1]);
        reply.extend_from_slice(&[0, 0, 0, TTL_SECS, 0, 4]);
        reply.extend_from_slice(&ip.octets());
    }
    Some(reply)
}

/// answers queries forever.
pub fn serve(socket: UdpSocket, ip: Ipv4Addr) -> io::Result<()> {
    let mut buf = [0u8; MAX_PACKET];
    loop {
        match socket.recv_from(&mut buf) {
            Ok((len, peer)) => {
                if let Some(reply) = answer(&buf[..len], ip) {
                    let _ = socket.send_to(&reply, peer);
                }
            }
            Err(_) => thread::sleep(ERROR_BACKOFF),
        }
    }
}
