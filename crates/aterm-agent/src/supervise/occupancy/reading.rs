// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MACHINE'S OCCUPANCY AS THE HOST READS IT FOR ONE SESSION
//! (`docs/DESIGN-harness-occupancy-2026-10-20.md` §2, §4.2; the red-team's F15): what
//! `IdleHost::occupancy` answers, from the window's facts board taken once a disk look. Plain
//! numbers and closed words, never a path, a name or a process's text, so a line built from it
//! types nothing anyone else wrote.
//!
//! The board judges what needs the predicate grammar or more than one look, so the loop, which
//! is not called while the agent's turn runs, carries nothing across a gap it never saw:
//!
//! * [`OccupancyReading::slots_free_ms`] is `hold-room`'s slot term, [`super::rules::HOLD_ROOM_SLOTS`]
//!   (`slots>=1 for=20m`), latched over the board's own looks by their stamps;
//! * [`OccupancyReading::room`] is its machine terms, [`super::rules::HOLD_ROOM_MACHINE`], judged at
//!   the look;
//! * [`OccupancyReading::held_slot_last_ms`] is the last look at which a build slot's holder lay
//!   in this session's process tree, kept per session on the board look after look.
//!
//! [`OccupancyReading::default`] is THE EMPTY READING: no board (`taken_ms` 0), never fresh, so
//! no rule that needs the machine fires on it. A host that reads nothing answers it, and so does
//! a replay of a tape whose recording build had no `host.occupancy` door (`tape::host`).

use crate::harness::disk::Level;
use crate::supervise::memory::Pressure;

/// The build slots as the board last proved them: those proven free less those an orphaned
/// build still fills, and the slot files present (`slots=<usable>/<n>`).
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
    /// How long `hold-room`'s slot term has held on the board's looks, ms: `Some` once that
    /// spans its `for=`, `None` before and whenever a look broke it.
    pub slots_free_ms: Option<u64>,
    /// `hold-room`'s machine terms at the look: `None` where the board could not judge them (a
    /// term this host does not measure, no snapshot).
    pub room: Option<bool>,
    /// The last look at which a build slot's holder lay in this session's process tree, unix
    /// ms; `None` for none seen.
    pub held_slot_last_ms: Option<i64>,
}

impl OccupancyReading {
    /// Whether the reading speaks for now: a board taken from a snapshot no older than two
    /// looks. The empty reading never does.
    #[must_use]
    pub const fn fresh(&self) -> bool {
        self.taken_ms > 0 && !self.stale
    }

    /// Whether a slot holder of this session's was seen within the last `held_ms` before the
    /// board was taken: a hold that has held `held_ms` saw it during the hold. The board is at
    /// most a look old when it is asked, so this may count a holder seen up to a look before
    /// the hold began, never one seen after it.
    #[must_use]
    pub fn held_slot_within(&self, held_ms: i64) -> bool {
        self.held_slot_last_ms
            .is_some_and(|at| self.taken_ms > 0 && self.taken_ms.saturating_sub(at) <= held_ms)
    }
}
