// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO VOUCHES FOR CLAUDE CODE'S READING OF THE ROWS ON THEIR VAULT TOKEN HERE
//! ([`super::TOKENS_HERE_FILE`]): the window whose upkeep keeps it ([`TokensHereHolder`]).
//!
//! The upkeep writes the reading only where the rows it names or their ends change
//! ([`super::Store::note_tokens_here`]), so a pass that reads the vault as before writes nothing,
//! and the reading's `read_at` says when it last changed, not when it was last read. While the
//! window's holder vouches for it, a reader judges no age of it ([`super::on_token_here`]): the
//! holder vouches from a turn whose passes read the vault and noted the reading, by holding the
//! exclusive `flock` of [`TOKENS_HERE_LOCK_FILE`] beside it. In a process with a holder of the
//! store the holder's own word decides; in any other a shared take of that lock that fails says a
//! holder stands ([`vouched`]), and two such looks never take each other for one.
//!
//! A turn whose passes noted no read of the vault (the vault could not be read, the accounts are
//! off, no roster stands) stands the vouch down: it writes the reading again with `read_at = now`
//! and then lets the lock go, so every reader ages it out [`super::TOKENS_HERE_FRESH_S`] later, as
//! a reading nobody reads again always aged out. A window that quits does the same at once, on the
//! thread that stops it ([`TokensHereHolder::quit`]), whether or not the thread that turns the
//! holder ends before the process does, and the last holder of a store does it as it drops, so a
//! reader after the window quits finds the hour it always had. A window killed before either
//! leaves the reading as it last changed, which a reader then ages out sooner, never later.
//!
//! Traced by the accounts keeper: what this file reads of the file system itself, it reads
//! through `aterm_uds::trace::fs` (census `traced-reads`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use aterm_uds::trace;

use super::{Cli, Lock, Store};

/// The lock file beside [`super::TOKENS_HERE_FILE`] whose exclusive `flock` a window's holder
/// takes while it vouches for the reading ([`TokensHereHolder`]).
pub const TOKENS_HERE_LOCK_FILE: &str = "tokens_here.lock";

/// What a pass noted of the reading for the holder's next turn ([`noted`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Noted {
    /// The vault was read, and the reading is the one it gave.
    Confirmed,
    /// The vault could not be read; the reading before it was kept.
    Unknown,
}

/// A store this process holds ([`TokensHereHolder`]).
#[derive(Debug)]
struct Hold {
    /// How many holders of it this process has.
    holders: usize,
    /// The lock of [`TOKENS_HERE_LOCK_FILE`], while they vouch.
    vouch: Option<Lock>,
    /// The last thing a pass noted since the last turn.
    noted: Option<Noted>,
}

/// The stores this process holds, by their directory.
static HOLDS: Mutex<BTreeMap<PathBuf, Hold>> = Mutex::new(BTreeMap::new());

/// Moves whenever which stores this process holds, or vouches for, changes: a run that read a
/// reading's vouch ([`vouched`]) records it, so a vouch taken or stood down after moves its trace.
static VOUCHING: LazyLock<trace::Generation> =
    LazyLock::new(|| trace::Generation::new("accounts/tokens_here_vouch"));

fn holds() -> MutexGuard<'static, BTreeMap<PathBuf, Hold>> {
    HOLDS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Note `noted` for the holder of the store at `dir`, where this process has one: its next turn
/// judges by the last thing noted ([`TokensHereHolder::turn`]).
pub(super) fn noted(dir: &Path, noted: Noted) {
    if let Some(hold) = holds().get_mut(dir) {
        hold.noted = Some(noted);
    }
}

/// Whether a window's holder vouches now for the reading kept in the store at `dir`: this
/// process's own holder of it, where it has one, and otherwise any other process's, which holds
/// [`TOKENS_HERE_LOCK_FILE`]'s exclusive lock ([`elsewhere`]). The generation of this process's
/// holds is recorded in the armed trace scope.
#[cfg(unix)]
pub(super) fn vouched(dir: &Path) -> bool {
    VOUCHING.record();
    let here = holds().get(dir).map(|hold| hold.vouch.is_some());
    here.unwrap_or_else(|| elsewhere(dir))
}

/// Whether another process's holder vouches for the reading kept at `dir`: a shared take of
/// [`TOKENS_HERE_LOCK_FILE`] fails now, as it does only while a holder holds it exclusively. It
/// poisons the armed trace scope ([`trace::poison`]): another process's lock moves no stamp.
#[cfg(unix)]
fn elsewhere(dir: &Path) -> bool {
    trace::poison("another process's vouch for tokens_here.json, which no stamp decides");
    match super::open_present_file(&dir.join(TOKENS_HERE_LOCK_FILE), "lock") {
        Ok(file) => Probe::held_elsewhere(file),
        Err(_) => false,
    }
}

/// A shared `flock(2)` of [`TOKENS_HERE_LOCK_FILE`] taken to see whether a holder has it, which
/// unlocks on drop.
#[cfg(unix)]
struct Probe(std::fs::File);

#[cfg(unix)]
impl Probe {
    /// Whether a shared take of `file` fails now because the lock is held exclusively; the take,
    /// where it is made, is let go at once.
    fn held_elsewhere(file: std::fs::File) -> bool {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock_shared();
        match taken {
            Ok(()) => {
                drop(Self(file));
                false
            }
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(_)) => false,
        }
    }
}

