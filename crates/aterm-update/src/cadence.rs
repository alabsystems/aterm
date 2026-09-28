// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! When the background update check runs, and what it says when it fails.
//!
//! The loop used to be `loop { check(); sleep(75s); }`. Three things were wrong with
//! that, all of them visible in a real machine's `aterm.log`:
//!
//! * **No sleep/wake awareness.** A laptop that closes its lid freezes the sleeping
//!   thread; on wake `nanosleep`'s deadline is long past, so the check fires
//!   immediately — *before* Wi-Fi has associated. The first post-wake check reliably
//!   fails on DNS. [`sleep_watching_for_wake`] notices the wall-clock jump and gives
//!   the network [`WAKE_SETTLE`] to come up instead of burning a guaranteed failure.
//! * **No backoff.** Offline for an hour meant 48 identical failures, 48 health-ledger
//!   increments, and 48 identical log lines. [`Cadence::delay`] doubles the interval
//!   per consecutive failure up to a ceiling of [`MAX_BACKOFF_INTERVALS`] base
//!   intervals (never below [`MAX_BACKOFF`]), and snaps back to the base interval the
//!   moment a check succeeds. A check that could not reach its release channel at all
//!   (DNS, connect, timeout — [`is_network_unreachable`], on a failure the health
//!   ledger filed as `network`) is retried sooner first, on [`OFFLINE_RETRY`], because
//!   that is what a Mac that has just booted or joined a network looks like for its
//!   first few seconds.
//! * **No jitter.** Every aterm on every machine woke on the same grid relative to
//!   its own launch; a fleet restarted together stays in lockstep and hits the host
//!   in a thundering herd. [`Cadence::delay`] spreads each wait by ±[`JITTER_PCT`]%.
//!
//! A healthy check can ask for a sooner one too: a channel head published
//! source-first, above the last release the machine authorized, has its app build
//! minutes behind it, so the next checks come on [`IN_FLIGHT_RETRY`] instead of a
//! whole interval.
//!
//! And the log itself: dozens of byte-identical `update check failed: …` lines say
//! nothing the first one didn't. [`FailureLog`] emits the first occurrence, then
//! stays quiet until the message CHANGES or [`STILL_FAILING_AFTER`] passes, and always
//! reports the recovery.
//!
//! Everything here is pure or `std`-only. A real `NSWorkspace`
//! `didWakeNotification` observer would need an Objective-C runtime dependency this
//! crate does not have (and a run loop the detached update thread does not run); the
//! wall-clock gap check is the same signal without the dependency, and it also
//! catches the cases a wake notification misses — a suspended VM, a laptop resumed
//! from hibernation, a large NTP step.

use std::time::{Duration, Instant, SystemTime};

pub(crate) use crate::cadence_bounds::{
    INTERVAL_SECS, JITTER_PCT, MAX_BACKOFF, MAX_BACKOFF_INTERVALS, WAKE_SETTLE,
};

/// A wall-clock jump larger than the requested sleep by this much means the machine
/// was not running: system sleep, hibernation, a suspended VM, or a large clock step.
/// Well above any scheduling delay or routine NTP slew.
pub(crate) const SLEEP_GAP: Duration = Duration::from_secs(90);

/// How long an unchanged failure message stays suppressed before being repeated.
pub(crate) const STILL_FAILING_AFTER: Duration = Duration::from_secs(30 * 60);

/// The waits after a check that could not reach the network at all
/// ([`is_network_unreachable`]): the first retry comes after 20 s, then 60 s, 2 min
/// and 5 min, and after those the ordinary ladder takes over from the base interval.
/// A Mac that has just booted, woken or joined a network cannot resolve a name for a
/// few seconds — the first check after a cold boot on 2026-09-23 failed on DNS three
/// seconds in — and a whole interval after that is a whole interval in which a new
/// release goes unnoticed. Four quick tries cost four tiny requests on the unmetered
/// download host; an outage longer than they cover falls back to the backoff it always
/// had. Never longer than the ordinary ladder's own wait at that point
/// ([`Cadence::nominal_at`]).
pub(crate) const OFFLINE_RETRY: [Duration; 4] = [
    Duration::from_secs(20),
    Duration::from_secs(60),
    Duration::from_secs(2 * 60),
    Duration::from_secs(5 * 60),
];

/// Whether a failed check's message says the network could not be reached at all:
/// curl exit 6 (could not resolve the host), 7 (could not connect) or 28 (timed out),
/// in either spelling the transport writes — `curl: (6) …` from curl's own stderr, or
/// `(exit status: 6)` from the exit status the transport quotes. Only a message that
/// names curl counts: another tool's exit 7 (a `ditto` that failed to extract) is not
/// a network that is down. Anything else — an HTTP status, a signature, a refused
/// redirect — is a failure the network carried, and waits on the ordinary ladder. The
/// check loop also asks the health ledger: the same exit on the container download,
/// after the channel answered, is not a network that is down either
/// (`unreachable_before_the_channel` in the crate root).
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

