//! oasis firmware: brings up the open access point and flash storage, then
//! hands over to the platform independent portal.

mod https;
mod space;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::fs::littlefs::Littlefs;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::io::vfs::MountedLittlefs;
use esp_idf_svc::ipv4::{self, Mask, RouterConfiguration, Subnet};
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{
    EspError, esp, esp_netif_dhcps_get_clients_by_mac, esp_netif_get_handle_from_ifkey,
    esp_netif_pair_mac_ip_t, esp_wifi_ap_get_sta_list, wifi_sta_list_t,
};
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, Configuration, EspWifi, WifiDriver,
};
use oasis_portal::{Config, Portal, Seed, dns};
use std::error::Error;
use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, TcpListener, UdpSocket};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// the settings file, compiled in, so that it is flashed with the program.
/// `make` checks it before the build, see `seed.rs` of the portal
const SETTINGS: &str = include_str!("../../oasis.conf");
/// name of the network when the settings give none
const SSID: &str = "OASIS";

const AP_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
const AP_PREFIX: u8 = 24;
const AP_CHANNEL: u8 = 6;
/// the most stations the ESP32 soft AP supports
const AP_MAX_CLIENTS: u16 = 10;
const HTTP_PORT: u16 = 80;
const DNS_PORT: u16 = 53;
/// reachable as `oasis.local`
const MDNS_HOST: &str = "oasis";

/// key that esp-idf-svc gives the access point interface
const AP_NETIF_KEY: &CStr = c"WIFI_AP_DEF";
const STORAGE_LABEL: &str = "storage";
const MOUNT_POINT: &str = "/data";
/// budget of the board. rounded up to littlefs blocks it takes about 1200KB
/// of the 2496KB partition. mail takes up to 830KB and accounts 70KB, which
/// leaves about 460KB for littlefs to copy on write. see the storage
/// section of DESIGN.md
const BOARD_BYTES: u64 = 1200 * 1000;
/// two 4096 byte littlefs blocks, minus the block pointers
const SEGMENT_BYTES: u64 = 8000;
const MAX_USERS: usize = 100;
/// a full mailbox is two littlefs blocks, 100 of them 800KB
const MAX_MAILBOXES: usize = 100;
const MAILBOX_BYTES: u64 = 8000;

const HTTP_WORKERS: usize = 4;
const HTTP_STACK: usize = 12 * 1024;
const DNS_STACK: usize = 4 * 1024;
const HTTPS_PORT: u16 = 443;
/// the mbedtls handshake runs on this stack
const HTTPS_STACK: usize = 12 * 1024;

type Wifi = BlockingWifi<EspWifi<'static>>;

/// starts the access point. dhcp hands out our own address as dns server
/// so that every lookup reaches the captive dns responder.
fn start_wifi(modem: Modem<'static>, ssid: &str) -> Result<Wifi, EspError> {
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let driver = WifiDriver::new(modem, sysloop.clone(), Some(nvs))?;
    let router = RouterConfiguration {
        subnet: Subnet { gateway: AP_IP, mask: Mask(AP_PREFIX) },
        dhcp_enabled: true,
        dns: Some(AP_IP),
        secondary_dns: None,
    };
    let ap_netif = EspNetif::new_with_conf(&NetifConfiguration {
        ip_configuration: Some(ipv4::Configuration::Router(router)),
        ..NetifConfiguration::wifi_default_router()
    })?;
    let wifi = EspWifi::wrap_all(driver, EspNetif::new(NetifStack::Sta)?, ap_netif)?;
    let mut wifi = BlockingWifi::wrap(wifi, sysloop)?;
    wifi.set_configuration(&Configuration::AccessPoint(AccessPointConfiguration {
        ssid: ssid.try_into().expect("ssid too long"),
        auth_method: AuthMethod::None,
        channel: AP_CHANNEL,
        max_connections: AP_MAX_CLIENTS,
        ..Default::default()
    }))?;
    wifi.start()?;
    wifi.wait_netif_up()?;
    Ok(wifi)
}

