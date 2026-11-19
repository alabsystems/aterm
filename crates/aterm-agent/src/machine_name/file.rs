// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `<state>/machine-name.toml` (`docs/DESIGN-machine-names-2026-11-04.md` §3.3): this machine's
//! NAME, its claim's fields, the nickname, `was`, a pending migration and the claim it last held
//! in each of its newest four fleets. It is the local mirror the title reads (`m8 Heron`, or
//! `unnumbered Otter` while unnumbered), so the menu, the ❯ item and Settings read it. Who
//! renamed this Mac, when, and every earlier name are its lab registry row's, which the registry
//! keeps ([`super::history`]); this file keeps none of them. A file an earlier build wrote with
//! `by` and `[[history]]` tables reads as the same name, and its next write drops them.
//!
//! The first reader to find none creates it under its own `machine-name.lock` from what the
//! caller computes from the bound registry row, roster or older `fleet-name`, else a drawn
//! noun; every other reader reads it back. A change replaces it atomically under that lock.
//! A number change takes the book lock first while it checks other nodes' claims.

use std::io;
use std::path::Path;
use std::time::Duration;

use aterm_net::fleet_member::name_record::{NickClass, node_of, node_word};
use aterm_types::machine_name::{
    Cap, Class, Nickname, NodeId, Number, UNNUMBERED, WAS_MAX, Was, nouns,
};
use aterm_uds::private_file::{PrivateFileError, PrivateLock, read_private, write_private};
use serde::{Deserialize, Serialize};

/// The file's name in the state directory.
pub const FILE: &str = "machine-name.toml";

/// Its lock's name beside it.
pub const LOCK: &str = "machine-name.lock";

/// The most fleets whose claim it keeps (§3.3).
pub const FLEETS_MAX: usize = 4;

/// The most bytes a well-formed file takes, with room to spare.
pub const FILE_MAX: usize = 16 * 1024;

/// Hang detector for required name creation or replacement behind a native writer.
pub const LOCK_HANG: Duration = Duration::from_secs(60);

/// A migration from an older build's names that has not finished (§7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Migration {
    /// The NAME came from an older build's evidence and its record is not signed yet.
    Pending,
    /// The rival's record was stripped: the node publishes no NAME and fetches it first-hand,
    /// keeping its NAME here, so the title still shows it.
    Waiting,
}

/// A claim as a fleet's entry keeps it: what a NAME was held as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// The NAME; `None` for a fleet where it held none.
    pub num: Option<Number>,
    /// Its class.
    pub class: Class,
    /// Its since.
    pub since: u64,
    /// Its take chain's origin; `None` for this node.
    pub base: Option<NodeId>,
    /// Its take generation.
    pub tgen: u32,
    /// Its succession generation.
    pub sgen: u32,
}

/// What this machine kept of one fleet it held a claim in (§3.3 `fleets`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetClaim {
    /// That fleet's seed digest.
    pub digest: [u8; 8],
    /// The highest number seen in its signed evidence.
    pub top: u32,
    /// The claim last held there.
    pub claim: Held,
    /// The numbers lost there in a contest, never reclaimed by itself.
    pub lost: Vec<Number>,
    /// A sync of the vault's default fleet put it there.
    pub synced: bool,
}

/// What a name file last saw of this Mac's own lab registry row's nickname
/// ([`MachineName::row`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RowSeen {
    /// No read of this build looked at the row: a file an earlier build wrote, or one made while
    /// no row was this Mac's.
    #[default]
    Never,
    /// The row's chosen nickname (a person's or a program's), or `None` where the cell was
    /// empty, an automatic path wrote it, or it was conflicted.
    Saw(Option<Nickname>),
}

/// `num` and the nickname `nick` as a title spells them: `m8 Heron`, or `unnumbered Heron`
/// ([`UNNUMBERED`]) while unnumbered. [`MachineName::title`] and the name an undo leaves are
/// this one spelling.
#[must_use]
pub fn title_of(num: Option<Number>, nick: &str) -> String {
    match num {
        Some(num) => format!("{num} {nick}"),
        None => format!("{UNNUMBERED} {nick}"),
    }
}

