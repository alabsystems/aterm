// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Background update-check cadence, sleep/wake detection, and failure log suppression.

use std::time::{Duration, Instant, SystemTime};

#[cfg(test)]
use crate::check_wake::{SLEEP_GAP, is_wake_gap};
pub(crate) use crate::check_wake::{
    Waited, WakeSignal, is_network_unreachable, sleep_watching_for_wake,
};

pub(crate) use crate::cadence_bounds::{
    INTERVAL_SECS, JITTER_PCT, MAX_BACKOFF, MAX_BACKOFF_INTERVALS, WAKE_SETTLE,
};

/// Post-wake grace window during which check failures are treated as transient network bring-up.
pub(crate) const WAKE_GRACE: Duration = Duration::from_secs(2 * 60);

/// Whether `check_began` falls within [`WAKE_GRACE`] of `woke_at`.
pub(crate) fn after_wake(woke_at: Option<Instant>, check_began: Instant) -> bool {
    woke_at.is_some_and(|woke| check_began.saturating_duration_since(woke) <= WAKE_GRACE)
}

/// Suppression window before repeating an unchanged failure log message.
pub(crate) const STILL_FAILING_AFTER: Duration = Duration::from_secs(30 * 60);

/// Quick retry schedule after a network-unreachable check failure.
pub(crate) const OFFLINE_RETRY: [Duration; 4] = [
    Duration::from_secs(20),
    Duration::from_secs(60),
    Duration::from_secs(2 * 60),
    Duration::from_secs(5 * 60),
];

/// Retry interval when the channel head's app bundle build is still in flight.
pub(crate) const IN_FLIGHT_RETRY: Duration = Duration::from_secs(60);

/// Maximum consecutive in-flight retries before reverting to the base cadence.
pub(crate) const IN_FLIGHT_CHECKS: u32 = 30;

/// Retry acquiring this process's check lane without inheriting network backoff.
pub(crate) const LANE_BUSY_RETRY: Duration = Duration::from_secs(5);

/// Return [`IN_FLIGHT_RETRY`] for the first [`IN_FLIGHT_CHECKS`] checks, then `None`.
pub(crate) fn in_flight_retry(checks: u32) -> Option<Duration> {
    (1..=IN_FLIGHT_CHECKS)
        .contains(&checks)
        .then_some(IN_FLIGHT_RETRY)
}

/// Minimum wait duration applied to a sibling-check hold.
pub(crate) const HOLD_FLOOR: Duration = Duration::from_secs(60);

/// Update-check interval schedule with exponential backoff, offline retries, and sibling holds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cadence {
    base: Duration,
    failures: u32,
    /// Consecutive checks, ending with the latest, that could not reach the network.
    offline: u32,
    /// Consecutive completed checks, machine-wide, that found the channel head's app build
    /// in flight, as of this process's latest healthy check; names an [`IN_FLIGHT_RETRY`]
    /// rung, `0` none.
    in_flight: u32,
    /// While in the future, the next wait ends here (capped by [`Self::cap`], floored by
    /// [`HOLD_FLOOR`], unjittered) instead of on the doubling ladder.
    hold: Option<Instant>,
}

impl Cadence {
    /// Choose this wait's schedule without changing the network outcome history.
    /// A busy local lane made no request and may become available at any time.
    pub(crate) fn after_lane(&self, busy: bool) -> Self {
        if busy {
            Self::new(self.base.min(LANE_BUSY_RETRY))
        } else {
            *self
        }
    }

    /// Create a healthy schedule with `base` interval.
    pub(crate) const fn new(base: Duration) -> Self {
        Self {
            base,
            failures: 0,
            offline: 0,
            in_flight: 0,
            hold: None,
        }
    }

    /// Hold the next check until `until` (the end of a sibling's fresh check window).
    pub(crate) const fn hold_until(&mut self, until: Instant) {
        self.hold = Some(until);
    }

    /// Configured base check interval.
    #[cfg(target_os = "macos")]
    pub(crate) const fn base(&self) -> Duration {
        self.base
    }

