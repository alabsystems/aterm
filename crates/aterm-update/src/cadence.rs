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
//!   moment a check succeeds.
//! * **No jitter.** Every aterm on every machine woke on the same 75 s grid relative
//!   to its own launch; a fleet restarted together stays in lockstep and hits the API
//!   in a thundering herd. [`Cadence::delay`] spreads each wait by ±[`JITTER_PCT`]%.
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

/// The FLOOR on the backoff ceiling — i.e. the ceiling that applies to a fast base
/// interval. Fifteen minutes is long enough that an offline laptop costs ~4 log lines
/// an hour instead of 48, and short enough that reconnecting still gets an update
/// within a coffee break.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// How many base intervals the backoff may grow to. The real ceiling is
/// `max(MAX_BACKOFF, MAX_BACKOFF_INTERVALS × base)` — see [`Cadence::cap`].
///
/// The ceiling used to be [`MAX_BACKOFF`] alone, raised to the base so that an
/// operator's long interval could never be silently SHORTENED by it:
/// `min(MAX_BACKOFF.max(base))`. On the anonymous lane that expression is
/// arithmetically inert — its base is 15 minutes, which IS `MAX_BACKOFF`, so the
/// ceiling equalled the base and every doubling was clamped straight back down to it.
/// The one lane that most needs to retreat while failing (a ~60 requests/hour budget
/// shared by every machine behind one NAT) was the one lane with no backoff at all,
/// and the same silent no-op applied to any operator interval at or above the cap.
/// A ceiling expressed in INTERVALS is inert for no base: four of them is a real
/// retreat (30 min → 60 → 120 on today's anonymous lane) while bounding the worst
/// case at 4× a cadence the lane or the operator has already accepted — and a wake,
/// or one healthy check, still snaps all the way back to the base, so recovery is
/// never rate-limited by the cap.
pub(crate) const MAX_BACKOFF_INTERVALS: u32 = 4;

/// Jitter applied to every wait, as a percentage either side of the nominal delay.
pub(crate) const JITTER_PCT: u64 = 20;

/// A wall-clock jump larger than the requested sleep by this much means the machine
/// was not running: system sleep, hibernation, a suspended VM, or a large clock step.
/// Well above any scheduling delay or routine NTP slew.
pub(crate) const SLEEP_GAP: Duration = Duration::from_secs(90);

/// How long to let the network come up after a detected wake before checking. A Mac
/// takes a few seconds to associate Wi-Fi and re-resolve DNS; checking inside that
/// window is a guaranteed failure that teaches the ledger nothing.
pub(crate) const WAKE_SETTLE: Duration = Duration::from_secs(20);

/// How long an unchanged failure message stays suppressed before being repeated.
pub(crate) const STILL_FAILING_AFTER: Duration = Duration::from_secs(30 * 60);

/// The base interval for a check on the AUTHENTICATED lane: a token buys 5000 GitHub
/// requests/hour, and ~5 requests per steady-state check on the armed tier (list +
/// manifest + roster + roster.sig + appcast.sig — 6 with a container download) is
/// ~240/hour — comfortably inside it, so the cadence can be the fast one the owner
/// asked for.
pub(crate) const AUTHENTICATED_INTERVAL_SECS: u64 = 75;

/// The base interval for a check on the ANONYMOUS lane.
///
/// Unauthenticated GitHub allows ~60 requests/hour PER IP — shared by every machine
/// behind one NAT, and by anything else on that address using the API. At 75 s a
/// single machine would spend ~240 requests/hour and live permanently rate-limited: it
/// would not update FASTER, it would not update at all. Since PAPER_MASTER_PUBKEYS
/// armed (2026-08-15) every production check costs 5 requests, not the pre-armed 3
/// this comment used to count — at 15 minutes that was 20/hour per machine, and
/// three or four Macs behind one NAT (the exact fleet the budget test protects)
/// blew the whole allowance and lived in rate-limit deferrals. Two checks an hour
/// costs ~10 requests/hour, leaving room for several machines and the retry budget,
/// while still picking a release up well inside the "one launch behind" bound the
/// crate docs promise. Provisioning a token restores the 75 s cadence automatically.
pub(crate) const ANONYMOUS_INTERVAL_SECS: u64 = 30 * 60;

/// The interval schedule: a base cadence plus the current consecutive-failure count.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cadence {
    base: Duration,
    failures: u32,
}

impl Cadence {
    /// A schedule at the configured base interval, starting healthy.
    pub(crate) fn new(base: Duration) -> Self {
        Self { base, failures: 0 }
    }

    /// Re-point the base interval (the credential lane is only known after the first
    /// completed check, so the loop adopts the lane's cadence as soon as it learns
    /// it). Leaves the failure count — and therefore any backoff in progress — alone.
    pub(crate) fn set_base(&mut self, base: Duration) {
        self.base = base;
    }

