// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The update checker's heartbeat.
//!
//! The check loop is one long-lived thread that a panic, or a stopped or hung sibling
//! holding `checker.lock`, could silence for days. The loop stamps a heartbeat (when,
//! in which [`CheckerPhase`], under which generation) at the top of every cycle, around
//! the lock wait and the check, and on a settings miss; [`classify`] judges it against
//! the phase's budget; the window's watchdog (`aterm-gui`'s `update_checker_watch`)
//! turns a stall into a log line, a health warning, `checker_stalled=` and a
//! replacement thread under a new generation. A superseded thread that wakes reads its
//! generation as stale and exits, so there are never two live checkers.
//!
//! Ages are `Instant` running time (`CLOCK_UPTIME_RAW` on macOS, `CLOCK_MONOTONIC` on
//! Linux), which stops while the machine sleeps: a closed lid is never a stall.
//!
//! Generation, phase and stamp share one `AtomicU64`, so a late beat from a superseded
//! thread can never overwrite its replacement's, and two looks at one stall spawn one
//! replacement ([`CheckerWatch::supersede`]). No mutex: the main thread's look never
//! waits on the checker.
//!
//! A cycle holds the check lane and `checker.lock` from its lock wait to the end of its
//! check, so a generation stalled there blocks its replacement, whose own heartbeat
//! stays fresh. [`CheckerWatch::hold_lane`] keeps the holder beside the heartbeat, and
//! [`standing_stall`] names a retired holder's last stamp while it holds the lane.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Where the check loop last stamped the heartbeat. It picks the budget
/// ([`phase_budget_secs`]) and is what `checker_stalled=<secs>:<phase>` names: a stall
/// in the lock wait and one inside a check are different bugs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckerPhase {
    /// Registered, before the loop's first stamp; a respawn resets to it, so a
    /// replacement that never started is judged too.
    Starting,
    /// Asking the host for the current channel and policy (bounded by the host's
    /// own 2 s query), or pausing after it did not answer.
    Settings,
    /// Reading the bundle under this executable and, when it is newer, running the
    /// codesign policy on it (bounded by the helper timeout).
    BundleProbe,
    /// Waiting for the machine-wide `checker.lock` (bounded by
    /// [`CHECKER_LOCK_WAIT`]).
    LockWait,
    /// Inside the network check and stage. Every download progress report re-stamps it,
    /// so only a check that stops moving goes stale.
    Checking,
    /// The cadence wait between cycles (jittered, backed off, wake-aware).
    Waiting,
}

impl CheckerPhase {
    #[inline]
    const fn tag(self) -> u64 {
        match self {
            Self::Starting => 1,
            Self::Settings => 2,
            Self::BundleProbe => 3,
            Self::LockWait => 4,
            Self::Checking => 5,
            Self::Waiting => 6,
        }
    }

    #[inline]
    const fn from_tag(tag: u64) -> Self {
        match tag {
            2 => Self::Settings,
            3 => Self::BundleProbe,
            4 => Self::LockWait,
            5 => Self::Checking,
            6 => Self::Waiting,
            _ => Self::Starting,
        }
    }

    /// The phase's wire word, which `aterm ctl update status` prints; log lines use
    /// [`Self::words`].
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Settings => "settings",
            Self::BundleProbe => "bundle-probe",
            Self::LockWait => "lock-wait",
            Self::Checking => "checking",
            Self::Waiting => "waiting",
        }
    }

    /// The phase in a person's words, for log lines (as
    /// `aterm_gui::update_words::checker_stalled_body` says it).
    #[inline]
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Settings => "reading its settings",
            Self::BundleProbe => "reading the installed app",
            Self::LockWait => "waiting for another aterm's check to finish",
            Self::Checking => "checking for a new version",
            Self::Waiting => "waiting between checks",
        }
    }
}

/// How long one cycle waits for the machine-wide `checker.lock` before recording a
/// deferral. Twice the check's HEAD timeout (curl `--max-time 30`): a sibling's routine
/// check ends inside it and becomes the ordinary "a sibling just checked" skip; a holder
/// past it is downloading, stopped or hung.
pub const CHECKER_LOCK_WAIT: Duration = Duration::from_secs(60);

