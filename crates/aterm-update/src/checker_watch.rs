// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The update checker's heartbeat.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Where the check loop last stamped the heartbeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckerPhase {
    /// Registered, before the loop's first stamp.
    Starting,
    /// The OS refused to create this generation's thread; no worker can stamp it.
    StartFailed,
    /// The active worker returned or unwound; no worker remains for this generation.
    Stopped,
    /// Asking the host for the current channel and policy, or pausing after it did not answer.
    Settings,
    /// Reading the bundle under this executable and verifying its signature when newer.
    BundleProbe,
    /// Waiting for the machine-wide `checker.lock` (bounded by [`CHECKER_LOCK_WAIT`]).
    LockWait,
    /// Inside the network check and stage.
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
            Self::StartFailed => 7,
            Self::Stopped => 8,
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
            7 => Self::StartFailed,
            8 => Self::Stopped,
            _ => Self::Starting,
        }
    }

    /// The phase's wire word, which `aterm ctl update status` prints.
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
            Self::StartFailed => "start-failed",
            Self::Stopped => "stopped",
        }
    }

    /// The phase in a person's words, for log lines.
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
            Self::StartFailed => "waiting to retry a failed thread start",
            Self::Stopped => "waiting to replace an exited checker",
        }
    }

    /// Whether the host has positive evidence that this generation has no worker.
    #[must_use]
    pub const fn worker_absent(self) -> bool {
        matches!(self, Self::StartFailed | Self::Stopped)
    }
}

/// How long one cycle waits for the machine-wide `checker.lock` before recording a deferral.
pub const CHECKER_LOCK_WAIT: Duration = Duration::from_secs(60);

/// Consecutive stalled `checker.lock` deferrals past [`CHECKER_LOCK_WAIT`] before checking ungated.
pub const CHECKER_UNGATED_AFTER: u64 = 3;

/// The longest a check may run between two heartbeats: its non-download steps, each bounded.
pub const CHECK_PHASE_BUDGET: Duration = Duration::from_secs(30 * 60);

/// Slack on top of twice the budget, so a merely late stamp is never a stall.
pub const STALL_SLACK: Duration = Duration::from_secs(5 * 60);

/// How long a stale verdict must stand before the watchdog acts.
pub const STALL_CONFIRM: Duration = STALL_SLACK;

/// Maximum extra checker workers that may remain outstanding beside one worker.
pub const MAX_RESPAWNS: u64 = 3;

/// Backoff after a refused thread start or an observed worker exit.
pub const START_RETRY: Duration = Duration::from_secs(5);

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
    let base = anchor();
    base.checked_add(Duration::from_secs(secs))
        .unwrap_or_else(|| base + Duration::from_secs(u64::from(u32::MAX)))
}

const GEN_SHIFT: u32 = 48;
const PHASE_SHIFT: u32 = 40;
const SECS_MASK: u64 = (1 << PHASE_SHIFT) - 1;
const PHASE_MASK: u64 = 0xff;
const COUNT_MASK: u64 = (1 << GEN_SHIFT) - 1;
#[cfg(any(target_os = "macos", test))]
const DEADLINE_SECS_MASK: u64 = (1 << GEN_SHIFT) - 1;

/// Keep a delayed retired writer from replacing a newer generation's count.
fn publish_count(count: &AtomicU64, generation: u64, value: u64) {
    if generation == 0 {
        return;
    }
    let _ = count.try_update(Ordering::AcqRel, Ordering::Acquire, |previous| {
        (generation_of(previous) <= generation)
            .then_some((generation << GEN_SHIFT) | value.min(COUNT_MASK))
    });
}

fn read_count(count: &AtomicU64, generation: u64) -> u64 {
    let word = count.load(Ordering::Acquire);
    if generation_of(word) == generation {
        word & COUNT_MASK
    } else {
        0
    }
}

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
    /// The loop's longest legitimate wait between two stamps, restated every cycle.
    pub max_interval_secs: u64,
    /// Replacement threads this process has started.
    pub respawns: u64,
    /// Started or reserved workers that have not exited or failed to start.
    pub workers: usize,
    /// Changes when a worker slot is returned, allowing a capacity refusal to retry.
    pub worker_epoch: u64,
    /// Consecutive cycles deferred to an unresponsive `checker.lock` holder (`0` while active).
    pub deferrals: u64,
    /// Consecutive times the host did not answer the settings query.
    pub settings_misses: u64,
    /// The loop generation holding this process's check lane, with its phase and stamp.
    pub lane: Option<LaneHolder>,
    /// Completed check cycles in this process across all generations.
    pub checks: u64,
}

