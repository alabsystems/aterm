// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What aterm v0.198.0 and v0.201.0 do with what a format-2 node offers them, captured from
//! their source so a test runs it here: the records bound they read and state
//! (`crates/aterm-secrets/src/crossing.rs`, `RecordsBound`, renamed [`Bound`]), the gate their
//! server's `answer` holds a records POST's query to before it reads a body
//! (`crates/aterm-secrets/src/server.rs`), their `records_fence`, and the magic and version
//! check `membership_log::Record::parse` and its frame readers make of every record before
//! anything else.
//!
//! The release-line commits are `7e9765cf9d5` (v0.198.0, hop 4) and `f4d4392c85c` (v0.201.0,
//! hop 6). The `RecordsBound` item text, from `pub struct RecordsBound` to `past_legacy`'s doc,
//! is byte-identical in both (its SHA-1 is `1ae095cf3425f334d43eb5c21a62b43c30f70bdd` at each),
//! the server's query gate and `records_fence` read the same at both, and both define `MAGIC`
//! `ALM1` and `VERSION` 1. The function bodies below are those texts with the type renamed, and
//! the gate's and fence's replies reduced to their status.

use crate::membership_log::LEGACY_RECORDS;

/// v0.198.0's and v0.201.0's `RecordsBound`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bound {
    pub(crate) held: u32,
    pub(crate) crossed: bool,
}

const VERSION: &str = "1";
const CROSSED: &str = "crossed";
const UNCROSSED: &str = "uncrossed";

impl Bound {
    pub(crate) const fn can_exchange(self, peer: Self) -> bool {
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

    pub(crate) fn of(held: usize, crossed: bool) -> Self {
        Self {
            held: u32::try_from(held).unwrap_or(u32::MAX),
            crossed,
        }
    }

    pub(crate) fn value(self) -> String {
        let state = if self.crossed { CROSSED } else { UNCROSSED };
        format!("{VERSION} {} {state}", self.held)
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
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

/// Their server's answer to a records POST's query and stated bound before it reads a body:
/// `Some(400)` refuses it unread, `None` goes on (`answer`, `crossed_post`).
pub(crate) fn post_gate(query: Option<&str>, records_bound: Option<Bound>) -> Option<u16> {
    let waits = false;
    let crossed_post = query == Some("crossed=2");
    if query.is_some() && !waits && !crossed_post {
        return Some(400);
    }
    if crossed_post && !records_bound.is_some_and(|bound| bound.crossed) {
        return Some(400);
    }
    None
}

/// Their `records_fence` for a server of `held` records, `crossed` or not: `Some(409)` refuses
/// the request, `None` serves it or reads its body.
pub(crate) fn records_fence(held: usize, crossed: bool, peer: Option<Bound>) -> Option<u16> {
    let own = Bound::of(held, crossed);
    match peer {
        Some(peer) if !own.can_exchange(peer) => Some(409),
        None if crossed => Some(409),
        _ => None,
    }
}

/// Whether their record readers go past a record's first five bytes: `ALM1` and version 1, as
/// `reader.take(4)? != MAGIC || reader.byte()? != VERSION` refuses anything else as "not a
/// membership record", which refuses the whole stream it is in.
pub(crate) fn frames_as_a_record(wire: &[u8]) -> bool {
    wire.get(..4) == Some(b"ALM1".as_slice()) && wire.get(4) == Some(&1)
}
