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
//!
//! **The format fence** (`docs/DESIGN-vault-log-2026-11-14.md` step 3, shipped dark). A bound
//! also states the format its node's records are in and the most bytes its log may hold
//! ([`RecordsBound::format`], [`RecordsBound::bytes`]). Format 1 is every record this build and
//! every build since 0.146 reads; its capacity is [`FORMAT_ONE_BYTES`], the one stream bound
//! every such build shares, so format 1 states it by its version alone: a node with no
//! [`FormatMark`] states the version-1 bound byte for byte, as every build since the fence did. A
//! node stores its format mark before it holds its first record of a later format
//! ([`after_format_mark`]), and from then on states the version-2 bound
//! (`2 <held> <bytes> <format> crossed|uncrossed`), which every build before it reads as no bound
//! at all. [`RecordsBound::can_exchange`] requires both formats equal and both byte capacities to
//! hold the other's, so a format-2 node answers `409` to a request that states no version-2
//! bound, takes nothing from an answer that states none, and posts with [`FORMAT_POST_QUERY`],
//! which every server before it refuses unread. Nothing in this build stores a format mark: the
//! first format-2 write (tracker FL-148) waits on the fence's floor
//! ([`crate::commons::hearing::FENCE`]).

use crate::membership_log::{LEGACY_RECORDS, MAX_RECORDS_STREAM, RecordBudgets};

/// What a node states of its records ([`crate::transport::RECORDS_BOUND_HEADER`]): the most
/// records it holds, the most bytes, the format they are in, and whether its log has crossed
/// ([`past_legacy`], [`Mark`]).
///
/// On the wire a format-1 bound is `1 <held> crossed` or `1 <held> uncrossed`: the version `1`,
/// the held bound in decimal with no leading zero, and the state, each one space apart and
/// nothing else; its byte capacity is [`FORMAT_ONE_BYTES`] and is not stated. A bound of a later
/// format is `2 <held> <bytes> <format> crossed|uncrossed`, each number a positive decimal with no
/// leading zero, the format at least 2. A value that is not exactly one of those is no bound at
/// all ([`RecordsBound::parse`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordsBound {
    /// The most records the sender holds in all ([`RecordBudgets::held`]).
    pub held: u32,
    /// The most bytes of records the sender's log holds: [`FORMAT_ONE_BYTES`] at format 1.
    pub bytes: u64,
    /// The format the sender's records are in: [`FORMAT_ONE`] until its [`FormatMark`] says
    /// otherwise.
    pub format: u32,
    /// Whether the sender's log has crossed.
    pub crossed: bool,
}

/// The version of [`RecordsBound`]'s wire form that states format 1.
const VERSION: &str = "1";
/// The version of [`RecordsBound`]'s wire form that states a format and a byte capacity.
const VERSION_FORMATTED: &str = "2";
/// [`RecordsBound::crossed`] on the wire.
const CROSSED: &str = "crossed";
/// Not [`RecordsBound::crossed`], on the wire.
const UNCROSSED: &str = "uncrossed";

/// The records format every build since 0.146 reads and writes: record version 1.
pub const FORMAT_ONE: u32 = 1;
/// The format whose `Put`s sign their body's digest (`docs/DESIGN-vault-log-2026-11-14.md` step
/// 4): nothing in this build writes it.
pub const FORMAT_TWO: u32 = 2;
/// The byte capacity of a format-1 log: the stream bound every build since 0.146 shares
/// ([`MAX_RECORDS_STREAM`]), so a format-1 bound need not state it.
pub const FORMAT_ONE_BYTES: u64 = MAX_RECORDS_STREAM as u64;

/// The query a format-2 node's records POST carries, beside its version-2
/// [`crate::transport::RECORDS_BOUND_HEADER`]: every server before the format fence answers a
/// records POST with any query but [`crate::transport::CROSSED_POST_QUERY`] `400` before it reads
/// a body, so none of them is offered a format-2 node's records.
pub const FORMAT_POST_QUERY: &str = "format=2";