    /// Record a check failure and advance the exponential backoff.
    pub(crate) const fn failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
        self.offline = 0;
        self.in_flight = 0;
        self.hold = None;
    }

    /// Record a host deferral with optional `window_end` hold.
    pub(crate) fn deferred(&mut self, window_end: Option<Instant>, now: Instant, entropy: u8) {
        self.failed();
        let Some(end) = window_end else {
            return;
        };
        let ladder = self.nominal_at(now);
        let (Some(shortest), Some(drawn)) = (
            now.checked_add(jitter(ladder, 0)),
            now.checked_add(jitter(ladder, entropy)),
        ) else {
            return;
        };
        if shortest >= end {
            return;
        }
        self.hold = Some(end.max(drawn));
    }

    /// Record a network-unreachable check failure ([`OFFLINE_RETRY`]).
    pub(crate) const fn failed_offline(&mut self) {
        self.offline = self.offline.saturating_add(1);
        if self.offline as usize > OFFLINE_RETRY.len() {
            self.failures = self.failures.saturating_add(1);
        }
        self.in_flight = 0;
        self.hold = None;
    }

    /// Whether the next wait uses a quick [`OFFLINE_RETRY`] rung.
    pub(crate) const fn retrying_offline(&self) -> bool {
        self.offline > 0 && self.offline as usize <= OFFLINE_RETRY.len()
    }

    /// Record a successful check and reset backoff (or schedule an in-flight retry).
    pub(crate) const fn succeeded(&mut self, head_in_flight_checks: u32) {
        self.failures = 0;
        self.offline = 0;
        self.in_flight = head_in_flight_checks;
        self.hold = None;
    }

    /// Reset failure backoff and holds after a system wake.
    pub(crate) const fn woke(&mut self) {
        self.failures = 0;
        self.offline = 0;
        self.in_flight = 0;
        self.hold = None;
    }

    /// Consecutive failure count driving exponential backoff.
    #[cfg(test)]
    pub(crate) fn failures(&self) -> u32 {
        self.failures
    }

    /// The remaining hold, if one is set and still in the future at `now`; an expired
    /// hold is simply over (the ladder below applies).
    fn hold_remaining(&self, now: Instant) -> Option<Duration> {
        let until = self.hold?;
        let remaining = until.checked_duration_since(now)?;
        (!remaining.is_zero()).then(|| remaining.max(HOLD_FLOOR).min(self.cap()))
    }

    /// The ceiling on [`Self::nominal_at`]: `max(MAX_BACKOFF, MAX_BACKOFF_INTERVALS × base)`.
    fn cap(&self) -> Duration {
        MAX_BACKOFF.max(self.base.saturating_mul(MAX_BACKOFF_INTERVALS))
    }

    /// Upper bound on a single [`wait`] call including jitter and post-wake settle.
    /// Depends only on the fixed base, so schedule transitions leave it unchanged.
    pub(crate) fn max_wait(&self) -> Duration {
        let cap = u64::try_from(self.cap().as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(cap.saturating_mul(100 + JITTER_PCT) / 100) + WAKE_SETTLE
    }

    /// Pre-jitter wait duration.
    #[cfg(test)]
    pub(crate) fn nominal(&self) -> Duration {
        self.nominal_at(Instant::now())
    }

    /// Compute `Self::nominal` at an injected `now`.
    pub(crate) fn nominal_at(&self, now: Instant) -> Duration {
        if let Some(held) = self.hold_remaining(now) {
            return held;
        }
        let doublings = self.failures.saturating_sub(1).min(20);
        let ladder = self.base.saturating_mul(1u32 << doublings).min(self.cap());
        match self.offline.checked_sub(1) {
            Some(rung) if self.retrying_offline() => OFFLINE_RETRY[rung as usize].min(ladder),
            _ => in_flight_retry(self.in_flight).map_or(ladder, |rung| rung.min(ladder)),
        }
    }

    /// Jittered wait duration using random byte `entropy`.
    pub(crate) fn delay(&self, entropy: u8) -> Duration {
        self.delay_at(Instant::now(), entropy)
    }

    /// Compute [`Self::delay`] at an injected `now`.
    pub(crate) fn delay_at(&self, now: Instant, entropy: u8) -> Duration {
        if let Some(held) = self.hold_remaining(now) {
            return held;
        }
        jitter(self.nominal_at(now), entropy)
    }
}

/// Spread `d` by ±[`JITTER_PCT`]% using one random byte; saturating, so it cannot panic.
fn jitter(d: Duration, entropy: u8) -> Duration {
    let span = 2 * JITTER_PCT;
    let offset = (u64::from(entropy) * (span + 1)) / 256;
    let scale = 100 - JITTER_PCT + offset;
    let millis = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(millis.saturating_mul(scale) / 100)
}

/// Sample one random byte for jitter, falling back to `128` (no jitter).
#[cfg(target_os = "macos")]
pub(crate) fn entropy_byte() -> u8 {
    let mut b = [128u8; 1];
    let _ = aterm_uds::rand::fill(&mut b);
    b[0]
}

