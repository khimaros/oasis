//! runs the portal on a development host. configured through environment
//! variables so that the end-to-end tests can shrink limits and pick ports.

use oasis_portal::{Config, Portal, Space, dhcp, dns, mdns, sni};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const WORKER_STACK: usize = 256 * 1024;
/// size of the data partition on the device
const STORAGE_BYTES: u64 = 0x270000;

fn var<T: FromStr>(name: &str, default: T) -> T {
    env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// there is no access point on a host, so every source address counts as
/// its own device. the tests connect from several loopback addresses.
fn mac_of(ip: IpAddr) -> Option<[u8; 6]> {
    match ip {
        IpAddr::V4(ip) => Some([2, 0, ip.octets()[0], ip.octets()[1], ip.octets()[2], ip.octets()[3]]),
        IpAddr::V6(_) => None,
    }
}

/// a host has no wifi. the addresses listed in OASIS_STATIONS stand in for
/// the devices that are connected to it.
fn stations() -> Vec<[u8; 6]> {
    let listed: String = var("OASIS_STATIONS", String::new());
    listed.split(',').filter_map(|ip| ip.parse().ok()).filter_map(mac_of).collect()
}

/// bytes of all files below `dir`.
fn dir_bytes(dir: &Path) -> u64 {
    let size = |entry: fs::DirEntry| match entry.metadata() {
        Ok(meta) if meta.is_dir() => dir_bytes(&entry.path()),
        Ok(meta) => meta.len(),
        Err(_) => 0,
    };
    fs::read_dir(dir).into_iter().flatten().flatten().map(size).sum()
}

/// a host has no flash partitions. the data directory stands in for the
/// one that holds user data on the device.
fn space() -> Vec<Space> {
    let dir: PathBuf = var("OASIS_DATA_DIR", "data".into());
    vec![Space { name: "storage", used: dir_bytes(&dir), size: STORAGE_BYTES }]
}

fn main() -> io::Result<()> {
    let http_addr: String = var("OASIS_HTTP_ADDR", "127.0.0.1:8080".into());
    let dns_addr: String = var("OASIS_DNS_ADDR", "127.0.0.1:5353".into());
    let config = Config {
        title: var("OASIS_TITLE", "oasis".into()),
        origin: var("OASIS_ORIGIN", http_addr.clone()),
        aliases: env::var("OASIS_ALIASES")
            .iter()
            .flat_map(|list| list.split(','))
            .map(String::from)
            .collect(),
        data_dir: var("OASIS_DATA_DIR", "data".into()),
        admin_token: env::var("OASIS_ADMIN_TOKEN").ok(),
        mac_of,
        stations,
        space,
        https: false,
        verbose: var("OASIS_VERBOSE", false),
        workers: var("OASIS_WORKERS", 4),
        worker_stack: WORKER_STACK,
        board_bytes: var("OASIS_BOARD_BYTES", 1200 * 1000),
        segment_bytes: var("OASIS_SEGMENT_BYTES", 8000),
        index_replies: var("OASIS_INDEX_REPLIES", 0) != 0,
        max_users: var("OASIS_MAX_USERS", 100),
        max_mailboxes: var("OASIS_MAX_MAILBOXES", 100),
        mailbox_bytes: var("OASIS_MAILBOX_BYTES", 8000),
        chat_interval: Duration::from_millis(var("OASIS_CHAT_INTERVAL_MS", 1000)),
        board_interval: Duration::from_millis(var("OASIS_BOARD_INTERVAL_MS", 10_000)),
    };
    let dns_ip: Ipv4Addr = var("OASIS_DNS_IP", Ipv4Addr::LOCALHOST);
    let dns_socket = UdpSocket::bind(&dns_addr)?;
    thread::spawn(move || dns::serve(dns_socket, dns_ip));
    // the device's platform or its own wiring provides these two. here
    // they only run when a test asks for them
    if let Ok(addr) = env::var("OASIS_DHCP_ADDR") {
        let pool = dhcp::Pool {
            server: dns_ip,
            first: var("OASIS_DHCP_FIRST", 2),
            last: var("OASIS_DHCP_LAST", 254),
            lease: Duration::from_secs(var("OASIS_DHCP_LEASE_SECS", 7200)),
        };
        let socket = UdpSocket::bind(addr)?;
        thread::spawn(move || dhcp::serve(socket, pool));
    }
    if let Ok(addr) = env::var("OASIS_MDNS_ADDR") {
        let name: String = var("OASIS_MDNS_NAME", "oasis.local".into());
        let socket = UdpSocket::bind(addr)?;
        thread::spawn(move || mdns::serve(socket, &name, dns_ip));
    }
    // a host has no tls. this listener only applies the filter that decides
    // which clients the device's https listener answers, for the tests
    if let Ok(addr) = env::var("OASIS_HTTPS_ADDR") {
        let (https, names) = (TcpListener::bind(addr)?, config.aliases.clone());
        thread::spawn(move || {
            for mut stream in https.incoming().flatten() {
                if sni::addressed_to(&stream, &names).unwrap_or(false) {
                    let _ = stream.write_all(b"tls");
                }
            }
        });
    }
    let listener = TcpListener::bind(&http_addr)?;
    println!("oasis portal on http://{http_addr}/ dns on {dns_addr}");
    oasis_portal::serve(Arc::new(Portal::new(config)?), listener)
}