/// After how many consecutive cycles behind a `checker.lock` held past
/// [`CHECKER_LOCK_WAIT`] by a holder showing no progress a process checks without the
/// lock. A working holder rewrites the lock (the holder beat, downloads included), so
/// deferring to it counts toward nothing; three intervals behind an unwritten lock is a
/// stopped (`SIGSTOP`, ctrl-z) or hung aterm. The gate saves cost, not correctness:
/// checking without it costs one request per interval, and the bounded stage lock
/// (`install::BACKGROUND_LOCK_WAIT`) still serializes the download. The window's
/// watchdog (`aterm-gui`'s `update_checker_watch`) warns when a streak reaches it.
pub const CHECKER_UNGATED_AFTER: u64 = 3;

/// The longest a check may run between two heartbeats: its non-download steps, each
/// bounded (requests by curl's `--max-time`, retried; stage and publish lock waits 2 min
/// each; container unpack 5 min; codesign/Gatekeeper helpers 30 s each). A download
/// re-stamps on every progress report, so it never spends this budget.
pub const CHECK_PHASE_BUDGET: Duration = Duration::from_secs(30 * 60);

/// Slack on top of twice the budget (scheduling noise, the watchdog's wake granularity),
/// so a merely late stamp is never a stall.
pub const STALL_SLACK: Duration = Duration::from_secs(5 * 60);

/// How long a stale verdict must stand before the watchdog acts: the first stale look
/// arms a second this much later, and only the same stale stamp then is a stall.
///
/// `CLOCK_UPTIME_RAW` leaves out machine sleep but runs while this process is stopped
/// (`SIGSTOP`, a debugger), so the first look after a continue sees every stamp as old
/// as the stop. A continued checker stamps within [`CHECKER_LOCK_WAIT`] of its lock
/// wait, the post-wake settle plus one slice of its cadence wait, or the end of a check's
/// current bounded step, so one [`STALL_SLACK`] later a healthy loop has moved.
pub const STALL_CONFIRM: Duration = STALL_SLACK;

/// How many replacement threads one process may start. Each leaves the stalled thread
/// behind (it can only be told to exit when it wakes), and a stall that survives three
/// replacements is not one a fourth will fix.
pub const MAX_RESPAWNS: u64 = 3;

/// The process-wide monotonic anchor every stamp is measured from.
fn anchor() -> Instant {
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    *ANCHOR.get_or_init(Instant::now)
}

/// Running seconds since this process's anchor: the clock every stamp is judged on.
#[must_use]
pub fn now_secs() -> u64 {
    anchor().elapsed().as_secs()
}

/// The `Instant` a stamp-relative number of running seconds names.
#[must_use]
pub fn instant_at(secs: u64) -> Instant {
    anchor() + Duration::from_secs(secs)
}

const GEN_SHIFT: u32 = 48;
const PHASE_SHIFT: u32 = 40;
const SECS_MASK: u64 = (1 << PHASE_SHIFT) - 1;
const PHASE_MASK: u64 = 0xff;

#[inline]
const fn pack(generation: u64, phase: CheckerPhase, secs: u64) -> u64 {
    (generation << GEN_SHIFT) | (phase.tag() << PHASE_SHIFT) | (secs & SECS_MASK)
}

#[inline]
const fn generation_of(word: u64) -> u64 {
    word >> GEN_SHIFT
}

#[inline]
const fn phase_of(word: u64) -> CheckerPhase {
    CheckerPhase::from_tag((word >> PHASE_SHIFT) & PHASE_MASK)
}

/// One read of the heartbeat: everything [`classify`] and the status line need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckerBeat {
    /// The live thread's generation (1 for the first spawn, +1 per replacement).
    pub generation: u64,
    /// Where it last stamped.
    pub phase: CheckerPhase,
    /// When it last stamped, in [`now_secs`] running seconds.
    pub at_secs: u64,
    /// The loop's longest legitimate wait between two stamps (`Cadence::max_wait`,
    /// `cadence_bounds::LONGEST_WAIT` for the shipping schedule), restated every cycle.
    pub max_interval_secs: u64,
    /// Replacement threads this process has started.
    pub respawns: u64,
    /// Consecutive cycles that deferred to a `checker.lock` held past
    /// [`CHECKER_LOCK_WAIT`] by a holder showing no progress; 0 while the holder is seen
    /// working.
    pub deferrals: u64,
    /// Consecutive times the host did not answer the settings query.
    pub settings_misses: u64,
    /// The loop generation holding this process's check lane, with its phase and stamp:
    /// when it took the lane, or, once retired, its last stamp. `None` while no loop
    /// generation holds it (a manual check may).
    pub lane: Option<LaneHolder>,
    /// Checks the loop completed in this process (any generation or result, sibling
    /// skips included): how the window's watchdog sees a replacement checking again.
    pub checks: u64,
}

