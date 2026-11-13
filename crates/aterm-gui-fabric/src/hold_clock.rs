// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Aggregate hold transitions retained between observers' reads.

use std::time::Instant;

/// One session's aggregate hold, read with its release history under the fabric lock.
/// It grants no drive authority; a task decision can use it to reject an outdated read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoldObservation {
    held: bool,
    generation: Option<u64>,
    released_at: Option<Instant>,
}

impl HoldObservation {
    /// Whether any origin holds the session.
    #[must_use]
    pub fn is_held(self) -> bool {
        self.held
    }

    /// When the aggregate last changed from held to clear, including a release between reads.
    #[must_use]
    pub fn released_at(self) -> Option<Instant> {
        self.released_at
    }

    /// Whether an unheld decision made from `observed` still has the same hold history.
    /// A checked generation that exhausted its range proves no continuity, even to itself.
    #[must_use]
    pub fn agrees_unheld(self, observed: Self) -> bool {
        !self.held && self.generation.is_some() && self == observed
    }
}

/// The history beside the actual holds. The aggregate boolean is derived from those holds,
/// so no second copy can disagree with the map a transaction changes.
pub(super) struct HoldHistory {
    generation: Option<u64>,
    released_at: Option<Instant>,
}

impl Default for HoldHistory {
    fn default() -> Self {
        Self {
            generation: Some(0),
            released_at: None,
        }
    }
}

impl HoldHistory {
    pub(super) fn observation(&self, held: bool) -> HoldObservation {
        HoldObservation {
            held,
            generation: self.generation,
            released_at: self.released_at,
        }
    }

    /// Called under the hold map's lock after its mutation, with the aggregate on both sides.
    /// Slot/reason changes and direct resets are not aggregate transitions.
    pub(super) fn moved(&mut self, before: bool, after: bool, now: Instant) {
        if before == after {
            return;
        }
        self.generation = self.generation.and_then(|value| value.checked_add(1));
        if !after {
            self.released_at = Some(now);
        }
    }
}

#[cfg(test)]
#[path = "hold_clock_tests.rs"]
mod tests;
