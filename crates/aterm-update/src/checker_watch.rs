// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE UPDATE CHECKER'S HEARTBEAT (the 2026-09-22/23 update audit, plan P2-1).
//!
//! On the owner's machine the background check loop went silent for 4.6 days in a
//! live process: from 1788394817 onward it logged nothing, v0.74.0 and v0.75.0 were
//! published inside that window and never staged, and the machine left 0.73 only
//! when a fresh process started, 11.9 days after 0.74 shipped. Nothing noticed. The
//! loop was one thread, spawned once, never supervised: its `JoinHandle` was
//! dropped, a panic ended it, and it took the machine-wide `checker.lock` with a
//! BLOCKING `flock` while holding this process's check lane — so one stopped or
//! hung sibling process parked it (and every manual check behind it) forever. The
//! only trace a stall left was `stale_check=` on `aterm ctl update status`, four
//! hours in, for whoever thought to ask.
//!
//! This module is the half of the fix that makes a stall VISIBLE. The loop stamps a
//! heartbeat — when, in which [`CheckerPhase`], under which generation — at the top
//! of every cycle, around the lock wait and the check, and in the settings-miss
//! branch; [`classify`] judges it against the phase's own budget; and the window's
//! watchdog (`aterm-gui`'s `update_checker_watch`) turns a stale verdict into a log
//! line, the update-health warning, a `checker_stalled=` token and a replacement
//! thread under a new generation. The superseded thread, if it ever wakes, reads its
//! generation as stale and exits — so a respawn can never leave two live checkers.
//!
//! # Why a monotonic clock, not unix seconds
//!
//! A Mac that slept for eight hours has a checker that did nothing for eight hours
//! of WALL time and is perfectly healthy. `std::time::Instant` on macOS is
//! `CLOCK_UPTIME_RAW` (and `CLOCK_MONOTONIC` on Linux), neither of which advances
//! while the machine is asleep, so every age here is RUNNING time: a lid that was
//! closed is never a stall, and the first watchdog look after a wake sees the same
//! age the loop's own wait saw.
//!
//! # Why one packed word
//!
//! The generation, the phase and the stamp share one `AtomicU64`, so a beat and a
//! respawn are one compare-and-swap apart: a superseded thread's late beat can never
//! overwrite the replacement's (`try_update` refuses it), and two watchdog looks at
//! the same stalled generation can never spawn two replacements
//! ([`CheckerWatch::supersede`] succeeds once). No mutex, so the main thread's look
//! costs a few atomic loads and can never wait on the checker thread.
//!
//! # A replacement cannot unstick a check
//!
//! A cycle holds this process's check lane (and the machine-wide `checker.lock`)
//! from its lock wait to the end of its check, so a generation that stalls THERE
//! keeps both, and the replacement the watchdog starts finds the lane busy on every
//! cycle. The replacement stamps, so its own heartbeat is fresh — and the stall
//! used to vanish from `aterm ctl update status` the moment it started, while no
//! check could run. The lane holder is therefore kept beside the heartbeat
//! ([`CheckerWatch::hold_lane`]): when the watchdog retires a generation that holds
//! it, that generation's last stamp is kept there, and [`standing_stall`] names it
//! for as long as it holds the lane ([`CheckerBeat::blocked_by`]).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Where the check loop was when it last stamped the heartbeat. The phase decides
/// the budget a stamp is judged against ([`phase_budget_secs`]), and it is what
/// `checker_stalled=<secs>:<phase>` names — "stalled while waiting for the checker
/// lock" and "stalled inside a check" are different bugs with different fixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckerPhase {
    /// Registered, before the loop's first stamp — and the stamp a respawn resets
    /// to, so a replacement that never started is itself judged.
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
    /// Inside the network check and stage. Re-stamped by every download progress
    /// report, so a long download on a slow link is alive, and only a check that
    /// stops MOVING goes stale.
    Checking,
    /// The cadence wait between cycles (jittered, backed off, wake-aware).
    Waiting,
}

impl CheckerPhase {
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

    /// The phase's wire word — what `aterm ctl update status` and the log print.
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
}

