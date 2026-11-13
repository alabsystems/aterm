// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FLEET TABLES: machines, AI accounts and agents as three typed, read-only relations,
//! folded from what this Mac already reads. Nothing here is stored and nothing is asked of a
//! machine or a provider: the registry's LabView and the journal window (read every five
//! minutes by the harness's journal cache), the Commons network observer, and the fleet read
//! the glance already makes are each offered to one process-wide [`Folder`], and one pure
//! [`fold::fold`] joins them.
//!
//! - [`rows`]: the relations' schemas (names, types and words: what `aterm db fleet` prints by
//!   and a sheet reads) and their row types.
//! - [`read`]: the offered records. [`read::At`] is a value with its read time and has no
//!   `Default`, so an unread value is never a zero.
//! - [`fold`]: the join, its inputs and each input's state (`live`, `held`, `never`, `off`).
//! - [`command`]: `aterm db fleet machines|accounts|agents|schema`.
//! - [`fixture`]: a whole fleet at the lab's real scale, for a lab instance and the tests.
//!
//! A reader takes the tables through [`Folder::try_tables`], a `try_lock` on an `Arc`: a
//! painting thread never waits on a fold, and every custody wait stays on the thread that
//! already makes it. Nothing is folded until a reader first asks: an offer before that swaps a
//! pointer and no more, and a process nothing reads the tables in (a command, a window with no
//! surface of them) starts no thread. From the first ask the fold runs on its own thread, only
//! when an offer changed an input or a row is due to change by itself
//! ([`fold::Folded::next_change_ms`]).

pub mod command;
pub mod fixture;
pub mod fold;
pub mod read;
pub mod rows;

use crate::alab::{journal::seen::Seen, view::LabView};
use crate::publish::{Current, Published};
use aterm_types::network_state::{HistoryState, NetworkStateSnapshot, Visibility};
use fold::{FleetTables, Inputs, Source};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, TryLockError};
use std::time::Duration;

/// Why an input is held: its reader's storage or custody could not be read.
pub const HELD_UNREADABLE: &str = "unreadable";
/// Why the registry is held: this Mac's history of it still waits on a record.
pub const HELD_PARTIAL: &str = "partial";
/// Why an input is held: this Mac's local policy for it could not be read.
pub const HELD_POLICY: &str = "policy-unavailable";
/// Why the fleet reads are held: the last read failed.
pub const HELD_READ_FAILED: &str = "read-failed";
/// Why the fleet reads are held: this Mac's own windows answered and its question of the whole
/// fleet was refused (this Mac is away, or its roster is unread). This Mac's row is of that
/// read; every peer's is of the read before it.
pub const HELD_FLEET_UNREAD: &str = "fleet-unread";
/// Why the fleet reads are held: the last one is older than [`fold::REACH_KEEP_MS`], so it no
/// longer says which machines answer. The fold gives this word; no reader offers it.
pub const HELD_STALE: &str = "stale";
/// Why the network observation is held: no observer is observing and no retained history is
/// here to read.
pub const HELD_NO_HISTORY: &str = "no-history";
/// Why the network observation is held: no observer is observing now (none has started, or its
/// last sample expired), so the rows show its retained history and no machine is `up` by it.
pub const HELD_UNOBSERVED: &str = "unobserved";

/// The network observation `held` becomes on taking `snapshot`, and whether that changes what
/// the fold reads. An observation no observer is making now is held, never live: with retained
/// history as [`HELD_UNOBSERVED`], dated by that history's last sample; with none as
/// [`HELD_NO_HISTORY`], keeping what was observed before. The same facts in the same state again
/// change nothing.
pub fn observe(
    held: &mut Source<Arc<NetworkStateSnapshot>>,
    snapshot: &Arc<NetworkStateSnapshot>,
) -> bool {
    let network = &snapshot.network;
    let why = if network.visibility != Visibility::Unavailable {
        None
    } else if network.history == HistoryState::Unavailable {
        Some(HELD_NO_HISTORY)
    } else {
        Some(HELD_UNOBSERVED)
    };
    let same = match (&mut *held, why) {
        (Source::Live { value, .. }, None) => Some(value),
        (
            Source::Held {
                last: Some((value, _)),
                why: HELD_UNOBSERVED,
            },
            Some(HELD_UNOBSERVED),
        ) => Some(value),
        _ => None,
    };
    if let Some(value) = same
        && (Arc::ptr_eq(value, snapshot) || value.network == *network)
    {
        *value = Arc::clone(snapshot);
        return false;
    }
    *held = match why {
        None => Source::Live {
            value: Arc::clone(snapshot),
            at_ms: network.observed_at_ms,
        },
        Some(HELD_NO_HISTORY) => {
            if held.state().why() == Some(HELD_NO_HISTORY) {
                return false;
            }
            held.held(HELD_NO_HISTORY)
        }
        Some(why) => Source::Held {
            last: Some((Arc::clone(snapshot), network.observed_at_ms)),
            why,
        },
    };
    true
}

