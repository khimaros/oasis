//! one topic of the board: threads, their replies, and pinned threads.
//!
//! threads and replies are two logs. the reference of a thread record is the
//! id that its first reply would get, which tells readers where to start
//! looking. the reference of a reply is the id of its thread.

use crate::store::{Page, Store, Usage};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_PINS: usize = 8;
/// file name suffixes of a topic, which shares its directory with the others
const THREADS: &str = ".t.";
const REPLIES: &str = ".r.";
const PINNED: &str = ".pinned";
/// threads get one part in this many of a topic's storage, replies the rest
const THREAD_SHARE: u64 = 4;
/// most reply bytes sent per request, so that a long thread neither fills
/// the heap nor blocks the board while it is read
const REPLY_PAGE_BYTES: usize = 16_000;

#[derive(Debug, PartialEq)]
pub enum Pin {
    Done,
    Unknown,
    Full,
}

pub struct Topic {
    pinned_file: PathBuf,
    threads: Store,
    replies: Store,
    pinned: BTreeSet<u64>,
}

impl Topic {
    /// opens the topic `name`, whose files live in `dir`.
    /// `indexed` keeps an index of the replies by thread, for a reply log
    /// too large to scan.
    pub fn open(dir: &Path, name: &str, bytes: u64, segment_bytes: u64, indexed: bool) -> io::Result<Topic> {
        let thread_bytes = bytes / THREAD_SHARE;
        let open = |suffix, bytes| Store::open(dir, &format!("{name}{suffix}"), bytes, segment_bytes);
        let (threads, replies) = (open(THREADS, thread_bytes)?, open(REPLIES, bytes - thread_bytes)?);
        let replies = if indexed { replies.indexed()? } else { replies };
        let pinned_file = dir.join(format!("{name}{PINNED}"));
        let stored = fs::read_to_string(&pinned_file).unwrap_or_default();
        let pinned = stored.lines().filter_map(|line| line.parse().ok()).collect();
        Ok(Topic { pinned_file, threads, replies, pinned })
    }

    /// starts a thread. the first line of its text is the subject.
    pub fn start(&mut self, ts: u64, name: &str, subject: &str, text: &str) -> io::Result<u64> {
        let marker = self.replies.next_id();
        self.threads.append(ts, marker, name, &format!("{subject}\n{text}"))
    }

    /// how full the thread log and the reply log are.
    pub fn usage(&self) -> (Usage, Usage) {
        (self.threads.usage(), self.replies.usage())
    }

    pub fn has_thread(&self, thread: u64) -> bool {
        self.threads.contains(thread)
    }

    pub fn reply(&mut self, ts: u64, name: &str, thread: u64, text: &str) -> io::Result<u64> {
        self.replies.append(ts, thread, name, text)
    }

    /// pins that still point at a stored thread.
    fn live_pins(&self) -> Vec<u64> {
        self.pinned.iter().copied().filter(|thread| self.has_thread(*thread)).collect()
    }

    /// the page of threads that holds thread `at`, or the newest page, the
    /// pinned threads of the topic, and how many replies each thread of the
    /// page has. the replies are counted from where those of the oldest
    /// thread of the page start, which for the newest page is not far back.
    pub fn page(&self, at: Option<u64>) -> io::Result<(Page, Vec<u64>, Vec<u16>)> {
        let page = self.threads.read(at)?;
        let markers = page.references();
        let counts = match markers.first() {
            Some(from) => self.replies.count(*from, page.first, markers.len())?,
            None => Vec::new(),
        };
        Ok((page, self.live_pins(), counts))
    }

    /// number of threads, not counting the deleted ones.
    pub fn threads(&self) -> u64 {
        self.threads.usage().count
    }

    /// replies to `thread` that come after reply `after`, each preceded by
    /// its id, and the reply to continue after when there are more.
    pub fn replies(&self, thread: u64, after: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
        self.replies.scan(after, REPLY_PAGE_BYTES, thread)
    }

    /// hides a thread, or a reply when `reply` is set.
    pub fn delete(&mut self, reply: bool, id: u64) -> io::Result<bool> {
        if reply { self.replies.delete(id) } else { self.threads.delete(id) }
    }

    /// pins or unpins a thread, which the page shows ahead of the others.
    pub fn pin(&mut self, thread: u64, pinned: bool) -> io::Result<Pin> {
        let mut pins: BTreeSet<u64> = self.live_pins().into_iter().collect();
        if !self.has_thread(thread) {
            return Ok(Pin::Unknown);
        }
        if pinned && !pins.contains(&thread) && pins.len() >= MAX_PINS {
            return Ok(Pin::Full);
        }
        if pinned {
            pins.insert(thread)
        } else {
            pins.remove(&thread)
        };
        fs::write(&self.pinned_file, pins.iter().map(|id| format!("{id}\n")).collect::<String>())?;
        self.pinned = pins;
        Ok(Pin::Done)
    }
}
