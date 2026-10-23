// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Background update-check cadence, sleep/wake detection, and failure log suppression.

use std::time::{Duration, Instant, SystemTime};

pub(crate) use crate::cadence_bounds::{
    INTERVAL_SECS, JITTER_PCT, MAX_BACKOFF, MAX_BACKOFF_INTERVALS, WAKE_SETTLE,
};

/// Wall-clock excess over a requested sleep slice that indicates the machine was asleep.
pub(crate) const SLEEP_GAP: Duration = Duration::from_secs(90);

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

/// Whether an update-check error `message` indicates an unreachable network.
pub(crate) fn is_network_unreachable(message: &str) -> bool {
    const UNREACHABLE: [u32; 3] = [6, 7, 28];
    if !message.contains("curl") {
        return false;
    }
    let code_after = |marker: &str| {
        message.match_indices(marker).any(|(at, _)| {
            let digits: String = message[at + marker.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits
                .parse::<u32>()
                .is_ok_and(|code| UNREACHABLE.contains(&code))
        })
    };
    code_after("curl: (") || code_after("exit status: ") || code_after("(exit ")
}

/// Retry interval when the channel head's app bundle build is still in flight.
pub(crate) const IN_FLIGHT_RETRY: Duration = Duration::from_secs(60);

/// Maximum consecutive in-flight retries before reverting to the base cadence.
pub(crate) const IN_FLIGHT_CHECKS: u32 = 30;

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
    in_flight: u32,
    /// While in the future, the next wait ends here (capped by [`Self::cap`], floored by
    hold: Option<Instant>,
}

impl Cadence {
    /// Create a healthy schedule with `base` interval.
    pub(crate) fn new(base: Duration) -> Self {
        Self {
            base,
            failures: 0,
            offline: 0,
            in_flight: 0,
            hold: None,
        }
    }

    /// Hold the next check until `until` (the end of a sibling's fresh check window).
    pub(crate) fn hold_until(&mut self, until: Instant) {
        self.hold = Some(until);
    }

    /// Configured base check interval.
    #[cfg(target_os = "macos")]
    pub(crate) fn base(&self) -> Duration {
        self.base
    }

