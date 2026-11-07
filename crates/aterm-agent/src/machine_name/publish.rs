// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publishing a chosen machine number (`docs/DECISION-name-publication-2026-11-06.md`): once
//! [`super::set_name`] has committed a number, [`publish`] makes every other reader of this
//! machine's name read it too, in order: aterm.toml's three naming keys ([`Policy`]), the checked
//! fleet name, and on macOS the Mac's three system names ([`SystemNames`]), each only while the
//! name file still holds that number. [`publication`] says, writing nothing, which of those a
//! chosen number still owes. `aterm name set` is its caller.

use std::io;
use std::path::Path;
use std::time::Duration;

use aterm_toml::edit::{DocumentMut, Item, Table, Value};
use aterm_types::machine_name::{Class, Number};
use aterm_uds::atomic_file::ConfigFile;

use super::file::{self, MachineName, Migration};

/// The root step's whole bound: its password sheets and sudo's own retries.
pub const SYSTEM_RENAME_WAIT: Duration = Duration::from_secs(600);

/// macOS's three system names, in the order they are read and set: one operation, one approval.
pub const SYSTEM_NAMES: [&str; 3] = ["ComputerName", "LocalHostName", "HostName"];

/// How long one `scutil --get` or `--set` may take.
#[cfg(target_os = "macos")]
const SCUTIL_WAIT: Duration = Duration::from_secs(10);

/// What a request a newer set replaced says (`aterm name set`'s words).
const SUPERSEDED: &str = "a newer machine name replaced this request";

/// What aterm.toml's naming hold says.
const HELD_OFF: &str = "[identity] name = false holds machine naming off";

/// aterm.toml's keys that name this machine: its table, its key, and how a person reads them.
const POLICY_KEYS: [(&str, &str, &str); 3] = [
    ("identity", "name", "[identity] name"),
    ("ai", "machine", "[ai] machine"),
    ("fleet", "machine", "[fleet] machine"),
];

/// aterm.toml's naming policy for one number: the file as it was read and the text that names
/// the number in its three keys, other settings and comments kept. It is read before the set, so
/// a naming hold or an aterm.toml that cannot be edited refuses before any name changes, and
/// [`publish`] writes it only over the bytes read here.
pub struct Policy {
    number: Number,
    snapshot: ConfigFile,
    text: String,
}

impl Policy {
    /// The policy that names `number` in the aterm.toml at `config`, where no file reads as an
    /// empty one.
    ///
    /// # Errors
    /// aterm.toml cannot be read ([`ConfigFile::open`]) or is not UTF-8; it is not valid TOML
    /// (`invalid aterm.toml: …`); `[identity] name = false holds machine naming off`; or its
    /// `identity`, `ai` or `fleet` is not a table (`aterm.toml {section} is not a table`).
    pub fn read(config: &Path, number: Number) -> Result<Self, String> {
        let snapshot = ConfigFile::open(config).map_err(|e| e.to_string())?;
        let text = policy_text(
            snapshot.text().map_err(|e| e.to_string())?.unwrap_or(""),
            &number.to_string(),
        )?;
        Ok(Self {
            number,
            snapshot,
            text,
        })
    }
}

/// aterm.toml's text, `text`, parsed as its editor keeps it.
fn parsed(text: &str) -> Result<DocumentMut, String> {
    text.parse().map_err(|e| format!("invalid aterm.toml: {e}"))
}

/// Whether `doc` holds machine naming off (`[identity] name = false`).
fn held_off(doc: &DocumentMut) -> bool {
    doc.get("identity")
        .and_then(Item::as_table_like)
        .and_then(|table| table.get("name"))
        .and_then(Item::as_bool)
        == Some(false)
}

/// `text` with each of [`POLICY_KEYS`] set to `number`.
///
/// # Errors
/// As [`Policy::read`], after the read.
fn policy_text(text: &str, number: &str) -> Result<String, String> {
    let mut doc = parsed(text)?;
    if held_off(&doc) {
        return Err(HELD_OFF.into());
    }
    for (section, key, _) in POLICY_KEYS {
        let root = doc.as_table_mut();
        if root.get(section).is_none_or(Item::is_none) {
            root.insert(section, Item::Table(Table::new()));
        }
        let table = root
            .get_mut(section)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("aterm.toml {section} is not a table"))?;
        table.insert(key, Item::Value(Value::from(number)));
    }
    Ok(doc.to_string())
}

