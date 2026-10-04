//! the open access point, set up through nl80211. the wifi chip of the
//! raspberry pi is "fullmac": its firmware answers probes and associates
//! clients by itself. so an open network takes a few requests to the driver
//! and no daemon like hostapd.

use crate::link::{Link, MAC_BYTES};
use crate::netlink::{Socket, attr, attrs};
use std::io;
use std::sync::Mutex;

const FAMILY: &[u8] = b"nl80211\0";
const GENL_HEADER_BYTES: usize = 4;
const GENL_VERSION: u8 = 1;
// from linux/nl80211.h
const CMD_SET_INTERFACE: u8 = 6;
const CMD_START_AP: u8 = 15;
const CMD_GET_STATION: u8 = 17;
const ATTR_IFINDEX: u16 = 3;
const ATTR_IFTYPE: u16 = 5;
const ATTR_MAC: u16 = 6;
const ATTR_BEACON_INTERVAL: u16 = 12;
const ATTR_DTIM_PERIOD: u16 = 13;
const ATTR_BEACON_HEAD: u16 = 14;
const ATTR_BEACON_TAIL: u16 = 15;
const ATTR_WIPHY_FREQ: u16 = 38;
const ATTR_SSID: u16 = 52;
const ATTR_AUTH_TYPE: u16 = 53;
const IFTYPE_AP: u32 = 3;
const AUTH_OPEN: u32 = 0;

const SSID_MAX: usize = 32;
/// in units of 1024 microseconds
const BEACON_INTERVAL: u16 = 100;
/// beacons between the wake-ups of clients that save power
const DTIM_PERIOD: u32 = 2;
/// channel 1 is 2412 MHz, and channels are 5 MHz apart
const CHANNEL_BASE_MHZ: u32 = 2407;
const CHANNEL_STEP_MHZ: u32 = 5;
const FRAME_BEACON: [u8; 2] = [0x80, 0];
const EVERYONE: [u8; MAC_BYTES] = [0xff; MAC_BYTES];
/// an access point, with short slot time
const CAPABILITY: u16 = 0x0401;
const ELEMENT_SSID: u8 = 0;
const ELEMENT_RATES: u8 = 1;
const ELEMENT_CHANNEL: u8 = 3;
const ELEMENT_ERP: u8 = 42;
const ELEMENT_MORE_RATES: u8 = 50;
/// in units of 500 kbit/s. 1 to 11 Mbit/s are marked as required by their
/// top bit, then come 6 to 18 Mbit/s
const RATES: [u8; 8] = [0x82, 0x84, 0x8b, 0x96, 12, 18, 24, 36];
/// 24 to 54 Mbit/s
const MORE_RATES: [u8; 4] = [48, 72, 96, 108];

pub struct Wifi {
    socket: Mutex<Socket>,
    family: u16,
    index: u32,
}

fn element(id: u8, value: &[u8]) -> Vec<u8> {
    [&[id, value.len() as u8][..], value].concat()
}

/// the beacon frame in two parts: up to the traffic map, which the driver
/// inserts, and after it.
fn beacon(bssid: [u8; MAC_BYTES], ssid: &str, channel: u8) -> (Vec<u8>, Vec<u8>) {
    let (duration, sequence, timestamp) = ([0; 2], [0; 2], [0; 8]);
    let header = [&FRAME_BEACON[..], &duration, &EVERYONE, &bssid, &bssid, &sequence].concat();
    let fixed = [&timestamp[..], &BEACON_INTERVAL.to_le_bytes(), &CAPABILITY.to_le_bytes()].concat();
    let elements = [
        element(ELEMENT_SSID, ssid.as_bytes()),
        element(ELEMENT_RATES, &RATES),
        element(ELEMENT_CHANNEL, &[channel]),
    ];
    let tail = [element(ELEMENT_ERP, &[0]), element(ELEMENT_MORE_RATES, &MORE_RATES)].concat();
    ([header, fixed, elements.concat()].concat(), tail)
}

impl Wifi {
    /// connects to the wifi driver of `link`.
    pub fn open(link: &Link) -> io::Result<Wifi> {
        let mut socket = Socket::open(libc::NETLINK_GENERIC)?;
        let lookup = [
            &[libc::CTRL_CMD_GETFAMILY as u8, GENL_VERSION, 0, 0][..],
            &attr(libc::CTRL_ATTR_FAMILY_NAME as u16, FAMILY),
        ];
        let replies = socket.request(libc::GENL_ID_CTRL as u16, 0, &lookup.concat())?;
        let found =
            replies.iter().flat_map(|reply| attrs(reply.get(GENL_HEADER_BYTES..).unwrap_or_default()));
        let family = found
            .filter(|(kind, _)| *kind == libc::CTRL_ATTR_FAMILY_ID as u16)
            .find_map(|(_, value)| value.try_into().ok().map(u16::from_ne_bytes))
            .ok_or_else(|| io::Error::other("the kernel has no nl80211"))?;
        Ok(Wifi { socket: Mutex::new(socket), family, index: link.index })
    }

    /// sends a command about our interface and returns the attributes of
    /// every reply.
    fn command(&self, command: u8, flags: i32, more: &[Vec<u8>]) -> io::Result<Vec<Vec<u8>>> {
        let interface = attr(ATTR_IFINDEX, &self.index.to_ne_bytes());
        let payload = [&[command, 0, 0, 0][..], &interface, &more.concat()].concat();
        let replies = self.socket.lock().unwrap().request(self.family, flags, &payload)?;
        Ok(replies
            .into_iter()
            .map(|reply| reply.get(GENL_HEADER_BYTES..).unwrap_or_default().to_vec())
            .collect())
    }

    /// switches the interface from client to access point. it has to be
    /// down for that.
    pub fn become_access_point(&self) -> io::Result<()> {
        self.command(CMD_SET_INTERFACE, 0, &[attr(ATTR_IFTYPE, &IFTYPE_AP.to_ne_bytes())]).map(drop)
    }

    /// starts an open network. the interface has to be up.
    pub fn start(&self, link: &Link, ssid: &str, channel: u8) -> io::Result<()> {
        if ssid.is_empty() || ssid.len() > SSID_MAX {
            return Err(io::Error::other("the ssid must be 1 to 32 bytes"));
        }
        let (head, tail) = beacon(link.mac, ssid, channel);
        let frequency = CHANNEL_BASE_MHZ + CHANNEL_STEP_MHZ * u32::from(channel);
        let settings = [
            attr(ATTR_BEACON_HEAD, &head),
            attr(ATTR_BEACON_TAIL, &tail),
            attr(ATTR_BEACON_INTERVAL, &u32::from(BEACON_INTERVAL).to_ne_bytes()),
            attr(ATTR_DTIM_PERIOD, &DTIM_PERIOD.to_ne_bytes()),
            attr(ATTR_SSID, ssid.as_bytes()),
            attr(ATTR_AUTH_TYPE, &AUTH_OPEN.to_ne_bytes()),
            attr(ATTR_WIPHY_FREQ, &frequency.to_ne_bytes()),
        ];
        self.command(CMD_START_AP, 0, &settings).map(drop)
    }

    /// mac addresses of the clients that are associated.
    pub fn stations(&self) -> Vec<[u8; MAC_BYTES]> {
        let replies = self.command(CMD_GET_STATION, libc::NLM_F_DUMP, &[]).unwrap_or_default();
        let mac = |reply: &Vec<u8>| attrs(reply).find(|(kind, _)| *kind == ATTR_MAC)?.1.try_into().ok();
        replies.iter().filter_map(mac).collect()
    }
}