/// Who holds the check lane for the loop, and from when ([`CheckerBeat::lane`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneHolder {
    /// The holding generation.
    pub generation: u64,
    /// Its phase: [`CheckerPhase::LockWait`] from the moment it took the lane,
    /// or, once it was retired, the phase it stalled in.
    pub phase: CheckerPhase,
    /// [`now_secs`] running seconds: when it took the lane, or its last stamp.
    pub at_secs: u64,
}

impl CheckerBeat {
    /// The retired generation still holding this process's check lane, if the live
    /// checker is stuck behind one: a replacement cannot check until it lets go.
    #[inline]
    #[must_use]
    pub fn blocked_by(&self) -> Option<LaneHolder> {
        self.lane
            .filter(|holder| holder.generation != self.generation)
    }

    /// Whether the live generation itself holds the check lane.
    #[inline]
    #[must_use]
    pub fn holds_lane(&self) -> bool {
        self.lane
            .is_some_and(|holder| holder.generation == self.generation)
    }
}

/// What [`classify`] makes of a beat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckerVerdict {
    /// Stamped recently enough for its phase; it goes stale `stale_in_secs` from
    /// now unless it stamps again.
    Fresh {
        /// Running seconds until the stamp crosses its threshold.
        stale_in_secs: u64,
    },
    /// Not stamped for `for_secs` running seconds, past twice its phase's budget
    /// plus [`STALL_SLACK`].
    Stalled {
        /// Running seconds since the last stamp.
        for_secs: u64,
        /// The phase it stamped last: where it is stuck.
        phase: CheckerPhase,
    },
}

/// The budget one stamp in `phase` is judged against: how long the loop may
/// legitimately go before its next stamp from there.
#[inline]
#[must_use]
pub fn phase_budget_secs(phase: CheckerPhase, max_interval_secs: u64) -> u64 {
    match phase {
        CheckerPhase::LockWait => CHECKER_LOCK_WAIT.as_secs(),
        CheckerPhase::Checking => CHECK_PHASE_BUDGET.as_secs(),
        // A handoff successor holds its first cycle until the parent commits; that
        // bound floors the published interval.
        CheckerPhase::Starting => {
            max_interval_secs.max(crate::UNCOMMITTED_CANDIDATE_HOLD_BOUND.as_secs())
        }
        CheckerPhase::Settings | CheckerPhase::BundleProbe | CheckerPhase::Waiting => {
            max_interval_secs
        }
    }
}

/// The running-second age past which a stamp in `phase` is a stall: twice the
/// phase's budget plus [`STALL_SLACK`].
#[inline]
#[must_use]
pub fn stall_threshold_secs(phase: CheckerPhase, max_interval_secs: u64) -> u64 {
    phase_budget_secs(phase, max_interval_secs)
        .saturating_mul(2)
        .saturating_add(STALL_SLACK.as_secs())
}

/// Judge `beat` at `now_secs`. A stamp from the future (impossible on one monotonic
/// anchor) is fresh.
#[inline]
#[must_use]
pub fn classify(beat: &CheckerBeat, now_secs: u64) -> CheckerVerdict {
    let age = now_secs.saturating_sub(beat.at_secs);
    let threshold = stall_threshold_secs(beat.phase, beat.max_interval_secs);
    if age > threshold {
        CheckerVerdict::Stalled {
            for_secs: age,
            phase: beat.phase,
        }
    } else {
        CheckerVerdict::Fresh {
            stale_in_secs: threshold - age + 1,
        }
    }
}

