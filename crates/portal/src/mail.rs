//! private messages. an owner, which is an account or the device of a guest,
//! has a small mailbox log holding received mail and copies of sent mail.
//! mailboxes are opened on demand, so only their ids and the unread
//! counters live in RAM. all mailboxes share one directory, each under the
//! prefix of its owner.
//!
//! the reference of a mail record is its direction, and its name the other
//! party.

use crate::store::{Page, Store};
use crate::text::excerpt;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::PathBuf;

pub const MAIL_MAX: usize = 1000;
/// just under a littlefs block
const SEGMENT_BYTES: u64 = 4000;
/// notices kept for all mailboxes together, which bounds the RAM that a
/// pile of unread mail can take
const MAX_NOTICES: usize = 64;
/// notices sent to one client at a time. the rest is only counted
const MAX_UNREAD_NOTICES: usize = 8;
const EXCERPT_BYTES: usize = 48;
const RECEIVED: u64 = 0;
const SENT: u64 = 1;

/// one end of a mail: the id that owns the mailbox, and the name shown.
pub type Party<'a> = (u64, &'a str);

/// announces an unread mail: who it is from and how it starts.
#[derive(Clone)]
pub struct Notice {
    to: u64,
    pub from: String,
    pub excerpt: String,
}

pub struct Mail {
    dir: PathBuf,
    /// owners of the stored mailboxes, least recently used first. the
    /// order is not kept across reboots.
    owners: VecDeque<u64>,
    /// mail received since the owner last read the mailbox
    unread: HashMap<u64, u32>,
    /// the newest unread mail of all mailboxes, oldest first
    notices: VecDeque<Notice>,
    /// bounds the storage that mail can take. guests are not limited in
    /// number, so the mailbox unused for longest makes room for a new one.
    max_mailboxes: usize,
    mailbox_bytes: u64,
}

/// the owner that a file of the mail directory belongs to.
fn owner_of(entry: &fs::DirEntry) -> Option<u64> {
    let name = entry.file_name();
    u64::from_str_radix(name.to_str()?.split_once('.')?.0, 16).ok()
}

impl Mail {
    pub fn open(dir: PathBuf, max_mailboxes: usize, mailbox_bytes: u64) -> Mail {
        let mut owners = VecDeque::new();
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            match owner_of(&entry) {
                Some(owner) if !owners.contains(&owner) => owners.push_back(owner),
                Some(_) => {}
                // a mailbox directory of the layout before flat files
                None => drop(fs::remove_dir_all(entry.path())),
            }
        }
        let (unread, notices) = (HashMap::new(), VecDeque::new());
        Mail { dir, owners, unread, notices, max_mailboxes, mailbox_bytes }
    }

    fn mailbox(&self, owner: u64) -> io::Result<Store> {
        Store::open(&self.dir, &format!("{owner:016x}."), self.mailbox_bytes, SEGMENT_BYTES)
    }

    /// appends to the owner's mailbox, creating it if needed and evicting
    /// the least recently used ones beyond the limit.
    fn deliver(&mut self, owner: u64, ts: u64, direction: u64, other: &str, text: &str) -> io::Result<()> {
        self.mailbox(owner)?.append(ts, direction, other, text)?;
        self.owners.retain(|known| *known != owner);
        self.owners.push_back(owner);
        while self.owners.len() > self.max_mailboxes {
            let evicted = self.owners[0];
            self.remove(evicted)?;
        }
        Ok(())
    }

    /// delivers to the recipient and keeps a copy for the sender.
    pub fn send(&mut self, ts: u64, from: Party, to: Party, text: &str) -> io::Result<()> {
        self.deliver(from.0, ts, SENT, to.1, text)?;
        self.deliver(to.0, ts, RECEIVED, from.1, text)?;
        *self.unread.entry(to.0).or_default() += 1;
        if self.notices.len() == MAX_NOTICES {
            self.notices.pop_front();
        }
        self.notices.push_back(Notice {
            to: to.0,
            from: from.1.into(),
            excerpt: excerpt(text, EXCERPT_BYTES),
        });
        Ok(())
    }

    fn mark_read(&mut self, owner: u64) {
        self.unread.remove(&owner);
        self.notices.retain(|notice| notice.to != owner);
    }

    /// one segment of the owner's mailbox, None when there is no mailbox.
    /// marks its mail as read.
    pub fn read(&mut self, owner: u64, segment: Option<u64>) -> io::Result<Option<Page>> {
        self.mark_read(owner);
        if !self.owners.contains(&owner) {
            return Ok(None);
        }
        self.mailbox(owner)?.read(segment).map(Some)
    }

    /// number of stored mailboxes.
    pub fn count(&self) -> usize {
        self.owners.len()
    }

    /// how much mail the owner has not read, and notices of the newest of
    /// it, oldest first.
    pub fn unread(&self, owner: u64) -> (u32, Vec<Notice>) {
        let count = self.unread.get(&owner).copied().unwrap_or(0);
        let newest = self.notices.iter().rev().filter(|notice| notice.to == owner).take(MAX_UNREAD_NOTICES);
        let mut notices: Vec<Notice> = newest.cloned().collect();
        notices.reverse();
        (count, notices)
    }

    /// discards a mailbox.
    pub fn remove(&mut self, owner: u64) -> io::Result<()> {
        self.mark_read(owner);
        if !self.owners.contains(&owner) {
            return Ok(());
        }
        self.owners.retain(|known| *known != owner);
        self.mailbox(owner)?.destroy()
    }
}
