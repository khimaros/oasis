//! platform independent portal: captive dns, http server, ephemeral chat,
//! persistent message logs, and peer signaling. uses only std so that the
//! same code runs on every device and on a development host. dhcp and mdns
//! are here for the platforms that do not bring their own.

pub mod board;
pub mod chat;
pub mod crypto;
pub mod dhcp;
pub mod dns;
pub mod events;
pub mod http;
pub mod mail;
pub mod mdns;
pub mod peers;
mod routes;
pub mod seed;
pub mod sni;
pub mod store;
pub mod text;
pub mod users;

use board::Topic;
use chat::Chat;
use events::Events;
use mail::Mail;
use peers::Peers;
pub use seed::Seed;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use store::Usage;
use users::{User, Users, default_name};

pub(crate) const CHAT_CAPACITY: usize = 50;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// connections that have been accepted and have not sent anything yet.
/// with the workers and the listeners they fit the sockets of the ESP32
const MAX_WAITING: usize = 6;
/// how often the waiting connections are checked for a request
const DISPATCH_INTERVAL: Duration = Duration::from_millis(10);
const MAX_DRAIN_BYTES: u64 = 256 * 1024;
/// 2026-01-01. client clocks earlier than this are considered unset.
const MIN_CLOCK: u64 = 1_767_225_600;
const MAIL_DIR: &str = "mail";
const MAX_RELEASED: usize = 32;
const MAX_GUESTS: usize = 64;
/// name for clients whose device could not be identified
const UNKNOWN_NAME: &str = "anon";
/// board topics, each stored in a directory of the same name. `index.html`
/// holds their labels and descriptions.
const TOPICS: [&str; 5] = ["general", "events", "marketplace", "lost", "intros"];
/// directories of earlier layouts, removed at startup to free their space
const LEGACY_DIRS: [&str; 7] = ["news", "board", "general", "events", "marketplace", "lost", "intros"];
/// holds the logs of every topic
const TOPICS_DIR: &str = "topics";
/// present once the threads of the settings file were started
const SEEDED_FILE: &str = "seeded";

/// reads a settings file, see `seed.rs`. the error names what is wrong.
pub fn parse_seed(text: &str) -> Result<Seed, String> {
    seed::parse(text, &TOPICS)
}

/// a connection and the address of its client
type Client = (TcpStream, IpAddr);
/// a connection without a request so far, and since when
type Waiting = (TcpStream, IpAddr, Instant);

/// bytes in use on a partition of the platform.
pub struct Space {
    pub name: &'static str,
    pub used: u64,
    pub size: u64,
}

pub struct Config {
    pub title: String,
    /// value of the Host header for the portal itself. requests for any
    /// other host are captive portal probes and get redirected here.
    pub origin: String,
    /// friendlier Host values that also reach the portal, e.g. its mDNS
    /// name. these are served directly and shown to users ahead of `origin`.
    pub aliases: Vec<String>,
    pub data_dir: PathBuf,
    /// hardware address of the client at an ip address, when the platform
    /// can tell. identifies devices for names and accounts.
    pub mac_of: fn(IpAddr) -> Option<[u8; 6]>,
    /// hardware addresses of the devices that are connected to the network,
    /// whether or not they have the page open.
    pub stations: fn() -> Vec<[u8; 6]>,
    /// how full each partition of the platform is, for the status page.
    pub space: fn() -> Vec<Space>,
    /// whether the platform also answers https on `origin`, with a
    /// certificate that no client trusts. the page then offers android
    /// sign-in windows a link to it, as their way into the real browser.
    pub https: bool,
    /// logs captive portal traffic to stderr, which is the serial console
    /// on the device
    pub verbose: bool,
    pub workers: usize,
    pub worker_stack: usize,
    /// storage shared evenly between the board topics
    pub board_bytes: u64,
    pub segment_bytes: u64,
    /// keeps an index of the replies by thread. worth its RAM once the
    /// reply log is too large to scan for the replies of one thread.
    pub index_replies: bool,
    /// the oldest account makes room for one more than this
    pub max_users: usize,
    /// the mailbox unused for longest makes room for one more than this
    pub max_mailboxes: usize,
    pub mailbox_bytes: u64,
    /// minimum time between posts from one address
    pub chat_interval: Duration,
    pub board_interval: Duration,
}

