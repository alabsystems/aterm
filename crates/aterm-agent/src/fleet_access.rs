// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This machine's stored fleet access level (`<state>/fleet-access`), the one source of truth
//! every listener reads per request line and only this machine writes
//! (`docs/DESIGN-fleet-access-lock-2026-11-10.md` §4).
//!
//! The file is private state, three lines:
//!
//! ```text
//! level=read|write|full
//! at=<unix seconds>
//! by=menu|cli
//! ```
//!
//! Reading fails closed: a file that is missing, not private, too large, or does not hold
//! exactly one `level=` line naming a known level reads as Read Only ([`decode`]).

use std::ffi::CStr;
#[cfg(unix)]
use std::path::Path;

use aterm_types::fleet_access::FleetAccess;

/// The state file's name.
pub const FILE: &str = aterm_types::fleet_access::FILE;

/// The Darwin notification [`announce`] posts once the stored level changed: a running window
/// reads the file again and repaints its lock.
pub const CHANGED: &CStr = c"com.aterm.aterm.fleet-access.changed";

/// More than the three lines ever hold; a larger file is not this module's.
#[cfg(unix)]
const MAX: usize = 256;

/// What changed the level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    /// The menu-bar lock.
    Menu,
    /// `aterm fleet access`.
    Cli,
}

impl By {
    /// The word in the file's `by=` line.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Menu => "menu",
            Self::Cli => "cli",
        }
    }

    /// What a person reads for it.
    #[must_use]
    pub const fn said(self) -> &'static str {
        match self {
            Self::Menu => "the menu-bar lock",
            Self::Cli => "`aterm fleet access`",
        }
    }

    /// The writer a `by=` line's word names; only [`read`] asks.
    #[cfg(unix)]
    fn from_word(word: &str) -> Option<Self> {
        [Self::Menu, Self::Cli]
            .into_iter()
            .find(|by| by.word() == word)
    }
}

/// Why a stored level that is present was not used, so the level in force is Read Only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unread {
    /// The file or its folder is open to another user, or is a link.
    NotPrivate(String),
    /// The file is no regular file of at most 256 bytes.
    Malformed(String),
    /// The file could not be opened or read.
    Io(String),
    /// The file does not hold exactly one `level=` line naming a level.
    NotUnderstood,
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPrivate(why) | Self::Malformed(why) | Self::Io(why) => f.write_str(why),
            Self::NotUnderstood => {
                f.write_str("it does not hold exactly one `level=read|write|full` line")
            }
        }
    }
}

/// The level in force and what the file said beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The level every listener enforces: Read Only unless the file names another.
    pub level: FleetAccess,
    /// Why a file that is there was not used. Enforcement ignores it; the CLI and the journal
    /// print it.
    pub unread: Option<Unread>,
    /// When the level was last written, in unix seconds, where the file says.
    pub at: Option<u64>,
    /// What wrote it, where the file says.
    pub by: Option<By>,
}

/// A written change of level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    /// The level in force before.
    pub from: FleetAccess,
    /// The level in force now.
    pub to: FleetAccess,
}

/// The level in `line`, where it is a `level=` line naming one.
fn level_of(line: &str) -> Option<Option<FleetAccess>> {
    line.strip_prefix("level=").map(FleetAccess::from_word)
}

/// The level a file of `bytes` (`None`: no file) puts in force. Only a file with exactly one
/// `level=` line, naming a level, raises it above Read Only: a missing or empty file, one that is
/// not UTF-8, an unknown word and two `level=` lines (whichever they name) are all Read Only.
#[must_use]
pub fn decode(bytes: Option<&[u8]>) -> FleetAccess {
    let Some(text) = bytes.and_then(|bytes| std::str::from_utf8(bytes).ok()) else {
        return FleetAccess::ReadOnly;
    };
    let mut levels = text.lines().filter_map(level_of);
    match (levels.next(), levels.next()) {
        (Some(Some(level)), None) => level,
        _ => FleetAccess::ReadOnly,
    }
}

