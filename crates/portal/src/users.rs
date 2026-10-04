//! accounts, sessions, and the generated names of devices without one.
//!
//! an account is a username with a password and an optional description.
//! logging in yields a session token that the client stores and sends along.
//! the network is open and unencrypted, so passwords and tokens can be read
//! by anyone in radio range: accounts keep honest people apart, no more.

use crate::crypto::{hex, hmac, pbkdf2, random};
use crate::text::{clean, constant_time_eq};
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_USERS: usize = 100;
const NAME_MAX: usize = 24;
const DESCRIPTION_MAX: usize = 160;
const PASSWORD_MIN: usize = 6;
const PASSWORD_MAX: usize = 64;
/// kept low enough for the ESP32 to hash a password in well under a second
const PBKDF2_ROUNDS: u32 = 1000;
/// marks generated names. usernames may not start with it, so a generated
/// name can never be mistaken for a chosen one.
const DEFAULT_PREFIX: char = '~';
const RESERVED: [&str; 3] = ["admin", "anon", "oasis"];
const SALT_FILE: &str = "salt";
const SECRET_FILE: &str = "secret";
const USERS_FILE: &str = "users";
const TEMP_FILE: &str = "users.tmp";
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const MIX_1: u64 = 0xff51_afd7_ed55_8ccd;
const MIX_2: u64 = 0xc4ce_b9fe_1a85_ec53;

const ADJECTIVES: [&str; 32] = [
    "amber", "brisk", "calm", "dusty", "eager", "faint", "gentle", "hazy", "idle", "jolly", "keen", "lucky",
    "mellow", "nimble", "odd", "plain", "quiet", "rusty", "shy", "tidy", "upbeat", "vivid", "warm", "young",
    "zesty", "bold", "crisp", "dapper", "early", "fuzzy", "glad", "humble",
];
const ANIMALS: [&str; 32] = [
    "otter", "heron", "lynx", "moth", "newt", "finch", "gecko", "hare", "ibis", "koala", "lemur", "mole",
    "owl", "panda", "quail", "raven", "seal", "tapir", "viper", "wren", "yak", "zebra", "badger", "crane",
    "dingo", "egret", "ferret", "gull", "hyena", "jackal", "kiwi", "marten",
];

#[derive(Debug, PartialEq)]
pub enum Rejected {
    Invalid,
    Taken,
}

#[derive(Clone)]
pub struct User {
    pub id: u64,
    pub name: String,
    /// hex, random per account
    salt: String,
    /// hex pbkdf2 of the password
    hash: String,
    /// empty when not given
    pub description: String,
}

impl User {
    fn parse(line: &str) -> Option<User> {
        let mut fields = line.split('\t').map(String::from);
        let id = u64::from_str_radix(&fields.next()?, 16).ok()?;
        let (name, salt, hash) = (fields.next()?, fields.next()?, fields.next()?);
        Some(User { id, name, salt, hash, description: fields.next()? })
    }

    fn line(&self) -> String {
        format!("{:016x}\t{}\t{}\t{}\t{}\n", self.id, self.name, self.salt, self.hash, self.description)
    }
}

pub struct Users {
    dir: PathBuf,
    /// keeps device ids from being linked to mac addresses
    salt: u64,
    /// signs session tokens
    secret: u64,
    /// oldest account first
    users: VecDeque<User>,
}

/// seeded fnv-1a with the murmur3 finalizer. fnv alone leaves the high bits
/// nearly equal for macs that differ only in their last byte, which is the
/// common case for devices of one vendor.
fn mix(seed: u64, bytes: &[u8]) -> u64 {
    let fnv = bytes.iter().fold(seed, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME));
    let mixed = (fnv ^ (fnv >> 33)).wrapping_mul(MIX_1);
    let mixed = (mixed ^ (mixed >> 33)).wrapping_mul(MIX_2);
    mixed ^ (mixed >> 33)
}

/// the name shown for a device that is not logged in, e.g. `~amber-otter`.
pub fn default_name(device: u64) -> String {
    let (adjective, animal) =
        ((device >> 32) as usize % ADJECTIVES.len(), (device >> 48) as usize % ANIMALS.len());
    format!("{DEFAULT_PREFIX}{}-{}", ADJECTIVES[adjective], ANIMALS[animal])
}

/// true for names of the form that `default_name` produces.
pub fn is_generated(name: &str) -> bool {
    name.starts_with(DEFAULT_PREFIX)
}

/// reads a random value that is created on first boot and then kept.
fn load_or_create(path: &Path) -> io::Result<u64> {
    let stored = fs::read_to_string(path).ok().and_then(|text| u64::from_str_radix(text.trim(), 16).ok());
    match stored {
        Some(value) => Ok(value),
        None => {
            let value = random();
            fs::write(path, format!("{value:016x}")).map(|()| value)
        }
    }
}

/// true when `input` is at most `max` bytes and free of control characters.
fn is_plain(input: &str, max: usize) -> bool {
    input.is_empty() || clean(input, max, false).is_some_and(|cleaned| cleaned == input)
}

