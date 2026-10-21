// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The background check's schedule constants and the bounds derived from them.
//!
//! Split out of `cadence` (macOS-only) because [`crate::STALE_CHECK_AFTER`] is public on
//! every target (`aterm ctl update status` flags a frozen ledger on Linux too) and is
//! derived from these. `cadence` re-exports the schedule's own constants.

use std::time::Duration;

/// The floor on the backoff ceiling, i.e. the ceiling for a fast base interval: an
/// offline laptop logs ~4 lines an hour instead of 48, and reconnecting still updates
/// within a coffee break.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// How many base intervals the backoff may grow to: the ceiling is
/// `max(MAX_BACKOFF, MAX_BACKOFF_INTERVALS × base)` (`cadence::Cadence::cap`).
///
/// A ceiling of [`MAX_BACKOFF`] alone (`min(MAX_BACKOFF.max(base))`) is arithmetically
/// inert whenever the base is at or above `MAX_BACKOFF` — the ceiling equals the base
/// and every doubling is clamped straight back down to it, leaving no backoff at all.
/// Expressing the ceiling in intervals gives a slow base a real retreat (an hour → two →
/// four) while bounding the worst case at 4× the cadence. At the two-minute interval
/// [`MAX_BACKOFF`] is the larger term, so a failing check retreats 2 min → 4 → 8 → 15.
/// A wake, or one healthy check, still snaps all the way back to the base, so recovery
/// is never rate-limited by the cap.
pub(crate) const MAX_BACKOFF_INTERVALS: u32 = 4;

/// Jitter applied to every wait, as a percentage either side of the nominal delay.
pub(crate) const JITTER_PCT: u64 = 20;

/// How long to let the network come up after a detected wake before checking. A Mac
/// takes a few seconds to associate Wi-Fi and re-resolve DNS. Short
/// because a release published overnight is found by the first check after the lid
/// opens: a check that still meets a half-up network is no fault, since any failure
/// inside `cadence::WAKE_GRACE` takes the quick `cadence::OFFLINE_RETRY` rung at INFO
/// and never doubles the backoff.
pub(crate) const WAKE_SETTLE: Duration = Duration::from_secs(3);

/// The base interval of the background check; there is no knob.
///
/// A check spends no metered requests: its steady state is one HEAD of
/// `github.com/…/releases/latest/download/aterm-appcast.toml` (a 302 with no
/// `x-ratelimit-*` header), and a moved pointer adds only
/// tag-specific GETs on the same unmetered host. There is no per-IP budget to share,
/// so the interval is a courtesy to the web host and a bound on how long a new release
/// waits to be found: a release published now is staged by a running aterm within one
/// interval (plus jitter, plus the download), and the in-session apply lane lands it
/// within `aterm-gui`'s `LANDS_WITHIN` (and its switch, under a minute together) of
/// that — so publish-to-applied on a healthy running window is bounded by
/// `INTERVAL_SECS × 1.2 + download + LANDS_WITHIN`, about three and a half minutes
/// plus the download (a mean of about one minute to find the release).
///
/// Two minutes because a verified update installs by itself soon after it is staged,
/// so the check is what decides how soon a release lands. It
/// stays one HEAD per machine per interval however many aterm processes run:
/// `checker.lock` and the 70 % freshness window in the check loop dedupe every
/// sibling. What is counted in checks rather than time is re-expressed in time so a
/// faster cadence cannot shorten it: a failure class is called persistent only once
/// its streak spans `PERSISTENT_SPAN` (health ledger), and a host's 429 holds the
/// whole machine off for at least `DEFERRED_WINDOW_FLOOR` (check loop).
pub(crate) const INTERVAL_SECS: u64 = 2 * 60;

/// The longest wait the check loop legitimately takes between two checks: the backoff
/// ceiling at [`INTERVAL_SECS`] (`cadence::Cadence::cap`), stretched by the full +[`JITTER_PCT`]%,
/// plus the post-wake [`WAKE_SETTLE`]. A sibling-skip hold is bounded by the same
/// ceiling and not jittered, and an `cadence::OFFLINE_RETRY` or `cadence::IN_FLIGHT_RETRY` rung is
/// never longer than the ladder, so none of them can exceed it. 18 min 3 s today (the
/// 15-minute [`MAX_BACKOFF`] floor is the ceiling at a two-minute base); pinned
/// against the real `cadence::Cadence::delay_at` by
/// `max_wait_bounds_every_delay_the_schedule_can_take`.
pub(crate) const LONGEST_WAIT: Duration = {
    let base = INTERVAL_SECS * MAX_BACKOFF_INTERVALS as u64;
    let ceiling = if base > MAX_BACKOFF.as_secs() {
        base
    } else {
        MAX_BACKOFF.as_secs()
    };
    Duration::from_secs(ceiling * (100 + JITTER_PCT) / 100 + WAKE_SETTLE.as_secs())
};

/// How old the last completed check may be before `aterm ctl update status` says the
/// ledger is frozen (`stale_check=<stamp>`): five [`LONGEST_WAIT`]s, and never under
/// four hours.
/// The margin over one wait is for the check itself — a container download has no
/// wall-clock cap, only a stall detector — so the token names a loop that has stopped,
/// never one that is merely backed off or mid-download. Derived here from the
/// constants that make the schedule so a cadence change moves it too.
/// The floor is the Linux lane's, which reads the same bound: its healthy interval is
/// half an hour and its failures back off for hours, so a faster macOS cadence must
/// not call a backed-off Linux loop frozen.
pub(crate) const STALE_CHECK_AFTER: Duration = {
    let derived = LONGEST_WAIT.as_secs() * 5;
    let floor = 4 * 3600;
    Duration::from_secs(if derived > floor { derived } else { floor })
};

const _: () = assert!(
    STALE_CHECK_AFTER.as_secs() >= 4 * LONGEST_WAIT.as_secs(),
    "a frozen-ledger flag inside a few legitimate waits would cry wolf on a backed-off loop"
);