/// The level stored under `state`, failing closed ([`decode`]).
#[cfg(unix)]
#[must_use]
pub fn read(state: &Path) -> Reading {
    use aterm_uds::private_file::PrivateFileError as E;
    let closed = |unread| Reading {
        level: FleetAccess::ReadOnly,
        unread,
        at: None,
        by: None,
    };
    let bytes = match aterm_uds::private_file::read_private(state, FILE, MAX) {
        Ok(bytes) => bytes,
        Err(E::Missing) => return closed(None),
        Err(error @ (E::NotPrivate(_) | E::Writable(_) | E::Symlink)) => {
            return closed(Some(Unread::NotPrivate(error.to_string())));
        }
        Err(error @ E::Malformed(_)) => {
            return closed(Some(Unread::Malformed(error.to_string())));
        }
        Err(error @ E::Io(_)) => return closed(Some(Unread::Io(error.to_string()))),
    };
    let level = decode(Some(&bytes));
    let text = std::str::from_utf8(&bytes).unwrap_or_default();
    // A file that says Read Only in so many words is understood; any other file that reads as
    // Read Only was not.
    let understood = level != FleetAccess::ReadOnly
        || text.lines().filter_map(level_of).collect::<Vec<_>>() == [Some(FleetAccess::ReadOnly)];
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
    };
    Reading {
        level,
        unread: (!understood).then_some(Unread::NotUnderstood),
        at: field("at").and_then(|at| at.parse().ok()),
        by: field("by").and_then(By::from_word),
    }
}

/// What the level file's place on disk says of it: a write replaces the file, so another
/// writer's statement shows here even where it states the level already in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// The file's inode.
    pub ino: u64,
    /// When it was last modified: seconds, then nanoseconds.
    pub modified: (i64, i64),
}

/// What a raise is asked over: the stored level as it stood ([`Stamp`]; `None`: no file) and
/// the fleet this machine held (`None`: no key it can read). A raise the lock's click asked for
/// is written only while both still stand ([`raise_over`]): a level another writer stated since,
/// a leave and a move to another fleet each change one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Basis {
    /// The level file, where there is one.
    pub stored: Option<Stamp>,
    /// The fleet whose key this machine holds: its mark (`FleetSecret::mark`), as a number.
    pub fleet: Option<u64>,
}

/// What a raise asked now is asked over ([`Basis`]).
#[cfg(unix)]
#[must_use]
pub fn basis(state: &Path) -> Basis {
    use std::os::unix::fs::MetadataExt as _;
    Basis {
        stored: std::fs::symlink_metadata(state.join(FILE))
            .ok()
            .map(|meta| Stamp {
                ino: meta.ino(),
                modified: (meta.mtime(), meta.mtime_nsec()),
            }),
        fleet: aterm_net::fleet::read_key_file(state)
            .ok()
            .and_then(|secret| u64::from_str_radix(&secret.mark().hex(), 16).ok()),
    }
}

/// Store `level` under `state` as `by`'s statement, and read it back: a write the listeners
/// would not honour is an error. A level already in force is written again, so the file says
/// who stated it last and a raise pending elsewhere sees the statement ([`Basis`]). The book's
/// lock is waited for as any writer of the book waits ([`set_within`]).
/// It posts nothing: a caller whose change a running window should see calls [`announce`].
///
/// # Errors
/// As [`set_within`]'s.
#[cfg(unix)]
pub fn set(state: &Path, level: FleetAccess, by: By) -> std::io::Result<Change> {
    set_within(state, level, by, None)
}

/// [`set`], the book's lock waited for `within` at most (`None`: as any writer of the book
/// waits). A level above Read Only is written under that lock, a raise or not, so no join, init
/// or leave removes the level beneath the write and then finds it stored for the fleet it
/// moved to. Read Only is written with no lock: the lowest level binds whatever holds the book.
///
/// # Errors
/// The folder is not this user's private state, the book's lock was not taken for a level above
/// Read Only, the write failed, or the level read back is not `level` (naming why the file was
/// not used).
#[cfg(unix)]
pub fn set_within(
    state: &Path,
    level: FleetAccess,
    by: By,
    within: Option<std::time::Duration>,
) -> std::io::Result<Change> {
    aterm_uds::private_file::create_private_dir(state)?;
    if level == FleetAccess::ReadOnly {
        write(state, level, by)
    } else {
        held(state, within, || write(state, level, by))?
    }
}

