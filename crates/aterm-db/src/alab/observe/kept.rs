// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What this Mac observed of its accounts, kept across passes and processes: each login it saw
//! (a CLI's login place, a tab's, a browser profile's) by its newest sighting, and whether a pass
//! that read the registry has taken that sighting yet.
//!
//! A pass that cannot run leaves its sightings here. On a Mac whose writer is fenced (a writer
//! succession still preserving old records, or a Mac that is no Writer yet) every pass fails, and
//! a sighting the in-memory queue took would otherwise be gone with the pass that dropped it; a
//! tab closed within the hour, or a process that ended, was never registered. Here it waits,
//! `pending`, and every later pass hands every pending sighting again until one whose plan ran
//! takes it ([`Kept::settle`]). The same file is what `aterm db alab check` compares with the
//! registry ([`super::super::checks::observed`]).
//!
//! The file is `<state>/alab/account-seen.json`, private (0600 in a 0700 folder), replaced whole
//! under its lock beside it, since every long-lived aterm process on a Mac passes sightings. It
//! holds login emails, never a credential, as the sweep's own file does.

use super::{Sighting, browser_only};
use aterm_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file's name in its folder.
pub const FILE: &str = "account-seen.json";
/// The lock beside it.
pub const LOCK: &str = "account-seen.lock";
/// How long a login taken by a pass is kept after its newest sighting: what `check` still
/// compares. A pending one is kept until a pass takes it, however old.
pub const KEEP_MS: u64 = 30 * 86_400_000;
/// How long a CLI's or a tab's sighting of a login holds it against a newer browser sighting of
/// the same login: the sweep hands every CLI login once a day, so one still there is heard again
/// well within this, and one that left gives way.
pub const HOLD_MS: u64 = 2 * 86_400_000;
/// The most logins the file keeps; more than any Mac's login places and profiles make.
pub const MOST: usize = 512;
/// The most bytes the file is read to.
const MAX_BYTES: usize = 1 << 20;
/// The file's format.
const VERSION: u64 = 1;

/// A login by (CLI, realm, email as the queue keys it).
type Key = (String, String, String);

fn key(s: &Sighting) -> Key {
    (
        s.cli.clone(),
        s.realm.clone(),
        s.email.trim().to_ascii_lowercase(),
    )
}

/// What the file says: each login's newest sighting and whether it waits for a pass. No `Debug`:
/// it holds login emails.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Kept {
    logins: BTreeMap<Key, (Sighting, bool)>,
}

impl Kept {
    /// Keep `sightings`, each pending: a sighting older than the one kept of its login is no news,
    /// and a browser's does not replace a CLI's or a tab's from within [`HOLD_MS`]. Either way
    /// round: a CLI's or a tab's from within [`HOLD_MS`] before a kept browser's replaces it, as
    /// another process that gathered it a little before this one's browser look would hand it.
    pub fn hold(&mut self, sightings: &[Sighting]) {
        for s in sightings {
            let key = key(s);
            if let Some((held, _)) = self.logins.get(&key) {
                let weaker = browser_only(s)
                    && !browser_only(held)
                    && s.at_ms < held.at_ms.saturating_add(HOLD_MS);
                let stronger = !browser_only(s)
                    && browser_only(held)
                    && held.at_ms < s.at_ms.saturating_add(HOLD_MS);
                if (s.at_ms < held.at_ms && !stronger) || weaker {
                    continue;
                }
            }
            self.logins.insert(key, (s.clone(), true));
        }
    }

    /// Every sighting no pass has taken yet.
    #[must_use]
    pub fn pending(&self) -> Vec<Sighting> {
        self.logins
            .values()
            .filter(|(_, pending)| *pending)
            .map(|(s, _)| s.clone())
            .collect()
    }

    /// Keep that a pass whose plan ran took `passed`: each login whose kept sighting is no newer
    /// than the one passed waits no more.
    pub fn settle(&mut self, passed: &[Sighting]) {
        for s in passed {
            if let Some((held, pending)) = self.logins.get_mut(&key(s))
                && held.at_ms <= s.at_ms
            {
                *pending = false;
            }
        }
    }

    /// Every login this Mac observed that its newest sighting does not say is signed out, each
    /// with whether it still waits for a pass: each pending one however old, and each taken one
    /// within [`KEEP_MS`] of `now_ms`. What `check` compares with the registry.
    #[must_use]
    pub fn observed(&self, now_ms: u64) -> Vec<(Sighting, bool)> {
        self.logins
            .values()
            .filter(|(s, _)| s.state != "signed-out")
            .filter(|(s, pending)| *pending || s.at_ms.saturating_add(KEEP_MS) >= now_ms)
            .cloned()
            .collect()
    }

    /// Drop each taken login last seen more than [`KEEP_MS`] before `now_ms`, then, past
    /// [`MOST`], the oldest taken ones, then the oldest of all.
    pub fn prune(&mut self, now_ms: u64) {
        self.logins
            .retain(|_, (s, pending)| *pending || s.at_ms.saturating_add(KEEP_MS) >= now_ms);
        while self.logins.len() > MOST {
            let oldest = |taken: bool| {
                self.logins
                    .iter()
                    .filter(|(_, (_, pending))| !taken || !*pending)
                    .min_by_key(|(_, (s, _))| s.at_ms)
                    .map(|(key, _)| key.clone())
            };
            let Some(key) = oldest(true).or_else(|| oldest(false)) else {
                break;
            };
            self.logins.remove(&key);
        }
    }

