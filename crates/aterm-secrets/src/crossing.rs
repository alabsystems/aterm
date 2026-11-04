// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crossed mark: the one-way line a membership log passes once it holds more records than a
//! 0.146-0.152 build takes ([`LEGACY_RECORDS`]), and the records bound every build since states
//! on the records wire ([`RecordsBound`], [`crate::transport::RECORDS_BOUND_HEADER`]).
//!
//! A build of 0.146-0.152 refuses a whole records stream past [`LEGACY_RECORDS`], and one whose
//! store is near that bound takes only the part of a stream that fits it: past a rotation's
//! `Remove` and `Epoch`, the first of its `Wrap`s. A 0.148-0.152 member that holds such a cut
//! batch reads every member whose `Wrap` lies past the cut as left out of the newest
//! `[everyone]` key on purpose: itself as revoked, the others as out of the fleet. So no build
//! may hand an old one any record of a log that ever held more than [`LEGACY_RECORDS`], whole or
//! in part, and none may take an old one's word for the newest records once its own log can hold
//! records that old one cannot.
//!
//! A node (a Mac's own records, or a vault server's records for one vault) is **crossed** once
//! its log holds more than [`LEGACY_RECORDS`] records ([`past_legacy`]) or once it stored its
//! [`Mark`]. Over the records wire (a records GET, POST or peer pull) it stores the mark before
//! it holds a record a crossed source (one whose [`RecordsBound`] says crossed) taught it, and
//! before the write that would hold more than [`LEGACY_RECORDS`] records: it ingests what came
//! into a copy no reader sees, and where that copy holds more complete records than the node
//! did, stores the mark before the copy is served or written. A node that has not crossed takes
//! none of a crossed source's batch that brings it only records waiting on a parent: a record
//! waits for any key that ever held a seat, a removed Mac's among them, with a parent that may
//! never come, and such a record would cross the node for good though none of it is ever served.
//! So no record a crossed log taught it over HTTP, and no log past the bound, is ever held
//! without the mark beside it, and a crossed source's answer the node takes none of (a batch it
//! refuses, records it holds, records that only wait on a parent) leaves no mark. Nothing
//! removes the mark: a log read back short (a file cut at a stream bound, a torn write) stays
//! crossed.
//!
//! Legacy Commons control repair (`client::retain_records_for_commons`) carries no source bound.
//! A locally crossed node refuses those database protocols before its hello, and again before
//! their membership reads and writes. The frontier database protocol states the source bound
//! before its evidence and record frames; its bounded receiver inherits the mark before newly
//! completed records from a crossed source become durable. A version-three join bundle states
//! the source bound and is checked against the joiner's actual capacity before retention. It
//! keeps the whole offered history and any previous history of the same root, and stores the
//! mark before publishing the configuration, records or values of a crossed source. Legacy
//! log bundles remain available only for an uncrossed source and an uncrossed recipient.
//!
//! A crossed node neither serves nor accepts a source that states no bound: a records GET, POST
//! or peer pull without [`crate::transport::RECORDS_BOUND_HEADER`] is answered `409` with no
//! record, which every old build takes for a source that failed. It takes nothing from an answer
//! that states no bound, and posts with [`crate::transport::CROSSED_POST_QUERY`] (which every
//! old server refuses before it reads a body). A node that has not crossed serves and takes old
//! builds' records as before: it holds at most [`LEGACY_RECORDS`], and none of them came from a
//! crossed log, so whatever an old reader takes of its answer it takes whole.
//!
//! Stating a bound alone is not enough: both capacities must represent every history the other
//! may serve ([`RecordsBound::can_exchange`]). An uncrossed node may serve at most the legacy
//! bound; a crossed node may need its full advertised capacity even after its file shrinks.
//! An incompatible request gets `409` before its body is read or any records are served, and an
//! incompatible answer is refused before its records are ingested. Crossed posts use the second
//! fence query, which the first, header-only fence also rejects before reading the body.
//!
//! Every node of this build, crossed or not, counts a records pass as fresh (it read the vault's
//! newest records) only where a source compatible with the final retained history answered.
//! The merge itself can cross the local log, so this judgment is made under the writer lock
//! after ingestion. An old server cannot take a crossed log, and a smaller server cannot take
//! a larger crossed log; either can hold none of what came since. A node whose own log has not
//! crossed cannot tell a vault that has not crossed from an old server frozen past the crossing.

