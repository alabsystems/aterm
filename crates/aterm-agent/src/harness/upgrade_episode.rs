// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The clock an upgrade's hold counts on ([`Episode`]), and the first sighting that dates a wall
//! no transcript row dates ([`sighted`]).
//!
//! A hold is one episode from its first look until a progress event ends it: a move or a phase
//! change, a person's key, a new target build, or a clear stretch that lasts [`CLEAR_GAP_S`]. A
//! change of the word that names the hold, or of the process that holds it, is no progress: the
//! episode keeps its start. A clear look is not progress either until another clear look at
//! least [`CLEAR_GAP_S`] after the first proves the stretch that long, so background children
//! that roll one after another with short gaps between them are one hold, and a single clear
//! look between two holds never moves a horizon later.

/// How long a hold must have been clear, as two clear looks prove it, before its episode ends
/// ([`Episode::clear`]).
pub(in crate::harness) const CLEAR_GAP_S: u64 = 5 * 60;

/// One hold, counted from its first look across changes of word and process and across clear
/// gaps shorter than [`CLEAR_GAP_S`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::harness) struct Episode {
    /// When the episode began: the first look that found the hold after the last progress
    /// event; 0 for none.
    pub(in crate::harness) since: u64,
    /// The first clear look since the episode's last hold; 0 while it holds.
    pub(in crate::harness) clear_at: u64,
}

impl Episode {
    /// An episode begun at `since`, held at its last look.
    pub(in crate::harness) const fn from(since: u64) -> Episode {
        Episode { since, clear_at: 0 }
    }

    /// A look at `now` that finds the hold: it begins the episode where none stands, and keeps
    /// the start of one that does whatever word or process holds it now.
    pub(in crate::harness) const fn hold(&mut self, now: u64) {
        if self.since == 0 {
            self.since = now;
        }
        self.clear_at = 0;
    }

    /// A look at `now` that finds nothing holding: the first marks where the clear stretch began,
    /// and one at least [`CLEAR_GAP_S`] after it ends the episode, a gap that long being progress.
    pub(in crate::harness) const fn clear(&mut self, now: u64) {
        if self.since == 0 {
            return;
        }
        if self.clear_at == 0 {
            self.clear_at = now;
        } else if now.saturating_sub(self.clear_at) >= CLEAR_GAP_S {
            *self = Episode::from(0);
        }
    }

    /// A progress event: the episode ends, and the next hold begins another.
    pub(in crate::harness) const fn end(&mut self) {
        *self = Episode::from(0);
    }

    /// How long the episode has run at `now`; 0 with none.
    pub(in crate::harness) const fn age(self, now: u64) -> u64 {
        if self.since == 0 {
            0
        } else {
            now.saturating_sub(self.since)
        }
    }
}

/// The hold's episode a record read back carries: the one it recorded (`recorded`), else, in a
/// record written before the episode was kept, one begun when its standing wait's word began
/// (`word_since`) where that word holds the round (`holds`), the earliest start such a record
/// proves of the hold standing across the update; none where its word holds nothing.
#[must_use]
pub(in crate::harness) const fn read_back(
    recorded: Option<Episode>,
    word_since: u64,
    holds: bool,
) -> Episode {
    match recorded {
        Some(episode) => episode,
        None if holds => Episode::from(word_since),
        None => Episode::from(0),
    }
}

/// The first sighting of a wall as a look at `now` reads it (`stands`): the sighting kept
/// (`seen`) while the wall stands, `now` for one first seen here, and 0 once a look finds no wall.
/// A wall no transcript row dates (one only the screen shows, one whose transcript cannot be read,
/// or one whose key record cannot be read) ages from it.
#[must_use]
pub(in crate::harness) const fn sighted(seen: u64, stands: bool, now: u64) -> u64 {
    match (stands, seen) {
        (false, _) => 0,
        (true, 0) => now,
        (true, seen) => seen,
    }
}

#[cfg(test)]
#[path = "upgrade_episode_tests.rs"]
mod tests;