    /// The current base interval — the cross-process checker gate sizes its
    /// freshness window from it (see the check loop in `lib.rs`).
    pub(crate) fn base(&self) -> Duration {
        self.base
    }

    /// Note a failed check (lengthens the next wait).
    pub(crate) fn failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    /// Note a successful check — the next wait returns to the base interval
    /// immediately. Recovery must not be rate-limited by how long the outage was.
    pub(crate) fn succeeded(&mut self) {
        self.failures = 0;
    }

    /// A wake resets the backoff: the network the failures were about is gone, and
    /// the machine is in a genuinely new state.
    pub(crate) fn woke(&mut self) {
        self.failures = 0;
    }

    /// The ceiling on [`Self::nominal`] for THIS base: at least [`MAX_BACKOFF`], at
    /// least the base itself (a configured interval is a floor on the wait, never
    /// something a cap may shorten), and at most [`MAX_BACKOFF_INTERVALS`] × base —
    /// the term that keeps the ceiling strictly above the base for every lane, so a
    /// slow lane still backs off instead of clamping to where it started.
    fn cap(&self) -> Duration {
        MAX_BACKOFF.max(self.base.saturating_mul(MAX_BACKOFF_INTERVALS))
    }

    /// The nominal (pre-jitter) wait: `base` doubled once per consecutive failure,
    /// clamped to [`Self::cap`]. Exposed for tests; [`Self::delay`] is what the
    /// loop uses.
    pub(crate) fn nominal(&self) -> Duration {
        // `1 << 20` already exceeds any sane base × ceiling ratio; the shift is
        // clamped so a long outage can never overflow the multiply.
        let doublings = self.failures.saturating_sub(1).min(20);
        self.base.saturating_mul(1u32 << doublings).min(self.cap())
    }

