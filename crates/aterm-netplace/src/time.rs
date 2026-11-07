// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::cmp::Ordering;

/// Forward flip points recorded by the comparisons the decision actually uses.
/// Ordinary evaluations select `TRACK = false` and do not project future decisions.
pub(super) struct Clock<const TRACK: bool> {
    pub(super) now: u64,
    next: Cell<Option<u64>>,
}

impl<const TRACK: bool> Clock<TRACK> {
    pub(super) const fn at(now: u64) -> Self {
        Self {
            now,
            next: Cell::new(None),
        }
    }

    fn watch(&self, candidate: Option<u64>) {
        if TRACK && let Some(at) = candidate.filter(|at| *at > self.now) {
            self.next
                .set(Some(self.next.get().map_or(at, |held| held.min(at))));
        }
    }

    pub(super) fn next(&self) -> Option<u64> {
        self.next.get()
    }

    pub(super) fn admits(&self, at: u64, ahead: u64) -> bool {
        let admitted = at <= self.now.saturating_add(ahead);
        if !admitted {
            self.watch(Some(at.saturating_sub(ahead)));
        }
        admitted
    }

    pub(super) fn older_than(&self, at: u64, age: u64) -> bool {
        let older = self.now.saturating_sub(at) > age;
        if !older {
            self.watch(at.checked_add(age).and_then(|at| at.checked_add(1)));
        }
        older
    }

    pub(super) fn at_least(&self, at: u64, age: u64) -> bool {
        let due = self.now.saturating_sub(at) >= age;
        if !due {
            self.watch(at.checked_add(age));
        }
        due
    }

    pub(super) fn cmp_now(&self, at: u64) -> Ordering {
        let order = self.now.cmp(&at);
        self.watch(match order {
            Ordering::Less => Some(at),
            Ordering::Equal => at.checked_add(1),
            Ordering::Greater => None,
        });
        order
    }
}