/// Write `policy`'s text under the name file's lock while that file holds its number, through
/// the config service's locked compare-and-replace over the bytes [`Policy::read`] read.
///
/// # Errors
/// The name lock is busy, or the name file is missing or unread ([`file::replace`]); it holds
/// another number (`a newer machine name replaced this request`); or the config service's lock
/// cannot be taken or its replace fails ([`aterm_uds::atomic_file::ConfigEdit::replace`]), as
/// when aterm.toml changed since it was read.
fn write_policy(state: &Path, policy: &Policy) -> io::Result<()> {
    file::replace(state, |held| {
        if held.num != Some(policy.number) {
            return Err(io::Error::other(SUPERSEDED));
        }
        policy.snapshot.lock()?.replace(&policy.text)?;
        Ok(())
    })
    .map(|_| ())
}

/// Whether the name file in `state` holds `number`.
///
/// # Errors
/// The name file cannot be read ([`file::read`]).
fn holds(state: &Path, number: Number) -> io::Result<bool> {
    Ok(file::read(state)?.is_some_and(|name| name.num == Some(number)))
}

/// `Ok` while the name file holds `number`.
///
/// # Errors
/// [`Pending::Superseded`] once it holds another number or none, [`Pending::NameUnread`] when it
/// cannot be read.
fn still(state: &Path, number: Number) -> Result<(), Pending> {
    match holds(state, number) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Pending::Superseded(SUPERSEDED.to_owned())),
        Err(e) => Err(Pending::NameUnread(e.to_string())),
    }
}

/// A step that did not finish, and `why`, judged again by the name file: superseded, in the
/// step's own words, once the file holds another number or none; otherwise `step`'s, an unread
/// file included.
fn judged(state: &Path, number: Number, why: String, step: fn(String) -> Pending) -> Pending {
    if matches!(holds(state, number), Ok(false)) {
        Pending::Superseded(why)
    } else {
        step(why)
    }
}

/// The root half's four words for `number`: `exe` (this program, canonical), `name`,
/// `apply-system` and the number (`m59`).
///
/// # Errors
/// `this program's path is not UTF-8`.
pub fn apply_system_words(exe: &Path, number: Number) -> Result<Vec<String>, String> {
    let exe = exe.to_str().ok_or("this program's path is not UTF-8")?;
    Ok(vec![
        exe.to_owned(),
        "name".to_owned(),
        "apply-system".to_owned(),
        number.to_string(),
    ])
}

/// The number `words` ask the root half to set, where they are exactly [`apply_system_words`]
/// for `exe` and that number; `None` for any other words.
#[must_use]
pub fn is_apply_system(words: &[String], exe: &Path) -> Option<Number> {
    let number = Number::parse(words.get(3)?).ok()?;
    (apply_system_words(exe, number).ok()? == words).then_some(number)
}

/// Whether each of `names` ([`SYSTEM_NAMES`]' values) reads `number` (`m59`).
#[must_use]
pub fn all_read(names: &[Option<String>; 3], number: Number) -> bool {
    let number = number.to_string();
    names
        .iter()
        .all(|name| name.as_deref() == Some(number.as_str()))
}

/// The only way [`publish`] reads or sets macOS's three system names: `aterm name set` asks
/// through `aterm ctl sudo`, and a test's arm is a fake.
pub trait SystemNames {
    /// Each of [`SYSTEM_NAMES`], in that order: `None` where it is unset or unread.
    fn read(&self) -> [Option<String>; 3];

    /// Set all three to `number` under one approval.
    ///
    /// # Errors
    /// Why they were not set, worded for the person.
    fn apply(&self, number: Number) -> Result<(), String>;
}

/// macOS's three system names as Apple's `scutil --get` reads each within 10 seconds: `None`
/// for one it reports unset, fails to read, or does not answer in time.
#[cfg(target_os = "macos")]
#[must_use]
pub fn read_system_names() -> [Option<String>; 3] {
    SYSTEM_NAMES.map(|field| {
        let mut command = aterm_uds::spawnfd::command("/usr/sbin/scutil");
        command.args(["--get", field]);
        aterm_uds::deadline::output_within(&mut command, SCUTIL_WAIT)
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    })
}

/// Set each of [`SYSTEM_NAMES`] to `number` with Apple's fixed `/usr/sbin/scutil --set`, within
/// 10 seconds each, reading no configuration, registry or user path: `aterm name apply-system`'s
/// root half, once it has checked that it runs as root. Answers the fields scutil did not set.
#[cfg(target_os = "macos")]
#[must_use]
pub fn set_as_root(number: Number) -> Vec<&'static str> {
    let number = number.to_string();
    SYSTEM_NAMES
        .into_iter()
        .filter(|field| {
            let mut command = aterm_uds::spawnfd::command("/usr/sbin/scutil");
            command.args(["--set", field, &number]);
            !matches!(
                aterm_uds::deadline::output_within(&mut command, SCUTIL_WAIT),
                Ok(out) if out.status.success()
            )
        })
        .collect()
}

