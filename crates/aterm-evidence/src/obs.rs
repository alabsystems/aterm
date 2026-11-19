// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One reading of evidence ([`Obs`]): what it says, when the evidence itself is dated, which
//! witness it was read from, where, and of which subject; and the reading young enough to act on
//! ([`Fresh`]).

use std::fmt;
use std::time::Duration;

/// The domain tag every [`CredentialPrint`] hashes first, so no other SHA-256 in aterm can equal
/// one.
const PRINT_DOMAIN: &[u8] = b"aterm-evidence credential print v1\0";

/// A credential as evidence names it: a SHA-256 over its namespace (the vendor's login store, a
/// token route), its row pk and its binding generation, with no credential bytes in it. A
/// renewal is a new generation, so it is a new print and starts with no evidence at all.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CredentialPrint([u8; 32]);

impl CredentialPrint {
    /// The print of generation `generation` of row `row_pk` in `namespace`. Each part is hashed
    /// with its length before it, so no two different triples share an input.
    #[must_use]
    pub fn new(namespace: &str, row_pk: &str, generation: u64) -> Self {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(PRINT_DOMAIN);
        for part in [namespace.as_bytes(), row_pk.as_bytes()] {
            hash.update((part.len() as u64).to_be_bytes());
            hash.update(part);
        }
        hash.update(generation.to_be_bytes());
        Self(hash.finalize())
    }

    /// The digest.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for CredentialPrint {
    /// The first eight bytes in hex: enough to tell prints apart in a record.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialPrint(")?;
        for byte in &self.0[..8] {
            write!(f, "{byte:02x}")?;
        }
        f.write_str(")")
    }
}

/// What one observation was read from. The same witness is one observation however often it is
/// read: a screen line still on screen at a later look, a transcript row read again, a peer's
/// record relayed twice.
///
/// An id names one observation wherever it was made, so two observations never share one: a
/// [`Ledger`](crate::Ledger) keeps one reading per id across every subject, so a second
/// observation under an id already read would be taken for a re-read and its evidence dropped.
/// Each variant names the place it was read where its other parts repeat across places.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WitnessId {
    /// A transcript row, by an id no other transcript's row carries (a vendor's row UUID).
    TranscriptRow(String),
    /// A screen line: the tab it is on, its text's hash and the epoch of the row it stands on,
    /// so the same line still on that screen is the same witness, the same words printed again
    /// later are another, and the same words at the same row epoch in another tab are another
    /// (each terminal counts its row epochs on its own).
    ScreenLine {
        /// The session id of the tab whose screen shows the line.
        sid: String,
        /// The line's text hash.
        hash: u64,
        /// The epoch of the row the line stands on.
        row_epoch: u64,
    },
    /// A response's or a reading's id, unique as its minter makes it (a server's request id).
    Reading(String),
    /// A fleet peer's record: the node that wrote it and its sequence number there.
    Fleet {
        /// The node.
        node: String,
        /// The record's sequence number at that node.
        seq: u64,
    },
}

/// Where an observation was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// A vendor's transcript.
    Transcript,
    /// A terminal screen.
    Screen,
    /// A reading of a program's traffic or a server's response.
    Reading,
    /// A fleet peer's record.
    Fleet,
    /// A record this program kept at the time of the evidence (a binding's served clock).
    Record,
}

/// What an observation is about. An observation of one subject never speaks for another.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Subject {
    /// One credential, by its print.
    Credential(CredentialPrint),
    /// One account, by its roster row pk.
    Account(String),
    /// One process: its pid and its kernel start time, so a reused pid is another process.
    Process {
        /// The pid.
        pid: u32,
        /// The process's kernel start time.
        born: u64,
    },
    /// One tab, by its session id.
    Tab(String),
}

/// One dated, witnessed observation of one subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Obs<T> {
    /// What the evidence says.
    pub value: T,
    /// When the evidence itself is dated, in seconds since the Unix epoch: a transcript row's
    /// date, a reading's time, a screen line's first sighting. Never when it was read again or
    /// written down.
    pub at: u64,
    /// What it was read from.
    pub witness: WitnessId,
    /// Where it was read.
    pub source: Source,
    /// What it is about.
    pub subject: Subject,
}

impl<T: Clone> Obs<T> {
    /// This observation as evidence of `now`: `Some` only when it is dated no later than `now`
    /// and no more than `horizon` before it. An observation dated after `now` is not evidence of
    /// `now`.
    #[must_use]
    pub fn fresh(&self, now: u64, horizon: Duration) -> Option<Fresh<T>> {
        let age = now.checked_sub(self.at)?;
        let horizon = horizon.as_secs();
        (age <= horizon).then(|| Fresh {
            obs: self.clone(),
            until: self.at.saturating_add(horizon),
        })
    }
}

/// An observation young enough to act on: made only by [`Obs::fresh`], so a value of this type is
/// a check of its age that someone made, against a horizon they named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fresh<T> {
    obs: Obs<T>,
    until: u64,
}

impl<T> Fresh<T> {
    /// The observation.
    #[must_use]
    pub const fn obs(&self) -> &Obs<T> {
        &self.obs
    }

    /// What it says.
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.obs.value
    }

    /// The last second, since the Unix epoch, it is fresh at under the horizon it was checked
    /// against.
    #[must_use]
    pub const fn until(&self) -> u64 {
        self.until
    }

    /// The observation, taken back.
    #[must_use]
    pub fn into_obs(self) -> Obs<T> {
        self.obs
    }
}

#[cfg(test)]
#[path = "obs_tests.rs"]
mod tests;
