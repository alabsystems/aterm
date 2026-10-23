// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tracks how many looks a due line has gone untold and raises [`ReadyIdle`] past [`K_LOOKS`].

use std::collections::BTreeMap;

use super::reading::LOOK_EVERY_MS;
use crate::supervise::Guard;

/// Looks a due line stands untold before it is the tab's `ready-idle` Entry.
pub const K_LOOKS: i64 = 10;

/// Per-key clock tracking how long a due line has stood untold.
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

/// A key that has stood due past `k` looks and raised its `ready-idle` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyIdle<K> {
    /// The key.
    pub key: K,
    /// The guard holding it at the last look; `None` for a line due with its door shut.
    pub guard: Option<Guard>,
    /// When it was first seen due, unix ms.
    pub since_ms: i64,
}

/// Clocks of the keys currently due.
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
    /// Seeds `key`'s clock from predecessor ledger rows if not already tracked.
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

    /// Updates clocks for the keys due at `now_ms` and forgets keys no longer due.
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

    /// Returns keys due for at least `k` looks at `now_ms` that are not held by a person.
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