/// Who holds the check lane for the loop, and from when ([`CheckerBeat::lane`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneHolder {
    /// The holding generation.
    pub generation: u64,
    /// [`CheckerPhase::LockWait`] while active, or the stalled phase once retired.
    pub phase: CheckerPhase,
    /// [`now_secs`] running seconds: when it took the lane, or its last stamp.
    pub at_secs: u64,
}

impl CheckerBeat {
    /// Retired generation still holding this process's check lane, if any.
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
    /// Stamped recently enough for its phase.
    Fresh {
        /// Running seconds until the stamp crosses its threshold.
        stale_in_secs: u64,
    },
    /// Unstamped for `for_secs` running seconds, past its phase's stall or failed-start threshold.
    Stalled {
        /// Running seconds since the last stamp.
        for_secs: u64,
        /// The phase it stamped last: where it is stuck.
        phase: CheckerPhase,
    },
}

/// The budget one stamp in `phase` is judged against.
#[inline]
#[must_use]
pub fn phase_budget_secs(phase: CheckerPhase, max_interval_secs: u64) -> u64 {
    match phase {
        CheckerPhase::StartFailed | CheckerPhase::Stopped => START_RETRY.as_secs(),
        CheckerPhase::LockWait => CHECKER_LOCK_WAIT.as_secs(),
        CheckerPhase::Checking => CHECK_PHASE_BUDGET.as_secs(),
        CheckerPhase::Starting => {
            max_interval_secs.max(crate::UNCOMMITTED_CANDIDATE_HOLD_BOUND.as_secs())
        }
        CheckerPhase::Settings | CheckerPhase::BundleProbe | CheckerPhase::Waiting => {
            max_interval_secs
        }
    }
}

/// The running-second age past which a stamp in `phase` is a stall. A known
/// failed start or observed exit uses only its retry backoff, without late-heartbeat slack.
#[inline]
#[must_use]
pub fn stall_threshold_secs(phase: CheckerPhase, max_interval_secs: u64) -> u64 {
    if phase.worker_absent() {
        return START_RETRY.as_secs();
    }
    phase_budget_secs(phase, max_interval_secs)
        .saturating_mul(2)
        .saturating_add(STALL_SLACK.as_secs())
}

/// Judge `beat` at `now_secs`.
#[inline]
#[must_use]
pub fn classify(beat: &CheckerBeat, now_secs: u64) -> CheckerVerdict {
    let age = now_secs.saturating_sub(beat.at_secs);
    let threshold = stall_threshold_secs(beat.phase, beat.max_interval_secs);
    if age > threshold || (beat.phase.worker_absent() && age == threshold) {
        CheckerVerdict::Stalled {
            for_secs: age,
            phase: beat.phase,
        }
    } else {
        CheckerVerdict::Fresh {
            stale_in_secs: (threshold - age).saturating_add(u64::from(!beat.phase.worker_absent())),
        }
    }
}

/// The stall `aterm ctl update status` names, as (running seconds since the last stamp, phase).
#[must_use]
pub fn standing_stall(beat: &CheckerBeat, now_secs: u64) -> Option<(u64, CheckerPhase)> {
    if let CheckerVerdict::Stalled { for_secs, phase } = classify(beat, now_secs) {
        return Some((for_secs, phase));
    }
    beat.blocked_by()
        .map(|holder| (now_secs.saturating_sub(holder.at_secs), holder.phase))
}

