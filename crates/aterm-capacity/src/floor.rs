// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE VOLUME'S LINES, ONE DEFINITION: the warning, the floor and the emergency line of a
//! volume's free space, scaled to its size and raised to its room. The window's disk watch
//! (`aterm-agent`'s `harness::disk`) and every tool that admits a disk need read them here,
//! from the same inputs ([`Watch`]): the owner's `[disk]` knobs in aterm.toml ([`Knobs::read`])
//! and the room the watch measured ([`crate::room`]).

use crate::room::Rooms;

/// One GiB.
pub const GIB: u64 = 1 << 30;

/// `disk.warn_free_gib`'s default (design §5.5 of `docs/DESIGN-aterm-wrapper-2026-09-17.md`).
pub const DEFAULT_WARN_FREE_GIB: u64 = 40;

/// `disk.auto_free_gib`'s default: the automatic floor.
pub const DEFAULT_AUTO_FREE_GIB: u64 = 10;

/// How far above the floor the pressure pass reclaims before it stops.
pub const PRESSURE_MARGIN_GIB: u64 = 5;

/// The automatic floor is at least this percent of the volume's size.
pub const AUTO_FREE_PCT: u64 = 5;

/// The warning threshold is at least this percent of the volume's size.
pub const WARN_FREE_PCT: u64 = 8;

/// The most the volume's share ([`AUTO_FREE_PCT`]) raises the automatic floor to.
pub const AUTO_FREE_SHARE_MAX_GIB: u64 = 32;

/// The most the volume's share ([`WARN_FREE_PCT`]) raises the warning to.
pub const WARN_FREE_SHARE_MAX_GIB: u64 = 64;

/// The most the volume's share ([`EMERGENCY_FREE_PCT`]) raises the emergency line to.
pub const EMERGENCY_FREE_SHARE_MAX_GIB: u64 = 8;

/// An UNWRITTEN `warn_free_gib` is held to at most this percent of the volume.
pub const SMALL_WARN_MAX_PCT: u64 = 20;

/// An UNWRITTEN `auto_free_gib` is held to at most this percent of the volume.
pub const SMALL_FLOOR_MAX_PCT: u64 = 10;

/// THE EMERGENCY: below the larger of this many GiB and [`EMERGENCY_FREE_PCT`] of the volume, or with fewer than [`EMERGENCY_FILES_FREE`] inodes, writes are about to fail with `ENOSPC`.
pub const EMERGENCY_FREE_GIB: u64 = 4;

/// See [`EMERGENCY_FREE_GIB`].
pub const EMERGENCY_FREE_PCT: u64 = 1;

/// Fewer free inodes than this is an emergency whatever the bytes say: a volume out of inodes fails a write with `ENOSPC` too.
pub const EMERGENCY_FILES_FREE: u64 = 50_000;

/// The room's least is at most this many GiB ([`room_bounds`]).
pub const ROOM_LO_GIB: u64 = 16;

/// The room's least is at most this percent of the volume ([`room_bounds`]).
pub const ROOM_LO_PCT: u64 = 5;

/// The room's cap is at most this percent of the volume ([`room_bounds`]).
pub const ROOM_HI_PCT: u64 = 10;

/// The room's cap is at most this many GiB ([`room_bounds`]).
pub const ROOM_HI_GIB: u64 = 256;

/// The `(least, most)` a measured ROOM (the second-largest daily two-hour fall of free space over a week) is clamped to on a volume of `total` bytes; with fewer than two days measured the room is the least.
#[must_use]
pub fn room_bounds(total: u64) -> (u64, u64) {
    let pct = |p: u64| total.saturating_mul(p) / 100;
    let lo = ROOM_LO_GIB.saturating_mul(GIB).min(pct(ROOM_LO_PCT));
    let hi = lo.max(pct(ROOM_HI_PCT).min(ROOM_HI_GIB.saturating_mul(GIB)));
    (lo, hi)
}

/// Which of the volume-scaled `[disk]` knobs the owner WROTE in aterm.toml: those are read as written, never raised to the volume's share or its room.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Written {
    /// `disk.auto_free_gib` is written.
    pub auto_free_gib: bool,
    /// `disk.warn_free_gib` is written.
    pub warn_free_gib: bool,
}

/// The `[disk]` knobs the lines are made from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Knobs {
    /// `disk.warn_free_gib`.
    pub warn_free_gib: u64,
    /// `disk.auto_free_gib`; `0` turns the floor off.
    pub auto_free_gib: u64,
    /// `disk.room_gib`: the room PINNED (`None`: the measured room the caller passes).
    pub room_gib: Option<u64>,
    /// Which knobs are written.
    pub written: Written,
}