/// The waits after a healthy check that found the channel head's APP BUILD IN FLIGHT
/// — a release published source-first, whose `aterm-appcast.toml` the download host
/// does not carry yet, and whose tag outranks the last one the machine authorized
/// (`github::app_build_in_flight`) — indexed by how many consecutive completed checks,
/// machine-wide, have found it so (`head_in_flight_checks` in the check receipt): 2,
/// 4, then 8 minutes, and after those the ordinary interval.
///
/// Measured 2026-09-24 on the owner's machine: v0.92.0 was published source-first,
/// the 12:38:35 check met its appcast's 404 ("channel head v0.92.0 has no app
/// manifest"), and the next check came a whole interval later, at 13:13:10 — which
/// staged it and applied it in 1035 ms. The app assets had landed minutes after the
/// first check; the interval was the whole delay. Three quick tries cost three HEADs
/// and three appcast GETs on the unmetered download host; a head that stays
/// source-only for a night (v0.80.0 did) falls back to the ordinary cadence after
/// them. Never longer than the ordinary ladder's own wait ([`Cadence::nominal_at`]).
pub(crate) const IN_FLIGHT_RETRY: [Duration; 3] = [
    Duration::from_secs(2 * 60),
    Duration::from_secs(4 * 60),
    Duration::from_secs(8 * 60),
];

/// The quick wait for the `checks`-th consecutive check that found the head's app build
/// in flight ([`IN_FLIGHT_RETRY`]), or `None` once those are spent (or for `0`: no head
/// in flight). Shared by the one scheduler ([`Cadence`]) and the machine-wide dedup
/// window that must not outlast it (`checker_skip_for` in the crate root).
pub(crate) fn in_flight_retry(checks: u32) -> Option<Duration> {
    let rung = usize::try_from(checks.checked_sub(1)?).ok()?;
    IN_FLIGHT_RETRY.get(rung).copied()
}

/// The floor on a HELD wait that is still in force. A hold a few seconds out (a
/// sibling's window was nearly over when this process skipped) must not become a
/// near-zero wait: the loop would re-check at once, read the same fresh ledger stamp,
/// skip, and spin through `wait` with nothing to wait for. A hold whose epoch has
/// already PASSED is not floored — it is simply over, and the ordinary ladder (base ×
/// the failure count, which a hold never raised) applies.
pub(crate) const HOLD_FLOOR: Duration = Duration::from_secs(60);

/// The interval schedule: a base cadence plus the current consecutive-failure count,
/// and — after a skip behind a sibling's fresh check — the instant that sibling's
/// window ends.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cadence {
    base: Duration,
    failures: u32,
    /// Consecutive checks, ending with the latest, that could not reach the network
    /// ([`Self::failed_offline`]). While it is within [`OFFLINE_RETRY`] the next wait
    /// is that rung and `failures` has not moved; past it, each one counts as an
    /// ordinary failure.
    offline: u32,
    /// How many consecutive completed checks, machine-wide, have found the channel
    /// head's app build in flight, as of this process's latest healthy check
    /// ([`Self::succeeded`]). While it names an [`IN_FLIGHT_RETRY`] rung the next wait
    /// is that rung; `0` is none. Any other outcome clears it.
    in_flight: u32,
    /// When set and still in the future, the next wait ends HERE (bounded by
    /// [`Self::cap`], floored by [`HOLD_FLOOR`], un-jittered — the epoch already
    /// carries the loop's 0–60 s scatter) instead of on the doubling ladder.
    hold: Option<Instant>,
}

impl Cadence {
    /// A schedule at the configured base interval, starting healthy.
    pub(crate) fn new(base: Duration) -> Self {
        Self {
            base,
            failures: 0,
            offline: 0,
            in_flight: 0,
            hold: None,
        }
    }

    /// Hold the next check until `until` — the end of a sibling's fresh window. Does
    /// NOT count as a failure: the doubling ladder is for outages of unknown length.
    pub(crate) fn hold_until(&mut self, until: Instant) {
        self.hold = Some(until);
    }

    /// The current base interval — the cross-process checker gate sizes its
    /// freshness window from it (see the check loop in `lib.rs`).
    #[cfg(target_os = "macos")]
    pub(crate) fn base(&self) -> Duration {
        self.base
    }

