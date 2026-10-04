//! dhcp server for platforms that bring none. every device gets an address
//! next to the portal's, with the portal as router and dns server, so that
//! all lookups reach the captive dns responder. leases are kept in RAM: a
//! client that returns after a reboot asks for its old address again.

use crate::dns::ERROR_BACKOFF;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

const CLIENT_PORT: u16 = 68;
const MAX_PACKET: usize = 1500;
/// bootp relays and some clients drop anything shorter
const MIN_REPLY: usize = 300;
const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
const BOOT_REQUEST: u8 = 1;
const BOOT_REPLY: u8 = 2;
const MAC_BYTES: usize = 6;
// offsets of the fixed fields
const HLEN_AT: usize = 2;
const HOPS_AT: usize = 3;
const SECS_AT: usize = 8;
const CIADDR_AT: usize = 12;
const YIADDR_AT: usize = 16;
const SIADDR_AT: usize = 20;
const GIADDR_AT: usize = 24;
const CHADDR_AT: usize = 28;
const SNAME_AT: usize = 44;
const MAGIC_AT: usize = 236;
const OPTIONS_AT: usize = 240;
const MAGIC: [u8; 4] = [99, 130, 83, 99];
const OPT_PAD: u8 = 0;
const OPT_NETMASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_REQUESTED: u8 = 50;
const OPT_LEASE: u8 = 51;
const OPT_TYPE: u8 = 53;
const OPT_SERVER: u8 = 54;
const OPT_END: u8 = 255;
const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;
const RELEASE: u8 = 7;
/// how long an offered address waits for the client to request it
const OFFER_HOLD: Duration = Duration::from_secs(30);

/// the addresses to hand out: those of the server's /24 whose last byte is
/// between `first` and `last`. this bounds the lease table.
pub struct Pool {
    pub server: Ipv4Addr,
    pub first: u8,
    pub last: u8,
    pub lease: Duration,
}

struct Lease {
    mac: [u8; MAC_BYTES],
    /// last byte of the address
    host: u8,
    expires: Instant,
}

struct Leases {
    pool: Pool,
    /// at most one per address and per client
    leases: Vec<Lease>,
}

impl Leases {
    fn address(&self, host: u8) -> Ipv4Addr {
        let [a, b, c, _] = self.pool.server.octets();
        Ipv4Addr::new(a, b, c, host)
    }

    /// the last byte of `ip` when the pool holds it.
    fn host_of(&self, ip: Ipv4Addr) -> Option<u8> {
        let host = ip.octets()[3];
        let pooled = (self.pool.first..=self.pool.last).contains(&host) && ip != self.pool.server;
        (pooled && self.address(host) == ip).then_some(host)
    }

    /// true when nobody else holds a running lease of the address.
    fn available(&self, mac: [u8; MAC_BYTES], host: u8, now: Instant) -> bool {
        self.leases.iter().all(|lease| lease.host != host || lease.mac == mac || lease.expires <= now)
    }

    /// the address to offer: the one the client has, the one it wants, or
    /// the first that is free. None when the pool is used up.
    fn choose(&self, mac: [u8; MAC_BYTES], wanted: Option<u8>, now: Instant) -> Option<u8> {
        let own = self.leases.iter().find(|lease| lease.mac == mac).map(|lease| lease.host);
        let pooled =
            (self.pool.first..=self.pool.last).filter(|host| self.host_of(self.address(*host)).is_some());
        own.into_iter().chain(wanted).chain(pooled).find(|host| self.available(mac, *host, now))
    }

    fn assign(&mut self, mac: [u8; MAC_BYTES], host: u8, expires: Instant) {
        self.release(mac);
        self.leases.retain(|lease| lease.host != host);
        self.leases.push(Lease { mac, host, expires });
    }

    fn release(&mut self, mac: [u8; MAC_BYTES]) {
        self.leases.retain(|lease| lease.mac != mac);
    }
}

/// the value of option `code` in a message.
fn option(packet: &[u8], code: u8) -> Option<&[u8]> {
    let mut rest = packet.get(OPTIONS_AT..)?;
    loop {
        match *rest.first()? {
            OPT_END => return None,
            OPT_PAD => rest = &rest[1..],
            found => {
                let length = usize::from(*rest.get(1)?);
                let value = rest.get(2..2 + length)?;
                if found == code {
                    return Some(value);
                }
                rest = &rest[2 + length..];
            }
        }
    }
}

