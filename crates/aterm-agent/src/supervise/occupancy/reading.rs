// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MACHINE'S OCCUPANCY AS THE HOST READS IT FOR ONE SESSION
//! (`docs/DESIGN-harness-occupancy-2026-10-20.md` §2, §4.2; the red-team's F15): what
//! `IdleHost::occupancy` answers, from the window's facts board taken once a disk look.

use crate::harness::disk::Level;
use crate::supervise::memory::Pressure;

/// The board's disk-look cadence, shared by its producer and cached readers.
pub const LOOK_EVERY_MS: i64 = 60_000;

/// A reading older than two board looks no longer speaks for now.
pub const STALE_AFTER_MS: i64 = 2 * LOOK_EVERY_MS;

/// The build slots as the board last proved them: those proven free less those an orphaned build
/// still fills, and the slot files present (`slots=<usable>/<n>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotsSeen {
    /// Proven free, less those discounted.
    pub usable: u16,
    /// Slot files present.
    pub n: u16,
}

/// The board as one session's host reads it ([module docs](self)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OccupancyReading {
    /// When the board was taken, unix ms; 0 for none (the empty reading).
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
    /// How long `hold-room`'s slot term has held on the board's looks, ms: `Some` once that spans
    /// its `for=`, `None` before and whenever a look broke it.
    pub slots_free_ms: Option<u64>,
    /// `hold-room`'s machine terms at the look: `None` where the board could not judge them (a term
    /// this host does not measure, no snapshot).
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

    /// Whether a slot holder of this session's was seen within the last `held_ms` before the board
    /// was taken: a hold that has held `held_ms` saw it during the hold.
    #[must_use]
    pub fn held_slot_within(&self, held_ms: i64) -> bool {
        self.held_slot_last_ms
            .is_some_and(|at| self.taken_ms > 0 && self.taken_ms.saturating_sub(at) <= held_ms)
    }
}
