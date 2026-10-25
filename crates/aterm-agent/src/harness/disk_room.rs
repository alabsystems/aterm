// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Free-space fall metering over a seven-day rolling window for disk-room thresholds: one
//! definition with the volume's lines (`aterm_capacity::room`), so the tools that admit a disk
//! need hold a volume to the floor the window's watch holds it to.

use std::path::Path;

pub use aterm_capacity::floor::{ROOM_HI_GIB, ROOM_HI_PCT, ROOM_LO_GIB, ROOM_LO_PCT, room_bounds};
pub use aterm_capacity::room::{
    FALL_WINDOW_S, MAX_ROOMS, ROOM_DAYS, ROOM_FILE, ROOM_SAVE_EVERY_S, RoomMeter, Rooms,
};

/// Reads the persisted room for `reading`'s volume from `state` at `now_s`.
#[must_use]
pub fn kept_room(state: &Path, reading: &aterm_sysprobe::volumes::Reading, now_s: i64) -> u64 {
    Rooms::load(state).room(&reading.key.spelled(), now_s, reading.total)
}