/// The stall `aterm ctl update status` names, as (running seconds since the last stamp,
/// phase): the live generation's when stale ([`classify`]), else that of a retired
/// generation still holding the check lane ([`CheckerBeat::blocked_by`]), which no
/// replacement can clear. `None` while checks can run.
#[must_use]
pub fn standing_stall(beat: &CheckerBeat, now_secs: u64) -> Option<(u64, CheckerPhase)> {
    if let CheckerVerdict::Stalled { for_secs, phase } = classify(beat, now_secs) {
        return Some((for_secs, phase));
    }
    beat.blocked_by()
        .map(|holder| (now_secs.saturating_sub(holder.at_secs), holder.phase))
}

/// The heartbeat. The process's checker stamps [`WATCH`]; tests build their own.
pub struct CheckerWatch {
    /// `generation << 48 | phase << 40 | running secs`.
    beat: AtomicU64,
    /// The check lane's loop holder, packed the same way; `0` while none holds
    /// it ([`Self::hold_lane`], [`CheckerBeat::lane`]).
    lane: AtomicU64,
    max_interval: AtomicU64,
    respawns: AtomicU64,
    deferrals: AtomicU64,
    settings_misses: AtomicU64,
    checks: AtomicU64,
    /// Optional host event-loop wake on a phase change; progress stamps never call it,
    /// so an idle window is not woken per download report.
    phase_wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl Default for CheckerWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl CheckerWatch {
    /// An unregistered heartbeat: no checker has started in this process.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            beat: AtomicU64::new(0),
            lane: AtomicU64::new(0),
            max_interval: AtomicU64::new(0),
            respawns: AtomicU64::new(0),
            deferrals: AtomicU64::new(0),
            settings_misses: AtomicU64::new(0),
            checks: AtomicU64::new(0),
            phase_wake: OnceLock::new(),
        }
    }

    /// Wake the host on a phase change, before the old deadline expires. The GUI sets it;
    /// a terminal-only session has no event loop and leaves it unset. First one wins.
    pub fn set_phase_wake(&self, wake: Box<dyn Fn() + Send + Sync>) {
        let _ = self.phase_wake.set(wake);
    }

    /// Register a checker thread, stamped `Starting` now, and return its generation. A
    /// second registration takes the next generation, retiring the first at its next
    /// look: one live checker per process.
    pub fn register(&self, max_interval_secs: u64) -> u64 {
        self.max_interval
            .store(max_interval_secs, Ordering::Relaxed);
        let now = now_secs();
        let mut next = 0;
        let _ = self
            .beat
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                next = generation_of(word) + 1;
                Some(pack(next, CheckerPhase::Starting, now))
            });
        next
    }

    /// Stamp `phase` for `generation`, now. `false`, with nothing written, when that
    /// generation was superseded: the caller is a stalled thread that woke, and exits.
    pub fn beat(&self, generation: u64, phase: CheckerPhase) -> bool {
        let now = now_secs();
        let previous = self
            .beat
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                (generation_of(word) == generation).then(|| pack(generation, phase, now))
            });
        if let Ok(previous) = previous {
            if phase_of(previous) != phase
                && let Some(wake) = self.phase_wake.get()
            {
                wake();
            }
            true
        } else {
            false
        }
    }

    /// A download progress report: re-stamp the live generation only while it is
    /// `Checking`. One check runs per process (the check lane), so a report in any
    /// other phase is a manual check's.
    pub fn beat_progress(&self) {
        let now = now_secs();
        let _ = self
            .beat
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                let phase = phase_of(word);
                (generation_of(word) != 0 && phase == CheckerPhase::Checking)
                    .then(|| pack(generation_of(word), phase, now))
            });
    }

    /// Whether `generation` is still the live checker.
    #[must_use]
    pub fn is_current(&self, generation: u64) -> bool {
        generation_of(self.beat.load(Ordering::Acquire)) == generation
    }

    /// Publish the loop's longest legitimate wait between stamps, restated every cycle
    /// from `Cadence::max_wait`. Ignored from a superseded thread.
    pub fn set_max_interval(&self, generation: u64, secs: u64) {
        if self.is_current(generation) {
            self.max_interval.store(secs, Ordering::Relaxed);
        }
    }

    /// Publish the consecutive count of `checker.lock` deferrals to a holder
    /// showing no progress ([`CheckerBeat::deferrals`]).
    pub fn set_deferrals(&self, generation: u64, n: u64) {
        if self.is_current(generation) {
            self.deferrals.store(n, Ordering::Relaxed);
        }
    }

    /// Publish the consecutive settings-miss count.
    pub fn set_settings_misses(&self, generation: u64, n: u64) {
        if self.is_current(generation) {
            self.settings_misses.store(n, Ordering::Relaxed);
        }
    }

    /// Record `generation` as the check lane's holder until the returned mark drops,
    /// where the cycle lets go of the lane.
    #[must_use = "the lane is marked held only while the mark lives"]
    pub fn hold_lane(&self, generation: u64) -> LaneMark<'_> {
        self.lane.store(
            pack(generation, CheckerPhase::LockWait, now_secs()),
            Ordering::Release,
        );
        LaneMark {
            watch: self,
            generation,
        }
    }

    /// A check completed, or was skipped behind a sibling's: the loop is checking.
    /// Counted for the live generation only.
    pub fn note_check(&self, generation: u64) {
        if self.is_current(generation) {
            self.checks.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The heartbeat as it stands; `None` when no checker was ever registered here
    /// (automatic checks off, an uninstalled copy, a headless run).
    #[must_use]
    pub fn snapshot(&self) -> Option<CheckerBeat> {
        let word = self.beat.load(Ordering::Acquire);
        let generation = generation_of(word);
        let lane = self.lane.load(Ordering::Acquire);
        (generation != 0).then(|| CheckerBeat {
            generation,
            phase: phase_of(word),
            at_secs: word & SECS_MASK,
            max_interval_secs: self.max_interval.load(Ordering::Relaxed),
            respawns: self.respawns.load(Ordering::Relaxed),
            deferrals: self.deferrals.load(Ordering::Relaxed),
            settings_misses: self.settings_misses.load(Ordering::Relaxed),
            lane: (generation_of(lane) != 0).then(|| LaneHolder {
                generation: generation_of(lane),
                phase: phase_of(lane),
                at_secs: lane & SECS_MASK,
            }),
            checks: self.checks.load(Ordering::Acquire),
        })
    }

    /// Retire `stalled` and return its replacement's generation, stamped `Starting` now.
    /// `None` when `stalled` is no longer live (one replacement per stall) or
    /// [`MAX_RESPAWNS`] is spent.
    pub fn supersede(&self, stalled: u64) -> Option<u64> {
        if stalled == 0 || self.respawns.load(Ordering::Acquire) >= MAX_RESPAWNS {
            return None;
        }
        let now = now_secs();
        let next = stalled + 1;
        let last = self
            .beat
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                (generation_of(word) == stalled).then(|| pack(next, CheckerPhase::Starting, now))
            })
            .ok()?;
        // A retired generation keeps the lane until it wakes, so its last stamp stays
        // beside the lane for the status line.
        let _ = self
            .lane
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                (generation_of(word) == stalled).then_some(last)
            });
        self.respawns.fetch_add(1, Ordering::AcqRel);
        self.deferrals.store(0, Ordering::Relaxed);
        self.settings_misses.store(0, Ordering::Relaxed);
        Some(next)
    }

    /// The replacement [`Self::supersede`] counted was never started (no thread): give
    /// its slot back. Its `Starting` stamp goes stale and the watchdog tries again.
    pub fn unstarted(&self) {
        let _ = self
            .respawns
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
    }
}