impl RecordsBound {
    /// Whether both nodes can represent every history the other may serve: the same format, a
    /// byte capacity each that holds the other's, and record capacities that do. An uncrossed
    /// history is at most the legacy bound; a crossed history requires the sender's advertised
    /// capacity even if its file is now short. This is a compatibility check, not authorization
    /// or evidence of freshness.
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
        self.format == peer.format
            && self.bytes != 0
            && self.bytes >= peer.bytes
            && peer.bytes >= self.bytes
            && self.held != 0
            && peer.held != 0
            && self.held >= peer_history
            && peer.held >= own_history
    }

    /// Whether this node exchanges nothing with a source that states no bound (a build before
    /// the crossed fence) or a version-1 bound where it states version 2: its log crossed, or its
    /// records are past format 1.
    #[must_use]
    pub const fn fenced(self) -> bool {
        self.crossed || self.format != FORMAT_ONE
    }

    /// A format-1 node holding at most `held` records, `crossed` or not. A bound past `u32::MAX`
    /// is stated as `u32::MAX`.
    #[must_use]
    pub fn of(held: usize, crossed: bool) -> Self {
        Self {
            held: u32::try_from(held).unwrap_or(u32::MAX),
            bytes: FORMAT_ONE_BYTES,
            format: FORMAT_ONE,
            crossed,
        }
    }

    /// This bound at `format`: its byte capacity stays the one this build's log holds
    /// ([`FORMAT_ONE_BYTES`]) until a later format's store states its own (tracker FL-148).
    #[must_use]
    pub const fn at_format(self, format: u32) -> Self {
        Self { format, ..self }
    }

    /// A format-1 node of this build holding its records within [`RecordBudgets::current`],
    /// `crossed` or not.
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

    /// The header's value: the version-1 form at format 1, byte for byte as every build since the
    /// fence states it, and the version-2 form at any later format.
    #[must_use]
    pub fn value(self) -> String {
        let state = if self.crossed { CROSSED } else { UNCROSSED };
        if self.format == FORMAT_ONE {
            format!("{VERSION} {} {state}", self.held)
        } else {
            format!(
                "{VERSION_FORMATTED} {} {} {} {state}",
                self.held, self.bytes, self.format
            )
        }
    }

    /// The bound `value` states, read strictly: `None` for anything but exactly
    /// `1 <held> crossed|uncrossed` or `2 <held> <bytes> <format> crossed|uncrossed`, each number
    /// a positive decimal with no leading zero that fits its field (`u32`, `u64`, `u32`), the
    /// format at least 2.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let words: Vec<&str> = value.split(' ').take(6).collect();
        let (held, bytes, format, state) = match words.as_slice() {
            [VERSION, held, state] => (*held, None, None, *state),
            [VERSION_FORMATTED, held, bytes, format, state] => {
                (*held, Some(*bytes), Some(*format), *state)
            }
            _ => return None,
        };
        let held = u32::try_from(decimal(held)?).ok()?;
        let (bytes, format) = match (bytes, format) {
            (Some(bytes), Some(format)) => (
                decimal(bytes)?,
                u32::try_from(decimal(format)?)
                    .ok()
                    .filter(|format| *format > FORMAT_ONE)?,
            ),
            _ => (FORMAT_ONE_BYTES, FORMAT_ONE),
        };
        let crossed = match state {
            CROSSED => true,
            UNCROSSED => false,
            _ => return None,
        };
        Some(Self {
            held,
            bytes,
            format,
            crossed,
        })
    }
}

/// A positive decimal with no leading zero and no sign that fits a `u64`.
fn decimal(word: &str) -> Option<u64> {
    if word.is_empty()
        || word.len() > 20
        || word.starts_with('0')
        || !word.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    word.parse().ok()
}

/// Whether a log holding `held` records has crossed by its count alone: it holds more than a
/// 0.146-0.152 build takes.
#[must_use]
pub const fn past_legacy(held: usize) -> bool {
    held > LEGACY_RECORDS
}