#[cfg(unix)]
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A WINDOW'S HOLD ON CLAUDE CODE'S READING OF THE ROWS ON THEIR VAULT TOKEN HERE, made by the
/// thread that runs the window's accounts' upkeep, which turns it after each upkeep
/// ([`Self::turn`]). A turn whose passes noted the vault read ([`super::Store::note_tokens_here`])
/// vouches for the reading; a turn whose passes did not, stands the vouch down (the module's doc
/// says how). A pass that keeps the reading without noting it leaves the turn unread. A holder
/// that quits ([`Self::quit`]) stands its vouch down at once and vouches no more; the last holder
/// of a store stands its vouch down as it drops.
#[derive(Debug)]
pub struct TokensHereHolder {
    /// Claude Code's store.
    store: Store,
    /// The window quit ([`Self::quit`]): no turn vouches again.
    quit: AtomicBool,
}

impl TokensHereHolder {
    /// The holder of `store`'s reading: Claude Code's store, on a host where a launch starts on a
    /// vault token ([`crate::harness::token_route::LAUNCHES_ON_TOKEN`]) and so a reading is kept;
    /// `None` elsewhere. It vouches for nothing until a turn has noted the vault read.
    #[must_use]
    pub fn new(store: &Store) -> Option<Self> {
        if !crate::harness::token_route::LAUNCHES_ON_TOKEN {
            return None;
        }
        Self::of(store)
    }

    /// [`Self::new`] on any Unix host: `None` for another CLI's store, and off Unix, where no
    /// reading is kept.
    #[must_use]
    pub(crate) fn of(store: &Store) -> Option<Self> {
        if !cfg!(unix) || store.cli != Cli::Claude {
            return None;
        }
        holds()
            .entry(store.dir.clone())
            .or_insert(Hold {
                holders: 0,
                vouch: None,
                noted: None,
            })
            .holders += 1;
        VOUCHING.bump();
        Some(Self {
            store: store.clone(),
            quit: AtomicBool::new(false),
        })
    }

    /// After an upkeep, at `now`: vouch for the reading where the passes since the last turn
    /// last noted the vault read, and otherwise stand the vouch down, writing the reading again
    /// at `now` before the lock goes. A vouch whose lock cannot be taken now (a store with no
    /// directory, a look from another process holding it) is not taken, and the next turn tries
    /// again; till then the upkeep writes the reading at each read as it would with no holder.
    /// After [`Self::quit`] every turn stands down.
    pub fn turn(&self, now: i64) {
        let noted = holds()
            .get_mut(&self.store.dir)
            .and_then(|hold| hold.noted.take());
        if noted == Some(Noted::Confirmed) && !self.quit.load(Ordering::SeqCst) {
            self.vouch();
        } else {
            self.stand_down(now);
        }
    }

    /// The window quits at `now`: stand the vouch down at once, on the calling thread, writing
    /// the reading again at `now` before the lock goes, and vouch no more, whatever a turn made
    /// after it noted. The thread that turns the holder may still be in a pass, and the process
    /// may end before that pass does: the reading then still ages from `now`, as one a window that
    /// quit always did.
    pub fn quit(&self, now: i64) {
        self.quit.store(true, Ordering::SeqCst);
        self.stand_down(now);
    }

    /// Whether this process's holders of the store vouch for its reading now.
    #[must_use]
    pub fn vouching(&self) -> bool {
        holds()
            .get(&self.store.dir)
            .is_some_and(|hold| hold.vouch.is_some())
    }

    fn vouch(&self) {
        if self.vouching() {
            return;
        }
        let Ok(file) =
            super::open_retained_file(&self.store.dir.join(TOKENS_HERE_LOCK_FILE), "lock")
        else {
            return;
        };
        let Ok(Ok(lock)) = Store::try_take(file) else {
            return;
        };
        {
            let mut holds = holds();
            // A quit made since the turn looked is seen here, under the holds' lock, and the lock
            // just taken is let go; a quit made after this finds the vouch held and stands it
            // down itself.
            if self.quit.load(Ordering::SeqCst) {
                return;
            }
            if let Some(hold) = holds.get_mut(&self.store.dir) {
                hold.vouch = Some(lock);
            }
        }
        VOUCHING.bump();
    }

    fn stand_down(&self, now: i64) {
        if !self.vouching() {
            return;
        }
        let _ = self.store.stamp_tokens_here(now);
        let lock = holds()
            .get_mut(&self.store.dir)
            .and_then(|hold| hold.vouch.take());
        drop(lock);
        VOUCHING.bump();
    }
}

impl Drop for TokensHereHolder {
    fn drop(&mut self) {
        let last = holds().get_mut(&self.store.dir).is_some_and(|hold| {
            hold.holders = hold.holders.saturating_sub(1);
            hold.holders == 0
        });
        if !last {
            return;
        }
        if let Some(now) = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()) {
            self.stand_down(now);
        }
        // A holder made since the count reached zero keeps the store held.
        let gone = {
            let mut holds = holds();
            if holds
                .get(&self.store.dir)
                .is_some_and(|hold| hold.holders == 0)
            {
                holds.remove(&self.store.dir)
            } else {
                None
            }
        };
        drop(gone);
        VOUCHING.bump();
    }
}

#[cfg(all(test, unix))]
#[path = "accounts_tokens_here_tests.rs"]
mod tests;
