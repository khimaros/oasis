//! state and address of a network interface, set through rtnetlink.

use crate::netlink::{Socket, attr};
use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

const SYS_NET: &str = "/sys/class/net";
pub const MAC_BYTES: usize = 6;

pub struct Link {
    pub name: String,
    pub index: u32,
    pub mac: [u8; MAC_BYTES],
}

/// reads a mac address written as `aa:bb:cc:dd:ee:ff`.
pub fn parse_mac(text: &str) -> Option<[u8; MAC_BYTES]> {
    let bytes: Option<Vec<u8>> = text.split(':').map(|byte| u8::from_str_radix(byte, 16).ok()).collect();
    bytes?.try_into().ok()
}

impl Link {
    fn sys(name: &str) -> PathBuf {
        Path::new(SYS_NET).join(name)
    }

    /// true once the kernel has an interface of this name.
    pub fn exists(name: &str) -> bool {
        Link::sys(name).exists()
    }

    pub fn find(name: &str) -> io::Result<Link> {
        let read = |file| fs::read_to_string(Link::sys(name).join(file));
        let index = read("ifindex")?.trim().parse().map_err(io::Error::other)?;
        let mac = parse_mac(read("address")?.trim()).ok_or_else(|| io::Error::other("no mac address"))?;
        Ok(Link { name: name.into(), index, mac })
    }

    pub fn is_wireless(&self) -> bool {
        Link::sys(&self.name).join("phy80211").exists()
    }

    pub fn set_up(&self, socket: &mut Socket) -> io::Result<()> {
        let up = (libc::IFF_UP as u32).to_ne_bytes();
        // family, device type, index, flags, and which of the flags to change
        let link = [&[0u8; 4][..], &self.index.to_ne_bytes(), &up, &up].concat();
        socket.request(libc::RTM_SETLINK, 0, &link).map(drop)
    }

    /// gives the interface `ip` in a network of `prefix` bits.
    pub fn set_address(&self, socket: &mut Socket, ip: Ipv4Addr, prefix: u8) -> io::Result<()> {
        let host_bits = u32::MAX.checked_shr(u32::from(prefix)).unwrap_or(0);
        let broadcast = Ipv4Addr::from(u32::from(ip) | host_bits);
        let attrs = [(libc::IFA_LOCAL, ip), (libc::IFA_ADDRESS, ip), (libc::IFA_BROADCAST, broadcast)];
        let attrs: Vec<u8> = attrs.iter().flat_map(|(kind, ip)| attr(*kind, &ip.octets())).collect();
        // family, prefix length, flags, scope, index
        let address = [&[libc::AF_INET as u8, prefix, 0, 0][..], &self.index.to_ne_bytes(), &attrs].concat();
        let flags = libc::NLM_F_CREATE | libc::NLM_F_REPLACE;
        socket.request(libc::RTM_NEWADDR, flags, &address).map(drop)
    }
}