/// answers multicast lookups for our `.local` name. unlike the captive dns
/// this still works for clients whose dns settings bypass the access point.
fn start_mdns() -> Result<EspMdns, EspError> {
    let mut mdns = EspMdns::take()?;
    mdns.set_hostname(MDNS_HOST)?;
    mdns.add_service(None, "_http", "_tcp", HTTP_PORT, &[])?;
    Ok(mdns)
}

/// mounts the data partition, formatting it on first boot.
fn mount_storage() -> Result<MountedLittlefs<Littlefs<()>>, EspError> {
    let mut littlefs = unsafe { Littlefs::<()>::new_partition(STORAGE_LABEL)? };
    if MountedLittlefs::mount(&mut littlefs, MOUNT_POINT).is_err() {
        log::warn!("formatting storage");
        littlefs.format()?;
    }
    MountedLittlefs::mount(littlefs, MOUNT_POINT)
}

/// mac addresses of the stations associated with the access point.
fn stations() -> Vec<[u8; 6]> {
    let mut list = wifi_sta_list_t::default();
    let listed = esp!(unsafe { esp_wifi_ap_get_sta_list(&mut list) }).is_ok();
    let count = if listed { list.num as usize } else { 0 };
    list.sta[..count].iter().map(|station| station.mac).collect()
}

/// mac address of the station holding the dhcp lease for `ip`. None for
/// clients that configured their address by hand.
fn mac_of(ip: IpAddr) -> Option<[u8; 6]> {
    let IpAddr::V4(ip) = ip else { return None };
    let pair = |mac: [u8; 6]| esp_netif_pair_mac_ip_t { mac, ..Default::default() };
    let mut pairs: Vec<_> = stations().into_iter().map(pair).collect();
    let netif = unsafe { esp_netif_get_handle_from_ifkey(AP_NETIF_KEY.as_ptr()) };
    esp!(unsafe { esp_netif_dhcps_get_clients_by_mac(netif, pairs.len() as i32, pairs.as_mut_ptr()) })
        .ok()?;
    // lwip keeps addresses in network byte order
    let wanted = u32::from_ne_bytes(ip.octets());
    pairs.iter().find(|pair| pair.ip.addr == wanted).map(|pair| pair.mac)
}

fn config(ssid: &str) -> Config {
    Config {
        title: ssid.into(),
        origin: AP_IP.to_string(),
        aliases: vec![format!("{MDNS_HOST}.local")],
        data_dir: MOUNT_POINT.into(),
        mac_of,
        stations,
        space: space::space,
        https: true,
        verbose: true,
        workers: HTTP_WORKERS,
        worker_stack: HTTP_STACK,
        board_bytes: BOARD_BYTES,
        segment_bytes: SEGMENT_BYTES,
        index_replies: false,
        max_users: MAX_USERS,
        max_mailboxes: MAX_MAILBOXES,
        mailbox_bytes: MAILBOX_BYTES,
        chat_interval: Duration::from_secs(1),
        board_interval: Duration::from_secs(10),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    // a file that the build check let through parses. should it not, the
    // device still comes up, with defaults
    let seed = oasis_portal::parse_seed(SETTINGS).unwrap_or_else(|err| {
        log::error!("settings ignored: {err}");
        Seed::default()
    });
    let ssid = seed.ssid.as_deref().unwrap_or(SSID);
    let _wifi = start_wifi(Peripherals::take()?.modem, ssid)?;
    let _mdns = start_mdns()?;
    space::measure_firmware();
    let storage = mount_storage()?;
    log::info!("storage: {:?}", storage.info()?);
    let config = config(ssid);
    let names = config.aliases.clone();
    let portal = Arc::new(Portal::new(config, &seed)?);
    let dns_socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, DNS_PORT))?;
    thread::Builder::new().stack_size(DNS_STACK).spawn(move || dns::serve(dns_socket, AP_IP))?;
    let https_listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, HTTPS_PORT))?;
    let https = thread::Builder::new().stack_size(HTTPS_STACK);
    https.spawn(move || https::serve(https_listener, format!("http://{AP_IP}/"), names))?;
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, HTTP_PORT))?;
    let free_heap = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() };
    log::info!("oasis \"{ssid}\" serving http://{AP_IP}/ with {free_heap} bytes of heap free");
    Ok(oasis_portal::serve(portal, listener)?)
}
