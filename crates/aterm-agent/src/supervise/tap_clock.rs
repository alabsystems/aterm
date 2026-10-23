// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE USAGE TAP TIMES THE WAIT AT A USAGE WALL (the accounts design v2
//! §3.3 and §10, slice S10a; ruling R-7): at a Claude Code usage wall, the reset
//! the usage tap read off the walled process's own figures
//! (`crate::harness::tap`, under `[harness] usage_tap`) is the reset its
//! continuation (`usage-resume@v1`) is timed to — to the second — where the
//! notice's words give a minute's clock, a clock cut in a narrow pane, or
//! nothing, and the policy would wait on its 30-minute back-off. The act is
//! the same one; only its time moves.
//!
//! * WHICH WINDOW ([`UsageWindow::of_wall`]): a five-hour wall (the notice
//!   names its session window) the `five_hour` window, a weekly one
//!   `seven_day`; a usage limit that names no window, the window that reads
//!   full, the later reset where both do. Any other wall none: a model's
//!   bucket, a lost login, an API error — and a spend limit, which only
//!   usage credits lift or a reset its notice names (R-9's credits rule, its
//!   act hanging on whether a reset is named): a reset read here would move
//!   its act, not its time.
//! * WHICH READING ([`TapClock::step`]): only one the tap WROTE at or after
//!   the loop's first read of the wall — of THIS answer of it: a turn the
//!   loop saw run that ends on it again, or its answer to an act of the
//!   loop's, starts the clock afresh, as the notice's reset is placed afresh
//!   there (`Session::tap_reset`). Claude Code runs its status line
//!   about 0.4 s after every reply and every 10 s (E1d, E1g), and every run
//!   carries the figures the process holds since its last request (E1f) —
//!   at a standing wall, its own 429's. So a run after the wall was read
//!   carries the wall's own figures; one before may not (a turn earlier, a
//!   window before a limit reset the plan granted, another process's file).
//!   The recorder's freshness rule (`crate::harness::tap::Fresh`) is about
//!   news for the ACCOUNT's figure, and drops every repeat; here a repeat
//!   written after the wall's first read is exactly the wall's own figure.
//! * WHAT IT TIMES: a FULL window, the vendor's figure at 100.0 % (in tenths,
//!   [`Window::x10`], as the accounts' at-limit rule reads it, so a 99.6 the
//!   tap's row rounds to 100 is not full), whose reset is ahead and within the
//!   window's own length (a reset further off is none of that window's):
//!   [`TapSays::ResetsAt`].
//! * THE NOTICE AND THE TAP ([`timed`]; the notice's reset of a limit only
//!   the transcript shows is its row's `quotaLimits.resetsAt`): within
//!   [`CLOCK_SLACK_S`] of each other the tap's (exact; the notice's is a
//!   minute's clock); further apart
//!   the LATER, and the loop writes a ledger row, never an alert: a wrong
//!   "later" costs minutes, a wrong "earlier" a continuation into the
//!   standing limit.
//! * NO RESET AT ALL (the notice's cut, no window timed): a window a reading
//!   since the first read showed full, then shown ROLLED OVER by a later one
//!   — dropped (Claude Code drops a window at its reset: the run at reset +
//!   1.4 s, E1f) or not full — ends the wait at that reading
//!   ([`TapSays::RolledAt`]); the policy then adds its grace and the
//!   session's jitter, as after any reset. A window that never read full
//!   says nothing: a 429 that carried no figure for it leaves the figure of
//!   an earlier answer, which a wall does not make news.
//!
//! PURE: the loop asks the host for the readings
//! ([`super::IdleHost::tap_window`]) and hands this the answers and its own
//! clock; the turn-end policy reads only `TurnEndReading::reset_at`.

use aterm_phase::WallKind;

use super::limit::{CLOCK_SLACK_S, names_a_session_window};
use crate::harness::tap::Window;

/// One of Claude Code's two usage windows, as its status line names them
/// (`rate_limits.five_hour`, `rate_limits.seven_day`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsageWindow {
    /// The five-hour session window.
    FiveHour,
    /// The seven-day window.
    SevenDay,
}