fn valid_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let reserved = lower.starts_with(DEFAULT_PREFIX) || RESERVED.contains(&lower.as_str());
    !name.is_empty() && is_plain(name, NAME_MAX) && !reserved
}

fn valid_password(password: &str) -> bool {
    (PASSWORD_MIN..=PASSWORD_MAX).contains(&password.len())
}

fn password_hash(password: &str, salt: &str) -> String {
    hex(&pbkdf2(password.as_bytes(), salt.as_bytes(), PBKDF2_ROUNDS))
}

impl Users {
    pub fn open(dir: &Path) -> io::Result<Users> {
        fs::create_dir_all(dir)?;
        let stored = fs::read_to_string(dir.join(USERS_FILE)).unwrap_or_default();
        let users = stored.lines().filter_map(User::parse).collect();
        let (salt, secret) = (load_or_create(&dir.join(SALT_FILE))?, load_or_create(&dir.join(SECRET_FILE))?);
        Ok(Users { dir: dir.to_path_buf(), salt, secret, users })
    }

    /// id of the device with this mac, used for its generated name.
    pub fn device(&self, mac: [u8; 6]) -> u64 {
        mix(self.salt, &mac)
    }

    fn get(&self, id: u64) -> Option<&User> {
        self.users.iter().find(|user| user.id == id)
    }

    /// number of accounts.
    pub fn count(&self) -> usize {
        self.users.len()
    }

    /// looks an account up by username, ignoring case.
    pub fn find(&self, name: &str) -> Option<&User> {
        let lower = name.trim().to_lowercase();
        self.users.iter().find(|user| user.name.to_lowercase() == lower)
    }

    /// replaces the table on flash through a rename, so that power loss
    /// leaves either the old or the new one.
    fn save(&self) -> io::Result<()> {
        fs::write(self.dir.join(TEMP_FILE), self.users.iter().map(User::line).collect::<String>())?;
        fs::rename(self.dir.join(TEMP_FILE), self.dir.join(USERS_FILE))
    }

    /// signs a new account up. once the table is full the oldest account
    /// makes room, and its id is returned along with the new account.
    pub fn create(
        &mut self,
        name: &str,
        password: &str,
        description: &str,
    ) -> io::Result<Result<(User, Option<u64>), Rejected>> {
        let (name, description) = (name.trim(), description.trim());
        if !valid_name(name) || !valid_password(password) || !is_plain(description, DESCRIPTION_MAX) {
            return Ok(Err(Rejected::Invalid));
        }
        if self.find(name).is_some() {
            return Ok(Err(Rejected::Taken));
        }
        let full = self.users.len() >= MAX_USERS;
        let evicted = full.then(|| self.users.pop_front()).flatten().map(|user| user.id);
        let salt = format!("{:016x}", random());
        let hash = password_hash(password, &salt);
        let user = User { id: random(), name: name.into(), salt, hash, description: description.into() };
        self.users.push_back(user.clone());
        self.save().map(|()| Ok((user, evicted)))
    }

    /// changes the profile of account `id`. an empty password keeps the
    /// current one.
    pub fn update(
        &mut self,
        id: u64,
        name: &str,
        password: &str,
        description: &str,
    ) -> io::Result<Result<User, Rejected>> {
        let (name, description) = (name.trim(), description.trim());
        let password_ok = password.is_empty() || valid_password(password);
        if !valid_name(name) || !password_ok || !is_plain(description, DESCRIPTION_MAX) {
            return Ok(Err(Rejected::Invalid));
        }
        if self.find(name).is_some_and(|owner| owner.id != id) {
            return Ok(Err(Rejected::Taken));
        }
        let Some(user) = self.users.iter_mut().find(|user| user.id == id) else {
            return Ok(Err(Rejected::Invalid));
        };
        (user.name, user.description) = (name.into(), description.into());
        if !password.is_empty() {
            user.hash = password_hash(password, &user.salt);
        }
        let user = user.clone();
        self.save().map(|()| Ok(user))
    }

    /// the account, when username and password match.
    pub fn login(&self, name: &str, password: &str) -> Option<&User> {
        let matches = |user: &&User| constant_time_eq(&password_hash(password, &user.salt), &user.hash);
        self.find(name).filter(matches)
    }

    /// the token that proves a login. it is derived from the password hash,
    /// so it needs no storage, survives reboots, and dies with the password.
    pub fn session(&self, user: &User) -> String {
        let signed = format!("{:016x}{}", user.id, user.hash);
        format!("{:016x}.{}", user.id, hex(&hmac(&self.secret.to_be_bytes(), signed.as_bytes())))
    }

    /// the account a session token belongs to, if the token is genuine.
    pub fn verify(&self, session: &str) -> Option<&User> {
        let (id, _) = session.split_once('.')?;
        let user = self.get(u64::from_str_radix(id, 16).ok()?)?;
        constant_time_eq(&self.session(user), session).then_some(user)
    }
}
