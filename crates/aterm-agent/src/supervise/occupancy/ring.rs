// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS RING (`docs/DESIGN-harness-occupancy-2026-10-20.md` §6).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::reading::{LOOK_EVERY_MS, OccupancyReading, SlotsSeen};
use crate::harness::disk::Level;
use crate::supervise::memory::Pressure;

/// How many rows the ring keeps: a day of looks.
pub const RING_ROWS: usize = 1440;

/// The ring's file under the aterm state directory `state`: `occupancy/facts.ring`.
#[must_use]
pub fn ring_path(state: &Path) -> PathBuf {
    state.join("occupancy").join("facts.ring")
}

/// One supervised session at a look.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RingSession {
    /// The session's id.
    pub sid: String,
    /// The last look at which its jobs held a build slot, unix ms.
    #[serde(default)]
    pub held_ms: Option<i64>,
    /// Its hold read ready at the look ([`super::rules::ready`]).
    #[serde(default)]
    pub ready: bool,
    /// The first guard its loop last told holding a line, by its word.
    #[serde(default)]
    pub guard: Option<String>,
}

/// ONE BOARD LOOK: one row of the ring.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RingRow {
    /// When the board was taken, unix ms.
    pub at: i64,
    /// The board's snapshot no longer spoke for the look.
    #[serde(default)]
    pub stale: bool,
    /// Memory available without paging, MiB.
    #[serde(default)]
    pub avail_mib: Option<u64>,
    /// The kernel's memory pressure: `normal`, `warn` or `critical`.
    #[serde(default)]
    pub mem: Option<String>,
    /// The home volume's level: `ok`, `low`, `short` or `emergency`.
    #[serde(default)]
    pub disk: Option<String>,
    /// The build slots proven usable.
    #[serde(default)]
    pub usable: Option<u16>,
    /// The slot files present.
    #[serde(default)]
    pub n: Option<u16>,
    /// How long `hold-room`'s slot term had held, ms, once it spans its `for=`.
    #[serde(default)]
    pub free_ms: Option<u64>,
    /// `hold-room`'s machine terms at the look.
    #[serde(default)]
    pub room: Option<bool>,
    /// The supervised sessions.
    #[serde(default)]
    pub sessions: Vec<RingSession>,
}

const fn pressure_word(p: Pressure) -> &'static str {
    match p {
        Pressure::Normal => "normal",
        Pressure::Warn => "warn",
        Pressure::Critical => "critical",
    }
}

fn pressure_of(word: &str) -> Option<Pressure> {
    match word {
        "normal" => Some(Pressure::Normal),
        "warn" => Some(Pressure::Warn),
        "critical" => Some(Pressure::Critical),
        _ => None,
    }
}

fn level_of(word: &str) -> Option<Level> {
    match word {
        "ok" => Some(Level::Ok),
        "low" => Some(Level::Low),
        "short" => Some(Level::Short),
        "emergency" => Some(Level::Emergency),
        _ => None,
    }
}

impl RingRow {
    /// The row of a look whose machine facts `machine` carries.
    #[must_use]
    pub fn of(machine: &OccupancyReading, sessions: Vec<RingSession>) -> Self {
        Self {
            at: machine.taken_ms,
            stale: machine.stale,
            avail_mib: machine.avail_mib,
            mem: machine.pressure.map(|p| pressure_word(p).to_string()),
            disk: machine.disk.map(|l| l.as_str().to_string()),
            usable: machine.slots.map(|s| s.usable),
            n: machine.slots.map(|s| s.n),
            free_ms: machine.slots_free_ms,
            room: machine.room,
            sessions,
        }
    }

    /// The board as session `sid`'s host read it at this look.
    #[must_use]
    pub fn reading(&self, sid: &str) -> OccupancyReading {
        OccupancyReading {
            taken_ms: self.at,
            stale: self.stale,
            avail_mib: self.avail_mib,
            pressure: self.mem.as_deref().and_then(pressure_of),
            disk: self.disk.as_deref().and_then(level_of),
            slots: self
                .usable
                .zip(self.n)
                .map(|(usable, n)| SlotsSeen { usable, n }),
            slots_free_ms: self.free_ms,
            room: self.room,
            held_slot_last_ms: self.session(sid).and_then(|s| s.held_ms),
        }
    }

    /// Session `sid` at this look.
    #[must_use]
    pub fn session(&self, sid: &str) -> Option<&RingSession> {
        let sid = sid.trim_start_matches('@');
        self.sessions.iter().find(|s| s.sid == sid)
    }

    /// The sessions whose hold read ready.
    #[must_use]
    pub fn ready(&self) -> u16 {
        u16::try_from(self.sessions.iter().filter(|s| s.ready).count()).unwrap_or(u16::MAX)
    }

    /// The build slots idle while ready at this look.
    #[must_use]
    pub fn idle_ready(&self) -> u16 {
        let usable = if self.stale {
            0
        } else {
            self.usable.unwrap_or(0)
        };
        usable.min(self.ready())
    }

    /// The row as the ring holds it: one JSON line, no newline.
    #[must_use]
    pub fn line(&self) -> String {
        aterm_json::to_string(self).unwrap_or_default()
    }
}

/// The rows of the ring text `body`, oldest first; a line this build cannot read is passed over.
#[must_use]
pub fn rows_in(body: &str) -> Vec<RingRow> {
    body.lines()
        .filter_map(|line| aterm_json::from_str::<RingRow>(line).ok())
        .collect()
}

/// THE SLOT-TIME IDLE WHILE READY over `rows` (oldest first) from `since_ms` on, ms.
#[must_use]
pub fn idle_ready_ms<'a>(rows: impl IntoIterator<Item = &'a RingRow>, since_ms: i64) -> u64 {
    let mut total = 0_u64;
    let mut before: Option<i64> = None;
    for row in rows {
        let span = before.map_or(LOOK_EVERY_MS, |at| {
            row.at.saturating_sub(at).clamp(0, LOOK_EVERY_MS)
        });
        before = Some(row.at);
        if row.at < since_ms {
            continue;
        }
        let span = u64::try_from(span).unwrap_or(0);
        total = total.saturating_add(span.saturating_mul(u64::from(row.idle_ready())));
    }
    total
}

#[cfg(test)]
#[path = "ring_tests.rs"]
mod tests;