pub struct Portal {
    pub(crate) config: Config,
    started: Instant,
    /// unix time at boot, learned from clients since there is no RTC.
    /// zero while unknown.
    boot_unix: Mutex<u64>,
    pub(crate) chat: Mutex<Chat>,
    pub(crate) peers: Mutex<Peers>,
    pub(crate) users: Mutex<Users>,
    pub(crate) mail: Mutex<Mail>,
    topics: Vec<(&'static str, Mutex<Topic>)>,
    /// last post per address and kind. bounded by the size of the subnet.
    posted: Mutex<HashMap<(IpAddr, &'static str), Instant>>,
    /// clients that pressed "continue", with the device seen at the time so
    /// that a reused address does not inherit it. their connectivity probes
    /// are answered with success. forgotten on reboot.
    released: Mutex<VecDeque<(IpAddr, Option<u64>)>>,
    /// devices of guests seen lately, most recent last. lets mail find a
    /// guest by its generated name.
    guests: Mutex<VecDeque<u64>>,
    pub(crate) events: Mutex<Events>,
}

/// name that posts are signed with: the account's, else one generated for
/// the device.
pub(crate) fn display_name(device: Option<u64>, user: Option<&User>) -> String {
    match (user, device) {
        (Some(user), _) => user.name.clone(),
        (None, Some(device)) => default_name(device),
        (None, None) => UNKNOWN_NAME.into(),
    }
}

impl Portal {
    /// opens the stores under `config.data_dir` and applies the settings
    /// file: its accounts every time, its threads once.
    pub fn new(config: Config, seed: &Seed) -> io::Result<Portal> {
        LEGACY_DIRS.iter().for_each(|dir| drop(fs::remove_dir_all(config.data_dir.join(dir))));
        let share = config.board_bytes / TOPICS.len() as u64;
        let open = |topic: &'static str| {
            let dir = config.data_dir.join(TOPICS_DIR);
            let opened = Topic::open(&dir, topic, share, config.segment_bytes, config.index_replies)?;
            Ok((topic, Mutex::new(opened)))
        };
        let topics = TOPICS.into_iter().map(open).collect::<io::Result<Vec<_>>>()?;
        let mut users = Users::open(&config.data_dir, config.max_users)?;
        users.build_in_all(&seed.users)?;
        let portal = Portal {
            started: Instant::now(),
            boot_unix: Mutex::default(),
            chat: Mutex::new(Chat::new(CHAT_CAPACITY)),
            peers: Mutex::default(),
            users: Mutex::new(users),
            mail: Mutex::new(Mail::open(
                config.data_dir.join(MAIL_DIR),
                config.max_mailboxes,
                config.mailbox_bytes,
            )),
            guests: Mutex::default(),
            events: Mutex::default(),
            topics,
            posted: Mutex::default(),
            released: Mutex::default(),
            config,
        };
        portal.start_threads(&seed.threads).map(|()| portal)
    }

    /// starts the threads of the settings file on a board that has not had
    /// them. a marker file remembers that, so that the ones that were
    /// deleted or evicted since do not come back.
    fn start_threads(&self, threads: &[seed::Thread]) -> io::Result<()> {
        let marker = self.config.data_dir.join(SEEDED_FILE);
        if marker.exists() {
            return Ok(());
        }
        for thread in threads {
            let Some(topic) = self.topic(&thread.topic) else { continue };
            let mut topic = topic.lock().unwrap();
            let id = topic.start(self.now(), &thread.author, &thread.subject, &thread.text)?;
            if thread.pinned {
                topic.pin(id, true)?;
            }
        }
        fs::write(marker, "")
    }

    /// id of the device behind `ip`. None when its mac is unknown.
    pub(crate) fn device(&self, ip: IpAddr) -> Option<u64> {
        (self.config.mac_of)(ip).map(|mac| self.users.lock().unwrap().device(mac))
    }

    /// the device a request comes from, and the account its `session`
    /// parameter proves a login for, if any.
    pub(crate) fn identify(&self, req: &http::Request) -> (Option<u64>, Option<User>) {
        let user = self.users.lock().unwrap().verify(req.param("session")).cloned();
        (self.device(req.peer), user)
    }

    /// remembers the device of a guest.
    pub(crate) fn note_guest(&self, device: u64) {
        let mut guests = self.guests.lock().unwrap();
        guests.retain(|other| *other != device);
        if guests.len() >= MAX_GUESTS {
            guests.pop_front();
        }
        guests.push_back(device);
    }

    /// the device last seen under the generated `name`, ignoring case.
    pub(crate) fn find_guest(&self, name: &str) -> Option<u64> {
        let lower = name.to_lowercase();
        self.guests.lock().unwrap().iter().rev().copied().find(|device| default_name(*device) == lower)
    }

    /// ends the captive state for the client at `ip`.
    pub(crate) fn release(&self, ip: IpAddr) {
        let entry = (ip, self.device(ip));
        let mut released = self.released.lock().unwrap();
        released.retain(|(other, _)| *other != ip);
        if released.len() >= MAX_RELEASED {
            released.pop_front();
        }
        released.push_back(entry);
    }

    pub(crate) fn is_released(&self, ip: IpAddr) -> bool {
        let entry = (ip, self.device(ip));
        self.released.lock().unwrap().contains(&entry)
    }

    /// how full the thread logs and the reply logs of all topics are.
    pub(crate) fn board_usage(&self) -> (Usage, Usage) {
        let usages = self.topics.iter().map(|(_, topic)| topic.lock().unwrap().usage());
        usages.fold(Default::default(), |(threads, replies), (t, r)| (threads.plus(t), replies.plus(r)))
    }

    /// the json object of how many threads each topic holds.
    pub(crate) fn thread_counts(&self) -> String {
        let count =
            |(name, topic): &(&str, Mutex<Topic>)| format!("\"{name}\":{}", topic.lock().unwrap().threads());
        format!("{{{}}}", self.topics.iter().map(count).collect::<Vec<_>>().join(","))
    }

    pub(crate) fn topic(&self, name: &str) -> Option<&Mutex<Topic>> {
        self.topics.iter().find(|(topic, _)| *topic == name).map(|(_, store)| store)
    }

    /// unix time, or zero until a client has told us.
    pub(crate) fn now(&self) -> u64 {
        match *self.boot_unix.lock().unwrap() {
            0 => 0,
            boot => boot + self.started.elapsed().as_secs(),
        }
    }

    /// adopts a client supplied time. untrusted clients only set the clock
    /// while it is unknown.
    pub(crate) fn sync_clock(&self, claimed: u64, trusted: bool) {
        if claimed >= MIN_CLOCK && (trusted || self.now() == 0) {
            *self.boot_unix.lock().unwrap() = claimed - self.started.elapsed().as_secs();
        }
    }

    /// rate limit: true when `ip` last posted `kind` at least `interval` ago.
    pub(crate) fn allow(&self, ip: IpAddr, kind: &'static str, interval: Duration) -> bool {
        let mut posted = self.posted.lock().unwrap();
        let now = Instant::now();
        let recent = posted.get(&(ip, kind)).is_some_and(|at| now.duration_since(*at) < interval);
        if !recent {
            posted.insert((ip, kind), now);
        }
        !recent
    }
}

fn connection(portal: &Portal, stream: TcpStream, peer: IpAddr) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut out = BufWriter::new(&stream);
    match http::read_request(&mut &stream, peer) {
        Ok(request) => routes::handle(portal, &request, &mut out)?,
        Err(err) if err.kind() == io::ErrorKind::InvalidData => {
            http::send(&mut out, http::BAD_REQUEST, http::TEXT, &err.to_string())?;
            out.flush()?;
            return drain(&stream);
        }
        Err(err) => return Err(err),
    }
    out.flush()
}

