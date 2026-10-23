// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE K CLOCK (`docs/DESIGN-harness-occupancy-2026-10-20.md` §5, "a due line lost to a
//! pre-empting writer"; the slice-2 plan's D2): how many looks a due line has gone untold, and
//! the one raise past [`K_LOOKS`] that becomes the tab's `ready-idle` Entry.
//!
//! Pure and clockless, generic over the key (the loop keys it by a hold rule's episode, the
//! window's host by a wake's). A key's clock starts the first time it is seen due and runs while
//! it stays due, whichever guard holds it: a guard that changes its word never restarts it. A key
//! no longer due (told, lapsed, closed) is forgotten. A key held by a person never raises, and
//! one whose holder passes from a person to another guard past K raises at once.

use std::collections::BTreeMap;

use super::reading::LOOK_EVERY_MS;
use crate::supervise::Guard;

/// Looks a due line stands untold before it is the tab's `ready-idle` Entry.
pub const K_LOOKS: i64 = 10;

/// ONE KEY'S CLOCK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Due {
    /// When it was first seen due, unix ms (or seeded from a predecessor's rows).
    pub since_ms: i64,
    /// The first guard seen holding it; `None` while nothing held it (due, its door not open).
    pub first: Option<Guard>,
    /// The guard holding it at the last look.
    pub last: Option<Guard>,
    /// It has raised: it stays raised until it is no longer due.
    pub raised: bool,
}

/// A KEY PAST K, raised: what the Entry carries (closed words and numbers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyIdle<K> {
    /// The key.
    pub key: K,
    /// The guard holding it at the last look; `None` for a line due with its door shut.
    pub guard: Option<Guard>,
    /// When it was first seen due, unix ms.
    pub since_ms: i64,
}

/// THE CLOCKS OF THE KEYS DUE NOW ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueClock<K: Ord + Copy> {
    keys: BTreeMap<K, Due>,
}

impl<K: Ord + Copy> Default for DueClock<K> {
    fn default() -> Self {
        Self {
            keys: BTreeMap::new(),
        }
    }
}

impl<K: Ord + Copy> DueClock<K> {
    /// A successor's clock for `key` from its predecessor's rows: due since `since_ms` (the
    /// earliest of the episode's held or would rows), kept by the next [`Self::observe`] that
    /// names it as due. An earlier seed or clock stands.
    pub fn seed(&mut self, key: K, since_ms: i64) {
        use super::fault::{Fault, armed};
        if armed(Fault::KUnseeded) {
            return;
        }
        let due = self.keys.entry(key).or_insert(Due {
            since_ms,
            first: None,
            last: None,
            raised: false,
        });
        due.since_ms = due.since_ms.min(since_ms);
    }

    /// THE KEYS DUE AT `now_ms`, each with the guard holding it: a new key's clock starts now,
    /// a known one's runs on whatever its guard, and a key not named is forgotten.
    pub fn observe(&mut self, due: &[(K, Option<Guard>)], now_ms: i64) {
        use super::fault::{Fault, armed};
        self.keys
            .retain(|key, _| due.iter().any(|(named, _)| named == key));
        for &(key, guard) in due {
            let clock = self.keys.entry(key).or_insert(Due {
                since_ms: now_ms,
                first: guard,
                last: guard,
                raised: false,
            });
            if guard != clock.last && armed(Fault::KFromLastWord) {
                clock.since_ms = now_ms;
            }
            clock.first = clock.first.or(guard);
            clock.last = guard;
        }
    }

    /// How many whole looks `key` has stood due at `now_ms`; `None` for a key not due.
    #[must_use]
    pub fn looks(&self, key: &K, now_ms: i64) -> Option<i64> {
        self.keys
            .get(key)
            .map(|due| now_ms.saturating_sub(due.since_ms).max(0) / LOOK_EVERY_MS)
    }

    /// `key`'s clock.
    #[must_use]
    pub fn get(&self, key: &K) -> Option<&Due> {
        self.keys.get(key)
    }

    /// THE KEYS RAISED AT `now_ms`: each key that has stood due `k` looks or more raises once,
    /// unless a person holds it now, and stays raised while it stays due. Answers every key
    /// raised, lowest key first.
    pub fn sample(&mut self, k: i64, now_ms: i64) -> Vec<ReadyIdle<K>> {
        use super::fault::{Fault, armed};
        let late = i64::from(armed(Fault::LateRaise));
        let mut raised = Vec::new();
        for (key, due) in &mut self.keys {
            let looks = now_ms.saturating_sub(due.since_ms).max(0) / LOOK_EVERY_MS;
            if !due.raised && looks >= k + late && due.last != Some(Guard::Person) {
                due.raised = true;
            }
            if due.raised {
                raised.push(ReadyIdle {
                    key: *key,
                    guard: due.last,
                    since_ms: due.since_ms,
                });
            }
        }
        raised
    }
}

#[cfg(test)]
#[path = "due_tests.rs"]
mod tests;