    /// The file's text.
    #[must_use]
    pub fn render(&self) -> String {
        let seen: Vec<Value> = self
            .logins
            .values()
            .map(|(s, pending)| {
                let mut row = Map::new();
                row.insert("cli".into(), Value::from(s.cli.as_str()));
                row.insert("realm".into(), Value::from(s.realm.as_str()));
                row.insert("email".into(), Value::from(s.email.as_str()));
                row.insert("state".into(), Value::from(s.state.as_str()));
                row.insert("tabs".into(), Value::from(s.tabs));
                row.insert("src".into(), Value::from(s.src.as_str()));
                row.insert("at".into(), Value::from(s.at_ms));
                row.insert("pending".into(), Value::Bool(*pending));
                Value::Object(row)
            })
            .collect();
        let mut file = Map::new();
        file.insert("v".into(), Value::from(VERSION));
        file.insert("seen".into(), Value::Array(seen));
        format!("{}\n", Value::Object(file))
    }

    /// What `bytes` of the file say. A file of another format, or that does not read, is empty;
    /// an entry that does not read is left out.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Self {
        let Ok(file) = aterm_json::from_slice::<Value>(bytes) else {
            return Self::default();
        };
        if file.get("v").and_then(Value::as_u64) != Some(VERSION) {
            return Self::default();
        }
        let mut kept = Self::default();
        for row in file
            .get("seen")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
        {
            let text = |col: &str| row.get(col).and_then(Value::as_str).map(str::to_owned);
            let read = || {
                Some((
                    Sighting {
                        cli: text("cli")?,
                        realm: text("realm")?,
                        email: text("email")?,
                        state: text("state")?,
                        tabs: row.get("tabs")?.as_u64()?,
                        src: text("src")?,
                        at_ms: row.get("at")?.as_u64()?,
                    },
                    row.get("pending")?.as_bool()?,
                ))
            };
            if let Some((s, pending)) = read() {
                kept.logins.insert(key(&s), (s, pending));
            }
        }
        kept
    }
}

/// The file's folder under the aterm state folder `state`: `<state>/alab`, beside the journal's
/// spool.
#[must_use]
pub fn under(state: &Path) -> PathBuf {
    state.join("alab")
}

/// This Mac's folder of the file, where the aterm state folder is known.
#[must_use]
pub fn local() -> Option<PathBuf> {
    aterm_types::dirs::state_dir().map(|state| under(&state))
}

/// What the file in `dir` says; nothing where it is absent or does not read.
#[must_use]
pub fn load(dir: &Path) -> Kept {
    aterm_uds::private_file::read_private(dir, FILE, MAX_BYTES)
        .map_or_else(|_| Kept::default(), |bytes| Kept::parse(&bytes))
}

/// Change the file in `dir` with `change` under its lock, pruned at `now_ms`, and replace it whole.
///
/// # Errors
/// The folder cannot be made private, the lock is held past ten seconds or cannot be taken, or
/// the file cannot be written; the file is then as it was.
pub fn update<R>(
    dir: &Path,
    now_ms: u64,
    change: impl FnOnce(&mut Kept) -> R,
) -> std::io::Result<R> {
    aterm_uds::private_file::ensure_private_dir(dir)?;
    let now = std::time::Instant::now();
    let deadline = now
        .checked_add(std::time::Duration::from_secs(10))
        .unwrap_or(now);
    let _lock = aterm_uds::file_lock::LockFile::take(
        &dir.join(LOCK),
        deadline,
        "another aterm process holds the account sightings",
    )?;
    let mut kept = load(dir);
    let out = change(&mut kept);
    kept.prune(now_ms);
    aterm_uds::private_file::write_private(dir, FILE, kept.render().as_bytes())?;
    Ok(out)
}

/// One pass through the file in `dir` at `now_ms`: keep `taken` (what the queue gathered since
/// the last pass), hand every sighting still pending to `pass`, and keep that it took what it
/// says it took ([`crate::alab::Observed::took`]): nothing where it read no registry, and none
/// whose birth or first row the quota refused. Returns what was handed and what `pass`
/// answered. Where the file cannot be kept, `taken` alone is handed, as a pass did before the
/// file was kept, and nothing is settled.
pub fn through<E>(
    dir: &Path,
    taken: &[Sighting],
    now_ms: u64,
    pass: impl FnOnce(&[Sighting]) -> Result<crate::alab::Observed, E>,
) -> (Vec<Sighting>, Result<crate::alab::Observed, E>) {
    let Ok(pending) = update(dir, now_ms, |kept| {
        kept.hold(taken);
        kept.pending()
    }) else {
        return (taken.to_vec(), pass(taken));
    };
    let done = pass(&pending);
    let took = done
        .as_ref()
        .map_or_else(|_| Vec::new(), |done| done.took(&pending));
    if !took.is_empty() {
        // Where this cannot be kept, the next pass hands them again and the planner writes
        // nothing for a row that says the same.
        let _ = update(dir, now_ms, |kept| kept.settle(&took));
    }
    (pending, done)
}