/// What a reader made of one input on one read ([`Folder::offer_lab`]).
#[derive(Clone, Debug)]
pub enum Reading<T> {
    /// It read it.
    Read(T),
    /// It is turned off on this Mac, or this Mac holds none: its rows go.
    Off,
    /// It could not read it, for this `HELD_` word: what it read before stays in the rows, and
    /// the tables say it is held.
    Held(&'static str),
}

impl<T: Clone> Reading<T> {
    /// The source `held` becomes after this reading, made at `read_at_ms`.
    fn onto(self, held: &Source<T>, read_at_ms: u64) -> Source<T> {
        match self {
            Self::Read(value) => Source::Live {
                value,
                at_ms: read_at_ms,
            },
            Self::Off => Source::Off,
            Self::Held(why) => held.held(why),
        }
    }
}

/// What starts a thread the first ask for the tables calls for: the reader of the registry and
/// the journal ([`Folder::install_lab_reader`]), and the fold thread ([`folder`]).
type Starter = Box<dyn Fn() + Send + Sync>;

/// The fleet tables' inputs and their published fold.
pub struct Folder {
    inputs: Mutex<Inputs>,
    /// A fixture holds the inputs: no reader's offer replaces them.
    fixed: AtomicBool,
    tables: Published<FleetTables>,
    /// An input changed since the last fold.
    dirty: Mutex<bool>,
    wake: Condvar,
    /// Some reader has asked for the tables.
    asked: AtomicBool,
    lab_reader: OnceLock<Starter>,
    /// What starts this folder's fold thread, on the first ask ([`folder`] sets it; a folder
    /// without one is folded by its holder, [`Self::fold_now`]).
    fold_thread: OnceLock<Starter>,
}

impl Default for Folder {
    fn default() -> Self {
        Self::new()
    }
}

impl Folder {
    /// A folder nothing was offered to: its tables hold no rows and say every input is `never`.
    pub fn new() -> Self {
        Self {
            inputs: Mutex::new(Inputs::default()),
            fixed: AtomicBool::new(false),
            tables: Published::new(FleetTables::unread()),
            dirty: Mutex::new(false),
            wake: Condvar::new(),
            asked: AtomicBool::new(false),
            lab_reader: OnceLock::new(),
            fold_thread: OnceLock::new(),
        }
    }

    /// Change the inputs under their lock, which is held for pointer swaps alone, and tell the
    /// fold thread where `change` says they moved. A fixture's inputs stand.
    fn offer(&self, change: impl FnOnce(&mut Inputs) -> bool) {
        if self.fixed.load(Ordering::Acquire) {
            return;
        }
        let changed = change(&mut self.inputs.lock().unwrap_or_else(PoisonError::into_inner));
        if changed {
            self.mark();
        }
    }

    fn mark(&self) {
        *self.dirty.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_one();
    }

    /// One read of the registry and the journal window, made at `read_at_ms` (Unix milliseconds)
    /// by the one reader that holds them together: what it made of each. Both change at once,
    /// so no fold sees a new registry beside the journal of the read before.
    pub fn offer_lab(
        &self,
        lab: Reading<Arc<LabView>>,
        journal: Reading<Arc<Seen>>,
        read_at_ms: u64,
    ) {
        self.offer(|inputs| {
            inputs.lab = lab.onto(&inputs.lab, read_at_ms);
            inputs.journal = journal.onto(&inputs.journal, read_at_ms);
            true
        });
    }

