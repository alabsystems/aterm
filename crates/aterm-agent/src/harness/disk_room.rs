// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ROOM: what one round of this machine's builds takes, measured as the
//! two-hour fall in free space on a typical day of the last week (design
//! `docs/DESIGN-disk-awareness-2026-10-17.md` §2.3). The lines derive from it
//! ([`super::Config::thresholds_with`]): warn at least twice the room, the
//! floor at least the room.
//!
//! ```text
//! fall(t) = max over readings s in (t − 2 h, t] of free(s) − free(t)
//! day(d)  = max over the readings of UTC day d of fall(t)
//! raw     = the second-largest day(d) of the last 7 days     (readings on ≥ 2 days)
//! lo      = min(16 GiB, 5 % of total)
//! hi      = max(lo, min(10 % of total, 256 GiB))
//! room    = clamp(raw, lo, hi); no history, < 2 days, an unreadable file: lo
//! ```
//!
//! * *Falls, not build sizes.* A build directory's size is everything it ever
//!   accumulated, not what the next build writes. A fall is what the volume
//!   lost, concurrent builds included, read from the `statfs` the watch makes
//!   every minute anyway. A later rise (a pass, an agent's `rm`) cancels no
//!   earlier fall.
//! * *The second-largest day.* One event (a dataset copy, a VM image) must not
//!   set the floors for a week; a fall seen on two days of the week is what
//!   the machine does.
//!
//! [`RoomMeter`] is pure: every instant is the caller's (as sysprobe's
//! `SwapMeter`). [`Rooms`] is the watch's set, one meter per volume, persisted
//! in `<harness state>/disk-room.json` ([`ROOM_FILE`]) at most hourly by temp
//! file and rename; only the day pairs are kept, the in-day ring is rebuilt.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use aterm_json::{Map, Value};

use super::GIB;

/// The fall's window: readings within two hours before a reading.
pub const FALL_WINDOW_S: i64 = 2 * 3600;

/// The days the room is read over.
pub const ROOM_DAYS: i64 = 7;

/// The room's floor is at most this many GiB ([`room_bounds`]).
pub const ROOM_LO_GIB: u64 = 16;

/// The room's floor is at most this percent of the volume ([`room_bounds`]).
pub const ROOM_LO_PCT: u64 = 5;

/// The room's cap is at most this percent of the volume ([`room_bounds`]).
pub const ROOM_HI_PCT: u64 = 10;

/// The room's cap is at most this many GiB ([`room_bounds`]).
pub const ROOM_HI_GIB: u64 = 256;

/// The most volumes [`Rooms`] keeps.
pub const MAX_ROOMS: usize = 8;

/// The file the watch keeps its rooms in, under the harness state directory.
pub const ROOM_FILE: &str = "disk-room.json";

/// How often the watch writes [`ROOM_FILE`] at most.
pub const ROOM_SAVE_EVERY_S: i64 = 3600;

/// One-minute buckets in the fall window.
const WINDOW_MINUTES: i64 = FALL_WINDOW_S / 60;

const DAY_S: i64 = 86_400;

/// The clamp of a room on a volume of `total` bytes: `(lo, hi)`, with
/// `lo = min(16 GiB, 5 %)` and `hi = max(lo, min(10 %, 256 GiB))` — defined on
/// every volume (`[16 GiB, 10 %]` alone is empty below about 160 GiB).
#[must_use]
pub fn room_bounds(total: u64) -> (u64, u64) {
    let pct = |p: u64| total.saturating_mul(p) / 100;
    let lo = (ROOM_LO_GIB * GIB).min(pct(ROOM_LO_PCT));
    let hi = lo.max(pct(ROOM_HI_PCT).min(ROOM_HI_GIB * GIB));
    (lo, hi)
}

/// ONE VOLUME'S ROOM METER: at most 120 one-minute maxima of free space for
/// the two-hour window, and at most [`ROOM_DAYS`] `(UTC day, largest fall)`
/// pairs. Pure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomMeter {
    /// `(unix minute, the most free bytes read in it)`, oldest first.
    minutes: VecDeque<(i64, u64)>,
    /// `(UTC day, the largest two-hour fall read on it)`, oldest first.
    days: Vec<(i64, u64)>,
}

impl RoomMeter {
    /// A meter that has read nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `free` bytes at unix second `at_s` (the caller's clock): its fall
    /// against the window's largest reading enters its day's maximum. A clock
    /// that moved back drops the readings after it (they say nothing of now),
    /// never a day's maximum it already holds for an earlier day.
    pub fn observe(&mut self, at_s: i64, free: u64) {
        let minute = at_s.div_euclid(60);
        self.minutes
            .retain(|(m, _)| *m <= minute && *m > minute - WINDOW_MINUTES);
        match self.minutes.back_mut() {
            Some((m, f)) if *m == minute => *f = (*f).max(free),
            _ => self.minutes.push_back((minute, free)),
        }
        let peak = self.minutes.iter().map(|(_, f)| *f).max().unwrap_or(free);
        let fall = peak.saturating_sub(free);
        let day = at_s.div_euclid(DAY_S);
        self.days.retain(|(d, _)| *d <= day && *d > day - ROOM_DAYS);
        match self.days.last_mut() {
            Some((d, f)) if *d == day => *f = (*f).max(fall),
            _ => self.days.push((day, fall)),
        }
    }