    /// Note a failed check (lengthens the next wait). A failure of unknown length
    /// supersedes a hold of known length.
    pub(crate) fn failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
        self.offline = 0;
        self.in_flight = 0;
        self.hold = None;
    }

    /// Note a DEFERRAL — the host asked this machine to slow down — as a failure (the
    /// doubling ladder lengthens) whose next wait also ends no earlier than
    /// `window_end`: the end of the machine-wide deferred window this very check
    /// stamped on the receipt (`checker_skip_for` in the crate root; the caller
    /// scatters it). `None` (no receipt could be read) is a plain [`Self::failed`].
    ///
    /// The ladder's first rung is ONE base interval (±[`JITTER_PCT`]%) and the
    /// deferred window is 1.4 of them (`DEFERRED_WINDOW_INTERVALS`), so on
    /// `failed` alone the deferring process always woke inside its own window, read
    /// its own note as a sibling's and slept again to the window's end — measured
    /// 2026-09-24: the 20:27:06 wake skipped on the deferral its own 19:54:42 check
    /// wrote, logging "the shared update ledger records a deferred check". Held to the
    /// window's end instead (un-jittered past it: the epoch carries the scatter), or to
    /// the jittered ladder when that is later. A rung that can never wake inside the
    /// window keeps its ordinary jittered wait.
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

    /// Note a check that could not reach the network at all
    /// ([`is_network_unreachable`]). The next wait is the next [`OFFLINE_RETRY`] rung;
    /// once those are spent, this counts as an ordinary [`Self::failed`] and the
    /// doubling ladder goes on from the base interval.
    pub(crate) fn failed_offline(&mut self) {
        self.offline = self.offline.saturating_add(1);
        if self.offline as usize > OFFLINE_RETRY.len() {
            self.failures = self.failures.saturating_add(1);
        }
        self.in_flight = 0;
        self.hold = None;
    }

    /// Whether the next wait is one of the quick [`OFFLINE_RETRY`] rungs — a network
    /// that is not up YET, which the check loop logs as news rather than a warning.
    pub(crate) fn retrying_offline(&self) -> bool {
        self.offline > 0 && self.offline as usize <= OFFLINE_RETRY.len()
    }

    /// Note a successful check — the next wait returns to the base interval
    /// immediately, or, when `head_in_flight_checks` names an [`IN_FLIGHT_RETRY`]
    /// rung, to that rung: the check found the channel head's app build in flight
    /// that many times running (`github::head_in_flight`), and its assets are
    /// minutes away. Recovery must not be rate-limited by how long the outage was.
    pub(crate) fn succeeded(&mut self, head_in_flight_checks: u32) {
        self.failures = 0;
        self.offline = 0;
        self.in_flight = head_in_flight_checks;
        self.hold = None;
    }

    /// A wake resets the backoff: the network the failures were about is gone, and
    /// the machine is in a genuinely new state.
    pub(crate) fn woke(&mut self) {
        self.failures = 0;
        self.offline = 0;
        self.in_flight = 0;
        self.hold = None;
    }

    /// The consecutive-failure count — what the doubling ladder is keyed on. Exposed
    /// for tests, so "the quick rungs are not the ladder's" is assertable on the count.
    #[cfg(test)]
    pub(crate) fn failures(&self) -> u32 {
        self.failures
    }

    /// The remaining hold, if one is set and still in the future at `now`; an expired
    /// hold is simply over (the ladder below applies).
    fn hold_remaining(&self, now: Instant) -> Option<Duration> {
        let until = self.hold?;
        let remaining = until.checked_duration_since(now)?;
        Some(remaining.max(HOLD_FLOOR).min(self.cap()))
    }

    /// The ceiling on [`Self::nominal`] for THIS base: at least [`MAX_BACKOFF`], at
    /// least the base itself (a configured interval is a floor on the wait, never
    /// something a cap may shorten), and at most [`MAX_BACKOFF_INTERVALS`] × base —
    /// the term that keeps the ceiling strictly above the base, so a slow base still
    /// backs off instead of clamping to where it started.
    fn cap(&self) -> Duration {
        MAX_BACKOFF.max(self.base.saturating_mul(MAX_BACKOFF_INTERVALS))
    }

    /// The longest one [`wait`] of this schedule can take, plus the post-wake
    /// [`WAKE_SETTLE`] the loop may add after it: the ceiling, spread by the
    /// jitter's upper edge (a hold is capped at the ceiling and never spread).
    ///
    /// The check loop publishes it as its heartbeat's budget while it waits
    /// (`checker_watch`, plan P2-1): the longest silence between two stamps that is
    /// still a healthy loop. Derived here, from the same ceiling and jitter
    /// [`Self::delay_at`] uses, so a later change to either cannot leave the
    /// watchdog judging a wait it no longer knows the length of.
    pub(crate) fn max_wait(&self) -> Duration {
        let cap = u64::try_from(self.cap().as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(cap.saturating_mul(100 + JITTER_PCT) / 100) + WAKE_SETTLE
    }

    /// The nominal (pre-jitter) wait: `base` doubled once per consecutive failure,
    /// clamped to [`Self::cap`] — or, while a run of unreachable-network failures is
    /// within [`OFFLINE_RETRY`], that rung; or, after a healthy check that found the
    /// head's app build in flight, its [`IN_FLIGHT_RETRY`] rung. Exposed for tests;
    /// [`Self::delay`] is what the loop uses.
    #[cfg(test)]
    pub(crate) fn nominal(&self) -> Duration {
        self.nominal_at(Instant::now())
    }

    /// [`Self::nominal`] at an injected `now`, so a hold's arithmetic is testable
    /// without waiting.
    pub(crate) fn nominal_at(&self, now: Instant) -> Duration {
        if let Some(held) = self.hold_remaining(now) {
            return held;
        }
        // `1 << 20` already exceeds any sane base × ceiling ratio; the shift is
        // clamped so a long outage can never overflow the multiply.
        let doublings = self.failures.saturating_sub(1).min(20);
        let ladder = self.base.saturating_mul(1u32 << doublings).min(self.cap());
        match self.offline.checked_sub(1) {
            Some(rung) if self.retrying_offline() => OFFLINE_RETRY[rung as usize].min(ladder),
            _ => in_flight_retry(self.in_flight).map_or(ladder, |rung| rung.min(ladder)),
        }
    }

    /// The actual wait: [`Self::nominal`] spread by ±[`JITTER_PCT`]%. `entropy` is a
    /// uniformly random byte; the caller supplies it so this stays pure and testable.
    /// A HELD wait is not spread: the epoch is already scattered by the loop's own
    /// 0–60 s, and −20 % of it would wake this machine inside the sibling's window —
    /// the one thing a hold exists to avoid.
    pub(crate) fn delay(&self, entropy: u8) -> Duration {
        self.delay_at(Instant::now(), entropy)
    }

    /// [`Self::delay`] at an injected `now`.
    pub(crate) fn delay_at(&self, now: Instant, entropy: u8) -> Duration {
        if let Some(held) = self.hold_remaining(now) {
            return held;
        }
        jitter(self.nominal_at(now), entropy)
    }
}