    /// The network observer's facts as the window's snapshot holds them, observed at
    /// `snapshot.network.observed_at_ms` ([`observe`] says what state they are in: one no
    /// observer is making now is held). The same snapshot again, or one whose network facts are
    /// the ones held, changes nothing. A painting path calls this: it never waits, and an offer
    /// that finds the inputs in use is dropped, for the next look to make again.
    pub fn offer_network(&self, snapshot: &Arc<NetworkStateSnapshot>) {
        if self.fixed.load(Ordering::Acquire) {
            return;
        }
        let mut inputs = match self.inputs.try_lock() {
            Ok(inputs) => inputs,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let changed = observe(&mut inputs.network, snapshot);
        drop(inputs);
        if changed {
            self.mark();
        }
    }

    /// One fleet read, joined to the reads before it ([`read::Latest::with`]). One that asked
    /// the fleet is the live reading; one whose question of the fleet was refused
    /// ([`read::Read::fleet`]) gives this Mac's row its own answer and holds the rest as
    /// [`HELD_FLEET_UNREAD`], dated by the last read that did ask. The join is made before the
    /// inputs are locked; one reader offers reads, so none is lost between.
    pub fn offer_read(&self, read: read::Read) {
        if self.fixed.load(Ordering::Acquire) {
            return;
        }
        let (at_ms, fleet) = (read.read_at_ms, read.fleet);
        let held = self
            .inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .read
            .value()
            .cloned();
        let latest = Arc::new(match held {
            Some(held) => held.with(read),
            None => read::Latest::default().with(read),
        });
        self.offer(|inputs| {
            inputs.read = if fleet {
                Source::Live {
                    value: latest,
                    at_ms,
                }
            } else {
                // With no read of the fleet before it, this Mac's row is all there is to date.
                let of = match latest.fleet_at_ms {
                    0 => at_ms,
                    asked => asked,
                };
                Source::Held {
                    last: Some((latest, of)),
                    why: HELD_FLEET_UNREAD,
                }
            };
            true
        });
    }

    /// A fleet read failed, for `why` (a `HELD_` word): the reads before it stay.
    pub fn hold_read(&self, why: &'static str) {
        self.offer(|inputs| {
            inputs.read = inputs.read.held(why);
            true
        });
    }

    /// Hold `inputs` in place of every reader's: a fixture's whole fleet. No later offer
    /// replaces them.
    pub fn offer_fixture(&self, inputs: Inputs) {
        *self.inputs.lock().unwrap_or_else(PoisonError::into_inner) = inputs;
        self.fixed.store(true, Ordering::Release);
        self.mark();
    }

    /// Install what starts the reader of the registry and the journal. That reader runs only
    /// while something reads what it reads, so it starts on the first ask for the tables
    /// ([`Self::try_tables`]), or here where one was already made, and never under a fixture,
    /// which no reader's offer replaces. The first install wins.
    pub fn install_lab_reader(&self, start: Starter) -> bool {
        let installed = self.lab_reader.set(start).is_ok();
        if installed
            && self.asked.load(Ordering::Acquire)
            && !self.fixed.load(Ordering::Acquire)
            && let Some(start) = self.lab_reader.get()
        {
            start();
        }
        installed
    }

    /// The tables and their revision, without waiting: `None` while a fold is being swapped in,
    /// which is no new value, so the caller keeps the `Arc` it has. The first ask starts the
    /// registry and journal reader where one is installed, and this folder's fold thread where
    /// it has one: until then the tables are the ones nothing was folded into
    /// ([`FleetTables::unread`]), and what was offered meanwhile is folded at once.
    pub fn try_tables(&self) -> Option<Current<FleetTables>> {
        if !self.asked.swap(true, Ordering::AcqRel) {
            if !self.fixed.load(Ordering::Acquire)
                && let Some(start) = self.lab_reader.get()
            {
                start();
            }
            if let Some(start) = self.fold_thread.get() {
                start();
            }
        }
        self.tables.try_current()
    }

    /// The tables and their revision, waiting out a swap: a worker's, a command's or a test's
    /// read, never a painting path's.
    pub fn tables(&self) -> Current<FleetTables> {
        self.tables.current()
    }

    /// Fold what is held as of `now_ms` and publish it; the revision moves only where the
    /// tables differ. Returns the next instant the rows change by themselves.
    pub fn fold_now(&self, now_ms: u64) -> Option<u64> {
        *self.dirty.lock().unwrap_or_else(PoisonError::into_inner) = false;
        let inputs = self
            .inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let folded = fold::fold(&inputs, now_ms);
        self.tables.publish(folded.tables);
        folded.next_change_ms
    }

    /// One turn of the fold thread: wait until an offer changed an input or the instant `due`
    /// (what the last fold named) has come by `now_ms`, then fold and publish. Returns the next
    /// such instant. Nothing wakes it on a timer.
    fn turn(&self, due: Option<u64>, now_ms: &dyn Fn() -> u64) -> Option<u64> {
        {
            let mut dirty = self.dirty.lock().unwrap_or_else(PoisonError::into_inner);
            while !*dirty {
                match due {
                    None => {
                        dirty = self
                            .wake
                            .wait(dirty)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    Some(at) => {
                        let now = now_ms();
                        if now >= at {
                            break;
                        }
                        dirty = self
                            .wake
                            .wait_timeout(dirty, Duration::from_millis(at - now))
                            .unwrap_or_else(PoisonError::into_inner)
                            .0;
                    }
                }
            }
        }
        self.fold_now(now_ms())
    }

    /// The fold thread: [`Self::turn`] after turn.
    fn run(&self, now_ms: &dyn Fn() -> u64) -> ! {
        let mut due: Option<u64> = None;
        loop {
            due = self.turn(due, now_ms);
        }
    }
}

/// This process's folder, once [`folder`] made it.
static FOLDER: OnceLock<Folder> = OnceLock::new();

/// Start this process's fold thread (`aterm-fleet-fold`): the folder's first ask calls it, once.
fn start_fold_thread() {
    let Some(folder) = FOLDER.get() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("aterm-fleet-fold".into())
        .spawn(move || folder.run(&|| aterm_time::unix_millis().unwrap_or(0)));
}

/// This process's folder. Making it starts nothing: its fold thread starts on the first ask for
/// the tables ([`Folder::try_tables`]), so a process that only offers, or that never reads
/// them, folds nothing. Where the dev seam asks ([`fixture::under_seam`]) it holds the
/// fixture's lab from its making, for every reader and against every offer.
pub fn folder() -> &'static Folder {
    FOLDER.get_or_init(|| {
        let made = Folder::new();
        let _ = made.fold_thread.set(Box::new(start_fold_thread));
        if let Some(inputs) = fixture::under_seam(aterm_time::unix_millis().unwrap_or(0)) {
            made.offer_fixture(inputs);
        }
        made
    })
}

#[cfg(test)]
mod tests;