impl Default for Knobs {
    fn default() -> Self {
        Knobs {
            warn_free_gib: DEFAULT_WARN_FREE_GIB,
            auto_free_gib: DEFAULT_AUTO_FREE_GIB,
            room_gib: None,
            written: Written::default(),
        }
    }
}

impl Knobs {
    /// Read `disk.<key> = n` when `key` is one of the lines' knobs, as the window reads aterm.toml: a negative `warn_free_gib` is the default and unwritten, a negative `auto_free_gib` turns the floor off (written), and a negative `room_gib` leaves the measured room. `false` for any other key, which this leaves alone.
    pub fn read(&mut self, key: &str, n: i64) -> bool {
        match key {
            "warn_free_gib" => {
                self.warn_free_gib = u64::try_from(n).unwrap_or(DEFAULT_WARN_FREE_GIB);
                self.written.warn_free_gib = n >= 0;
            }
            "auto_free_gib" => {
                self.auto_free_gib = u64::try_from(n).unwrap_or(0);
                self.written.auto_free_gib = true;
            }
            "room_gib" => self.room_gib = u64::try_from(n).ok(),
            _ => return false,
        }
        true
    }

    /// The knobs an aterm.toml's `text` writes in its `[disk]` table, each whole number [`Self::read`]; the defaults for any it does not write as a whole number, and for a file that is not TOML. (A `[disk]` the window cannot read turns its automatic pass off; admission still keeps the floor these give.)
    #[must_use]
    pub fn from_toml(text: &str) -> Knobs {
        let mut knobs = Knobs::default();
        let Ok(doc) = aterm_toml::from_str::<aterm_toml::Value>(text) else {
            return knobs;
        };
        if let Some(aterm_toml::Value::Table(disk)) = doc.get("disk") {
            for (key, value) in disk {
                if let aterm_toml::Value::Integer(n) = value {
                    knobs.read(key, *n);
                }
            }
        }
        knobs
    }

    /// The knobs the account's own aterm.toml writes (resolved as its window resolves it, even from a scratch home); the defaults where it is absent or unreadable.
    #[must_use]
    pub fn kept() -> Knobs {
        crate::own_roots()
            .1
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map_or_else(Knobs::default, |text| Knobs::from_toml(&text))
    }
}

/// The warning and the floor in force, GiB ([`in_force`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InForce {
    /// Below this many free GiB the volume is `low`.
    pub warn_free_gib: u64,
    /// Below this many free GiB the volume is `short`; `0` is off.
    pub auto_free_gib: u64,
}

/// The warning and the floor in force on a volume of `total` bytes (`None`: unknown, the knobs as they are), each knob the owner did not write raised to the volume's share and then to `room` bytes (the measured room; [`Knobs::room_gib`] pins it): the warning to twice the room, the floor to the room.
#[must_use]
pub fn in_force(knobs: Knobs, total: Option<u64>, room: Option<u64>) -> InForce {
    let base = on_volume(knobs, total);
    let Some(room) = knobs.room_gib.map(|g| g.saturating_mul(GIB)).or(room) else {
        return base;
    };
    if total.is_none() {
        return base;
    }
    let room_gib = room.div_ceil(GIB);
    InForce {
        warn_free_gib: if knobs.written.warn_free_gib {
            base.warn_free_gib
        } else {
            base.warn_free_gib.max(room_gib.saturating_mul(2))
        },
        auto_free_gib: if base.auto_free_gib == 0 || knobs.written.auto_free_gib {
            base.auto_free_gib
        } else {
            base.auto_free_gib.max(room_gib)
        },
    }
}

/// [`in_force`] before the room: each unwritten knob held to its small-volume share and raised to the volume's share.
fn on_volume(knobs: Knobs, total: Option<u64>) -> InForce {
    let as_written = InForce {
        warn_free_gib: knobs.warn_free_gib,
        auto_free_gib: knobs.auto_free_gib,
    };
    let Some(total) = total else {
        return as_written;
    };
    let share = |pct: u64, max: u64| (total / 100).saturating_mul(pct).div_ceil(GIB).min(max);
    let held = |knob: u64, pct: u64| knob.min((total.saturating_mul(pct) / 100 / GIB).max(1));
    InForce {
        warn_free_gib: if knobs.written.warn_free_gib {
            knobs.warn_free_gib
        } else {
            held(knobs.warn_free_gib, SMALL_WARN_MAX_PCT)
                .max(share(WARN_FREE_PCT, WARN_FREE_SHARE_MAX_GIB))
        },
        auto_free_gib: if knobs.auto_free_gib == 0 || knobs.written.auto_free_gib {
            knobs.auto_free_gib
        } else {
            held(knobs.auto_free_gib, SMALL_FLOOR_MAX_PCT)
                .max(share(AUTO_FREE_PCT, AUTO_FREE_SHARE_MAX_GIB))
        },
    }
}