/// Spread `d` by ±[`JITTER_PCT`]% using one random byte. Saturating throughout, so
/// no input can panic.
fn jitter(d: Duration, entropy: u8) -> Duration {
    let span = 2 * JITTER_PCT; // the full width, in percent
    let offset = (u64::from(entropy) * (span + 1)) / 256; // 0..=span
    let scale = 100 - JITTER_PCT + offset; // (100-P)..=(100+P)
    let millis = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(millis.saturating_mul(scale) / 100)
}

/// One random byte from the audited entropy surface, or a fixed midpoint if it is
/// unavailable. A missing byte must degrade to "no jitter", never to a panic or a
/// hand-rolled `/dev/urandom` read. Shared with the skip timer's scatter.
#[cfg(target_os = "macos")]
pub(crate) fn entropy_byte() -> u8 {
    let mut b = [128u8; 1];
    let _ = aterm_uds::rand::fill(&mut b);
    b[0]
}

/// How a wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waited {
    /// The full delay elapsed normally.
    Elapsed,
    /// The machine was not running for part of the wait (system sleep / hibernation /
    /// VM suspend / clock step). Carries the observed gap, for the log.
    Woke(Duration),
}

/// Whether a slice that requested `slice` but consumed `observed` wall-clock seconds
/// means the machine stopped running. Pure, so the threshold is testable without
/// sleeping a Mac.
///
/// Deliberately compares wall time against the REQUESTED duration rather than against
/// an [`Instant`]: whether `Instant` advances across system sleep is a
/// platform-and-libc detail that has changed under us before, while "I asked to sleep
/// 15 seconds and 3 hours of wall clock went by" is true on every platform and also
/// catches hibernation and VM suspend.
pub(crate) fn is_wake_gap(slice: Duration, observed: Duration) -> bool {
    observed.saturating_sub(slice) >= SLEEP_GAP
}

