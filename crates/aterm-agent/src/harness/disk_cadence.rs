// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pressure-pass cadence shared by the GUI and standalone automatic watches.

use std::time::{Duration, Instant};

use super::Level;

/// Ordinary pressure-pass interval.
pub const PASS_EVERY: Duration = Duration::from_secs(10 * 60);
/// A fresh emergency drop that permits another otherwise fruitless pass.
pub const EMERGENCY_DROP: u64 = 512 << 20;
/// Initial interval for a low, unproductive volume.
pub const LOW_EVERY: Duration = Duration::from_secs(3600);
/// Maximum low-volume interval.
pub const LOW_EVERY_MAX: Duration = Duration::from_secs(8 * 3600);

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
    /// Its low-volume retry interval.
    pub low_every: Duration,
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
    if level == Level::Low {
        let fell =
            matches!((pass.free, free), (Some(was), Some(is)) if is.saturating_add(room / 4) < was);
        return poked || pass.level != Level::Low || fell || since >= pass.low_every;
    }
    if since >= PASS_EVERY {
        return true;
    }
    if pass.fruitless {
        return matches!((pass.free, free), (Some(was), Some(is)) if is.saturating_add(EMERGENCY_DROP) < was);
    }
    poked || level == Level::Emergency || pass.level < Level::Short
}

/// The next retry interval after this pass.
#[must_use]
pub fn low_every(level: Level, freed: bool, before: Option<&Pass>) -> Duration {
    match before {
        Some(last) if level == Level::Low && !freed && last.level == Level::Low => {
            last.low_every.saturating_mul(2).min(LOW_EVERY_MAX)
        }
        _ => LOW_EVERY,
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
    let low_every = low_every(level, freed, last.as_ref());
    *last = Some(Pass {
        at,
        free,
        fruitless,
        level,
        low_every,
    });
}
