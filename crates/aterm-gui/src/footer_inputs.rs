// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOOTER'S MACHINE INPUTS, DECODED ONCE A CHANGE: what every Claude Code footer read on the
//! resolver's thread shares of this machine's files (each login profile, the accounts store's
//! state and login bindings, the roster), held with the stamps of the files it was read from
//! ([`Stamp`]: device, inode, length, and modification and change times to the nanosecond) and
//! read again only where one of those stamps moved. While they stand a read only looks at the
//! files: it opens and decodes none of them (the footer cause design's step C3, law L5
//! OneDecodePerStamp).
//!
//! Each stamp is taken before the read it gates ([`Memo::fresh`]), so a write after the stamp
//! moves the next one: a write between a stamp and its read is read twice, never missed. Each is
//! also a look of the read it is taken in ([`looked`]): the resolver watches the path, and reads
//! again when it moves (`claude_inputs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_agent::harness::accounts;
use aterm_agent::harness::looks::{self, Heed, Input};
use aterm_agent::harness::profile::{self, ProfileRead};

/// What a path names now, as a memo compares it ([`aterm_uds::file_watch::FileStamp`]): `None`
/// where it names nothing or cannot be looked at. A rewrite in place of the same length in the
/// same second moves it (its modification and change times to the nanosecond), and so does a
/// replace by rename (its inode).
pub(crate) type Stamp = Option<aterm_uds::file_watch::FileStamp>;

/// [`Stamp`] of `path` now: one look at it, no open, noted for no read.
#[cfg(test)]
pub(crate) fn stamp(path: &Path) -> Stamp {
    aterm_uds::file_watch::FileStamp::of(path)
}

/// [`Stamp`] of `input`'s file at `path` now, noted as a look of the read this runs in
/// ([`looks::note`]).
pub(crate) fn looked(input: Input, path: &Path) -> Stamp {
    looks::note(input, path, Heed::All)
}

/// [`looked`] of each of `paths`, in their order.
fn looked_all(input: Input, paths: &[PathBuf]) -> Vec<Stamp> {
    paths.iter().map(|path| looked(input, path)).collect()
}

/// `read`, one gather of `input` (counted in a test build: `work_counts::FooterWork::gathers`).
fn gather<T>(input: Input, read: impl FnOnce() -> T) -> impl FnOnce() -> T {
    move || {
        #[cfg(test)]
        crate::work_counts::gathered(input);
        #[cfg(not(test))]
        let _ = input;
        read()
    }
}

/// A value read from files, held with their stamps as they were just before the read.
#[derive(Debug)]
pub(crate) struct Memo<T> {
    stamps: Vec<Stamp>,
    value: T,
}

impl<T> Memo<T> {
    /// What `held` holds where its stamps are `stamps`; else `read`'s answer, now held with
    /// them. `stamps` are taken before `read` runs.
    pub(crate) fn fresh(
        held: &mut Option<Self>,
        stamps: Vec<Stamp>,
        read: impl FnOnce() -> T,
    ) -> &Self {
        if held.as_ref().is_none_or(|memo| memo.stamps != stamps) {
            *held = Some(Memo {
                value: read(),
                stamps,
            });
        }
        held.as_ref().expect("a memo was just held")
    }

    /// The value read.
    pub(crate) const fn value(&self) -> &T {
        &self.value
    }

    /// The stamps it was read at.
    pub(crate) fn stamps(&self) -> &[Stamp] {
        &self.stamps
    }
}

/// The most login profiles held: a machine's sessions run on a few logins.
const PROFILES_MAX: usize = 32;

/// Each login profile as the resolver last read it, by its path ([`Self::read`]): one memo for
/// every session on that file, so two tabs on one login decode it once between them.
#[derive(Default)]
pub(crate) struct Profiles {
    held: HashMap<PathBuf, (u64, Option<Memo<ProfileRead>>)>,
    /// A count of [`Self::read`]s, which dates each path's last use for the eviction.
    reads: u64,
}

impl Profiles {
    /// What the profile file at `path` says ([`profile::read_facts`]: one read and one decode),
    /// held while the file's stamp stands. Past [`PROFILES_MAX`] paths, the one used longest ago
    /// goes.
    pub(crate) fn read(&mut self, path: &Path) -> &ProfileRead {
        self.reads += 1;
        if !self.held.contains_key(path)
            && self.held.len() >= PROFILES_MAX
            && let Some(oldest) = self
                .held
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(path, _)| path.clone())
        {
            self.held.remove(&oldest);
        }
        let (used, memo) = self.held.entry(path.to_path_buf()).or_default();
        *used = self.reads;
        Memo::fresh(
            memo,
            vec![looked(Input::Profile, path)],
            gather(Input::Profile, || profile::read_facts(path)),
        )
        .value()
    }
}