/// Atomic heartbeat and serialized worker reservations for the update checker.
pub struct CheckerWatch {
    /// `generation << 48 | phase << 40 | running secs`.
    beat: AtomicU64,
    /// Packed check-lane holder (`0` when unheld; see [`Self::hold_lane`]).
    lane: AtomicU64,
    /// Registered cadence ceiling, inherited unchanged by every replacement.
    max_interval: AtomicU64,
    respawns: AtomicU64,
    workers: Mutex<Workers>,
    /// Generation-prefixed count, saturated to [`COUNT_MASK`].
    deferrals: AtomicU64,
    /// Generation-prefixed count, saturated to [`COUNT_MASK`].
    settings_misses: AtomicU64,
    checks: AtomicU64,
    /// Generation in the high bits, positive Unix seconds in the low bits.
    #[cfg(any(target_os = "macos", test))]
    next_check: AtomicU64,
    /// Optional host event-loop wake invoked on phase transitions.
    phase_wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

struct Workers {
    generations: BTreeMap<u64, Worker>,
    epoch: u64,
}

struct Worker {
    started: bool,
    replacement: bool,
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
            workers: Mutex::new(Workers {
                generations: BTreeMap::new(),
                epoch: 0,
            }),
            deferrals: AtomicU64::new(0),
            settings_misses: AtomicU64::new(0),
            checks: AtomicU64::new(0),
            #[cfg(any(target_os = "macos", test))]
            next_check: AtomicU64::new(0),
            phase_wake: OnceLock::new(),
        }
    }

    /// Wake the host on a phase change, before the old deadline expires.
    pub fn set_phase_wake(&self, wake: Box<dyn Fn() + Send + Sync>) {
        let _ = self.phase_wake.set(wake);
    }

    /// Register a checker thread, stamped `Starting` now, and return its generation.
    pub fn register(&self, max_interval_secs: u64) -> u64 {
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        workers.generations.insert(
            next,
            Worker {
                started: false,
                replacement: false,
            },
        );
        next
    }

    /// Retain a reserved slot until this thread exits, including during unwinding.
    #[cfg(any(target_os = "macos", target_os = "linux", test))]
    pub(crate) fn worker(&self, generation: u64) -> Option<WorkerMark<'_>> {
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let worker = workers.generations.get_mut(&generation)?;
        if worker.started {
            return None;
        }
        worker.started = true;
        Some(WorkerMark {
            watch: self,
            generation,
        })
    }

    /// Stamp `phase` for `generation`, now.
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

    /// Re-stamp progress only when its originating generation is still checking.
    pub fn beat_progress(&self, generation: u64) -> bool {
        let now = now_secs();
        self.beat
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                let phase = phase_of(word);
                (generation != 0
                    && generation_of(word) == generation
                    && phase == CheckerPhase::Checking)
                    .then(|| pack(generation, phase, now))
            })
            .is_ok()
    }

    /// Whether `generation` is still the live checker.
    #[must_use]
    pub fn is_current(&self, generation: u64) -> bool {
        generation_of(self.beat.load(Ordering::Acquire)) == generation
    }

    /// Publish a live generation's wait. Its marker alone may clear this value.
    /// Unrepresentable timestamps and a second wait of the same generation are refused.
    #[must_use]
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn publish_next_check(
        &self,
        generation: u64,
        unix_secs: u64,
    ) -> Option<NextCheckMark<'_>> {
        if generation == 0
            || !self.is_current(generation)
            || unix_secs == 0
            || unix_secs > DEADLINE_SECS_MASK
        {
            return None;
        }
        let word = (generation << GEN_SHIFT) | unix_secs;
        self.next_check
            .try_update(Ordering::AcqRel, Ordering::Acquire, |previous| {
                (previous == 0 || generation_of(previous) < generation).then_some(word)
            })
            .ok()?;
        Some(NextCheckMark { watch: self, word })
    }

    /// The current generation's published deadline, never a retired wait's.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn next_check_unix(&self) -> Option<u64> {
        let word = self.next_check.load(Ordering::Acquire);
        let generation = generation_of(word);
        (generation != 0 && self.is_current(generation)).then_some(word & DEADLINE_SECS_MASK)
    }

    /// Publish the consecutive count of `checker.lock` deferrals to a holder showing no progress.
    pub fn set_deferrals(&self, generation: u64, n: u64) {
        if self.is_current(generation) {
            publish_count(&self.deferrals, generation, n);
        }
    }

    /// Publish the consecutive settings-miss count.
    pub fn set_settings_misses(&self, generation: u64, n: u64) {
        if self.is_current(generation) {
            publish_count(&self.settings_misses, generation, n);
        }
    }

    /// Marks `generation` as holding the check lane until the returned [`LaneMark`] drops.
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
    pub fn note_check(&self, generation: u64) {
        if self.is_current(generation) {
            self.checks.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The heartbeat as it stands; `None` when no checker was ever registered here.
    #[must_use]
    pub fn snapshot(&self) -> Option<CheckerBeat> {
        let workers = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let word = self.beat.load(Ordering::Acquire);
        let generation = generation_of(word);
        let lane = self.lane.load(Ordering::Acquire);
        (generation != 0).then(|| CheckerBeat {
            generation,
            phase: phase_of(word),
            at_secs: word & SECS_MASK,
            max_interval_secs: self.max_interval.load(Ordering::Relaxed),
            respawns: self.respawns.load(Ordering::Relaxed),
            workers: workers.generations.len(),
            worker_epoch: workers.epoch,
            deferrals: read_count(&self.deferrals, generation),
            settings_misses: read_count(&self.settings_misses, generation),
            lane: (generation_of(lane) != 0).then(|| LaneHolder {
                generation: generation_of(lane),
                phase: phase_of(lane),
                at_secs: lane & SECS_MASK,
            }),
            checks: self.checks.load(Ordering::Acquire),
        })
    }

    /// Retire `stalled` and return its replacement's generation, stamped `Starting` now.
    pub fn supersede(&self, stalled: u64) -> Option<u64> {
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if stalled == 0
            || stalled >= u64::from(u16::MAX)
            || workers.generations.len() as u64 > MAX_RESPAWNS
        {
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
        let _ = self
            .lane
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                (generation_of(word) == stalled).then_some(last)
            });
        self.respawns.fetch_add(1, Ordering::AcqRel);
        workers.generations.insert(
            next,
            Worker {
                started: false,
                replacement: true,
            },
        );
        Some(next)
    }

    /// No thread started for this generation. Retain retirement of its predecessor,
    /// return any replacement slot, and wake the host for a bounded retry.
    pub fn unstarted(&self, generation: u64) {
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let word = self.beat.load(Ordering::Acquire);
        if generation_of(word) == generation && phase_of(word) != CheckerPhase::Starting {
            return;
        }
        let Some(worker) = workers.generations.get(&generation) else {
            return;
        };
        if worker.started {
            return;
        }
        let replacement = worker.replacement;
        workers.generations.remove(&generation);
        workers.epoch += 1;
        let now = now_secs();
        let changed = self
            .beat
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                (generation_of(word) == generation && phase_of(word) == CheckerPhase::Starting)
                    .then(|| pack(generation, CheckerPhase::StartFailed, now))
            })
            .is_ok();
        if replacement {
            let _ = self
                .respawns
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
        }
        drop(workers);
        if (changed || !self.is_current(generation))
            && let Some(wake) = self.phase_wake.get()
        {
            wake();
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(crate) struct WorkerMark<'a> {
    watch: &'a CheckerWatch,
    generation: u64,
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
impl Drop for WorkerMark<'_> {
    fn drop(&mut self) {
        let mut workers = self
            .watch
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let removed = workers.generations.remove(&self.generation).is_some();
        if removed {
            workers.epoch += 1;
            let _ = self
                .watch
                .beat
                .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                    (generation_of(word) == self.generation)
                        .then(|| pack(self.generation, CheckerPhase::Stopped, now_secs()))
                });
        }
        drop(workers);
        if removed && let Some(wake) = self.watch.phase_wake.get() {
            wake();
        }
    }
}

/// Ownership of one published wait deadline.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct NextCheckMark<'a> {
    watch: &'a CheckerWatch,
    word: u64,
}

#[cfg(any(target_os = "macos", test))]
impl Drop for NextCheckMark<'_> {
    fn drop(&mut self) {
        let _ = self.watch.next_check.compare_exchange(
            self.word,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// The check lane marked held by one generation ([`CheckerWatch::hold_lane`]).
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

/// A consecutive-repeat counter deciding which repeats get a log line.
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
    /// The streak reached another multiple of `every`: say it is still going, with this count.
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
    pub const fn note(&mut self) -> Streak {
        self.consecutive = self.consecutive.saturating_add(1);
        if self.consecutive == 1 {
            Streak::Began
        } else if self.consecutive.is_multiple_of(self.every) {
            Streak::Continues(self.consecutive)
        } else {
            Streak::Quiet
        }
    }

    /// The streak ended; `Some` when there was one to end, `None` when nothing was running.
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
#[path = "checker_watch_tests.rs"]
mod tests;