/// Store `level`, a raise asked over `over`, as `by`'s change, where `state` still stands as
/// `over` says, under the book's lock waited for `within` at most: `None`, and nothing written,
/// where the stored level was stated again or this machine left or changed its fleet since the
/// raise was asked. The level is read back as [`set`]'s is. It posts nothing ([`announce`]).
///
/// # Errors
/// As [`set`]'s; the lock not taken within `within` among them.
#[cfg(unix)]
pub fn raise_over(
    state: &Path,
    level: FleetAccess,
    by: By,
    over: &Basis,
    within: std::time::Duration,
) -> std::io::Result<Option<Change>> {
    aterm_uds::private_file::create_private_dir(state)?;
    held(state, Some(within), || {
        if basis(state) == *over {
            write(state, level, by).map(Some)
        } else {
            Ok(None)
        }
    })?
}

/// `raise` under the book's lock in `state` (`aterm_net::fleet_member::access_held`).
#[cfg(unix)]
fn held<R>(
    state: &Path,
    within: Option<std::time::Duration>,
    raise: impl FnOnce() -> R,
) -> std::io::Result<R> {
    use aterm_net::fleet_member::Unwritten;
    aterm_net::fleet_member::access_held(state, within, raise).map_err(|unwritten| {
        std::io::Error::other(match unwritten {
            Unwritten::Lock(why) => why,
            Unwritten::Moved => "this machine's fleet changed meanwhile".to_owned(),
        })
    })
}

/// Write `level` as `by`'s statement into the private folder `state`, and read it back.
#[cfg(unix)]
fn write(state: &Path, level: FleetAccess, by: By) -> std::io::Result<Change> {
    let from = read(state).level;
    let text = format!(
        "level={}\nat={}\nby={}\n",
        level.word(),
        aterm_time::unix_secs().unwrap_or(0),
        by.word()
    );
    aterm_uds::private_file::write_private(state, FILE, text.as_bytes())?;
    let after = read(state);
    if after.level != level {
        let why = after.unread.map_or_else(
            || "another writer changed it meanwhile".to_owned(),
            |unread| unread.to_string(),
        );
        return Err(std::io::Error::other(format!(
            "{} was written and does not read back as `{}`: {why}",
            state.join(FILE).display(),
            level.word()
        )));
    }
    Ok(Change { from, to: level })
}

/// Put the level back to Read Only by removing the file, answering whether there was one.
/// It posts nothing ([`announce`]).
///
/// # Errors
/// The folder is another user's or a link, or the file could not be removed.
#[cfg(unix)]
pub fn reset(state: &Path) -> std::io::Result<bool> {
    aterm_uds::private_file::remove_private(state, FILE)
}

/// Tell every running window on this machine that the stored level changed ([`CHANGED`]). Off
/// macOS there is no such notification and nothing is posted; enforcement never depends on it,
/// since each listener reads the file per request line.
pub fn announce() {
    let _ = aterm_sysprobe::notify::post(CHANGED);
}

/// Whether session `sid` is a worker another fleet Mac started here: the driven window's record
/// of it under `<state>/`[`aterm_types::fleet_access::DRIVE_WORKERS`]. A plain shell another Mac
/// opened has no record, nor has a worker such a worker spawned locally.
#[cfg(unix)]
#[must_use]
pub fn fleet_started(state: &Path, sid: &str) -> bool {
    aterm_types::control_request::Sid::parse(sid).is_some()
        && std::fs::symlink_metadata(
            state
                .join(aterm_types::fleet_access::DRIVE_WORKERS)
                .join(sid),
        )
        .is_ok_and(|meta| meta.is_file())
}

#[cfg(test)]
#[path = "fleet_access_tests.rs"]
mod tests;
