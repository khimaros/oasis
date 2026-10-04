//! announcements of recent replies, held in RAM. clients ask for those
//! newer than the last one they saw and decide for themselves which concern
//! them, so the device does not track who takes part in which thread.

use std::collections::VecDeque;

pub const MAX_EVENTS: usize = 64;

pub struct Reply {
    pub id: u64,
    pub topic: String,
    pub thread: u64,
    /// id of the reply, by which its author recognizes it
    pub reply: u64,
    /// who replied
    pub name: String,
}

#[derive(Default)]
pub struct Events {
    replies: VecDeque<Reply>,
    last: u64,
}

impl Events {
    pub fn push(&mut self, topic: &str, thread: u64, reply: u64, name: &str) {
        if self.replies.len() == MAX_EVENTS {
            self.replies.pop_front();
        }
        self.last += 1;
        let (id, topic, name) = (self.last, topic.into(), name.into());
        self.replies.push_back(Reply { id, topic, thread, reply, name });
    }

    /// id of the newest announcement, zero when there was none yet.
    pub fn last(&self) -> u64 {
        self.last
    }

    /// announcements newer than `cursor`. a cursor from before a reboot is
    /// ahead of our ids, in which case all are returned.
    pub fn since(&self, cursor: u64) -> impl Iterator<Item = &Reply> {
        let cursor = if cursor > self.last { 0 } else { cursor };
        self.replies.iter().filter(move |reply| reply.id > cursor)
    }
}