use crate::membership_log::{LEGACY_RECORDS, RecordBudgets};

/// What a node states of its records ([`crate::transport::RECORDS_BOUND_HEADER`]): the most
/// records it holds, and whether its log has crossed ([`past_legacy`], [`Mark`]).
///
/// On the wire it is `1 <held> crossed` or `1 <held> uncrossed`: the version `1`, the held
/// bound in decimal with no leading zero, and the state, each one space apart and nothing else.
/// A value that is not exactly that is no bound at all ([`RecordsBound::parse`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordsBound {
    /// The most records the sender holds in all ([`RecordBudgets::held`]).
    pub held: u32,
    /// Whether the sender's log has crossed.
    pub crossed: bool,
}

/// The version of [`RecordsBound`]'s wire form.
const VERSION: &str = "1";
/// [`RecordsBound::crossed`] on the wire.
const CROSSED: &str = "crossed";
/// Not [`RecordsBound::crossed`], on the wire.
const UNCROSSED: &str = "uncrossed";

impl RecordsBound {
    /// Whether both nodes can represent every history the other may serve.
    /// An uncrossed history is at most the legacy bound; a crossed history
    /// requires the sender's advertised capacity even if its file is now short.
    /// This is a compatibility check, not authorization or evidence of freshness.
    #[must_use]
    pub const fn can_exchange(self, peer: Self) -> bool {
        let legacy = LEGACY_RECORDS as u32;
        let own_history = if self.crossed || self.held < legacy {
            self.held
        } else {
            legacy
        };
        let peer_history = if peer.crossed || peer.held < legacy {
            peer.held
        } else {
            legacy
        };
        self.held != 0 && peer.held != 0 && self.held >= peer_history && peer.held >= own_history
    }

    /// A node holding at most `held` records, `crossed` or not. A bound past `u32::MAX` is stated
    /// as `u32::MAX`.
    #[must_use]
    pub fn of(held: usize, crossed: bool) -> Self {
        Self {
            held: u32::try_from(held).unwrap_or(u32::MAX),
            crossed,
        }
    }

    /// A node of this build holding its records within [`RecordBudgets::current`], `crossed` or
    /// not.
    #[must_use]
    pub fn here(crossed: bool) -> Self {
        Self::of(RecordBudgets::current().held, crossed)
    }

    /// What a caller that keeps no membership log of its own states (a test, a lab reading a
    /// server): [`RecordsBound::here`], not crossed.
    #[must_use]
    pub fn uncrossed() -> Self {
        Self::here(false)
    }

    /// The header's value.
    #[must_use]
    pub fn value(self) -> String {
        let state = if self.crossed { CROSSED } else { UNCROSSED };
        format!("{VERSION} {} {state}", self.held)
    }

    /// The bound `value` states, read strictly: `None` for anything but exactly
    /// `1 <held> crossed` or `1 <held> uncrossed`, `<held>` a positive decimal with no leading
    /// zero that fits a `u32`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let mut words = value.split(' ');
        let (Some(VERSION), Some(held), Some(state), None) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            return None;
        };
        if held.is_empty()
            || held.len() > 10
            || held.starts_with('0')
            || !held.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let held = held.parse::<u32>().ok()?;
        let crossed = match state {
            CROSSED => true,
            UNCROSSED => false,
            _ => return None,
        };
        Some(Self { held, crossed })
    }
}

/// Whether a log holding `held` records has crossed by its count alone: it holds more than a
/// 0.146-0.152 build takes.
#[must_use]
pub const fn past_legacy(held: usize) -> bool {
    held > LEGACY_RECORDS
}

#[cfg(unix)]
pub(crate) use mark::Mark;

#[cfg(unix)]
mod mark {
    use crate::Error;
    #[cfg(test)]
    use std::path::Path;
    use std::path::PathBuf;

    /// What a stored mark holds: nothing reads it back (any file there is the mark), it says what
    /// the file is to a person who finds it.
    const MARK: &[u8] = b"aterm-secrets records crossed 1\n";

