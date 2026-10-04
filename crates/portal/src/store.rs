//! persistent message log, stored as small append only segment files so
//! that eviction is a file delete and a page read is a single segment.
//!
//! several logs share a directory and are told apart by a file name prefix,
//! because a littlefs directory costs two blocks of flash.
//!
//! a record is `[length u16][ts u32][reference u32][name length u8][name]
//! [text]`, little endian, the length counting what follows it. the id of a
//! record is its position: the first id of its segment, which is the file
//! name, plus its index. the reference is free for the owner of the log.
//!
//! a log too large to scan can keep an index: which segments hold records
//! of each reference. it lives in RAM and in a file of `[reference u32]
//! [segment u32]` entries, written ahead of the record they announce.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const DELETED_FILE: &str = "deleted";
const INDEX_FILE: &str = "index";
const INDEX_ENTRY_BYTES: usize = 8;
const TEMP_FILE: &str = "tmp";
const FIRST_ID: u64 = 1;
const LENGTH_BYTES: usize = 2;
/// timestamp, reference, and name length
const HEAD_BYTES: usize = 9;
const REFERENCE_AT: usize = LENGTH_BYTES + 4;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: u64,
    pub ts: u64,
    pub name: String,
    pub text: String,
}

/// one segment exactly as stored, oldest record first. parsing the records
/// and hiding the deleted ids is left to the client, which has cpu to spare.
#[derive(Default)]
pub struct Page {
    /// id of the first record
    pub first: u64,
    pub records: Vec<u8>,
    pub older: Option<u64>,
    pub deleted: Vec<u64>,
}

/// how much of something is in use.
#[derive(Clone, Copy, Default)]
pub struct Usage {
    pub count: u64,
    pub used: u64,
    pub max: u64,
}

impl Usage {
    pub fn plus(self, other: Usage) -> Usage {
        Usage { count: self.count + other.count, used: self.used + other.used, max: self.max + other.max }
    }
}

pub struct Store {
    dir: PathBuf,
    prefix: String,
    segment_bytes: u64,
    max_segments: usize,
    /// first record id of each segment, ascending. also the file name.
    segments: Vec<u64>,
    next_id: u64,
    tail_bytes: u64,
    deleted: BTreeSet<u64>,
    /// segments that hold records of each reference, ascending. None for
    /// logs small enough to scan.
    index: Option<Index>,
}

type Index = HashMap<u64, Vec<u64>>;

fn index_entry(reference: u64, segment: u64) -> [u8; INDEX_ENTRY_BYTES] {
    let mut entry = [0; INDEX_ENTRY_BYTES];
    entry[..4].copy_from_slice(&(reference as u32).to_le_bytes());
    entry[4..].copy_from_slice(&(segment as u32).to_le_bytes());
    entry
}

/// the (reference, segment) pairs of an index file. an entry torn by power
/// loss is left out.
fn index_entries(data: &[u8]) -> impl Iterator<Item = (u64, u64)> {
    let field = |bytes: &[u8]| u64::from(u32::from_le_bytes(bytes.try_into().unwrap()));
    data.chunks_exact(INDEX_ENTRY_BYTES).map(move |entry| (field(&entry[..4]), field(&entry[4..])))
}

fn read_or_empty(path: &Path) -> io::Result<Vec<u8>> {
    match fs::read(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        result => result,
    }
}

fn append(path: &Path, data: &[u8]) -> io::Result<()> {
    OpenOptions::new().create(true).append(true).open(path)?.write_all(data)
}

/// the complete records at the start of `data`, each with its length field.
fn records(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let length = usize::from(u16::from_le_bytes([*rest.first()?, *rest.get(1)?]));
        let (record, after) = rest.split_at_checked(LENGTH_BYTES + length)?;
        rest = after;
        Some(record)
    })
}

fn encode(ts: u64, reference: u64, name: &str, text: &str) -> io::Result<Vec<u8>> {
    let length = u16::try_from(HEAD_BYTES + name.len() + text.len()).map_err(io::Error::other)?;
    let name_length = u8::try_from(name.len()).map_err(io::Error::other)?;
    let mut record = length.to_le_bytes().to_vec();
    record.extend_from_slice(&(ts as u32).to_le_bytes());
    record.extend_from_slice(&(reference as u32).to_le_bytes());
    record.push(name_length);
    record.extend_from_slice(name.as_bytes());
    record.extend_from_slice(text.as_bytes());
    Ok(record)
}

fn reference(record: &[u8]) -> u64 {
    let bytes = record.get(REFERENCE_AT..REFERENCE_AT + 4).and_then(|bytes| bytes.try_into().ok());
    bytes.map_or(0, |bytes| u64::from(u32::from_le_bytes(bytes)))
}

