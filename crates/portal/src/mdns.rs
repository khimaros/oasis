//! mdns responder for platforms that bring none: answers A queries for the
//! portal's `.local` name. unlike the captive dns this still works for
//! clients whose dns settings bypass the access point.

use crate::dns::{
    CLASS_IN, ERROR_BACKOFF, FLAG_AUTHORITATIVE, FLAG_RESPONSE, HEADER_LEN, LABEL_POINTER, TYPE_A,
};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::thread;

pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const PORT: u16 = 5353;
const MAX_PACKET: usize = 1500;
const TTL_SECS: u32 = 120;
/// the most that resolvers without mdns support should cache an answer
const LEGACY_TTL_SECS: u32 = 10;
const TYPE_ANY: u16 = 255;
/// top bit of the class: "unicast reply wanted" in a question, "replaces
/// what you cached" in an answer
const CLASS_FLAG: u16 = 0x8000;
const MAX_QUESTIONS: u16 = 16;
const MAX_POINTERS: usize = 8;

/// a name as length prefixed labels.
fn labels(name: &str) -> Vec<u8> {
    let label = |label: &str| [&[label.len() as u8][..], label.as_bytes()].concat();
    name.split('.').flat_map(label).chain([0]).collect()
}

/// the name at `pos` in lowercase, and the offset just past it. follows
/// compression pointers, which later questions use to repeat a suffix.
fn name_at(packet: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let (mut name, mut end, mut pointers) = (String::new(), None, 0);
    loop {
        let len = usize::from(*packet.get(pos)?);
        if len == 0 {
            return Some((name, end.unwrap_or(pos + 1)));
        }
        if len as u8 & LABEL_POINTER == LABEL_POINTER {
            pointers += 1;
            end = end.or(Some(pos + 2)).filter(|_| pointers <= MAX_POINTERS);
            end?;
            pos = (len & usize::from(!LABEL_POINTER)) << 8 | usize::from(*packet.get(pos + 1)?);
            continue;
        }
        let label = String::from_utf8_lossy(packet.get(pos + 1..pos + 1 + len)?).to_ascii_lowercase();
        name = if name.is_empty() { label } else { format!("{name}.{label}") };
        pos += 1 + len;
    }
}

/// the question of `query` that asks for the address of `host`, as sent.
fn question<'a>(query: &'a [u8], host: &str) -> Option<&'a [u8]> {
    let count = u16::from_be_bytes([query[4], query[5]]).min(MAX_QUESTIONS);
    let mut pos = HEADER_LEN;
    for _ in 0..count {
        let (name, end) = name_at(query, pos)?;
        let fields = query.get(end..end + 4)?;
        let (kind, class) =
            (u16::from_be_bytes([fields[0], fields[1]]), u16::from_be_bytes([fields[2], fields[3]]));
        if name == host && [TYPE_A, TYPE_ANY].contains(&kind) && class & !CLASS_FLAG == CLASS_IN {
            return Some(&query[pos..end + 4]);
        }
        pos = end + 4;
    }
    None
}

/// the reply to a query that asks for the address of `host`, a lowercase
/// name. `legacy` queries come from resolvers that do not speak mdns: they
/// get their id and question back, like from a dns server.
pub fn answer(query: &[u8], host: &str, ip: Ipv4Addr, legacy: bool) -> Option<Vec<u8>> {
    if query.len() < HEADER_LEN || query[2] & FLAG_RESPONSE != 0 {
        return None;
    }
    let asked = question(query, host)?;
    let name = labels(host);
    let (id, class, ttl) = match legacy {
        true => (&query[..2], CLASS_IN, LEGACY_TTL_SECS),
        false => (&[0, 0][..], CLASS_IN | CLASS_FLAG, TTL_SECS),
    };
    let counts = [FLAG_RESPONSE | FLAG_AUTHORITATIVE, 0, 0, legacy as u8, 0, 1, 0, 0, 0, 0];
    let mut reply = [id, &counts[..]].concat();
    if legacy && asked.len() == name.len() + 4 {
        reply.extend_from_slice(asked);
    } else if legacy {
        reply.extend_from_slice(&[&name[..], &TYPE_A.to_be_bytes(), &CLASS_IN.to_be_bytes()].concat());
    }
    reply.extend_from_slice(&name);
    reply.extend_from_slice(&[TYPE_A.to_be_bytes(), class.to_be_bytes()].concat());
    reply.extend_from_slice(&ttl.to_be_bytes());
    reply.extend_from_slice(&[0, 4]);
    reply.extend_from_slice(&ip.octets());
    Some(reply)
}

/// answers queries forever. the socket must have joined `GROUP`, and send
/// multicast out of the interface that the clients are on.
pub fn serve(socket: UdpSocket, host: &str, ip: Ipv4Addr) -> io::Result<()> {
    let host = host.to_ascii_lowercase();
    let mut buf = [0u8; MAX_PACKET];
    loop {
        match socket.recv_from(&mut buf) {
            Ok((len, peer)) => {
                let legacy = peer.port() != PORT;
                if let Some(reply) = answer(&buf[..len], &host, ip, legacy) {
                    let group = SocketAddr::from((GROUP, PORT));
                    let _ = socket.send_to(&reply, if legacy { peer } else { group });
                }
            }
            Err(_) => thread::sleep(ERROR_BACKOFF),
        }
    }
}