/// What `machine-name.toml` holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineName {
    /// The NAME; `None` while unnumbered (no assigned ID).
    pub num: Option<Number>,
    /// How it was claimed.
    pub class: Class,
    /// When this node first held it in this fleet (ms), or its evidence rank (§7.1).
    pub since: u64,
    /// Rises on every change.
    pub rev: u64,
    /// The NAME before the last change.
    pub prev: Option<Number>,
    /// The take chain's origin; `None` for this node.
    pub base: Option<NodeId>,
    /// The take generation.
    pub tgen: u32,
    /// The succession generation.
    pub sgen: u32,
    /// When this node last changed its claim (ms).
    pub act: u64,
    /// A person's cap on `top`.
    pub cap: Option<Cap>,
    /// Up to [`WAS_MAX`] earlier NAMEs, newest first.
    pub was: Vec<Was>,
    /// The nickname.
    pub nick: Nickname,
    /// How it was set.
    pub nick_class: NickClass,
    /// What this file last saw of this Mac's own lab registry row's nickname: a read follows
    /// the row only once it says something else ([`crate::machine_name::follow_registry`]), so
    /// the row still holding the value a rename replaced undoes nothing.
    pub row: RowSeen,
    /// The first name was made while this Mac's lab registry could not be read yet
    /// ([`crate::machine_name::RegistryNameRead::Pending`]), and nothing changed it since (a
    /// rename, a nickname, an undo or a followed number or nickname clears it,
    /// [`MachineName::made_own`]): the first read that finds the registry settles it as a first
    /// read then would have, its bound row's number and nickname taking the name's place whoever
    /// wrote them, or the name standing where no row binds this Mac.
    pub registry_unread: bool,
    /// A migration that has not finished.
    pub migration: Option<Migration>,
    /// The newest [`FLEETS_MAX`] fleets' kept claims, newest first.
    pub fleets: Vec<FleetClaim>,
}

impl MachineName {
    /// A machine never numbered: `unnumbered` and `nick`, drawn (`auto`), at revision zero.
    #[must_use]
    pub const fn unnumbered(nick: Nickname, now_ms: u64) -> Self {
        Self {
            num: None,
            class: Class::Auto,
            since: now_ms,
            rev: 0,
            prev: None,
            base: None,
            tgen: 0,
            sgen: 0,
            act: now_ms,
            cap: None,
            was: Vec::new(),
            nick,
            nick_class: NickClass::Auto,
            row: RowSeen::Never,
            registry_unread: false,
            migration: None,
            fleets: Vec::new(),
        }
    }

    /// This name as a change made it, which is the name's own from then on: no later registry
    /// read replaces it as a first name ([`MachineName::registry_unread`]).
    #[must_use]
    pub fn made_own(mut self) -> Self {
        self.registry_unread = false;
        self
    }

    /// The NAME: `m8`, or `unnumbered` while unnumbered.
    #[must_use]
    pub fn name(&self) -> String {
        self.num
            .map_or_else(|| UNNUMBERED.to_owned(), |num| num.to_string())
    }

    /// The NAME and the whole nickname, `m8 Heron` (`unnumbered Otter` while unnumbered), as `aterm name`
    /// prints it; the menu bar clips a long nickname (`aterm_gui::machine_title`).
    #[must_use]
    pub fn title(&self) -> String {
        title_of(self.num, &self.display_nick())
    }

    /// The chosen display spelling, or the title-cased default noun.
    #[must_use]
    pub fn display_nick(&self) -> String {
        match self.nick_class {
            NickClass::Chosen => self.nick.as_str().to_owned(),
            NickClass::Auto => self.nick.display(),
        }
    }

    /// The file's text.
    ///
    /// # Errors
    /// A value TOML cannot hold (a time or `rev` past `i64::MAX`).
    pub fn to_toml(&self) -> Result<String, String> {
        let wire = Wire::of(self);
        let body = aterm_toml::to_string(&wire).map_err(|e| e.to_string())?;
        Ok(format!(
            "# This machine's name, kept by aterm (docs/DESIGN-machine-names-2026-11-04.md §3.3).\n{body}"
        ))
    }

    /// The name `text` spells.
    ///
    /// # Errors
    /// It is no TOML, or a field is outside its grammar.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let wire: Wire = aterm_toml::from_str(text).map_err(|e| e.to_string())?;
        wire.into_name()
    }
}

