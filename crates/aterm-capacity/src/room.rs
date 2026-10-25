// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ROOM: free-space fall metering over a seven-day rolling window (design
//! `docs/DESIGN-disk-awareness-2026-10-17.md` §6.3). The window's disk watch feeds each look to
//! a [`RoomMeter`] and keeps the meters in [`ROOM_FILE`] under the harness state ([`state_dir`]);
//! the volume's lines are raised to the room ([`crate::floor`]), so every reader of that file
//! holds a volume to the floor the watch holds it to.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use crate::floor::room_bounds;

/// The fall's window: readings within two hours before a reading.
pub const FALL_WINDOW_S: i64 = 2 * 3600;

/// The days the room is read over.
pub const ROOM_DAYS: i64 = 7;

/// The most volumes [`Rooms`] keeps.
pub const MAX_ROOMS: usize = 8;

/// The file the watch keeps its rooms in, under the harness state directory ([`state_dir`]).
pub const ROOM_FILE: &str = "disk-room.json";

/// The harness's folder under `<aterm state>/harness`, where the window's disk watch keeps its rooms.
pub const HARNESS: &str = "claude-harness";

/// The harness state directory the account's own window keeps [`ROOM_FILE`] in: `<aterm state>/harness/`[`HARNESS`], the state root resolved as that window resolves it, even from a scratch home; `None` without a state root.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    crate::own_roots()
        .0
        .map(|root| root.join("harness").join(HARNESS))
}

/// How often the watch writes [`ROOM_FILE`] at most.
pub const ROOM_SAVE_EVERY_S: i64 = 3600;

/// One-minute buckets in the fall window.
const WINDOW_MINUTES: i64 = FALL_WINDOW_S / 60;

const DAY_S: i64 = 86_400;

/// One volume's room meter tracking two-hour free-space falls across [`ROOM_DAYS`] days.
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

    /// Records `free` bytes at unix second `at_s` into the current window and daily maximum.
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

    /// Returns the room at `now_s` on a volume of `total` bytes.
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

    /// Decodes a meter from [`Self::encode`]'s JSON representation.
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

/// Per-volume [`RoomMeter`] table bounded to [`MAX_ROOMS`] volumes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rooms {
    meters: BTreeMap<String, RoomMeter>,
    /// When [`Self::save_due`] last let a write through.
    saved_at: Option<i64>,
}

impl Rooms {
    /// Returns the meter for volume `key`, creating one when there is room.
    pub fn meter(&mut self, key: &str) -> Option<&mut RoomMeter> {
        if !self.meters.contains_key(key) && self.meters.len() >= MAX_ROOMS {
            return None;
        }
        Some(self.meters.entry(key.to_owned()).or_default())
    }

    /// Returns the room of volume `key` at `now_s` on a volume of `total` bytes.
    #[must_use]
    pub fn room(&self, key: &str, now_s: i64, total: u64) -> u64 {
        self.meters
            .get(key)
            .map_or_else(|| room_bounds(total).0, |m| m.room(now_s, total))
    }

    /// Returns whether a write is due at `now_s`, recording `now_s` as the last save when it is.
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

    /// Decodes the rooms from `text`, returning an empty table on invalid input.
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

    /// Write the rooms to `dir`'s [`ROOM_FILE`]: staged in a private folder beside it (`0600`), ordered to disk ([`aterm_durable::ordered`]), then renamed over it.
    ///
    /// # Errors
    ///
    /// The directory could not be made or the write failed (a full disk):
    /// the caller skips it, and the in-memory meters stand.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let stage = aterm_tempfile::Builder::new()
            .prefix(".aterm-write-")
            .tempdir_in(dir)?;
        let staged = stage.path().join("data");
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        let mut file = open.open(&staged)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(self.encode().as_bytes())?;
        aterm_durable::ordered(&file)?;
        drop(file);
        std::fs::rename(&staged, dir.join(ROOM_FILE))
    }
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
