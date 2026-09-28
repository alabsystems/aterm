// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The background check's schedule constants and the bounds derived from them.
//!
//! Split out of `cadence` (the schedule itself, macOS-only) because
//! [`crate::STALE_CHECK_AFTER`] is public on every target — `aterm ctl update
//! status` flags a frozen ledger on Linux too — and it is DERIVED from these, so a
//! cadence change moves it. `cadence` re-exports the schedule's own constants, so
//! the schedule reads them under their old names.

use std::time::Duration;

/// The FLOOR on the backoff ceiling — i.e. the ceiling that applies to a fast base
/// interval. Fifteen minutes is long enough that an offline laptop costs ~4 log lines
/// an hour instead of 48, and short enough that reconnecting still gets an update
/// within a coffee break.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// How many base intervals the backoff may grow to. The real ceiling is
/// `max(MAX_BACKOFF, MAX_BACKOFF_INTERVALS × base)` — see `cadence::Cadence::cap`.
///
/// The ceiling used to be [`MAX_BACKOFF`] alone, raised to the base so that an
/// operator's long interval could never be silently SHORTENED by it:
/// `min(MAX_BACKOFF.max(base))`. On the slow (then anonymous, now web) lane that
/// expression was arithmetically inert — its base was 15 minutes, which IS
/// `MAX_BACKOFF`, so the ceiling equalled the base and every doubling was clamped
/// straight back down to it: the one lane that most needed to retreat while failing
/// was the one lane with no backoff at all, and the same silent no-op applied to any
/// operator interval at or above the cap.
/// A ceiling expressed in INTERVALS is inert for no base: four of them is a real
/// retreat (10 min → 20 → 40 at today's interval) while bounding the worst
/// case at 4× the cadence — and a wake,
/// or one healthy check, still snaps all the way back to the base, so recovery is
/// never rate-limited by the cap.
pub(crate) const MAX_BACKOFF_INTERVALS: u32 = 4;

/// Jitter applied to every wait, as a percentage either side of the nominal delay.
pub(crate) const JITTER_PCT: u64 = 20;

/// How long to let the network come up after a detected wake before checking. A Mac
/// takes a few seconds to associate Wi-Fi and re-resolve DNS; checking inside that
/// window is a guaranteed failure that teaches the ledger nothing.
pub(crate) const WAKE_SETTLE: Duration = Duration::from_secs(20);

/// The base interval of the background check — one cadence, no knob.
///
/// A check spends ZERO metered requests: its steady state is one HEAD of
/// `github.com/…/releases/latest/download/aterm-appcast.toml` (a 302 with no
/// `x-ratelimit-*` header at all — measured 2026-09-03), and a moved pointer adds only
/// tag-specific GETs on the same unmetered host. There is no per-IP budget to share,
/// so the interval is a courtesy to the web host and a bound on how long a new release
/// waits to be found: a release published now is STAGED by a running aterm within one
/// interval (plus jitter, plus the download), and the in-session apply lane lands it
/// within `aterm-gui`'s `LANDS_WITHIN` (and its switch, under a minute together) of
/// that — so publish-to-applied on a healthy running window is bounded by
/// `INTERVAL_SECS × 1.2 + download + LANDS_WITHIN`, about 13 minutes plus the
/// download, not "a minute".
///
/// Ten minutes (2026-09-23; it was thirty) because a verified update installs by
/// itself soon after it is staged, so the check is what decides how soon a release
/// lands — and the owner wants it to land promptly. It stays one HEAD per machine per
/// interval however many aterm processes run: `checker.lock` and the 70 % freshness
/// window in the check loop dedupe every sibling.
pub(crate) const INTERVAL_SECS: u64 = 10 * 60;

/// The longest wait the check loop legitimately takes between two checks: the backoff
/// ceiling at [`INTERVAL_SECS`] (`cadence::Cadence::cap`), stretched by the full +[`JITTER_PCT`]%,
/// plus the post-wake [`WAKE_SETTLE`]. A sibling-skip hold is bounded by the same
/// ceiling and not jittered, and an `cadence::OFFLINE_RETRY` or `cadence::IN_FLIGHT_RETRY` rung is
/// never longer than the ladder, so none of them can exceed it. 48 min 20 s today; pinned against the real
/// `cadence::Cadence::delay_at` by `no_wait_exceeds_the_longest_wait`.
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
/// ledger is frozen (`stale_check=<stamp>`): five [`LONGEST_WAIT`]s, about four hours.
/// The margin over one wait is for the check itself — a container download has no
/// wall-clock cap, only a stall detector — so the token names a loop that has STOPPED,
/// never one that is merely backed off or mid-download. Derived here, from the
/// constants that make the schedule, so a cadence change moves it too (it used to be a
/// literal `4 * 3600` in `aterm-gui`'s `control.rs` beside a comment doing this sum).
pub(crate) const STALE_CHECK_AFTER: Duration = Duration::from_secs(LONGEST_WAIT.as_secs() * 5);

const _: () = assert!(
    STALE_CHECK_AFTER.as_secs() >= 4 * LONGEST_WAIT.as_secs(),
    "a frozen-ledger flag inside a few legitimate waits would cry wolf on a backed-off loop"
);