/// A nickname drawn from the noun list with the OS's CSPRNG, redrawn while `in_use` (§2.4).
///
/// # Errors
/// The CSPRNG cannot be read, or every default noun is already in use.
pub fn draw_nick(in_use: impl Fn(&str) -> bool) -> io::Result<Nickname> {
    let mut failed = None;
    let noun = nouns::draw(
        || {
            let mut bytes = [0; 4];
            if let Err(e) = aterm_uds::rand::fill(&mut bytes) {
                failed.get_or_insert(e);
            }
            u32::from_le_bytes(bytes)
        },
        in_use,
    );
    if let Some(e) = failed {
        return Err(e);
    }
    let noun = noun.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "all default names are already used by other machines",
        )
    })?;
    Nickname::parse(noun).map_err(|e| io::Error::other(format!("noun {noun}: {e}")))
}

/// This machine's name, `None` when no file is there yet.
///
/// # Errors
/// It cannot be read privately, or it is malformed.
pub fn read(state: &Path) -> io::Result<Option<MachineName>> {
    let bytes = match read_private(state, FILE, FILE_MAX) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(e) => return Err(io::Error::other(format!("{FILE}: {e}"))),
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{FILE}: not UTF-8")))?;
    MachineName::from_toml(text)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{FILE}: {e}")))
}

/// This machine's name, created from `first` when no file is there: under [`LOCK`], read again,
/// and written whole by an atomic rename, so of two first readers one writes and the other reads
/// that back, and no reader meets a part of it or a second link to it. `true` beside it when
/// this call wrote it. `first` runs under the lock, which comes after the book's: it takes
/// none.
///
/// # Errors
/// As [`read`], the lock is held past [`LOCK_HANG`], `first` fails, or the file cannot be
/// written.
pub fn read_or_create(
    state: &Path,
    first: impl FnOnce() -> io::Result<MachineName>,
) -> io::Result<(MachineName, bool)> {
    read_or_create_with_lock(state, first, take_name_lock)
}

fn take_name_lock(state: &Path, within: Duration) -> io::Result<PrivateLock> {
    PrivateLock::take(state, LOCK, within, "machine-name.lock is busy")
}

fn read_or_create_with_lock(
    state: &Path,
    first: impl FnOnce() -> io::Result<MachineName>,
    take: impl FnOnce(&Path, Duration) -> io::Result<PrivateLock>,
) -> io::Result<(MachineName, bool)> {
    if let Some(name) = read(state)? {
        return Ok((name, false));
    }
    let _lock = take(state, LOCK_HANG)?;
    if let Some(name) = read(state)? {
        return Ok((name, false));
    }
    let name = first()?;
    let text = name.to_toml().map_err(io::Error::other)?;
    write_private(state, FILE, text.as_bytes())?;
    Ok((name, true))
}

/// Change this machine's name with `edit` under [`LOCK`], replacing the file atomically when the
/// name changed; the caller sets `rev` and `act` as the change requires. Answers the name as it
/// now stands.
///
/// # Errors
/// The lock is held past [`LOCK_HANG`], there is no file yet ([`read_or_create`] makes it), it
/// cannot be read or written, or `edit` refuses.
pub fn replace(
    state: &Path,
    edit: impl FnOnce(&mut MachineName) -> io::Result<()>,
) -> io::Result<MachineName> {
    replace_with_lock(state, edit, take_name_lock)
}

/// Hold the name writer before admitting a source, then persist only inside that admission.
pub(super) struct Writer<'a> {
    state: &'a Path,
    name: MachineName,
    _lock: PrivateLock,
}

pub(super) fn writer(state: &Path) -> io::Result<Writer<'_>> {
    let lock = take_name_lock(state, LOCK_HANG)?;
    let name = read(state)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("{FILE}: not created yet"))
    })?;
    Ok(Writer {
        state,
        name,
        _lock: lock,
    })
}

impl Writer<'_> {
    pub(super) fn name(&self) -> &MachineName {
        &self.name
    }

    pub(super) fn replace(&mut self, name: MachineName) -> io::Result<()> {
        if name != self.name {
            let text = name.to_toml().map_err(io::Error::other)?;
            write_private(self.state, FILE, text.as_bytes())?;
            self.name = name;
        }
        Ok(())
    }
}

fn replace_with_lock(
    state: &Path,
    edit: impl FnOnce(&mut MachineName) -> io::Result<()>,
    take: impl FnOnce(&Path, Duration) -> io::Result<PrivateLock>,
) -> io::Result<MachineName> {
    replace_admitted_with_lock(state, edit, |write| write(), take)
}

