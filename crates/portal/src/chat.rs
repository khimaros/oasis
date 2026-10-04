//! ephemeral chat: a bounded in-memory ring of recent messages.

use crate::store::Entry;
use std::collections::VecDeque;

pub struct Chat {
    messages: VecDeque<Entry>,
    capacity: usize,
    next_id: u64,
}

impl Chat {
    pub fn new(capacity: usize) -> Chat {
        Chat { messages: VecDeque::with_capacity(capacity), capacity, next_id: 1 }
    }

    pub fn push(&mut self, ts: u64, name: &str, text: &str) -> u64 {
        if self.messages.len() == self.capacity {
            self.messages.pop_front();
        }
        let id = self.next_id;
        self.messages.push_back(Entry { id, ts, name: name.into(), text: text.into() });
        self.next_id += 1;
        id
    }

    /// number of messages held.
    pub fn count(&self) -> usize {
        self.messages.len()
    }

    /// messages newer than `since`. a `since` from before a reboot is ahead
    /// of our ids, in which case everything is returned.
    pub fn since(&self, since: u64) -> Vec<Entry> {
        let since = if since >= self.next_id { 0 } else { since };
        self.messages.iter().filter(|entry| entry.id > since).cloned().collect()
    }
}