/// The step of [`publish`] that did not finish, with why in that step's own words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pending {
    /// aterm.toml's naming policy was not written.
    Policy(String),
    /// The fleet name was not written.
    FleetName(String),
    /// macOS's three names were not set, or did not keep the number.
    SystemNames(String),
    /// The name file could not be read to say whether it still holds the number.
    NameUnread(String),
    /// The name file holds another number (or none): a newer set replaced this one, which owes
    /// nothing more. The words are those of the step that found it.
    Superseded(String),
}

impl Pending {
    /// Why, in the words of the step that found it: what `aterm name set` prints after
    /// `publication pending: `.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Self::Policy(why)
            | Self::FleetName(why)
            | Self::SystemNames(why)
            | Self::NameUnread(why)
            | Self::Superseded(why) => why,
        }
    }

    /// What is still owed, worded to follow `m86 is this Mac's number now, but `.
    #[must_use]
    pub fn phrase(&self) -> String {
        match self {
            Self::Policy(why) => format!("aterm.toml does not name it yet ({why})"),
            Self::FleetName(why) => format!("its fleet name is not published yet ({why})"),
            Self::SystemNames(why) => {
                format!("macOS still calls this Mac by its old name ({why})")
            }
            Self::NameUnread(why) => format!("its name could not be read back ({why})"),
            Self::Superseded(_) => "a newer machine name replaced it".to_owned(),
        }
    }
}

/// Publish `policy`'s number, which [`super::set_name`] has committed in `state`, in order:
/// 1. aterm.toml's policy ([`Policy`]), under the name file's lock while it holds the number;
/// 2. the fleet name in `state`, through `aterm fleet name set`'s checked writer
///    ([`crate::fleet_member::choose_machine_name_at`]), which reads the `[[fleet.peers]]` of
///    the aterm.toml at `config` among the machines that may already answer to it and checks
///    the number again under the fleet writer's lock;
/// 3. with `system`, macOS's three names: read, and where any does not read the number, set
///    under one approval and read again.
///
/// The name file is read again after the fleet name, and again once macOS's names read the
/// number or have been set; a file that holds another number (or none) then ends publication
/// as [`Pending::Superseded`].
///
/// # Errors
/// The first step that did not finish; later steps do not run. A step that failed while the
/// name file no longer holds the number is [`Pending::Superseded`], in that step's words; a name
/// file that cannot be read at one of those checks is [`Pending::NameUnread`].
pub fn publish(
    state: &Path,
    config: &Path,
    policy: &Policy,
    system: Option<&dyn SystemNames>,
) -> Result<(), Pending> {
    publish_through(state, policy, system, || {
        crate::fleet_member::choose_machine_name_at(state, config, policy.number).map(|_| ())
    })
}

/// [`publish`] with its fleet step given as `fleet`: the fleet name's checked write of
/// `policy`'s number, answering why it did not finish.
///
/// # Errors
/// As [`publish`], with `fleet`'s words for the fleet step.
fn publish_through(
    state: &Path,
    policy: &Policy,
    system: Option<&dyn SystemNames>,
    fleet: impl FnOnce() -> Result<(), String>,
) -> Result<(), Pending> {
    let number = policy.number;
    write_policy(state, policy)
        .map_err(|e| judged(state, number, e.to_string(), Pending::Policy))?;
    fleet().map_err(|why| judged(state, number, why, Pending::FleetName))?;
    still(state, number)?;
    let Some(system) = system else {
        return Ok(());
    };
    let observed = system.read();
    still(state, number)?;
    if all_read(&observed, number) {
        return Ok(());
    }
    system
        .apply(number)
        .map_err(|why| judged(state, number, why, Pending::SystemNames))?;
    still(state, number)?;
    let observed = system.read();
    still(state, number)?;
    if !all_read(&observed, number) {
        return Err(judged(
            state,
            number,
            "macOS did not retain all three system names".to_owned(),
            Pending::SystemNames,
        ));
    }
    Ok(())
}

/// What a machine name's number still owes its readers ([`publish`]'s steps).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Publication {
    /// aterm.toml holds machine naming off: why.
    Off(String),
    /// Nothing is owed: the number is published, or it is no chosen number.
    Current,
    /// What does not read the chosen number yet, in [`publish`]'s order.
    Owed(Vec<Owed>),
}