/// [`replace`], with the file written only inside `admit`: the name lock is taken and the edit
/// made first, and `admit` runs the write it is handed where its source admits it, or answers
/// why not, which leaves the file as it was. An edit that changes nothing asks no admission.
///
/// # Errors
/// As [`replace`], and `admit`'s refusal.
pub(super) fn replace_admitted(
    state: &Path,
    edit: impl FnOnce(&mut MachineName) -> io::Result<()>,
    admit: impl FnOnce(&mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
) -> io::Result<MachineName> {
    replace_admitted_with_lock(state, edit, admit, take_name_lock)
}

fn replace_admitted_with_lock(
    state: &Path,
    edit: impl FnOnce(&mut MachineName) -> io::Result<()>,
    admit: impl FnOnce(&mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
    take: impl FnOnce(&Path, Duration) -> io::Result<PrivateLock>,
) -> io::Result<MachineName> {
    let _lock = take(state, LOCK_HANG)?;
    let held = read(state)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("{FILE}: not created yet"))
    })?;
    let mut name = held.clone();
    edit(&mut name)?;
    if name != held {
        let text = name.to_toml().map_err(io::Error::other)?;
        admit(&mut || write_private(state, FILE, text.as_bytes()))?;
    }
    Ok(name)
}

#[derive(Serialize, Deserialize)]
struct Wire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    num: Option<u32>,
    class: String,
    since: u64,
    rev: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prev: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(default)]
    tgen: u32,
    #[serde(default)]
    sgen: u32,
    act: u64,
    nick: String,
    nick_class: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    row_seen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    row_nick: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    registry_unread: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    migration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cap: Option<WireCap>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    was: Vec<WireWas>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    fleets: Vec<WireFleet>,
}

#[derive(Serialize, Deserialize)]
struct WireCap {
    value: u32,
    since: u64,
}

#[derive(Serialize, Deserialize)]
struct WireHeld {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    num: Option<u32>,
    class: String,
    since: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    #[serde(default)]
    tgen: u32,
    #[serde(default)]
    sgen: u32,
}

#[derive(Serialize, Deserialize)]
struct WireWas {
    num: u32,
    #[serde(default)]
    retired: bool,
    #[serde(default)]
    lost: bool,
}

#[derive(Serialize, Deserialize)]
struct WireFleet {
    digest: String,
    top: u32,
    claim: WireHeld,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    lost: Vec<u32>,
    #[serde(default)]
    synced: bool,
}

/// How the file spells `class`, as `aterm name` prints it.
#[must_use]
pub const fn class_word(class: Class) -> &'static str {
    match class {
        Class::Auto => "auto",
        Class::Chosen => "chosen",
        Class::Seeded => "seeded",
    }
}

/// How the file spells a nickname's `class`.
#[must_use]
pub const fn nick_class_word(class: NickClass) -> &'static str {
    match class {
        NickClass::Auto => "auto",
        NickClass::Chosen => "chosen",
    }
}

/// How the file spells `migration`.
#[must_use]
pub const fn migration_word(migration: Migration) -> &'static str {
    match migration {
        Migration::Pending => "pending",
        Migration::Waiting => "waiting",
    }
}

fn class_of(word: &str) -> Result<Class, String> {
    match word {
        "auto" => Ok(Class::Auto),
        "chosen" => Ok(Class::Chosen),
        "seeded" => Ok(Class::Seeded),
        _ => Err(format!("class `{word}` is none of auto, chosen, seeded")),
    }
}

fn number(n: u32, field: &str) -> Result<Number, String> {
    Number::new(n).ok_or_else(|| format!("{field} {n} is no NAME (m1 to m{})", Number::MAX))
}

fn node(word: &str, field: &str) -> Result<NodeId, String> {
    node_of(word).ok_or_else(|| format!("{field} `{word}` is no node id"))
}

impl WireHeld {
    fn of(held: &Held) -> Self {
        Self {
            num: held.num.map(Number::get),
            class: class_word(held.class).to_owned(),
            since: held.since,
            base: held.base.map(node_word),
            tgen: held.tgen,
            sgen: held.sgen,
        }
    }

    fn into_held(self, field: &str) -> Result<Held, String> {
        Ok(Held {
            num: self.num.map(|n| number(n, field)).transpose()?,
            class: class_of(&self.class)?,
            since: self.since,
            base: self
                .base
                .as_deref()
                .map(|word| node(word, field))
                .transpose()?,
            tgen: self.tgen,
            sgen: self.sgen,
        })
    }
}

