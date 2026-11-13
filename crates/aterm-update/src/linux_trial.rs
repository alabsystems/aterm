// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Linux trial launch budget, settle window, and per-lane confirmation
//! ([`crate::LINUX_TRIAL_LAUNCHES`], [`START_SETTLES_SECS`], [`crate::LINUX_SESSION_SILENT_DEFER`],
//! [`start_stamp`], [`judged_now`], [`Lanes`]).

/// How long after a start it may still confirm itself.
pub(crate) const START_SETTLES_SECS: i64 = 20;

/// Whether the start stamped at `previous_start_unix` may still confirm itself at `now_unix`.
#[inline]
pub(crate) fn start_is_young(previous_start_unix: i64, now_unix: i64) -> bool {
    previous_start_unix > 0
        && (0..START_SETTLES_SECS).contains(&now_unix.saturating_sub(previous_start_unix))
}

/// Whether a silent session's deferral moves the trial's stamp.
#[inline]
pub(crate) fn defer_moves(own_unix: i64, now_unix: i64) -> bool {
    now_unix > own_unix && start_is_young(own_unix, now_unix)
}

/// Whether the start just counted rolls the trial back.
#[inline]
pub(crate) fn start_rolls_back(starts: u32, previous_start_unix: i64, now_unix: i64) -> bool {
    starts > crate::LINUX_TRIAL_LAUNCHES && !start_is_young(previous_start_unix, now_unix)
}

/// The stamp the start just counted (`starts` includes it) leaves for the next start to be
/// judged against ([`start_rolls_back`]): its own time while the trial is within its
/// budget, the stamp as it stood once past it.
#[inline]
pub(crate) fn start_stamp<T>(starts: u32, previous_start: T, now: T) -> T {
    if starts <= crate::LINUX_TRIAL_LAUNCHES {
        now
    } else {
        previous_start
    }
}

/// When a start was counted, as the record keeps it and the next start reads it ([`judged_now`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StartClock {
    /// Wall-clock Unix seconds.
    pub(crate) unix: i64,
    /// The same moment on a clock no step moves, where the system has one to read.
    pub(crate) steady: Option<Steady>,
}

impl From<i64> for StartClock {
    /// A wall-clock reading alone: what an older build's record holds, and what a
    /// system with no steady clock to read gives.
    fn from(unix: i64) -> Self {
        Self { unix, steady: None }
    }
}

/// Whole seconds on the system's monotonic clock.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Steady {
    /// The boot (`/proc/sys/kernel/random/boot_id`) and time namespace.
    pub(crate) clock: String,
    /// Seconds on it.
    pub(crate) secs: i64,
}

/// `now` as the wall clock that stamped `previous` reads it.
pub(crate) fn judged_now(previous: &StartClock, now: &StartClock) -> i64 {
    match (&previous.steady, &now.steady) {
        (Some(then), Some(steady)) if previous.unix > 0 && then.clock == steady.clock => previous
            .unix
            .saturating_add(steady.secs.saturating_sub(then.secs)),
        _ => now.unix,
    }
}

/// Whether the start just counted (`starts` includes it) finds the trial past its budget.
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
pub(crate) fn stamp_lost(starts: u32, previous_start_unix: i64) -> bool {
    previous_start_unix == 0
        && (crate::LINUX_TRIAL_LAUNCHES + 1
            ..=crate::LINUX_TRIAL_LAUNCHES.saturating_add(LOST_STAMP_SPARES))
            .contains(&starts)
}

/// How many starts past the budget may each find the stamp lost and be spared ([`stamp_lost`]).
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
pub(crate) const LOST_STAMP_SPARES: u32 = 8;

/// Which kind of start of a replaced executable counts or confirms its trial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    /// The GUI entry with a window.
    Window,
    /// An interactive terminal session, or a `--headless` instance.
    Session,
}

/// Where a trial's two lanes stand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lanes {
    /// A start of either lane confirmed the trial.
    pub(crate) healthy: bool,
    /// A window start of the trial confirmed it.
    pub(crate) window_proven: bool,
}

/// Whether a start from `lane` counts against the trial.
#[inline]
pub(crate) const fn start_counts(lane: Lane, lanes: Lanes) -> bool {
    match lane {
        Lane::Window => !lanes.window_proven,
        Lane::Session => !lanes.healthy,
    }
}

/// What one start's confirmation, from `lane`, does to the trial ([`confirm`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaneConfirm {
    /// It confirms: `lanes` is where the trial now stands.
    Confirms { lanes: Lanes, settles: bool },
    /// Nothing is waiting on this lane.
    NotPending,
}

/// A start from `lane` of the file on trial proved healthy.
#[inline]
pub(crate) fn confirm(lane: Lane, lanes: Lanes) -> LaneConfirm {
    if !start_counts(lane, lanes) {
        return LaneConfirm::NotPending;
    }
    LaneConfirm::Confirms {
        lanes: Lanes {
            healthy: true,
            window_proven: lanes.window_proven || lane == Lane::Window,
        },
        settles: !lanes.healthy,
    }
}

/// Whether a newer release installed over this trial keeps the trial's own rollback target instead.
#[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
#[inline]
pub(crate) const fn installed_window_unproven(lanes: Lanes, starts: u32) -> bool {
    lanes.healthy && !lanes.window_proven && starts > 0
}

#[cfg(test)]
#[path = "linux_trial_tests.rs"]
mod tests;