/// The check lane marked held by one generation ([`CheckerWatch::hold_lane`]); dropping
/// it clears the mark unless another generation has marked the lane since.
pub struct LaneMark<'a> {
    watch: &'a CheckerWatch,
    generation: u64,
}

impl Drop for LaneMark<'_> {
    fn drop(&mut self) {
        let _ = self
            .watch
            .lane
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                (generation_of(word) == self.generation).then_some(0)
            });
    }
}

/// This process's heartbeat, stamped by the shipping loop and read by the window.
pub static WATCH: CheckerWatch = CheckerWatch::new();

/// The process heartbeat as it stands (see [`CheckerWatch::snapshot`]).
#[must_use]
pub fn checker_snapshot() -> Option<CheckerBeat> {
    WATCH.snapshot()
}

/// A consecutive-repeat counter deciding which repeats get a log line: the first of a
/// streak, every `every`-th, and the streak's end.
#[derive(Clone, Copy, Debug)]
pub struct StreakLog {
    consecutive: u64,
    every: u64,
}

/// What [`StreakLog::note`] asks the caller to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Streak {
    /// The streak just started: say so.
    Began,
    /// The streak reached another multiple of `every`: say it is still going, with
    /// this count.
    Continues(u64),
    /// A repeat inside the quiet stretch: say nothing.
    Quiet,
}