/// discards the rest of a rejected request. closing a socket with unread
/// input resets the connection before the client has seen our reply.
fn drain(stream: &TcpStream) -> io::Result<()> {
    stream.shutdown(Shutdown::Write)?;
    io::copy(&mut Read::take(stream, MAX_DRAIN_BYTES), &mut io::sink()).map(drop)
}

fn worker(portal: &Portal, ready: &Mutex<Receiver<Client>>) {
    loop {
        let next = ready.lock().unwrap().recv();
        let Ok((stream, peer)) = next else { return };
        drop(connection(portal, stream, peer));
    }
}

/// accepts what is pending. beyond `MAX_WAITING`, the connection that has
/// waited for longest is closed.
fn accept_pending(listener: &TcpListener, waiting: &mut VecDeque<Waiting>) {
    while let Ok((stream, peer)) = listener.accept() {
        if stream.set_nonblocking(true).is_err() {
            continue;
        }
        if waiting.len() >= MAX_WAITING {
            waiting.pop_front();
        }
        waiting.push_back((stream, peer.ip(), Instant::now()));
    }
}

/// hands the connections that have sent something to a free worker, and
/// closes those that hung up or stayed silent for too long.
fn dispatch(waiting: &mut VecDeque<Waiting>, ready: &SyncSender<Client>) {
    for _ in 0..waiting.len() {
        let Some((stream, peer, since)) = waiting.pop_front() else { return };
        match stream.peek(&mut [0]) {
            Ok(0) => {}
            Ok(_) => {
                if let Err(TrySendError::Full((stream, peer))) = ready.try_send((stream, peer)) {
                    waiting.push_back((stream, peer, since));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock && since.elapsed() < IO_TIMEOUT => {
                waiting.push_back((stream, peer, since));
            }
            Err(_) => {}
        }
    }
}

/// serves http forever from a fixed pool of worker threads, which bounds
/// memory use on the device. browsers open connections ahead of time and
/// leave them unused, so a connection only gets a worker once its request
/// starts to arrive.
pub fn serve(portal: Arc<Portal>, listener: TcpListener) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let (ready, queue) = mpsc::sync_channel(0);
    let queue = Arc::new(Mutex::new(queue));
    for index in 0..portal.config.workers {
        let (portal, queue) = (portal.clone(), queue.clone());
        let builder = thread::Builder::new().name(format!("http{index}"));
        builder.stack_size(portal.config.worker_stack).spawn(move || worker(&portal, &queue))?;
    }
    let mut waiting = VecDeque::with_capacity(MAX_WAITING);
    loop {
        accept_pending(&listener, &mut waiting);
        dispatch(&mut waiting, &ready);
        thread::sleep(DISPATCH_INTERVAL);
    }
}
