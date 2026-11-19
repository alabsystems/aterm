// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TAB'S OUT RING (HX-15h): what a harness's record stream said and made that no log row
//! holds whole. A `msg` or `diff` record's body, and any record longer than one log record, is
//! kept here as its line (as the wire codec writes it) escaped as the log escapes text, behind a
//! row under the record's own kind that carries its `hseq=` and its digest `fp=`
//! (`docs/HARNESS-CONTRACT.md`, "The conversation records").
//!
//! One ring per tab, apart from the session log's own: [`OUT_RING_BYTES`] at most, the oldest
//! entries dropped first. It lives in memory only and is never carried across a seamless
//! update: a successor's ring starts empty, and a stream that opens on a ring holding none of
//! its claimant's conversation is sent `want`, which asks the harness for the conversation
//! again (the vendor's own record is the lasting copy until LT-06 gives the ring a store). Every
//! platform builds this file; nothing in it names a wire type.
#![cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "only the Unix control stream fills or reads the ring"
    )
)]

use std::collections::VecDeque;

use crate::harness_claim::Proven;

/// The most bytes one tab's ring keeps (1 MiB): a push drops the oldest entries until it fits.
pub(crate) const OUT_RING_BYTES: usize = 1 << 20;

/// The hex characters of a record's digest (`fp=`): a log ID at its bound.
pub(crate) const FP_HEX: usize = 40;

/// What an entry costs beside its texts: its claimant, its numbers and its kind's word.
const ENTRY_OVERHEAD: usize = 64;

/// One record the ring keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutEntry {
    /// The claimant whose stream sent it.
    pub(crate) claimant: Proven,
    /// The aterm instance whose stream applied it.
    pub(crate) epoch: String,
    /// The record's `seq` (its row's `hseq=`).
    pub(crate) hseq: u64,
    /// The record's kind, by its wire word (its row's kind).
    pub(crate) kind: &'static str,
    /// The record's digest ([`fp`]), its row's `fp=`.
    pub(crate) fp: String,
    /// The record's line, escaped ([`escape`]).
    pub(crate) body: String,
}

impl OutEntry {
    /// What the entry counts against [`OUT_RING_BYTES`].
    pub(crate) fn bytes(&self) -> usize {
        ENTRY_OVERHEAD
            + self.claimant.start.len()
            + self.epoch.len()
            + self.fp.len()
            + self.body.len()
    }
}

/// One tab's Out ring.
#[derive(Clone, Debug, Default)]
pub(crate) struct OutRing {
    entries: VecDeque<OutEntry>,
    bytes: usize,
    /// Entries dropped to make room, oldest first, since the ring began.
    dropped: u64,
}

impl OutRing {
    /// Keep `entry`, dropping the oldest entries until it fits. An entry larger than the whole
    /// ring is not kept (no record's line, escaped, comes near it: a line is at most 256 KiB and
    /// its escape at most three times that). Whether it was kept.
    pub(crate) fn push(&mut self, entry: OutEntry) -> bool {
        let size = entry.bytes();
        if size > OUT_RING_BYTES {
            return false;
        }
        while self.bytes + size > OUT_RING_BYTES {
            let Some(oldest) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= oldest.bytes();
            self.dropped += 1;
        }
        self.bytes += size;
        self.entries.push_back(entry);
        true
    }

    /// Whether the ring holds any of `claimant`'s conversation.
    pub(crate) fn holds(&self, claimant: &Proven) -> bool {
        self.entries.iter().any(|entry| entry.claimant == *claimant)
    }

    /// The entries, oldest first.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "read by tests until HX-38's page reads the ring")
    )]
    pub(crate) fn entries(&self) -> impl Iterator<Item = &OutEntry> {
        self.entries.iter()
    }

    /// The bytes the entries count.
    #[cfg_attr(not(test), allow(dead_code, reason = "read by tests"))]
    pub(crate) const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Entries dropped to make room since the ring began.
    #[cfg_attr(not(test), allow(dead_code, reason = "read by tests"))]
    pub(crate) const fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// A record's line escaped as the session log escapes text (`aterm_slog::BoundedText`): the
/// characters the log never shows dropped, then percent-encoded (`aterm_codec::pct`), with no
/// clamp: the ring keeps the whole line.
pub(crate) fn escape(line: &str) -> String {
    let kept: String = line
        .chars()
        .filter(|c| !aterm_messages::text::is_hidden_char(*c))
        .collect();
    aterm_codec::pct::encode(&kept)
}

/// A record's digest, its row's `fp=`: the first 20 bytes of the SHA-256 of its line as the wire
/// codec writes it, as 40 lowercase hex characters (a log ID at its bound). The same record
/// always has the same digest, so a reader can tell a row's body in the ring from another's.
pub(crate) fn fp(line: &str) -> String {
    let digest = aterm_digest::Sha256::digest(line.as_bytes());
    let mut hex = aterm_codec::hex::encode_lower(&digest);
    hex.truncate(FP_HEX);
    hex
}

#[cfg(test)]
#[path = "out_ring_tests.rs"]
mod tests;