impl StreakLog {
    /// A counter that repeats its line every `every` consecutive notes (at least 2).
    #[must_use]
    pub const fn new(every: u64) -> Self {
        Self {
            consecutive: 0,
            every: if every < 2 { 2 } else { every },
        }
    }

    /// One more repeat.
    pub fn note(&mut self) -> Streak {
        self.consecutive = self.consecutive.saturating_add(1);
        if self.consecutive == 1 {
            Streak::Began
        } else if self.consecutive.is_multiple_of(self.every) {
            Streak::Continues(self.consecutive)
        } else {
            Streak::Quiet
        }
    }

    /// The streak ended; `Some(length)` when there was one to end (the recovery
    /// line is owed), `None` when nothing was running.
    pub fn clear(&mut self) -> Option<u64> {
        let ended = self.consecutive;
        self.consecutive = 0;
        (ended > 0).then_some(ended)
    }

    /// The current streak length.
    #[must_use]
    pub const fn consecutive(&self) -> u64 {
        self.consecutive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn beat(phase: CheckerPhase, at: u64, max_interval: u64) -> CheckerBeat {
        CheckerBeat {
            generation: 1,
            phase,
            at_secs: at,
            max_interval_secs: max_interval,
            respawns: 0,
            deferrals: 0,
            settings_misses: 0,
            lane: None,
            checks: 0,
        }
    }

    /// The loop's longest wait, as it registers it (`cadence_bounds::LONGEST_WAIT`).
    const LONGEST: u64 = crate::cadence_bounds::LONGEST_WAIT.as_secs();

    /// Each phase is judged against its own budget: a waiting loop may be quiet for
    /// twice the loop's longest wait, a lock wait only for twice its bound, and a check
    /// for twice the longest non-download step — so a checker parked on a lock is called
    /// out in minutes.
    #[test]
    fn a_stamp_is_stale_past_twice_its_phase_budget_plus_slack_and_not_before() {
        let slack = STALL_SLACK.as_secs();
        for (phase, budget) in [
            (CheckerPhase::Waiting, LONGEST),
            (CheckerPhase::Settings, LONGEST),
            (CheckerPhase::BundleProbe, LONGEST),
            (CheckerPhase::LockWait, CHECKER_LOCK_WAIT.as_secs()),
            (CheckerPhase::Checking, CHECK_PHASE_BUDGET.as_secs()),
        ] {
            let limit = 2 * budget + slack;
            let b = beat(phase, 1_000, LONGEST);
            assert_eq!(
                classify(&b, 1_000 + limit),
                CheckerVerdict::Fresh { stale_in_secs: 1 },
                "{phase:?}: at the threshold it is still fresh"
            );
            assert_eq!(
                classify(&b, 1_000 + limit + 1),
                CheckerVerdict::Stalled {
                    for_secs: limit + 1,
                    phase
                },
                "{phase:?}: one second past it is a stall, named by its phase"
            );
        }
        // A thread parked in the lock wait is a stall within minutes.
        let parked = beat(CheckerPhase::LockWait, 0, LONGEST);
        assert!(matches!(
            classify(&parked, 10 * 60),
            CheckerVerdict::Stalled {
                phase: CheckerPhase::LockWait,
                ..
            }
        ));
        // A handoff successor's first cycle may hold for the uncommitted-candidate bound:
        // the cadence's longest wait covers it, and the bound floors a smaller interval.
        let hold = crate::UNCOMMITTED_CANDIDATE_HOLD_BOUND.as_secs();
        assert!(hold <= LONGEST, "the cadence's wait covers the hold");
        assert_eq!(phase_budget_secs(CheckerPhase::Starting, LONGEST), LONGEST);
        assert_eq!(phase_budget_secs(CheckerPhase::Starting, 0), hold);
        let starting = beat(CheckerPhase::Starting, 0, LONGEST);
        assert!(matches!(
            classify(&starting, 2 * hold),
            CheckerVerdict::Fresh { .. }
        ));
        // A stamp from the future is never a panic and never a stall.
        assert!(matches!(
            classify(&beat(CheckerPhase::Waiting, 50, LONGEST), 10),
            CheckerVerdict::Fresh { .. }
        ));
    }

    /// A phase change wakes the host so it can fold the shorter deadline; same-phase stamps
    /// and download progress do not wake an idle window.
    #[test]
    fn phase_change_wakes_host_to_replace_an_older_long_deadline() {
        let watch = CheckerWatch::new();
        let wakes = Arc::new(AtomicU64::new(0));
        let called = Arc::clone(&wakes);
        watch.set_phase_wake(Box::new(move || {
            called.fetch_add(1, Ordering::Relaxed);
        }));
        let generation = watch.register(LONGEST);
        assert!(watch.beat(generation, CheckerPhase::Settings));
        let old = watch.snapshot().unwrap();
        let old_deadline = stall_threshold_secs(old.phase, old.max_interval_secs);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);

        assert!(watch.beat(generation, CheckerPhase::LockWait));
        let new = watch.snapshot().unwrap();
        let new_deadline = stall_threshold_secs(new.phase, new.max_interval_secs);
        assert!(
            new_deadline < old_deadline / 5,
            "the new phase needs a prompt look"
        );
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
        assert!(watch.beat(generation, CheckerPhase::LockWait));
        watch.beat_progress();
        assert_eq!(wakes.load(Ordering::Relaxed), 2, "no unchanged-phase wakes");

        let replacement = watch.supersede(generation).unwrap();
        assert!(!watch.beat(generation, CheckerPhase::Checking));
        assert_eq!(
            wakes.load(Ordering::Relaxed),
            2,
            "a retired thread is silent"
        );
        assert!(watch.beat(replacement, CheckerPhase::Settings));
        assert_eq!(wakes.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn a_superseded_generation_cannot_beat_and_a_stall_is_replaced_once() {
        let watch = CheckerWatch::new();
        assert!(watch.snapshot().is_none(), "nothing registered yet");
        let first = watch.register(100);
        assert_eq!(first, 1);
        assert!(watch.beat(first, CheckerPhase::LockWait));
        let second = watch.supersede(first).expect("the stall is replaced");
        assert_eq!(second, 2);
        assert_eq!(watch.supersede(first), None, "and only once");
        assert!(!watch.is_current(first));
        assert!(
            !watch.beat(first, CheckerPhase::Checking),
            "the old thread's late beat is refused, and tells it to exit"
        );
        let snap = watch.snapshot().unwrap();
        assert_eq!(snap.generation, second);
        assert_eq!(snap.phase, CheckerPhase::Starting);
        assert_eq!(snap.respawns, 1);
        watch.set_max_interval(first, 1);
        assert_eq!(
            watch.snapshot().unwrap().max_interval_secs,
            100,
            "a superseded thread cannot move the budget either"
        );
        // The respawn budget is finite.
        let mut live = second;
        for _ in 1..MAX_RESPAWNS {
            live = watch.supersede(live).unwrap();
        }
        assert_eq!(watch.supersede(live), None, "MAX_RESPAWNS is a ceiling");
        assert_eq!(watch.snapshot().unwrap().respawns, MAX_RESPAWNS);
        // A replacement whose thread could not be created gives its slot back.
        watch.unstarted();
        assert_eq!(watch.snapshot().unwrap().respawns, MAX_RESPAWNS - 1);
        assert!(watch.supersede(live).is_some(), "the slot is spent again");
    }

    /// A generation stalled in its check holds the lane its replacement can never take; the
    /// lane keeps the holder's last stamp so the stall stays standing until it lets go.
    #[test]
    fn a_retired_generation_holding_the_lane_keeps_its_stall_standing_until_it_lets_go() {
        let watch = CheckerWatch::new();
        // A longest wait far past the instants below keeps the replacement's stamp fresh.
        let first = watch.register(10_000);
        let mark = watch.hold_lane(first);
        assert!(watch.beat(first, CheckerPhase::Checking));
        let stalled_at = watch.snapshot().unwrap().at_secs;
        assert!(watch.snapshot().unwrap().holds_lane(), "the live holder");
        assert_eq!(watch.snapshot().unwrap().blocked_by(), None);

        let second = watch.supersede(first).expect("replaced");
        // The replacement is healthy by its own heartbeat…
        assert!(watch.beat(second, CheckerPhase::Waiting));
        let live = watch.snapshot().unwrap();
        assert!(matches!(
            classify(&live, live.at_secs),
            CheckerVerdict::Fresh { .. }
        ));
        assert!(!live.holds_lane());
        // …but blocked behind the retired holder, whose stall stands.
        assert_eq!(
            live.blocked_by(),
            Some(LaneHolder {
                generation: first,
                phase: CheckerPhase::Checking,
                at_secs: stalled_at,
            })
        );
        assert_eq!(
            standing_stall(&live, stalled_at + 4_000),
            Some((4_000, CheckerPhase::Checking))
        );
        // Each generation's lane mark is its own: one letting go never clears the other's.
        drop(mark);
        let live = watch.snapshot().unwrap();
        assert_eq!(live.lane, None, "the stuck check let go");
        assert_eq!(standing_stall(&live, live.at_secs + 60), None);
        let own = watch.hold_lane(second);
        assert!(watch.snapshot().unwrap().holds_lane());
        drop(own);
        assert_eq!(watch.snapshot().unwrap().lane, None);
    }

    #[test]
    fn only_the_retired_holder_is_kept_and_only_the_live_generation_counts_checks() {
        let watch = CheckerWatch::new();
        let first = watch.register(100);
        watch.note_check(first);
        assert_eq!(watch.snapshot().unwrap().checks, 1);
        let second = watch.supersede(first).unwrap();
        assert_eq!(watch.snapshot().unwrap().lane, None, "it held nothing");
        watch.note_check(first);
        assert_eq!(
            watch.snapshot().unwrap().checks,
            1,
            "a retired thread's late check is not the live loop checking"
        );
        watch.note_check(second);
        assert_eq!(watch.snapshot().unwrap().checks, 2);
    }

    #[test]
    fn progress_restamps_only_a_check_in_flight() {
        let watch = CheckerWatch::new();
        watch.beat_progress();
        assert!(watch.snapshot().is_none(), "no checker, nothing to stamp");
        let g = watch.register(100);
        assert!(watch.beat(g, CheckerPhase::Waiting));
        watch.beat_progress();
        assert_eq!(
            watch.snapshot().unwrap().phase,
            CheckerPhase::Waiting,
            "a manual check's download says nothing about a waiting loop"
        );
        assert!(watch.beat(g, CheckerPhase::Checking));
        watch.beat_progress();
        let snap = watch.snapshot().unwrap();
        assert_eq!(snap.phase, CheckerPhase::Checking);
        assert_eq!(snap.generation, g);
    }

    #[test]
    fn a_streak_speaks_on_its_edges_and_every_nth_repeat() {
        let mut log = StreakLog::new(3);
        assert_eq!(log.clear(), None, "no streak, no recovery line");
        assert_eq!(log.note(), Streak::Began);
        assert_eq!(log.note(), Streak::Quiet);
        assert_eq!(log.note(), Streak::Continues(3));
        assert_eq!(log.note(), Streak::Quiet);
        assert_eq!(log.note(), Streak::Quiet);
        assert_eq!(log.note(), Streak::Continues(6));
        assert_eq!(log.consecutive(), 6);
        assert_eq!(log.clear(), Some(6));
        assert_eq!(log.clear(), None);
        assert_eq!(log.note(), Streak::Began, "a new streak begins again");
    }

    #[test]
    fn every_phase_round_trips_through_the_packed_word() {
        for phase in [
            CheckerPhase::Starting,
            CheckerPhase::Settings,
            CheckerPhase::BundleProbe,
            CheckerPhase::LockWait,
            CheckerPhase::Checking,
            CheckerPhase::Waiting,
        ] {
            let word = pack(7, phase, 12_345);
            assert_eq!(generation_of(word), 7);
            assert_eq!(
                CheckerPhase::from_tag((word >> PHASE_SHIFT) & PHASE_MASK),
                phase
            );
            assert_eq!(word & SECS_MASK, 12_345);
        }
    }
}