fn address(bytes: &[u8]) -> Option<Ipv4Addr> {
    <[u8; 4]>::try_from(bytes).ok().map(Ipv4Addr::from).filter(|ip| !ip.is_unspecified())
}

/// a server message of type `kind` in reply to `request`. `yours` is the
/// address it grants, with the settings that go with it.
fn message(request: &[u8], kind: u8, yours: Option<Ipv4Addr>, pool: &Pool) -> Vec<u8> {
    let server = pool.server.octets();
    let mut reply = request[..OPTIONS_AT].to_vec();
    reply[0] = BOOT_REPLY;
    reply[HOPS_AT] = 0;
    reply[SECS_AT..CIADDR_AT].fill(0);
    reply[CIADDR_AT..GIADDR_AT].fill(0);
    reply[SIADDR_AT..GIADDR_AT].copy_from_slice(&server);
    reply[SNAME_AT..MAGIC_AT].fill(0);
    reply.extend_from_slice(&[OPT_TYPE, 1, kind, OPT_SERVER, 4]);
    reply.extend_from_slice(&server);
    if let Some(yours) = yours {
        reply[YIADDR_AT..SIADDR_AT].copy_from_slice(&yours.octets());
        let lease = (pool.lease.as_secs() as u32).to_be_bytes();
        let settings =
            [(OPT_LEASE, lease), (OPT_NETMASK, NETMASK.octets()), (OPT_ROUTER, server), (OPT_DNS, server)];
        for (code, value) in settings {
            reply.extend_from_slice(&[code, 4]);
            reply.extend_from_slice(&value);
        }
    }
    reply.push(OPT_END);
    reply.resize(reply.len().max(MIN_REPLY), OPT_PAD);
    reply
}

/// the reply to a client message, None when it calls for none.
fn reply(leases: &mut Leases, packet: &[u8], now: Instant) -> Option<Vec<u8>> {
    let sound = packet.len() >= OPTIONS_AT
        && packet[0] == BOOT_REQUEST
        && usize::from(packet[HLEN_AT]) == MAC_BYTES
        && packet[MAGIC_AT..OPTIONS_AT] == MAGIC;
    let kind = *option(packet, OPT_TYPE).filter(|_| sound)?.first()?;
    if option(packet, OPT_SERVER).is_some_and(|server| server != leases.pool.server.octets()) {
        return None;
    }
    let mac: [u8; MAC_BYTES] = packet[CHADDR_AT..CHADDR_AT + MAC_BYTES].try_into().ok()?;
    let wanted = option(packet, OPT_REQUESTED).and_then(address);
    let wanted = wanted.or_else(|| address(&packet[CIADDR_AT..YIADDR_AT])).and_then(|ip| leases.host_of(ip));
    let lease = leases.pool.lease;
    match kind {
        DISCOVER => {
            let host = leases.choose(mac, wanted, now)?;
            leases.assign(mac, host, now + OFFER_HOLD.min(lease));
            Some(message(packet, OFFER, Some(leases.address(host)), &leases.pool))
        }
        REQUEST => match wanted.filter(|host| leases.available(mac, *host, now)) {
            Some(host) => {
                leases.assign(mac, host, now + lease);
                Some(message(packet, ACK, Some(leases.address(host)), &leases.pool))
            }
            None => Some(message(packet, NAK, None, &leases.pool)),
        },
        RELEASE => {
            leases.release(mac);
            None
        }
        _ => None,
    }
}

/// where a reply goes. a client that has no address yet is only reached by
/// broadcast.
fn destination(peer: SocketAddr) -> SocketAddr {
    match peer.ip().is_unspecified() {
        true => SocketAddr::from((Ipv4Addr::BROADCAST, CLIENT_PORT)),
        false => peer,
    }
}

/// answers clients forever.
pub fn serve(socket: UdpSocket, pool: Pool) -> io::Result<()> {
    socket.set_broadcast(true)?;
    let mut leases = Leases { pool, leases: Vec::new() };
    let mut buf = [0u8; MAX_PACKET];
    loop {
        match socket.recv_from(&mut buf) {
            Ok((len, peer)) => {
                if let Some(reply) = reply(&mut leases, &buf[..len], Instant::now()) {
                    let _ = socket.send_to(&reply, destination(peer));
                }
            }
            Err(_) => thread::sleep(ERROR_BACKOFF),
        }
    }
}
