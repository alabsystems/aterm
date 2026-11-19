// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One hold episode ([`Episode`]) and the clock that keeps it across looks ([`EpisodeClock`]).
//!
//! Clocks count episodes, not words: a wait whose word changes, or whose holding process is
//! another one, is the same wait, and a clear gap shorter than [`CLEAR_GAP_S`] does not end it.
//! Restarting on every word change is how a wait outlived every horizon it had.

use std::time::Duration;

/// The shortest clear gap that ends an episode, in seconds: a hold that clears and comes back
/// within it is the same episode.
pub const CLEAR_GAP_S: u64 = 5 * 60;

/// One hold episode, in seconds since the Unix epoch: when it began, and the horizon at which its
/// act is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Episode {
    /// The first look in this episode that found a hold.
    pub since: u64,
    /// `since` plus the shortest horizon of any hold seen in this episode: a later hold's longer
    /// horizon never extends it.
    pub horizon_at: u64,
}

/// The episode, if any, across looks: each look says a hold was found ([`Self::held`]) or not
/// ([`Self::clear`]), and the caller's lane says when progress ends it ([`Self::progress`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EpisodeClock {
    episode: Option<Episode>,
    /// The first look since the last hold that found none.
    clear_since: Option<u64>,
}

impl EpisodeClock {
    /// A look at `now` that found a hold whose horizon is `horizon`, whatever its word or holding
    /// process. It continues the episode unless that episode's hold has been clear for
    /// [`CLEAR_GAP_S`], and never moves its `since` later or its `horizon_at` later.
    pub fn held(&mut self, now: u64, horizon: Duration) -> Episode {
        let horizon = horizon.as_secs();
        let continues = self
            .clear_since
            .is_none_or(|clear| now.saturating_sub(clear) < CLEAR_GAP_S);
        let episode = match self.episode {
            Some(episode) if continues => Episode {
                since: episode.since,
                horizon_at: episode
                    .horizon_at
                    .min(episode.since.saturating_add(horizon)),
            },
            _ => Episode {
                since: now,
                horizon_at: now.saturating_add(horizon),
            },
        };
        *self = Self {
            episode: Some(episode),
            clear_since: None,
        };
        episode
    }

    /// A look at `now` that found no hold. The episode stands through a clear gap shorter than
    /// [`CLEAR_GAP_S`] and is over at the first look that finds it clear for that long.
    pub fn clear(&mut self, now: u64) {
        let Some(clear) = self.episode.map(|_| *self.clear_since.get_or_insert(now)) else {
            return;
        };
        if now.saturating_sub(clear) >= CLEAR_GAP_S {
            *self = Self::default();
        }
    }

    /// A progress event ends the episode. Which events are progress is the lane's (the design
    /// names a phase change, a move, a person's key, and a person's or progressing work's hold
    /// lapsing); a change of word or of holding process is not one.
    pub fn progress(&mut self) {
        *self = Self::default();
    }

    /// The episode standing now, a clear gap shorter than [`CLEAR_GAP_S`] included.
    #[must_use]
    pub const fn current(&self) -> Option<Episode> {
        self.episode
    }
}

#[cfg(test)]
#[path = "episode_tests.rs"]
mod tests;