    /// The actual wait: [`Self::nominal`] spread by ±[`JITTER_PCT`]%. `entropy` is a
    /// uniformly random byte; the caller supplies it so this stays pure and testable.
    pub(crate) fn delay(&self, entropy: u8) -> Duration {
        jitter(self.nominal(), entropy)
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
/// hand-rolled `/dev/urandom` read.
fn entropy_byte() -> u8 {
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
pub(crate) fn wait(cadence: &Cadence) -> (Duration, Waited) {
    let delay = cadence.delay(entropy_byte());
    (delay, sleep_watching_for_wake(delay))
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
///   news: DNS → auth, say);
/// * an identical repeat is suppressed until [`STILL_FAILING_AFTER`], then warned
///   once with the suppressed count, so the log always shows an ongoing outage
///   without showing it 48 times an hour;
/// * recovery is always logged, with how long/how many it took — the transition
///   nobody records and everybody wants.
#[derive(Debug, Default)]
pub(crate) struct FailureLog {
    last: Option<String>,
    /// Failures observed since the last emitted line (the first one included).
    since_emit: u32,
    /// Total consecutive failures in the current outage.
    streak: u32,
    emitted_at: Option<Instant>,
}

impl FailureLog {
    /// Record a failed check and decide what to say about it.
    pub(crate) fn failure(&mut self, message: &str) -> LogAction {
        self.streak = self.streak.saturating_add(1);
        self.since_emit = self.since_emit.saturating_add(1);
        let changed = self.last.as_deref() != Some(message);
        let stale = self
            .emitted_at
            .is_none_or(|t| t.elapsed() >= STILL_FAILING_AFTER);
        if !changed && !stale {
            return LogAction::Suppress;
        }
        let suppressed = self.since_emit.saturating_sub(1);
        self.last = Some(message.to_string());
        self.since_emit = 0;
        self.emitted_at = Some(Instant::now());
        LogAction::Warn(if suppressed == 0 {
            format!("update check failed: {message}")
        } else {
            format!(
                "update check still failing ({} consecutive, {suppressed} identical \
                 messages suppressed): {message}",
                self.streak
            )
        })
    }

    /// Record a successful check. Emits the recovery line iff there was an outage.
    pub(crate) fn success(&mut self) -> Option<LogAction> {
        let streak = std::mem::take(&mut self.streak);
        self.last = None;
        self.since_emit = 0;
        self.emitted_at = None;
        (streak > 0).then(|| {
            LogAction::Log(format!(
                "update check recovered after {streak} consecutive failure(s)"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_secs(75);

    #[test]
    fn healthy_cadence_is_the_base_interval() {
        assert_eq!(Cadence::new(BASE).nominal(), BASE);
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

    #[test]
    fn recovery_and_wake_both_snap_back_to_base() {
        let mut c = Cadence::new(BASE);
        for _ in 0..10 {
            c.failed();
        }
        assert!(c.nominal() > BASE);
        c.succeeded();
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

    /// The anonymous lane's budget is the whole reason `set_base` exists: at the
    /// authenticated 75 s cadence an unauthenticated machine spends ~150 GitHub
    /// requests/hour against a ~60/hour per-IP allowance and never gets a clean
    /// check. Adopting the lane's interval must not disturb a backoff in progress.
    #[test]
    fn the_anonymous_lane_fits_inside_githubs_unauthenticated_budget() {
        // 5 requests per steady-state check on the ARMED tier (releases list +
        // manifest + roster + roster.sig + appcast.sig — authorize_by_roster runs
        // before the downgrade gate on every production check since
        // PAPER_MASTER_PUBKEYS armed, 2026-08-15). A check that also fetches a
        // container spends 6, but that is the rare stage, not the steady state
        // this budget must sustain.
        const REQUESTS_PER_CHECK: u64 = 5;
        const ANON_BUDGET_PER_HOUR: u64 = 60;
        let anon_per_hour = (3600 / ANONYMOUS_INTERVAL_SECS) * REQUESTS_PER_CHECK;
        assert!(
            anon_per_hour * 4 <= ANON_BUDGET_PER_HOUR,
            "the anonymous cadence must leave headroom for several machines behind one \
             NAT: {anon_per_hour}/hour against a {ANON_BUDGET_PER_HOUR}/hour budget"
        );
        // Every operand is a constant, so this is decided at COMPILE time — the
        // split stops being meaningful the moment it stops holding, and a const
        // block says so by failing the build rather than one test run. The rate
        // is named rather than interpolated into the message: a const panic takes
        // a literal, so the number has to be readable in the source instead.
        const AUTHENTICATED_PER_HOUR: u64 =
            (3600 / AUTHENTICATED_INTERVAL_SECS) * REQUESTS_PER_CHECK;
        const {
            assert!(
                AUTHENTICATED_PER_HOUR > ANON_BUDGET_PER_HOUR,
                "…and the authenticated cadence must genuinely be too fast for it, or this \
                 whole split is pointless"
            )
        };

        let mut c = Cadence::new(Duration::from_secs(AUTHENTICATED_INTERVAL_SECS));
        c.failed();
        c.failed();
        let backed_off = c.nominal();
        c.set_base(Duration::from_secs(ANONYMOUS_INTERVAL_SECS));
        assert!(
            c.nominal() > backed_off,
            "adopting the slower lane must not shorten a wait"
        );
        c.succeeded();
        assert_eq!(
            c.nominal(),
            Duration::from_secs(ANONYMOUS_INTERVAL_SECS),
            "a healthy check returns to the LANE's interval, not the original one"
        );
    }

    #[test]
    fn a_base_longer_than_the_cap_is_respected_and_still_backs_off() {
        // `ATERM_UPDATE_INTERVAL_SECS=3600` must not be silently shortened to 15 min
        // by the cap; the cap bounds BACKOFF, it is not a ceiling on the operator's
        // configured interval. It must not silently DELETE the backoff either, which
        // is what `min(MAX_BACKOFF.max(base))` did for every base at or above the cap:
        // the wait stayed at exactly the base no matter how many checks in a row
        // failed.
        let hour = Duration::from_secs(3600);
        let mut c = Cadence::new(hour);
        assert_eq!(c.nominal(), hour);
        c.failed();
        assert_eq!(c.nominal(), hour, "the first retry is still the base interval");
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

    /// Regression, and the reason the ceiling is now relative: [`MAX_BACKOFF`] and
    /// [`ANONYMOUS_INTERVAL_SECS`] were BOTH 15 minutes at the time, so the old
    /// `min(MAX_BACKOFF.max(base))` clamp returned the base for every failure count —
    /// a tokenless client that could not reach GitHub retried at full speed forever,
    /// against the very ~60 requests/hour per-IP budget the slow lane exists to
    /// respect. The lane with the least request headroom had the least backoff.
    #[test]
    fn the_anonymous_lane_genuinely_backs_off_instead_of_clamping_to_its_own_base() {
        let anon = Duration::from_secs(ANONYMOUS_INTERVAL_SECS);
        let mut c = Cadence::new(anon);
        c.failed();
        assert_eq!(c.nominal(), anon, "the first retry is still the base interval");
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
        c.succeeded();
        assert_eq!(c.nominal(), anon, "recovery snaps back to the lane's base");
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
            text.contains("1 identical messages suppressed"),
            "the suppressed count is carried forward, not lost: {text}"
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
        assert!(text.contains("after 5 consecutive"), "{text}");
        assert!(log.success().is_none(), "and only once");
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