    /// The room at `now_s` on a volume of `total` bytes: the second-largest
    /// daily fall of the last [`ROOM_DAYS`] days clamped to [`room_bounds`],
    /// or the bounds' `lo` with readings on fewer than two days.
    #[must_use]
    pub fn room(&self, now_s: i64, total: u64) -> u64 {
        let (lo, hi) = room_bounds(total);
        let today = now_s.div_euclid(DAY_S);
        let mut falls: Vec<u64> = self
            .days
            .iter()
            .filter(|(d, _)| *d <= today && *d > today - ROOM_DAYS)
            .map(|(_, f)| *f)
            .collect();
        if falls.len() < 2 {
            return lo;
        }
        falls.sort_unstable_by(|a, b| b.cmp(a));
        falls[1].clamp(lo, hi)
    }

    /// The days read, `(UTC day, largest fall)`, oldest first.
    #[must_use]
    pub fn days(&self) -> &[(i64, u64)] {
        &self.days
    }

    /// The day pairs as JSON (`[[day, fall], …]`): what persists.
    #[must_use]
    pub fn encode(&self) -> Value {
        Value::Array(
            self.days
                .iter()
                .map(|(d, f)| Value::Array(vec![Value::from(*d), Value::from(*f)]))
                .collect(),
        )
    }

    /// A meter from [`Self::encode`]'s JSON: its day pairs, the in-day ring
    /// empty. `None` for anything else (a corrupt file is no history).
    #[must_use]
    pub fn decode(v: &Value) -> Option<Self> {
        let mut days: Vec<(i64, u64)> = Vec::new();
        for pair in v.as_array()? {
            let pair = pair.as_array()?;
            let (Some(d), Some(f), 2) = (
                pair.first().and_then(Value::as_i64),
                pair.get(1).and_then(Value::as_u64),
                pair.len(),
            ) else {
                return None;
            };
            if days.last().is_some_and(|(last, _)| *last >= d) {
                return None;
            }
            days.push((d, f));
        }
        if days.len() > usize::try_from(ROOM_DAYS).unwrap_or(7) {
            return None;
        }
        Some(Self {
            minutes: VecDeque::new(),
            days,
        })
    }
}

/// THE WATCH'S ROOMS: one [`RoomMeter`] per volume key
/// (`aterm_sysprobe::volumes::VolumeKey::spelled`), at most [`MAX_ROOMS`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rooms {
    meters: BTreeMap<String, RoomMeter>,
    /// When [`Self::save_due`] last let a write through.
    saved_at: Option<i64>,
}

impl Rooms {
    /// The meter of volume `key`, made when it is new and there is room for
    /// it (a ninth volume is not metered: its room is its bounds' `lo`).
    pub fn meter(&mut self, key: &str) -> Option<&mut RoomMeter> {
        if !self.meters.contains_key(key) && self.meters.len() >= MAX_ROOMS {
            return None;
        }
        Some(self.meters.entry(key.to_owned()).or_default())
    }

    /// The room of volume `key` at `now_s` on a volume of `total` bytes: its
    /// meter's, or the bounds' `lo` for one with no meter.
    #[must_use]
    pub fn room(&self, key: &str, now_s: i64, total: u64) -> u64 {
        self.meters
            .get(key)
            .map_or_else(|| room_bounds(total).0, |m| m.room(now_s, total))
    }

    /// Whether a write is due at `now_s` ([`ROOM_SAVE_EVERY_S`] since the
    /// last one let through, or none yet); one let through is counted, so a
    /// write that then fails (a full disk) waits for the next hour too.
    pub fn save_due(&mut self, now_s: i64) -> bool {
        if self
            .saved_at
            .is_some_and(|at| now_s.saturating_sub(at) < ROOM_SAVE_EVERY_S && now_s >= at)
        {
            return false;
        }
        self.saved_at = Some(now_s);
        true
    }

    /// The rooms as the file holds them.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut volumes = Map::new();
        for (key, m) in &self.meters {
            volumes.insert(key.clone(), m.encode());
        }
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("volumes".to_owned(), Value::Object(volumes));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The rooms of the file's `text`; a volume whose entry does not decode
    /// is left out, and a text that is not the file's is no history.
    #[must_use]
    pub fn decode(text: &str) -> Self {
        let Ok(v) = aterm_json::from_str::<Value>(text) else {
            return Self::default();
        };
        if v.get("schema").and_then(Value::as_u64) != Some(1) {
            return Self::default();
        }
        let meters = v
            .get("volumes")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, m)| Some((k.clone(), RoomMeter::decode(m)?)))
                    .take(MAX_ROOMS)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            meters,
            saved_at: None,
        }
    }

    /// The rooms in `dir`'s [`ROOM_FILE`]; none where it cannot be read.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(ROOM_FILE))
            .map(|t| Self::decode(&t))
            .unwrap_or_default()
    }

    /// Write the rooms to `dir`'s [`ROOM_FILE`] by temp file and rename.
    ///
    /// # Errors
    ///
    /// The directory could not be made or the write failed (a full disk):
    /// the caller skips it, and the in-memory meters stand.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        super::super::upgrade_models::write_atomic_ordered(&dir.join(ROOM_FILE), &self.encode())
    }
}

/// The room of the volume `reading` read, from the rooms the window's watch
/// keeps in the harness state directory `state` ([`Rooms::load`]), at
/// `now_s`: what `aterm harness disk` (another process) shows on its watch
/// line. No file, or no meter for this volume: the bounds' `lo`.
#[must_use]
pub fn kept_room(state: &Path, reading: &aterm_sysprobe::volumes::Reading, now_s: i64) -> u64 {
    Rooms::load(state).room(&reading.key.spelled(), now_s, reading.total)
}

#[cfg(test)]
#[path = "disk_room_tests.rs"]
mod tests;