/// Sleep for `total`, in slices, returning early with [`Waited::Woke`] when the
/// machine turns out to have been asleep. On wake the caller gets control back
/// promptly instead of finishing a wait whose premise (an interval of *running*
/// time) no longer holds.
pub(crate) fn sleep_watching_for_wake(total: Duration) -> Waited {
    /// Slice length. Short enough to notice a wake promptly, long enough that a
    /// full-interval wait costs a handful of wakeups.
    const SLICE: Duration = Duration::from_secs(15);

    let mut remaining = total;
    while !remaining.is_zero() {
        let slice = remaining.min(SLICE);
        let before = SystemTime::now();
        std::thread::sleep(slice);
        // A backwards clock step yields Err; treat it as an ordinary slice rather
        // than inventing a gap (a backwards step is not a wake).
        let observed = before.elapsed().unwrap_or(slice);
        if is_wake_gap(slice, observed) {
            return Waited::Woke(observed.saturating_sub(slice));
        }
        remaining = remaining.saturating_sub(slice);
    }
    Waited::Elapsed
}

/// Wait one cadence interval, jittered, watching for a wake.
#[cfg(target_os = "macos")]
pub(crate) fn wait(cadence: &Cadence) -> (Duration, Waited) {
    wait_with(cadence, entropy_byte(), sleep_watching_for_wake)
}

/// [`wait`] over an injected sleep. The next check is published for exactly as long as
/// the wait lasts: once it is over the loop is settling after a wake or checking, the
/// instant it named is past, and `next_check=` says nothing until the next wait names
/// another (review of 2026-09-26: it used to keep the past instant through the whole
/// check and the wake settle).
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

/// When this process's check loop looks next, as Unix seconds (`0`: no wait is under
/// way — the loop has not started one, or is past it and checking) — set by [`wait`]
/// as each wait starts and cleared as it ends, read by `crate::next_check_at` for
/// `aterm ctl update status`'s `next_check=`. The loop's own schedule, not the
/// machine's: a sibling process may check sooner, and this one then skips behind its
/// receipt.
static NEXT_CHECK_UNIX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Record that the loop's next check is `delay` after `now`.
fn note_next_check(now: SystemTime, delay: Duration) {
    let at = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.saturating_add(delay).as_secs());
    NEXT_CHECK_UNIX.store(at, std::sync::atomic::Ordering::Relaxed);
}

/// The loop's next check ([`NEXT_CHECK_UNIX`]), `None` while no wait is under way.
pub(crate) fn next_check_unix() -> Option<u64> {
    Some(NEXT_CHECK_UNIX.load(std::sync::atomic::Ordering::Relaxed)).filter(|at| *at > 0)
}

/// What the loop should do about a check result's log line. Returned instead of
/// logging directly so the whole policy is testable without an installed logger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LogAction {
    /// Say nothing — an identical failure was reported recently.
    Suppress,
    /// Emit this as a warning.
    Warn(String),
    /// Emit this as an ordinary log line.
    Log(String),
}

/// Collapses repeated identical failures.
///
/// The owner's log contains dozens of byte-identical `update check failed:` lines.
/// Every one after the first is noise that buries the lines that matter — including
/// the handoff diagnostics this whole effort is about. Policy:
///
/// * a message DIFFERENT from the last one is always warned (a changed failure is
///   news: DNS → auth, say) — or said at INFO while the network is expected to be
///   down, on the quick [`OFFLINE_RETRY`] rungs ([`Self::failure_expected`]);
/// * an identical repeat is suppressed until [`STILL_FAILING_AFTER`], then warned
///   once with the consecutive count, so the log always shows an ongoing outage
///   without showing it 48 times an hour;
/// * recovery is always logged, with how long/how many it took — the transition
///   nobody records and everybody wants.
#[derive(Debug, Default)]
pub(crate) struct FailureLog {
    last: Option<String>,
    /// Whether the last emitted line was a warning — a failure first said at INFO is
    /// said again, as a warning, the moment it stops being expected.
    last_warned: bool,
    /// Failures observed since the last emitted line (the first one included).
    since_emit: u32,
    /// Total consecutive failures in the current outage.
    streak: u32,
    emitted_at: Option<Instant>,
}

impl FailureLog {
    /// Record a failed check and decide what to say about it.
    pub(crate) fn failure(&mut self, message: &str) -> LogAction {
        self.failure_expected(message, false)
    }

