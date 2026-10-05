//! oasis as the first process of a raspberry pi 4: loads the wifi driver,
//! brings up the open access point and the storage on the sd card, then
//! hands over to the platform independent portal.

mod https;
mod init;
mod link;
mod netlink;
mod wifi;

use link::Link;
use netlink::Socket;
use oasis_portal::{Config, Portal, Seed, Space, dhcp, dns, mdns};
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, TcpListener, UdpSocket};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;
use wifi::Wifi;

const SSID: &str = "OASIS";
const AP_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
const AP_PREFIX: u8 = 24;
const AP_CHANNEL: u8 = 6;
const HTTP_PORT: u16 = 80;
const HTTPS_PORT: u16 = 443;
const DNS_PORT: u16 = 53;
const DHCP_PORT: u16 = 67;
const DHCP_FIRST: u8 = 2;
const DHCP_LAST: u8 = 254;
const DHCP_LEASE: Duration = Duration::from_secs(2 * 60 * 60);
const MDNS_HOST: &str = "oasis.local";
/// mdns packets must not have crossed a router
const MDNS_TTL: u32 = 255;

/// the kernel runs us under this name to load a module that a driver asks for
const MODPROBE: &str = "modprobe";
const WIFI_MODULE: &str = "brcmfmac";
const INTERFACE: &str = "wlan0";
/// label of the data partition, as set by tools/rpi_image.py
const DATA_LABEL: &str = "oasis";
const DATA_FILESYSTEM: &str = "ext4";
const MOUNT_POINT: &str = "/data";
/// certificate and key of the https listener, put there by the image build
const TLS_DIR: &str = "/tls";
/// the settings file, put there by the image build
const SETTINGS_FILE: &str = "/oasis.conf";
const CMDLINE_FILE: &str = "/proc/cmdline";
const ARP_FILE: &str = "/proc/net/arp";
const MEMORY_FILE: &str = "/proc/meminfo";
/// flags of an arp entry that has no mac address yet
const ARP_INCOMPLETE: &str = "0x0";
const DEVICE_TIMEOUT: Duration = Duration::from_secs(30);
const REBOOT_DELAY: Duration = Duration::from_secs(10);

/// the board gets one part in this many of the data partition
const BOARD_SHARE: u64 = 2;
/// and all mailboxes together one part in this many. the rest is left for
/// accounts and for the file system, which rounds files up to its blocks
const MAIL_SHARE: u64 = 4;
const MAILBOX_BYTES: u64 = 64_000;
const MEGABYTE: u64 = 1024 * 1024;
const SEGMENT_BYTES: u64 = 8000;
const MAX_USERS: usize = 10_000;
const HTTP_WORKERS: usize = 16;
const HTTP_STACK: usize = 256 * 1024;

static WIFI: OnceLock<Wifi> = OnceLock::new();

/// what the kernel command line can set, e.g. `oasis.ssid="base camp"` in
/// `cmdline.txt` on the boot partition. it goes before the settings file.
struct Settings {
    ssid: String,
    interface: String,
}

/// the value of `key` on the kernel command line. a value with spaces is
/// quoted.
fn setting(cmdline: &str, key: &str) -> Option<String> {
    let mut quoted = false;
    let mut words = cmdline.split(|c: char| {
        quoted ^= c == '"';
        c.is_whitespace() && !quoted
    });
    words.find_map(|word| word.strip_prefix(key)?.strip_prefix('=').map(|value| value.replace('"', "")))
}

fn settings(cmdline: &str, seed: &Seed) -> Settings {
    let or = |key, default: &str| setting(cmdline, key).unwrap_or_else(|| default.into());
    Settings {
        ssid: or("oasis.ssid", seed.ssid.as_deref().unwrap_or(SSID)),
        interface: or("oasis.interface", INTERFACE),
    }
}

/// the settings file that the image build put next to us. should it be
/// missing or wrong, the device still comes up, with defaults.
fn seed() -> Seed {
    let parsed = fs::read_to_string(SETTINGS_FILE).map_err(|err| err.to_string());
    parsed.and_then(|text| oasis_portal::parse_seed(&text)).unwrap_or_else(|err| {
        eprintln!("oasis: {SETTINGS_FILE} ignored: {err}");
        Seed::default()
    })
}

/// brings the interface up with the portal's address. a wifi interface
/// becomes an open access point. any other kind, such as ethernet, serves
/// whoever is plugged into it.
fn start_network(settings: &Settings) -> io::Result<()> {
    if let Err(err) = init::modprobe(WIFI_MODULE) {
        eprintln!("oasis: wifi driver: {err}");
    }
    init::wait_for(&settings.interface, DEVICE_TIMEOUT, || Link::exists(&settings.interface))?;
    let link = Link::find(&settings.interface)?;
    let mut route = Socket::open(libc::NETLINK_ROUTE)?;
    let wifi = link.is_wireless().then(|| Wifi::open(&link)).transpose()?;
    wifi.iter().try_for_each(Wifi::become_access_point)?;
    link.set_up(&mut route)?;
    link.set_address(&mut route, AP_IP, AP_PREFIX)?;
    if let Some(wifi) = wifi {
        wifi.start(&link, &settings.ssid, AP_CHANNEL)?;
        let _ = WIFI.set(wifi);
    }
    Ok(())
}

/// mounts the data partition of the sd card, which the image build labels.
fn mount_storage() -> io::Result<()> {
    let find = || init::find_filesystem(DATA_LABEL);
    init::wait_for("the data partition", DEVICE_TIMEOUT, || find().is_some())?;
    let device = find().ok_or_else(|| io::Error::other("the data partition is gone"))?;
    init::mount(&device.to_string_lossy(), MOUNT_POINT, DATA_FILESYSTEM, libc::MS_NOATIME)
}

