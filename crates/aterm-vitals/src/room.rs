// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Processes Room: the fullest readable bounded pair of the current task-table reading.
//! One model-owned latch carries the 90% raise / below-85% clear rule between readings.
//! No count or capacity is retained when the current reading cannot supply a pair.

use crate::model::TaskRoom;

/// The task-table limit of a Room reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// The machine's process limit.
    Processes,
    /// This user's effective process limit, already resolved by the native reader.
    UserProcesses,
    /// The machine's thread limit.
    Threads,
    /// The machine's open-file limit.
    Files,
}

/// One current binding pair and the model's hysteresis judgment of it. The fullest pair is
/// chosen only among readable bounded pairs; it says nothing of any unread or unlimited pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    limit: Limit,
    count: u64,
    cap: u64,
    near: bool,
}

impl Reading {
    /// Which limit binds among the current readable bounded pairs.
    #[must_use]
    pub const fn limit(self) -> Limit {
        self.limit
    }

    /// The current count against the binding limit, including a readable zero.
    #[must_use]
    pub const fn count(self) -> u64 {
        self.count
    }

    /// The binding limit, always positive.
    #[must_use]
    pub const fn cap(self) -> u64 {
        self.cap
    }

    /// The binding pair's share for presentation, which may exceed one. Selection and warning
    /// thresholds use exact integer arithmetic rather than this rounded representation.
    #[must_use]
    pub fn share(self) -> f64 {
        self.count as f64 / self.cap as f64
    }

    /// Whether Room raised at 90% and has not since read below 85%.
    #[must_use]
    pub const fn near(self) -> bool {
        self.near
    }
}

/// The one Room latch of a running model. An unread reading publishes no pair and leaves the
/// latch unchanged: it is neither evidence of a near limit nor evidence that one cleared.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    near: bool,
}

impl Tracker {
    pub(crate) fn absorb(&mut self, room: TaskRoom) -> Option<Reading> {
        let (limit, count, cap) = binding(room)?;
        let threshold = if self.near { 85 } else { 90 };
        self.near = u128::from(count) * 100 >= u128::from(cap) * threshold;
        Some(Reading {
            limit,
            count,
            cap,
            near: self.near,
        })
    }
}

fn binding(room: TaskRoom) -> Option<(Limit, u64, u64)> {
    let mut best = None::<(Limit, u64, u64)>;
    // A tie keeps this fixed order. Native readers represent unknown or unlimited caps as
    // None; a malformed zero cap cannot bind either. Large finite caps remain valid.
    for (limit, count, cap) in [
        (Limit::Processes, room.procs, room.procs_max),
        (Limit::UserProcesses, room.user_procs, room.user_procs_max),
        (Limit::Threads, room.threads, room.threads_max),
        (Limit::Files, room.files, room.files_max),
    ] {
        let (Some(count), Some(cap)) = (count, cap.filter(|cap| *cap > 0)) else {
            continue;
        };
        if best.is_none_or(|(_, other_count, other_cap)| {
            u128::from(count) * u128::from(other_cap) > u128::from(other_count) * u128::from(cap)
        }) {
            best = Some((limit, count, cap));
        }
    }
    best
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
