// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pressure-pass cadence shared by the GUI and standalone automatic watches.
//!
//! A pass that freed nothing is not run again hot. `low` and `short` are one family: each pass in
//! it that frees nothing doubles the wait before the next one, whichever of the two it ran at, up
//! to the level's cap, so free space wandering across the floor never calls a pass a look. A pass
//! that freed something, or free space that fell by [`EMERGENCY_DROP`] since the last pass,
//! brings the level's base interval back. Between passes, a fall of a quarter of the volume's
//! room (at `short` at least [`EMERGENCY_DROP`]) calls one at once, and so do a move from `low`
//! to `short` and an agent's poke while the passes are not backing off; a move from `short` to
//! `low` never does. An emergency pass that freed nothing waits for free space to fall by
//! [`EMERGENCY_DROP`] or for [`PASS_EVERY`] (and, where it wrote `cannot-meet`, for
//! [`Emergency::due`]).

use std::time::{Duration, Instant};

use super::{Held, Level, Waits};

/// Ordinary pressure-pass interval, and a `short` volume's base interval.
pub const PASS_EVERY: Duration = Duration::from_secs(10 * 60);
/// A fresh emergency drop that permits another otherwise fruitless pass.
pub const EMERGENCY_DROP: u64 = 512 << 20;
/// Initial interval for a low, unproductive volume.
pub const LOW_EVERY: Duration = Duration::from_secs(3600);
/// Maximum low-volume interval.
pub const LOW_EVERY_MAX: Duration = Duration::from_secs(8 * 3600);
/// Maximum interval for a `short` volume whose passes free nothing.
pub const SHORT_EVERY_MAX: Duration = Duration::from_secs(2 * 3600);
/// The most build locks a pass keeps to probe for their release: each probe takes a lock a
/// moment, so the count bounds what a watch's tick does while it waits.
pub const HELD_PROBES: usize = 16;

/// The last automatic pressure pass; these facts change scheduling, never removal authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    /// When it started.
    pub at: Instant,
    /// Measured free bytes before it.
    pub free: Option<u64>,
    /// An emergency pass that could make no progress.
    pub fruitless: bool,
    /// Its measured level.
    pub level: Level,
    /// The wait before the next pass ([`every`]): at `low` and `short` it doubles while passes
    /// free nothing.
    pub every: Duration,
}

/// Whether `level` is one of the backing-off family, `low` and `short`.
const fn backs_off(level: Level) -> bool {
    matches!(level, Level::Low | Level::Short)
}

/// The base and the cap of a backing-off level's wait.
const fn waits(level: Level) -> (Duration, Duration) {
    match level {
        Level::Low => (LOW_EVERY, LOW_EVERY_MAX),
        _ => (PASS_EVERY, SHORT_EVERY_MAX),
    }
}

/// Whether the current real reading calls for a pressure pass.
#[must_use]
pub fn due(
    last: &mut Option<Pass>,
    now: Instant,
    level: Level,
    free: Option<u64>,
    poked: bool,
    room: u64,
) -> bool {
    if matches!(level, Level::Unknown | Level::Ok) {
        return false;
    }
    let Some(pass) = last.as_mut() else {
        return true;
    };
    if level != Level::Emergency {
        pass.fruitless = false;
    }
    let since = now.saturating_duration_since(pass.at);
    let fell_by = |drop: u64| matches!((pass.free, free), (Some(was), Some(is)) if is.saturating_add(drop) < was);
    if backs_off(level) && backs_off(pass.level) {
        // A poke, or a move from `low` to `short`, calls a pass at once only while the passes
        // are not backing off: one that freed nothing finds nothing new because a session ran
        // short or the floor was crossed by a few bytes. A move to `low` calls none.
        let backing = pass.every > waits(pass.level).0;
        let drop = if level == Level::Short {
            EMERGENCY_DROP.max(room / 4)
        } else {
            room / 4
        };
        return since >= pass.every.min(waits(level).1)
            || fell_by(drop)
            || (!backing && (poked || level > pass.level));
    }
    if since >= PASS_EVERY {
        return true;
    }
    if pass.fruitless {
        return fell_by(EMERGENCY_DROP);
    }
    poked || level == Level::Emergency || pass.level < Level::Short
}