/// A volume's lines in bytes ([`lines`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lines {
    /// Below this the volume is `low` and the watch warns.
    pub warn: u64,
    /// Below this the volume is `short` and the automatic pass runs (`0`: off). Admission keeps free space above it.
    pub floor: u64,
    /// Where the pressure pass stops: the floor plus [`PRESSURE_MARGIN_GIB`].
    pub target: u64,
    /// Below this writes are about to fail ([`EMERGENCY_FREE_GIB`]).
    pub emergency: u64,
    /// Fewer free inodes than this is an emergency ([`EMERGENCY_FILES_FREE`]).
    pub files_emergency: u64,
}

/// The lines in force on a volume of `total` bytes with room `room` ([`in_force`]), in bytes.
#[must_use]
pub fn lines(knobs: Knobs, total: Option<u64>, room: Option<u64>) -> Lines {
    let c = in_force(knobs, total, room);
    let pct = |p: u64| total.map_or(0, |t| (t / 100).saturating_mul(p));
    let floor = c.auto_free_gib.saturating_mul(GIB);
    let emergency = EMERGENCY_FREE_GIB
        .saturating_mul(GIB)
        .max(pct(EMERGENCY_FREE_PCT).min(EMERGENCY_FREE_SHARE_MAX_GIB.saturating_mul(GIB)));
    Lines {
        warn: c.warn_free_gib.saturating_mul(GIB),
        floor,
        target: c
            .auto_free_gib
            .saturating_add(PRESSURE_MARGIN_GIB)
            .saturating_mul(GIB),
        emergency: if floor > 0 {
            emergency.min(floor)
        } else {
            emergency
        },
        files_emergency: EMERGENCY_FILES_FREE,
    }
}

/// The floor of a volume of `total` bytes under the default knobs and no measured room: the room's least ([`room_bounds`]), which is what the window's watch holds a volume to before it has measured two days.
#[must_use]
pub fn default_floor(total: u64) -> u64 {
    lines(Knobs::default(), Some(total), Some(room_bounds(total).0)).floor
}

/// The free space admission keeps on a volume with `lines`: the floor, or where the owner turned the floor off (`disk.auto_free_gib = 0`), the emergency line below which writes fail.
#[must_use]
pub const fn admission_floor(lines: &Lines) -> u64 {
    if lines.floor > 0 {
        lines.floor
    } else {
        lines.emergency
    }
}

/// WHAT THE WINDOW'S DISK WATCH HOLDS A VOLUME TO: the owner's `[disk]` knobs and the rooms it measured, read from the files the watch reads and writes ([`Self::kept`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    /// The `[disk]` knobs.
    pub knobs: Knobs,
    /// The rooms the watch measured, by volume.
    pub rooms: Rooms,
}

impl Watch {
    /// As this user's files keep it: aterm.toml's knobs ([`Knobs::kept`]) and the rooms the watch last wrote ([`crate::room::state_dir`]; none written, each volume's room is the room's least).
    #[must_use]
    pub fn kept() -> Watch {
        Watch {
            knobs: Knobs::kept(),
            rooms: crate::room::state_dir()
                .map(|dir| Rooms::load(&dir))
                .unwrap_or_default(),
        }
    }

    /// The room of the volume keyed `key` (`aterm_sysprobe::volumes::VolumeKey::spelled`) of `total` bytes at unix second `now_s`: `disk.room_gib` where written, else the measured room.
    #[must_use]
    pub fn room(&self, key: &str, total: u64, now_s: i64) -> u64 {
        self.knobs.room_gib.map_or_else(
            || self.rooms.room(key, now_s, total),
            |g| g.saturating_mul(GIB),
        )
    }

    /// The lines the watch holds that volume to ([`lines`], raised to [`Self::room`]).
    #[must_use]
    pub fn lines(&self, key: &str, total: u64, now_s: i64) -> Lines {
        lines(self.knobs, Some(total), Some(self.room(key, total, now_s)))
    }
}