#[cfg(unix)]
pub(crate) use mark::{FormatMark, Mark};

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

    /// What a stored format mark holds, before its format's decimal and a newline.
    const FORMAT_MARK: &str = "aterm-secrets records format ";

    /// The most bytes a format mark holds: its text, a `u32` and a newline.
    const FORMAT_MARK_MAX: usize = 64;

    /// A node's format mark: a small file beside its records that names the format they are in
    /// ([`super::RecordsBound::format`]), stored atomically and made durable before the node holds
    /// its first record of that format ([`super::after_format_mark`]), and never lowered or
    /// removed. No file there is format 1.
    #[derive(Clone, Debug)]
    pub(crate) struct FormatMark {
        path: PathBuf,
    }

    impl FormatMark {
        /// The format mark at `path`.
        pub(crate) const fn at(path: PathBuf) -> Self {
            Self { path }
        }

        /// Where it is: what a test's faults are keyed by ([`inject`]).
        #[cfg(test)]
        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        /// The format it names: [`super::FORMAT_ONE`] where nothing is there.
        ///
        /// # Errors
        /// The path cannot be read, or holds anything but exactly a mark of a format past
        /// [`super::FORMAT_ONE`]: a node that cannot say which format its records are in states
        /// no bound, serves nothing and takes nothing, rather than state format 1 beside records
        /// of a later one.
        pub(crate) fn stored(&self) -> crate::Result<u32> {
            #[cfg(test)]
            if faults(&self.path).fail_look {
                return Err(Error::new(format!(
                    "{}: a test fails the look for the format mark",
                    self.path.display()
                )));
            }
            if crate::fsx::kind(&self.path)? == crate::fsx::Kind::Missing {
                return Ok(super::FORMAT_ONE);
            }
            let unread = || {
                Error::new(format!(
                    "{}: the records format mark is not one this build reads",
                    self.path.display()
                ))
            };
            let bytes = crate::fsx::read_owned(&self.path, FORMAT_MARK_MAX)?.ok_or_else(unread)?;
            std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| text.strip_prefix(FORMAT_MARK))
                .and_then(|rest| rest.strip_suffix('\n'))
                .and_then(super::decimal)
                .and_then(|format| u32::try_from(format).ok())
                .filter(|format| *format > super::FORMAT_ONE)
                .ok_or_else(unread)
        }

        /// Store it at `format`, durably, unless it names `format` or a later one already: a mark
        /// is never lowered.
        ///
        /// # Errors
        /// `format` is [`super::FORMAT_ONE`] (which no mark names), the mark cannot be read, or
        /// the write or its flush fails: the mark may not be stored, and the caller holds nothing
        /// that needed it.
        pub(crate) fn store(&self, format: u32) -> crate::Result<()> {
            if format <= super::FORMAT_ONE {
                return Err(Error::new("format 1 is stated by no format mark"));
            }
            #[cfg(test)]
            if faults(&self.path).fail_store {
                return Err(Error::new(format!(
                    "{}: a test refuses the format mark",
                    self.path.display()
                )));
            }
            // The read and the write under one lock: two stores at once otherwise both read the
            // mark below them, and the later write can lower the one the other made.
            let _held = StoreLock::take(&self.lock_path())?;
            if self.stored()? >= format {
                return Ok(());
            }
            crate::fsx::write_atomic(
                &self.path,
                format!("{FORMAT_MARK}{format}\n").as_bytes(),
                0o600,
            )
            .map_err(|e| Error::new(format!("storing the records format mark: {e}")))
        }

        /// The lock file beside the mark that [`Self::store`] holds while it reads and writes:
        /// the mark itself is replaced by a rename, so a lock on it would guard nothing.
        fn lock_path(&self) -> PathBuf {
            let mut name = self.path.as_os_str().to_owned();
            name.push(".lock");
            PathBuf::from(name)
        }
    }

    /// An exclusive `flock` on [`FormatMark::lock_path`], released by `LOCK_UN` when dropped.
    struct StoreLock(std::fs::File);

    impl StoreLock {
        /// Wait for it and take it.
        fn take(path: &std::path::Path) -> crate::Result<Self> {
            let file = crate::fsx::open_lock(path)?;
            #[expect(
                clippy::disallowed_methods,
                reason = "the take of StoreLock, whose Drop releases by LOCK_UN (File::unlock)"
            )]
            file.lock()
                .map_err(|e| Error::new(format!("{}: {e}", path.display())))?;
            Ok(Self(file))
        }
    }

    impl Drop for StoreLock {
        fn drop(&mut self) {
            let _ = self.0.unlock();
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
        /// MUTANT: the node holds its first record of a later format before it stores the format
        /// mark ([`super::after_format_mark`]), so [`Faults::dies_between`] leaves it held with
        /// no mark, and the node states format 1 beside it.
        pub(crate) format_after_write: bool,
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

/// What the node whose log `store` holds, whose crossed mark is `mark` and whose format mark is
/// `format` states of its records: the version-1 bound byte for byte until a format mark is
/// stored.
///
/// # Errors
/// As [`crossed`], or the format mark cannot be read ([`FormatMark::stored`]).
#[cfg(unix)]
pub(crate) fn stated(
    mark: &Mark,
    format: &FormatMark,
    store: &crate::membership_log::Store,
) -> crate::Result<RecordsBound> {
    Ok(RecordsBound::of(store.budgets().held, crossed(mark, store)?).at_format(format.stored()?))
}

/// Run `write`, which makes the node whose format mark is `mark` hold its first record of
/// `format`, only once the mark names `format` durably: so no records file, store or reply holds
/// a record of a format its node does not state, and a node that dies between the two holds the
/// mark and none of the records. Nothing in this build calls it outside a test: the first
/// format-2 write (tracker FL-148) waits on [`crate::commons::hearing::FENCE`].
///
/// # Errors
/// The mark cannot be stored (then `write` does not run), or `write` fails.
#[cfg(unix)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the first format-2 write (tracker FL-148) calls it; nothing writes format 2 yet"
    )
)]
pub(crate) fn after_format_mark<T>(
    mark: &FormatMark,
    format: u32,
    write: impl FnOnce() -> crate::Result<T>,
) -> crate::Result<T> {
    // MUTANT (`Faults::format_after_write`): the records first, then the mark.
    #[cfg(test)]
    if faults(mark.path()).format_after_write {
        let written = write()?;
        if faults(mark.path()).dies_between {
            return Err(crate::Error::new(
                "a test's node died before its format mark",
            ));
        }
        mark.store(format)?;
        return Ok(written);
    }
    mark.store(format)?;
    #[cfg(test)]
    if faults(mark.path()).dies_between {
        return Err(crate::Error::new(
            "a test's node died between its format mark and the records",
        ));
    }
    write()
}

#[cfg(all(unix, test))]
#[path = "crossing/fixture.rs"]
pub(crate) mod fixture;

#[cfg(all(unix, test))]
#[path = "crossing/legacy.rs"]
pub(crate) mod legacy;

#[cfg(test)]
#[path = "crossing_tests.rs"]
mod tests;