/// How long one cycle waits for the machine-wide `checker.lock` before recording a
/// deferral and waiting for the next cycle instead.
///
/// Twice the check's own network timeout (the API and HEAD requests are bounded by
/// curl's `--max-time 30`): a sibling's routine check ends well inside it, so the
/// wait still turns into the ordinary "a sibling just checked" skip. A holder past it
/// is downloading — this process could only duplicate that — or it is stopped or
/// hung, which is exactly the holder that used to park this thread forever.
pub const CHECKER_LOCK_WAIT: Duration = Duration::from_secs(60);

/// The longest a check may run between two heartbeats: the steps inside a check
/// that are not a download, each bounded on its own — the API and HEAD requests
/// (30 s each, retried), the stage and publish lock waits (2 min each), the
/// container unpack (5 min), the codesign/Gatekeeper helpers (30 s each). A
/// download re-stamps on every progress report, so it never spends this budget
/// however long it takes on a slow link.
pub const CHECK_PHASE_BUDGET: Duration = Duration::from_secs(30 * 60);

/// Slack on top of twice the budget — the scheduling noise of a loaded machine, the
/// watchdog's own wake granularity — so a stamp that is merely late is never a stall.
pub const STALL_SLACK: Duration = Duration::from_secs(5 * 60);

/// How long a stale verdict must STAND before the watchdog acts on it: the first
/// stale look arms a second one this much later, and only a stamp that is still
/// the same stale stamp then is a stall.
///
/// Why a second look: the clock is `CLOCK_UPTIME_RAW`, which leaves out the
/// MACHINE's sleep but keeps running while this PROCESS is stopped — a `SIGSTOP`, a
/// debugger pause. The first look after the process is continued runs within
/// milliseconds (the watchdog's own wake is long past) and sees every stamp as old
/// as the stop, while the checker, stopped with it, has had no chance to stamp. A
/// continued checker stamps again within [`CHECKER_LOCK_WAIT`] of its lock wait, the
/// post-wake settle plus one wait slice of its cadence wait, and the end of the
/// current bounded step of a check, so one [`STALL_SLACK`] later a healthy loop has
/// always moved, and a stuck one has not.
pub const STALL_CONFIRM: Duration = STALL_SLACK;

/// How many replacement threads one process may start. Each replacement leaves the
/// stalled thread behind (it cannot be killed, only told to exit when it wakes), so
/// the count is bounded: a stall that survives three replacements is not something a
/// fourth will fix, and the warning and the status token have already said so.
pub const MAX_RESPAWNS: u64 = 3;

/// The process-wide monotonic anchor every stamp is measured from. See the module
/// docs for why this is `Instant` (sleep-excluded running time), not the wall clock.
fn anchor() -> Instant {
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    *ANCHOR.get_or_init(Instant::now)
}

/// Running seconds since this process's anchor — the clock every stamp and every
/// judgement shares.
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

const fn pack(generation: u64, phase: CheckerPhase, secs: u64) -> u64 {
    (generation << GEN_SHIFT) | (phase.tag() << PHASE_SHIFT) | (secs & SECS_MASK)
}

const fn generation_of(word: u64) -> u64 {
    word >> GEN_SHIFT
}

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
    /// The loop's longest legitimate wait between two stamps while it waits
    /// between cycles: its one cadence's backoff ceiling plus the jitter and the
    /// post-wake settle (`Cadence::max_wait`, which is `cadence_bounds::LONGEST_WAIT`
    /// for the shipping schedule). Registered with the loop and restated by it every
    /// cycle, from the schedule itself.
    pub max_interval_secs: u64,
    /// Replacement threads this process has started.
    pub respawns: u64,
    /// Consecutive cycles that found `checker.lock` held past
    /// [`CHECKER_LOCK_WAIT`] and deferred — 0 while cycles are getting it.
    pub deferrals: u64,
    /// Consecutive times the host did not answer the settings query.
    pub settings_misses: u64,
    /// The loop generation holding this process's check lane, with the phase and
    /// stamp it holds it under: while the live generation holds it, when it took
    /// it; once the watchdog has retired the holder, its LAST stamp. `None` while
    /// no loop generation holds the lane (a manual check may).
    pub lane: Option<LaneHolder>,
    /// Checks the loop has completed in this process (any generation, any
    /// result, a sibling-dedup skip included) — how the window's watchdog knows
    /// a replacement is checking again.
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
    #[must_use]
    pub fn blocked_by(&self) -> Option<LaneHolder> {
        self.lane
            .filter(|holder| holder.generation != self.generation)
    }

    /// Whether the live generation itself holds the check lane — true of a
    /// generation that stalls in its lock wait or its check.
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
        /// The phase it stamped last — where it is stuck.
        phase: CheckerPhase,
    },
}