impl Wire {
    fn of(name: &MachineName) -> Self {
        Self {
            num: name.num.map(Number::get),
            class: class_word(name.class).to_owned(),
            since: name.since,
            rev: name.rev,
            prev: name.prev.map(Number::get),
            base: name.base.map(node_word),
            tgen: name.tgen,
            sgen: name.sgen,
            act: name.act,
            nick: name.nick.as_str().to_owned(),
            nick_class: nick_class_word(name.nick_class).to_owned(),
            row_seen: name.row != RowSeen::Never,
            row_nick: match &name.row {
                RowSeen::Saw(Some(nick)) => Some(nick.as_str().to_owned()),
                RowSeen::Never | RowSeen::Saw(None) => None,
            },
            registry_unread: name.registry_unread,
            migration: name
                .migration
                .map(|migration| migration_word(migration).to_owned()),
            cap: name.cap.map(|cap| WireCap {
                value: cap.value,
                since: cap.since,
            }),
            was: name
                .was
                .iter()
                .map(|was| WireWas {
                    num: was.num.get(),
                    retired: was.retired,
                    lost: was.lost,
                })
                .collect(),
            fleets: name
                .fleets
                .iter()
                .map(|fleet| WireFleet {
                    digest: aterm_codec::hex::encode_lower(&fleet.digest),
                    top: fleet.top,
                    claim: WireHeld::of(&fleet.claim),
                    lost: fleet.lost.iter().map(|num| num.get()).collect(),
                    synced: fleet.synced,
                })
                .collect(),
        }
    }

    fn into_name(self) -> Result<MachineName, String> {
        if self.was.len() > WAS_MAX {
            return Err(format!("more than {WAS_MAX} `was` entries"));
        }
        if self.fleets.len() > FLEETS_MAX {
            return Err(format!("more than {FLEETS_MAX} fleets"));
        }
        let fleets = self
            .fleets
            .into_iter()
            .map(|fleet| {
                let mut digest = [0; 8];
                if !(aterm_codec::hex::is_lower(&fleet.digest, 16)
                    && aterm_codec::hex::decode_into(fleet.digest.as_bytes(), &mut digest))
                {
                    return Err(format!("fleet digest `{}` is not 16 hex", fleet.digest));
                }
                Ok(FleetClaim {
                    digest,
                    top: fleet.top,
                    claim: fleet.claim.into_held("fleet claim")?,
                    lost: fleet
                        .lost
                        .into_iter()
                        .map(|n| number(n, "fleet lost"))
                        .collect::<Result<_, String>>()?,
                    synced: fleet.synced,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(MachineName {
            num: self.num.map(|n| number(n, "num")).transpose()?,
            class: class_of(&self.class)?,
            since: self.since,
            rev: self.rev,
            prev: self.prev.map(|n| number(n, "prev")).transpose()?,
            base: self
                .base
                .as_deref()
                .map(|word| node(word, "base"))
                .transpose()?,
            tgen: self.tgen,
            sgen: self.sgen,
            act: self.act,
            cap: self.cap.map(|cap| Cap {
                value: cap.value,
                since: cap.since,
            }),
            was: self
                .was
                .into_iter()
                .map(|was| {
                    Ok(Was {
                        num: number(was.num, "was")?,
                        retired: was.retired,
                        lost: was.lost,
                    })
                })
                .collect::<Result<_, String>>()?,
            nick: Nickname::parse(&self.nick).map_err(|e| format!("nick: {e}"))?,
            nick_class: match self.nick_class.as_str() {
                "auto" => NickClass::Auto,
                "chosen" => NickClass::Chosen,
                other => return Err(format!("nick_class `{other}` is none of auto, chosen")),
            },
            row: match (self.row_seen, self.row_nick.as_deref()) {
                (false, None) => RowSeen::Never,
                (false, Some(_)) => return Err("row_nick without row_seen".to_owned()),
                (true, word) => RowSeen::Saw(
                    word.map(|word| Nickname::parse(word).map_err(|e| format!("row_nick: {e}")))
                        .transpose()?,
                ),
            },
            registry_unread: self.registry_unread,
            migration: self
                .migration
                .map(|word| match word.as_str() {
                    "pending" => Ok(Migration::Pending),
                    "waiting" => Ok(Migration::Waiting),
                    other => Err(format!("migration `{other}` is none of pending, waiting")),
                })
                .transpose()?,
            fleets,
        })
    }
}

#[cfg(test)]
mod tests;
