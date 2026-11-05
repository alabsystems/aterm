// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OVERLAP FACT ([`crate::supervise::facts::FactKind::Overlap`]): which other sessions of
//! this aterm change a file this session's branch changes, read by the host from the sessions'
//! work records ([`crate::harness::work_record::overlap`]) and told to the agent as a count and
//! those sessions' sids, never a path, a file name or a branch. Another aterm's sessions on the
//! same Mac are not compared: the host reads its own sessions' records alone.
//!
//! A sid is the one word the sentence takes besides numbers, and only one aterm minted
//! ([`PeerSid`]): a reading is made from words ([`OverlapReading::new`]), and one whose peer word
//! is anything else is no reading, so nothing a repository or a branch is named can reach the
//! typed line.

use aterm_types::control_request::Sid;

use super::facts::EpisodeKey;

/// How long a minted sid is: `s-` and 20 lowercase hex digits.
const MINTED_SID_LEN: usize = 22;

/// How many sids the sentence names before it counts the rest.
const SIDS_SHOWN: usize = 3;

/// How old the oldest work record an overlap rests on may be, ms, for its reading to tell its
/// episode or close one ([`OverlapReading::tells_at`]); the host compares no record older
/// (`aterm-gui`'s `work_overlap_live`). A record is read at its session's turn ends, at most once
/// per [`crate::harness::work_record::WORK_FLOOR`], so a peer's is as old as its last turn end
/// and is no reading of the moment, as [`super::facts::FACT_FRESH_MS`] asks of other facts. One
/// older than this is a session whose reads are held, failing or not made (its harness off), or
/// one in a turn this long, and is no evidence of what its branch changes now.
pub const OVERLAP_FRESH_MS: i64 = 10 * 60_000;

/// A SESSION ID ATERM MINTED, the only word of a peer's an overlap fact carries: `s-` and 20
/// lowercase hex digits ([`Self::of`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerSid(String);

impl PeerSid {
    /// `word` as a peer's sid where it is one aterm minted (the control grammar's sid, at the
    /// minted length), else `None`: a path, a branch or any other word is refused.
    #[must_use]
    pub fn of(word: &str) -> Option<Self> {
        (word.len() == MINTED_SID_LEN && Sid::parse(word).is_some()).then(|| Self(word.to_string()))
    }

    /// The sid.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// WHAT THE HOST READ OF THIS SESSION'S OVERLAP (`IdleHost::overlap`), as of `as_of_ms` (unix
/// ms, when the oldest record it rests on was read): how many of the files its branch changes
/// other sessions of this aterm also change, and those sessions, sorted. No peer is "read, no
/// overlap", which closes a told episode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlapReading {
    as_of_ms: i64,
    files: u32,
    peers: Vec<PeerSid>,
}

impl OverlapReading {
    /// THE CLOSED-TEMPLATE CHECK: a reading of `files` files shared with the sessions `peers`
    /// names, as of `as_of_ms`, where every peer word is a minted sid ([`PeerSid::of`]), and a
    /// reading that names a peer names a file; else `None`. The peers are kept sorted, each once.
    #[must_use]
    pub fn new(as_of_ms: i64, files: u32, peers: &[&str]) -> Option<Self> {
        let mut sids = peers
            .iter()
            .map(|word| PeerSid::of(word))
            .collect::<Option<Vec<_>>>()?;
        sids.sort();
        sids.dedup();
        if !sids.is_empty() && files == 0 {
            return None;
        }
        Some(Self {
            as_of_ms,
            files: if sids.is_empty() { 0 } else { files },
            peers: sids,
        })
    }

    /// When it was read, unix ms.
    #[must_use]
    pub const fn as_of_ms(&self) -> i64 {
        self.as_of_ms
    }

    /// How many of this session's files the peers also change.
    #[must_use]
    pub const fn files(&self) -> u32 {
        self.files
    }

    /// The peers, sorted.
    #[must_use]
    pub fn peers(&self) -> &[PeerSid] {
        &self.peers
    }

    /// No older than [`OVERLAP_FRESH_MS`] at `now_ms` (one from the future, a clock stepped back,
    /// is no older): a reading that may tell its episode, or close one it no longer names.
    #[must_use]
    pub fn tells_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) <= OVERLAP_FRESH_MS
    }

    /// Its episode: one set of peers ([`EpisodeKey::overlap`]); `None` with none.
    #[must_use]
    pub fn episode(&self) -> Option<EpisodeKey> {
        (!self.peers.is_empty()).then(|| EpisodeKey::overlap(&self.peers))
    }

    /// THE OVERLAP SENTENCE, from a closed template of numbers and sids only: `{k} files this
    /// branch changes are also changed by {sid}, {sid} and {sid}` (`1 file … is also changed by
    /// …`), at most [`SIDS_SHOWN`] sids and then `and {n} more sessions`, within
    /// [`super::facts::FACT_CAP`] however many files and peers. Empty with no peer.
    #[must_use]
    pub fn told(&self) -> String {
        let n = self.peers.len();
        if n == 0 {
            return String::new();
        }
        let shown: Vec<&str> = self
            .peers
            .iter()
            .take(SIDS_SHOWN)
            .map(PeerSid::as_str)
            .collect();
        let by = if n > SIDS_SHOWN {
            let more = n - SIDS_SHOWN;
            let sessions = if more == 1 { "session" } else { "sessions" };
            format!("{} and {more} more {sessions}", shown.join(", "))
        } else {
            match shown.split_last() {
                Some((last, [])) => (*last).to_string(),
                Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
                None => String::new(),
            }
        };
        let files = if self.files == 1 {
            "1 file this branch changes is".to_string()
        } else {
            format!("{} files this branch changes are", self.files)
        };
        format!("{files} also changed by {by}")
    }
}

#[cfg(test)]
#[path = "overlap_tests.rs"]
mod tests;