impl UsageWindow {
    /// Both windows, the five-hour one first.
    pub const ALL: [UsageWindow; 2] = [UsageWindow::FiveHour, UsageWindow::SevenDay];

    /// How long the window is, seconds: a reset further off than this is no
    /// reset of it.
    #[must_use]
    pub fn len_s(self) -> i64 {
        match self {
            UsageWindow::FiveHour => 5 * 3600,
            UsageWindow::SevenDay => 7 * 86_400,
        }
    }

    /// Its name in the status line's payload.
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

    /// THE WINDOWS A WALL OF `kind` IS TIMED BY, its notice saying
    /// `message`: a session window named (`session limit`, `5-hour`) the
    /// five-hour one, a weekly wall the seven-day one, a usage limit that
    /// names no window both (the one that reads full times it), anything
    /// else none — a spend limit too, whose act (R-9's credits rule) hangs on
    /// whether a reset is named.
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

/// What the usage tap's newest reading of the walled process says of one
/// window ([`super::IdleHost::tap_window`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapWindow {
    /// The window's figure, or `None`: the reading names none for it (Claude
    /// Code drops a window from its payload once its reset passes, E1f).
    pub figure: Option<Window>,
    /// When the tap wrote the reading (unix seconds): its file's last write,
    /// which every run moves, an idle re-report too — not the reading's own
    /// `at`, which a re-report keeps from the run that first reported the
    /// figures (`crate::harness::tap::run`).
    pub read_at: i64,
}

/// What the tap says of a wall's reset ([`TapClock::says`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapSays {
    /// Nothing: no reading since the wall's first read times it.
    Nothing,
    /// The wall's window resets then (unix seconds), to the second.
    ResetsAt(i64),
    /// Every window the tap showed full at this wall rolled over, the last as
    /// a reading written then showed (unix seconds).
    RolledAt(i64),
}

/// THE USAGE TAP AT ONE WALL ([`Self::step`]), kept from the loop's first
/// read of it: the windows it showed full there, whether each rolled over
/// since, and the reset it timed.
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
    /// A clock for a wall the loop first read at `first_read` (unix seconds).
    #[must_use]
    pub fn new(first_read: i64) -> TapClock {
        TapClock {
            first_read,
            full: [None, None],
            timed: None,
        }
    }

    /// When the loop first read the wall.
    #[must_use]
    pub fn first_read(&self) -> i64 {
        self.first_read
    }

    /// Fold in the tap's newest reading of each window the wall is timed by
    /// (`None`: no reading at all), at `now` (unix seconds). A reading written
    /// before the wall's first read is ignored. A window whose figure reads
    /// 100.0 % ([`Window::x10`]) is full: its reset is kept, and it TIMES the
    /// wall where the reset is ahead and within the window's length (the later
    /// of two). A full window shown dropped or not full afterwards has rolled
    /// over, until a reading shows it full again. The same reading folded in
    /// again changes nothing.
    pub fn step(&mut self, readings: &[(UsageWindow, Option<TapWindow>)], now: i64) {
        let mut times: Option<i64> = None;
        for &(window, reading) in readings {
            let Some(reading) = reading.filter(|r| r.read_at >= self.first_read) else {
                continue;
            };
            let slot = &mut self.full[window.slot()];
            match reading.figure {
                Some(f) if f.x10() >= 1000 => {
                    // Full (again: a rollover a reading showed before is over).
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

    /// What the readings folded in say of the reset of a wall timed by
    /// `windows`: the reset the tap timed, else — every window shown full
    /// there having rolled over since — when the last did, else nothing.
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

/// The reset a wall is waited to ([`timed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timed {
    /// The reset (unix seconds), or `None`: none known, the policy's back-off.
    pub at: Option<i64>,
    /// The notice's reset and the tap's, where they disagree by more than
    /// [`CLOCK_SLACK_S`] (the later was taken): the loop's ledger row.
    pub disagree: Option<(i64, i64)>,
}

/// THE RESET A WALL IS WAITED TO, from the notice's placed reset (`notice`)
/// and what the tap says: the tap's reset within [`CLOCK_SLACK_S`] of the
/// notice's or with none from the notice, the later of the two further apart
/// ([`Timed::disagree`]); a rollover only where nothing else names a reset;
/// else the notice's. PURE.
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
