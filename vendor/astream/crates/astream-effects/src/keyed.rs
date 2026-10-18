// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A read-only, keyed view of a tape's reply doors.
//!
//! [`ReplaySeam`](crate::ReplaySeam) answers in order and consumes as it goes:
//! the right shape for re-running the recorded program, the wrong one for
//! asking a recorded question out of order. A second reader — a candidate
//! decision function re-deciding a recorded point, a "why did it do that"
//! report — needs "what did the door `probe.git_view` answer to THIS request, as
//! of record N?" without disturbing the replay that drives the program, and
//! without depending on the order in which it asks. [`KeyedView`] is that
//! reader: it indexes every [`Exchange`](crate::EffectRecord::Exchange) by
//! (door, request) once, and each [`lookup`](KeyedView::lookup) is a pure
//! function of the tape and its arguments.

use crate::replay::ReplayDivergence;
use crate::seam::EffectRecord;
use std::collections::BTreeMap;

// The Clean island, out of line: only a Trust compiler lexes it, so it mounts under
// `cfg(clean_islands)` (build.rs) and a stock compiler never opens the file.
#[cfg(clean_islands)]
#[path = "keyed_island.rs"]
mod keyed_island;
#[cfg(not(clean_islands))]
pub use crate::twins::keyed_clamp_cutoff;
#[cfg(clean_islands)]
pub use keyed_island::keyed_clamp_cutoff;

contract_fn! {
    /// Verified check that an indexed exchange `pos` lies at or before `at_or_before`,
    /// linked via `clean-kernel-defeq` to `keyed_hit_within_cutoff_isl`.
    #[must_use]
    pub fn keyed_hit_within_cutoff(pos: usize, at_or_before: usize) -> bool
    clauses [ensures result == (pos <= at_or_before)]
    {
        pos <= at_or_before
    }
}

/// The answer a [`KeyedView`] found: the reply and where on the tape it sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyedReply<'a> {
    /// The index of the `Exchange` record in the slice the view was built from.
    pub position: usize,
    /// The recorded reply.
    pub reply: &'a [u8],
}

/// Every [`Exchange`](crate::EffectRecord::Exchange) of a tape, indexed by
/// (door, request). Built once from a slice of records; never consumes, never
/// mutates, never asks the live world. Other record kinds are ignored: a clock
/// or a file read is not a question with a reusable answer.
///
/// The index is a `BTreeMap` rather than a hash map so the crate stays free of
/// randomness anywhere a replay can reach: no answer depends on the map's
/// order, but a reviewer should not have to check that.
#[derive(Debug, Clone, Default)]
pub struct KeyedView<'a> {
    /// (door, request) → its exchanges, by ascending position (the slice is
    /// walked front to back, so each push is the latest so far).
    index: BTreeMap<(&'a str, &'a [u8]), Vec<KeyedReply<'a>>>,
}

impl<'a> KeyedView<'a> {
    /// Index `records` — typically the whole tape, decoded once.
    pub fn new(records: &'a [EffectRecord]) -> KeyedView<'a> {
        let mut index: BTreeMap<(&'a str, &'a [u8]), Vec<KeyedReply<'a>>> = BTreeMap::new();
        let mut position = 0usize;
        while position < records.len() {
            if let EffectRecord::Exchange {
                door,
                request,
                reply,
            } = &records[position]
            {
                index
                    .entry((door.as_str(), request.as_slice()))
                    .or_default()
                    .push(KeyedReply {
                        position,
                        reply: reply.as_slice(),
                    });
            }
            position = position.saturating_add(1);
        }
        KeyedView { index }
    }

    /// The LATEST exchange through `door` with exactly `request` whose
    /// position is at or before `at_or_before` (an index into the slice the
    /// view was built from; anything past its end means "anywhere").
    ///
    /// "At or before" is the whole point: a reader standing at record N may use
    /// what the recorded program had learned by then, never what it learned
    /// later — an answer from the future would let a re-decision see a world
    /// the recorded one had not reached. A miss is
    /// [`ReplayDivergence::Unrecorded`], never a reason to ask the live world.
    ///
    /// The bound is inclusive, and [`ReplaySeam::position`](crate::ReplaySeam::position)
    /// is not a consumed record: it is the index of the NEXT one. A reader
    /// standing where a replay over the same slice has got to passes the index
    /// of the last record that replay consumed (`position() - 1`, or a mark's
    /// own index); passing `position()` itself would admit the very record
    /// the replay is about to read.
    pub fn lookup(
        &self,
        door: &str,
        request: &[u8],
        at_or_before: usize,
    ) -> Result<KeyedReply<'a>, ReplayDivergence> {
        // `self.index` is keyed by borrows of the tape (lifetime 'a); the query
        // borrows the caller's door and request, which may be shorter-lived.
        // BTreeMap is covariant in its key, so the map is read here at the
        // query's lifetime without copying either side.
        let index: &BTreeMap<(&str, &[u8]), Vec<KeyedReply<'a>>> = &self.index;
        let hits: &[KeyedReply<'a>] = match index.get(&(door, request)) {
            Some(v) => v.as_slice(),
            None => &[],
        };
        let upto = hits.partition_point(|hit| keyed_hit_within_cutoff(hit.position, at_or_before));
        let found = upto.checked_sub(1).and_then(|last| hits.get(last)).copied();
        match found {
            Some(hit) => Ok(hit),
            None => Err(ReplayDivergence::Unrecorded {
                door: door.to_string(),
                request: request.to_vec(),
            }),
        }
    }

    /// How many exchanges the view indexed.
    pub fn len(&self) -> usize {
        let mut total: usize = 0;
        for hits in self.index.values() {
            total = total.saturating_add(hits.len());
        }
        total
    }

    /// Whether the tape held no exchange at all.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}