/// Wait one jittered `cadence` interval while watching `signal` and wall-clock jumps.
#[cfg(target_os = "macos")]
pub(crate) fn wait(
    cadence: &Cadence,
    signal: &WakeSignal,
    seen: u64,
    watch: &crate::checker_watch::CheckerWatch,
    generation: u64,
) -> (Duration, Waited) {
    wait_with(
        cadence,
        entropy_byte(),
        watch,
        generation,
        SystemTime::now(),
        |delay| sleep_watching_for_wake(signal, seen, delay, || watch.is_current(generation)),
    )
}

/// [`wait`] over an injected sleep. The next check is published only while the wait
/// lasts; after it the loop is settling or checking and the instant is past.
fn wait_with(
    cadence: &Cadence,
    entropy: u8,
    watch: &crate::checker_watch::CheckerWatch,
    generation: u64,
    now: SystemTime,
    sleep: impl FnOnce(Duration) -> Waited,
) -> (Duration, Waited) {
    let delay = cadence.delay(entropy);
    let at = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.saturating_add(delay).as_secs());
    let _published = watch.publish_next_check(generation, at);
    let waited = sleep(delay);
    (delay, waited)
}

/// Unix timestamp of the next scheduled check, or `None` when not waiting.
#[cfg(target_os = "macos")]
pub(crate) fn next_check_unix() -> Option<u64> {
    crate::checker_watch::WATCH.next_check_unix()
}

/// Log action chosen for an update-check result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LogAction {
    /// Suppress repeated identical failure output.
    Suppress,
    /// Log at `WARN` level.
    Warn(String),
    /// Log at `INFO` level.
    Log(String),
}

/// Deduplicating logger state for consecutive update-check failures.
#[derive(Debug, Default)]
pub(crate) struct FailureLog {
    last: Option<String>,
    /// Whether the last emitted line was a warning: a failure first said at INFO is
    /// warned the moment it stops being expected.
    last_warned: bool,
    /// Failures observed since the last emitted line (the first one included).
    since_emit: u32,
    /// Total consecutive failures in the current outage.
    streak: u32,
    emitted_at: Option<Instant>,
}

impl FailureLog {
    /// Record a failed check and return its [`LogAction`].
    pub(crate) fn failure(&mut self, message: &str) -> LogAction {
        self.failure_expected(message, false)
    }

    /// Record a failed check, logging at `INFO` instead of `WARN` while `expected` is true.
    pub(crate) fn failure_expected(&mut self, message: &str, expected: bool) -> LogAction {
        self.streak = self.streak.saturating_add(1);
        self.since_emit = self.since_emit.saturating_add(1);
        let changed = self.last.as_deref() != Some(message);
        let stale = self
            .emitted_at
            .is_none_or(|t| t.elapsed() >= STILL_FAILING_AFTER);
        let escalated = !expected && !self.last_warned;
        if !changed && !stale && !escalated {
            return LogAction::Suppress;
        }
        let suppressed = self.since_emit.saturating_sub(1);
        self.last = Some(message.to_string());
        self.last_warned = !expected;
        self.since_emit = 0;
        self.emitted_at = Some(Instant::now());
        let line = if suppressed == 0 {
            format!("update check failed: {message}")
        } else {
            format!("update check failing, {} in a row: {message}", self.streak)
        };
        if expected {
            LogAction::Log(line)
        } else {
            LogAction::Warn(line)
        }
    }

    /// Record a successful check, returning a recovery log line if an outage ended.
    pub(crate) fn success(&mut self) -> Option<LogAction> {
        let streak = std::mem::take(&mut self.streak);
        self.last = None;
        self.last_warned = false;
        self.since_emit = 0;
        self.emitted_at = None;
        (streak > 0).then(|| {
            LogAction::Log(if streak == 1 {
                "update check recovered after 1 failure".to_string()
            } else {
                format!("update check recovered after {streak} failures")
            })
        })
    }
}

/// Update `cadence` and `log` for a failed check at `check_began`.
pub(crate) fn note_failure(
    schedule: &mut Cadence,
    log: &mut FailureLog,
    message: &str,
    offline: bool,
) -> LogAction {
    if offline {
        schedule.failed_offline();
        log.failure_expected(message, schedule.retrying_offline())
    } else {
        schedule.failed();
        log.failure(message)
    }
}

#[cfg(test)]
#[path = "cadence_tests.rs"]
mod tests;