/// One step of [`publish`] that does not read a chosen number yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owed {
    /// aterm.toml's naming keys that do not name it, as a person reads them (`[ai] machine`).
    Policy(Vec<&'static str>),
    /// The fleet name as it reads, `None` while unset.
    FleetName(Option<String>),
    /// Each of macOS's names that does not read it, with what it reads (`None`: unset or unread).
    SystemNames(Vec<(&'static str, Option<String>)>),
}

impl Owed {
    /// What this step reads instead of `number`: `aterm.toml's [ai] machine not yet m86`,
    /// `the fleet name is unset`, `macOS's ComputerName is m184 and its HostName is unset`.
    #[must_use]
    pub fn phrase(&self, number: Number) -> String {
        match self {
            Self::Policy(keys) => format!("aterm.toml's {} not yet {number}", keys.join(", ")),
            Self::FleetName(Some(name)) => format!("the fleet name is {name}"),
            Self::FleetName(None) => "the fleet name is unset".to_owned(),
            Self::SystemNames(names) => {
                let said: Vec<String> = names
                    .iter()
                    .enumerate()
                    .map(|(at, (field, value))| {
                        let value = value.as_deref().unwrap_or("unset");
                        if at == 0 {
                            format!("macOS's {field} is {value}")
                        } else {
                            format!("its {field} is {value}")
                        }
                    })
                    .collect();
                match said.split_last() {
                    Some((last, [])) => last.clone(),
                    Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
                    None => String::new(),
                }
            }
        }
    }
}

/// [`publication_of`] over the aterm.toml at `config`, the fleet name in `state` and macOS's
/// `names` where they were read. It takes no lock, makes no node key and spawns nothing.
///
/// # Errors
/// aterm.toml or the fleet name cannot be read, or aterm.toml is not valid TOML.
pub fn publication(
    state: &Path,
    config: &Path,
    name: &MachineName,
    names: Option<&[Option<String>; 3]>,
) -> Result<Publication, String> {
    let snapshot = ConfigFile::open(config).map_err(|e| e.to_string())?;
    let text = snapshot.text().map_err(|e| e.to_string())?;
    let fleet = aterm_net::fleet_member::read_primary_name(state)?;
    publication_of(text, fleet.as_deref(), name, names)
}

/// What `name`'s number still owes, from aterm.toml's text (`None`: no file), the fleet name
/// (`None`: unset) and macOS's `names` (`None`: not read, so not judged): [`Publication::Off`]
/// under the naming hold whatever the name; [`Publication::Current`] for a name that is not a
/// chosen number (an automatic or seeded one, an unnumbered one, or one whose migration waits,
/// which publishes no number), since only a chosen number is projected to the other readers;
/// otherwise each step that does not read it.
///
/// # Errors
/// aterm.toml is not valid TOML (`invalid aterm.toml: …`).
pub fn publication_of(
    config_text: Option<&str>,
    fleet_name: Option<&str>,
    name: &MachineName,
    names: Option<&[Option<String>; 3]>,
) -> Result<Publication, String> {
    let doc = parsed(config_text.unwrap_or(""))?;
    if held_off(&doc) {
        return Ok(Publication::Off(HELD_OFF.to_owned()));
    }
    let Some(number) = name
        .num
        .filter(|_| name.class == Class::Chosen && name.migration != Some(Migration::Waiting))
    else {
        return Ok(Publication::Current);
    };
    let word = number.to_string();
    let mut owed = Vec::new();
    let keys: Vec<&'static str> = POLICY_KEYS
        .iter()
        .filter(|(section, key, _)| {
            doc.get(section)
                .and_then(Item::as_table_like)
                .and_then(|table| table.get(key))
                .and_then(Item::as_str)
                != Some(word.as_str())
        })
        .map(|(_, _, label)| *label)
        .collect();
    if !keys.is_empty() {
        owed.push(Owed::Policy(keys));
    }
    if fleet_name != Some(word.as_str()) {
        owed.push(Owed::FleetName(fleet_name.map(str::to_owned)));
    }
    if let Some(names) = names {
        let differ: Vec<(&'static str, Option<String>)> = SYSTEM_NAMES
            .into_iter()
            .zip(names)
            .filter(|(_, value)| value.as_deref() != Some(word.as_str()))
            .map(|(field, value)| (field, value.clone()))
            .collect();
        if !differ.is_empty() {
            owed.push(Owed::SystemNames(differ));
        }
    }
    Ok(if owed.is_empty() {
        Publication::Current
    } else {
        Publication::Owed(owed)
    })
}

#[cfg(test)]
mod tests;