    /// A node's crossed mark: a small file beside its records, stored atomically and made durable
    /// ([`crate::fsx::write_atomic`]) before the node takes what crosses it, and never removed.
    #[derive(Clone, Debug)]
    pub(crate) struct Mark {
        path: PathBuf,
    }

    impl Mark {
        /// The mark at `path`.
        pub(crate) const fn at(path: PathBuf) -> Self {
            Self { path }
        }

        /// Where it is: what a test's faults are keyed by ([`inject`]).
        #[cfg(test)]
        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        /// Whether it is stored: anything at its path is the mark, whatever it holds, so one cut
        /// short or written by another build still counts.
        ///
        /// # Errors
        /// The path cannot be read (`lstat` fails but for its absence).
        pub(crate) fn stored(&self) -> crate::Result<bool> {
            #[cfg(test)]
            if faults(&self.path).fail_look {
                return Err(Error::new(format!(
                    "{}: a test fails the look for the crossed mark",
                    self.path.display()
                )));
            }
            Ok(crate::fsx::kind(&self.path)? != crate::fsx::Kind::Missing)
        }

        /// Store it, durably, unless it is stored already.
        ///
        /// # Errors
        /// The path cannot be read, or the write or its flush fails: the mark may not be stored,
        /// and the caller takes nothing that needed it.
        pub(crate) fn store(&self) -> crate::Result<()> {
            #[cfg(test)]
            if faults(&self.path).fail_store {
                return Err(Error::new(format!(
                    "{}: a test refuses the crossed mark",
                    self.path.display()
                )));
            }
            if self.stored()? {
                return Ok(());
            }
            crate::fsx::write_atomic(&self.path, MARK, 0o600)
                .map_err(|e| Error::new(format!("storing the crossed mark: {e}")))
        }
    }

    /// What a test does to the node whose mark is at a path, as [`inject`] set it.
    #[cfg(test)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct Faults {
        /// [`Mark::stored`] fails, as an `lstat` that fails but for the mark's absence.
        pub(crate) fail_look: bool,
        /// [`Mark::store`] fails.
        pub(crate) fail_store: bool,
        /// The node dies between storing the mark and taking the records it stored it for: what
        /// it does after the first of the two is undone, as by a process that ends there.
        pub(crate) dies_between: bool,
        /// MUTANT: the node takes the records first and stores the mark after, so
        /// [`Faults::dies_between`] leaves them held with no mark.
        pub(crate) mark_after_take: bool,
        /// MUTANT: the node stores the mark once a crossed source's records arrive, before it
        /// knows it takes any of them, so a batch it refuses, or one of records it holds,
        /// crosses it.
        pub(crate) mark_on_receipt: bool,
        /// MUTANT: the node takes a crossed source's batch that brings it only records waiting
        /// on a parent, storing the mark first, so one record any seat ever held (a removed
        /// Mac's) signs, naming a parent that never comes, crosses it.
        pub(crate) mark_on_waiting: bool,
        /// MUTANT: the node, crossed, serves a records GET that states no bound the first
        /// [`LEGACY_RECORDS`](crate::membership_log::LEGACY_RECORDS) records it would serve (the
        /// raised-bound design's capped reply) rather than the fence's `409`, so a build before
        /// the fence takes a crossed log's prefix whole.
        pub(crate) capped_prefix: bool,
    }

    #[cfg(test)]
    static FAULTS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, Faults>> =
        std::sync::Mutex::new(std::collections::BTreeMap::new());

    /// From now on the node whose mark is at `path` suffers `faults` (by path, so a server's
    /// connection threads suffer them too).
    #[cfg(test)]
    pub(crate) fn inject(path: &Path, faults: Faults) {
        FAULTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.to_path_buf(), faults);
    }

    /// What [`inject`] set for the mark at `path`.
    #[cfg(test)]
    pub(crate) fn faults(path: &Path) -> Faults {
        FAULTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(path)
            .copied()
            .unwrap_or_default()
    }
}

#[cfg(all(unix, test))]
pub(crate) use mark::{Faults, faults, inject};

/// Whether the node whose log `store` holds and whose mark is `mark` has crossed.
///
/// # Errors
/// The mark's path cannot be read ([`Mark::stored`]).
#[cfg(unix)]
pub(crate) fn crossed(mark: &Mark, store: &crate::membership_log::Store) -> crate::Result<bool> {
    let held = store
        .log()
        .map_or(0, crate::membership_log::Log::held_count);
    Ok(past_legacy(held) || mark.stored()?)
}

