// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Usage-tap reset clock for timing `usage-resume@v1` continuations at a Claude Code usage wall.

use aterm_phase::WallKind;

use super::limit::{CLOCK_SLACK_S, names_a_session_window};
use crate::harness::tap::Window;

/// Claude Code rate-limit usage window (`five_hour` or `seven_day`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsageWindow {
    /// Five-hour session window.
    FiveHour,
    /// Seven-day weekly window.
    SevenDay,
}

impl UsageWindow {
    /// All usage windows (`FiveHour`, `SevenDay`).
    pub const ALL: [UsageWindow; 2] = [UsageWindow::FiveHour, UsageWindow::SevenDay];

    /// Window duration in seconds.
    #[must_use]
    pub fn len_s(self) -> i64 {
        match self {
            UsageWindow::FiveHour => 5 * 3600,
            UsageWindow::SevenDay => 7 * 86_400,
        }
    }

    /// Status-line payload key for this window.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            UsageWindow::FiveHour => "five_hour",
            UsageWindow::SevenDay => "seven_day",
        }
    }

    /// Its slot in [`TapClock`]'s table.
    fn slot(self) -> usize {
        match self {
            UsageWindow::FiveHour => 0,
            UsageWindow::SevenDay => 1,
        }
    }

    /// Usage windows that time a wall of `kind` with notice `message`.
    #[must_use]
    pub fn of_wall(kind: WallKind, message: &str) -> &'static [UsageWindow] {
        match kind {
            WallKind::UsageSession if names_a_session_window(message) => &[UsageWindow::FiveHour],
            WallKind::UsageSession => &UsageWindow::ALL,
            WallKind::UsageWeekly => &[UsageWindow::SevenDay],
            WallKind::Spend
            | WallKind::ModelBucket { .. }
            | WallKind::Context
            | WallKind::Auth
            | WallKind::ApiError { .. }
            | WallKind::Overloaded
            | WallKind::Memory => &[],
        }
    }
}

/// Latest usage-tap reading for one window of the walled process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapWindow {
    /// Parsed window utilization and reset timestamp, if present in the payload.
    pub figure: Option<Window>,
    /// Unix timestamp (seconds) when the tap wrote the reading.
    pub read_at: i64,
}

/// Reset verdict derived from [`TapClock`] readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapSays {
    /// No qualifying tap reading since the wall was first read.
    Nothing,
    /// Exact window reset timestamp (Unix seconds).
    ResetsAt(i64),
    /// All full windows rolled over by the reading at this Unix timestamp.
    RolledAt(i64),
}

/// Per-wall accumulator tracking full usage windows and their reset or rollover timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapClock {
    /// When the loop first read the wall (unix seconds): a reading the tap
    /// wrote before is ignored.
    first_read: i64,
    /// Per window ([`UsageWindow::slot`]): the reset of the last reading
    /// since the first read that showed it full, and when a reading after
    /// that showed it rolled over.
    full: [Option<(i64, Option<i64>)>; 2],
    /// The reset the tap timed: the later full window's of the newest
    /// reading that timed one.
    timed: Option<i64>,
}

impl TapClock {
    /// Create a tap clock anchored at the wall's first read (`first_read` Unix seconds).
    #[must_use]
    pub fn new(first_read: i64) -> TapClock {
        TapClock {
            first_read,
            full: [None, None],
            timed: None,
        }
    }

    /// Unix timestamp (seconds) when the wall was first read.
    #[must_use]
    pub fn first_read(&self) -> i64 {
        self.first_read
    }

    /// Fold the newest tap readings for the wall's windows into the clock at `now`.
    pub fn step(&mut self, readings: &[(UsageWindow, Option<TapWindow>)], now: i64) {
        let mut times: Option<i64> = None;
        for &(window, reading) in readings {
            let Some(reading) = reading.filter(|r| r.read_at >= self.first_read) else {
                continue;
            };
            let slot = &mut self.full[window.slot()];
            match reading.figure {
                Some(f) if f.x10() >= 1000 => {
                    *slot = Some((f.resets_at, None));
                    let ahead = f.resets_at.saturating_sub(now);
                    if ahead > 0 && ahead <= window.len_s() {
                        times = times.max(Some(f.resets_at));
                    }
                }
                _ => {
                    if let Some((_, rolled)) = slot.as_mut() {
                        rolled.get_or_insert(reading.read_at);
                    }
                }
            }
        }
        if times.is_some() {
            self.timed = times;
        }
    }

    /// Return the tap's reset or rollover verdict for `windows`.
    #[must_use]
    pub fn says(&self, windows: &[UsageWindow]) -> TapSays {
        if let Some(at) = self.timed {
            return TapSays::ResetsAt(at);
        }
        let full: Vec<Option<i64>> = windows
            .iter()
            .filter_map(|w| self.full[w.slot()])
            .map(|(_, rolled)| rolled)
            .collect();
        match full.iter().copied().collect::<Option<Vec<i64>>>() {
            Some(rolled) => rolled
                .into_iter()
                .max()
                .map_or(TapSays::Nothing, TapSays::RolledAt),
            None => TapSays::Nothing,
        }
    }
}

/// Selected reset timestamp and any clock disagreement beyond [`CLOCK_SLACK_S`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timed {
    /// Chosen reset timestamp (Unix seconds), or `None` to use policy backoff.
    pub at: Option<i64>,
    /// `(notice_reset, tap_reset)` when the two clocks differ by more than [`CLOCK_SLACK_S`].
    pub disagree: Option<(i64, i64)>,
}

/// Combine the notice's parsed reset with [`TapSays`] to choose the target reset time.
#[must_use]
pub fn timed(notice: Option<i64>, tap: TapSays) -> Timed {
    let plain = |at| Timed { at, disagree: None };
    match (notice, tap) {
        (Some(n), TapSays::ResetsAt(t)) if t.abs_diff(n) > CLOCK_SLACK_S.unsigned_abs() => Timed {
            at: Some(n.max(t)),
            disagree: Some((n, t)),
        },
        (_, TapSays::ResetsAt(t)) => plain(Some(t)),
        (None, TapSays::RolledAt(at)) => plain(Some(at)),
        (n, TapSays::RolledAt(_) | TapSays::Nothing) => plain(n),
    }
}

#[cfg(test)]
#[path = "tap_clock_tests.rs"]
mod tests;