fn stations() -> Vec<[u8; link::MAC_BYTES]> {
    WIFI.get().map(Wifi::stations).unwrap_or_default()
}

/// mac address of the client at `ip`, from the kernel's arp table. it knows
/// every client that has sent us a packet.
fn mac_of(ip: IpAddr) -> Option<[u8; link::MAC_BYTES]> {
    let (table, ip) = (fs::read_to_string(ARP_FILE).ok()?, ip.to_string());
    // address, hardware type, flags, mac address, mask, interface
    let entries = table.lines().map(|line| line.split_whitespace().collect::<Vec<_>>());
    let known =
        |entry: &Vec<&str>| entry.first() == Some(&ip.as_str()) && entry.get(2) != Some(&ARP_INCOMPLETE);
    entries.filter(known).find_map(|entry| link::parse_mac(entry.get(3)?))
}

/// RAM in use and in total, from the kernel's counters in kB.
fn memory() -> Option<Space> {
    let counters = fs::read_to_string(MEMORY_FILE).ok()?;
    let bytes = |name: &str| {
        let value = counters.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))?;
        value.split_whitespace().next()?.parse::<u64>().ok().map(|kilobytes| kilobytes * 1024)
    };
    let (size, available) = (bytes("MemTotal")?, bytes("MemAvailable")?);
    Some(Space { name: "memory", used: size - available, size })
}

/// the data partition of the sd card, and the RAM.
fn space() -> Vec<Space> {
    let storage =
        init::filesystem(MOUNT_POINT).ok().map(|(used, size)| Space { name: "storage", used, size });
    [storage, memory()].into_iter().flatten().collect()
}

/// the limits grow with the data partition of the card.
fn config(settings: &Settings, https: bool) -> io::Result<Config> {
    let (_, card) = init::filesystem(MOUNT_POINT)?;
    Ok(Config {
        title: settings.ssid.clone(),
        origin: AP_IP.to_string(),
        aliases: vec![MDNS_HOST.into()],
        data_dir: MOUNT_POINT.into(),
        mac_of,
        stations,
        space,
        https,
        verbose: true,
        workers: HTTP_WORKERS,
        worker_stack: HTTP_STACK,
        board_bytes: card / BOARD_SHARE,
        segment_bytes: SEGMENT_BYTES,
        index_replies: true,
        max_users: MAX_USERS,
        max_mailboxes: (card / MAIL_SHARE / MAILBOX_BYTES) as usize,
        mailbox_bytes: MAILBOX_BYTES,
        chat_interval: Duration::from_secs(1),
        board_interval: Duration::from_secs(10),
    })
}

/// a udp socket on `port` that only talks to `interface`. broadcast and
/// multicast replies then leave through it without a route.
fn udp_on(interface: &str, port: u16) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
    let (name, length) = (interface.as_ptr().cast(), interface.len() as libc::socklen_t);
    let bound = unsafe {
        libc::setsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_BINDTODEVICE, name, length)
    };
    if bound == 0 { Ok(socket) } else { Err(io::Error::last_os_error()) }
}

/// starts what ESP-IDF provides on the ESP32, the dhcp server and the mdns
/// responder, and the captive dns.
fn start_services(settings: &Settings) -> io::Result<()> {
    let dns_socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DNS_PORT))?;
    thread::spawn(move || dns::serve(dns_socket, AP_IP));
    let pool = dhcp::Pool { server: AP_IP, first: DHCP_FIRST, last: DHCP_LAST, lease: DHCP_LEASE };
    let dhcp_socket = udp_on(&settings.interface, DHCP_PORT)?;
    thread::spawn(move || dhcp::serve(dhcp_socket, pool));
    let mdns_socket = udp_on(&settings.interface, mdns::PORT)?;
    mdns_socket.join_multicast_v4(&mdns::GROUP, &AP_IP)?;
    mdns_socket.set_multicast_ttl_v4(MDNS_TTL)?;
    thread::spawn(move || mdns::serve(mdns_socket, MDNS_HOST, AP_IP));
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    init::mount_system()?;
    let seed = seed();
    let settings = settings(&fs::read_to_string(CMDLINE_FILE)?, &seed);
    start_network(&settings)?;
    mount_storage()?;
    start_services(&settings)?;
    let tls = https::config(Path::new(TLS_DIR)).inspect_err(|err| eprintln!("oasis: no https: {err}")).ok();
    let config = config(&settings, tls.is_some())?;
    eprintln!(
        "oasis: room for {} MB of board, {} accounts, {} mailboxes",
        config.board_bytes / MEGABYTE,
        config.max_users,
        config.max_mailboxes
    );
    let portal = Arc::new(Portal::new(config, &seed)?);
    if let Some(tls) = tls {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, HTTPS_PORT))?;
        let names = vec![MDNS_HOST.to_string()];
        thread::spawn(move || https::serve(listener, tls, names, format!("http://{AP_IP}/")));
    }
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, HTTP_PORT))?;
    eprintln!("oasis \"{}\" serving http://{AP_IP}/ on {}", settings.ssid, settings.interface);
    Ok(oasis_portal::serve(portal, listener)?)
}

fn main() {
    if env::args().next().is_some_and(|name| name.ends_with(MODPROBE)) {
        // called as `modprobe -q -- <module>`
        let loaded = init::modprobe(&env::args().next_back().unwrap_or_default());
        process::exit(loaded.is_err().into());
    }
    if process::id() != 1 {
        eprintln!("oasis-rpi is the init of the device image, see `make rpi-image`");
        process::exit(1);
    }
    if let Err(err) = run() {
        eprintln!("oasis: {err}");
    }
    // the kernel panics when its first process ends. start over instead
    thread::sleep(REBOOT_DELAY);
    init::reboot();
}
