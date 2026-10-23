// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Background update check schedule constants and derived bounds, shared across targets (`cadence` is macOS-only, while [`crate::STALE_CHECK_AFTER`] is public everywhere).

use std::time::Duration;

/// Minimum backoff ceiling for fast base intervals (15 minutes).
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// Maximum base-interval multiple for the backoff ceiling (`max(MAX_BACKOFF, MAX_BACKOFF_INTERVALS * base)`), so base intervals at or above [`MAX_BACKOFF`] still back off up to 4x.
pub(crate) const MAX_BACKOFF_INTERVALS: u32 = 4;

/// Jitter applied to every wait, as a percentage either side of the nominal delay.
pub(crate) const JITTER_PCT: u64 = 20;

/// Post-wake settle delay before checking, giving Wi-Fi and DNS a few seconds to come up.
pub(crate) const WAKE_SETTLE: Duration = Duration::from_secs(3);

/// Base background check interval (2 minutes), deduplicated across sibling processes by `checker.lock` and the 70% freshness window.
pub(crate) const INTERVAL_SECS: u64 = 2 * 60;

/// Longest delay the check loop legitimately takes between checks: the backoff ceiling at [`INTERVAL_SECS`] plus [`JITTER_PCT`]% jitter and [`WAKE_SETTLE`].
pub(crate) const LONGEST_WAIT: Duration = {
    let base = INTERVAL_SECS * MAX_BACKOFF_INTERVALS as u64;
    let ceiling = if base > MAX_BACKOFF.as_secs() {
        base
    } else {
        MAX_BACKOFF.as_secs()
    };
    Duration::from_secs(ceiling * (100 + JITTER_PCT) / 100 + WAKE_SETTLE.as_secs())
};

/// Maximum age of the last completed check before `aterm ctl update status` reports `stale_check`: five [`LONGEST_WAIT`]s, floored at four hours so backed-off Linux loops are not flagged frozen.
pub(crate) const STALE_CHECK_AFTER: Duration = {
    let derived = LONGEST_WAIT.as_secs() * 5;
    let floor = 4 * 3600;
    Duration::from_secs(if derived > floor { derived } else { floor })
};

const _: () = assert!(
    STALE_CHECK_AFTER.as_secs() >= 4 * LONGEST_WAIT.as_secs(),
    "a frozen-ledger flag inside a few legitimate waits would cry wolf on a backed-off loop"
);