impl Store {
    /// opens the log whose files in `dir` start with `prefix`.
    pub fn open(dir: &Path, prefix: &str, max_bytes: u64, segment_bytes: u64) -> io::Result<Store> {
        fs::create_dir_all(dir)?;
        let mut store = Store {
            dir: dir.to_path_buf(),
            prefix: prefix.into(),
            segment_bytes,
            max_segments: (max_bytes / segment_bytes).max(1) as usize,
            segments: Vec::new(),
            next_id: FIRST_ID,
            tail_bytes: 0,
            deleted: BTreeSet::new(),
            index: None,
        };
        store.list_segments()?;
        store.recover_tail()?;
        store.load_deleted()?;
        Ok(store)
    }

    /// keeps an index by reference from here on, which `scan` then uses.
    /// loads the stored one, or builds it from the records when the log has
    /// none yet. entries of evicted segments are dropped.
    pub fn indexed(mut self) -> io::Result<Store> {
        let path = self.file(INDEX_FILE);
        let (stored, pairs) = match fs::read(&path) {
            Ok(data) => (data.len(), index_entries(&data).collect()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (0, self.index_pairs()?),
            Err(err) => return Err(err),
        };
        let mut index = Index::new();
        for (reference, segment) in pairs {
            let segments = index.entry(reference).or_default();
            if self.segments.binary_search(&segment).is_ok() && segments.last() != Some(&segment) {
                segments.push(segment);
            }
        }
        index.retain(|_, segments| !segments.is_empty());
        let entries = index.iter().flat_map(|(reference, segments)| {
            segments.iter().map(|segment| index_entry(*reference, *segment))
        });
        let data = entries.collect::<Vec<_>>().concat();
        if data.len() != stored {
            fs::write(self.file(TEMP_FILE), data)?;
            fs::rename(self.file(TEMP_FILE), path)?;
        }
        self.index = Some(index);
        Ok(self)
    }

    /// the (reference, segment) pair of every stored record.
    fn index_pairs(&self) -> io::Result<Vec<(u64, u64)>> {
        let mut pairs = Vec::new();
        for segment in &self.segments {
            let data = read_or_empty(&self.path(*segment))?;
            pairs.extend(records(&data).map(|record| (reference(record), *segment)));
            pairs.dedup();
        }
        Ok(pairs)
    }

    /// notes that the newest segment holds a record of `reference`.
    fn index_tail(&mut self, reference: u64) -> io::Result<()> {
        let (tail, path) = (self.tail(), self.file(INDEX_FILE));
        let Some(index) = &mut self.index else { return Ok(()) };
        let segments = index.entry(reference).or_default();
        if segments.last() != Some(&tail) {
            append(&path, &index_entry(reference, tail))?;
            segments.push(tail);
        }
        Ok(())
    }

    /// drops a segment that is about to be evicted from the index.
    fn unindex(&mut self, segment: u64) -> io::Result<()> {
        let data = if self.index.is_some() { read_or_empty(&self.path(segment))? } else { Vec::new() };
        let Some(index) = &mut self.index else { return Ok(()) };
        for reference in records(&data).map(reference) {
            if let Some(segments) = index.get_mut(&reference) {
                segments.retain(|other| *other != segment);
                if segments.is_empty() {
                    index.remove(&reference);
                }
            }
        }
        Ok(())
    }

    fn list_segments(&mut self) -> io::Result<()> {
        let id = |entry: fs::DirEntry| entry.file_name().to_str()?.strip_prefix(&self.prefix)?.parse().ok();
        self.segments = fs::read_dir(&self.dir)?.flatten().filter_map(id).collect();
        self.segments.sort_unstable();
        if self.segments.is_empty() {
            self.segments.push(FIRST_ID);
        }
        Ok(())
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}{name}", self.prefix))
    }

    fn path(&self, segment: u64) -> PathBuf {
        self.file(&format!("{segment:010}"))
    }

    fn tail(&self) -> u64 {
        *self.segments.last().unwrap()
    }

    /// finds the next id, and drops a record torn by power loss so that the
    /// next append does not continue it.
    fn recover_tail(&mut self) -> io::Result<()> {
        let path = self.path(self.tail());
        let data = read_or_empty(&path)?;
        let (count, bytes) =
            records(&data).fold((0, 0), |(count, bytes), record| (count + 1, bytes + record.len()));
        if bytes < data.len() {
            fs::write(self.file(TEMP_FILE), &data[..bytes])?;
            fs::rename(self.file(TEMP_FILE), &path)?;
        }
        self.next_id = self.tail() + count;
        self.tail_bytes = bytes as u64;
        Ok(())
    }

    /// loads deletion marks, dropping those whose segment was evicted.
    fn load_deleted(&mut self) -> io::Result<()> {
        let path = self.file(DELETED_FILE);
        let data = String::from_utf8_lossy(&read_or_empty(&path)?).into_owned();
        let all: BTreeSet<u64> = data.lines().filter_map(|line| line.parse().ok()).collect();
        self.deleted = all.iter().copied().filter(|id| *id >= self.segments[0]).collect();
        if self.deleted.len() < all.len() {
            let lines: String = self.deleted.iter().map(|id| format!("{id}\n")).collect();
            fs::write(&path, lines)?;
        }
        Ok(())
    }