/// The budget one stamp in `phase` is judged against: how long the loop may
/// legitimately go before its next stamp from there.
#[must_use]
pub fn phase_budget_secs(phase: CheckerPhase, max_interval_secs: u64) -> u64 {
    match phase {
        CheckerPhase::LockWait => CHECKER_LOCK_WAIT.as_secs(),
        CheckerPhase::Checking => CHECK_PHASE_BUDGET.as_secs(),
        // A handoff successor holds its first cycle until the parent commits (the
        // hold's own bound, a floor under whatever interval is published), and the
        // loop's longest wait covers every other phase.
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
#[must_use]
pub fn stall_threshold_secs(phase: CheckerPhase, max_interval_secs: u64) -> u64 {
    phase_budget_secs(phase, max_interval_secs)
        .saturating_mul(2)
        .saturating_add(STALL_SLACK.as_secs())
}

/// Judge `beat` at `now_secs` (pure: the watchdog, the status line and the tests
/// all ask this one question). A stamp from the future — impossible on one
/// monotonic anchor, but never worth a panic — is fresh.
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

/// The stall `aterm ctl update status` names: the live generation's own when its
/// stamp is stale ([`classify`]), else — while it is stuck behind a retired
/// generation that still holds the check lane ([`CheckerBeat::blocked_by`]) — that
/// generation's, which no replacement can clear: `(running seconds since its last
/// stamp, the phase it stamped)`. `None` while checks can run.
#[must_use]
pub fn standing_stall(beat: &CheckerBeat, now_secs: u64) -> Option<(u64, CheckerPhase)> {
    if let CheckerVerdict::Stalled { for_secs, phase } = classify(beat, now_secs) {
        return Some((for_secs, phase));
    }
    beat.blocked_by()
        .map(|holder| (now_secs.saturating_sub(holder.at_secs), holder.phase))
}

/// The heartbeat itself. The process's checker stamps the static [`WATCH`];
/// tests build their own so a generation they supersede is never the process's.
pub struct CheckerWatch {
    /// `generation << 48 | phase << 40 | running secs` — see the module docs.
    beat: AtomicU64,
    /// The check lane's loop holder, packed the same way; `0` while none holds
    /// it ([`Self::hold_lane`], [`CheckerBeat::lane`]).
    lane: AtomicU64,
    max_interval: AtomicU64,
    respawns: AtomicU64,
    deferrals: AtomicU64,
    settings_misses: AtomicU64,
    checks: AtomicU64,
    /// Optional host event-loop wake. Phase changes are rare; progress stamps
    /// never call it, so an idle window can re-fold the phase's deadline without
    /// polling or waking for each download progress report.
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

    /// Ask the host to look at a changed phase before its old deadline expires.
    /// The GUI installs this before starting the checker; a terminal-only session
    /// has no event loop to wake and leaves it unset. The first registration wins.
    pub fn set_phase_wake(&self, wake: Box<dyn Fn() + Send + Sync>) {
        let _ = self.phase_wake.set(wake);
    }

    /// Register the first checker thread of this process, stamped `Starting` now,
    /// and return its generation. A second registration (a second spawn call in
    /// one process) takes the next generation, which retires the first thread at
    /// its next look — one live checker per process, whoever spawned it.
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

    /// Stamp `phase` for `generation`, now. `false` — and nothing written — when
    /// that generation has been superseded: the caller is a stalled thread that
    /// woke, and must exit.
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

    /// A download progress report: re-stamp the live generation, but only while it
    /// is inside a check. Only one check runs per process at a time (the check lane),
    /// so a report while the live thread is `Checking` is its own download; a report
    /// in any other phase is a manual check's, which says nothing about this thread.
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

    /// Publish the loop's longest legitimate wait between stamps. The loop restates it
    /// every cycle from its schedule (`Cadence::max_wait`), so a ceiling that moves
    /// can never leave the watchdog judging a wait it no longer knows the length of.
    /// Ignored from a superseded thread.
    pub fn set_max_interval(&self, generation: u64, secs: u64) {
        if self.is_current(generation) {
            self.max_interval.store(secs, Ordering::Relaxed);
        }
    }

    /// Publish the consecutive `checker.lock` deferral count.
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

    /// A cycle of `generation` has taken this process's check lane: record it as
    /// the lane's holder until the returned mark is dropped, which the cycle does
    /// exactly where it lets go of the lane.
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

    /// A check completed (whatever it found) or was skipped because a sibling had
    /// just completed one: the loop is checking. Counted for the live generation
    /// only.
    pub fn note_check(&self, generation: u64) {
        if self.is_current(generation) {
            self.checks.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The heartbeat as it stands, or `None` when no checker was ever registered
    /// in this process (automatic checks off, an uninstalled copy, a headless run).
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

    /// Retire `stalled` and hand out the generation its replacement runs under,
    /// stamped `Starting` now. `None` when `stalled` is no longer the live
    /// generation (another look already replaced it — so a replacement is spawned
    /// at most once per stall) or when [`MAX_RESPAWNS`] is spent.
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
        // A retired generation that holds the check lane keeps it until it wakes —
        // no replacement can take it — so its last stamp stays on record beside the
        // lane, where the status line reads the stall that is still standing.
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
}

/// The check lane marked held by one generation ([`CheckerWatch::hold_lane`]);
/// dropping it clears the mark — unless another generation has marked the lane
/// since, which is then its own.
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

/// This process's heartbeat: the one the shipping loop stamps and the window reads.
pub static WATCH: CheckerWatch = CheckerWatch::new();

/// The process heartbeat as it stands (see [`CheckerWatch::snapshot`]).
#[must_use]
pub fn checker_snapshot() -> Option<CheckerBeat> {
    WATCH.snapshot()
}

/// A consecutive-repeat counter that decides which repeats deserve a log line: the
/// first of a streak, then every `every`-th, then the streak's end. The settings
/// misses and the `checker.lock` deferrals both used to say either nothing at all
/// (the settings branch slept five seconds and looped, forever, with no line) or
/// would say the same line every cycle — neither is a log.
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

    /// The loop's longest wait, as it registers it: its one cadence (a check every
    /// `INTERVAL_SECS`, backed off to a 40 min ceiling), +20 % jitter, +20 s of
    /// post-wake settle — `cadence_bounds::LONGEST_WAIT`, 48 min 20 s.
    const LONGEST: u64 = crate::cadence_bounds::LONGEST_WAIT.as_secs();

    /// The number every judgement of a waiting loop below rests on is the one the
    /// shipping loop registers and restates (`spawn_background_check_with_settings`,
    /// `run_checker`): its schedule's `max_wait`, which is `LONGEST_WAIT`.
    #[test]
    fn the_loop_registers_its_one_cadences_longest_wait() {
        assert_eq!(
            crate::cadence::Cadence::new(Duration::from_secs(crate::cadence::INTERVAL_SECS))
                .max_wait()
                .as_secs(),
            LONGEST
        );
        assert_eq!(LONGEST, 48 * 60 + 20, "a 40 min ceiling, +20 %, +20 s");
    }

    /// THE CLASSIFIER, phase by phase. Each phase is judged against its OWN
    /// budget: a waiting loop may be quiet for twice the loop's longest wait,
    /// a lock wait only for twice its bound, and a check for twice the longest
    /// non-download step — so a checker parked on a lock is called out in
    /// minutes, not after the four hours `stale_check=` used to need.
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
        // A waiting loop is a stall only past twice the longest wait plus slack:
        // 1 h 41 min 40 s of running time.
        assert_eq!(
            stall_threshold_secs(CheckerPhase::Waiting, LONGEST),
            2 * (48 * 60 + 20) + 5 * 60
        );
        // The 4.6-day silence, as the watchdog would have seen it from its first
        // look: a thread parked in the lock wait is a stall within minutes.
        let parked = beat(CheckerPhase::LockWait, 0, LONGEST);
        assert!(matches!(
            classify(&parked, 10 * 60),
            CheckerVerdict::Stalled {
                phase: CheckerPhase::LockWait,
                ..
            }
        ));
        // A handoff successor's first cycle may hold for the whole uncommitted-
        // candidate bound before it stamps again. The one cadence already waits
        // longer than that, so the loop's longest wait is the Starting budget; the
        // bound is a floor under it whatever interval is published.
        let hold = crate::UNCOMMITTED_CANDIDATE_HOLD_BOUND.as_secs();
        assert!(hold <= LONGEST, "the cadence's wait covers the hold");
        assert_eq!(phase_budget_secs(CheckerPhase::Starting, LONGEST), LONGEST);
        assert_eq!(phase_budget_secs(CheckerPhase::Starting, 0), hold);
        let starting = beat(CheckerPhase::Starting, 0, LONGEST);
        assert!(matches!(
            classify(&starting, 2 * hold),
            CheckerVerdict::Fresh { .. }
        ));
        // A stamp "from the future" is never a panic and never a stall.
        assert!(matches!(
            classify(&beat(CheckerPhase::Waiting, 50, LONGEST), 10),
            CheckerVerdict::Fresh { .. }
        ));
    }

    /// A window that went idle during Settings once slept until that phase's
    /// hour-away deadline even after the checker entered its one-minute lock
    /// wait. The host must get a wake on the changed phase and can then fold
    /// the shorter deadline; same-phase stamps and 10 Hz download progress
    /// must not wake the idle window.
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
            new_deadline < old_deadline / 10,
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

    /// A STALE GENERATION CANNOT WRITE. The replacement's stamp is never
    /// overwritten by the thread it replaced, a superseded thread learns it is
    /// superseded from its own beat, and one stall is replaced once.
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
    }

    /// A STALL INSIDE THE LANE OUTLIVES ITS REPLACEMENT. The generation that
    /// stalls in its check holds the check lane, and the replacement the watchdog
    /// starts can never take it: it stamps every cycle, so its own heartbeat is
    /// fresh, and before this the stall left `aterm ctl update status` the moment
    /// the replacement started (the `standing_stall` assertion below fails against
    /// that: nothing kept the retired holder). The lane now keeps the holder's last
    /// stamp until it lets go.
    #[test]
    fn a_retired_generation_holding_the_lane_keeps_its_stall_standing_until_it_lets_go() {
        let watch = CheckerWatch::new();
        // A longest wait far past the instants read below, so the replacement's
        // own stamp stays fresh throughout.
        let first = watch.register(10_000);
        let mark = watch.hold_lane(first);
        assert!(watch.beat(first, CheckerPhase::Checking));
        let stalled_at = watch.snapshot().unwrap().at_secs;
        assert!(watch.snapshot().unwrap().holds_lane(), "the live holder");
        assert_eq!(watch.snapshot().unwrap().blocked_by(), None);

        let second = watch.supersede(first).expect("replaced");
        // The replacement stamps and is healthy by its own heartbeat…
        assert!(watch.beat(second, CheckerPhase::Waiting));
        let live = watch.snapshot().unwrap();
        assert!(matches!(
            classify(&live, live.at_secs),
            CheckerVerdict::Fresh { .. }
        ));
        assert!(!live.holds_lane());
        // …but it is blocked behind the retired holder, whose stall stands, named
        // by the phase and stamp it stalled at.
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
        // The replacement's own mark (a later cycle of it taking the lane) is its
        // own: the retired holder letting go must not clear it, and vice versa.
        drop(mark);
        let live = watch.snapshot().unwrap();
        assert_eq!(live.lane, None, "the stuck check let go");
        assert_eq!(standing_stall(&live, live.at_secs + 60), None);
        let own = watch.hold_lane(second);
        assert!(watch.snapshot().unwrap().holds_lane());
        drop(own);
        assert_eq!(watch.snapshot().unwrap().lane, None);
    }

    /// A retirement that finds the lane free, or held by nobody it retires,
    /// leaves the lane alone; only the live generation counts checks.
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

    /// A download keeps a check alive; nothing else a download reports does.
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

    /// The streak log: the first repeat, every `every`-th, and the end.
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

    /// The phase tag survives the packed word for every phase.
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