/// What a load of one store file gave: the value, or why none.
pub(crate) type Loaded<T> = Result<Arc<T>, String>;

/// One accounts store's state and login bindings as the resolver last loaded them.
#[derive(Default)]
struct Store {
    state: Option<Memo<Loaded<accounts::State>>>,
    logins: Option<Memo<Loaded<accounts::Logins>>>,
}

/// The accounts stores the resolver reads, by their harness state (a window has one): the
/// footer's own week, the roster's bindings and the accounts outlook read the one load each of
/// them holds.
#[derive(Default)]
pub(crate) struct Stores(HashMap<PathBuf, Store>);

impl Stores {
    /// The store's state under `dir` as a load at wall second `now` gave it
    /// ([`accounts::Store::load`]), held while every file that load reads keeps its stamp
    /// ([`accounts::Store::state_files`]): a held state's stamps ahead of the second it was
    /// loaded at stay capped at that second's bound until one of the files moves. A failed load
    /// is held as well, and loaded again when a file moves.
    pub(crate) fn state(&mut self, dir: &Path, now: i64) -> &Memo<Loaded<accounts::State>> {
        let store = accounts::Store::at(dir);
        let held = &mut self.0.entry(dir.to_path_buf()).or_default().state;
        Memo::fresh(
            held,
            looked_all(Input::Store, &store.state_files()),
            gather(Input::Store, || {
                #[cfg(test)]
                crate::work_counts::store_loaded();
                store.load(now).map(|(state, _)| Arc::new(state))
            }),
        )
    }

    /// The store's login bindings under `dir` as a load at wall second `now` gave them
    /// ([`accounts::Store::load_logins`]), held while every file that load reads keeps its
    /// stamp ([`accounts::Store::logins_files`]), as [`Self::state`] is.
    pub(crate) fn logins(&mut self, dir: &Path, now: i64) -> &Memo<Loaded<accounts::Logins>> {
        let store = accounts::Store::at(dir);
        let held = &mut self.0.entry(dir.to_path_buf()).or_default().logins;
        Memo::fresh(
            held,
            looked_all(Input::Logins, &store.logins_files()),
            gather(Input::Logins, || {
                store.load_logins(now).map(|(logins, _)| Arc::new(logins))
            }),
        )
    }
}

/// The accounts roster as the resolver last read it from its file, and as bound to the login
/// bindings of the harness state beside it ([`Self::at`]).
#[derive(Default)]
pub(crate) struct Roster {
    /// The roster file and the harness state the held reads are of.
    of: Option<(PathBuf, Option<PathBuf>)>,
    /// The roster file as it reads ([`accounts::Roster::load`]).
    read: Option<Memo<Result<accounts::Roster, String>>>,
    /// The bound roster, with the stamps of the roster file and of the bindings it was bound
    /// with.
    bound: Option<(Vec<Stamp>, Option<accounts::Roster>)>,
}

impl Roster {
    /// The roster at `path` as a read now would give it: an unconfigured one (no rows; an absent
    /// file is one) as it reads, never asking for `state`'s bindings; a configured one bound to
    /// the bindings under `state` ([`accounts::bound`]), or `None` where they cannot be read or
    /// no `state` is given; `None` where the file does not load. The file is read again only
    /// where its stamp moved, and bound again only where it or the bindings moved
    /// ([`Stores::logins`] at wall second `now`).
    pub(crate) fn at(
        &mut self,
        path: &Path,
        state: Option<&Path>,
        stores: &mut Stores,
        now: i64,
    ) -> Option<&accounts::Roster> {
        if self
            .of
            .as_ref()
            .is_none_or(|(p, s)| p != path || s.as_deref() != state)
        {
            *self = Roster {
                of: Some((path.to_path_buf(), state.map(Path::to_path_buf))),
                ..Roster::default()
            };
        }
        let read = Memo::fresh(
            &mut self.read,
            vec![looked(Input::Roster, path)],
            gather(Input::Roster, || {
                #[cfg(test)]
                crate::work_counts::roster_loaded();
                accounts::Roster::load(path)
            }),
        );
        let roster = read.value().as_ref().ok()?;
        if roster.rows.is_empty() {
            return Some(roster);
        }
        let logins = stores.logins(state?, now);
        let key: Vec<Stamp> = read
            .stamps()
            .iter()
            .chain(logins.stamps())
            .copied()
            .collect();
        if self.bound.as_ref().is_none_or(|(held, _)| *held != key) {
            let bound = logins
                .value()
                .as_ref()
                .ok()
                .map(|logins| accounts::bound(roster, logins));
            self.bound = Some((key, bound));
        }
        self.bound.as_ref().and_then(|(_, bound)| bound.as_ref())
    }
}

#[cfg(test)]
#[path = "footer_inputs_tests.rs"]
mod tests;