/// What the node whose log `store` holds and whose mark is `mark` states of its records.
///
/// # Errors
/// As [`crossed`].
#[cfg(unix)]
pub(crate) fn stated(
    mark: &Mark,
    store: &crate::membership_log::Store,
) -> crate::Result<RecordsBound> {
    Ok(RecordsBound::of(
        store.budgets().held,
        crossed(mark, store)?,
    ))
}

#[cfg(all(unix, test))]
#[path = "crossing/fixture.rs"]
pub(crate) mod fixture;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_capacities_must_represent_each_others_possible_history() {
        let bound = RecordsBound::of;
        for (left, right, expected) in [
            (bound(4096, false), bound(8192, false), true),
            (bound(4096, false), bound(8192, true), false),
            (bound(4096, true), bound(8192, false), true),
            (bound(8192, true), bound(8192, true), true),
            (bound(8192, true), bound(16384, true), false),
            (bound(4096, true), bound(1000, false), false),
            (bound(1000, false), bound(1000, false), true),
            (bound(0, false), bound(4096, false), false),
        ] {
            assert_eq!(left.can_exchange(right), expected, "{left:?} / {right:?}");
            assert_eq!(right.can_exchange(left), expected, "{right:?} / {left:?}");
        }
    }

    /// The wire form reads back, and only exactly it does.
    ///
    /// NEGATIVE CONTROL (in the test): each near miss (another version, a leading zero, a sign,
    /// a doubled space, a trailing word, another state, a bound past `u32`) is no bound.
    #[test]
    fn a_records_bound_reads_back_and_nothing_near_it_does() {
        for bound in [
            RecordsBound::of(4_096, false),
            RecordsBound::of(16_384, true),
            RecordsBound::of(1, true),
            RecordsBound::of(u32::MAX as usize, false),
        ] {
            assert_eq!(RecordsBound::parse(&bound.value()), Some(bound));
        }
        assert_eq!(RecordsBound::of(usize::MAX, true).held, u32::MAX);
        assert_eq!(RecordsBound::of(4_096, true).value(), "1 4096 crossed");
        for near in [
            "",
            "1",
            "1 4096",
            "2 4096 crossed",
            "01 4096 crossed",
            "1 04096 crossed",
            "1 0 crossed",
            "1 +4096 crossed",
            "1 -4096 crossed",
            "1  4096 crossed",
            "1 4096  crossed",
            " 1 4096 crossed",
            "1 4096 crossed ",
            "1 4096 crossed extra",
            "1 4096 Crossed",
            "1 4096 open",
            "1 4294967296 crossed",
            "1 99999999999 crossed",
            "1\t4096\tcrossed",
            "1 4,096 crossed",
        ] {
            assert_eq!(RecordsBound::parse(near), None, "{near:?}");
        }
    }

    /// The line is the legacy bound itself: a log at it has not crossed, one record more has.
    #[test]
    fn a_log_crosses_one_record_past_the_legacy_bound() {
        assert!(!past_legacy(LEGACY_RECORDS));
        assert!(past_legacy(LEGACY_RECORDS + 1));
    }

    /// A mark is stored once, durably, and stays; one a test refuses is not stored.
    ///
    /// NEGATIVE CONTROL (in the test): the refused write leaves nothing at the path.
    #[cfg(unix)]
    #[test]
    fn a_mark_is_stored_and_stays() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let mark = Mark::at(root.path().join("records.crossed"));
        assert!(!mark.stored().expect("readable"));
        inject(
            mark.path(),
            Faults {
                fail_store: true,
                ..Faults::default()
            },
        );
        assert!(mark.store().is_err());
        assert!(
            !mark.stored().expect("readable"),
            "a refused write stores nothing"
        );
        inject(mark.path(), Faults::default());
        mark.store().expect("stored");
        mark.store().expect("stored again, as it was");
        assert!(mark.stored().expect("readable"));
        // Whatever is there counts: a mark cut short is still a mark.
        std::fs::write(mark.path(), b"").expect("truncated");
        assert!(mark.stored().expect("readable"));
    }
}
