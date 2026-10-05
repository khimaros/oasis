//! http routing and the json api.

use crate::board::{Pin, Topic};
use crate::events::Reply;
use crate::http::{self, Request};
use crate::mail::{MAIL_MAX, Notice};
use crate::peers::{MAX_SIGNAL_BYTES, Peer, Signal};
use crate::store::{Entry, Page, Usage};
use crate::text::{clean, json};
use crate::users::{Granted, Rejected, User, default_name, is_generated};
use crate::{CHAT_CAPACITY, Portal, Space, display_name};
use std::io::{self, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const INDEX: &str = include_str!("index.html");
const KEY_MAX: usize = 64;
const INVALID_ACCOUNT: &str =
    "username: up to 24 characters. password: 6 to 64 characters. description: up to 160 characters";
/// bodies that apple and firefox compare their probe results against
const APPLE_SUCCESS: &str = "<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>";
const FIREFOX_CANONICAL: &str =
    "<meta http-equiv=\"refresh\" content=\"0;url=https://support.mozilla.org/kb/captive-portal\"/>";
const CHAT_MAX: usize = 280;
const BOARD_MAX: usize = 2000;
const SUBJECT_MAX: usize = 80;

pub fn handle(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let config = &portal.config;
    if req.host != config.origin && !config.aliases.contains(&req.host) {
        let reply = probe_reply(&req.path).filter(|_| portal.is_released(req.peer));
        if config.verbose {
            let status = reply.map_or(http::FOUND, |(status, _, _)| status);
            eprintln!("captive: {} asks {}{} -> {status}", req.peer, req.host, req.path);
        }
        return match reply {
            Some((status, content_type, body)) => http::send(out, status, content_type, body),
            None => http::redirect(out, &format!("http://{}/", config.origin)),
        };
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => http::send(out, http::OK, http::HTML, INDEX),
        ("GET", "/api/poll") => poll(portal, req, out),
        ("POST", "/api/signal") => signal(portal, req, out),
        ("POST", "/api/register") => register(portal, req, out),
        ("POST", "/api/login") => login(portal, req, out),
        ("POST", "/api/profile") => profile(portal, req, out),
        ("GET", "/api/user") => user_info(portal, req, out),
        ("GET", "/api/status") => status(portal, out),
        ("POST", "/api/release") => {
            portal.release(req.peer);
            if config.verbose {
                eprintln!("captive: {} released, device {:?}", req.peer, portal.device(req.peer));
            }
            http::send(out, http::OK, http::JSON, "{}")
        }
        ("GET", "/api/mail") => mail_read(portal, req, out),
        ("POST", "/api/mail") => mail_send(portal, req, out),
        ("GET", "/api/board") => with_topic(portal, req, out, threads),
        ("POST", "/api/board") => with_topic(portal, req, out, thread_post),
        ("GET", "/api/thread") => with_topic(portal, req, out, replies),
        ("POST", "/api/reply") => with_topic(portal, req, out, reply_post),
        ("POST", "/api/delete") if is_admin(portal, req) => with_topic(portal, req, out, delete),
        ("POST", "/api/pin") if is_admin(portal, req) => with_topic(portal, req, out, pin),
        ("POST", "/api/admin") if is_admin(portal, req) => grant(portal, req, out),
        ("POST", "/api/delete" | "/api/pin" | "/api/admin") => {
            http::send(out, http::FORBIDDEN, http::TEXT, "admin only")
        }
        ("POST", "/api/chat") => {
            post(portal, req, out, "chat", CHAT_MAX, config.chat_interval, |ts, name, text| {
                Ok(portal.chat.lock().unwrap().push(ts, name, text))
            })
        }
        _ => http::send(out, http::NOT_FOUND, http::TEXT, "not found"),
    }
}

/// the reply that tells an operating system's connectivity probe that it
/// is online: (status, content type, body). None when `path` is no probe.
/// released clients get these, which closes their captive sign-in window
/// and keeps them on the network.
fn probe_reply(path: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match path {
        "/generate_204" | "/gen_204" => Some((http::NO_CONTENT, http::TEXT, "")),
        "/hotspot-detect.html" | "/library/test/success.html" => Some((http::OK, http::HTML, APPLE_SUCCESS)),
        "/connecttest.txt" => Some((http::OK, http::TEXT, "Microsoft Connect Test")),
        "/ncsi.txt" => Some((http::OK, http::TEXT, "Microsoft NCSI")),
        "/success.txt" => Some((http::OK, http::TEXT, "success\n")),
        "/canonical.html" => Some((http::OK, http::HTML, FIREFOX_CANONICAL)),
        _ => None,
    }
}

/// runs a board handler on the topic named by the `topic` parameter.
fn with_topic<W: Write>(
    portal: &Portal,
    req: &Request,
    out: &mut W,
    handler: impl FnOnce(&Portal, &Mutex<Topic>, &Request, &mut W) -> io::Result<()>,
) -> io::Result<()> {
    match portal.topic(req.param("topic")) {
        Some(topic) => handler(portal, topic, req, out),
        None => http::send(out, http::NOT_FOUND, http::TEXT, "unknown topic"),
    }
}

/// one page of a topic's threads, in the format of `send_page`, with the
/// number of replies of each. the client parses it, drops deleted threads,
/// and puts pinned ones first.
fn threads(_: &Portal, topic: &Mutex<Topic>, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let (page, pinned, counts) = topic.lock().unwrap().page(req.param("segment").parse().ok())?;
    send_page(out, page, &pinned, &counts)
}

/// starts a thread from `subject` and `text`.
fn thread_post(portal: &Portal, topic: &Mutex<Topic>, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let subject = req.param("subject").trim();
    if clean(subject, SUBJECT_MAX, false).is_none_or(|cleaned| cleaned != subject) {
        return http::send(out, http::BAD_REQUEST, http::TEXT, "subject empty or too long");
    }
    post(portal, req, out, "board", BOARD_MAX, portal.config.board_interval, |ts, name, text| {
        topic.lock().unwrap().start(ts, name, subject, text)
    })
}

/// the replies to `thread` after reply `after`: the id of the reply to
/// continue after as a u32, zero when these were the last, then the
/// records, each preceded by its id as a u32.
fn replies(_: &Portal, topic: &Mutex<Topic>, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let (thread, after) = (req.param("thread").parse().unwrap_or(0), req.param("after").parse().unwrap_or(0));
    let (records, following) = topic.lock().unwrap().replies(thread, after)?;
    http::head(out, http::OK, http::BINARY)?;
    out.write_all(&(following.unwrap_or(0) as u32).to_le_bytes())?;
    out.write_all(&records)
}

fn reply_post(portal: &Portal, topic: &Mutex<Topic>, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let thread = req.param("thread").parse().ok().filter(|id| topic.lock().unwrap().has_thread(*id));
    let Some(thread) = thread else {
        return http::send(out, http::NOT_FOUND, http::TEXT, "no such thread");
    };
    post(portal, req, out, "board", BOARD_MAX, portal.config.board_interval, |ts, name, text| {
        let id = topic.lock().unwrap().reply(ts, name, thread, text)?;
        portal.events.lock().unwrap().push(req.param("topic"), thread, id, name, text);
        Ok(id)
    })
}

fn reply_json(reply: &Reply) -> String {
    let (topic, name, text) = (json(&reply.topic), json(&reply.name), json(&reply.excerpt));
    let Reply { id, thread, reply, .. } = reply;
    format!(r#"{{"id":{id},"topic":{topic},"thread":{thread},"reply":{reply},"name":{name},"text":{text}}}"#)
}

/// the json fields announcing replies newer than the client's `events`
/// cursor. a client that sends none only learns where to start.
fn events_json(portal: &Portal, req: &Request) -> String {
    let events = portal.events.lock().unwrap();
    let cursor = req.param("events").parse::<u64>().ok();
    let replies: Vec<&Reply> = cursor.map(|cursor| events.since(cursor).collect()).unwrap_or_default();
    format!(r#""events":{},"replies":{}"#, events.last(), list_json(&replies, |reply| reply_json(reply)))
}

/// pins or unpins thread `id`, depending on `pinned`.
fn pin(_: &Portal, topic: &Mutex<Topic>, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let (thread, pinned) = (req.param("id").parse().unwrap_or(0), req.param("pinned") == "1");
    match topic.lock().unwrap().pin(thread, pinned)? {
        Pin::Done => http::send(out, http::OK, http::JSON, "{}"),
        Pin::Unknown => http::send(out, http::NOT_FOUND, http::TEXT, "no such thread"),
        Pin::Full => http::send(out, http::BAD_REQUEST, http::TEXT, "too many pinned threads"),
    }
}

/// true when the requester is logged in as an admin. an admin's clock is
/// trusted over other clients.
fn is_admin(portal: &Portal, req: &Request) -> bool {
    let admin = portal.identify(req).1.is_some_and(|user| user.admin);
    if admin {
        portal.sync_clock(req.param("now").parse().unwrap_or(0), true);
    }
    admin
}

/// makes the account `username` an admin, or ends that, depending on `admin`.
fn grant(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    match portal.users.lock().unwrap().set_admin(req.param("username"), req.param("admin") == "1")? {
        Granted::Done => http::send(out, http::OK, http::JSON, "{}"),
        Granted::Unknown => http::send(out, http::NOT_FOUND, http::TEXT, "no such user"),
        Granted::BuiltIn => http::send(out, http::CONFLICT, http::TEXT, "the built in admin stays"),
    }
}

fn list_json<T>(items: &[T], item: impl Fn(&T) -> String) -> String {
    format!("[{}]", items.iter().map(item).collect::<Vec<_>>().join(","))
}

fn entry_json(entry: &Entry) -> String {
    let (name, text) = (json(&entry.name), json(&entry.text));
    format!(r#"{{"id":{},"ts":{},"name":{name},"text":{text}}}"#, entry.id, entry.ts)
}

fn peer_json(peer: &Peer) -> String {
    format!(r#"{{"id":{},"name":{}}}"#, peer.id, json(&peer.name))
}

fn signal_json(signal: &Signal) -> String {
    let (ip, data) = (json(&signal.ip.to_string()), json(&signal.data));
    format!(r#"{{"from":{},"ip":{ip},"data":{data}}}"#, signal.from)
}

fn option_json(value: Option<u64>) -> String {
    value.map_or("null".into(), |value| value.to_string())
}

/// who the requester is: the name it posts under, and the json fields
/// that describe its account to the page.
fn account_json(portal: &Portal, req: &Request) -> (String, String) {
    let (device, user) = portal.identify(req);
    let name = display_name(device, user.as_ref());
    let released = portal.is_released(req.peer);
    if let (Some(device), None) = (device, &user) {
        portal.note_guest(device);
    }
    let owner = user.as_ref().map(|user| user.id).or(device);
    let (unread, notices) = owner.map(|owner| portal.mail.lock().unwrap().unread(owner)).unwrap_or_default();
    let notice =
        |notice: &Notice| format!(r#"{{"from":{},"text":{}}}"#, json(&notice.from), json(&notice.excerpt));
    let description = user.as_ref().map_or("", |user| &user.description);
    let fields = format!(
        r#""name":{},"registered":{},"admin":{},"description":{},"mail":{unread},"unread":{},"released":{released}"#,
        json(&name),
        user.is_some(),
        user.as_ref().is_some_and(|user| user.admin),
        json(description),
        list_json(&notices, notice),
    );
    (name, fields)
}

/// everything a client needs on its periodic refresh in a single request:
/// new chat messages, who else is here, and pending signals. passing `key`
/// registers the client as a peer.
fn poll(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let key = req.param("key");
    let (name, account) = account_json(portal, req);
    let mut peers = portal.peers.lock().unwrap();
    let registers = !key.is_empty() && key.len() <= KEY_MAX;
    let me = registers.then(|| peers.touch(key, &name, req.peer, Instant::now())).flatten();
    let signals = me.map(|id| peers.take(id)).unwrap_or_default();
    let others: Vec<&Peer> = peers.list().iter().filter(|peer| Some(peer.id) != me).collect();
    let others = list_json(&others, |peer| peer_json(peer));
    drop(peers);
    let chat = portal.chat.lock().unwrap().since(req.param("since").parse().unwrap_or(0));
    let config = &portal.config;
    let hosts: Vec<&String> = config.aliases.iter().chain([&config.origin]).collect();
    let body = format!(
        r#"{{"title":{},{account},{},"threads":{},"hosts":{},"https":{},"time":{},"me":{},"peers":{others},"signals":{},"chat":{}}}"#,
        json(&config.title),
        events_json(portal, req),
        portal.thread_counts(),
        list_json(&hosts, |host| json(host)),
        config.https,
        portal.now(),
        option_json(me),
        list_json(&signals, signal_json),
        list_json(&chat, entry_json),
    );
    http::send(out, http::OK, http::JSON, &body)
}

fn signal(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let (to, data) = (req.param("to").parse::<u64>(), req.param("data"));
    let Some(to) = to.ok().filter(|_| !data.is_empty() && data.len() <= MAX_SIGNAL_BYTES) else {
        return http::send(out, http::BAD_REQUEST, http::TEXT, "bad signal");
    };
    match portal.peers.lock().unwrap().send(req.param("key"), to, data, Instant::now()) {
        true => http::send(out, http::OK, http::JSON, "{}"),
        false => http::send(out, http::UNAVAILABLE, http::TEXT, "peer unavailable"),
    }
}

/// answers an account change. success carries the session token that the
/// client sends along from then on.
fn account_reply(
    portal: &Portal,
    out: &mut impl Write,
    outcome: io::Result<Result<User, Rejected>>,
) -> io::Result<()> {
    let reject = |out: &mut _, status, reason| http::send(out, status, http::TEXT, reason);
    match outcome {
        Ok(Ok(user)) => {
            let session = portal.users.lock().unwrap().session(&user);
            http::send(out, http::OK, http::JSON, &format!(r#"{{"session":{}}}"#, json(&session)))
        }
        Ok(Err(Rejected::Invalid)) => reject(out, http::BAD_REQUEST, INVALID_ACCOUNT),
        Ok(Err(Rejected::Taken)) => reject(out, http::CONFLICT, "that username is taken"),
        Err(_) => reject(out, http::UNAVAILABLE, "storage error"),
    }
}

/// true when the requester may try another signup or login. slows down
/// password guessing.
fn may_authenticate(portal: &Portal, req: &Request) -> bool {
    portal.allow(req.peer, "account", portal.config.chat_interval)
}

/// creates an account from `username`, `password`, and `description`.
fn register(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    if !may_authenticate(portal, req) {
        return http::send(out, http::TOO_MANY, http::TEXT, "slow down");
    }
    let (name, password, description) =
        (req.param("username"), req.param("password"), req.param("description"));
    let created = portal.users.lock().unwrap().create(name, password, description);
    let created = created.and_then(|outcome| match outcome {
        Ok((user, evicted)) => {
            evicted.map_or(Ok(()), |id| portal.mail.lock().unwrap().remove(id))?;
            Ok(Ok(user))
        }
        Err(rejected) => Ok(Err(rejected)),
    });
    account_reply(portal, out, created)
}

fn login(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    if !may_authenticate(portal, req) {
        return http::send(out, http::TOO_MANY, http::TEXT, "slow down");
    }
    let user = portal.users.lock().unwrap().login(req.param("username"), req.param("password")).cloned();
    match user {
        Some(user) => account_reply(portal, out, Ok(Ok(user))),
        None => http::send(out, http::FORBIDDEN, http::TEXT, "wrong username or password"),
    }
}

/// changes the logged in account. an empty `password` keeps the current one.
fn profile(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let (_, Some(user)) = portal.identify(req) else {
        return http::send(out, http::FORBIDDEN, http::TEXT, "log in first");
    };
    let (name, password, description) =
        (req.param("username"), req.param("password"), req.param("description"));
    let updated = portal.users.lock().unwrap().update(user.id, name, password, description);
    account_reply(portal, out, updated)
}

/// one row of the status page: how much of a limit is in use. `bytes`
/// tells whether `used` and `max` are bytes or the count itself.
fn usage_json(name: &str, usage: Usage, bytes: bool) -> String {
    let Usage { count, used, max } = usage;
    format!(r#"{{"name":{},"count":{count},"used":{used},"max":{max},"bytes":{bytes}}}"#, json(name))
}

/// everyone around: first the clients that have the page open, then the
/// devices that are on the wifi without it, under their generated names.
fn online_json(portal: &Portal) -> Vec<String> {
    let row = |name: &str, app: bool| format!(r#"{{"name":{},"app":{app}}}"#, json(name));
    let peers = portal.peers.lock().unwrap();
    let in_app: Vec<Option<u64>> = peers.list().iter().map(|peer| portal.device(peer.ip)).collect();
    let mut online: Vec<String> = peers.list().iter().map(|peer| row(&peer.name, true)).collect();
    drop(peers);
    let users = portal.users.lock().unwrap();
    let stations = (portal.config.stations)().into_iter().map(|mac| users.device(mac));
    online.extend(stations.filter(|id| !in_app.contains(&Some(*id))).map(|id| row(&default_name(id), false)));
    online
}

/// who is online, how full every store is, and how full the partitions are.
fn status(portal: &Portal, out: &mut impl Write) -> io::Result<()> {
    let counted =
        |count: usize, max: usize| Usage { count: count as u64, used: count as u64, max: max as u64 };
    let online = online_json(portal);
    let (threads, replies) = portal.board_usage();
    let config = &portal.config;
    let stored = [
        usage_json("accounts", counted(portal.users.lock().unwrap().count(), config.max_users), false),
        usage_json("mailboxes", counted(portal.mail.lock().unwrap().count(), config.max_mailboxes), false),
        usage_json("chat messages", counted(portal.chat.lock().unwrap().count(), CHAT_CAPACITY), false),
        usage_json("threads", threads, true),
        usage_json("replies", replies, true),
    ];
    let space = (config.space)();
    let row = |space: &Space| {
        format!(r#"{{"name":{},"used":{},"max":{}}}"#, json(space.name), space.used, space.size)
    };
    let body = format!(
        r#"{{"online":[{}],"stored":[{}],"space":{}}}"#,
        online.join(","),
        stored.join(","),
        list_json(&space, row),
    );
    http::send(out, http::OK, http::JSON, &body)
}

/// the public profile of the account named `name`.
fn user_info(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let user = portal.users.lock().unwrap().find(req.param("name")).cloned();
    match user {
        Some(user) => {
            let (name, description) = (json(&user.name), json(&user.description));
            let body = format!(r#"{{"name":{name},"description":{description},"admin":{}}}"#, user.admin);
            http::send(out, http::OK, http::JSON, &body)
        }
        None => http::send(out, http::NOT_FOUND, http::TEXT, "no such user"),
    }
}

/// sends one segment of a log, little endian: the id of its first record,
/// the id of an older segment or zero, the deleted and then the pinned ids,
/// then `counts` as u16, one per record of the segment or none at all, each
/// list preceded by its length as a u16, and the segment as stored.
fn send_page(out: &mut impl Write, page: Page, pinned: &[u64], counts: &[u16]) -> io::Result<()> {
    let mut head = [page.first, page.older.unwrap_or(0)].map(|id| id as u32).map(u32::to_le_bytes).concat();
    for ids in [&page.deleted[..], pinned] {
        head.extend_from_slice(&(ids.len() as u16).to_le_bytes());
        ids.iter().for_each(|id| head.extend_from_slice(&(*id as u32).to_le_bytes()));
    }
    head.extend_from_slice(&(counts.len() as u16).to_le_bytes());
    counts.iter().for_each(|count| head.extend_from_slice(&count.to_le_bytes()));
    http::head(out, http::OK, http::BINARY)?;
    out.write_all(&head)?;
    out.write_all(&page.records)
}

/// who a request sends and reads mail as: its account, or else its device
/// under the generated name. None when the device is unknown.
fn mail_owner(portal: &Portal, req: &Request) -> Option<(u64, String)> {
    match portal.identify(req) {
        (_, Some(user)) => Some((user.id, user.name)),
        (Some(device), None) => {
            portal.note_guest(device);
            Some((device, default_name(device)))
        }
        (None, None) => None,
    }
}

/// the mailbox that mail addressed to `name` goes to: an account, or a
/// guest seen lately under that generated name.
fn mail_recipient(portal: &Portal, name: &str) -> Option<(u64, String)> {
    match is_generated(name.trim()) {
        true => portal.find_guest(name.trim()).map(|device| (device, default_name(device))),
        false => portal.users.lock().unwrap().find(name).map(|user| (user.id, user.name.clone())),
    }
}

/// one segment of the requester's own mailbox, in the format of `threads`.
fn mail_read(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let Some((owner, _)) = mail_owner(portal, req) else {
        return http::send(out, http::FORBIDDEN, http::TEXT, "device not recognized");
    };
    let page = portal.mail.lock().unwrap().read(owner, req.param("segment").parse().ok())?;
    send_page(out, page.unwrap_or_default(), &[], &[])
}

/// sends mail from the requester to the account or guest named by `to`.
fn mail_send(portal: &Portal, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let Some((from, sender)) = mail_owner(portal, req) else {
        return http::send(out, http::FORBIDDEN, http::TEXT, "device not recognized");
    };
    let Some((to, recipient)) = mail_recipient(portal, req.param("to")) else {
        return http::send(out, http::NOT_FOUND, http::TEXT, "nobody here by that name");
    };
    post(portal, req, out, "mail", MAIL_MAX, portal.config.chat_interval, |ts, _, text| {
        portal.mail.lock().unwrap().send(ts, (from, &sender), (to, &recipient), text).map(|()| 0)
    })
}

/// validates and rate limits a post, then hands it to `save`.
fn post(
    portal: &Portal,
    req: &Request,
    out: &mut impl Write,
    kind: &'static str,
    max: usize,
    interval: Duration,
    save: impl FnOnce(u64, &str, &str) -> io::Result<u64>,
) -> io::Result<()> {
    let reject = |out: &mut _, status, reason| http::send(out, status, http::TEXT, reason);
    let (device, user) = portal.identify(req);
    let name = display_name(device, user.as_ref());
    let Some(text) = clean(req.param("text"), max, true) else {
        return reject(out, http::BAD_REQUEST, "text empty or too long");
    };
    if !portal.allow(req.peer, kind, interval) {
        return reject(out, http::TOO_MANY, "slow down");
    }
    portal.sync_clock(req.param("now").parse().unwrap_or(0), false);
    match save(portal.now(), &name, &text) {
        Ok(id) => http::send(out, http::OK, http::JSON, &format!(r#"{{"id":{id}}}"#)),
        Err(_) => reject(out, http::UNAVAILABLE, "storage error"),
    }
}

/// hides thread `id`, or reply `id` when `log` is `replies`.
fn delete(_: &Portal, topic: &Mutex<Topic>, req: &Request, out: &mut impl Write) -> io::Result<()> {
    let (id, reply) = (req.param("id").parse().unwrap_or(0), req.param("log") == "replies");
    let deleted = topic.lock().unwrap().delete(reply, id).unwrap_or(false);
    http::send(out, http::OK, http::JSON, &format!(r#"{{"deleted":{deleted}}}"#))
}
