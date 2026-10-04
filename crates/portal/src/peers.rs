//! presence and WebRTC signaling. the device only introduces clients to
//! each other, after which they talk directly.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::time::{Duration, Instant};

pub const MAX_PEERS: usize = 16;
pub const MAX_SIGNALS: usize = 12;
pub const MAX_SIGNAL_BYTES: usize = 4096;
const PEER_TTL: Duration = Duration::from_secs(10);
const SIGNAL_TTL: Duration = Duration::from_secs(30);

pub struct Peer {
    /// secret chosen by the client, proves ownership of `id`
    key: String,
    pub id: u64,
    pub name: String,
    pub ip: IpAddr,
    seen: Instant,
}

pub struct Signal {
    to: u64,
    pub from: u64,
    /// lets the receiver replace mDNS obfuscated ICE candidates
    pub ip: IpAddr,
    pub data: String,
    sent: Instant,
}

#[derive(Default)]
pub struct Peers {
    peers: Vec<Peer>,
    signals: VecDeque<Signal>,
    last_id: u64,
}

impl Peers {
    fn expire(&mut self, now: Instant) {
        self.peers.retain(|peer| now.duration_since(peer.seen) < PEER_TTL);
        self.signals.retain(|signal| now.duration_since(signal.sent) < SIGNAL_TTL);
    }

    /// registers or refreshes the client holding `key` and returns its
    /// public id. None when the table is full.
    pub fn touch(&mut self, key: &str, name: &str, ip: IpAddr, now: Instant) -> Option<u64> {
        self.expire(now);
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.key == key) {
            (peer.name, peer.ip, peer.seen) = (name.into(), ip, now);
            return Some(peer.id);
        }
        if self.peers.len() >= MAX_PEERS {
            return None;
        }
        self.last_id += 1;
        let (key, name) = (key.into(), name.into());
        self.peers.push(Peer { key, id: self.last_id, name, ip, seen: now });
        Some(self.last_id)
    }

    pub fn list(&self) -> &[Peer] {
        &self.peers
    }

    /// queues `data` for peer `to`. false when the sender or receiver is
    /// unknown or the queue is full.
    pub fn send(&mut self, key: &str, to: u64, data: &str, now: Instant) -> bool {
        self.expire(now);
        let from = self.peers.iter().find(|peer| peer.key == key).map(|peer| (peer.id, peer.ip));
        let known = self.peers.iter().any(|peer| peer.id == to);
        match from {
            Some((from, ip)) if known && self.signals.len() < MAX_SIGNALS => {
                self.signals.push_back(Signal { to, from, ip, data: data.into(), sent: now });
                true
            }
            _ => false,
        }
    }

    /// removes and returns the signals queued for peer `id`.
    pub fn take(&mut self, id: u64) -> Vec<Signal> {
        let (mine, rest): (Vec<_>, Vec<_>) = self.signals.drain(..).partition(|signal| signal.to == id);
        self.signals = rest.into();
        mine
    }
}