    /// Record a check failure and advance the exponential backoff.
    pub(crate) fn failed(&mut self) {
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
    pub(crate) fn failed_offline(&mut self) {
        self.offline = self.offline.saturating_add(1);
        if self.offline as usize > OFFLINE_RETRY.len() {
            self.failures = self.failures.saturating_add(1);
        }
        self.in_flight = 0;
        self.hold = None;
    }

    /// Whether the next wait uses a quick [`OFFLINE_RETRY`] rung.
    pub(crate) fn retrying_offline(&self) -> bool {
        self.offline > 0 && self.offline as usize <= OFFLINE_RETRY.len()
    }

    /// Record a successful check and reset backoff (or schedule an in-flight retry).
    pub(crate) fn succeeded(&mut self, head_in_flight_checks: u32) {
        self.failures = 0;
        self.offline = 0;
        self.in_flight = head_in_flight_checks;
        self.hold = None;
    }

    /// Reset failure backoff and holds after a system wake.
    pub(crate) fn woke(&mut self) {
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
    fn hold_remaining(&self, now: Instant) -> Option<Duration> {
        let until = self.hold?;
        let remaining = until.checked_duration_since(now)?;
        Some(remaining.max(HOLD_FLOOR).min(self.cap()))
    }

    /// The ceiling on [`Self::nominal`]: `max(MAX_BACKOFF, MAX_BACKOFF_INTERVALS × base)`.
    fn cap(&self) -> Duration {
        MAX_BACKOFF.max(self.base.saturating_mul(MAX_BACKOFF_INTERVALS))
    }

    /// Upper bound on a single [`wait`] call including jitter and post-wake settle.
    pub(crate) fn max_wait(&self) -> Duration {
        let cap = u64::try_from(self.cap().as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(cap.saturating_mul(100 + JITTER_PCT) / 100) + WAKE_SETTLE
    }

    /// Pre-jitter wait duration.
    #[cfg(test)]
    pub(crate) fn nominal(&self) -> Duration {
        self.nominal_at(Instant::now())
    }

    /// Compute [`Self::nominal`] at an injected `now`.
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

/// Outcome of a cadence sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waited {
    /// Full delay elapsed normally.
    Elapsed,
    /// System sleep/wake gap of the given duration was detected.
    Woke(Duration),
}

/// Whether `observed` wall-clock elapsed time exceeds `slice` by at least [`SLEEP_GAP`].
pub(crate) fn is_wake_gap(slice: Duration, observed: Duration) -> bool {
    observed.saturating_sub(slice) >= SLEEP_GAP
}

/// Cross-thread signal notified when the OS reports a system wake.
pub(crate) struct WakeSignal {
    posts: std::sync::Mutex<u64>,
    posted: std::sync::Condvar,
}

impl WakeSignal {
    pub(crate) const fn new() -> Self {
        Self {
            posts: std::sync::Mutex::new(0),
            posted: std::sync::Condvar::new(),
        }
    }

    /// Notify all waiters that the system woke.
    pub(crate) fn post(&self) {
        let mut posts = self
            .posts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *posts = posts.wrapping_add(1);
        self.posted.notify_all();
    }

    /// Total wake notifications posted so far.
    pub(crate) fn posts(&self) -> u64 {
        *self
            .posts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait up to `timeout` for a post past the `seen`-th; whether one came.
    fn wait_past(&self, seen: u64, timeout: Duration) -> bool {
        let posts = self
            .posts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (posts, _) = self
            .posted
            .wait_timeout_while(posts, timeout, |posts| *posts == seen)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *posts != seen
    }
}

/// Process-wide system wake signal.
#[cfg(target_os = "macos")]
pub(crate) static SYSTEM_WOKE: WakeSignal = WakeSignal::new();

/// Maximum sleep slice used by [`sleep_watching_for_wake`].
pub(crate) const WAKE_SLICE: Duration = Duration::from_secs(5);

/// Sleep for `total` in [`WAKE_SLICE`] increments, returning early on `signal` or a wall-clock jump.
pub(crate) fn sleep_watching_for_wake(signal: &WakeSignal, seen: u64, total: Duration) -> Waited {
    let mut remaining = total;
    while !remaining.is_zero() {
        let slice = remaining.min(WAKE_SLICE);
        let before = SystemTime::now();
        let ran = Instant::now();
        let posted = signal.wait_past(seen, slice);
        let observed = before.elapsed().unwrap_or(slice);
        if posted {
            // The wall clock's lead over this thread's own running time, for the log.
            return Waited::Woke(observed.saturating_sub(ran.elapsed()));
        }
        if is_wake_gap(slice, observed) {
            return Waited::Woke(observed.saturating_sub(slice));
        }
        remaining = remaining.saturating_sub(slice);
    }
    Waited::Elapsed
}

/// Wait one jittered `cadence` interval while watching `signal` and wall-clock jumps.
#[cfg(target_os = "macos")]
pub(crate) fn wait(cadence: &Cadence, signal: &WakeSignal, seen: u64) -> (Duration, Waited) {
    wait_with(cadence, entropy_byte(), |delay| {
        sleep_watching_for_wake(signal, seen, delay)
    })
}

/// [`wait`] over an injected sleep. The next check is published only while the wait
fn wait_with(
    cadence: &Cadence,
    entropy: u8,
    sleep: impl FnOnce(Duration) -> Waited,
) -> (Duration, Waited) {
    let delay = cadence.delay(entropy);
    note_next_check(SystemTime::now(), delay);
    let waited = sleep(delay);
    NEXT_CHECK_UNIX.store(0, std::sync::atomic::Ordering::Relaxed);
    (delay, waited)
}

/// When this process's check loop looks next, as Unix seconds (`0`: no wait under way),
static NEXT_CHECK_UNIX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Record that the loop's next check is `delay` after `now`.
fn note_next_check(now: SystemTime, delay: Duration) {
    let at = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.saturating_add(delay).as_secs());
    NEXT_CHECK_UNIX.store(at, std::sync::atomic::Ordering::Relaxed);
}

/// Unix timestamp of the next scheduled check, or `None` when not waiting.
pub(crate) fn next_check_unix() -> Option<u64> {
    Some(NEXT_CHECK_UNIX.load(std::sync::atomic::Ordering::Relaxed)).filter(|at| *at > 0)
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
mod tests {
    use super::*;
    use crate::cadence_bounds::LONGEST_WAIT;

    const BASE: Duration = Duration::from_secs(75);

    /// `max_wait` is what the heartbeat is judged against while the loop waits, so
    #[test]
    fn max_wait_bounds_every_delay_the_schedule_can_take() {
        let now = Instant::now();
        for base in [BASE, Duration::from_secs(INTERVAL_SECS)] {
            let mut schedules = Vec::new();
            let mut c = Cadence::new(base);
            for _ in 0..24 {
                schedules.push(c);
                c.failed();
            }
            let mut offline = Cadence::new(base);
            for _ in 0..8 {
                offline.failed_offline();
                schedules.push(offline);
            }
            for checks in [0, 1, 2, IN_FLIGHT_CHECKS, IN_FLIGHT_CHECKS + 1, u32::MAX] {
                let mut in_flight = Cadence::new(base);
                in_flight.succeeded(checks);
                schedules.push(in_flight);
            }
            let mut held = Cadence::new(base);
            held.hold_until(now + Duration::from_secs(24 * 3600));
            schedules.push(held);
            for window in [base * 14 / 10, Duration::from_secs(24 * 3600)] {
                for entropy in [0u8, 255] {
                    let mut deferred = Cadence::new(base);
                    deferred.deferred(Some(now + window), now, entropy);
                    schedules.push(deferred);
                }
            }
            for s in schedules {
                for entropy in 0..=u8::MAX {
                    assert!(
                        s.delay_at(now, entropy) + WAKE_SETTLE <= s.max_wait(),
                        "{s:?} at entropy {entropy}: {:?} + settle > {:?}",
                        s.delay_at(now, entropy),
                        s.max_wait()
                    );
                }
            }
        }
        assert_eq!(
            Cadence::new(Duration::from_secs(INTERVAL_SECS)).max_wait(),
            LONGEST_WAIT
        );
    }

    #[test]
    fn backoff_doubles_per_failure_and_caps() {
        let mut c = Cadence::new(BASE);
        c.failed();
        assert_eq!(
            c.nominal(),
            BASE,
            "the first retry is still the base interval"
        );
        c.failed();
        assert_eq!(c.nominal(), BASE * 2);
        c.failed();
        assert_eq!(c.nominal(), BASE * 4);
        for _ in 0..1000 {
            c.failed();
        }
        assert_eq!(c.nominal(), MAX_BACKOFF, "backoff is capped, not unbounded");
    }

    /// A network that is not up yet — the boot and wake case — is retried after 20 s,
    #[test]
    fn an_unreachable_network_is_retried_on_the_short_ladder_then_the_base() {
        let base = Duration::from_secs(INTERVAL_SECS);
        let mut c = Cadence::new(base);
        let mut waits = Vec::new();
        for _ in 0..OFFLINE_RETRY.len() {
            c.failed_offline();
            assert!(c.retrying_offline());
            waits.push(c.nominal());
        }
        assert_eq!(
            waits,
            [20, 60, 120, 120].map(Duration::from_secs),
            "20 s, 60 s, then the 2-minute base"
        );
        assert_eq!(
            c.failures(),
            0,
            "the quick rungs are not the doubling ladder's"
        );
        c.failed_offline();
        assert!(!c.retrying_offline());
        assert_eq!(c.nominal(), base, "then the base interval");
        c.failed_offline();
        assert_eq!(
            c.nominal(),
            base * 2,
            "and the ordinary doubling from there"
        );
        c.succeeded(0);
        assert!(!c.retrying_offline());
        assert_eq!(c.nominal(), base, "a success ends it");
        c.failed_offline();
        assert_eq!(
            c.nominal(),
            OFFLINE_RETRY[0],
            "and the next outage starts over"
        );
        c.woke();
        assert!(!c.retrying_offline());
        assert_eq!(c.nominal(), base, "a wake ends it too");
    }

    #[test]
    fn the_short_ladder_never_outwaits_the_ordinary_one() {
        let mut c = Cadence::new(BASE);
        let mut waits = Vec::new();
        for _ in 0..OFFLINE_RETRY.len() {
            c.failed_offline();
            waits.push(c.nominal());
        }
        assert_eq!(waits, [20, 60, 75, 75].map(Duration::from_secs));
        c.failed_offline();
        assert_eq!(c.nominal(), BASE);
    }

    #[test]
    fn an_ordinary_failure_ends_the_short_ladder() {
        let base = Duration::from_secs(INTERVAL_SECS);
        let mut c = Cadence::new(base);
        c.failed_offline();
        c.failed_offline();
        assert_eq!(c.nominal(), OFFLINE_RETRY[1]);
        c.failed();
        assert!(!c.retrying_offline());
        assert_eq!(c.nominal(), base, "the ordinary ladder's first rung");
    }

    /// A network still coming up after a wake is no fault, whatever it failed on. The
    #[test]
    fn a_failure_inside_the_wake_grace_takes_the_quick_rungs_at_info_whatever_it_was() {
        let base = Duration::from_secs(INTERVAL_SECS);
        let woke = Instant::now();
        for message in [
            "curl HEAD https://github.com/alabsystems/aterm/releases/latest/download/\
             aterm-appcast.toml failed (exit status: 35): curl: (35) LibreSSL SSL_connect: \
             SSL_ERROR_SYSCALL in connection to github.com:443",
            "curl HEAD https://github.com/x failed (exit status: 56): curl: (56) Recv \
             failure: Connection reset by peer",
            "curl HEAD https://github.com/x failed (exit status: 22): curl: (22) The \
             requested URL returned error: 511",
        ] {
            assert!(!is_network_unreachable(message), "premise: {message}");
            let mut c = Cadence::new(base);
            c.woke();
            let mut log = FailureLog::default();
            let mut waits = Vec::new();
            for since in [
                WAKE_SETTLE,
                WAKE_SETTLE + OFFLINE_RETRY[0],
                WAKE_SETTLE + OFFLINE_RETRY[0] + OFFLINE_RETRY[1],
            ] {
                let offline = after_wake(Some(woke), woke + since);
                let said = note_failure(&mut c, &mut log, message, offline);
                assert!(
                    !matches!(said, LogAction::Warn(_)),
                    "{since:?} after the wake: {said:?}"
                );
                assert_eq!(c.failures(), 0, "no doubling: {message}");
                waits.push(c.nominal());
            }
            assert_eq!(waits, [20, 60, 120].map(Duration::from_secs), "{message}");
            for woke_at in [Some(woke), None] {
                let mut c = Cadence::new(base);
                let mut log = FailureLog::default();
                let past = woke + WAKE_GRACE + Duration::from_secs(1);
                let said = note_failure(&mut c, &mut log, message, after_wake(woke_at, past));
                assert!(matches!(said, LogAction::Warn(_)), "{woke_at:?}: {said:?}");
                assert_eq!(c.failures(), 1, "{woke_at:?}");
            }
        }
    }

    /// A healthy check that found the head's app build in flight waits a minute —
    #[test]
    fn a_head_whose_app_build_is_in_flight_is_rechecked_within_a_minute() {
        let base = Duration::from_secs(INTERVAL_SECS);
        let mut c = Cadence::new(base);
        c.succeeded(0);
        assert_eq!(
            c.nominal(),
            base,
            "no head in flight: the ordinary interval"
        );
        for checks in [1, 2, IN_FLIGHT_CHECKS] {
            c.succeeded(checks);
            assert_eq!(c.nominal(), Duration::from_secs(60), "check {checks}");
            assert!(
                (0..=u8::MAX).all(|entropy| c.delay(entropy) < jitter(base, 0)),
                "check {checks}: every in-flight wait is shorter than every interval"
            );
        }
        for spent in [IN_FLIGHT_CHECKS + 1, u32::MAX] {
            c.succeeded(spent);
            assert_eq!(c.nominal(), base, "a head that stays source-only: {spent}");
        }
        for end in [Cadence::failed, Cadence::failed_offline, Cadence::woke] {
            c.succeeded(1);
            end(&mut c);
            assert_ne!(c.nominal(), IN_FLIGHT_RETRY, "cleared");
        }
        c.succeeded(1);
        let now = Instant::now();
        c.hold_until(now + Duration::from_secs(9 * 60));
        assert_eq!(
            c.nominal_at(now),
            Duration::from_secs(9 * 60),
            "a hold wins"
        );
    }

    /// The quick wait never waits LONGER than the ordinary one: under a base shorter
    #[test]
    fn the_in_flight_wait_never_outwaits_the_ordinary_one() {
        let short = Duration::from_secs(45);
        let mut c = Cadence::new(short);
        for checks in [1, IN_FLIGHT_CHECKS] {
            c.succeeded(checks);
            assert_eq!(c.nominal(), short);
        }
        assert_eq!(in_flight_retry(0), None, "0 is no head in flight");
        assert_eq!(in_flight_retry(1), Some(IN_FLIGHT_RETRY));
        assert_eq!(in_flight_retry(IN_FLIGHT_CHECKS), Some(IN_FLIGHT_RETRY));
        assert_eq!(in_flight_retry(IN_FLIGHT_CHECKS + 1), None, "spent");
    }

    #[test]
    fn only_dns_connect_and_timeout_failures_count_as_an_unreachable_network() {
        for unreachable in [
            "curl HEAD https://github.com/alabsystems/aterm/releases/latest/download/\
             aterm-appcast.toml failed (exit status: 6): curl: (6) Could not resolve host: \
             github.com",
            "curl GET https://github.com/x failed (exit status: 7): curl: (7) Failed to \
             connect to github.com port 443",
            "curl: (28) Operation timed out after 30001 milliseconds",
            "curl GET x failed (exit 6): dns",
            "fetch appcast: curl download failed (exit status: 28): ",
        ] {
            assert!(is_network_unreachable(unreachable), "{unreachable}");
        }
        for reached in [
            "curl: (22) The requested URL returned error: 404",
            "curl: (60) SSL certificate problem: unable to get local issuer certificate",
            "curl: (67) Login denied",
            "curl GET x failed (exit status: 35): curl: (35) TLS handshake",
            "ditto zip extract failed (exit status: 7)",
            "HTTP 404 for https://github.com/x: the channel has no published release",
            "no usable release this check — run `aterm ctl update status` for the reason",
        ] {
            assert!(!is_network_unreachable(reached), "{reached}");
        }
    }

    #[test]
    fn recovery_and_wake_both_snap_back_to_base() {
        let mut c = Cadence::new(BASE);
        for _ in 0..10 {
            c.failed();
        }
        assert!(c.nominal() > BASE);
        c.succeeded(0);
        assert_eq!(c.nominal(), BASE, "one success restores the fast cadence");
        for _ in 0..10 {
            c.failed();
        }
        c.woke();
        assert_eq!(
            c.nominal(),
            BASE,
            "a wake invalidates the outage the backoff was about"
        );
    }

    /// A failing host gets a real retreat from [`INTERVAL_SECS`], not a clamp to its base.
    #[test]
    fn the_interval_genuinely_backs_off_instead_of_clamping_to_its_own_base() {
        let anon = Duration::from_secs(INTERVAL_SECS);
        let mut c = Cadence::new(anon);
        c.failed();
        assert_eq!(
            c.nominal(),
            anon,
            "the first retry is still the base interval"
        );
        c.failed();
        assert!(
            c.nominal() > anon,
            "a second consecutive failure must lengthen the wait — the assertion the \
             two equal constants used to make unfalsifiable"
        );
        for _ in 0..20 {
            c.failed();
        }
        assert_eq!(
            c.nominal(),
            MAX_BACKOFF.max(anon * MAX_BACKOFF_INTERVALS),
            "and it climbs to the ceiling"
        );
        c.succeeded(0);
        assert_eq!(c.nominal(), anon, "recovery snaps back to the base");
    }

    /// A hold is waited out exactly, not doubled or jittered.
    #[test]
    fn a_hold_waits_until_the_reset_not_a_doubling() {
        let anon = Duration::from_secs(INTERVAL_SECS);
        let mut c = Cadence::new(anon);
        for _ in 0..3 {
            c.failed();
        }
        assert_eq!(c.nominal(), anon * 4, "precondition: the ladder is at 4×");
        let now = Instant::now();
        let reset = now + Duration::from_secs(5 * 60);
        c.hold_until(reset);
        assert_eq!(
            c.nominal_at(now),
            Duration::from_secs(5 * 60),
            "the wait is the time to the reset, not the 4× rung"
        );
        assert_eq!(
            c.delay_at(now, 0),
            c.delay_at(now, 255),
            "a held wait is not spread: −20 % would wake before the window renews"
        );
        assert_eq!(c.delay_at(now, 0), Duration::from_secs(5 * 60));
        c.hold_until(now + Duration::from_secs(1));
        assert_eq!(c.nominal_at(now), HOLD_FLOOR);
        c.hold_until(now - Duration::from_secs(1));
        assert_eq!(c.nominal_at(now), anon * 4);
    }

    #[test]
    fn a_hold_is_bounded_by_the_cap_and_cleared_by_success_or_wake() {
        let anon = Duration::from_secs(INTERVAL_SECS);
        let now = Instant::now();
        let mut c = Cadence::new(anon);
        c.hold_until(now + Duration::from_secs(10 * 3600));
        assert_eq!(
            c.nominal_at(now),
            MAX_BACKOFF.max(anon * MAX_BACKOFF_INTERVALS),
            "capped"
        );
        c.succeeded(0);
        assert_eq!(c.nominal_at(now), anon, "a success clears the hold");
        c.hold_until(now + Duration::from_secs(600));
        c.woke();
        assert_eq!(c.nominal_at(now), anon, "a wake clears the hold");
        c.hold_until(now + Duration::from_secs(600));
        c.failed();
        assert_eq!(
            c.nominal_at(now),
            anon,
            "a failure of unknown length supersedes it"
        );
        let mut expired = Cadence::new(anon);
        expired.hold_until(now + Duration::from_secs(600));
        assert_eq!(
            expired.nominal_at(now + Duration::from_secs(601)),
            anon,
            "past the epoch the base interval is back"
        );
    }

    /// The first rung (one base ±20 %) is shorter than the 1.4-base window, so a deferral
    #[test]
    fn a_deferral_never_wakes_inside_its_own_window() {
        let base = Duration::from_secs(INTERVAL_SECS);
        let now = Instant::now();
        let window_end = now + base * 14 / 10;
        for entropy in [0u8, 1, 128, 254, 255] {
            let mut c = Cadence::new(base);
            c.deferred(Some(window_end), now, entropy);
            assert_eq!(c.failures(), 1);
            let wait = c.delay_at(now, entropy);
            assert!(wait >= base * 14 / 10, "entropy {entropy}: {wait:?}");
            let mut ladder = Cadence::new(base);
            ladder.failed();
            assert!(ladder.delay_at(now, entropy) < base * 14 / 10);
        }
        let mut c = Cadence::new(base);
        c.failed();
        c.deferred(Some(window_end), now, 255);
        assert_eq!(c.failures(), 2);
        assert_eq!(c.delay_at(now, 0), jitter(base * 2, 0));
        assert_eq!(c.delay_at(now, 255), jitter(base * 2, 255));
        let mut c = Cadence::new(base);
        c.deferred(None, now, 0);
        assert_eq!(c.delay_at(now, 0), jitter(base, 0));
        let mut c = Cadence::new(base);
        c.deferred(Some(window_end), now, 0);
        c.succeeded(0);
        assert_eq!(c.nominal_at(now), base);
    }

    /// `next_check=` is now plus the wait about to start, published only while it runs.
    #[test]
    fn the_loop_publishes_its_next_check() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_280_801);
        note_next_check(at, Duration::from_secs(600));
        assert_eq!(next_check_unix(), Some(1_790_281_401));

        let base = Duration::from_secs(INTERVAL_SECS);
        for waited in [Waited::Elapsed, Waited::Woke(Duration::from_secs(3600))] {
            let mut during = None;
            let before = aterm_time::unix_secs().unwrap();
            let (delay, outcome) = wait_with(&Cadence::new(base), 0, |delay| {
                during = next_check_unix();
                assert_eq!(delay, jitter(base, 0));
                waited
            });
            assert_eq!(outcome, waited);
            let during = during.expect("published while the wait runs");
            assert!(
                (before + delay.as_secs()..=before + delay.as_secs() + 2).contains(&during),
                "{during} vs {before} + {delay:?}"
            );
            assert_eq!(next_check_unix(), None, "cleared once the wait is over");
        }
    }

    #[test]
    fn jitter_stays_within_the_declared_band_for_every_byte() {
        let lo = BASE.as_millis() as u64 * (100 - JITTER_PCT) / 100;
        let hi = BASE.as_millis() as u64 * (100 + JITTER_PCT) / 100;
        let mut seen_low = false;
        let mut seen_high = false;
        for b in 0..=u8::MAX {
            let d = jitter(BASE, b).as_millis() as u64;
            assert!(
                d >= lo && d <= hi,
                "byte {b} produced {d}ms, outside {lo}..={hi}"
            );
            seen_low |= d < BASE.as_millis() as u64;
            seen_high |= d > BASE.as_millis() as u64;
        }
        assert!(seen_low && seen_high, "jitter must spread both ways");
    }

    #[test]
    fn jitter_cannot_panic_on_extremes() {
        assert_eq!(jitter(Duration::ZERO, 255), Duration::ZERO);
        let _ = jitter(Duration::MAX, 255);
        let _ = jitter(Duration::MAX, 0);
    }

    #[test]
    fn wake_gap_ignores_scheduling_slop_and_catches_a_lid_close() {
        let slice = Duration::from_secs(15);
        assert!(!is_wake_gap(slice, slice), "an exact slice is not a wake");
        assert!(
            !is_wake_gap(slice, slice + Duration::from_secs(5)),
            "ordinary scheduling delay is not a wake"
        );
        assert!(
            !is_wake_gap(slice, slice + SLEEP_GAP - Duration::from_millis(1)),
            "just under the threshold is not a wake"
        );
        assert!(
            is_wake_gap(slice, slice + Duration::from_secs(3 * 3600)),
            "a three-hour lid close IS a wake"
        );
        assert!(
            !is_wake_gap(slice, Duration::ZERO),
            "a backwards clock step must not read as a wake"
        );
    }

    #[test]
    fn identical_failures_collapse_after_the_first() {
        let mut log = FailureLog::default();
        let msg = "curl GET https://api.github.com/... failed (exit 6): could not resolve host";
        assert!(
            matches!(log.failure(msg), LogAction::Warn(_)),
            "first is loud"
        );
        for _ in 0..47 {
            assert_eq!(
                log.failure(msg),
                LogAction::Suppress,
                "48 identical lines collapse to one"
            );
        }
    }

    #[test]
    fn a_changed_failure_is_always_reported() {
        let mut log = FailureLog::default();
        assert!(matches!(
            log.failure("could not resolve host"),
            LogAction::Warn(_)
        ));
        assert_eq!(log.failure("could not resolve host"), LogAction::Suppress);
        let LogAction::Warn(text) = log.failure("GitHub auth failed (HTTP 401)") else {
            panic!("a changed message must be reported");
        };
        assert!(text.contains("3 in a row"), "the streak is carried: {text}");
    }

    /// The same failure, once it outlasts the quick rungs, is warned, not suppressed.
    #[test]
    fn an_expected_failure_is_info_until_it_outlasts_the_short_ladder() {
        let mut log = FailureLog::default();
        let msg = "curl: (6) Could not resolve host: github.com";
        let LogAction::Log(_) = log.failure_expected(msg, true) else {
            panic!("a network that is not up yet is news, not a warning");
        };
        assert_eq!(log.failure_expected(msg, true), LogAction::Suppress);
        let LogAction::Warn(escalated) = log.failure_expected(msg, false) else {
            panic!("past the quick rungs the same failure must be warned");
        };
        assert!(escalated.contains("3 in a row"), "{escalated}");
        assert_eq!(log.failure_expected(msg, false), LogAction::Suppress);
        assert!(
            matches!(log.success(), Some(LogAction::Log(_))),
            "the recovery is logged"
        );
    }

    #[test]
    fn recovery_is_reported_once_with_the_streak() {
        let mut log = FailureLog::default();
        assert!(log.success().is_none(), "a healthy check says nothing");
        for _ in 0..5 {
            log.failure("offline");
        }
        let Some(LogAction::Log(text)) = log.success() else {
            panic!("recovery after an outage must be logged");
        };
        assert!(text.contains("after 5 failures"), "{text}");
        assert!(log.success().is_none(), "and only once");
    }

    #[test]
    fn wait_returns_for_a_short_delay() {
        let signal = WakeSignal::new();
        assert_eq!(
            sleep_watching_for_wake(&signal, 0, Duration::from_millis(50)),
            Waited::Elapsed
        );
        assert_eq!(
            sleep_watching_for_wake(&signal, 0, Duration::ZERO),
            Waited::Elapsed
        );
    }

    /// A day-long wait on a signal another thread posts past the count the waiter
    #[test]
    fn a_posted_wake_ends_the_wait_at_once() {
        static SIGNAL: WakeSignal = WakeSignal::new();
        SIGNAL.post();
        assert_eq!(
            sleep_watching_for_wake(&SIGNAL, SIGNAL.posts(), Duration::from_millis(50)),
            Waited::Elapsed,
            "an answered post is not this wait's wake"
        );
        let seen = SIGNAL.posts();
        let (done, answer) = std::sync::mpsc::channel();
        let sleeper = std::thread::spawn(move || {
            let _ = done.send(sleep_watching_for_wake(
                &SIGNAL,
                seen,
                Duration::from_secs(24 * 3600),
            ));
        });
        SIGNAL.post();
        let woke = answer.recv_timeout(aterm_spec::wait::MUST_HAPPEN).ok();
        assert!(
            matches!(woke, Some(Waited::Woke(_))),
            "a posted wake must end the wait: {woke:?}"
        );
        let _ = sleeper.join();
    }
}