/// The wait after a pass at `level` that measured `free` bytes before it and `freed` something
/// or not, the last one `before`: at `low` and `short`, twice the last wait, kept between the
/// level's base and cap ([`LOW_EVERY`] to [`LOW_EVERY_MAX`], [`PASS_EVERY`] to
/// [`SHORT_EVERY_MAX`]), where the last pass was at `low` or `short` too, this one freed nothing
/// and free space did not fall by [`EMERGENCY_DROP`] since the last; else the level's base.
#[must_use]
pub fn every(level: Level, freed: bool, before: Option<&Pass>, free: Option<u64>) -> Duration {
    if !backs_off(level) {
        return PASS_EVERY;
    }
    let (base, max) = waits(level);
    let fell = |last: &Pass| matches!((last.free, free), (Some(was), Some(is)) if is.saturating_add(EMERGENCY_DROP) <= was);
    match before {
        Some(last) if !freed && backs_off(last.level) && !fell(last) => {
            last.every.saturating_mul(2).clamp(base, max)
        }
        _ => base,
    }
}

/// Record a pass after it actually ran.
pub fn ran(
    last: &mut Option<Pass>,
    at: Instant,
    free: Option<u64>,
    fruitless: bool,
    level: Level,
    freed: bool,
) {
    let every = every(level, freed, last.as_ref(), free);
    *last = Some(Pass {
        at,
        free,
        fruitless,
        level,
        every,
    });
}

/// The longest, in seconds, an emergency pass that could not meet its line waits on the clock
/// ([`Unmet::ripens`]): the managed-cache sweep's wait under pressure
/// (`atpkg::build_cache::RECLAIM_STALE_UNDER_PRESSURE`). That sweep keeps a stale part of a live
/// build directory for an hour's writes, and neither that hold nor a process at work inside a
/// build directory reaches what a pass reports it waits on ([`Waits`]); so a keeper the pass
/// cannot see is asked again within the sweep's own window, never left to keep the wait for
/// good. It is a bound on what may ripen unseen, not a retry on the interval.
pub const UNMET_WAIT_MAX_S: i64 = atpkg::build_cache::RECLAIM_STALE_UNDER_PRESSURE.as_secs() as i64;

/// What an emergency pass that could not meet the line waits for before another is planned
/// ([`Emergency::due`]), in place of the [`PASS_EVERY`] retry: free space moving, an agent's
/// wait for room, or what kept the pass's rows moving ([`Unmet::moved`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmet {
    /// The free bytes the pass ended at.
    pub free: u64,
    /// The unix second the wait ends on the clock: the earliest second something the pass's
    /// cycle kept for its age may go, and never more than [`UNMET_WAIT_MAX_S`] after the pass.
    pub ripens: i64,
    /// The locks a build held over what the pass's cycle would take (at most [`HELD_PROBES`]).
    pub held: Vec<Held>,
    /// The live sessions' digest the pass ran against (`super::budget::live_digest`).
    pub live: u64,
}

impl Unmet {
    /// Whether what kept the pass's rows moved by unix second `now`: the clock reached
    /// [`Self::ripens`], a held lock was let go (each probed a moment), or the live sessions read
    /// as `live()` differ. `live()` is `None` where they could not be read, which moves nothing.
    #[must_use]
    pub fn moved(&self, now: i64, live: impl FnOnce() -> Option<u64>) -> bool {
        now >= self.ripens
            || self.held.iter().any(Held::released)
            || live().is_some_and(|live| live != self.live)
    }
}

/// Whether a look that read `level` and `free` bytes free is out of the emergency whose line is
/// `line`: off it, and [`EMERGENCY_DROP`] or more over the line. Free space hovering at the line
/// is one episode, not one each time it crosses; a level an inode count holds at the emergency
/// is not out of it, whatever the bytes.
#[must_use]
pub fn over(level: Level, free: Option<u64>, line: u64) -> bool {
    level != Level::Emergency && free.is_some_and(|f| f >= line.saturating_add(EMERGENCY_DROP))
}