    /// [`Self::failure`], said at INFO instead of as a warning when `expected`: a
    /// network that is not up yet while the quick [`OFFLINE_RETRY`] rungs still run
    /// (the boot and wake case) is news, not a fault. The same failure, still there
    /// once `expected` is false, is warned at once rather than suppressed as a repeat.
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
            format!(
                "update check still failing ({} consecutive): {message}",
                self.streak
            )
        };
        if expected {
            LogAction::Log(line)
        } else {
            LogAction::Warn(line)
        }
    }

    /// Record a successful check. Emits the recovery line iff there was an outage.
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
                format!("update check recovered after {streak} consecutive failures")
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cadence_bounds::{LONGEST_WAIT, STALE_CHECK_AFTER};

    /// [`LONGEST_WAIT`] bounds every wait the real schedule produces — through a
    /// saturated failure ladder, every offline rung, every in-flight rung, a hold at its
    /// ceiling, and every jitter byte — and the ladder really reaches it (minus the wake
    /// settle), so the bound is tight rather than merely safe.
    #[test]
    fn no_wait_exceeds_the_longest_wait() {
        let now = Instant::now();
        let mut longest = Duration::ZERO;
        let mut failing = Cadence::new(Duration::from_secs(INTERVAL_SECS));
        let mut offline = Cadence::new(Duration::from_secs(INTERVAL_SECS));
        let mut held = Cadence::new(Duration::from_secs(INTERVAL_SECS));
        held.hold_until(now + Duration::from_secs(24 * 3600));
        let mut in_flight = Cadence::new(Duration::from_secs(INTERVAL_SECS));
        for round in 0..30 {
            failing.failed();
            offline.failed_offline();
            in_flight.succeeded(round);
            for entropy in 0..=u8::MAX {
                for cadence in [&failing, &offline, &held, &in_flight] {
                    let wait = cadence.delay_at(now, entropy);
                    assert!(wait + WAKE_SETTLE <= LONGEST_WAIT, "{wait:?}");
                    longest = longest.max(wait);
                }
            }
        }
        assert_eq!(longest + WAKE_SETTLE, LONGEST_WAIT, "the bound is reached");
        assert!(STALE_CHECK_AFTER > LONGEST_WAIT * 4);
    }

    const BASE: Duration = Duration::from_secs(75);

    #[test]
    fn healthy_cadence_is_the_base_interval() {
        assert_eq!(Cadence::new(BASE).nominal(), BASE);
    }

    /// THE WATCHDOG'S BUDGET COVERS EVERY WAIT THE SCHEDULE CAN TAKE (plan P2-1).
    /// `max_wait` is what the heartbeat is judged against while the loop waits, so
    /// a wait longer than it would read as a stall of a healthy loop: swept over
    /// the shipped interval and a fast test base, every failure depth, the offline
    /// and in-flight ladders, a hold past the ceiling, a deferral held to its own
    /// window (`Cadence::deferred`) and every jitter byte, no delay (plus the wake
    /// settle) exceeds it.
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
            for checks in 0..8 {
                let mut in_flight = Cadence::new(base);
                in_flight.succeeded(checks);
                schedules.push(in_flight);
            }
            let mut held = Cadence::new(base);
            held.hold_until(now + Duration::from_secs(24 * 3600));
            schedules.push(held);
            // A deferral held to its own window (`Cadence::deferred`, the rate-limited
            // arm): the window the check stamped, and one far past the ceiling.
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
        // …and it is not vacuous: at the shipped interval it is exactly the
        // schedule's own LONGEST_WAIT (the 40-minute ceiling, +20 %, + the settle),
        // the bound `no_wait_exceeds_the_longest_wait` shows is reached.
        assert_eq!(
            Cadence::new(Duration::from_secs(INTERVAL_SECS)).max_wait(),
            LONGEST_WAIT
        );
        assert_eq!(LONGEST_WAIT, Duration::from_secs(48 * 60) + WAKE_SETTLE);
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
    /// 60 s, 2 min and 5 min, and only then does the ordinary ladder take over, from
    /// the base interval and doubling as before.
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
            [20, 60, 120, 300].map(Duration::from_secs),
            "20 s, 60 s, 2 min, 5 min"
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

    /// The quick rungs never wait LONGER than the ordinary ladder would: on a base
    /// shorter than the 2- and 5-minute rungs, those rungs are the base interval.
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

    /// A failure the network carried (an HTTP status, a signature) is not a network
    /// that is down: it ends the quick rungs and waits on the ordinary ladder.
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

    /// THE SOURCE-FIRST RELEASE (measured 2026-09-24: v0.92.0's first check met a
    /// 404 appcast and the next came a whole interval later). A healthy check that
    /// found the head's app build in flight waits 2, 4, then 8 minutes — keyed on the
    /// machine-wide count the check receipt carries — and the ordinary interval after
    /// those; a healthy check that found none (the negative control, `0`) waits the
    /// base interval exactly as before. Every other outcome — a failure, an
    /// unreachable network, a wake — ends the quick ladder, and a hold wins over it.
    #[test]
    fn a_head_whose_app_build_is_in_flight_is_rechecked_on_the_short_ladder() {
        let base = Duration::from_secs(INTERVAL_SECS);
        let mut c = Cadence::new(base);
        c.succeeded(0);
        assert_eq!(
            c.nominal(),
            base,
            "no head in flight: the ordinary interval"
        );
        let waits: Vec<Duration> = (1..=4)
            .map(|checks| {
                c.succeeded(checks);
                c.nominal()
            })
            .collect();
        assert_eq!(
            waits,
            [120, 240, 480, INTERVAL_SECS].map(Duration::from_secs),
            "2, 4, 8 minutes, then the interval"
        );
        c.succeeded(u32::MAX);
        assert_eq!(
            c.nominal(),
            base,
            "a head that stays source-only for a night"
        );
        for end in [Cadence::failed, Cadence::failed_offline, Cadence::woke] {
            c.succeeded(1);
            end(&mut c);
            assert_ne!(c.nominal(), Duration::from_secs(120), "cleared");
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

    /// The quick ladder never waits LONGER than the ordinary one: under a base
    /// shorter than its rungs, every rung is the base.
    #[test]
    fn the_in_flight_ladder_never_outwaits_the_ordinary_one() {
        let short = Duration::from_secs(75);
        let mut c = Cadence::new(short);
        for checks in 1..=3 {
            c.succeeded(checks);
            assert_eq!(c.nominal(), short);
        }
        assert_eq!(in_flight_retry(0), None, "0 is no head in flight");
        assert_eq!(in_flight_retry(1), Some(Duration::from_secs(120)));
        assert_eq!(in_flight_retry(3), Some(Duration::from_secs(480)));
        assert_eq!(in_flight_retry(4), None, "the rungs are spent");
    }

    /// Exits 6, 7 and 28 — could not resolve, could not connect, timed out — in both
    /// spellings the transport writes are an unreachable network; every other curl
    /// failure, and another tool's exit 7, is not.
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

    #[test]
    fn a_base_longer_than_the_cap_is_respected_and_still_backs_off() {
        // A one-hour base must not be silently shortened to 15 min by the cap; the
        // cap bounds BACKOFF, it is not a ceiling on the base interval. It must not silently DELETE the backoff either, which
        // is what `min(MAX_BACKOFF.max(base))` did for every base at or above the cap:
        // the wait stayed at exactly the base no matter how many checks in a row
        // failed.
        let hour = Duration::from_secs(3600);
        let mut c = Cadence::new(hour);
        assert_eq!(c.nominal(), hour);
        c.failed();
        assert_eq!(
            c.nominal(),
            hour,
            "the first retry is still the base interval"
        );
        c.failed();
        assert!(
            c.nominal() > hour,
            "a configured interval is a floor on the wait, not a cap on the backoff"
        );
        for _ in 0..5 {
            c.failed();
        }
        assert_eq!(c.nominal(), hour * MAX_BACKOFF_INTERVALS);
    }

    /// Regression, and the reason the ceiling is now relative: [`MAX_BACKOFF`] and the
    /// interval were BOTH 15 minutes at the time, so the old
    /// `min(MAX_BACKOFF.max(base))` clamp returned the base for every failure count —
    /// a client that could not reach GitHub retried at full speed forever. A host that
    /// is failing deserves a genuine retreat from [`INTERVAL_SECS`], not a clamp back
    /// to full speed.
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
            anon * MAX_BACKOFF_INTERVALS,
            "and it climbs to the RELATIVE ceiling, above the absolute 15-minute one"
        );
        c.succeeded(0);
        assert_eq!(c.nominal(), anon, "recovery snaps back to the base");
    }

    /// A hold (a sibling's window end) is waited out EXACTLY — not doubled, not
    /// jittered — however many failures preceded it.
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
        // A hold still in force but within the floor waits the floor, never ~zero.
        c.hold_until(now + Duration::from_secs(1));
        assert_eq!(c.nominal_at(now), HOLD_FLOOR);
        // A hold already in the past (a reset the clock skewed behind us) is over:
        // the ladder applies again — and a hold never raised it, so this is still 4×.
        c.hold_until(now - Duration::from_secs(1));
        assert_eq!(c.nominal_at(now), anon * 4);
    }

    /// A hold can never exceed the ladder's own ceiling, and every event that clears
    /// the ladder clears the hold too.
    #[test]
    fn a_hold_is_bounded_by_the_cap_and_cleared_by_success_or_wake() {
        let anon = Duration::from_secs(INTERVAL_SECS);
        let now = Instant::now();
        let mut c = Cadence::new(anon);
        c.hold_until(now + Duration::from_secs(10 * 3600));
        assert_eq!(c.nominal_at(now), anon * MAX_BACKOFF_INTERVALS, "capped");
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
        // An expired hold is simply over: the ladder applies again.
        let mut expired = Cadence::new(anon);
        expired.hold_until(now + Duration::from_secs(600));
        assert_eq!(
            expired.nominal_at(now + Duration::from_secs(601)),
            anon,
            "past the epoch the base interval is back"
        );
    }

    /// A DEFERRAL HOLDS ITS OWN PROCESS TO THE WINDOW IT STAMPED (2026-09-24: the
    /// first rung, one base ±20 %, woke the deferring process inside its own 1.4-base
    /// window). Every entropy byte now waits at least to the window's end; a rung that
    /// already outlasts the window keeps its ordinary jittered wait; no window known is
    /// a plain failure. The count moves exactly as `failed` moves it.
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
            // NEGATIVE CONTROL: the ladder alone wakes inside the window, every byte.
            let mut ladder = Cadence::new(base);
            ladder.failed();
            assert!(ladder.delay_at(now, entropy) < base * 14 / 10);
        }
        // The second deferral in a row: the ladder's shortest wait (2 × base × 0.8)
        // already outlasts the window, so the ordinary jittered ladder stands.
        let mut c = Cadence::new(base);
        c.failed();
        c.deferred(Some(window_end), now, 255);
        assert_eq!(c.failures(), 2);
        assert_eq!(c.delay_at(now, 0), jitter(base * 2, 0));
        assert_eq!(c.delay_at(now, 255), jitter(base * 2, 255));
        // No window known (no readable receipt): exactly `failed`.
        let mut c = Cadence::new(base);
        c.deferred(None, now, 0);
        assert_eq!(c.delay_at(now, 0), jitter(base, 0));
        // And a success clears it like any hold.
        let mut c = Cadence::new(base);
        c.deferred(Some(window_end), now, 0);
        c.succeeded(0);
        assert_eq!(c.nominal_at(now), base);
    }

    /// The loop publishes when it looks next (`aterm ctl update status`'s
    /// `next_check=`): now plus the wait it is about to start — for as long as that
    /// wait lasts, and no longer. Once the wait is over (elapsed, or cut short by a
    /// wake and its settle) the loop is checking, and a past instant is not shown.
    #[test]
    fn the_loop_publishes_its_next_check() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_280_801);
        note_next_check(at, Duration::from_secs(600));
        assert_eq!(next_check_unix(), Some(1_790_281_401));

        let base = Duration::from_secs(INTERVAL_SECS);
        for waited in [Waited::Elapsed, Waited::Woke(Duration::from_secs(3600))] {
            let mut during = None;
            let before = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs();
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
        // DNS failure → auth failure is genuinely new information.
        let LogAction::Warn(text) = log.failure("GitHub auth failed (HTTP 401)") else {
            panic!("a changed message must be reported");
        };
        assert!(text.contains("401"), "{text}");
        assert!(
            text.contains("3 consecutive") && !text.contains("suppressed"),
            "the streak is carried forward, not the log's own suppression count: {text}"
        );
    }

    /// A network that is not up yet is said at INFO while the quick rungs run, and the
    /// same failure is WARNED — not suppressed as a repeat — the moment it outlasts
    /// them. Once warned, repeats collapse as before.
    #[test]
    fn an_expected_failure_is_info_until_it_outlasts_the_short_ladder() {
        let mut log = FailureLog::default();
        let msg = "curl: (6) Could not resolve host: github.com";
        let LogAction::Log(first) = log.failure_expected(msg, true) else {
            panic!("a network that is not up yet is news, not a warning");
        };
        assert!(first.contains("Could not resolve host"), "{first}");
        assert_eq!(log.failure_expected(msg, true), LogAction::Suppress);
        let LogAction::Warn(escalated) = log.failure_expected(msg, false) else {
            panic!("past the quick rungs the same failure must be warned");
        };
        assert!(
            escalated.contains("3 consecutive") && !escalated.contains("identical"),
            "{escalated}"
        );
        assert_eq!(log.failure_expected(msg, false), LogAction::Suppress);
        let Some(LogAction::Log(recovered)) = log.success() else {
            panic!("the recovery is logged");
        };
        assert!(recovered.contains("after 4 consecutive"), "{recovered}");
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
        assert!(text.contains("after 5 consecutive"), "{text}");
        assert!(log.success().is_none(), "and only once");
        // One failure is counted, not hedged with "(s)".
        let _ = log.failure("offline");
        assert_eq!(
            log.success(),
            Some(LogAction::Log(
                "update check recovered after 1 failure".to_string()
            ))
        );
    }

    #[test]
    fn wait_returns_promptly_for_a_short_delay() {
        // A cheap end-to-end guard that the slicing loop terminates and does not
        // over-sleep; the wake path itself is covered purely by `is_wake_gap`.
        let start = Instant::now();
        assert_eq!(
            sleep_watching_for_wake(Duration::from_millis(50)),
            Waited::Elapsed
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(sleep_watching_for_wake(Duration::ZERO), Waited::Elapsed);
    }
}