    /// starts a new segment and evicts the oldest ones beyond capacity.
    fn rotate(&mut self) -> io::Result<()> {
        self.segments.push(self.next_id);
        self.tail_bytes = 0;
        while self.segments.len() > self.max_segments {
            self.unindex(self.segments[0])?;
            let evicted = self.segments.remove(0);
            fs::remove_file(self.path(evicted))?;
        }
        Ok(())
    }

    pub fn append(&mut self, ts: u64, reference: u64, name: &str, text: &str) -> io::Result<u64> {
        let record = encode(ts, reference, name, text)?;
        if self.tail_bytes > 0 && self.tail_bytes + record.len() as u64 > self.segment_bytes {
            self.rotate()?;
        }
        self.index_tail(reference)?;
        append(&self.path(self.tail()), &record)?;
        self.tail_bytes += record.len() as u64;
        self.next_id += 1;
        Ok(self.next_id - 1)
    }

    /// hides an entry from readers. false when the id is not stored.
    pub fn delete(&mut self, id: u64) -> io::Result<bool> {
        if !self.contains(id) {
            return Ok(false);
        }
        append(&self.file(DELETED_FILE), format!("{id}\n").as_bytes())?;
        self.deleted.insert(id);
        Ok(true)
    }

    /// removes every file of the log.
    pub fn destroy(self) -> io::Result<()> {
        let files = self.segments.iter().map(|segment| self.path(*segment));
        for path in files.chain([self.file(DELETED_FILE), self.file(INDEX_FILE)]) {
            match fs::remove_file(path) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                _ => {}
            }
        }
        Ok(())
    }

    /// index of the segment that holds entry `id`. ids from before the first
    /// segment map to it.
    fn segment_of(&self, id: u64) -> usize {
        self.segments.partition_point(|first| *first <= id).saturating_sub(1)
    }

    /// how full the log is: its entries, the bytes its segments take, and
    /// the bytes they may take. full segments are counted at their limit.
    pub fn usage(&self) -> Usage {
        let deleted = self.deleted.range(self.segments[0]..).count() as u64;
        let full = self.segments.len() as u64 - 1;
        Usage {
            count: self.next_id - self.segments[0] - deleted,
            used: full * self.segment_bytes + self.tail_bytes,
            max: self.max_segments as u64 * self.segment_bytes,
        }
    }

    /// the id that the next appended entry gets.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// true when entry `id` is stored and not deleted.
    pub fn contains(&self, id: u64) -> bool {
        id >= self.segments[0] && id < self.next_id && !self.deleted.contains(&id)
    }

    /// the stored segments from `from` on that may hold records of `wanted`:
    /// those the index names, or all of them without an index.
    fn candidates(&self, from: u64, wanted: u64) -> Vec<u64> {
        let all = match &self.index {
            Some(index) => index.get(&wanted).map_or(&[][..], Vec::as_slice),
            None => &self.segments,
        };
        all.iter().copied().filter(|segment| *segment >= from).collect()
    }

    /// the records after entry `after` whose reference is `wanted`, each
    /// preceded by its id as a u32, oldest first, up to about `limit` bytes.
    /// when there are more, also the id to continue after. deleted entries
    /// are left out.
    pub fn scan(&self, after: u64, limit: usize, wanted: u64) -> io::Result<(Vec<u8>, Option<u64>)> {
        let (mut found, mut last) = (Vec::new(), after);
        let from = self.segments[self.segment_of(after + 1)];
        for segment in self.candidates(from, wanted) {
            let data = read_or_empty(&self.path(segment))?;
            for (id, record) in (segment..).zip(records(&data)) {
                if id <= after || self.deleted.contains(&id) || reference(record) != wanted {
                    continue;
                }
                if !found.is_empty() && found.len() + record.len() > limit {
                    return Ok((found, Some(last)));
                }
                found.extend_from_slice(&(id as u32).to_le_bytes());
                found.extend_from_slice(record);
                last = id;
            }
        }
        Ok((found, None))
    }

    /// reads the segment that holds entry `at`, or the newest one. ids that
    /// are not stored read as the nearest segment, since a client may hold a
    /// reference to an evicted one.
    pub fn read(&self, at: Option<u64>) -> io::Result<Page> {
        let index = at.map_or(self.segments.len() - 1, |id| self.segment_of(id));
        let first = self.segments[index];
        let records = read_or_empty(&self.path(first))?;
        let end = self.segments.get(index + 1).copied().unwrap_or(self.next_id);
        let deleted = self.deleted.range(first..end).copied().collect();
        let older = index.checked_sub(1).map(|older| self.segments[older]);
        Ok(Page { first, records, older, deleted })
    }
}
