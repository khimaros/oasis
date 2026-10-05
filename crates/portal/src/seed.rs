//! the settings file that is flashed along with the program. it names the
//! network, lists accounts that the device makes sure exist, and threads
//! that it starts on a fresh board:
//!
//!     ssid = base camp
//!
//!     [user]
//!     name = admin
//!     password = sesame
//!     admin = yes
//!
//!     [thread]
//!     topic = general
//!     subject = welcome
//!     text = be kind.\nposts are public.
//!     pinned = yes
//!
//! a setting is one line. `\n` in a text stands for a line break, and `#`
//! starts a comment line.

use crate::users::storable;

const COMMENT: char = '#';
const USER_SECTION: &str = "[user]";
const THREAD_SECTION: &str = "[thread]";
const YES: &str = "yes";
const NO: &str = "no";
/// signs the threads that name no author. nobody can sign up under it
const DEFAULT_AUTHOR: &str = "oasis";
const LINE_BREAK: &str = "\\n";
/// the longest name that a wifi network can have, in bytes
const SSID_MAX: usize = 32;

#[derive(Default)]
pub struct Account {
    pub name: String,
    pub password: String,
    pub description: String,
    pub admin: bool,
}

#[derive(Default)]
pub struct Thread {
    pub topic: String,
    pub subject: String,
    pub text: String,
    pub author: String,
    pub pinned: bool,
}

#[derive(Default)]
pub struct Seed {
    /// name of the network and title of the portal
    pub ssid: Option<String>,
    pub users: Vec<Account>,
    pub threads: Vec<Thread>,
}

enum Section {
    Top,
    User,
    Thread,
}

fn flag(value: &str) -> Result<bool, String> {
    match value {
        YES => Ok(true),
        NO => Ok(false),
        _ => Err(format!("`{value}` is neither {YES} nor {NO}")),
    }
}

/// stores one setting of the section that is being read.
fn set(seed: &mut Seed, section: &Section, key: &str, value: &str) -> Result<(), String> {
    let text = || value.to_string();
    match (section, key, seed.users.last_mut(), seed.threads.last_mut()) {
        (Section::Top, "ssid", ..) => seed.ssid = Some(text()),
        (Section::User, "name", Some(user), _) => user.name = text(),
        (Section::User, "password", Some(user), _) => user.password = text(),
        (Section::User, "description", Some(user), _) => user.description = text(),
        (Section::User, "admin", Some(user), _) => user.admin = flag(value)?,
        (Section::Thread, "topic", _, Some(thread)) => thread.topic = text(),
        (Section::Thread, "subject", _, Some(thread)) => thread.subject = text(),
        (Section::Thread, "text", _, Some(thread)) => thread.text = value.replace(LINE_BREAK, "\n"),
        (Section::Thread, "author", _, Some(thread)) => thread.author = text(),
        (Section::Thread, "pinned", _, Some(thread)) => thread.pinned = flag(value)?,
        _ => return Err(format!("unknown setting `{key}`")),
    }
    Ok(())
}

/// reads one line into `seed`. returns the section that the next line is in.
fn line(seed: &mut Seed, section: Section, line: &str) -> Result<Section, String> {
    match line.trim() {
        blank if blank.is_empty() || blank.starts_with(COMMENT) => Ok(section),
        USER_SECTION => {
            seed.users.push(Account::default());
            Ok(Section::User)
        }
        THREAD_SECTION => {
            seed.threads.push(Thread { author: DEFAULT_AUTHOR.into(), ..Thread::default() });
            Ok(Section::Thread)
        }
        setting => {
            let (key, value) = setting.split_once('=').ok_or(format!("`{setting}` is no setting"))?;
            set(seed, &section, key.trim(), value.trim()).map(|()| section)
        }
    }
}

/// what an entry lacks, or names wrongly. `topics` are the topics of the board.
fn complete(seed: &Seed, topics: &[&str]) -> Result<(), String> {
    if seed.ssid.as_ref().is_some_and(|ssid| ssid.is_empty() || ssid.len() > SSID_MAX) {
        return Err(format!("the ssid must be 1 to {SSID_MAX} bytes"));
    }
    if let Some(user) = seed.users.iter().find(|user| !storable(user)) {
        return Err(format!("the user `{}` needs a name and a password that an account can have", user.name));
    }
    let known = |thread: &&Thread| !topics.contains(&thread.topic.as_str());
    if let Some(thread) = seed.threads.iter().find(known) {
        return Err(format!("the thread `{}` is in no topic of the board", thread.subject));
    }
    match seed.threads.iter().find(|thread| thread.subject.is_empty() || thread.text.is_empty()) {
        Some(thread) => Err(format!("a thread in `{}` needs a subject and a text", thread.topic)),
        None => Ok(()),
    }
}

/// reads a settings file. the error names the line that is wrong.
pub fn parse(text: &str, topics: &[&str]) -> Result<Seed, String> {
    let mut seed = Seed::default();
    let mut section = Section::Top;
    for (number, content) in text.lines().enumerate() {
        section = line(&mut seed, section, content).map_err(|err| format!("line {}: {err}", number + 1))?;
    }
    complete(&seed, topics).map(|()| seed)
}
