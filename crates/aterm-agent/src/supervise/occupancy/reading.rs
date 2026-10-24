// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MACHINE'S OCCUPANCY AS THE HOST READS IT FOR ONE SESSION.

use crate::harness::disk::Level;
use crate::supervise::memory::Pressure;

/// The board's disk-look cadence, shared by its producer and cached readers.
pub const LOOK_EVERY_MS: i64 = 60_000;

/// A reading older than two board looks no longer speaks for now.
pub const STALE_AFTER_MS: i64 = 2 * LOOK_EVERY_MS;

/// The build slots as the board last proved them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotsSeen {
    /// Proven free, less those discounted.
    pub usable: u16,
    /// Slot files present.
    pub n: u16,
}

/// The board as one session's host reads it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OccupancyReading {
    /// When the board was taken.
    pub taken_ms: i64,
    /// The board's snapshot is older than two looks, or there is none.
    pub stale: bool,
    /// Memory the machine can hand out without paging, MiB.
    pub avail_mib: Option<u64>,
    /// The kernel's memory pressure, where read.
    pub pressure: Option<Pressure>,
    /// The home volume's level on the disk watch, where judged.
    pub disk: Option<Level>,
    /// The build slots, where read.
    pub slots: Option<SlotsSeen>,
    /// How long `hold-room`'s slot term has held on the board's looks.
    pub slots_free_ms: Option<u64>,
    /// `hold-room`'s machine terms at the look.
    pub room: Option<bool>,
    /// The last look at which a build slot's holder lay in this session's process tree, unix ms.
    pub held_slot_last_ms: Option<i64>,
}

impl OccupancyReading {
    /// Age a retained reading against the current Unix millisecond clock.
    #[must_use]
    pub fn aged_at(mut self, now_ms: Option<i64>) -> Self {
        self.stale |= now_ms.is_none_or(|now| now.saturating_sub(self.taken_ms) > STALE_AFTER_MS);
        self
    }

    /// Whether the reading speaks for now: a board taken from a snapshot no older than two looks.
    #[must_use]
    pub const fn fresh(&self) -> bool {
        self.taken_ms > 0 && !self.stale
    }

    /// Whether the reading's figures may be typed at `at_ms` (unix).
    #[must_use]
    pub fn typeable_at(&self, at_ms: Option<i64>) -> bool {
        self.fresh()
            && at_ms.is_some_and(|at| {
                at.saturating_sub(self.taken_ms) <= crate::supervise::facts::FACT_FRESH_MS
            })
    }

    /// Whether a slot holder of this session's was seen within the last `held_ms` before the board
    /// was taken.
    #[must_use]
    pub fn held_slot_within(&self, held_ms: i64) -> bool {
        self.held_slot_last_ms
            .is_some_and(|at| self.taken_ms > 0 && self.taken_ms.saturating_sub(at) <= held_ms)
    }
}