/// One look's reading, as [`Emergency::due`] weighs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// When the look began.
    pub at: Instant,
    /// The same moment, unix seconds.
    pub unix: i64,
    /// The level the pass would run at (a standing projection reads short).
    pub level: Level,
    /// The free bytes measured.
    pub free: Option<u64>,
    /// The volume's room, bytes.
    pub room: u64,
    /// The watch was poked since its last look.
    pub poked: bool,
    /// An agent's wait for room (`await sys`) the pass adopts: an ask, so a move.
    pub demand: bool,
}

/// THE EMERGENCY'S WAIT, one a watch (DC-06a), which both hosts ask at every look
/// ([`Self::due`]) and hand how each pass ended (`crate::harness::cli::disk_emergency_end`,
/// which keeps the memo and writes the row). An emergency pass that could not meet its line
/// waits ([`Self::unmet`]) for something to move, in place of the [`PASS_EVERY`] retry. The pages
/// of a discovery cycle that took nothing gather what each waited on ([`Self::page`]), so the
/// page that closes the cycle waits on every page's, never on its own alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Emergency {
    /// The pass that could not meet its line, and what it waits for.
    pub unmet: Option<Unmet>,
    /// What the pages of the cycle under way waited on, each an emergency page that took nothing.
    pub gathered: Waits,
}

impl Emergency {
    /// No wait and nothing gathered, as a watch starts (a `static`'s initializer).
    pub const NONE: Self = Self {
        unmet: None,
        gathered: Waits {
            ripens: None,
            held: Vec::new(),
        },
    };

    /// Whether the look `look` runs a pressure pass. Where a wait stands, only once something
    /// moved: an agent's wait for room, free space by [`EMERGENCY_DROP`] either way or no longer
    /// read, or [`Unmet::moved`] at `look.unix` (the clock, a lock, the live sessions `live`
    /// reads). What moved clears the wait; at the emergency the pass is then due at once, and off
    /// it the ordinary cadence ([`due`]) decides, so a look out of the emergency is never handed
    /// a pass its level would not run. With no wait, [`due`] decides, and an agent's wait is
    /// always due. The caller has read the look for the episode's end first
    /// (`crate::harness::cli::disk_emergency_look`).
    pub fn due(
        &mut self,
        last: &mut Option<Pass>,
        look: &Look,
        live: impl FnOnce() -> Option<u64>,
    ) -> bool {
        if let Some(waiting) = &self.unmet {
            let moved = look.demand
                || look
                    .free
                    .is_none_or(|f| f.abs_diff(waiting.free) >= EMERGENCY_DROP)
                || waiting.moved(look.unix, live);
            if !moved {
                return false;
            }
            self.unmet = None;
            if look.level == Level::Emergency {
                return true;
            }
        }
        look.demand || due(last, look.at, look.level, look.free, look.poked, look.room)
    }

    /// An emergency page that took nothing waited on `waits`: gathered with the cycle's other
    /// pages'. Where it `closes` the cycle (it ran out of what it may take), what they all waited
    /// on comes back, the clock's wait bounded at `now` + [`UNMET_WAIT_MAX_S`], and the gathering
    /// starts again; `None` while the cycle goes on.
    pub fn page(&mut self, waits: &Waits, closes: bool, now: i64) -> Option<(i64, Vec<Held>)> {
        self.gathered.merge(waits);
        if !closes {
            return None;
        }
        let Waits { ripens, held } = std::mem::take(&mut self.gathered);
        let most = now.saturating_add(UNMET_WAIT_MAX_S);
        Some((ripens.map_or(most, |at| at.min(most)), held))
    }

    /// No wait and nothing gathered: a pass took something, ended out of the emergency or was
    /// no emergency's, or the look read the episode's end.
    pub fn reset(&mut self) {
        *self = Self::NONE;
    }
}

#[cfg(test)]
#[path = "disk_cadence_tests.rs"]
mod tests;
