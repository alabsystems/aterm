// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one probe thread that reads the machine for the stats Deck, and the loop's side of it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::harness::profile::{AccountKey, Bucket, PlanUsage};
use aterm_sysprobe::jobs::RunState;
use aterm_sysprobe::volumes::VolumeKey;
use aterm_vitals::Peaks;
use aterm_vitals::explain::{
    CoalLane, CoalSample, CoalUsage, IdMap, IdSet, LeadIdentity, TimelineSample,
};
use aterm_vitals::history::{
    FreeHistory, FreeSeries, FreeWeeks, SavedFree, SavedSpotlight, SpotlightHistory, VolumeId,
};
use aterm_vitals::model::{
    AccountIx, AneCheck, AneSample, ClusterSpec, CommitSample, DeedBy, DeedKind, DiskDeed,
    DiskFacts, DiskLevel, GpuSample, Hog, LinkKind, LinkSample, MemCounters, MemSample, PlanFact,
    Pressure, ProcSample, ProcState, Sample, SavedStrip, Snapshot, TabRoot, TabTokens, TaskRoom,
    Thermal, Throttle, Ticks, VolumeSample,
};
use aterm_vitals::sys::Machine;

use crate::disk_board::{Clock, DiskFactsRaw, Shelf};
use crate::disk_where::Where;
use crate::occupancy_host::Arming;
use crate::sys_host::PaneFact;
use strip::StripStore;

#[path = "vitals_assessment.rs"]
mod assessment;

#[path = "vitals_boot_capture.rs"]
pub(crate) mod boot_capture;

#[path = "vitals_boot_cost.rs"]
pub(crate) mod boot_cost;

#[path = "vitals_ownership.rs"]
mod ownership;

#[path = "vitals_self_cpu.rs"]
mod self_cpu;

/// A reading a second while the bars are in view, aterm has a focused window, and either the
/// focused window or another window on screen neither rests nor has gone unattended
/// ([`attends_readings`]) or a Tab Control or Allocation page is on screen
/// ([`Audience::Shown`]; an open stats lens reads on its own cadence either way).
pub(crate) const FOCUSED_PERIOD: Duration = Duration::from_secs(1);
/// One every five seconds while the bars are in view and aterm is in the background, or the
/// focused window and every other window on screen rest or have gone untouched for
/// [`crate::window_rest::ATTENTION`], with no such page on screen ([`Audience::Background`]).
pub(crate) const BACKGROUND_PERIOD: Duration = Duration::from_secs(5);
/// The machine counters every ten seconds while every window that draws the bars is hidden.
pub(crate) const HIDDEN_PERIOD: Duration = Duration::from_secs(10);
/// How long the probe thread waits for an ask before it reads the machine's counters on its own,
/// while its newest reading found memory tight or a starvation episode open
/// ([`aterm_vitals::starving::watching`]): twice [`HIDDEN_PERIOD`], so a host that paces
/// readings at a hidden deck's cadence or faster is never doubled, while one whose main thread
/// stops asking (a starving machine stalls it first) or asks none (no window in view draws the
/// bars) leaves memory judged. A machine with room arms none, so it adds no wake at rest.
pub(crate) const MEMORY_WATCH_EVERY: Duration = Duration::from_secs(20);
const PEAKS_EVERY: Duration = Duration::from_secs(600);
const PEAKS_FILE: &str = "vitals-peaks.json";
const PEAKS_SCHEMA: u32 = 1;
const PEAKS_MAX_BYTES: u64 = 16 * 1024;
/// The disk's free history, kept beside [`PEAKS_FILE`] and written as it is; a full day on a mount
/// point of up to 1024 bytes is under [`FREE_MAX_BYTES`].
const FREE_FILE: &str = "vitals-free.json";
/// The bound [`FREE_FILE`] is read under: a day of the widest time-weighted buckets fits in 75 %
/// of it.
const FREE_MAX_BYTES: u64 = 32 * 1024;
/// Schema 2: each bucket is a time-weighted [`aterm_vitals::Agg`]. A schema-1 file (least free
/// and a count) restores nothing.
const FREE_SCHEMA: u32 = 2;
/// The disk's three-week free history ([`FreeWeeks`]), kept beside [`FREE_FILE`] in its form, in
/// a file of its own: a damaged one never costs the day's, and an older aterm ignores it. Its name
/// carries [`WEEKS_SCHEMA`], so an aterm of another schema sharing the state home keeps its own
/// file, and neither overwrites the other's three weeks.
const WEEKS_FILE: &str = "vitals-free-weeks-v1.json";
/// The bound [`WEEKS_FILE`] is read under: three weeks of the widest hour buckets, on a mount
/// point of up to 1024 bytes, fit in 75 % of it.
const WEEKS_MAX_BYTES: u64 = 128 * 1024;
/// Schema 1: [`FREE_FILE`]'s schema-2 form of hour buckets. A day's file restores nothing here,
/// and this file nothing there. A new schema names a new [`WEEKS_FILE`].
const WEEKS_SCHEMA: u32 = 1;
/// The Spotlight family's day ([`SpotlightHistory`]), kept beside [`FREE_FILE`] in a file of its
/// own, written as the day's free history is. Its name carries [`SPOTLIGHT_SCHEMA`].
const SPOTLIGHT_FILE: &str = "vitals-spotlight-v1.json";
/// The bound [`SPOTLIGHT_FILE`] is read under: a day of the widest buckets of its three figures
/// fits in 75 % of it.
const SPOTLIGHT_MAX_BYTES: u64 = 128 * 1024;
/// Schema 1: [`FREE_FILE`]'s bucket form, one list a figure (CPU, reads, footprint).
const SPOTLIGHT_SCHEMA: u32 = 1;
/// How long a run of `mdutil -s -a` may take before it is killed and the volumes count as
/// unread this time: it measured 0.01 s on m42, and it runs on the probe thread at most once a
/// minute ([`aterm_vitals::spotlight::Cadence`]).
#[cfg(target_os = "macos")]
const MDUTIL_WAIT: Duration = Duration::from_secs(2);
/// How long a killed `mdutil` is given to be reaped before it is left to the reaper: the probe
/// thread waits on a run at most [`MDUTIL_WAIT`] plus this.
#[cfg(target_os = "macos")]
const MDUTIL_REAP: Duration = Duration::from_millis(250);
/// How often the probe thread reads the disk journal's tail again once it changed, while a window
/// holds a Disk lens open or a window in view draws the bars (the Disk Custody band shows the
/// newest deed beside the freed-today figure, which is live): one read of a file whose length or
/// time moved.
const DEEDS_EVERY: Duration = Duration::from_secs(10);
/// How often it does while neither: the list is in the Snapshot when a lens opens.
const DEEDS_IDLE_EVERY: Duration = Duration::from_secs(300);
/// The most deeds the Disk lens is handed ([`DiskFacts::deeds`]).
const DEEDS_KEPT: usize = 8;
/// How far back the journal's walk reads, ms: the three-week chart's span.
const DEEDS_REACH_MS: i64 = 504 * 3_600_000;
/// How much of the journal's tail is read: all of it, since `disk` cuts it back past this.
const DEEDS_READ_BYTES: u64 = aterm_agent::harness::cli::DISK_LEDGER_MAX_BYTES;
/// How often the probe thread gathers `sys disk held`'s rows again while a window holds a Disk
/// lens open ([`HoldingsRead`]). The lens says how old its rows are once they are older than a
/// few of these.
pub(crate) const HELD_EVERY: Duration = Duration::from_secs(5);
/// How often it does while none does: the rows are in the Snapshot when a lens opens.
const HELD_IDLE_EVERY: Duration = Duration::from_secs(300);
/// The lock beside [`PEAKS_FILE`] whose holder is the one aterm of a state home that writes it and
/// the strip's checkpoint ([`StripLock`], [`strip::StripStore`]): the strip's base, rewritten at a
/// start's first reading, hourly and at the thread's stop (clean only at a quit,
/// [`VitalsHost::stopping`]), and its tail, a line per closed epoch, so restarts and crashes keep
/// the strip; a lock holder that restored an unclean checkpoint writes its base at its start too
/// ([`restore_strip`]).
const STRIP_LOCK: &str = "vitals-strip.lock";
/// Raw anomaly evidence is saved by the existing probe thread, separately from live state.
const CPU_REPORT_FILE: &str = "vitals-cpu-report.json";
/// The CPU report is written at most this often, and at once at a start.
const CPU_REPORT_EVERY: Duration = Duration::from_secs(10);
const CPU_REPORT_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// How long the quit waits for the probe thread to end and write the peaks and the free history.
pub(crate) const STOP_BOUND: Duration = Duration::from_millis(500);

/// A pull whose thread's last reading is older than this reads twice.
pub(crate) const COLD_AFTER: Duration = Duration::from_secs(3);
/// Spacing between a cold pull's two readings.
pub(crate) const COLD_PAIR: Duration = Duration::from_millis(500);
/// How often the CPU lens samples CPU Stats: every other lens reading (A2 §6.4).
pub(crate) const LENS_HARDWARE_PERIOD: Duration = Duration::from_secs(1);
/// The recorder keeps its frames at least this far apart (`aterm_vitals::hardware`), so its CPU
/// Stats period is never finer.
const RECORDER_HARDWARE_FLOOR: Duration = Duration::from_secs(1);

/// An edge's readings with no window showing the bars come at most this often.
pub(crate) const JOBS_FLOOR: Duration = Duration::from_secs(2);
/// The shortest rate window a pull measures.
pub(crate) const MIN_WINDOW: Duration = Duration::from_millis(250);
/// Minimum interval between pane-layout checks between readings.
pub(crate) const LAYOUT_EVERY: Duration = Duration::from_millis(250);
const JOB_EVENTS_CAP: usize = 256;
/// Health episodes' edges the hand-over keeps for the loop: each episode makes two, so a loop
/// that has not adopted for a while loses only the oldest past this many.
const HEALTH_EVENTS_CAP: usize = 16;
/// How often the bars refresh signature and timeline details. Every whole reading retains
/// current process/coalition counters; intervening readings reuse only version-checked details.
pub(crate) const SPLIT_EVERY: Duration = Duration::from_secs(5);
/// How often a lens refreshes signature and timeline details.
pub(crate) const SPLIT_EVERY_LENS: Duration = Duration::from_secs(1);
/// The longest window a split reads.
pub(crate) const SPLIT_WINDOW_MAX: Duration =
    Duration::from_nanos(aterm_vitals::explain::WINDOW_MAX_NS);
/// Safety margin inside [`SPLIT_WINDOW_MAX`] before an armed pull reads twice.
pub(crate) const SPLIT_COLD_MARGIN: Duration = Duration::from_secs(1);
const WHY_FIXTURE_SEAM: &str = "ATERM_DEV_WHY_FIXTURE";

/// Pulls that arm the split set: `sys why` and the `sys` queries that print the family, an
/// `await sys` naming `spotlight`, and a parked `await wake` whose wake names it.
pub(crate) static SPLIT_ARMED: Arming = Arming::new();
/// Queries that print CPU hardware (`sys cores`, `sys report`, a forced `sys why`): a worker's
/// pull ([`Msg::Pull`]) that begins while one waits takes a CPU Stats sample
/// ([`Request::hardware`]), which no other reading of the deck takes (A2 §6.4). A memory
/// consumer's census ([`Msg::MemoryPull`]) reads no arming.
pub(crate) static HARDWARE_ARMED: Arming = Arming::new();
/// Pulls that arm the stuck engine (`sys jobs`): such a pull's reading makes a new jobs view
/// inside the engine's cadence, which every other pull keeps.
pub(crate) static JOBS_ARMED: Arming = Arming::new();
/// Queries that print each process's scheduling
/// ([`aterm_vitals::sys::SysQuery::prints_scheduling`]): a worker's pull ([`Msg::Pull`]) that
/// begins while one waits asks its sweep for it ([`Request::scheduling`]), which a reading
/// nothing else asks reads none of. A memory consumer's census ([`Msg::MemoryPull`]) reads no
/// arming.
pub(crate) static SCHEDULING_ARMED: Arming = Arming::new();
/// Queries that print aterm's own thread CPU ([`aterm_vitals::sys::SysQuery::prints_self_cpu`]:
/// `sys why`, bare or of CPU), which wait here only where a reader reads them (macOS:
/// [`crate::sys_host::Shared`]'s `self_cpu_reads`): a worker's pull ([`Msg::Pull`]) that begins
/// while one waits reads aterm's threads ([`Request::self_cpu`]), which no other reading reads,
/// and reads them twice, [`COLD_PAIR`] apart, where the thread's last reading read none within
/// [`COLD_AFTER`] ([`Source::self_cpu_cold`]). A memory consumer's census ([`Msg::MemoryPull`])
/// reads no arming.
pub(crate) static SELF_CPU_ARMED: Arming = Arming::new();

/// How soon a reading no window shows is due again after one that gave `view` and `left`, for
/// what no edge tells: Slow's 30 s while [`held_on_slow`](crate::vitals_jobs::held_on_slow) (the
/// view holds a wait, since a start raises no edge, or the leftover view offers a row whose owner
/// is gone); else `None`. A live job's own `Demand` arms nothing with no window (an idle aterm
/// wakes zero times, owner's bar): the reading its end needs comes on the edge that end makes (its
/// shell's prompt, the exit its session's process watch hears), and its verdict between edges is
/// read at the disk watch's look or on a pull. The engine's `Demand` itself stays its jobs' own, so
/// a window's bars keep its cadence. Never under [`JOBS_FLOOR`].
fn held_wait_after(
    view: &aterm_vitals::jobs::JobsView,
    left: Option<&aterm_vitals::left::LeftView>,
) -> Option<Duration> {
    aterm_vitals::jobs::Demand::Slow
        .every_ms()
        .filter(|_| crate::vitals_jobs::held_on_slow(view, left))
        .map(|every| Duration::from_millis(every).max(JOBS_FLOOR))
}

/// The episode edge `after`'s findings make against `before`'s: `Opened` when `after` stands in a
/// backed-up episode `before` did not (another opening, or none); else `Drained` when `after` names
/// the episode that closed at it and `before` did not, or `before` stood in an episode and `after`
/// stands in none (the drained view itself skipped); else `None`, so a view adopted again makes no
/// edge.
pub(crate) fn held_edge(
    before: Option<&aterm_vitals::jobs::Findings>,
    after: &aterm_vitals::jobs::Findings,
) -> Option<crate::message_reporters::HeldEdge> {
    use crate::message_reporters::HeldEdge;
    let was = before.and_then(|f| f.program_check_episode);
    if let Some(now) = after.program_check_episode {
        return (was.map(|w| w.opened_ms) != Some(now.opened_ms)).then_some(HeldEdge::Opened(now));
    }
    after
        .program_check_drained
        .filter(|d| before.and_then(|f| f.program_check_drained) != Some(*d))
        .or(was)
        .map(HeldEdge::Drained)
}

/// The programs `after` names as not started that `before` did not: its `translating`,
/// `not_started` and `stopped_unstarted` findings whose pid `before`'s same list lacks, each with
/// what keeps it (a stopped-unstarted row with the bits `after` read), so a view adopted again
/// names none and each program is told once while it stands, whatever its bits read next.
pub(crate) fn newly_not_started<'a>(
    before: Option<&aterm_vitals::jobs::Findings>,
    after: &'a aterm_vitals::jobs::Findings,
) -> Vec<(aterm_vitals::jobs::StartWait, &'a aterm_vitals::jobs::Named)> {
    let was = |kind: &str, pid: u32| {
        before.is_some_and(|b| not_started_rows(b).any(|(w, n)| w.word() == kind && n.pid == pid))
    };
    not_started_rows(after)
        .filter(|(wait, n)| !was(wait.word(), n.pid))
        .collect()
}

/// The programs `after` names stranded at macOS's program check that `before` did not: its
/// `program_check_stranded` rows whose pid `before`'s lacks, so a view adopted again names none and
/// each program is told once while it stands stranded. A view that names no syspolicyd names none,
/// so the next that names one names each it finds; the pass keeps a verdict while it reads the row
/// unrun, so a stale look, a full pool or a backlog unnames none.
pub(crate) fn newly_stranded<'a>(
    before: Option<&aterm_vitals::jobs::Findings>,
    after: &'a aterm_vitals::jobs::Findings,
) -> Vec<&'a aterm_vitals::jobs::Named> {
    after
        .program_check_stranded
        .iter()
        .filter(|n| {
            !before.is_some_and(|b| b.program_check_stranded.iter().any(|w| w.pid == n.pid))
        })
        .collect()
}

/// The rows `f` names as not started but not as held at macOS's program check, each with
/// what keeps it: translating, then not started, then stopped or traced.
fn not_started_rows(
    f: &aterm_vitals::jobs::Findings,
) -> impl Iterator<Item = (aterm_vitals::jobs::StartWait, &aterm_vitals::jobs::Named)> {
    use aterm_vitals::jobs::StartWait;
    f.translating
        .iter()
        .map(|n| (StartWait::Translating, n))
        .chain(f.not_started.iter().map(|n| (StartWait::NotStarted, n)))
        .chain(f.stopped_unstarted.iter().map(|s| {
            (
                StartWait::StoppedUnstarted {
                    stopped: s.stopped,
                    traced: s.traced,
                },
                &s.named,
            )
        }))
}

/// How many programs aterm started after its Developer Tools grant took `after` names held
/// (`Findings::program_check_after_grant`) when `before` named none: one such program means
/// the grant did not take, told once while any stands.
pub(crate) fn after_grant_edge(
    before: Option<&aterm_vitals::jobs::Findings>,
    after: &aterm_vitals::jobs::Findings,
) -> Option<u32> {
    let now = after.program_check_after_grant;
    let was = before.map_or(0, |f| f.program_check_after_grant);
    (now > 0 && was == 0).then_some(now)
}

/// Kernel process-event edge (`NOTE_EXIT`, `NOTE_FORK`).
pub(crate) struct ProcEdge {
    armed: AtomicBool,
    fired: AtomicBool,
    post: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl ProcEdge {
    pub(crate) const fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
            fired: AtomicBool::new(false),
            post: Mutex::new(None),
        }
    }

    /// Records a fork, `exec`, or exit heard by a session's process watch.
    #[cfg(any(unix, test))]
    pub(crate) fn heard(&self) -> bool {
        if !self.armed.swap(false, Ordering::AcqRel) {
            return false;
        }
        self.fired.store(true, Ordering::Release);
        if let Some(post) = self
            .post
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            post();
        }
        true
    }

    /// Arms the edge while `want` is true and no event is pending.
    pub(crate) fn arm(&self, want: bool) {
        let on = want && !self.fired.load(Ordering::Acquire);
        self.armed.store(on, Ordering::Release);
    }

    /// Takes any event heard since the last call.
    pub(crate) fn take(&self) -> bool {
        self.fired.swap(false, Ordering::AcqRel)
    }

    /// Installs the callback invoked when [`Self::heard`] fires.
    pub(crate) fn install(&self, post: impl Fn() + Send + 'static) {
        *self.post.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(post));
    }
}

impl std::fmt::Debug for ProcEdge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcEdge")
            .field("armed", &self.armed.load(Ordering::Relaxed))
            .field("fired", &self.fired.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// The process-wide kernel edge shared by session gathers and [`VitalsHost`].
pub(crate) static PROC_EDGE: ProcEdge = ProcEdge::new();

/// A forced jobs observation owed since `after`, retried at `retry_at` until a fresh view covers it.
#[derive(Clone, Copy, Debug)]
struct JobsOwed {
    after: Instant,
    retry_at: Instant,
}

/// Newest snapshot and job events handed from the probe thread to the loop.
#[derive(Default)]
pub(crate) struct Handover {
    snap: Option<Arc<Snapshot>>,
    sampled_at: Option<Instant>,
    jobs_continuous: bool,
    cpu_leases: Option<Arc<crate::native_allocation::ObservedCpu>>,
    /// When the probe thread began the oldest reading, handed over since the loop last adopted one,
    /// that carries `snap`'s jobs view, so the view is dated no later than its making. A reading
    /// with a new view restarts it, since an earlier reading's start (a `sys` pull's while the loop
    /// slept) is not the new view's; a view the loop already adopted keeps its first post on the
    /// board (`JobsBoard::post`).
    began: Option<Instant>,
    words: Vec<WordSeen>,
    job_events: Vec<aterm_vitals::events::Event>,
    /// Health episodes' edges (a memory starvation episode's), for the loop's records.
    health_events: Vec<aterm_vitals::events::Event>,
}

/// Who would see a reading now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Audience {
    Shown,
    Background,
    Hidden,
    None,
}

impl Audience {
    const fn of(in_view: bool, drawn: bool, focused: bool) -> Self {
        match (in_view, drawn) {
            (true, _) if focused => Self::Shown,
            (true, _) => Self::Background,
            (false, true) => Self::Hidden,
            (false, false) => Self::None,
        }
    }

    /// Whether a window in view shows the bars or the Monitor tab.
    pub(crate) const fn in_view(self) -> bool {
        matches!(self, Self::Shown | Self::Background)
    }

    /// The reading cadence and whether to read machine counters alone.
    pub(crate) const fn cadence(self, lens: bool) -> Option<(Duration, bool)> {
        match self {
            Self::None => None,
            Self::Hidden => Some((HIDDEN_PERIOD, true)),
            Self::Shown | Self::Background if lens => Some((crate::stats_lens::LENS_PERIOD, false)),
            Self::Shown => Some((FOCUSED_PERIOD, false)),
            Self::Background => Some((BACKGROUND_PERIOD, false)),
        }
    }
}

/// Whether window `ws` gives the readings a person's audience while aterm has a focused window
/// (`App::vitals_audience`): it neither rests nor is unattended ([`crate::window_rest::Rest`]),
/// and it is focused, focused by the event at hand (`focusing`), or not occluded. So a person
/// who scrolls or points at a window beside the focused one keeps the focused cadence after the
/// focused window's attention lapsed.
pub(crate) fn attends_readings(ws: &crate::WindowState, focusing: bool) -> bool {
    (focusing || ws.focused || !ws.occluded) && !ws.rest.resting() && ws.rest.attended()
}

/// When the next reading at cadence `period` is due at `now`, `wall_ms` on the wall clock: at
/// once before any ask, else on the half-slot (record §5.2), the first instant at least half a
/// period after the last ask (`asked`) whose wall time is half a period past a multiple of the
/// period (focused: x.500; a lens: x.250 and x.750; the background: 2.5 s into each 5 s), so each
/// reading stands in the middle of its own slot and a wake's lateness never accumulates; and no
/// later than `armed`. With no wall clock (`wall_ms` `None`), a period after the last ask.
fn due_on_phase(
    asked: Option<Instant>,
    armed: Option<Instant>,
    period: Duration,
    now: Instant,
    wall_ms: Option<i64>,
) -> Instant {
    let paced = asked.map_or(now, |t| match wall_ms {
        Some(wall_ms) => on_phase(t + period / 2, period, now, wall_ms),
        None => t + period,
    });
    armed.map_or(paced, |a| a.min(paced))
}

/// The first instant at or after `from` whose wall time is half of `period` past a multiple of
/// it, `now` being `wall_ms` on the wall clock.
fn on_phase(from: Instant, period: Duration, now: Instant, wall_ms: i64) -> Instant {
    let p = i64::try_from(period.as_millis()).unwrap_or(i64::MAX).max(1);
    let from_wall = wall_ms.saturating_add(signed_ms(from, now));
    let wait = (p / 2).saturating_sub(from_wall).rem_euclid(p);
    from + Duration::from_millis(u64::try_from(wait).unwrap_or(0))
}

/// `a − b` in whole milliseconds, either way round.
fn signed_ms(a: Instant, b: Instant) -> i64 {
    let ms = |d: Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    match a.checked_duration_since(b) {
        Some(ahead) => ms(ahead),
        None => -ms(b.saturating_duration_since(a)),
    }
}

/// Where the reading cadence reads the wall clock it keeps to ([`due_on_phase`]).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Wall {
    /// The system's.
    #[cfg_attr(
        test,
        expect(
            dead_code,
            reason = "a test's host keeps to no clock or to a fixed one"
        )
    )]
    System,
    /// `wall_ms` at `at`, moving with the monotonic clock: a test's (its one test is Unix's).
    #[cfg(all(test, unix))]
    Fixed { at: Instant, wall_ms: i64 },
    /// None: a reading is due a period after the last ask. Tests default to it, so their
    /// cadences count from their asks whatever the clock reads.
    #[cfg(test)]
    Unread,
}

impl Wall {
    /// The wall clock at `now`, unix ms; `None` where it does not read.
    fn at(self, now: Instant) -> Option<i64> {
        match self {
            Self::System => {
                let wall_ms = unix_ms_now();
                (wall_ms > 0).then(|| wall_ms.saturating_sub(signed_ms(Instant::now(), now)))
            }
            #[cfg(all(test, unix))]
            Self::Fixed { at, wall_ms } => Some(wall_ms.saturating_add(signed_ms(now, at))),
            #[cfg(test)]
            Self::Unread => None,
        }
    }
}

#[path = "vitals_process_memory.rs"]
pub(crate) mod memory;

/// What the loop asks the thread to read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Request {
    /// The Allocation view requests a nonblocking lease read on this worker.
    pub(crate) allocation: bool,
    /// This process's pid (aterm's tree is rooted here).
    pub(crate) self_pid: u32,
    /// Every pane's shell: each terminal session a tab holds is its own root, so a split pane's
    /// processes count in that pane.
    pub(crate) tabs: Vec<TabRoot>,
    /// Shell generations recorded when each pooled session took its process.
    pub(crate) shell_identities: Vec<(u64, aterm_pty::ShellIdentity)>,
    /// Those roots' terminal mode words, where read ([`TtyWord`]).
    pub(crate) ttys: Vec<TtyWord>,
    /// Where each of those roots' words is read again ([`Request::reread_words`]).
    pub(crate) tty_sources: Vec<TtySource>,
    /// What those roots' PTY readers read, read again at each pass
    /// ([`crate::vitals_jobs::JobFeed`]).
    pub(crate) feeds: Vec<(u64, Arc<crate::vitals_jobs::JobFeed>)>,
    /// When aterm's Developer Tools grant took effect and which of those roots another aterm
    /// process handed over ([`App::vitals_grant`]).
    pub(crate) grant: crate::vitals_jobs::GrantFacts,
    /// The keys driven into those roots' terminals, counted again at each pass
    /// ([`crate::vitals_jobs::JobFeed::typed_at`]).
    pub(crate) inputs: Vec<(u64, aterm_session::sink::DrivenInputWatch)>,
    /// Where each of those sessions sits (window, tab), for `sys`.
    pub(crate) layout: Vec<PaneFact>,
    /// Authenticated global session ids for those process-local tab keys.
    pub(crate) session_ids: Vec<(u64, String)>,
    /// Launch identities observed with those same shell roots, for identity-bound peer reads.
    pub(crate) pane_sources: Vec<crate::vitals_host::memory::PaneSource>,
    /// Every session aterm's table holds, its local key and id: `sys disk held`'s live set
    /// ([`crate::sys_host::held_for`]). `session_ids` leaves out a session with no pooled
    /// terminal or no shell, which that reply still counts as at work.
    pub(crate) live_ids: Vec<(u64, String)>,
    /// The cadence this reading was asked at, ms: the gap rule's (Stats Deck v2 §5.4).
    pub(crate) cadence_ms: u32,
    /// Read the machine counters alone: every window that draws the bars is hidden (Stats Deck v2
    /// §5.3).
    pub(crate) machine_only: bool,
    /// The stuck engine looks at this reading even inside its own cadence.
    pub(crate) jobs_now: bool,
    /// A control worker's pull ([`Msg::Pull`]) or a memory consumer's census
    /// ([`Msg::MemoryPull`]; the thread sets it): it reads the split set only while a query or
    /// wait arms it ([`SPLIT_ARMED`] names which), and its readings stand for an instant in the
    /// rings ([`aterm_vitals::model::Sample::pull`]).
    pub(crate) pull: bool,
    /// A worker's pull ([`Msg::Pull`]) that a `sys cores`, `sys report` or forced `sys why` had
    /// armed when it began ([`HARDWARE_ARMED`]; `pull` sets it): its readings take a CPU Stats
    /// sample. The pull reads the arming once, so a reading begun before a query armed takes
    /// none, and the query answers from its own pull's pair. A memory census
    /// ([`Msg::MemoryPull`]) leaves it false.
    pub(crate) hardware: bool,
    /// A worker's pull ([`Msg::Pull`]) that a query printing each process's scheduling had
    /// armed when it began ([`SCHEDULING_ARMED`]; `pull` sets it): its readings ask their sweep
    /// for it ([`Reader::sweep_reads_scheduling`]). A memory census ([`Msg::MemoryPull`]) leaves
    /// it false.
    pub(crate) scheduling: bool,
    /// A worker's pull ([`Msg::Pull`]) that a query printing aterm's own thread CPU had armed
    /// when it began ([`SELF_CPU_ARMED`]; `pull` sets it): its readings read aterm's threads
    /// (`task_threads` and each thread's `thread_info` on macOS), which a reading nothing asks
    /// reads none of. A memory census ([`Msg::MemoryPull`]) leaves it false.
    pub(crate) self_cpu: bool,
    /// A window in view shows the bars, as the Deck or the Sill, or the System Resources page
    /// (whose head draws the Full Deck): only then is the reading a split tick ([`split_due`]).
    /// The Sill draws no cause clause, but a close-up opens from it as from the Deck, and a
    /// close-up's causes are the episodes already open.
    pub(crate) deck: bool,
    /// A window in view shows the bars or a page that reads them ([`Audience::in_view`]): when
    /// none does, Linux's all-states TCP dump paces itself to a hidden deck's minute
    /// ([`Source::after_publish`]).
    pub(crate) in_view: bool,
    /// Every agent session's token totals, each session once.
    pub(crate) tokens: Vec<TokenRow>,
    /// Individual dated provider folds, collected even when the terminal has no open view.
    pub(crate) pane_token_observations: Vec<aterm_vitals::pane_tokens::Observation>,
    /// The front tab's plan usage, while current.
    pub(crate) plan: Option<PlanFact>,
    /// The rows whose lens a window holds open: the CPU lens's asks each reading for CPU Stats
    /// ([`HardwareDemand::Lens`]).
    pub(crate) lenses: Vec<aterm_vitals::Resource>,
    /// Discontinuity gaps queued on the host to record on the next reading.
    pub(crate) note_gaps: Vec<aterm_vitals::TimeGap>,
    /// Strip events (crash, restart, sleep/wake, spike) queued on the host.
    pub(crate) note_events: Vec<aterm_vitals::StripEvent>,
    /// The front tab's working directory, as its focused pane last reported it (OSC 7), where
    /// this machine made the report: the Disk row draws its device beside home's when it is
    /// another (A1 §4.4).
    pub(crate) front_dir: Option<PathBuf>,
    /// The closed tabs the host told of, by key ([`VitalsHost::left_told`]): the leftover pass
    /// keeps them told ([`crate::vitals_left::LeftPass::keep_told`]).
    pub(crate) left_told: Vec<u64>,
    /// Internal completion pull: publish the finished assessment without offering it again.
    pub(crate) assessment_only: bool,
}

/// One agent session's token totals and when the footer's resolver last read them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TokenRow {
    pub(crate) totals: TabTokens,
    pub(crate) read_at: Option<Instant>,
}

/// A session's terminal mode word read by the loop for a reading.
#[derive(Clone, Debug)]
pub(crate) struct TtyWord {
    /// The session's key ([`TabRoot::key`]).
    pub(crate) key: u64,
    /// `ICANON`.
    pub(crate) canonical: bool,
    /// `ECHO`.
    pub(crate) echo: bool,
    output_seen: u64,
    /// The reader's count of feeds that moved the screen ([`ScreenMoves`]): a write that moves no
    /// cell leaves the word standing (`docs/measured/vitals-3-2026-10-22.md`, V1).
    output: Arc<ScreenMoves>,
    input_seen: aterm_session::sink::DrivenInput,
    input: aterm_session::sink::DrivenInputWatch,
}

impl TtyWord {
    /// Reads the terminal mode word of session `key` behind `sink`: none while a key waits unread,
    /// or while nothing says whether one does
    /// ([`aterm_session::sink::SinkWriter::unread_input`]), since the tty names a wait for the
    /// person only over an empty queue.
    pub(crate) fn read(
        key: u64,
        sink: &aterm_session::sink::SinkWriter,
        output: &Arc<ScreenMoves>,
    ) -> Option<Self> {
        Self::read_seamed(
            key,
            sink,
            output,
            || {},
            aterm_session::sink::SinkWriter::unread_input,
        )
    }

    fn read_seamed(
        key: u64,
        sink: &aterm_session::sink::SinkWriter,
        output: &Arc<ScreenMoves>,
        between: impl FnOnce(),
        unread: impl FnOnce(&aterm_session::sink::SinkWriter) -> Option<usize>,
    ) -> Option<Self> {
        let output_seen = output.count();
        let input = sink.driven_input_watch();
        let input_seen = input.get();
        between();
        let echo = sink.tty_echo()?;
        if unread(sink) != Some(0) {
            return None;
        }
        Some(Self {
            key,
            canonical: echo.canonical,
            echo: echo.echo,
            output_seen,
            output: Arc::clone(output),
            input_seen,
            input,
        })
    }

    /// Returns the snapshot of this word kept by the loop once a reading lands.
    pub(crate) const fn seen(&self) -> WordSeen {
        WordSeen {
            key: self.key,
            canonical: self.canonical,
            echo: self.echo,
            input: self.input_seen,
        }
    }

    /// Returns the engine's word while no key was driven since the read. While no feed moved the
    /// screen since, it was read after every output the jobs feed holds, so it is stamped
    /// `u64::MAX`, newer than each of their stamps ([`crate::vitals_jobs::JobFeed::at_pass`]).
    /// After one it keeps its mode, which judges the job's output, but is stamped 0, before every
    /// output stamp, so the engine names no wait from it. Dropping the word instead would leave a
    /// program that redraws all the time with no mode, judged canonical, and its output uncounted.
    pub(crate) fn current(&self) -> Option<(u64, aterm_vitals::jobs::TtyMode)> {
        (self.input.get() == self.input_seen).then(|| {
            let moved = self.output.count() != self.output_seen;
            (
                self.key,
                aterm_vitals::jobs::TtyMode {
                    canonical: self.canonical,
                    echo: self.echo,
                    stamped_ms: if moved { 0 } else { u64::MAX },
                },
            )
        })
    }
}

/// Where a session's terminal mode word is read: the session's sink, held weakly so a request the
/// probe thread keeps never keeps a closed session's master open (and never holds a bare fd number
/// another PTY could reuse), and the reader's count of screen moves.
#[derive(Clone, Debug)]
pub(crate) struct TtySource {
    key: u64,
    sink: std::sync::Weak<aterm_session::sink::SinkWriter>,
    output: Arc<ScreenMoves>,
}

impl TtySource {
    /// The source of session `key`'s word, behind `sink`.
    pub(crate) fn new(
        key: u64,
        sink: &Arc<aterm_session::sink::SinkWriter>,
        output: &Arc<ScreenMoves>,
    ) -> Self {
        Self {
            key,
            sink: Arc::downgrade(sink),
            output: Arc::clone(output),
        }
    }

    /// Reads the word now ([`TtyWord::read`]): none once the session is gone.
    pub(crate) fn read(&self) -> Option<TtyWord> {
        let sink = self.sink.upgrade()?;
        TtyWord::read(self.key, &sink, &self.output)
    }
}

/// A [`TtyWord`] as a reading's request carried it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WordSeen {
    /// The session's key ([`TabRoot::key`]).
    pub(crate) key: u64,
    /// `ICANON` when the word was read.
    pub(crate) canonical: bool,
    /// `ECHO` when the word was read.
    pub(crate) echo: bool,
    /// The sink's driven input just before the word was read.
    pub(crate) input: aterm_session::sink::DrivenInput,
}

/// Minimum age before a parked session's own write triggers a recheck reading.
pub(crate) const PARKED_RECHECK: Duration = JOBS_FLOOR;

/// A test's stand-in for a PTY reader's feed of session `u64`.
#[cfg(test)]
pub(crate) type ReaderFeed = Box<dyn Fn(&crate::App, u64)>;

#[cfg(test)]
thread_local! {
    /// Callback run by `App::on_output_wake` just before clearing the session's output latch.
    pub(crate) static BEFORE_LATCH_CLEAR: std::cell::RefCell<Option<ReaderFeed>> =
        const { std::cell::RefCell::new(None) };
}

/// A session parked on the person's read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Parked {
    /// The parked job's process group.
    pub(crate) pgid: u32,
    /// The word the engine named the read from.
    pub(crate) word: WordSeen,
    /// When a reading last looked at the session: the view that parked it, or its own write's
    /// [`OutputEdge::Recheck`].
    pub(crate) read_at: Instant,
}

impl Parked {
    /// Collects the sessions parked by `view` at `now`.
    pub(crate) fn of(
        view: &aterm_vitals::jobs::JobsView,
        words: &[WordSeen],
        now: Instant,
    ) -> std::collections::HashMap<u64, Self> {
        view.parked_tabs
            .iter()
            .filter_map(|tab| {
                let word = *words.iter().find(|w| w.key == *tab)?;
                let mut groups = view
                    .rows
                    .iter()
                    .filter(|r| r.tab == *tab && r.verdict != aterm_vitals::jobs::Verdict::Exited)
                    .map(|r| r.pgid);
                let pgid = groups.next()?;
                groups.all(|g| g == pgid).then_some((
                    *tab,
                    Self {
                        pgid,
                        word,
                        read_at: now,
                    },
                ))
            })
            .collect()
    }

    /// Whether the park still holds on the terminal now.
    pub(crate) fn holds(
        &self,
        fg: i32,
        mode: Option<(bool, bool)>,
        input: aterm_session::sink::DrivenInput,
    ) -> bool {
        i64::from(fg) == i64::from(self.pgid)
            && mode == Some((self.word.canonical, self.word.echo))
            && input == self.word.input
    }
}

/// How many of the PTY reader's feeds moved the screen, and when the last one did
/// (`docs/DESIGN-system-awareness-2026-10-16.md` §6.1): counted by the reader under its hold of the
/// term lock, after each feed whose screen generation ([`crate::control::screen_gen`]) is not the
/// one it saw after its last feed. The output wake reads it without the term lock
/// ([`crate::App::vitals_output_kind`]), so a lock held elsewhere (a control worker's `text` or
/// `status`) never passes for a move, and a reading's terminal mode word holds while it stands
/// ([`TtyWord`]): a write that moves no cell, such as a colour reset at rest, never arms
/// `system_vitals` (`docs/measured/vitals-3-2026-10-22.md`, V1).
#[derive(Debug, Default)]
pub(crate) struct ScreenMoves {
    /// The generation the reader saw after its last feed; only the feeding side takes it.
    seen: Mutex<Option<crate::control::ScreenGen>>,
    moves: AtomicU64,
    /// When a feed last moved the screen, on the wall clock ([`aterm_gui_timing::wall_ms`]); `0`
    /// before one did.
    moved_ms: AtomicU64,
}

impl ScreenMoves {
    /// A feed left the screen at `now` (the reader, holding the term lock): whether it moved it.
    pub(crate) fn fed(&self, now: crate::control::ScreenGen) -> bool {
        let moved = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(now)
            != Some(now);
        if moved {
            self.moved_ms
                .store(aterm_gui_timing::wall_ms(), Ordering::Relaxed);
            self.moves.fetch_add(1, Ordering::Release);
            // The wake protocol's store-buffer pair, with [`Self::count_after_clear`]: the reader
            // counts this move, then reads the session's wake latch to post or coalesce its wake;
            // the output wake clears that latch, then reads this count. Without a fence on each
            // side both reads may take the value from before the other's write (Release and
            // Acquire allow it): the reader coalesces into a wake whose pass reads the old count,
            // and the move is no edge until the next one. The wake reads the count without the
            // term lock, so only these fences order the two.
            fence(Ordering::SeqCst);
        }
        moved
    }

    /// When a feed last moved the screen ([`Self::moved_ms`]): what a session's last topic post
    /// is newer than, or not, where its screen's rows are its words
    /// ([`crate::survey::last_words`]).
    pub(crate) fn moved_ms(&self) -> u64 {
        self.moved_ms.load(Ordering::Relaxed)
    }

    /// How many feeds moved the screen so far.
    pub(crate) fn count(&self) -> u64 {
        self.moves.load(Ordering::Acquire)
    }

    /// [`Self::count`], read by the output wake after it cleared the session's wake latch
    /// ([`crate::App::vitals_output_kind`]; the fence pairs with [`Self::fed`]'s).
    pub(crate) fn count_after_clear(&self) -> u64 {
        fence(Ordering::SeqCst);
        self.count()
    }
}

/// What one write of a session is to the stuck engine with no window showing the bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputEdge {
    Edge,
    Quiet,
    Recheck,
}

/// What the thread is sent.
#[derive(Debug)]
pub(crate) enum Msg {
    Read(Box<Request>),
    Facts(Box<Request>),
    Pull,
    /// A memory consumer needs one full census, without a CPU cold pair.
    MemoryPull,
    AssessmentReady,
    BootReady,
    CpuPolicy(aterm_vitals::sys::json::Jv),
    Name(Box<crate::sys_naming::Request>),
    /// The host's tellings at once, as the next request would carry them ([`Request::left_told`]).
    Told(Vec<u64>),
}

/// The loop's side of the probe thread.
pub(crate) struct VitalsHost {
    tx: Option<SyncSender<Msg>>,
    assessment_wake: Option<Arc<SyncSender<Msg>>>,
    sent_layout: Option<u64>,
    layout_looked: Option<Instant>,
    sys: Option<&'static crate::sys_host::Shared>,
    /// Set by [`Self::stop_at_exit`], which the app's quit runs once its event loop has ended: the
    /// probe thread writes its stop clean only where this is set when the host hangs up ([`run`]).
    /// A main thread's panic before the quit unwinds out of the event loop past it, so a run it
    /// takes down leaves its strip unclean, however its probe thread ends. One later in the quit,
    /// once this is set, leaves a clean stop where the probe thread writes it after that panic's
    /// event time (a thread still reading when the quit's wait for it ran out), and that crash
    /// draws as a pause.
    stopping: Arc<AtomicBool>,
    thread: Option<(std::thread::JoinHandle<()>, Receiver<()>)>,
    tried: bool,
    latest: Arc<Mutex<Handover>>,
    /// The snapshot the Deck draws.
    pub(crate) snapshot: Option<Arc<Snapshot>>,
    /// Worker read start, never the later UI adoption time.
    pub(crate) sampled_at: Option<Instant>,
    jobs_continuous: bool,
    pub(crate) cpu_leases: Option<Arc<crate::native_allocation::ObservedCpu>>,
    /// When the held snapshot's jobs view was adopted: what a reading no window shows counts its
    /// next from ([`Self::windowless_due`]).
    pub(crate) adopted_at: Option<Instant>,
    /// When the probe thread began the oldest reading carrying the held snapshot's jobs view
    /// since the adoption before (`Handover::began`): what the jobs board counts a new view's
    /// age from (`App::post_jobs_reading`).
    read_began: Option<Instant>,
    full_adopted_at: Option<Instant>,
    /// When the held snapshot landed (any new reading, whatever its jobs view): what the deck's
    /// live edge counts its beat and its staleness from (`crate::stats_live`).
    pub(crate) landed_at: Option<Instant>,
    /// The reading a strip's fine band slides or glides right for: the newest that opened a slot
    /// of its snapshot's grid past the snapshot held before it, kept through readings that open
    /// none ([`crate::stats_live::Opened`]).
    pub(crate) landed_open: Option<crate::stats_live::Opened>,
    /// The snapshot held immediately before [`Self::snapshot`], for smooth value interpolation
    /// across readings (`crate::stats_deck`).
    pub(crate) prev_snapshot: Option<Arc<Snapshot>>,
    /// The bars' cadence when it landed (`None` with no window showing them): a reading speaks
    /// for [`crate::stats_live::STALE_PERIODS`] of the slower of this and the cadence now.
    pub(crate) landed_period: Option<Duration>,
    /// The dial's needle in motion, one per app since the strain is the whole machine's
    /// ([`crate::stats_needle`]): retargeted where a snapshot is adopted
    /// (`App::retarget_needle`), and in a cell so a layout that adds, drops or resizes a dial can
    /// rest it on its target. `None` before a reading with a needle.
    pub(crate) needle: std::cell::Cell<Option<crate::stats_needle::Needle>>,
    /// When the next reading is due: the bars' cadence while a window in view shows them.
    pub(crate) next_due: Option<Instant>,
    asked_at: Option<Instant>,
    jobs_due: Option<Instant>,
    /// A forced look remains owed through queue acceptance and cached or stalled readings until
    /// a new jobs view observed since its requirement arrives ([`Self::adopt`]). Each attempt
    /// moves only its retry, never the observation it requires.
    jobs_owed: Option<JobsOwed>,
    /// An external output, process or close edge has not yet been given its required observation
    /// time. Service stamps it before adopting an in-flight view; retries never raise this flag.
    jobs_changed: bool,
    delegated_due: Option<Instant>,
    windowed: bool,
    disk_pull: bool,
    edge: bool,
    /// A session closed since the last ask ([`Self::note_tab_closed`]): the next service asks
    /// a whole reading at once, so the close knows what it left, and asks again while the
    /// thread's queue is full.
    tab_closed: bool,
    last_edge_read: Option<Instant>,
    /// Each session's [`ScreenMoves::count`] at its last output wake.
    screens: std::collections::HashMap<u64, u64>,
    parked: std::collections::HashMap<u64, Parked>,
    job_events: Vec<aterm_vitals::events::Event>,
    /// Health episodes' edges handed over and not yet recorded ([`Self::take_health_events`]).
    health_events: Vec<aterm_vitals::events::Event>,
    job_revs: std::collections::HashMap<u64, u64>,
    delegated_active: bool,
    chips_shown: bool,
    pending_gaps: Vec<aterm_vitals::TimeGap>,
    pending_cpu_policy: Option<aterm_vitals::sys::json::Jv>,
    #[cfg(target_os = "macos")]
    cpu_policy_generation: Option<(u64, u64)>,
    /// This App's own among CPU policy operations ([`App::cpu_policy_owner`]).
    #[cfg(target_os = "macos")]
    cpu_policy_owner: crate::cpu_policy_authorization::Owner,
    pending_events: Vec<aterm_vitals::StripEvent>,
    /// Whether the run before this one handed it its place (a seamless adoption, a relaunch of
    /// the running app for an update, or Windows' update spawn) and the startup scan found no
    /// kill evidence, so that run stopped on purpose, perhaps without its final strip write:
    /// [`open_strip`] reads its unclean checkpoint as a stop, never a crash, unless a crash event
    /// is newer than it ([`Self::crashed_at`]). Set before the probe thread starts
    /// ([`Self::note_handed_off_start`]).
    handed_off: bool,
    /// When the run before this one crashed, by its report's event time or artifact modification
    /// time as fallback, where the startup scan found evidence
    /// (`vitals_startup::PriorStop::Crashed` or `CrashedAndKilled`): [`open_strip`] dates the
    /// crash from it where the kept strip is older than it and no live peer holds [`STRIP_LOCK`],
    /// in time order with a killed run's launch ([`Self::killed_since`]). Set before the probe
    /// thread starts ([`Self::note_crash_at`]).
    crashed_at: Option<i64>,
    /// When the run before this one launched, where the startup scan found it killed
    /// (`vitals_startup::PriorStop::Killed` or `CrashedAndKilled`): [`open_strip`] dates the kill
    /// from it where the kept strip holds no checkpoint of that run and no live peer holds
    /// [`STRIP_LOCK`]. Set before the probe thread starts ([`Self::note_killed_run_launch`]).
    killed_since: Option<i64>,
    survey: Arc<Where>,
    /// The System Resources page's Spotlight scope probe ([`crate::resources_spotlight`]).
    pub(crate) spotlight_scope: Arc<crate::resources_spotlight::ScopeSlot>,
    /// The System Resources page's macOS configuration reader ([`crate::resources_macos`]).
    pub(crate) macos_config: Arc<crate::resources_macos::MacosSlot>,
    board: &'static crate::vitals_jobs::JobsBoard,
    proc_edge: &'static ProcEdge,
    /// The wall clock the reading cadence keeps to.
    pub(crate) wall: Wall,
    /// The closed tabs whose leftovers a `proc.left` record told, among those the adopted view
    /// holds: each close is told once ([`crate::message_reporters::newly_left`]).
    left_told: std::collections::BTreeSet<u64>,
    /// The dangling links a `proc.link` record told, by path
    /// ([`crate::message_reporters::newly_dangling`]).
    links_told: std::collections::VecDeque<String>,
    /// What the `vitals.stranded` entry last handed said: the pids it named and the syspolicyd
    /// running ([`App::note_held_episode`]).
    stranded_said: Option<(Vec<u32>, u32)>,
}

impl Default for VitalsHost {
    fn default() -> Self {
        Self {
            tx: None,
            assessment_wake: None,
            sent_layout: None,
            layout_looked: None,
            sys: None,
            stopping: Arc::default(),
            thread: None,
            tried: false,
            latest: Arc::default(),
            snapshot: None,
            sampled_at: None,
            jobs_continuous: false,
            cpu_leases: None,
            adopted_at: None,
            read_began: None,
            full_adopted_at: None,
            landed_at: None,
            landed_open: None,
            prev_snapshot: None,
            landed_period: None,
            needle: std::cell::Cell::new(None),
            next_due: None,
            asked_at: None,
            jobs_due: None,
            jobs_owed: None,
            jobs_changed: false,
            delegated_due: None,
            windowed: false,
            disk_pull: false,
            edge: false,
            tab_closed: false,
            last_edge_read: None,
            screens: std::collections::HashMap::new(),
            parked: std::collections::HashMap::new(),
            job_events: Vec::new(),
            health_events: Vec::new(),
            job_revs: std::collections::HashMap::new(),
            delegated_active: false,
            chips_shown: false,
            pending_gaps: Vec::new(),
            pending_events: Vec::new(),
            pending_cpu_policy: None,
            #[cfg(target_os = "macos")]
            cpu_policy_generation: None,
            #[cfg(target_os = "macos")]
            cpu_policy_owner: crate::cpu_policy_authorization::Owner::next(),
            handed_off: false,
            crashed_at: None,
            killed_since: None,
            survey: Arc::default(),
            spotlight_scope: Arc::default(),
            macos_config: Arc::default(),
            board: &crate::vitals_jobs::BOARD,
            proc_edge: if cfg!(test) {
                Box::leak(Box::new(ProcEdge::new()))
            } else {
                &PROC_EDGE
            },
            #[cfg(test)]
            wall: Wall::Unread,
            #[cfg(not(test))]
            wall: Wall::System,
            left_told: std::collections::BTreeSet::new(),
            links_told: std::collections::VecDeque::new(),
            stranded_said: None,
        }
    }
}

impl VitalsHost {
    /// Whether a "where" survey is running or has completed.
    #[cfg(all(test, unix))]
    pub(crate) fn survey_asked(&self) -> bool {
        let (last, running) = self.survey.read();
        running || last.is_some()
    }

    /// Adopts the thread's newest snapshot if newer than the held one, along with its job events.
    pub(crate) fn adopt(&mut self, now: Instant) -> bool {
        self.require_changed_jobs(now);
        let (fresh, began, words, events, health, leases, sampled_at, jobs_continuous) = {
            let mut h = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            (
                h.snap.take(),
                h.began.take(),
                std::mem::take(&mut h.words),
                std::mem::take(&mut h.job_events),
                std::mem::take(&mut h.health_events),
                h.cpu_leases.take(),
                h.sampled_at.take(),
                h.jobs_continuous,
            )
        };
        if leases.is_some() {
            self.cpu_leases = leases;
        }
        self.health_events.extend(health);
        if self.health_events.len() > HEALTH_EVENTS_CAP {
            let over = self.health_events.len() - HEALTH_EVENTS_CAP;
            self.health_events.drain(..over);
        }
        self.job_events.extend(events);
        if self.job_events.len() > JOB_EVENTS_CAP {
            let over = self.job_events.len() - JOB_EVENTS_CAP;
            self.job_events.drain(..over);
        }
        match fresh {
            Some(snap) if self.snapshot.as_ref().is_none_or(|s| s.seq != snap.seq) => {
                let held = self.snapshot.as_ref().and_then(|s| s.jobs.as_ref());
                let same = match (held, snap.jobs.as_ref()) {
                    (Some(held), Some(new)) => Arc::ptr_eq(held, new),
                    (held, new) => held.is_none() && new.is_none(),
                };
                if !same {
                    self.adopted_at = Some(now);
                    // Only the actual observation of a new jobs view can discharge a forced
                    // look. An older in-flight view and a cached reading leave it owed.
                    if snap.jobs.is_some()
                        && began
                            .zip(self.jobs_owed)
                            .is_some_and(|(began, owed)| began >= owed.after)
                    {
                        self.jobs_owed = None;
                    }
                    self.parked = snap
                        .jobs
                        .as_deref()
                        .zip(began)
                        .filter(|(_, at)| {
                            now.saturating_duration_since(*at) <= crate::vitals_jobs::JOBS_FRESH
                        })
                        .map(|(view, at)| Parked::of(view, &words, at))
                        .unwrap_or_default();
                }
                if !snap.machine_only {
                    self.full_adopted_at = Some(now);
                }
                self.landed_at = Some(now);
                self.landed_open = Some(crate::stats_live::Opened::after(
                    self.landed_open,
                    self.snapshot.as_ref().map(|held| held.at_unix_ms),
                    snap.at_unix_ms,
                    snap.slot_grid,
                    now,
                ));
                self.prev_snapshot = self.snapshot.clone();
                self.sampled_at = sampled_at;
                self.jobs_continuous = jobs_continuous;
                self.read_began = began;
                self.snapshot = Some(crate::strain_script::adopt(snap, now));
                true
            }
            _ => false,
        }
    }

    fn windowless_due(&self) -> Option<Instant> {
        if !self.reader_runs() {
            return None;
        }
        if let Some(owed) = self.jobs_owed {
            return Some(owed.retry_at);
        }
        self.snapshot.as_ref().and_then(|snap| {
            let view = snap.jobs.as_deref()?;
            let after = crate::vitals_jobs::windowless_after(view)
                .or_else(|| held_wait_after(view, snap.left.as_deref()))?;
            Some(self.adopted_at? + after)
        })
    }

    fn windowless_deadline(&self, hidden: Option<Duration>, now: Instant) -> Option<Instant> {
        let wall_ms = self.wall.at(now);
        let paced = hidden.map(|period| due_on_phase(self.asked_at, None, period, now, wall_ms));
        let full = match (self.jobs_due, self.delegated_due) {
            (Some(engine), Some(delegated)) => Some(engine.min(delegated)),
            (engine, delegated) => engine.or(delegated),
        };
        match (full, paced) {
            (Some(engine), Some(paced)) => Some(engine.min(paced)),
            (engine, paced) => engine.or(paced),
        }
    }

    /// Marks a session's close: the next service asks one whole reading at once, windowed or
    /// not, so the leftover pass reads the closed tab's horizon at the exit edge.
    pub(crate) const fn note_tab_closed(&mut self) {
        self.tab_closed = true;
        self.jobs_changed = true;
    }

    /// Marks an output or process-event edge when no window shows the bars.
    pub(crate) const fn note_edge(&mut self) {
        if !self.windowed && self.tx.is_some() {
            self.edge = true;
            self.jobs_changed = true;
        }
    }

    /// Keep the look owed by an external change, or by a timed forced pass. A retry uses the
    /// existing requirement; a newer external change supersedes it before any old view lands.
    fn owe_jobs(&mut self, now: Instant, changed: bool) {
        if !self.reader_runs() {
            self.jobs_owed = None;
            self.jobs_due = None;
            return;
        }
        match &mut self.jobs_owed {
            Some(owed) if changed => owed.after = owed.after.max(now),
            Some(_) => {}
            None => {
                self.jobs_owed = Some(JobsOwed {
                    after: now,
                    retry_at: now + JOBS_FLOOR,
                })
            }
        }
    }

    fn require_changed_jobs(&mut self, now: Instant) {
        if std::mem::take(&mut self.jobs_changed) {
            self.owe_jobs(now, true);
        }
    }

    fn take_proc_edge(&mut self) {
        if self.proc_edge.take() {
            self.note_edge();
        }
    }

    fn arm_proc_edge(&self) {
        self.proc_edge
            .arm(!self.windowed && self.tx.is_some() && !self.edge);
    }

    /// Whether the probe thread is running.
    pub(crate) const fn reader_runs(&self) -> bool {
        self.tx.is_some()
    }

    pub(crate) const fn jobs_view_continuous(&self) -> bool {
        self.reader_runs() && self.jobs_continuous
    }

    fn edge_at(&self, now: Instant) -> Option<Instant> {
        if !self.edge || self.windowed {
            return None;
        }
        Some(
            self.last_edge_read
                .map_or(now, |t| (t + JOBS_FLOOR).max(now)),
        )
    }

    /// Takes the job events handed over from the thread.
    pub(crate) fn take_job_events(&mut self) -> Vec<aterm_vitals::events::Event> {
        std::mem::take(&mut self.job_events)
    }

    /// Takes the health episodes' edges handed over from the thread.
    pub(crate) fn take_health_events(&mut self) -> Vec<aterm_vitals::events::Event> {
        std::mem::take(&mut self.health_events)
    }

    /// Returns the next `rev=` number for session `key`'s job events.
    pub(crate) fn next_job_rev(&mut self, key: u64) -> u64 {
        let rev = self.job_revs.entry(key).or_insert(0);
        *rev += 1;
        *rev
    }

    fn ensure_thread(&mut self, proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>) {
        if self.tx.is_some() || self.tried {
            return;
        }
        let proxy = proxy.clone();
        let survey = Arc::clone(&self.survey);
        let edge_proxy = proxy.clone();
        self.proc_edge.install(move || {
            let _ = edge_proxy.send_event(crate::Wake::Vitals);
        });
        self.start(
            move || Reader::new(survey),
            move || proxy.send_event(crate::Wake::Vitals).is_ok(),
            aterm_types::dirs::state_dir().map(|dir| dir.join(PEAKS_FILE)),
            Some(&crate::sys_host::SYS),
        );
    }

    fn start<S: Source>(
        &mut self,
        source: impl FnOnce() -> S + Send + 'static,
        post: impl Fn() -> bool + Send + 'static,
        peaks: Option<PathBuf>,
        sys: Option<&'static crate::sys_host::Shared>,
    ) {
        self.tried = true;
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        let (ended, done) = std::sync::mpsc::channel::<()>();
        let assessment_wake = Arc::new(tx.clone());
        let weak_wake = Arc::downgrade(&assessment_wake);
        let latest = Arc::clone(&self.latest);
        let workers = sys.map(|shared| (shared, tx.clone()));
        let stopping = Arc::clone(&self.stopping);
        let prior = PriorRun {
            handed_off: self.handed_off,
            crashed_at: self.crashed_at,
            killed_since: self.killed_since,
        };
        match crate::qos::spawn_lane_thread(
            "aterm-vitals",
            crate::qos::Role::Background,
            move || {
                let _ended = ended;
                let mut source = source();
                source.install_assessment_wake(weak_wake);
                run(
                    &rx, &latest, &post, source, peaks, workers, &stopping, prior,
                );
            },
        ) {
            Ok(handle) => {
                self.tx = Some(tx);
                self.assessment_wake = Some(assessment_wake);
                self.sys = sys;
                self.thread = Some((handle, done));
            }
            Err(error) => {
                aterm_log::warn!("system vitals: the probe thread did not start: {error}");
            }
        }
    }

    /// Stops the probe thread on application exit and waits up to `bound` for it to write the
    /// peaks and the free history.
    pub(crate) fn stop_at_exit(&mut self, bound: Duration) -> bool {
        self.stopping.store(true, Ordering::Release);
        // The host's last tellings, for the stop's write of the closed-tab record; a full channel
        // leaves it the last request's.
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(Msg::Told(self.left_told.iter().copied().collect()));
        }
        self.assessment_wake = None;
        self.tx = None;
        self.jobs_owed = None;
        self.jobs_due = None;
        self.jobs_changed = false;
        self.next_due = None;
        if let Some(shared) = self.sys.take() {
            shared.uninstall();
        }
        let Some((handle, done)) = self.thread.take() else {
            return true;
        };
        match done.recv_timeout(bound) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = handle.join();
                true
            }
            Err(RecvTimeoutError::Timeout) => {
                aterm_log::warn!(
                    "system vitals: the probe thread did not stop within {bound:?}; \
                     the peaks and free history since the last write are lost"
                );
                false
            }
        }
    }

    /// Notes that the run before this one stopped on purpose, an update's or a relaunch's
    /// predecessor ([`Self::handed_off`]); it counts only before the probe thread starts.
    pub(crate) fn note_handed_off_start(&mut self) {
        self.handed_off = true;
    }

    /// Notes that the run before this one crashed at `crash_ms`, by its crash evidence
    /// ([`Self::crashed_at`]); it counts only before the probe thread starts.
    pub(crate) fn note_crash_at(&mut self, crash_ms: i64) {
        self.crashed_at = Some(crash_ms);
    }

    /// Notes that the run before this one was killed and launched at `launched_ms`
    /// ([`Self::killed_since`]); it counts only before the probe thread starts.
    pub(crate) fn note_killed_run_launch(&mut self, launched_ms: i64) {
        self.killed_since = Some(launched_ms);
    }

    /// Queues a special time-series event marker (crash, restart, sleep/wake, spike) for the next
    /// reading.
    pub(crate) fn note_strip_event(&mut self, event: aterm_vitals::StripEvent) {
        self.pending_events.push(event);
    }

    fn offer(&mut self, msg: Msg) -> bool {
        let Some(tx) = self.tx.as_ref() else {
            self.jobs_owed = None;
            self.jobs_due = None;
            self.next_due = None;
            if let Msg::Read(r) | Msg::Facts(r) = msg {
                self.pending_gaps.extend(r.note_gaps);
                self.pending_events.extend(r.note_events);
            }
            return false;
        };
        match tx.try_send(msg) {
            Ok(()) => true,
            Err(TrySendError::Full(msg)) => {
                if let Msg::Read(r) | Msg::Facts(r) = msg {
                    self.pending_gaps.extend(r.note_gaps);
                    self.pending_events.extend(r.note_events);
                }
                false
            }
            Err(TrySendError::Disconnected(msg)) => {
                if let Msg::Read(r) | Msg::Facts(r) = msg {
                    self.pending_gaps.extend(r.note_gaps);
                    self.pending_events.extend(r.note_events);
                }
                self.tx = None;
                self.jobs_owed = None;
                self.jobs_due = None;
                self.next_due = None;
                false
            }
        }
    }
}

trait Source: Sized {
    fn reading(&mut self, req: &Request) -> Snapshot;
    fn process_receipt(&self) -> Option<Arc<crate::vitals_host::memory::Receipt>> {
        None
    }
    /// Where a memory starvation edge's line goes before its reading is handed over
    /// ([`crate::memory_starving::Journal`]): aterm.log's, which a stand-in replaces to see when
    /// the loop writes it.
    fn starving_sink(&self) -> &dyn crate::memory_starving::Sink {
        crate::logging::starving_sink()
    }
    /// How long the thread waits for an ask before it reads memory on its own while that memory
    /// is tight ([`MEMORY_WATCH_EVERY`]).
    fn memory_watch_every(&self) -> Duration {
        MEMORY_WATCH_EVERY
    }
    fn model(&mut self) -> &mut aterm_vitals::Vitals;
    fn machine(&self) -> Option<&Machine> {
        None
    }
    fn machine_at(&self) -> Option<Instant> {
        None
    }
    fn observed_at(&self) -> Option<memory::ObservedAt> {
        None
    }
    fn jobs_at(&self) -> Option<Instant> {
        None
    }
    fn left_at(&self) -> Option<Instant> {
        None
    }
    fn job_words(&self) -> Option<Vec<WordSeen>> {
        None
    }
    fn has_name_replies(&self) -> bool {
        false
    }

    fn jobs_continuous(&self) -> bool {
        true
    }
    fn install_assessment_wake(&mut self, _wake: std::sync::Weak<SyncSender<Msg>>) {}
    /// A remaining boot target; no deadline once both observations are recorded.
    fn boot_after(&mut self) -> Option<Duration> {
        None
    }
    fn assessment_work(&self) -> assessment::Work {
        assessment::Work::None
    }
    fn schedule_names(&mut self, names: Vec<crate::sys_naming::Request>) -> Vec<NameReply> {
        prepare_names(self, names)
    }
    fn publish_names(&mut self, replies: Vec<NameReply>) {
        finish_names(self, replies);
    }
    fn split_cold(&self) -> bool {
        false
    }
    /// Whether a query that prints hardware arms a pull beginning now ([`HARDWARE_ARMED`]).
    fn hardware_armed(&self) -> bool {
        false
    }
    /// Whether no CPU Stats sample is warm for a pull, so a pull that prints hardware reads its
    /// pair [`COLD_PAIR`] apart and the second sample's delta is the first's.
    fn hardware_cold(&self) -> bool {
        false
    }
    /// Whether a `sys jobs` waiting on a pull arms the stuck engine for its reading
    /// ([`JOBS_ARMED`]).
    fn jobs_armed(&self) -> bool {
        false
    }
    /// Whether a query that prints each process's scheduling arms a pull beginning now
    /// ([`SCHEDULING_ARMED`]).
    fn scheduling_armed(&self) -> bool {
        false
    }
    /// Whether a query that prints aterm's own thread CPU arms a pull beginning now
    /// ([`SELF_CPU_ARMED`]).
    fn self_cpu_armed(&self) -> bool {
        false
    }
    /// Whether the thread's last reading read no cut of aterm's own threads within
    /// [`COLD_AFTER`], so a pull that prints them reads its pair [`COLD_PAIR`] apart and the
    /// second cut's changes are from the first: a reading that reads none clears the model's
    /// continuity ([`aterm_vitals::self_cpu::OwnCpuState::absorb`]).
    fn self_cpu_cold(&self) -> bool {
        false
    }
    /// The host's tellings, by closed tab's key ([`Request::left_told`]): the leftover pass keeps
    /// them told ([`crate::vitals_left::LeftPass::keep_told`]).
    fn note_told(&mut self, _told: &[u64]) {}
    /// The thread's stop, after its last reading, with the host's last tellings: the leftover pass
    /// closes the tabs it holds at it ([`crate::vitals_left::LeftPass::at_stop`]).
    fn at_stop(&mut self, _told: &[u64]) {}
    /// After a reading is handed over and published, its post made: work a reading leaves off its
    /// own path, posting nothing to the main loop (A3 L1's hook, which A2's S7 lanes extend).
    /// `hidden`: no window in view shows the bars or a page that reads them
    /// ([`Request::in_view`]), whatever the reading read.
    fn after_publish(&mut self, _hidden: bool) {}
    fn capture_name(
        &self,
        _session: u64,
        _target: &aterm_vitals::sys::query::NameTarget,
    ) -> Result<aterm_vitals::jobs::NamingTicket, aterm_vitals::jobs::NamingError> {
        Err(aterm_vitals::jobs::NamingError::Unread)
    }
    fn apply_name(
        &mut self,
        _ticket: &aterm_vitals::jobs::NamingTicket,
        _word: aterm_vitals::jobs::TierB,
    ) -> Result<(), aterm_vitals::jobs::NamingError> {
        Err(aterm_vitals::jobs::NamingError::Unread)
    }
    fn name_is_shown(
        &self,
        _ticket: &aterm_vitals::jobs::NamingTicket,
        _word: aterm_vitals::jobs::TierB,
    ) -> bool {
        false
    }
}

impl Source for Reader {
    fn reading(&mut self, req: &Request) -> Snapshot {
        if req.machine_only {
            self.read_machine(req)
        } else {
            self.read(req)
        }
    }

    fn after_publish(&mut self, hidden: bool) {
        (self.net.after_publish)(hidden);
    }

    fn model(&mut self) -> &mut aterm_vitals::Vitals {
        &mut self.vitals
    }

    fn machine(&self) -> Option<&Machine> {
        Some(&self.machine)
    }

    fn machine_at(&self) -> Option<Instant> {
        self.machine_observed_at
    }
    fn observed_at(&self) -> Option<memory::ObservedAt> {
        Some(self.observed_at)
    }
    fn process_receipt(&self) -> Option<Arc<crate::vitals_host::memory::Receipt>> {
        self.process_receipt.clone()
    }
    fn jobs_at(&self) -> Option<Instant> {
        self.assessment.reading().jobs_at
    }

    fn split_cold(&self) -> bool {
        let now = Instant::now();
        self.split.cold(now) || self.gpu.cold(now)
    }

    fn hardware_armed(&self) -> bool {
        self.hardware_armed.armed()
    }

    fn hardware_cold(&self) -> bool {
        (self.hardware_read.cold)(&self.hardware, HardwareDemand::Pull.period(Duration::ZERO))
    }

    fn jobs_armed(&self) -> bool {
        self.jobs_armed.armed()
    }

    fn scheduling_armed(&self) -> bool {
        self.scheduling_armed.armed()
    }

    fn self_cpu_armed(&self) -> bool {
        self.self_cpu_armed.armed()
    }

    fn self_cpu_cold(&self) -> bool {
        let now = (self.clock)();
        self.self_cpu_last
            .is_none_or(|at| now.saturating_duration_since(at) > COLD_AFTER)
    }

    fn note_told(&mut self, told: &[u64]) {
        self.assessment
            .note_told(told, u64::try_from(unix_ms_now()).unwrap_or(0));
    }

    fn at_stop(&mut self, told: &[u64]) {
        self.assessment
            .at_stop(told, u64::try_from(unix_ms_now()).unwrap_or(0));
    }

    fn left_at(&self) -> Option<Instant> {
        self.assessment.reading().left_at
    }
    fn jobs_continuous(&self) -> bool {
        self.assessment.continuous()
    }
    fn job_words(&self) -> Option<Vec<WordSeen>> {
        Some(self.assessment.words())
    }
    fn has_name_replies(&self) -> bool {
        self.assessment.has_name_replies()
    }

    fn install_assessment_wake(&mut self, wake: std::sync::Weak<SyncSender<Msg>>) {
        let boot_wake = wake.clone();
        self.boot.set_wake(Arc::new(move || {
            if let Some(tx) = boot_wake.upgrade() {
                let _ = tx.try_send(Msg::BootReady);
            }
        }));
        self.assessment.install_wake(wake);
    }
    fn boot_after(&mut self) -> Option<Duration> {
        self.boot.after()
    }
    fn assessment_work(&self) -> assessment::Work {
        self.assessment.work()
    }
    fn schedule_names(&mut self, names: Vec<crate::sys_naming::Request>) -> Vec<NameReply> {
        self.assessment.schedule_names(names)
    }
    fn publish_names(&mut self, replies: Vec<NameReply>) {
        self.assessment.publish_names(replies);
    }
}

#[derive(Default)]
struct Asked {
    read: bool,
    /// A [`Msg::Read`] came earlier in this drain: a later one keeps what it asked.
    asked_read: bool,
    pull: bool,
    memory: bool,
    assessment_ready: bool,
    facts: bool,
    cpu_policy: Option<aterm_vitals::sys::json::Jv>,
    names: Vec<crate::sys_naming::Request>,
}

impl Asked {
    /// Takes one message into the drain's request. Only a [`Msg::Read`] changes what the reading
    /// reads, and a later one in the same drain never drops what an earlier one asked: a whole
    /// reading, and the engine's look (a close's ask beside a hidden window's machine-only one).
    fn take(&mut self, msg: Msg, req: &mut Request) {
        match msg {
            Msg::Read(mut r) => {
                if std::mem::replace(&mut self.asked_read, true) {
                    r.jobs_now |= req.jobs_now;
                    r.machine_only &= req.machine_only;
                }
                let mut gaps = std::mem::take(&mut req.note_gaps);
                let mut events = std::mem::take(&mut req.note_events);
                gaps.append(&mut r.note_gaps);
                events.append(&mut r.note_events);
                *req = *r;
                req.note_gaps = gaps;
                req.note_events = events;
                self.facts = true;
                self.read = true;
            }
            Msg::Facts(mut r) => {
                let (cadence_ms, machine_only, jobs_now) =
                    (req.cadence_ms, req.machine_only, req.jobs_now);
                let mut gaps = std::mem::take(&mut req.note_gaps);
                let mut events = std::mem::take(&mut req.note_events);
                gaps.append(&mut r.note_gaps);
                events.append(&mut r.note_events);
                r.note_gaps = gaps;
                r.note_events = events;
                *req = *r;
                req.cadence_ms = cadence_ms;
                req.machine_only = machine_only;
                req.jobs_now = jobs_now;
                self.facts = true;
            }
            Msg::Pull => self.pull = true,
            Msg::MemoryPull => self.memory = true,
            Msg::AssessmentReady => self.assessment_ready = true,
            Msg::BootReady => {}
            Msg::CpuPolicy(report) => {
                self.cpu_policy = Some(report);
                self.read = true;
            }
            Msg::Name(request) => {
                self.names.push(*request);
                self.pull = true;
            }
            Msg::Told(told) => req.left_told = told,
        }
    }
}

type NameReply = (
    crate::sys_naming::Request,
    Result<crate::sys_naming::Answer, crate::sys_naming::Error>,
);

/// Engine calls and the nonblocking peer check stay under the active lease; process readings
/// never do.
fn prepare_names<S: Source>(
    source: &mut S,
    names: Vec<crate::sys_naming::Request>,
) -> Vec<NameReply> {
    prepare_names_on(source, names, S::capture_name, S::apply_name)
}

fn prepare_names_on<T>(
    engine: &mut T,
    names: Vec<crate::sys_naming::Request>,
    capture: fn(
        &T,
        u64,
        &aterm_vitals::sys::query::NameTarget,
    ) -> Result<aterm_vitals::jobs::NamingTicket, aterm_vitals::jobs::NamingError>,
    apply: fn(
        &mut T,
        &aterm_vitals::jobs::NamingTicket,
        aterm_vitals::jobs::TierB,
    ) -> Result<(), aterm_vitals::jobs::NamingError>,
) -> Vec<NameReply> {
    use crate::sys_naming::{Answer, Operation};
    names
        .into_iter()
        .map(|request| {
            let answer = request.run_active(|| match &request.operation {
                Operation::Capture { session, target } => {
                    capture(engine, *session, target).map(Answer::Captured)
                }
                Operation::Apply { ticket, word } => {
                    apply(engine, ticket, *word)?;
                    Ok(Answer::Applied {
                        job: ticket.job().to_owned(),
                        word: *word,
                    })
                }
            });
            (request, answer)
        })
        .collect()
}

fn validate_names_on<T>(
    engine: &T,
    replies: Vec<NameReply>,
    shown: fn(&T, &aterm_vitals::jobs::NamingTicket, aterm_vitals::jobs::TierB) -> bool,
) -> Vec<NameReply> {
    use crate::sys_naming::Operation;
    replies
        .into_iter()
        .map(|(request, mut answer)| {
            if answer.is_ok()
                && let Operation::Apply { ticket, word } = &request.operation
            {
                answer = request.run_active(|| {
                    if shown(engine, ticket, *word) {
                        Ok(crate::sys_naming::Answer::Applied {
                            job: ticket.job().to_owned(),
                            word: *word,
                        })
                    } else {
                        Err(aterm_vitals::jobs::NamingError::Stale)
                    }
                });
            }
            (request, answer)
        })
        .collect()
}

fn respond_names(replies: Vec<NameReply>) {
    for (request, answer) in replies {
        request.respond(answer);
    }
}

/// Replies follow publication; an applied name must still carry its identity and episode.
fn finish_names<S: Source>(source: &S, replies: Vec<NameReply>) {
    respond_names(validate_names_on(source, replies, S::name_is_shown));
}

#[allow(
    clippy::too_many_arguments,
    reason = "the thread's channels, its source and files, and what the host told it at its start"
)]
fn run(
    rx: &Receiver<Msg>,
    latest: &Mutex<Handover>,
    post: &dyn Fn() -> bool,
    mut source: impl Source,
    peaks: Option<PathBuf>,
    workers: Option<(&'static crate::sys_host::Shared, SyncSender<Msg>)>,
    stopping: &AtomicBool,
    prior: PriorRun,
) {
    let sys = workers.map(|(shared, pulls)| {
        shared.install(pulls);
        if stopping.load(Ordering::Acquire) {
            shared.uninstall();
        }
        shared
    });
    let free = peaks.as_deref().map(|p| p.with_file_name(FREE_FILE));
    let weeks = peaks.as_deref().map(|p| p.with_file_name(WEEKS_FILE));
    let spotlight = peaks.as_deref().map(|p| p.with_file_name(SPOTLIGHT_FILE));
    let home = peaks
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let report = peaks.as_deref().map(|p| p.with_file_name(CPU_REPORT_FILE));
    let mut writer = StripLock::take(peaks.as_deref().map(|p| p.with_file_name(STRIP_LOCK)));
    let open_at = Instant::now();
    let open_ms = unix_ms_now();
    let (mut peaks, restored) = PeakFile::open(peaks, open_at);
    source.model().restore_peaks(restored);
    let (mut free, kept) = FreeFile::open(free, open_at, open_ms);
    source.model().restore_free_history(kept);
    let (mut weeks, kept_weeks) = WeeksFile::open(weeks, open_at, open_ms);
    source.model().restore_free_weeks(kept_weeks);
    let (mut spotlight, kept_spotlight) = SpotlightFile::open(spotlight, open_at, open_ms);
    source.model().restore_spotlight_history(kept_spotlight);
    let mut strip = restore_strip(
        home.as_deref(),
        open_at,
        open_ms,
        prior,
        &mut writer,
        source.model(),
    );
    let (mut report, prior_report) = CpuReportFile::open(report, open_at, open_ms);
    source.model().restore_cpu_report(prior_report);
    let mut current_report = None;
    let no_machine = Machine::default();
    let mut req = Request::default();
    let mut last_whole: Option<Instant> = None;
    let mut journal = crate::memory_starving::Journal::default();
    // When the newest reading ended, while its memory keeps the thread reading on its own.
    let mut watch_from: Option<Instant> = None;
    loop {
        // Initial layout facts precede the first scheduled capture. Completion notices wake
        // this worker after restore; no polling timer or UI event is needed for persistence.
        // The memory watch is the one timer of its own, and only while memory is tight.
        let boot = (req.self_pid != 0).then(|| source.boot_after()).flatten();
        let watch = watch_from.map(|at| source.memory_watch_every().saturating_sub(at.elapsed()));
        let first = match boot.into_iter().chain(watch).min() {
            Some(after) => match rx.recv_timeout(after) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            },
        };
        // A boot target due no later than the watch is the checkpoint's whole reading, which
        // judges memory as well.
        let checkpoint = first.is_none() && boot.is_some_and(|b| watch.is_none_or(|w| b <= w));
        let watching = first.is_none() && !checkpoint;
        let mut asked = Asked::default();
        if let Some(first) = first {
            asked.take(first, &mut req);
        }
        while let Ok(more) = rx.try_recv() {
            asked.take(more, &mut req);
        }
        match source.assessment_work() {
            assessment::Work::Input => asked.pull = true,
            assessment::Work::Publish => asked.assessment_ready = true,
            assessment::Work::None => {}
        }
        let assessment_only = asked.assessment_ready && !(asked.read || asked.pull || asked.memory);
        if assessment_only {
            asked.pull = true;
        }
        // A quit's last tellings wait for the stop's one write (`Source::at_stop`).
        if !stopping.load(Ordering::Acquire) {
            source.note_told(&req.left_told);
        }
        let policy_changed = asked.cpu_policy.is_some();
        if let Some(policy) = asked.cpu_policy.take() {
            source.model().record_cpu_policy(policy);
        }
        if asked.facts
            && let Some(sys) = sys
        {
            sys.set_facts(req.layout.clone());
        }
        if !(asked.read || asked.pull || asked.memory || checkpoint || watching) {
            continue;
        }
        if req.self_pid == 0 {
            req.self_pid = std::process::id();
        }
        let began = Instant::now();
        crate::metrics::note_reading(crate::metrics::ReadingStep::Began);
        req.assessment_only = assessment_only;
        let mut snap = if asked.pull {
            pull(&mut source, &mut req, last_whole, !asked.names.is_empty())
        } else if asked.memory {
            let mut memory_req = req.clone();
            memory_req.machine_only = false;
            memory_req.assessment_only = false;
            memory_req.pull = true;
            source.reading(&memory_req)
        } else if checkpoint {
            let mut checkpoint_req = req.clone();
            checkpoint_req.machine_only = false;
            checkpoint_req.assessment_only = false;
            source.reading(&checkpoint_req)
        } else if watching {
            // No one asked: the machine's counters alone, as a hidden deck reads them, with no
            // process sweep, which stalls first on a starving machine.
            let mut watch_req = req.clone();
            watch_req.machine_only = true;
            watch_req.assessment_only = false;
            watch_req.jobs_now = false;
            watch_req.in_view = false;
            watch_req.cadence_ms = duration_ms(source.memory_watch_every());
            source.reading(&watch_req)
        } else {
            source.reading(&req)
        };
        req.note_gaps.clear();
        req.note_events.clear();
        let replies = source.schedule_names(asked.names);
        if replies
            .iter()
            .any(|(_, answer)| matches!(answer, Ok(crate::sys_naming::Answer::Applied { .. })))
        {
            let previous = snap;
            snap = pull(&mut source, &mut req, Some(Instant::now()), true);
            keep_warm_events(previous, &mut snap);
        }
        if let Some(record) = cpu_report_record(&snap) {
            current_report = Some(record);
        }
        journal.note(&snap, source.starving_sink());
        watch_from = aterm_vitals::starving::watching(&snap).then(Instant::now);
        let snap = Arc::new(snap);
        if !snap.machine_only {
            last_whole = Some(Instant::now());
        }
        let words = source
            .job_words()
            .unwrap_or_else(|| req.ttys.iter().map(TtyWord::seen).collect());
        let leases = req.allocation.then(|| {
            Arc::new(crate::native_allocation::ObservedCpu {
                value: if cfg!(test) {
                    aterm_capacity::CpuObservation::Absent
                } else {
                    aterm_capacity::CpuObservation::read(
                        aterm_capacity::ledger::user_dir().as_deref(),
                    )
                },
                at: Instant::now(),
            })
        });
        let hidden = !req.in_view || watching;
        let observed_at = source.observed_at().unwrap_or(memory::ObservedAt {
            instant: source.machine_at().unwrap_or(began),
            continuous_ns: None,
        });
        let jobs_moved = hand_over_allocation(
            latest,
            &snap,
            words,
            observed_at.instant,
            leases,
            source.jobs_at(),
            source.jobs_continuous(),
        );
        crate::metrics::note_reading(crate::metrics::ReadingStep::Handed);
        if let Some(sys) = sys {
            sys.publish(
                snap,
                source.machine().unwrap_or(&no_machine),
                observed_at,
                source.jobs_at(),
                source.left_at(),
                source.jobs_continuous(),
                source.process_receipt(),
            );
        }
        if (asked.read
            || asked.assessment_ready
            || source.has_name_replies()
            || jobs_moved
            || !replies.is_empty())
            && !post()
        {
            for (request, _) in replies {
                request.respond(Err(crate::sys_naming::Error::Canceled));
            }
            // The event loop is gone, at a quit or as a main thread's panic unwinds out of it. The
            // thread takes no more readings (a cold one begun now could outlast `stop_at_exit`'s
            // bound), takes the workers' sender back so they answer from what they hold, and waits
            // for the host to hang up: `stop_at_exit` at a quit, or its drop in the unwind, when
            // `stopping` says which it was.
            if let Some(sys) = sys {
                sys.uninstall();
            }
            while let Ok(msg) = rx.recv() {
                match msg {
                    Msg::Told(told) => req.left_told = told,
                    Msg::Read(r) | Msg::Facts(r) => req.left_told = r.left_told,
                    Msg::Pull
                    | Msg::MemoryPull
                    | Msg::AssessmentReady
                    | Msg::BootReady
                    | Msg::CpuPolicy(_)
                    | Msg::Name(_) => {}
                }
            }
            break;
        }
        source.publish_names(replies);
        source.after_publish(hidden);
        let kept_at = Instant::now();
        if current_report.is_some() {
            report.keep(&current_report, kept_at, policy_changed);
        }
        peaks.keep_owned(source.model().peaks(), kept_at, false, &mut writer);
        free.keep(source.model().free_history(), kept_at, false);
        weeks.keep(source.model().free_weeks(), kept_at, false);
        spotlight.keep(source.model().spotlight_history(), kept_at, false);
        strip.keep(source.model(), kept_at, &mut writer);
    }
    let stop_at = Instant::now();
    let quit = stopping.load(Ordering::Acquire);
    if current_report.is_some() {
        report.keep(&current_report, stop_at, true);
    }
    peaks.keep_owned(source.model().peaks(), stop_at, true, &mut writer);
    free.keep(source.model().free_history(), stop_at, true);
    weeks.keep(source.model().free_weeks(), stop_at, true);
    spotlight.keep(source.model().spotlight_history(), stop_at, true);
    strip.stop(source.model(), quit, stop_at, &mut writer);
    // The lock goes only after the stop's write, so the aterm that takes it next writes after it.
    drop(writer);
    // After the files above, so a slow write of the closed-tab record delays none of them.
    source.at_stop(&req.left_told);
}

/// A worker's pull: a whole reading, or a cold one's pair [`COLD_PAIR`] apart, with the thread's
/// last request. Each reading reads every terminal word again just before it
/// ([`Request::reread_words`]): the thread may hold its request from long before, and a write
/// that moved a screen since a word was read (an agent's last redraw of a turn, or one inside the
/// pair) leaves that word stamped before the output, naming no wait. `req` keeps the words the
/// last reading read with, which the view is handed back with. The pull keeps the engine's
/// cadence, whatever that request asked of it, but for its last reading while a `sys jobs`
/// arms the engine ([`Source::jobs_armed`]): one look, never the warm one's too. Its readings'
/// cadence, [`COLD_PAIR`] or [`COLD_AFTER`], is the gap rule's alone: each stands for an instant
/// in the rings ([`aterm_vitals::model::Sample::pull`]), so the deck's reading after it keeps its
/// whole slot and its over time.
fn pull(
    source: &mut impl Source,
    req: &mut Request,
    last_whole: Option<Instant>,
    force_jobs: bool,
) -> Snapshot {
    if let Some(t) = last_whole {
        std::thread::sleep(MIN_WINDOW.saturating_sub(t.elapsed()));
    }
    let hardware = source.hardware_armed();
    let self_cpu = source.self_cpu_armed();
    let cold = last_whole.is_none_or(|t| t.elapsed() > COLD_AFTER)
        || source.split_cold()
        || (hardware && source.hardware_cold())
        || (self_cpu && source.self_cpu_cold());
    let mut pull = req.clone();
    pull.machine_only = false;
    pull.jobs_now = false;
    pull.pull = true;
    pull.hardware = hardware;
    pull.scheduling = source.scheduling_armed();
    pull.self_cpu = self_cpu;
    pull.cadence_ms = duration_ms(if cold { COLD_PAIR } else { COLD_AFTER }).max(req.cadence_ms);
    let warm = cold.then(|| {
        pull.reread_words();
        let warm = source.reading(&pull);
        pull.note_gaps.clear();
        pull.note_events.clear();
        std::thread::sleep(COLD_PAIR);
        warm
    });
    pull.reread_words();
    pull.jobs_now = force_jobs || source.jobs_armed();
    let mut snap = source.reading(&pull);
    if let Some(warm) = warm {
        keep_warm_events(warm, &mut snap);
    }
    req.ttys = pull.ttys;
    snap
}

/// The process group leader of the agent `publication` names, where it names one reading the
/// screen ([`crate::harness_host::identified_agent`]) and a group: the leftover pass reads its unique
/// id as the tab's agent start ([`crate::vitals_left`]).
fn agent_leader(publication: &crate::session_timeline::AgentPublication) -> Option<u32> {
    crate::harness_host::identified_agent(publication)?;
    u32::try_from(publication.program_pgid)
        .ok()
        .filter(|&pgid| pgid > 1)
}

fn duration_ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

fn keep_warm_events(warm: Snapshot, snap: &mut Snapshot) {
    snap.events_lost = snap.events_lost.saturating_add(warm.events_lost);
    let mut events = warm.events;
    events.append(&mut snap.events);
    snap.events = events;
}

#[cfg(test)]
fn hand_over(
    latest: &Mutex<Handover>,
    snap: &Arc<Snapshot>,
    words: Vec<WordSeen>,
    began: Instant,
) -> bool {
    hand_over_allocation(latest, snap, words, began, None, None, true)
}

#[cfg(all(test, unix))]
impl VitalsHost {
    /// A test's stand-in reader ([`Self::attach_test_reader`]) hands over the reading numbered
    /// `seq`, begun at `began`, as the probe thread does before it posts `Wake::Vitals`.
    pub(crate) fn land_for_test(&self, seq: u64, began: Instant) {
        let snap = Arc::new(Snapshot {
            seq,
            ..Snapshot::default()
        });
        let _ = hand_over(&self.latest, &snap, Vec::new(), began);
    }
}

fn hand_over_allocation(
    latest: &Mutex<Handover>,
    snap: &Arc<Snapshot>,
    words: Vec<WordSeen>,
    began: Instant,
    leases: Option<Arc<crate::native_allocation::ObservedCpu>>,
    jobs_at: Option<Instant>,
    jobs_continuous: bool,
) -> bool {
    let events: Vec<_> = snap
        .events
        .iter()
        .filter(|e| matches!(e.kind, aterm_vitals::events::EventKind::Job { .. }))
        .cloned()
        .collect();
    let health: Vec<_> = snap
        .events
        .iter()
        .filter(|e| matches!(e.kind, aterm_vitals::events::EventKind::Starving(_)))
        .cloned()
        .collect();
    let mut h = latest.lock().unwrap_or_else(PoisonError::into_inner);
    let same_view = match (
        h.snap.as_ref().and_then(|s| s.jobs.as_ref()),
        snap.jobs.as_ref(),
    ) {
        (Some(held), Some(new)) => Arc::ptr_eq(held, new),
        _ => false,
    };
    let jobs_began = jobs_at.unwrap_or(began);
    h.began = Some(match h.began {
        Some(held) if same_view => held.min(jobs_began),
        _ => jobs_began,
    });
    h.snap = Some(Arc::clone(snap));
    h.sampled_at = Some(began);
    h.jobs_continuous = jobs_continuous;
    h.cpu_leases = leases;
    h.words = words;
    let moved = !events.is_empty() || !health.is_empty();
    h.job_events.extend(events);
    if h.job_events.len() > JOB_EVENTS_CAP {
        let over = h.job_events.len() - JOB_EVENTS_CAP;
        h.job_events.drain(..over);
    }
    h.health_events.extend(health);
    if h.health_events.len() > HEALTH_EVENTS_CAP {
        let over = h.health_events.len() - HEALTH_EVENTS_CAP;
        h.health_events.drain(..over);
    }
    moved
}

/// The thermal level and Low Power Mode probe.
pub(crate) type ThermalProbe = fn() -> Option<Thermal>;

/// The CPU Stats reader, injected so headless tests open no native service: a sample after the
/// reading's machine and process counters, under its demands' gap, whether a sample now would
/// read `warming` under a gap ([`aterm_sysprobe::cpu_hardware::Probe::cold`]), and whether a
/// sample costs nothing because no source answers ([`aterm_sysprobe::cpu_hardware::SOURCE`]).
#[derive(Clone, Copy)]
struct HardwareProbe {
    sample: fn(
        &mut aterm_sysprobe::cpu_hardware::Probe,
        &Sample,
        Duration,
    ) -> aterm_sysprobe::cpu_hardware::Sample,
    cold: fn(&aterm_sysprobe::cpu_hardware::Probe, Duration) -> bool,
    free: bool,
}

impl HardwareProbe {
    /// The machine's IOReport reader.
    const NATIVE: Self = Self {
        sample: native_hardware,
        cold: aterm_sysprobe::cpu_hardware::Probe::cold,
        free: !aterm_sysprobe::cpu_hardware::SOURCE,
    };
    /// A unit test's: every sample unavailable, never cold, and a source that could answer, as
    /// on macOS.
    const INERT: Self = Self {
        sample: |_, _, _| aterm_sysprobe::cpu_hardware::Sample::unavailable("not_sampled"),
        cold: |_, _| false,
        free: false,
    };
}

fn native_hardware(
    probe: &mut aterm_sysprobe::cpu_hardware::Probe,
    _: &Sample,
    gap: Duration,
) -> aterm_sysprobe::cpu_hardware::Sample {
    probe.sample(gap)
}

/// What asks a whole reading for a CPU Stats sample (A2 §6.4). The deck's and a hidden window's
/// readings ask none: the deck draws nothing from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HardwareDemand {
    /// The CPU lens is open, whose Hardware & limits section prints it.
    Lens,
    /// The E-busy, P-idle recorder is pending ([`aterm_vitals::Vitals::cpu_recorder_pending`]),
    /// whose frames copy it.
    Recorder,
    /// A pull that prints it ([`Request::hardware`]).
    Pull,
    /// No source answers here (off macOS): the sample reads nothing and says `unsupported`, so
    /// every whole reading carries it and `sys cores` finds it held instead of pulling for it.
    Free,
}

impl HardwareDemand {
    /// How often it samples at the reading cadence `cadence`: the lens every
    /// [`LENS_HARDWARE_PERIOD`], the recorder every reading but never more often than its frames
    /// stand apart, and a pull at every pull reading, its sample warm within [`COLD_AFTER`], past
    /// which a pull already reads twice.
    fn period(self, cadence: Duration) -> Duration {
        match self {
            Self::Lens => LENS_HARDWARE_PERIOD,
            Self::Recorder => cadence.max(RECORDER_HARDWARE_FLOOR),
            Self::Pull => COLD_AFTER,
            Self::Free => Duration::ZERO,
        }
    }

    /// The gap rule's threshold: a sample more than 2.5 periods after the previous one would
    /// average over the gap, so it reads `warming` and starts the next interval.
    fn gap(self, cadence: Duration) -> Duration {
        self.period(cadence).saturating_mul(5) / 2
    }
}

/// The demands on whole reading `req`'s CPU Stats sample: the CPU lens open, the recorder
/// pending (`recorder`), a pull a hardware query armed, and a source that costs nothing (`free`).
fn hardware_demands(req: &Request, recorder: bool, free: bool) -> Vec<HardwareDemand> {
    [
        req.lenses
            .contains(&aterm_vitals::Resource::Cpu)
            .then_some(HardwareDemand::Lens),
        recorder.then_some(HardwareDemand::Recorder),
        req.hardware.then_some(HardwareDemand::Pull),
        free.then_some(HardwareDemand::Free),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// What a reading does about CPU Stats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HardwarePlan {
    /// No demand: the reading carries none.
    None,
    /// Demands stand but none is due: the reading carries the last sample.
    Carry,
    /// A sample, under this gap: the widest of the demands'.
    Sample(Duration),
}

/// The plan for a reading at `now` at cadence `cadence` under `demands`, the last sample taken
/// at `last`. A pull is due at every pull reading, a free source at every reading; the lens and
/// the recorder once their period has passed, less half a reading's slack, so a lens reading
/// every 500 ms samples every other.
fn hardware_plan(
    demands: &[HardwareDemand],
    cadence: Duration,
    last: Option<Instant>,
    now: Instant,
) -> HardwarePlan {
    let Some(gap) = demands.iter().map(|d| d.gap(cadence)).max() else {
        return HardwarePlan::None;
    };
    let since = last.map(|t| now.saturating_duration_since(t));
    let due = demands.iter().any(|d| {
        *d == HardwareDemand::Pull || since.is_none_or(|s| s + cadence / 2 >= d.period(cadence))
    });
    if due {
        HardwarePlan::Sample(gap)
    } else {
        HardwarePlan::Carry
    }
}

fn hardware_sample(s: aterm_sysprobe::cpu_hardware::Sample) -> aterm_vitals::hardware::Sample {
    use aterm_vitals::hardware as h;
    h::Sample {
        captured_unix_ms: s.captured_unix_ms,
        collection_span_ns: s.collection_span_ns,
        window_ns: s.window_ns,
        observation_span_ns: s.observation_span_ns,
        channels: s.channels.map(|rows| {
            rows.into_iter()
                .map(|c| h::Channel {
                    raw_elements: c.raw_elements,
                    name: c.name,
                    subgroup: c.subgroup,
                    unit: c.unit,
                    states: c
                        .states
                        .into_iter()
                        .map(|s| h::State {
                            name: s.name,
                            ticks: s.ticks,
                        })
                        .collect(),
                    active: c.active,
                    active_mhz: c.active_mhz,
                    frequency_source: c.frequency_source,
                })
                .collect()
        }),
        controls: h::Controls {
            os_version: s.controls.os_version,
            kernel_release: s.controls.kernel_release,
            kernel_version: s.controls.kernel_version,
            online: s.controls.online,
            active_cpus: s.controls.active_cpus,
            requested_recommended_mask: s.controls.requested_recommended_mask,
            cluster_masks: s.controls.cluster_masks,
            power_limits: s.controls.power_limits.map(|p| h::PowerLimits {
                speed_percent: p.speed_percent,
                scheduler_percent: p.scheduler_percent,
                available_cpus: p.available_cpus,
            }),
            last_sleep_ms: s.controls.last_sleep_ms,
            last_wake_ms: s.controls.last_wake_ms,
            last_sleep_reason: s.controls.last_sleep_reason,
        },
    }
}

/// The heat chain's host readers (A4 §4): the documented thermal pressure level's notify watch,
/// checked every reading and read again only when it fires; the chip's clock range, read once;
/// and, where no coalitions read, the system's own watts readers (Linux's RAPL package energy;
/// NVML's GPU power rides the GPU reading). A system without one reads `None` for it, which the
/// model carries as not supported.
pub(crate) struct HeatReaders {
    pressure: Option<aterm_sysprobe::notify::Watch>,
    range: Option<Result<aterm_vitals::heat::ClockRange, &'static str>>,
    rapl: Option<aterm_sysprobe::rapl::Rapl>,
}

impl HeatReaders {
    /// A unit test's: no notify token, no registry read, no powercap read.
    pub(crate) const INERT: Self = Self {
        pressure: None,
        range: None,
        rapl: None,
    };

    /// This system's: on macOS the pressure watch and the power manager's tables, on Linux the
    /// package energy counters.
    pub(crate) fn native() -> Self {
        Self {
            pressure: cfg!(target_os = "macos").then(aterm_sysprobe::notify::thermal_pressure),
            range: aterm_sysprobe::cpu_hardware::dvfs_range().map(|r| {
                r.map(|r| aterm_vitals::heat::ClockRange {
                    p_mhz: r.p_mhz,
                    e_mhz: r.e_mhz,
                })
            }),
            rapl: cfg!(target_os = "linux")
                .then(|| aterm_sysprobe::rapl::Rapl::open(std::path::Path::new("/"))),
        }
    }

    /// The system's own watts at this reading: the packages' since the last reading (RAPL), and
    /// the GPU's board power from this reading's `gpu` (NVML).
    fn host_watts(
        &mut self,
        gpu: Option<&aterm_sysprobe::GpuReading>,
    ) -> aterm_vitals::heat::HostWatts {
        host_watts_of(
            self.rapl.as_mut().map(|r| r.watts(Instant::now())),
            gpu.and_then(|g| g.power_mw),
        )
    }

    /// The pressure level at this reading ([`pressure_of`]).
    fn pressure(&mut self) -> Option<Result<aterm_vitals::heat::ThermalPressure, &'static str>> {
        self.pressure
            .as_mut()
            .map(|watch| pressure_of(watch.state()))
    }
}

/// The system's own watts from what its readers answered: `rapl`, the packages' watts since the
/// last reading (`None` with no RAPL reader), and `gpu_power`, the GPU reading's board power
/// (`None` with no reader of it). A reader refused for good, or a runtime-suspended card, is not
/// supported, so it adds nothing to compute watts and never reads 0 W; a reader that ran and gave
/// nothing this reading reads unread.
fn host_watts_of(
    rapl: Option<Result<Option<f64>, aterm_sysprobe::Why>>,
    gpu_power: Option<Result<u32, aterm_sysprobe::Why>>,
) -> aterm_vitals::heat::HostWatts {
    use aterm_sysprobe::Why;
    use aterm_vitals::heat::NoHeat;
    let none = NoHeat::NotSupported("no_power_reader");
    let cpu = match rapl {
        None => Err(none),
        Some(Ok(Some(w))) => Ok(w),
        Some(Ok(None)) => Err(NoHeat::Unread("warming")),
        Some(Err(Why::Absent)) => Err(NoHeat::NotSupported("no_rapl")),
        Some(Err(Why::NeedsPrivilege(_))) => Err(NoHeat::NotSupported("rapl_needs_root")),
        Some(Err(_)) => Err(NoHeat::Unread("rapl_read")),
    };
    let gpu = match gpu_power {
        None => Err(none),
        Some(Ok(mw)) => Ok(f64::from(mw) / 1e3),
        // Runtime-suspended: not read, so as not to wake it; it adds nothing measured.
        Some(Err(Why::Asleep)) => Err(NoHeat::NotSupported("gpu_asleep")),
        Some(Err(Why::Unsupported)) => Err(NoHeat::NotSupported("nvml_power_unsupported")),
        Some(Err(Why::NeedsPrivilege(_))) => Err(NoHeat::NotSupported("nvml_needs_privilege")),
        Some(Err(_)) => Err(NoHeat::Unread("nvml_power")),
    };
    aterm_vitals::heat::HostWatts { cpu, gpu }
}

/// The pressure level from what its watch answered: a posted state outside the documented levels
/// reads unread (`pressure_level`), never a guessed level; a failed read keeps its reason.
fn pressure_of(
    read: Result<u64, &'static str>,
) -> Result<aterm_vitals::heat::ThermalPressure, &'static str> {
    read.and_then(|state| {
        aterm_vitals::heat::ThermalPressure::of_state(state).ok_or("pressure_level")
    })
}

fn platform_thermal() -> Option<Thermal> {
    #[cfg(target_os = "macos")]
    {
        aterm_sysprobe::process_thermal()
            .map(|reading| thermal_of(reading.state.raw(), reading.low_power))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Maps `NSProcessInfoThermalState` to the portable [`Thermal`] level.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn thermal_of(raw: isize, low_power: Option<bool>) -> Thermal {
    let level = match raw {
        i if i <= 0 => Throttle::None,
        1 => Throttle::Light,
        2 => Throttle::Heavy,
        _ => Throttle::Severe,
    };
    Thermal {
        level,
        raw: u8::try_from(raw.clamp(0, 255)).unwrap_or(0),
        low_power,
        exact: true,
    }
}

const COUNTER_THROTTLE_EVERY: Duration = Duration::from_secs(10);

fn counter_thermal(t: Result<aterm_sysprobe::Throttle, aterm_sysprobe::Why>) -> Option<Thermal> {
    let t = t.ok()?;
    let (level, raw) = match t.level {
        aterm_sysprobe::ThrottleLevel::Nominal => (Throttle::None, 0),
        aterm_sysprobe::ThrottleLevel::Light => (Throttle::Light, 1),
        aterm_sysprobe::ThrottleLevel::Heavy => (Throttle::Heavy, 2),
        aterm_sysprobe::ThrottleLevel::Severe => (Throttle::Severe, 3),
    };
    Some(Thermal {
        level,
        raw,
        low_power: None,
        exact: t.exact,
    })
}

const fn os_family() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "this OS"
    }
}

/// Whether a whole reading `req` at `now` is a split tick.
pub(crate) fn split_due(last: Option<Instant>, req: &Request, armed: bool, now: Instant) -> bool {
    let since = last.map(|t| now.saturating_duration_since(t));
    if req.pull {
        return armed && since.is_none_or(|s| s >= MIN_WINDOW);
    }
    if !req.deck || req.jobs_now || req.machine_only {
        return false;
    }
    let every = if req.cadence_ms <= duration_ms(crate::stats_lens::LENS_PERIOD) {
        SPLIT_EVERY_LENS
    } else {
        SPLIT_EVERY
    };
    let slack = Duration::from_millis(u64::from(req.cadence_ms) / 2);
    since.is_none_or(|s| s + slack >= every)
}

/// The coalition lane's readers (`KERNEL` in production, `INERT` in unit tests).
#[derive(Clone, Copy)]
pub(crate) struct CoalProbe {
    /// The launch self-check, asked of this process
    /// ([`aterm_sysprobe::coalitions::self_check`]).
    pub(crate) self_check: fn(u32) -> Result<u64, &'static str>,
    /// The process sweep, each process's coalition and pid version read in
    /// it where the options ask, the coalition kept by pid under that
    /// version in the ids passed ([`aterm_sysprobe::processes_keeping`]):
    /// the kernel's in every arm, so a test's stand-in can count what a
    /// reading asks.
    pub(crate) sweep: fn(
        aterm_sysprobe::SweepOpts,
        &mut aterm_sysprobe::coalitions::CoalitionIds,
    ) -> Option<aterm_sysprobe::Sweep>,
    /// Every coalition's counters ([`aterm_sysprobe::coalitions::coalitions`]), read at every
    /// reading while the lane is on: the start counter's, and on split ticks the split's too.
    pub(crate) coalitions: fn() -> Option<aterm_sysprobe::coalitions::CoalitionRead>,
    /// The start counter where no coalition lane reads: Linux's forks, from the reading's own
    /// core-ticks read of `/proc/stat` ([`aterm_sysprobe::Probe::forks`]).
    pub(crate) forks: fn(&aterm_sysprobe::Probe) -> Option<u64>,
    /// A process's short name and birth, unix ms, from its kernel record
    /// ([`aterm_sysprobe::kproc::record`]).
    pub(crate) birth: fn(u32) -> Option<(String, i64)>,
    /// Every process's, by pid, in one read of the table
    /// ([`aterm_sysprobe::kproc::records`]): cheaper than [`Self::birth`]
    /// past [`BIRTHS_AT_ONCE`] pids.
    pub(crate) births: fn() -> Option<Births>,
    /// A process's image and signature ([`aterm_sysprobe::identity::identity`]).
    pub(crate) identity: fn(u32) -> Option<aterm_sysprobe::identity::Identity>,
    /// A process's pid version ([`aterm_sysprobe::pid_version`]), which an
    /// exec moves: read again after a lead's identity, so a read an exec
    /// raced is not kept (the sweep reads each process's first).
    pub(crate) version: fn(u32) -> Option<i32>,
    /// The machine's timeline ([`aterm_sysprobe::timeline::timeline`]).
    pub(crate) timeline: fn() -> Option<aterm_sysprobe::timeline::Timeline>,
    /// When the machine last woke ([`aterm_sysprobe::timeline::waketime_ms`]).
    pub(crate) waketime_ms: fn() -> Option<i64>,
}

impl CoalProbe {
    /// The kernel's readers.
    pub(crate) const KERNEL: Self = Self {
        self_check: aterm_sysprobe::coalitions::self_check,
        sweep: aterm_sysprobe::processes_keeping,
        coalitions: aterm_sysprobe::coalitions::coalitions,
        forks: aterm_sysprobe::Probe::forks,
        birth: |pid| aterm_sysprobe::kproc::record(pid).and_then(name_and_birth),
        births: || {
            Some(
                aterm_sysprobe::kproc::records()?
                    .into_iter()
                    .filter_map(|r| Some((r.pid, name_and_birth(r)?)))
                    .collect(),
            )
        },
        identity: aterm_sysprobe::identity::identity,
        version: aterm_sysprobe::pid_version,
        timeline: aterm_sysprobe::timeline::timeline,
        waketime_ms: aterm_sysprobe::timeline::waketime_ms,
    };

    /// The inert arm: no OS lane, nothing read but the sweep.
    pub(crate) const INERT: Self = Self {
        self_check: |_| Err("unsupported"),
        sweep: aterm_sysprobe::processes_keeping,
        coalitions: || None,
        forks: |_| None,
        birth: |_| None,
        births: || None,
        identity: |_| None,
        version: |_| None,
        timeline: || None,
        waketime_ms: || None,
    };
}

/// How long a reading waits for its process reads ([`ProcsRead::Lane`]): under a pull's own wait
/// ([`crate::sys_host::PULL_WAIT`]), so a pull is answered from the reading it asked for even
/// when the sweep does not answer. A sweep reads in tens of ms; one past this is held in the
/// kernel (a process in the middle of an exec holds every read of it).
pub(crate) const PROCS_WAIT: Duration = Duration::from_millis(400);

/// The process sweep's reader ([`CoalProbe::sweep`]).
pub(crate) type SweepFn = fn(
    aterm_sysprobe::SweepOpts,
    &mut aterm_sysprobe::coalitions::CoalitionIds,
) -> Option<aterm_sysprobe::Sweep>;

/// What a reading asks of the process reads: a sweep (none for a machine-only reading) with the
/// coalition ids it keeps, and the pid lists' counts for real user `uid`.
pub(crate) struct ProcsAsk {
    sweep: Option<(SweepFn, aterm_sysprobe::SweepOpts)>,
    ids: aterm_sysprobe::coalitions::CoalitionIds,
    uid: u32,
    ownership: Option<OwnershipAsk>,
    memory: crate::vitals_host::memory::Probes,
    observed_at: memory::ObservedAt,
}

struct OwnershipAsk {
    images: ownership::Images,
    self_pid: u32,
    tabs: Vec<TabRoot>,
    identities: Vec<(u64, aterm_pty::ShellIdentity)>,
    epoch: Instant,
}

struct OwnershipAnswer {
    images: ownership::Images,
    tabs: Vec<TabRoot>,
    unproved: Vec<u64>,
    shared: Vec<aterm_vitals::model::SharedRoot>,
}

impl OwnershipAsk {
    fn read(mut self, sweep: &mut aterm_sysprobe::Sweep) -> OwnershipAnswer {
        let present = sweep.procs.iter().map(|p| p.pid).collect();
        let unproved =
            self.images
                .tab_roots(&mut self.tabs, &self.identities, &present, !sweep.truncated);
        let mut procs: Vec<_> = sweep
            .procs
            .iter()
            .map(|p| proc_sample(p, self.epoch))
            .collect();
        let shared = self.images.roots(self.self_pid, &mut procs, &self.tabs);
        // Metadata proofs can fill an unread generation. Counters and their read clocks remain
        // exactly the process sweep's; no ownership callback supplies measurements.
        for (raw, proved) in sweep.procs.iter_mut().zip(procs) {
            debug_assert_eq!(raw.pid, proved.pid);
            raw.start = proved.start;
            raw.version = proved.version;
        }
        OwnershipAnswer {
            images: self.images,
            tabs: self.tabs,
            unproved,
            shared,
        }
    }
}

/// What the process reads answered.
pub(crate) struct ProcsAnswer {
    sweep: Option<aterm_sysprobe::Sweep>,
    ids: aterm_sysprobe::coalitions::CoalitionIds,
    /// Every process and this user's, from the pid lists ([`aterm_sysprobe::pid_counts`]).
    pids: (Option<u64>, Option<u64>),
    ownership: Option<OwnershipAnswer>,
    memory: Option<crate::vitals_host::memory::Captured>,
}

/// The process reads, wherever they run: the sweep, then the pid counts.
fn read_procs(ask: ProcsAsk) -> ProcsAnswer {
    let mut ids = ask.ids;
    let memory = ask
        .sweep
        .is_some()
        .then(|| crate::vitals_host::memory::Pending::begin(ask.memory, ask.observed_at));
    let mut sweep = ask.sweep.and_then(|(sweep, opts)| sweep(opts, &mut ids));
    let memory = memory
        .zip(sweep.as_ref())
        .map(|(pending, sweep)| pending.finish(sweep));
    let ownership = sweep
        .as_mut()
        .zip(ask.ownership)
        .map(|(sweep, ownership)| ownership.read(sweep));
    ProcsAnswer {
        sweep,
        ids,
        pids: aterm_sysprobe::pid_counts(ask.uid),
        ownership,
        memory,
    }
}

/// What a reading's process reads gave ([`Reader::read_procs`]).
struct ProcReads {
    /// The sweep, where one was asked and answered.
    sweep: Option<aterm_sysprobe::Sweep>,
    /// Every process and this user's, from the pid lists.
    pids: (Option<u64>, Option<u64>),
    /// The lanes the reading found stalled.
    stalled: Vec<aterm_vitals::lanes::LaneStall>,
    ownership: Option<OwnershipAnswer>,
    memory: Option<crate::vitals_host::memory::Captured>,
}

/// Where a reading's process reads run. Every read of another process can sit in the kernel
/// for as long as something else takes (a process in the middle of an exec holds the table), so
/// the probe makes them on a lane ([`aterm_sysprobe::lane`]) and waits [`PROCS_WAIT`] at most:
/// a reading whose sweep did not answer publishes every other figure, names the lane stalled
/// and the call it is in, and asks nothing more of the lane until that call returns. A unit
/// test's reader reads inline, on the reading's thread, where its stand-in readers count.
pub(crate) enum ProcsRead {
    /// On the reading's own thread (a unit test's).
    Inline,
    /// On a lane, waited for `wait` at most.
    Lane {
        lane: aterm_sysprobe::lane::Lane<ProcsAsk, ProcsAnswer>,
        wait: Duration,
    },
}

impl ProcsRead {
    /// On a lane of the reader's own, waited for `wait` at most.
    pub(crate) const fn lane(wait: Duration) -> Self {
        Self::Lane {
            lane: aterm_sysprobe::lane::Lane::new(aterm_vitals::lanes::PROCS, read_procs),
            wait,
        }
    }

    /// The answer, or the stall that kept it (none where the lane cannot read).
    fn read(&self, ask: ProcsAsk) -> Result<ProcsAnswer, Option<aterm_sysprobe::lane::Stall>> {
        match self {
            Self::Inline => Ok(read_procs(ask)),
            Self::Lane { lane, wait } => match lane.ask_within(ask, *wait) {
                aterm_sysprobe::lane::Read::Answered { out, ended, .. } => {
                    if let Some(ended) = ended {
                        aterm_log::info!(
                            "system vitals: the {} lane's {} returned after {} ms",
                            ended.lane,
                            ended.call,
                            ended.for_ms
                        );
                    }
                    Ok(out)
                }
                aterm_sysprobe::lane::Read::Stalled(stall) => Err(Some(stall)),
                aterm_sysprobe::lane::Read::Off(why) => {
                    aterm_log::warn!("system vitals: the process reads are off: {why}");
                    Err(None)
                }
            },
        }
    }
}

/// The network lane's passive kernel readers (`KERNEL` in production, `INERT` for deterministic
/// unit tests).
#[derive(Clone, Copy)]
pub(crate) struct NetProbe {
    /// Interface state and RX/TX counters ([`aterm_sysprobe::net_links_into`]).
    pub(crate) links: fn(&mut Vec<u8>) -> Option<Vec<aterm_sysprobe::LinkReading>>,
    /// TCP socket states and passive smoothed RTT ([`aterm_sysprobe::tcp_summary_into`]).
    pub(crate) tcp: fn(&mut Vec<u8>) -> Option<aterm_sysprobe::TcpSummary>,
    /// Open TCP sockets of one process ([`aterm_sysprobe::open_socket_count`]; macOS counts every
    /// socket).
    pub(crate) open_sockets: fn(u32) -> Option<u32>,
    /// The TCP readers' work after a reading publishes, `true` when no window in view reads the
    /// stream ([`aterm_sysprobe::tcp_after_publish`]: Linux's all-states dump, once an epoch).
    pub(crate) after_publish: fn(bool),
}

impl NetProbe {
    /// The kernel's passive local readers.
    pub(crate) const KERNEL: Self = Self {
        links: aterm_sysprobe::net_links_into,
        tcp: aterm_sysprobe::tcp_summary_into,
        open_sockets: aterm_sysprobe::open_socket_count,
        after_publish: aterm_sysprobe::tcp_after_publish,
    };

    /// The inert arm for deterministic offline tests.
    #[cfg(test)]
    pub(crate) const INERT: Self = Self {
        links: |_| None,
        tcp: |_| None,
        open_sockets: |_| None,
        after_publish: |_| {},
    };
}

/// Threshold above which a reading reads the whole process table for
/// births: one process's record (`KERN_PROC_PID`) cost 6.4 µs of CPU and
/// the table's ~700 records 83 µs (m22, 2026-10-23), so past about a dozen
/// the table is cheaper.
pub(crate) const BIRTHS_AT_ONCE: usize = 12;

/// Each process's kernel `p_comm` and start (unix ms) by pid.
pub(crate) type Births = HashMap<u32, (String, i64)>;

fn name_and_birth(r: aterm_sysprobe::kproc::Record) -> Option<(String, i64)> {
    let ms = i64::try_from(r.start_s)
        .ok()?
        .checked_mul(1000)?
        .checked_add(i64::from(r.start_us / 1000))?;
    Some((r.comm, ms))
}

/// A process's birth as read, under the name, kernel start and pid version
/// it was read for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Birth {
    name: String,
    start: Option<u64>,
    version: Option<i32>,
    ms: i64,
}

/// A lead's identity as read, and the birth and pid version it was read
/// under.
#[derive(Clone, Debug)]
struct Ident {
    birth: i64,
    version: i32,
    id: aterm_sysprobe::identity::Identity,
}

/// The split set's host reader and cached process births/timeline.
pub(crate) struct SplitReader {
    probe: CoalProbe,
    lane: CoalLane,
    /// aterm's own resource coalition, as the launch self-check read it.
    own: Option<u64>,
    vouches: bool,
    armed: &'static Arming,
    last: Option<Instant>,
    /// Most recent whole counter/ownership reading, independently of signature details.
    last_accounting: Option<Instant>,
    births: IdMap<u32, Birth>,
    /// When [`Self::births`] last read: its own gap clock, since births are
    /// read on every whole reading, split tick or not.
    births_at: Option<Instant>,
    /// Each lead's identity by pid, kept while it leads and its birth and
    /// pid version stay ([`Self::identity`]).
    idents: IdMap<u32, Ident>,
    /// Each swept process's coalition, kept by the sweep under its pid
    /// version ([`aterm_sysprobe::processes_keeping`]).
    pub(crate) coalition_ids: aterm_sysprobe::coalitions::CoalitionIds,
    wake_seen: Option<Option<i64>>,
    timeline: Option<TimelineSample>,
}

impl SplitReader {
    /// Constructs a split reader and runs the launch self-check on this process.
    pub(crate) fn new(probe: CoalProbe, armed: &'static Arming) -> Self {
        let checked = (probe.self_check)(std::process::id());
        let lane = match checked {
            Ok(_) => CoalLane::On,
            Err("unsupported") => CoalLane::Unsupported,
            Err(missed) => {
                aterm_log::warn!(
                    "system vitals: the coalitions lane is off: its self-check missed ({missed})"
                );
                CoalLane::Off
            }
        };
        let vouches = lane.on()
            && (probe.identity)(1)
                .is_some_and(|i| i.voucher() == aterm_sysprobe::identity::Voucher::Apple);
        if lane.on() && !vouches {
            aterm_log::warn!(
                "system vitals: launchd's signature does not read as Apple's: every \
                 coalition lead's voucher reads unknown"
            );
        }
        Self {
            probe,
            lane,
            own: checked.ok(),
            vouches,
            armed,
            last: None,
            last_accounting: None,
            births: IdMap::default(),
            births_at: None,
            idents: IdMap::default(),
            coalition_ids: aterm_sysprobe::coalitions::CoalitionIds::default(),
            wake_seen: None,
            timeline: None,
        }
    }

    fn due(&self, req: &Request, now: Instant) -> bool {
        self.lane.on() && split_due(self.last, req, self.armed.armed(), now)
    }

    /// Every coalition's counters, read at every reading while the lane is on (record §8.2):
    /// the start counter's read, which a split tick's split takes too.
    fn coalitions(&self) -> Option<aterm_sysprobe::coalitions::CoalitionRead> {
        self.lane.on().then(self.probe.coalitions).flatten()
    }

    /// The machine's start counter at this reading ([`aterm_vitals::starts`]): while the lane
    /// is on, each answering coalition's task counters from `read`, and aterm's own coalition (a
    /// read in which none answered counts nothing, [`aterm_vitals::starts::coalition_moves`]);
    /// with the lane off or unsupported, Linux's forks from `machine`'s core-ticks read at this
    /// reading.
    fn start_count(
        &self,
        read: Option<&mut CoalRead>,
        machine: &aterm_sysprobe::Probe,
    ) -> Option<aterm_vitals::StartCount> {
        if self.lane.on() {
            return read.map(|r| aterm_vitals::StartCount::Coalitions {
                read: coal_tasks(r),
                own: self.own,
            });
        }
        (self.probe.forks)(machine).map(aterm_vitals::StartCount::Forks)
    }

    fn cold(&self, now: Instant) -> bool {
        self.lane.on()
            && self.armed.armed()
            && self.last_accounting.is_none_or(|t| {
                now.saturating_duration_since(t) + SPLIT_COLD_MARGIN > SPLIT_WINDOW_MAX
            })
    }

    /// Each swept process's birth where the sweep gave none (macOS, whose
    /// sweep start is Mach time): kept under its name and kernel start (or
    /// pid version), else read from its kernel record, or from the whole
    /// table past [`BIRTHS_AT_ONCE`] unread. Run on every whole reading (one
    /// with a sweep), not only on split ticks, so a program's age
    /// ([`aterm_vitals::model::ProgramGroup`]) needs no coalitions lane.
    pub(crate) fn births(&mut self, sample: &mut Sample, now: Instant) {
        let Some(procs) = sample.procs.as_mut() else {
            return;
        };
        if self
            .births_at
            .is_some_and(|t| now.saturating_duration_since(t) > SPLIT_WINDOW_MAX)
        {
            self.births.clear();
        }
        self.births_at = Some(now);
        let swept: IdSet<u32> = procs.iter().map(|p| p.pid).collect();
        self.births.retain(|pid, _| swept.contains(pid));
        let mut unread = Vec::new();
        for (i, p) in procs.iter_mut().enumerate() {
            if p.birth_ms.is_none() {
                p.birth_ms = self.known(p);
            }
            if p.birth_ms.is_none() {
                unread.push(i);
            }
        }
        let table = (unread.len() > BIRTHS_AT_ONCE)
            .then(self.probe.births)
            .flatten();
        for i in unread {
            procs[i].birth_ms = self.read_birth(&procs[i], table.as_ref());
        }
    }

    fn fill(
        &mut self,
        sample: &mut Sample,
        read: aterm_sysprobe::coalitions::CoalitionRead,
        now: Instant,
        details: bool,
    ) {
        if sample.procs.is_none() {
            return;
        }
        if self
            .last_accounting
            .is_some_and(|t| now.saturating_duration_since(t) > SPLIT_WINDOW_MAX)
        {
            self.idents.clear();
        }
        self.births(sample, now);
        let procs = sample.procs.as_deref().unwrap_or_default();
        let born: IdMap<u32, (i64, Option<i32>)> = procs
            .iter()
            .filter_map(|p| Some((p.pid, (p.birth_ms?, p.version))))
            .collect();
        let members: Vec<(u32, u64, i64)> = procs
            .iter()
            .filter_map(|p| Some((p.pid, p.coalition?, p.birth_ms?)))
            .collect();
        let mut kept = IdMap::default();
        for (_, pid) in aterm_vitals::explain::leads(&members) {
            let Some(&(birth, version)) = born.get(&pid) else {
                continue;
            };
            let id = if details {
                self.identity(pid, birth, version, &mut kept)
            } else if let Some(cached) = self
                .idents
                .remove(&pid)
                .filter(|cached| cached.birth == birth && Some(cached.version) == version)
            {
                let identity = cached.id.clone();
                kept.insert(pid, cached);
                identity
            } else {
                aterm_sysprobe::identity::Identity::default()
            };
            sample
                .leads
                .push(lead_identity(pid, birth, id, self.vouches));
        }
        self.idents = kept;
        sample.coalitions = Some(coal_sample(read));
        self.last_accounting = Some(now);
        if details {
            sample.timeline = self.timeline();
            self.last = Some(now);
        } else {
            sample.timeline = self.timeline.clone();
        }
    }

    /// Lead `pid`'s identity (born at `birth`, at pid `version` as this
    /// tick's sweep read it), kept into `kept`: the one read under that
    /// version, read again only once an exec moved the version or the birth
    /// moved. A read an exec raced (the version moved across it) and a read
    /// that missed are unknown and kept for no later tick; with no version
    /// read, it is read at every split tick.
    fn identity(
        &mut self,
        pid: u32,
        birth: i64,
        version: Option<i32>,
        kept: &mut IdMap<u32, Ident>,
    ) -> aterm_sysprobe::identity::Identity {
        if let Some(k) = self
            .idents
            .remove(&pid)
            .filter(|k| k.birth == birth && Some(k.version) == version)
        {
            let id = k.id.clone();
            kept.insert(pid, k);
            return id;
        }
        let Some(id) = (self.probe.identity)(pid) else {
            return aterm_sysprobe::identity::Identity::default();
        };
        match version {
            None => id,
            Some(v) if (self.probe.version)(pid) == Some(v) => {
                kept.insert(
                    pid,
                    Ident {
                        birth,
                        version: v,
                        id: id.clone(),
                    },
                );
                id
            }
            Some(_) => aterm_sysprobe::identity::Identity::default(),
        }
    }

    /// `p`'s kept birth: one read for the same name and kernel start, or,
    /// for a process whose start the sweep cannot read (another user's), the
    /// same name and pid version (a new process at the pid has a new one;
    /// so does an exec, after which its birth is read again).
    fn known(&self, p: &ProcSample) -> Option<i64> {
        let same = |b: &Birth| match p.start {
            Some(_) => b.start == p.start,
            None => p.version.is_some() && b.version == p.version,
        };
        self.births
            .get(&p.pid)
            .filter(|b| b.name == p.name && same(b))
            .map(|b| b.ms)
    }

    fn read_birth(&mut self, p: &ProcSample, table: Option<&Births>) -> Option<i64> {
        let read = match table {
            Some(table) => table.get(&p.pid).cloned(),
            None => (self.probe.birth)(p.pid),
        };
        let Some((_, ms)) =
            read.filter(|(name, _)| aterm_vitals::model::same_process_name(name, &p.name))
        else {
            self.births.remove(&p.pid);
            return None;
        };
        self.births.insert(
            p.pid,
            Birth {
                name: p.name.clone(),
                start: p.start,
                version: p.version,
                ms,
            },
        );
        Some(ms)
    }

    fn timeline(&mut self) -> Option<TimelineSample> {
        let wake = (self.probe.waketime_ms)();
        if self.wake_seen != Some(wake) {
            self.timeline = (self.probe.timeline)().map(|t| TimelineSample {
                boot_ms: t.boot_ms,
                sleep_ms: t.sleep_ms,
                wake_ms: t.wake_ms,
                wake_reason: t.wake_reason,
            });
            self.wake_seen = self.timeline.as_ref().map(|_| wake);
        }
        self.timeline.clone()
    }
}

fn lead_identity(
    pid: u32,
    birth_ms: i64,
    id: aterm_sysprobe::identity::Identity,
    vouches: bool,
) -> LeadIdentity {
    use aterm_sysprobe::identity::Voucher as Kernel;
    use aterm_vitals::explain::Voucher;
    let voucher = match id.voucher() {
        _ if !vouches => Voucher::Unknown,
        Kernel::Apple => Voucher::Apple,
        Kernel::AppStore => Voucher::AppStore,
        Kernel::DeveloperId => Voucher::DeveloperId,
        Kernel::Development => Voucher::Development,
        Kernel::Enterprise => Voucher::Enterprise,
        Kernel::AdHoc => Voucher::AdHoc,
        Kernel::Unsigned => Voucher::Unsigned,
        Kernel::Unknown => Voucher::Unknown,
    };
    LeadIdentity {
        pid,
        birth_ms,
        exe: id.exe,
        voucher,
        signing_id: id.signing_id,
        team: id.team,
    }
}

/// A coalition read and the middle of its span on the reader's epoch clock, ns: the heat
/// change's window runs from one read's middle to the next's.
pub(crate) struct CoalRead {
    read: aterm_sysprobe::coalitions::CoalitionRead,
    at_ns: u64,
}

/// The task counters of `read`'s answering coalitions ([`aterm_vitals::starts`]), carrying the
/// heat counters its copy loop decoded (moved out of `read`, which the split's fill reads only
/// for its rows): the start counter's merge takes their change (A4 §4, one engine).
fn coal_tasks(read: &mut CoalRead) -> aterm_vitals::starts::CoalTaskRead {
    let heat = std::mem::take(&mut read.read.heat);
    aterm_vitals::starts::CoalTaskRead::of(
        read.read.newest,
        read.read.truncated,
        read.read
            .rows
            .iter()
            .map(|u| aterm_vitals::starts::CoalTasks {
                id: u.id,
                started: u.tasks_started,
                exited: u.tasks_exited,
            }),
    )
    .with_heat(aterm_vitals::heat::HeatBook {
        at_ns: read.at_ns,
        timebase: heat.timebase,
        by_id: heat.by_id,
        total: heat.total,
    })
}

fn coal_sample(read: aterm_sysprobe::coalitions::CoalitionRead) -> CoalSample {
    CoalSample {
        listed: read.listed,
        newest: read.newest,
        truncated: read.truncated,
        rows: read
            .rows
            .into_iter()
            .map(|u| CoalUsage {
                id: u.id,
                tasks_started: u.tasks_started,
                tasks_exited: u.tasks_exited,
                cpu_ns: u.cpu_ns,
                bytes_read: u.bytes_read,
                eqos_ns: u.eqos_ns,
                footprint: u.footprint,
            })
            .collect(),
    }
}

/// A debug fixture: a recorded scene, or live hardware with utilization deliberately unread.
#[cfg(debug_assertions)]
pub(crate) enum WhyFixture {
    Recorded {
        scene: Box<aterm_vitals::explain::fixtures::Scene>,
        t0: Option<Instant>,
    },
    HardwareUnread,
}

/// No fixture in a build without debug assertions.
#[cfg(not(debug_assertions))]
pub(crate) enum WhyFixture {}

#[cfg(debug_assertions)]
impl WhyFixture {
    fn from_seam() -> Option<Self> {
        let named = aterm_types::dev_seam!(WHY_FIXTURE_SEAM)?;
        let name = named.to_str()?;
        if name == "hardware-unread" {
            aterm_log::info!("system vitals: showing live hardware with unread utilization");
            return Some(Self::HardwareUnread);
        }
        let Some(scene) = Self::scene_named(name) else {
            aterm_log::warn!("system vitals: {WHY_FIXTURE_SEAM}={name} names no scene");
            return None;
        };
        aterm_log::info!(
            "system vitals: reading the {} scene",
            named.to_string_lossy()
        );
        Some(Self::Recorded {
            scene: Box::new(scene),
            t0: None,
        })
    }

    fn is_recorded(&self) -> bool {
        matches!(self, Self::Recorded { .. })
    }

    /// The view-only fixture never changes the model's samples or the other resources.
    fn snapshot(&self, live: &Snapshot) -> Option<Snapshot> {
        if !matches!(self, Self::HardwareUnread) {
            return None;
        }
        let mut snap = live.clone();
        if let Some(gpu) = snap.facts.gpu.as_mut() {
            gpu.device_pct = None;
            gpu.renderer_pct = None;
            gpu.tiler_pct = None;
        }
        snap.gpu = None;
        let unread = |row: &mut aterm_vitals::model::ResourceView| {
            *row = aterm_vitals::model::ResourceView {
                capacity: row.capacity,
                state: row.state,
                basis: row.basis,
                ..Default::default()
            };
        };
        unread(&mut snap.resources[aterm_vitals::Resource::Gpu.index()]);
        if snap
            .cpu_counter_sample
            .as_ref()
            .is_some_and(|cores| !cores.is_empty())
        {
            snap.cpu = None;
            unread(&mut snap.resources[aterm_vitals::Resource::Cpu.index()]);
        }
        Some(snap)
    }

    /// The scene the seam's `name` names: `calm`, or `spotlight`'s work under calm's memory and
    /// thermal state (it records neither); `None` for any other name.
    fn scene_named(name: &str) -> Option<aterm_vitals::explain::fixtures::Scene> {
        let calm = aterm_vitals::explain::fixtures::calm_1510();
        match name {
            "spotlight" => Some(aterm_vitals::explain::fixtures::Scene {
                mem: calm.mem,
                thermal: calm.thermal,
                ..aterm_vitals::explain::fixtures::spotlight_1032()
            }),
            "calm" => Some(calm),
            _ => None,
        }
    }

    fn reading(&mut self, live: &Sample, split: bool) -> Sample {
        let Self::Recorded { t0, .. } = self else {
            return live.clone();
        };
        let t0 = *t0.get_or_insert_with(Instant::now);
        self.at(
            live,
            split,
            u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
        )
    }

    /// The scene's reading `at_ms` in, over the machine's `live` one: its
    /// tabs are the machine's tabs in order (the live tab's keys and label,
    /// the scene's shell), so a caller in a live tab reads the scene's tab.
    fn at(&self, live: &Sample, split: bool, at_ms: u64) -> Sample {
        let Self::Recorded { scene, .. } = self else {
            return live.clone();
        };
        let mut scene = scene.reading(at_ms, split);
        for (t, l) in scene.tabs.iter_mut().zip(&live.tabs) {
            (t.key, t.tab, t.label) = (l.key, l.tab, l.label.clone());
        }
        let cores = scene.cores.as_ref().map_or(0, Vec::len);
        Sample {
            clusters: vec![ClusterSpec {
                name: "CPU".into(),
                logical: (0..cores).filter_map(|c| u16::try_from(c).ok()).collect(),
            }],
            procs: live.procs.as_ref().and(scene.procs),
            mem: scene.mem.or(live.mem),
            cpu_millicores: scene.cpu_millicores.or(live.cpu_millicores),
            mem_bytes: scene.mem_bytes.or(live.mem_bytes),
            thermal: scene.thermal.or(live.thermal),
            thermal_pressure: live.thermal_pressure,
            clock_range: live.clock_range,
            host_watts: live.host_watts,
            gpu: live.gpu.clone(),
            gpu_clients: live.gpu_clients.clone(),
            ane: live.ane,
            self_cpu_read: live.self_cpu_read,
            volume: live.volume.clone(),
            cadence_ms: live.cadence_ms,
            machine_only: live.machine_only,
            load1: live.load1,
            run_queue: live.run_queue,
            dstate: live.dstate.clone(),
            zombie_read: live.zombie_read,
            load: live.load,
            cpu_brand: live.cpu_brand.clone(),
            swapped_kib: live.swapped_kib,
            links: live.links.clone(),
            tcp_srtt_ms: live.tcp_srtt_ms,
            aterm_sockets: live.aterm_sockets,
            tab_sockets: live.tab_sockets.clone(),
            room: live.room,
            starts: live.starts.clone(),
            disk: live.disk.clone(),
            tokens: live.tokens.clone(),
            pane_token_observations: live.pane_token_observations.clone(),
            plan: live.plan.clone(),
            note_events: live.note_events.clone(),
            note_gaps: live.note_gaps.clone(),
            situation: live.situation.clone(),
            place: live.place,
            stuck_work: live.stuck_work,
            ..scene
        }
    }
}

#[cfg(not(debug_assertions))]
impl WhyFixture {
    fn from_seam() -> Option<Self> {
        if aterm_types::dev_seam!(WHY_FIXTURE_SEAM).is_some() {
            aterm_log::warn!(
                "system vitals: {WHY_FIXTURE_SEAM} names a scene only a debug build has: \
                 reading the machine"
            );
        }
        None
    }

    fn reading(&mut self, _live: &Sample, _split: bool) -> Sample {
        match *self {}
    }

    fn is_recorded(&self) -> bool {
        match *self {}
    }

    fn snapshot(&self, _live: &Snapshot) -> Option<Snapshot> {
        match *self {}
    }
}

/// The principal captured with each usable shell root in this same request.
fn event_sources(req: &Request) -> Vec<aterm_vitals::events::SessionScope> {
    req.pane_sources
        .iter()
        .filter(|pane| {
            req.tabs
                .iter()
                .any(|root| root.key == pane.local && root.shell_pid > 0)
        })
        .map(|pane| aterm_vitals::events::SessionScope {
            local: pane.local,
            sid: pane.sid.as_str().to_owned(),
            nonce: pane.nonce.to_hex(),
        })
        .collect()
}

/// The readers and the model: one per thread.
pub(crate) struct Reader {
    boot: crate::vitals_host::boot_capture::BootCapture,
    memory_probes: crate::vitals_host::memory::Probes,
    process_receipt: Option<Arc<crate::vitals_host::memory::Receipt>>,
    probe: aterm_sysprobe::Probe,
    ownership: ownership::Images,
    hardware: aterm_sysprobe::cpu_hardware::Probe,
    hardware_read: HardwareProbe,
    /// Own-process thread observations, read only by a reading a pull that prints them asked
    /// ([`Request::self_cpu`]) where the reader reads them ([`self_cpu::Probe::reads`]: macOS);
    /// test readers never inspect the host.
    self_cpu_read: self_cpu::Probe,
    /// The queries that arm a pull's read of aterm's own threads ([`SELF_CPU_ARMED`]; a unit
    /// test's reader has its own), read once as each pull begins ([`Request::self_cpu`]).
    self_cpu_armed: &'static Arming,
    /// When the last reading read a cut of aterm's own threads, on the pass's clock; `None`
    /// where it read none or its read failed, which clears the model's continuity
    /// ([`Source::self_cpu_cold`]).
    self_cpu_last: Option<Instant>,
    /// The queries that arm a pull's CPU Stats ([`HARDWARE_ARMED`]; a unit test's reader has its
    /// own), read once as each pull begins ([`Request::hardware`]).
    hardware_armed: &'static Arming,
    /// The queries that arm a pull's process scheduling ([`SCHEDULING_ARMED`]; a unit test's
    /// reader has its own), read once as each pull begins ([`Request::scheduling`]).
    pub(crate) scheduling_armed: &'static Arming,
    /// The last CPU Stats sample and when it was taken: a reading whose demands stand but none is
    /// due carries it ([`HardwarePlan::Carry`]).
    hardware_last: Option<(Instant, aterm_vitals::hardware::Sample)>,
    clusters: Vec<ClusterSpec>,
    vitals: aterm_vitals::Vitals,
    assessment: assessment::Lane,
    machine_observed_at: Option<Instant>,
    observed_at: memory::ObservedAt,
    epoch: Instant,
    /// The pass's clock ([`Request::tab_reads`]): the machine's, or a test's own, which no load
    /// spaces.
    clock: fn() -> Instant,
    home: PathBuf,
    scratch: Vec<u8>,
    uid: Option<u32>,
    survey: Arc<Where>,
    job_watch: aterm_vitals::events::JobWatch,
    watched: Option<Arc<aterm_vitals::jobs::JobsView>>,
    thermal: ThermalProbe,
    /// The heat chain's own readers: the thermal pressure level, the chip's clock range and,
    /// on Linux, the packages' energy.
    heat: HeatReaders,
    counter_throttle: Option<(Instant, Option<Thermal>)>,
    /// When `mdutil -s -a` runs, and what it last read ([`aterm_vitals::spotlight::Cadence`]).
    spotlight: aterm_vitals::spotlight::Cadence,
    /// Its port: [`MDUTIL_LIVE`], or a unit test's inert arm.
    pub(crate) mdutil: aterm_vitals::spotlight::MdutilProbe,
    /// When aterm's root LAN broker is asked for Spotlight's stores (SD-94), off this thread
    /// ([`aterm_vitals::spotlight::StoreAsk`]).
    spotlight_store: aterm_vitals::spotlight::StoreAsk,
    /// Its port: [`STORE_LIVE`], or a unit test's inert arm.
    pub(crate) store_probe: aterm_vitals::spotlight::StoreProbe,
    /// The Neural Engine as its reader read it once, as this reader started (A2 §3.1:
    /// [`ane_at_start`]), which every reading carries: whether it reads here, its cores, and
    /// whether it was powered then. No reading reads its power again: nothing draws it yet
    /// (its one reader, [`crate::stats_units::UnitFacts::of`], is called only by tests). `None`
    /// where the OS has no reader, and in a unit test's reader, which reads no registry.
    pub(crate) ane: Option<Result<AneSample, AneCheck>>,
    slots: aterm_sysprobe::slots::SlotReader,
    /// The split set's lane and what it keeps between split ticks.
    pub(crate) split: SplitReader,
    /// The pulls that arm the stuck engine ([`JOBS_ARMED`]; a unit test's reader has its own).
    pub(crate) jobs_armed: &'static Arming,
    /// The network lane's passive kernel readers.
    pub(crate) net: NetProbe,
    /// The GPU client lane.
    pub(crate) gpu: crate::vitals_gpu::GpuLane,
    fixture: Option<WhyFixture>,
    /// The machine's words, read once.
    pub(crate) machine: Machine,
    /// The block-storage reader, which keeps each volume's walk to its device.
    block: aterm_sysprobe::blockio::BlockReader,
    /// The machine's situation and type (A4 §8, §9), kept current on notifications.
    situation: crate::vitals_situation::SituationReader,
    /// The own-thread D-state census's worker (macOS; inert in a unit test's reader).
    census: crate::vitals_census::CensusLane,
    /// The disk journal's tail, for the Disk lens's list and chart.
    journal: DeedTail,
    /// `sys disk held`'s rows, for the Disk lens's Held by and its bar's classes.
    holdings: HoldingsRead,
    /// Where the process reads run: a lane, or a unit test's reading thread.
    pub(crate) procs: ProcsRead,
    /// The processes of the last whole sweep that answered: the names a stalled read's pid goes
    /// by, never read live. The sweep itself goes to the assessment owner.
    last_procs: Option<Vec<ProcSample>>,
    /// The stall the last reading named, so its start is journaled once.
    procs_stalled: Option<i64>,
}

/// What a reading takes of its tabs before its sweep ([`Request::tab_reads`]): the pass's clock,
/// ms past the reader's zero, each feed, and each current mode word.
#[derive(Debug)]
pub(crate) struct TabReads {
    observed_at: Instant,
    at_ms: u64,
    epoch: Instant,
    feeds: Vec<(u64, crate::vitals_jobs::TabFeed)>,
    ttys: Vec<(u64, aterm_vitals::jobs::TtyMode)>,
}

impl Request {
    /// Reads every root's word again from its source, none for a session that is gone. A worker's
    /// pull reads with the request the thread last held, however old: a write that moved the
    /// screen since its words were read (an agent's last redraw of a turn, after the reading that
    /// parked it) would leave each word stamped before that output, naming no wait, so each of the
    /// pull's readings reads them afresh first.
    pub(crate) fn reread_words(&mut self) {
        self.ttys = self
            .tty_sources
            .iter()
            .filter_map(TtySource::read)
            .collect();
    }

    /// Reads the pass's clock (`now`, ms past `epoch`, at least 1 ms), then each feed on it, then
    /// each current mode word. A reading takes them before its sweep, so a block's `D` and the exit
    /// of the command it closes, read while the sweep runs, are the next pass's, never a closed
    /// block over a member the sweep still saw (a false orphan). A feed rounds its stamps up
    /// ([`crate::vitals_jobs::JobFeed::at_pass`]), so whatever a reader reads after the clock is
    /// stamped past it, and a line or output stamped past the clock waits for the next pass.
    pub(crate) fn tab_reads(&self, epoch: Instant, now: impl FnOnce() -> Instant) -> TabReads {
        let observed_at = now();
        let at_ms = pass_ms(epoch, observed_at);
        let feeds = self
            .feeds
            .iter()
            .map(|(key, f)| {
                let mut feed = f.at_pass(epoch, Some(at_ms));
                feed.typed_ms = self
                    .inputs
                    .iter()
                    .find(|(k, _)| k == key)
                    .and_then(|(_, w)| f.typed_at(at_ms, w.get()));
                (*key, feed)
            })
            .collect();
        let ttys = self.ttys.iter().filter_map(TtyWord::current).collect();
        TabReads {
            observed_at,
            at_ms,
            epoch,
            feeds,
            ttys,
        }
    }
}

/// The pass's clock at `clock`: ms past the reader's zero `epoch`, at least 1 ms.
fn pass_ms(epoch: Instant, clock: Instant) -> u64 {
    u64::try_from(clock.saturating_duration_since(epoch).as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

/// How old `view` is at a reading at `at_ms` on the pass's clock ([`Snapshot::jobs_age_ms`]).
fn jobs_age_ms(view: Option<&aterm_vitals::jobs::JobsView>, at_ms: u64) -> u64 {
    view.map_or(0, |v| at_ms.saturating_sub(v.at_ms))
}

impl TabReads {
    /// The reader's clock now, ms past its zero, rounded up: no earlier than anything read
    /// before it.
    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos().div_ceil(1_000_000)).unwrap_or(u64::MAX)
    }
}

/// Steps the stuck engine over one whole reading, on the clock and with the tab reads taken
/// before its sweep.
pub(crate) fn jobs_step(
    pass: &mut crate::vitals_jobs::JobsPass,
    reads: &TabReads,
    facts: &crate::vitals_jobs::ProcessFacts,
    req: &Request,
    thrashing: bool,
) -> Option<Arc<aterm_vitals::jobs::JobsView>> {
    let view = pass.tick(
        reads.at_ms,
        || reads.now_ms(),
        facts,
        &req.tabs,
        &reads.ttys,
        &reads.feeds,
        &req.grant,
        req.self_pid,
        thrashing,
        req.jobs_now,
    );
    pass.note_observed(reads.observed_at);
    view
}

impl Reader {
    pub(crate) fn new(survey: Arc<Where>) -> Self {
        let clusters = aterm_sysprobe::cpu_layout()
            .clusters
            .into_iter()
            .map(|c| ClusterSpec {
                name: c.name,
                logical: c.logical,
            })
            .collect();
        let words = aterm_sysprobe::machine_words();
        let home = aterm_types::dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let fixture = if cfg!(test) {
            None
        } else {
            WhyFixture::from_seam()
        };
        let split = SplitReader::new(
            if cfg!(test) {
                CoalProbe::INERT
            } else {
                CoalProbe::KERNEL
            },
            &SPLIT_ARMED,
        );
        let mut reader = Self {
            boot: if cfg!(test) || fixture.as_ref().is_some_and(WhyFixture::is_recorded) {
                crate::vitals_host::boot_capture::BootCapture::inert()
            } else {
                crate::vitals_host::boot_capture::BootCapture::native()
            },
            memory_probes: if cfg!(test) || fixture.as_ref().is_some_and(WhyFixture::is_recorded) {
                crate::vitals_host::memory::Probes::INERT
            } else {
                crate::vitals_host::memory::Probes::NATIVE
            },
            process_receipt: None,
            probe: aterm_sysprobe::Probe::new(),
            ownership: if cfg!(test) {
                ownership::Images::INERT
            } else {
                ownership::Images::NATIVE
            },
            block: aterm_sysprobe::blockio::BlockReader::new(),
            census: crate::vitals_census::CensusLane::new(cfg!(all(
                target_os = "macos",
                not(test)
            ))),
            hardware: aterm_sysprobe::cpu_hardware::Probe::default(),
            hardware_read: if cfg!(test) {
                HardwareProbe::INERT
            } else {
                HardwareProbe::NATIVE
            },
            self_cpu_read: if cfg!(test) {
                self_cpu::Probe::INERT
            } else {
                self_cpu::Probe::NATIVE
            },
            self_cpu_armed: if cfg!(test) {
                Box::leak(Box::new(Arming::new()))
            } else {
                &SELF_CPU_ARMED
            },
            self_cpu_last: None,
            hardware_armed: if cfg!(test) {
                Box::leak(Box::new(Arming::new()))
            } else {
                &HARDWARE_ARMED
            },
            scheduling_armed: if cfg!(test) {
                Box::leak(Box::new(Arming::new()))
            } else {
                &SCHEDULING_ARMED
            },
            hardware_last: None,
            clusters,
            vitals: aterm_vitals::Vitals::with_group_book(aterm_vitals::groups::GroupBook::new()),
            assessment: assessment::Lane::new(),
            machine_observed_at: None,
            observed_at: memory::ObservedAt {
                instant: Instant::now(),
                continuous_ns: None,
            },
            epoch: aterm_session::terminal_stream::epoch(),
            clock: Instant::now,
            slots: aterm_sysprobe::slots::SlotReader::new(
                aterm_sysprobe::slots::default_dir(&home),
                {
                    #[cfg(all(target_os = "macos", not(test)))]
                    {
                        aterm_uds::account::home()
                            .and_then(|home| std::fs::canonicalize(home).ok())
                            .map(|home| aterm_sysprobe::slots::default_dir(&home).join("adopted"))
                    }
                    #[cfg(any(not(target_os = "macos"), test))]
                    {
                        None
                    }
                },
            ),
            home,
            scratch: Vec::new(),
            // Whose a process is: this person's uid, and on Windows this process's session,
            // which a sweep row's owner is there.
            uid: if cfg!(unix) {
                Some(aterm_agent::harness::disk::discover::own_uid())
            } else {
                aterm_sysprobe::own_session()
            },
            survey,
            job_watch: aterm_vitals::events::JobWatch::default(),
            watched: None,
            thermal: if cfg!(test) {
                || None
            } else {
                platform_thermal
            },
            heat: if cfg!(test) {
                HeatReaders::INERT
            } else {
                HeatReaders::native()
            },
            counter_throttle: None,
            spotlight: aterm_vitals::spotlight::Cadence::default(),
            mdutil: if cfg!(test) {
                aterm_vitals::spotlight::MdutilProbe::INERT
            } else {
                MDUTIL_LIVE
            },
            spotlight_store: aterm_vitals::spotlight::StoreAsk::default(),
            store_probe: if cfg!(test) {
                aterm_vitals::spotlight::StoreProbe::INERT
            } else {
                STORE_LIVE
            },
            ane: if cfg!(test) {
                None
            } else {
                ane_at_start(aterm_sysprobe::neural_engine::Reader::native())
            },
            split,
            jobs_armed: if cfg!(test) {
                Box::leak(Box::new(Arming::new()))
            } else {
                &JOBS_ARMED
            },
            net: NetProbe::KERNEL,
            gpu: crate::vitals_gpu::GpuLane::host(),
            fixture: None,
            machine: Machine {
                executable: std::env::current_exe()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned()),
                cpu: words.cpu,
                os: words.os,
                family: os_family(),
            },
            situation: if cfg!(test) {
                crate::vitals_situation::SituationReader::inert()
            } else {
                crate::vitals_situation::SituationReader::host()
            },
            // Where the window's own disk watch writes it ([`crate::harness_host`]'s look).
            journal: DeedTail::new(if cfg!(test) {
                None
            } else {
                crate::harness_host::harness_state()
                    .map(|state| state.join(aterm_agent::harness::cli::DISK_LEDGER))
            }),
            // A unit test's reader gathers nothing: no board, build-start record or aterm.toml.
            holdings: HoldingsRead::new(if cfg!(test) {
                None
            } else {
                Some(crate::sys_host::held_for)
            }),
            procs: if cfg!(test) {
                ProcsRead::Inline
            } else {
                ProcsRead::lane(PROCS_WAIT)
            },
            last_procs: None,
            procs_stalled: None,
        };
        reader.read_scene(fixture);
        reader
    }

    /// The machine's coalitions at this reading ([`SplitReader::coalitions`]), stamped with the
    /// middle of the read; none while a recorded scene stands for the machine's lane, whose
    /// coalitions are the scene's.
    fn coalitions(&self) -> Option<CoalRead> {
        if self.fixture.as_ref().is_some_and(WhyFixture::is_recorded) {
            return None;
        }
        let before = self.epoch.elapsed();
        let read = self.split.coalitions()?;
        let middle = (before + self.epoch.elapsed()) / 2;
        Some(CoalRead {
            read,
            at_ns: u64::try_from(middle.as_nanos()).unwrap_or(u64::MAX),
        })
    }

    /// The process reads `sweep` asks for (none: the pid counts alone), and the lanes this
    /// reading finds stalled. The kept coalition ids go with the ask and come back with its
    /// answer; a stalled ask's are lost, and the next sweep keeps them afresh.
    fn read_procs(
        &mut self,
        sweep: Option<aterm_sysprobe::SweepOpts>,
        req: Option<&Request>,
    ) -> ProcReads {
        let ask = ProcsAsk {
            sweep: sweep.map(|opts| (self.split.probe.sweep, opts)),
            ids: std::mem::take(&mut self.split.coalition_ids),
            uid: self.uid.unwrap_or(0),
            ownership: req.map(|req| OwnershipAsk {
                images: self.ownership.clone(),
                self_pid: req.self_pid,
                tabs: req.tabs.clone(),
                identities: req.shell_identities.clone(),
                epoch: self.epoch,
            }),
            memory: self.memory_probes,
            observed_at: self.observed_at,
        };
        let (sweep, pids, procs, ownership, memory) = match self.procs.read(ask) {
            Ok(answer) => {
                self.split.coalition_ids = answer.ids;
                (
                    answer.sweep,
                    answer.pids,
                    None,
                    answer.ownership,
                    answer.memory,
                )
            }
            Err(stall) => (None, (None, None), stall, None, None),
        };
        match (procs, self.procs_stalled) {
            (Some(stall), None) => {
                aterm_log::info!(
                    "system vitals: the process sweep is stalled in {} (pid {:?}) since unix ms \
                     {}: its figures are unknown until it returns",
                    stall.call,
                    stall.pid,
                    stall.since_unix_ms
                );
                self.procs_stalled = Some(stall.since_unix_ms);
            }
            (None, Some(_)) => self.procs_stalled = None,
            _ => {}
        }
        let stalled = procs
            .into_iter()
            .chain(aterm_sysprobe::lane::stalls())
            .map(|stall| self.lane_stall(stall))
            .collect();
        ProcReads {
            sweep,
            pids,
            stalled,
            ownership,
            memory,
        }
    }

    /// `stall` as the model holds it, its pid named by the last whole sweep that answered.
    fn lane_stall(&self, stall: aterm_sysprobe::lane::Stall) -> aterm_vitals::lanes::LaneStall {
        let name = stall.pid.and_then(|pid| {
            self.last_procs
                .as_ref()?
                .iter()
                .find(|p| p.pid == pid)
                .map(|p| p.name.clone())
        });
        aterm_vitals::lanes::LaneStall {
            lane: stall.lane,
            call: stall.call,
            pid: stall.pid,
            name,
            since_unix_ms: stall.since_unix_ms,
            for_ms: stall.for_ms,
        }
    }

    fn read_scene(&mut self, fixture: Option<WhyFixture>) {
        if fixture.as_ref().is_some_and(WhyFixture::is_recorded) {
            self.split.lane = CoalLane::On;
        }
        self.fixture = fixture;
    }

    fn counter_throttle(&mut self) -> Option<Thermal> {
        if let Some((at, last)) = self.counter_throttle
            && at.elapsed() < COUNTER_THROTTLE_EVERY
        {
            return last;
        }
        let now = counter_thermal(self.probe.throttle());
        self.counter_throttle = Some((Instant::now(), now));
        now
    }

    fn watch_jobs(&mut self, snap: &mut Snapshot, context: Option<&assessment::EventContext>) {
        if let Some(view) = snap.jobs.as_ref()
            && !self.watched.as_ref().is_some_and(|w| Arc::ptr_eq(w, view))
        {
            let events = self.job_watch.step(
                view,
                snap.seq,
                context.map_or(snap.at_ns, |context| context.at_ns),
                context.and_then(|context| context.at_unix_ms),
                context.map_or(&[], |context| context.sources.as_slice()),
            );
            snap.events.extend(events);
            self.watched = Some(Arc::clone(view));
        }
    }

    fn set_occupancy_facts(&mut self, snap: &mut Snapshot) -> aterm_sysprobe::slots::SlotsRead {
        let machine = aterm_sysprobe::slots::Machine {
            load5: snap.load.map(|l| l[1]),
            ncpu: snap.total_cores(),
        };
        let now_us = aterm_time::since_epoch()
            .and_then(|d| u64::try_from(d.as_micros()).ok())
            .unwrap_or(0);
        let now = Instant::now();
        let period = crate::occupancy_host::slots_period();
        let read = self.slots.read_within(now, now_us, &machine, period);
        snap.slots = Some(crate::occupancy_host::slots_view(&read));
        snap.slots_age_ms = self.slots.read_at().map(|at| {
            u64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(u64::MAX)
        });
        read
    }

    /// Takes one full reading (machine counters, process sweep, split set if due, and jobs pass).
    pub(crate) fn read(&mut self, req: &Request) -> Snapshot {
        self.observed_at =
            memory::ObservedAt::capture(self.clock, self.memory_probes.continuous_ns);
        let boot_before = self.boot.begin();
        let reads = req.tab_reads(self.epoch, self.clock);
        self.machine_observed_at = Some(reads.observed_at);
        let now = Instant::now();
        let split = self.split.due(req, now);
        let live_accounting =
            self.split.lane.on() && !self.fixture.as_ref().is_some_and(WhyFixture::is_recorded);
        // The reading's wall clock, read before the sweep lists the
        // processes: one born at or after it is in no earlier sweep, which is
        // how the reap credit tells a process born since the last reading
        // (counted from its birth) from one older (counted from now).
        let at_unix_ms = unix_ms_now();
        // Every whole reading reads the pid version of each process whose
        // start it cannot read (another user's), so the births kept for them
        // (`SplitReader::known`) hold from one reading to the next. Only a
        // reading with the coalition lane on (and no recorded scene) asks for
        // coalitions, and with them keeps each name at the short name's cut
        // in `SplitReader::coalition_ids` across readings (a stalled ask
        // loses them, `read_procs`); with the lane off the sweep reads such
        // a name whole every reading. Each process's scheduling is read only where a reader
        // of it asks (`sweep_reads_scheduling`).
        let scheduling = self.sweep_reads_scheduling(req);
        let ProcReads {
            sweep,
            pids,
            stalled,
            ownership,
            memory,
        } = self.read_procs(
            Some(aterm_sysprobe::SweepOpts {
                coalitions: live_accounting,
                versions: true,
                scheduling,
            }),
            Some(req),
        );
        let mut checked_req = req.clone();
        let (unproved_tabs, shared_roots) = if let Some(ownership) = ownership {
            self.ownership = ownership.images;
            checked_req.tabs = ownership.tabs;
            (ownership.unproved, ownership.shared)
        } else {
            let unproved = checked_req
                .tabs
                .iter_mut()
                .filter_map(|root| {
                    if root.shell_pid == 0 {
                        return None;
                    }
                    root.shell_pid = 0;
                    Some(root.key)
                })
                .collect();
            (unproved, Vec::new())
        };
        let req = &checked_req;
        let mut coalitions = self.coalitions();
        let mut sample = self.machine_sample(
            req,
            at_unix_ms,
            sweep.as_ref().map(|sweep| sweep.procs.as_slice()),
            pids,
        );
        sample.stalled = stalled;
        sample.starts = self.split.start_count(coalitions.as_mut(), &self.probe);
        sample.machine_only = false;
        sample.scheduling_read = scheduling;
        sample.spotlight_volumes = self
            .spotlight
            .step(sample.mono_ms, self.vitals.spotlight_busy(), &self.mdutil)
            .cloned();
        if let Some(store) = self.spotlight_store.step(sample.mono_ms, &self.store_probe) {
            self.vitals.note_spotlight_store(store);
        }
        if let aterm_vitals::blockage::DStateRead::Census(pass) = &mut sample.dstate {
            *pass = self.census.step(now, self.epoch);
        }
        sample.procs_truncated = sweep.as_ref().is_some_and(|s| s.truncated);
        sample.unproved_tabs = unproved_tabs;
        // Native final events use the same cross-process clock. A final whose exit proof is
        // later than this complete frontend CPU sample belongs to a subsequent reading.
        let native_cutoff = aterm_vitals::model::NativeCpuCut {
            continuous_ns: aterm_uds::process::continuous_ns(),
            boot: aterm_uds::process::boot_clock_now().map(|clock| clock.0),
        };
        sample.native_cpu_cut = native_cutoff;
        let observed = sweep
            .as_ref()
            .map(|sweep| {
                crate::resource_jobs::sample(sweep, &req.session_ids, self.epoch, native_cutoff)
            })
            .unwrap_or_default();
        sample.procs = observed.procs;
        sample.delegated = observed.delegated;
        sample.native_roots = observed.native_roots;
        sample.native_terminals = observed.native_terminals;
        sample.native_incomplete = observed.incomplete;
        sample.native_source_lost = observed.source_lost;
        sample.unproved_tabs.extend(observed.unproved_tabs);
        sample.shared_roots = shared_roots;
        if let Some(procs) = sample.procs.as_deref() {
            let (aterm_sockets, tab_sockets) =
                read_sockets(procs, req.self_pid, &req.tabs, self.net.open_sockets);
            sample.aterm_sockets = aterm_sockets;
            sample.tab_sockets = tab_sockets;
        }
        sample.gpu_clients = self.gpu.read(req, now).map(|(rows, entries, complete)| {
            aterm_vitals::GpuClientsSample {
                at_ns: u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX),
                rows,
                entries,
                complete,
            }
        });
        if let Some(CoalRead { read, .. }) =
            coalitions.filter(|_| live_accounting && sweep.is_some())
        {
            self.split.fill(&mut sample, read, now, split);
        } else {
            // Every whole reading, a jobs-only edge's too: `sys` answers a
            // pull from whichever whole reading is fresh, so one read without
            // births would leave its ages unread on macOS.
            self.split.births(&mut sample, now);
            if req.deck {
                sample.timeline = self.split.timeline();
            }
        }
        // Preserve the existing core/process capture window: optional native
        // discovery can be slow, so it runs only after those readings finish.
        self.attach_hardware(&mut sample, req);
        let place = self.situate(&mut sample);
        let mut scene = self
            .fixture
            .as_mut()
            .filter(|fixture| fixture.is_recorded())
            .map(|fixture| {
                if split {
                    self.split.last = Some(now);
                }
                fixture.reading(&sample, split)
            });
        if let Some(scene) = scene.as_mut() {
            scene.event_sources.clear();
        }
        if scene.is_none() {
            sample.ownership_incomplete =
                crate::resource_jobs::restore_origins(&mut self.vitals, &req.session_ids);
        }
        let mut snap = self.vitals.absorb(scene.as_ref().unwrap_or(&sample));
        if scene.is_none() {
            snap.boot_cost = Some(self.boot.finish(
                &sample,
                self.vitals.historical_reported(),
                boot_before,
            ));
        }
        if scene.is_none() {
            crate::resource_jobs::publish_origins(self.vitals.process_origins(), &req.session_ids);
            if sample.procs.is_some() {
                crate::resource_jobs::native_model_pending(self.vitals.has_pending_native());
            }
        }
        if self.split.lane.on() && snap.coal_lane == CoalLane::Off && scene.is_none() {
            aterm_log::warn!(
                "system vitals: the coalitions lane is off: a read did not read as the \
                 record's layout"
            );
            self.split.lane = CoalLane::Off;
        }
        let slots = self.set_occupancy_facts(&mut snap);
        snap.place = place;
        snap.slot_tabs = crate::occupancy_host::slot_tabs(
            &slots,
            sample.procs.as_deref(),
            req.self_pid,
            &req.tabs,
            &mut |pid| {
                i32::try_from(pid)
                    .ok()
                    .and_then(aterm_uds::process::read_process_start)
            },
        );
        crate::resource_jobs::publish(
            sample
                .procs
                .as_ref()
                .and(snap.aterm.as_ref())
                .filter(|_| {
                    scene.is_none()
                        && !sample.native_incomplete
                        && !sample.ownership_incomplete
                        && snap.delegated.len() == sample.delegated.len()
                })
                .map(|_| snap.delegated.clone()),
        );
        let thrashing = sample.mem.is_some_and(|m| m.pressure == Pressure::Critical);
        let events_before = snap.strip_events.len();
        let complete = !sweep.as_ref().is_some_and(|s| s.truncated);
        let process_facts = crate::vitals_jobs::ProcessFacts::new(
            sweep.map(|s| s.procs),
            complete,
            scene.is_none().then_some((&sample, &snap)),
            self.vitals.current_zombie_facts(),
        );
        let assessed = self.assessment.read(
            assessment::Input::new(
                reads,
                process_facts,
                req,
                u64::try_from(at_unix_ms).unwrap_or(0),
                sample.at_ns,
                (at_unix_ms != 0).then_some(at_unix_ms),
                thrashing,
            ),
            !req.assessment_only,
        );
        snap.jobs_age_ms = jobs_age_ms(
            assessed.jobs.as_deref(),
            pass_ms(self.epoch, (self.clock)()),
        );
        snap.set_left(assessed.left);
        let context = assessed.event_context.as_deref();
        snap.set_jobs_observed(
            assessed.jobs,
            context.and_then(|context| context.at_unix_ms),
            context.map_or(&[], |context| context.sources.as_slice()),
        );
        for ev in snap.strip_events[events_before..].iter().cloned() {
            self.vitals.record_strip_event(ev);
        }
        self.watch_jobs(&mut snap, context);
        self.vitals.drain_strip_events(&mut snap);
        self.process_receipt = memory.filter(|_| scene.is_none()).map(|memory| {
            let panes = req
                .pane_sources
                .iter()
                .filter(|pane| {
                    !sample.unproved_tabs.contains(&pane.local)
                        && req
                            .tabs
                            .iter()
                            .any(|root| root.key == pane.local && root.shell_pid > 0)
                })
                .cloned()
                .collect();
            Arc::new(memory.publish(&snap, panes))
        });
        if sample.procs.is_some() {
            self.last_procs = sample.procs;
        }
        self.fixture_snapshot(snap)
    }

    /// Hands `sample` what the model keeps of the reading's surroundings in the strip, which it
    /// reads from neither netplace nor the jobs engine itself: the place's kind, for the
    /// situation spans, and what the newest jobs pass this reader holds found stuck, for the
    /// blockage ring (that pass is the one the reading before this published; this reading's
    /// own runs after its absorb). The place is returned for the Snapshot, so the reading
    /// publishes the place its span keeps.
    fn situate(&self, sample: &mut Sample) -> Option<aterm_vitals::PlaceView> {
        let place = crate::net_place::sys_view();
        sample.place = place.as_ref().map(|p| p.kind);
        let at_ms = pass_ms(self.epoch, (self.clock)());
        sample.stuck_work = self.assessment.reading().jobs.as_deref().map(|view| {
            aterm_vitals::blockage::StuckWork::of(view, jobs_age_ms(Some(view), at_ms))
        });
        place
    }

    /// Takes a hidden deck's reading (machine counters alone, no process sweep).
    pub(crate) fn read_machine(&mut self, req: &Request) -> Snapshot {
        self.observed_at =
            memory::ObservedAt::capture(self.clock, self.memory_probes.continuous_ns);
        self.machine_observed_at = Some((self.clock)());
        let ProcReads { pids, stalled, .. } = self.read_procs(None, None);
        let mut coalitions = self.coalitions();
        let mut sample = self.machine_sample(req, unix_ms_now(), None, pids);
        sample.stalled = stalled;
        sample.machine_only = true;
        sample.event_sources.clear();
        sample.spotlight_volumes = self.spotlight.last().cloned();
        sample.starts = self.split.start_count(coalitions.as_mut(), &self.probe);
        sample.timeline = self.split.timeline();
        let place = self.situate(&mut sample);
        let mut scene = self
            .fixture
            .as_mut()
            .filter(|fixture| fixture.is_recorded())
            .map(|fixture| fixture.reading(&sample, false));
        if let Some(scene) = scene.as_mut() {
            scene.event_sources.clear();
        }
        let mut snap = self.vitals.absorb(scene.as_ref().unwrap_or(&sample));
        if scene.is_none() {
            snap.boot_cost = Some(self.boot.report());
        }
        self.set_occupancy_facts(&mut snap);
        snap.place = place;
        let events_before = snap.strip_events.len();
        let assessed = self.assessment.carried();
        snap.jobs_age_ms = jobs_age_ms(
            assessed.jobs.as_deref(),
            pass_ms(self.epoch, (self.clock)()),
        );
        let context = assessed.event_context.as_deref();
        snap.set_jobs_observed(
            assessed.jobs,
            context.and_then(|context| context.at_unix_ms),
            context.map_or(&[], |context| context.sources.as_slice()),
        );
        snap.set_left(assessed.left);
        for ev in snap.strip_events[events_before..].iter().cloned() {
            self.vitals.record_strip_event(ev);
        }
        self.watch_jobs(&mut snap, context);
        self.vitals.drain_strip_events(&mut snap);
        self.fixture_snapshot(snap)
    }

    /// Whether whole reading `req`'s sweep reads each process's scheduling (two `getpriority`
    /// calls a process on macOS, `aterm_sysprobe::SweepOpts::scheduling`): only where a reader of
    /// it asks. Those are a pull a query that prints it armed ([`Request::scheduling`]), the CPU
    /// lens open (its diagnostic export copies each worker's,
    /// `stats_lens::cpu_diagnostic_report`), and the model's CPU reports
    /// ([`aterm_vitals::Vitals::cpu_reports_copy_scheduling`]: the recorder's frames, the report
    /// the host saves while a bundle or a CPU policy is held). The deck draws none.
    pub(crate) fn sweep_reads_scheduling(&self, req: &Request) -> bool {
        req.scheduling
            || req.lenses.contains(&aterm_vitals::Resource::Cpu)
            || self.vitals.cpu_reports_copy_scheduling()
    }

    /// Attaches CPU Stats to whole reading `sample` where a demand asks for it (A2 §6.4): a
    /// sample when one is due, else the last one while a demand stands, else none.
    fn attach_hardware(&mut self, sample: &mut Sample, req: &Request) {
        let demands = hardware_demands(
            req,
            self.vitals.cpu_recorder_pending(),
            self.hardware_read.free,
        );
        let cadence = Duration::from_millis(u64::from(req.cadence_ms));
        let now = (self.clock)();
        let last = self.hardware_last.as_ref().map(|(at, _)| *at);
        sample.hardware = match hardware_plan(&demands, cadence, last, now) {
            HardwarePlan::None => None,
            HardwarePlan::Carry => self.hardware_last.as_ref().map(|(_, h)| h.clone()),
            HardwarePlan::Sample(gap) => {
                let read = (self.hardware_read.sample)(&mut self.hardware, sample, gap);
                let read = hardware_sample(read);
                self.hardware_last = Some((now, read.clone()));
                Some(read)
            }
        };
    }

    fn fixture_snapshot(&self, snap: Snapshot) -> Snapshot {
        match self
            .fixture
            .as_ref()
            .and_then(|fixture| fixture.snapshot(&snap))
        {
            Some(view) => view,
            None => snap,
        }
    }

    /// The machine's counters, stamped `at_unix_ms` on the wall clock; `procs` is the whole
    /// reading's sweep (`None` for a machine-only reading, or where it did not answer), which
    /// names Windows' compressed memory ([`compression_named`]), and `pids` the process lane's
    /// pid counts ([`aterm_sysprobe::pid_counts`]). Nothing here reads another process.
    fn machine_sample(
        &mut self,
        req: &Request,
        at_unix_ms: i64,
        procs: Option<&[aterm_sysprobe::ProcessReading]>,
        pids: (Option<u64>, Option<u64>),
    ) -> Sample {
        let since = self.epoch.elapsed();
        let shelf = &crate::disk_board::SHELF;
        let MachineVolumes {
            home: volume,
            key,
            block,
            home_device_id,
        } = machine_volumes(
            &self.home,
            shelf.watched(),
            req.front_dir.as_deref(),
            |mounts| self.block.read(mounts),
        );
        let load = aterm_sysprobe::load_average();
        let links = (self.net.links)(&mut self.scratch)
            .map(|links| links.iter().map(link_sample).collect());
        let tcp_sum = (self.net.tcp)(&mut self.scratch);
        let gpu = aterm_sysprobe::gpu();
        let room = aterm_sysprobe::task_tables(self.uid.unwrap_or(0)).with_pids(pids.0, pids.1);
        let capacity = aterm_sysprobe::capacity();
        self.journal.refresh(
            req.deck || req.lenses.contains(&aterm_vitals::Resource::Disk),
            req.machine_only,
            Instant::now(),
            at_unix_ms,
        );
        self.holdings.refresh(
            req.lenses.contains(&aterm_vitals::Resource::Disk),
            req.machine_only,
            Instant::now(),
            &req.live_ids,
        );
        let mut disk = disk_facts(shelf, key.as_ref(), Clock::read, &self.survey, &self.home);
        (disk.deeds, disk.removals) = self.journal.facts(&req.session_ids, &req.tabs, at_unix_ms);
        disk.journal_read = self.journal.read;
        disk.held = self.holdings.rows();
        disk.held_age_s = self.holdings.age_s(Instant::now());
        // aterm's own threads only where a pull that prints them asked (`Request::self_cpu`), or
        // where the reader reads none and answers why at once (off macOS, `unsupported`): a
        // reading that reads none leaves the model's continuity cleared, which such a pull's
        // pair restores (`Source::self_cpu_cold`).
        let self_cpu = self.self_cpu_read.sample(
            self.epoch,
            self.fixture.as_ref().is_some_and(WhyFixture::is_recorded),
            req.self_cpu,
        );
        self.self_cpu_last = matches!(self_cpu, Some(Ok(_))).then(self.clock);
        let mut sample = Sample {
            self_cpu,
            self_cpu_read: req.self_cpu,
            hardware: None,
            scheduling_read: false,
            at_ns: u64::try_from(since.as_nanos()).unwrap_or(u64::MAX),
            cores: self.probe.core_ticks().map(|cores| {
                cores
                    .iter()
                    .map(|t| Ticks {
                        busy: t.busy(),
                        total: t.total(),
                    })
                    .collect()
            }),
            clusters: self.clusters.clone(),
            mem: aterm_sysprobe::memory_detail()
                .map(|detail| mem_sample(compression_named(detail, procs))),
            cpu_millicores: capacity.cpu_millicores,
            mem_bytes: capacity.mem_bytes,
            host_watts: self.heat.host_watts(gpu.as_ref()),
            gpu: gpu.map(gpu_sample),
            gpu_clients: None,
            ane: self.ane,
            volume: volume.map(|v| VolumeSample {
                total: v.total,
                free: v.free,
                reserved: v.reserved,
                mount: v.mount.display().to_string(),
            }),
            procs: None,
            stalled: Vec::new(),
            self_pid: req.self_pid,
            tabs: req.tabs.clone(),
            delegated: Vec::new(),
            shared_roots: Vec::new(),
            unproved_tabs: Vec::new(),
            native_roots: Vec::new(),
            native_terminals: Vec::new(),
            native_incomplete: false,
            native_source_lost: false,
            native_cpu_cut: Default::default(),
            ownership_incomplete: false,
            at_unix_ms,
            mono_ms: u64::try_from(since.as_millis()).unwrap_or(u64::MAX),
            cadence_ms: req.cadence_ms,
            pull: req.pull,
            machine_only: req.machine_only,
            load1: load.map(|l| l[0]),
            load,
            cpu_brand: self.machine.cpu.clone(),
            thermal: (self.thermal)().or_else(|| self.counter_throttle()),
            thermal_pressure: self.heat.pressure(),
            clock_range: self.heat.range,
            swapped_kib: aterm_sysprobe::swapped_kib(),
            links,
            tcp: tcp_sum.map(|s| tcp_counts(s.counts, s.counters_only)),
            tcp_srtt_ms: tcp_sum.and_then(|s| s.srtt_ms),
            aterm_sockets: None,
            tab_sockets: Vec::new(),
            room: task_room(room),
            run_queue: run_queue_read(room.run_queue),
            dstate: aterm_vitals::blockage::DStateRead::Unsupported,
            // Windows' sweep reads a state from each process's threads and never an unreaped
            // one (Windows keeps no zombie in its table), so its count is unread there, never 0.
            zombie_read: if cfg!(any(target_os = "macos", target_os = "linux")) {
                aterm_vitals::blockage::ZombieRead::Sweep
            } else {
                aterm_vitals::blockage::ZombieRead::Unsupported
            },
            starts: None,
            self_uid: self.uid,
            disk: Some(disk),
            tokens: req
                .tokens
                .iter()
                .map(|row| TabTokens {
                    read_ms: row.read_at.map(|at| mono_ms_of(at, self.epoch)),
                    ..row.totals.clone()
                })
                .collect(),
            pane_token_observations: req.pane_token_observations.clone(),
            plan: req.plan.clone(),
            procs_truncated: false,
            coalitions: None,
            leads: Vec::new(),
            timeline: None,
            coal_lane: self.split.lane,
            note_events: req.note_events.clone(),
            event_sources: event_sources(req),
            note_gaps: req.note_gaps.clone(),
            block,
            home_device_id,
            situation: self.situation.read(at_unix_ms),
            // What a reading takes of its surroundings is handed in once it is whole
            // ([`Self::situate`]).
            place: None,
            stuck_work: None,
            // A whole reading runs `mdutil` where its cadence says ([`Self::read`]).
            spotlight_volumes: None,
        };
        // After the core ticks: Linux's `procs_blocked` comes from their read of `/proc/stat`.
        sample.dstate = dstate_read(&self.probe);
        sample
    }
}

/// What a reading reads of uninterruptible waits: Linux's sweep states with `procs_blocked`,
/// macOS's census (a whole reading adds its pass, [`crate::vitals_census`]), none elsewhere.
fn dstate_read(probe: &aterm_sysprobe::Probe) -> aterm_vitals::blockage::DStateRead {
    use aterm_vitals::blockage::DStateRead;
    if cfg!(target_os = "linux") {
        DStateRead::Sweep {
            blocked: probe.blocked(),
        }
    } else if cfg!(target_os = "macos") {
        DStateRead::Census(None)
    } else {
        DStateRead::Unsupported
    }
}

/// The model's copy of the run queue read with the task tables.
const fn run_queue_read(q: aterm_sysprobe::runq::RunQueue) -> aterm_vitals::runq::RunQueueRead {
    use aterm_sysprobe::runq::RunQueue as Q;
    use aterm_vitals::runq::RunQueueRead as R;
    match q {
        Q::Milli(m) => R::Milli(m),
        Q::Failed => R::Failed,
        Q::UnreadFormat => R::UnreadFormat,
        Q::Unsupported => R::Unsupported,
    }
}

fn mono_ms_of(at: Instant, epoch: Instant) -> i64 {
    match at.checked_duration_since(epoch) {
        Some(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        None => i64::try_from(epoch.duration_since(at).as_millis()).map_or(i64::MIN, |ms| -ms),
    }
}

/// Counts open `AF_INET`/`AF_INET6` sockets across processes attributed to `self_pid` and `tabs`.
fn read_sockets(
    procs: &[ProcSample],
    self_pid: u32,
    tabs: &[TabRoot],
    open_sockets: fn(u32) -> Option<u32>,
) -> (Option<u32>, Vec<(u64, u32)>) {
    let membership = aterm_vitals::model::attribute(procs, self_pid, tabs);
    let mut aterm_total = 0u32;
    let mut any_read = false;
    let mut per_tab: Vec<(u64, u32)> = tabs.iter().map(|t| (t.key, 0u32)).collect();
    for (p, m) in procs.iter().zip(membership) {
        if !m.aterm && m.tab.is_none() {
            continue;
        }
        if let Some(n) = open_sockets(p.pid) {
            any_read = true;
            if m.aterm {
                aterm_total = aterm_total.saturating_add(n);
            }
            if let Some(ti) = m.tab
                && let Some(slot) = per_tab.get_mut(ti)
            {
                slot.1 = slot.1.saturating_add(n);
            }
        }
    }
    if any_read {
        (Some(aterm_total), per_tab)
    } else {
        (None, Vec::new())
    }
}

/// The model's process-state vocabulary for one original native observation.
pub(crate) const fn proc_state(state: RunState) -> ProcState {
    match state {
        RunState::Running | RunState::Sleeping | RunState::Idle => ProcState::Run,
        RunState::Uninterruptible => ProcState::Uninterruptible,
        RunState::Stopped => ProcState::Stopped,
        RunState::Zombie => ProcState::Zombie,
        RunState::Unknown => ProcState::Unknown,
    }
}

/// Converts a swept [`aterm_sysprobe::ProcessReading`] into a [`ProcSample`].
pub(crate) fn proc_sample(p: &aterm_sysprobe::ProcessReading, epoch: Instant) -> ProcSample {
    ProcSample {
        scheduling: p.scheduling.map(|s| aterm_vitals::scheduling::Scheduling {
            nice: s.nice,
            application: s.application,
            background_internal: s.background_internal,
            background_external: s.background_external,
            suppressed: s.suppressed,
            role: s.role,
            qos_cpu_ns: s.qos_cpu_ns,
        }),
        pid: p.pid,
        start: p.start,
        ppid: p.ppid,
        cpu_ns: p.cpu_ns,
        child_cpu_ns: p.child_cpu_ns,
        footprint: p.footprint,
        name: p.name.clone(),
        disk_read: p.disk_read,
        disk_written: p.disk_written,
        energy_nj: p.energy_nj,
        uid: p.uid,
        pgid: p.pgid,
        sid: p.sid,
        state: proc_state(p.state),
        runnable_ns: p.runnable_ns,
        peak: p.peak_footprint,
        at_ns: p.read_at.map(|t| {
            u64::try_from(t.saturating_duration_since(epoch).as_nanos()).unwrap_or(u64::MAX)
        }),
        coalition: p.coalition,
        version: p.version,
        birth_ms: p.birth_ms,
        reaps_unwaited: p.reaps_unwaited,
    }
}

/// `detail` with Windows' memory compression store named as its compressed class from the
/// whole reading's sweep (`aterm_sysprobe::windows::with_compression_store`): Windows' memory
/// read names no compressed pages, and its store is a process the sweep lists. A machine-only
/// reading has no sweep (`procs` is `None`), so its compressed class is unread; off Windows
/// `detail` is as the memory read gave it.
fn compression_named(
    detail: aterm_sysprobe::MemoryDetail,
    procs: Option<&[aterm_sysprobe::ProcessReading]>,
) -> aterm_sysprobe::MemoryDetail {
    match procs {
        Some(procs) if cfg!(windows) => {
            aterm_sysprobe::windows::with_compression_store(detail, procs)
        }
        _ => detail,
    }
}

fn mem_sample(m: aterm_sysprobe::MemoryDetail) -> MemSample {
    MemSample {
        total: m.total,
        app: m.app.saturating_add(m.shared.unwrap_or(0)),
        wired: m.wired,
        compressed: m.compressed,
        stored: m.stored,
        // macOS's file-backed pages hold its speculative ones: adding them again would count
        // them twice (A1 §4.3, amended by R4).
        cached: m.file_backed.saturating_add(m.purgeable.unwrap_or(0)),
        free: m.free,
        available: m.available,
        swap_used: m.swap_used,
        swap_total: m.swap_total,
        pressure: match m.pressure {
            Some(aterm_sysprobe::MemoryLevel::Normal) => Pressure::Normal,
            Some(aterm_sysprobe::MemoryLevel::Warn) => Pressure::Warn,
            Some(aterm_sysprobe::MemoryLevel::Critical) => Pressure::Critical,
            None => Pressure::Unknown,
        },
        commit: m.commit.map(|c| CommitSample {
            charge: c.charge,
            limit: c.limit,
            exact: c.exact,
        }),
        counters: m.counters.map(|c| MemCounters {
            swapin_bytes: c.swapin_bytes,
            swapout_bytes: c.swapout_bytes,
            decompressions: c.decompressions,
            compressions: c.compressions,
            pageins: c.pageins,
            page_bytes: c.page_bytes,
            pushed: c.pushed,
            refaulted: c.refaulted,
            stall_us: c.stall_us,
        }),
        memorystatus_level_pct: m.memorystatus_level_pct,
        exact: m.exact,
        headroom: m.headroom,
        wall: m.wall,
        kept_permille: m.kept_permille(),
    }
}

#[cfg(not(windows))]
fn block_sample(b: aterm_sysprobe::blockio::BlockReading) -> aterm_vitals::BlockSample {
    use aterm_sysprobe::blockio::BlockLocation;
    use aterm_vitals::DiskLocation;
    aterm_vitals::BlockSample {
        id: b.id,
        name: b.name,
        location: match b.location {
            BlockLocation::Internal => DiskLocation::Internal,
            BlockLocation::External => DiskLocation::External,
            BlockLocation::Image => DiskLocation::Image,
            BlockLocation::Unknown => DiskLocation::Unknown,
        },
        read_bytes: b.counters.read_bytes,
        write_bytes: b.counters.write_bytes,
        reads: b.counters.reads,
        writes: b.counters.writes,
        read_ns: b.counters.read_ns,
        write_ns: b.counters.write_ns,
    }
}

/// The Neural Engine reader's one reading, made as the host's reader starts (A2 §3.1): its
/// self-check's start (the engine's class matched, its core count) and one power-state read, or
/// the check that failed; `None` where the OS has no reader.
fn ane_at_start(
    engine: Option<aterm_sysprobe::neural_engine::Reader>,
) -> Option<Result<AneSample, AneCheck>> {
    engine.map(|mut engine| ane_sample(engine.read()))
}

/// The Neural Engine reader's answer as the model takes it.
fn ane_sample(
    read: Result<aterm_sysprobe::neural_engine::Reading, aterm_sysprobe::neural_engine::Check>,
) -> Result<AneSample, AneCheck> {
    use aterm_sysprobe::neural_engine::Check;
    read.map(|r| AneSample {
        cores: r.cores,
        powered: r.powered,
    })
    .map_err(|check| match check {
        Check::Matched => AneCheck::Matched,
        Check::Cores => AneCheck::Cores,
        Check::PowerState => AneCheck::PowerState,
        Check::Cost => AneCheck::Cost,
    })
}

fn gpu_sample(g: aterm_sysprobe::GpuReading) -> GpuSample {
    let asleep = g.unread == Some(aterm_sysprobe::Why::Asleep);
    GpuSample {
        device_pct: g.device_pct.or(asleep.then_some(0)),
        cores: g.cores,
        name: g.name,
        in_use_bytes: g.in_use_bytes,
        alloc_bytes: g.alloc_bytes,
        recoveries: g.recoveries,
        renderer_pct: g.renderer_pct,
        tiler_pct: g.tiler_pct,
        unified: g.unified,
    }
}

fn link_sample(l: &aterm_sysprobe::LinkReading) -> LinkSample {
    let named = |prefixes: &[&str]| prefixes.iter().any(|p| l.name.starts_with(p));
    LinkSample {
        name: l.name.clone(),
        index: l.index,
        byte_counter: match l.byte_counter {
            aterm_sysprobe::LinkByteCounter::Unknown => {
                aterm_vitals::net_counter::ByteCounter::Unknown
            }
            aterm_sysprobe::LinkByteCounter::Cumulative64 => {
                aterm_vitals::net_counter::ByteCounter::Cumulative64
            }
            aterm_sysprobe::LinkByteCounter::DarwinModulo32 => {
                aterm_vitals::net_counter::ByteCounter::DarwinModulo32
            }
        },
        kind: match l.kind {
            aterm_sysprobe::LinkKind::Wired => LinkKind::Ethernet,
            aterm_sysprobe::LinkKind::Wifi => LinkKind::Wifi,
            aterm_sysprobe::LinkKind::Cellular => LinkKind::Cellular,
            aterm_sysprobe::LinkKind::Other if named(&["utun", "ipsec", "ppp", "tun", "wg"]) => {
                LinkKind::Vpn
            }
            aterm_sysprobe::LinkKind::Other if named(&["bridge", "br-", "virbr"]) => {
                LinkKind::Bridge
            }
            aterm_sysprobe::LinkKind::Other => LinkKind::Other,
        },
        up: l.up,
        loopback: l.loopback,
        counts: aterm_sysprobe::counts_toward_machine(l),
        rx: l.rx.bytes,
        tx: l.tx.bytes,
        rx_packets: l.rx.packets,
        tx_packets: l.tx.packets,
        rx_errors: l.rx.errors,
        tx_errors: l.tx.errors,
        rx_drops: l.rx.drops,
        tx_drops: if cfg!(target_os = "macos") {
            None
        } else {
            Some(l.tx.drops)
        },
        rate_bps: l.rate_bps,
    }
}

const fn tcp_counts(t: aterm_sysprobe::TcpCounts, counters_only: bool) -> aterm_vitals::TcpCounts {
    aterm_vitals::TcpCounts {
        established: t.established,
        syn_sent: t.syn_sent,
        syn_recv: t.syn_recv,
        fin_wait1: t.fin_wait1,
        fin_wait2: t.fin_wait2,
        time_wait: t.time_wait,
        close: t.close,
        close_wait: t.close_wait,
        last_ack: t.last_ack,
        listen: t.listen,
        closing: t.closing,
        counters_only,
    }
}

const fn task_room(r: aterm_sysprobe::TaskRoom) -> TaskRoom {
    TaskRoom {
        procs: r.procs,
        procs_max: r.procs_max,
        user_procs: r.user_procs,
        user_procs_max: r.user_procs_max,
        threads: r.threads,
        threads_max: r.threads_max,
        files: r.files,
        files_max: r.files_max,
    }
}

/// The volume holding a tab's directory, for `sys disk`, read as the home volume is.
pub(crate) fn tab_volume(path: &Path) -> Option<aterm_sysprobe::VolumeReading> {
    #[cfg(windows)]
    {
        aterm_sysprobe::volume(path)
    }
    #[cfg(not(windows))]
    {
        local_volume(
            path,
            &aterm_sysprobe::volumes::table(),
            aterm_sysprobe::volume,
        )
    }
}

/// What a machine reading reads of the volumes, from one mount table.
struct MachineVolumes {
    /// Home's volume.
    home: Option<aterm_sysprobe::VolumeReading>,
    /// Its key, while the shelf watches it.
    key: Option<VolumeKey>,
    /// The readable devices under home's volume and the front tab's, in request order.
    block: Vec<aterm_vitals::BlockSample>,
    /// Home's device bound during this same block read, before counter refusal or deduplication.
    home_device_id: Option<String>,
}

/// Home's volume, its key and the devices under home's and `front`'s volumes, which
/// `read_block` reads (the reader's [`aterm_sysprobe::blockio::BlockReader::read`]).
fn machine_volumes(
    home: &Path,
    keyed: bool,
    front: Option<&Path>,
    read_block: impl FnOnce(&[&aterm_sysprobe::volumes::Mount]) -> aterm_sysprobe::blockio::BlockBatch,
) -> MachineVolumes {
    #[cfg(windows)]
    {
        // No block reader on Windows: the Disk row draws no units (A1 §4.4). The key is the
        // one the harness's disk watch files home's volume under, so the row shows its facts.
        let _ = (front, read_block);
        let key = keyed
            .then(|| {
                aterm_agent::harness::disk::volume_of(home, &aterm_sysprobe::volumes::table()).ok()
            })
            .flatten()
            .map(|r| r.key);
        MachineVolumes {
            home: aterm_sysprobe::volume(home),
            key,
            block: Vec::new(),
            home_device_id: None,
        }
    }
    #[cfg(not(windows))]
    {
        let table = aterm_sysprobe::volumes::table();
        let key = keyed
            .then(|| aterm_agent::harness::disk::volume_of(home, &table).ok())
            .flatten()
            .map(|r| r.key);
        let volume = local_volume(home, &table, aterm_sysprobe::volume);
        let mounts = block_mounts(home, front, &table);
        let batch = read_block(&mounts.mounts);
        let home_device_id = mounts.home_device_id(&batch);
        MachineVolumes {
            home: volume,
            key,
            block: batch.devices.into_iter().map(block_sample).collect(),
            home_device_id,
        }
    }
}

/// Original block requests with the home role kept independently of missing mounts.
#[cfg(not(windows))]
#[derive(Default)]
struct BlockMounts<'t> {
    mounts: Vec<&'t aterm_sysprobe::volumes::Mount>,
    home: Option<usize>,
}

#[cfg(not(windows))]
impl BlockMounts<'_> {
    fn home_device_id(&self, batch: &aterm_sysprobe::blockio::BlockBatch) -> Option<String> {
        if batch.mounts.len() != self.mounts.len() {
            return None;
        }
        self.home
            .and_then(|index| batch.mounts.get(index))
            .and_then(Option::as_deref)
            .filter(|id| aterm_vitals::units::is_device_id(id))
            .map(str::to_owned)
    }
}

/// The local mounts holding `home` and `front`, home's first, each once: a front tab under home's
/// volume (the usual case) asks the block reader for one mount, not the same one twice. The home
/// role remains unknown when home's mount is absent, even when the front supplies request zero.
#[cfg(not(windows))]
fn block_mounts<'t>(
    home: &Path,
    front: Option<&Path>,
    table: &'t aterm_sysprobe::volumes::Table,
) -> BlockMounts<'t> {
    let mut out = BlockMounts::default();
    for (is_home, path) in std::iter::once((true, home)).chain(front.map(|path| (false, path))) {
        let Some(mount) = local_mount(path, table) else {
            continue;
        };
        let index = match out.mounts.iter().position(|other| other.on == mount.on) {
            Some(index) => index,
            None => {
                let index = out.mounts.len();
                out.mounts.push(mount);
                index
            }
        };
        if is_home {
            out.home = Some(index);
        }
    }
    out
}

/// The mount holding `path` where a reading reads it: a local volume the automounter did not
/// mount, from a complete table.
#[cfg(not(windows))]
fn local_mount<'t>(
    path: &Path,
    table: &'t aterm_sysprobe::volumes::Table,
) -> Option<&'t aterm_sysprobe::volumes::Mount> {
    if !table.complete {
        return None;
    }
    aterm_sysprobe::volumes::mount_of(path, table)
        .filter(|m| m.local && !m.automounted && m.fstype != "autofs")
}

#[cfg(not(windows))]
fn local_volume(
    path: &Path,
    table: &aterm_sysprobe::volumes::Table,
    mut read: impl FnMut(&Path) -> Option<aterm_sysprobe::VolumeReading>,
) -> Option<aterm_sysprobe::VolumeReading> {
    read(&local_mount(path, table)?.on)
}

/// The Disk row's facts for the home volume `home_key`: the watch's level, lines and passes
/// with the survey's folders ([`deck_disk`]), and DC-04's budget as the watch last sized it
/// ([`DiskFacts::budget`]: its cap, and how far the rebuildable bytes are from its target, which
/// the Disk Custody band says). With the harness off (no watch) it reads no clock and no board
/// fact, and the facts are the default.
fn disk_facts(
    shelf: &Shelf,
    home_key: Option<&VolumeKey>,
    now: impl FnOnce() -> Clock,
    survey: &Where,
    home: &Path,
) -> DiskFacts {
    let (raw, budget, (cleanup, budget_unmet_unix)) = match home_key {
        Some(key) if shelf.watched() => {
            let clock = now();
            // The level and the budget are one board state: one guard reads both, and it is
            // held across no other lock.
            let board = shelf.board_lock();
            let budget = board.budget_read(Some(key), clock.unix);
            (
                board.deck_facts(key, &clock),
                budget,
                board.budget_pass(Some(key), budget, clock.unix),
            )
        }
        _ => (
            DiskFactsRaw::default(),
            aterm_vitals::sys::BudgetRead::NotSampled,
            (None, None),
        ),
    };
    let (found, sizing) = survey.read();
    DiskFacts {
        budget,
        cleanup,
        budget_unmet_unix,
        ..deck_disk(raw, found, sizing, home)
    }
}

fn deck_disk(
    raw: DiskFactsRaw,
    found: Option<crate::disk_where::Found>,
    sizing: bool,
    home: &Path,
) -> DiskFacts {
    let ms = |s: i64| s.saturating_mul(1000);
    let pass_at = raw.hogs_at.map(ms);
    let whole_and_newer =
        |f: &crate::disk_where::Found| !f.cut && !f.hogs.is_empty() && f.at_unix_ms > pass_at;
    let (hogs, hogs_at) = match found {
        Some(f) if raw.hogs.is_empty() || whole_and_newer(&f) => (f.hogs, f.at_unix_ms),
        _ => (
            raw.hogs
                .iter()
                .map(|h| Hog {
                    path: crate::disk_where::shown(&h.path, home),
                    bytes: h.bytes,
                    partial: h.bytes_partial,
                    remedy: h.why.clone(),
                })
                .collect(),
            pass_at,
        ),
    };
    DiskFacts {
        level: raw.level.and_then(|l| disk_level(l, raw.files_emergency)),
        warn: raw.warn,
        floor: raw.floor,
        emergency: raw.emergency,
        stuck: raw.stuck,
        others: raw
            .others
            .into_iter()
            .filter_map(|(name, level, free)| Some((name, disk_level(level, false)?, free)))
            .collect(),
        stalled_since: raw.stalled_since.map(ms),
        freed_today: raw
            .freed_today
            .map(|t| (t.aterm_bytes, ms(t.last_at), t.last_what)),
        hogs,
        hogs_at,
        watch: raw.watch,
        sizing,
        deeds: Vec::new(),
        removals: Vec::new(),
        held: None,
        held_age_s: None,
        budget: aterm_vitals::sys::BudgetRead::NotSampled,
        journal_read: false,
        cleanup: None,
        budget_unmet_unix: None,
    }
}

const fn disk_level(level: aterm_agent::harness::disk::Level, files: bool) -> Option<DiskLevel> {
    use aterm_agent::harness::disk::Level;
    match level {
        Level::Unknown => None,
        Level::Ok => Some(DiskLevel::Ok),
        Level::Low => Some(DiskLevel::Low),
        Level::Short => Some(DiskLevel::Short),
        Level::Emergency if files => Some(DiskLevel::EmergencyFiles),
        Level::Emergency => Some(DiskLevel::Emergency),
    }
}

fn tab_tokens(key: u64, facts: &FooterFacts, read_at: Option<Instant>) -> Option<TokenRow> {
    let usage = facts.usage.as_ref()?;
    let owner = facts.owner.as_ref()?;
    Some(TokenRow {
        totals: TabTokens {
            key,
            session: owner.session_id.clone(),
            out_total: usage.output_total(),
            in_total: usage.input_total(),
            unread: usage.unread,
            unread_more: usage.unread_more,
            complete: usage.complete,
            read_ms: None,
        },
        read_at,
    })
}

fn each_session_once(mut rows: Vec<TokenRow>) -> Vec<TokenRow> {
    let mut seen = std::collections::HashSet::with_capacity(rows.len());
    rows.retain(|t| seen.insert(t.totals.session.clone()));
    rows
}

fn plan_fact(plan: &PlanUsage, accounts: &[AccountKey], now: i64) -> Option<PlanFact> {
    if !plan.current_at(now) {
        return None;
    }
    let ix = accounts
        .iter()
        .position(|a| *a == plan.account)
        .unwrap_or(accounts.len());
    Some(PlanFact {
        window_word: window_word(plan.window).to_owned(),
        used_x10: plan.used_x10,
        read_at: plan.read_at.saturating_mul(1000),
        account: AccountIx(u8::try_from(ix).unwrap_or(u8::MAX)),
    })
}

const fn window_word(window: Bucket) -> &'static str {
    match window {
        Bucket::FiveHour => "5-hour",
        Bucket::SevenDay | Bucket::Other => "weekly",
        Bucket::SevenDayOpus => "Opus weekly",
        Bucket::SevenDaySonnet => "Sonnet weekly",
        Bucket::Fable => "Fable weekly",
    }
}

/// A reading the probe thread keeps in a small JSON file: read at the thread's start, then written
/// atomically when it changed, at most once every `every` and again at the thread's stop.
struct KeptFile<T> {
    path: Option<PathBuf>,
    held: T,
    at: Instant,
    every: Duration,
    /// What the warning of a failed write calls it.
    what: &'static str,
    json: fn(&T) -> Option<String>,
}

/// [`PEAKS_FILE`].
type PeakFile = KeptFile<Peaks>;
/// [`FREE_FILE`].
type FreeFile = KeptFile<FreeHistory>;
/// [`WEEKS_FILE`].
type WeeksFile = KeptFile<FreeWeeks>;
/// [`SPOTLIGHT_FILE`].
type SpotlightFile = KeptFile<SpotlightHistory>;
type CpuReportFile = KeptFile<Option<aterm_vitals::sys::json::Jv>>;

impl<T: Clone + Default + PartialEq> KeptFile<T> {
    /// The file and what `of` reads from its text; `T::default()` when it is missing, over
    /// `max_bytes` or refused by `of`.
    fn read(
        path: Option<PathBuf>,
        now: Instant,
        every: Duration,
        max_bytes: u64,
        what: &'static str,
        json: fn(&T) -> Option<String>,
        of: impl FnOnce(&str) -> Option<T>,
    ) -> (Self, T) {
        let held = path
            .as_deref()
            .and_then(|p| read_bounded(p, max_bytes))
            .and_then(|text| of(&text))
            .unwrap_or_default();
        let file = Self {
            path,
            held: held.clone(),
            at: now,
            every,
            what,
            json,
        };
        (file, held)
    }

    fn keep(&mut self, value: &T, now: Instant, stopping: bool) {
        if self.due(value, now, stopping) {
            self.write(value, now);
        }
    }

    /// [`Self::keep`] for a file only [`STRIP_LOCK`]'s holder writes: a write that is due first
    /// tries a live peer's lock again, and while the peer still holds it nothing is written and the
    /// write stays due, for the next reading's checkpoint to try.
    fn keep_owned(&mut self, value: &T, now: Instant, stopping: bool, lock: &mut StripLock) {
        if self.due(value, now, stopping) && lock.writes() {
            self.write(value, now);
        }
    }

    /// Whether `value` is to be written at `now`: it changed, and `every` has passed since the last
    /// write or the thread is stopping.
    fn due(&self, value: &T, now: Instant, stopping: bool) -> bool {
        (stopping || now.saturating_duration_since(self.at) >= self.every) && *value != self.held
    }

    fn write(&mut self, value: &T, now: Instant) {
        let (Some(path), Some(text)) = (self.path.as_deref(), (self.json)(value)) else {
            return;
        };
        self.at = now;
        match aterm_agent::harness::atomic_write::write_atomic(path, &text) {
            Ok(()) => self.held = value.clone(),
            Err(error) => aterm_log::warn!(
                "system vitals: {} could not be written to {}: {error}",
                self.what,
                path.display()
            ),
        }
    }
}

impl KeptFile<Peaks> {
    fn open(path: Option<PathBuf>, now: Instant) -> (Self, Peaks) {
        Self::read(
            path,
            now,
            PEAKS_EVERY,
            PEAKS_MAX_BYTES,
            "the peaks",
            peaks_json,
            peaks_of,
        )
    }
}

impl KeptFile<FreeHistory> {
    /// The file's history of the 24 hours ending at `now_unix_ms`; empty when it is missing,
    /// damaged or of another schema.
    fn open(path: Option<PathBuf>, now: Instant, now_unix_ms: i64) -> (Self, FreeHistory) {
        Self::read(
            path,
            now,
            PEAKS_EVERY,
            FREE_MAX_BYTES,
            "the disk's free history",
            free_json,
            |text| free_of(text, now_unix_ms),
        )
    }
}

impl KeptFile<FreeWeeks> {
    /// The file's history of the 3 weeks ending at `now_unix_ms`; empty when it is missing,
    /// damaged or of another schema.
    fn open(path: Option<PathBuf>, now: Instant, now_unix_ms: i64) -> (Self, FreeWeeks) {
        Self::read(
            path,
            now,
            PEAKS_EVERY,
            WEEKS_MAX_BYTES,
            "the disk's three-week history",
            weeks_json,
            |text| weeks_of(text, now_unix_ms),
        )
    }
}

impl KeptFile<SpotlightHistory> {
    /// The file's Spotlight day ending at `now_unix_ms`; empty when it is missing, damaged or of
    /// another schema.
    fn open(path: Option<PathBuf>, now: Instant, now_unix_ms: i64) -> (Self, SpotlightHistory) {
        Self::read(
            path,
            now,
            PEAKS_EVERY,
            SPOTLIGHT_MAX_BYTES,
            "the Spotlight family's history",
            spotlight_json,
            |text| spotlight_of(text, now_unix_ms),
        )
    }
}

/// What a launch knew of how the run before it stopped, which [`open_strip`] reads the kept
/// checkpoint by.
#[derive(Clone, Copy, Debug, Default)]
struct PriorRun {
    /// [`VitalsHost::handed_off`].
    handed_off: bool,
    /// [`VitalsHost::crashed_at`].
    crashed_at: Option<i64>,
    /// [`VitalsHost::killed_since`].
    killed_since: Option<i64>,
}

/// [`strip::StripStore::open`] in the state folder `home` at `now_ms` on the wall clock for a start
/// that knows how the run before it stopped (`prior`): whether it stopped on purpose ([`VitalsHost::handed_off`]), when a
/// crashed one crashed ([`VitalsHost::crashed_at`]), and when a killed one launched
/// ([`VitalsHost::killed_since`]). A checkpoint whose writer still runs, another aterm of this
/// state home holding [`STRIP_LOCK`] (`lock` [`StripLock::Peer`]), is marked clean, so the restore
/// treats that stop as a graceful one: a pause where it spans a 10 s bucket, and never a crash.
/// That writer has not stopped at all (a holder whose restore drew the run before it as a crash
/// writes its own checkpoint as soon as it has restored, [`restore_strip`], so a start beside it
/// finds that crash in the holder's checkpoint). Otherwise, after a kill, a kept strip that holds
/// no checkpoint of the killed run gains one at its launch
/// ([`crate::vitals_startup::date_unseen_kill`]), and a crash event newer than the kept strip as
/// read dates the crash by its time ([`crate::vitals_startup::date_crash`]), the two in the order
/// they came (one older than the launch is an earlier run's, which the killed run never took), and
/// the crash event after a handoff too: no checkpoint says the run went on past that event, so a
/// panic it caught and survived in the seconds between its last checkpoint and its handoff reads as
/// a crash too. A strip is newer than the event by its last reading, and a clean one also by its
/// file's last write: a quit's clean write takes no reading of its own, so a run that survived a
/// panic (on any thread but the probe's) after its last reading and then quit wrote its clean stop
/// after the crash event, and only a quit writes a stop clean ([`VitalsHost::stopping`], which names
/// the one late panic that still leaves one), never a probe thread that outlives the event loop of
/// a run its main thread's panic takes down. A panic on the probe thread ends it with no stop
/// written, so its run's quit leaves the last checkpoint, unclean, and that crash draws. A seamless
/// update's predecessor `_exit`s at its Commit (`seamless::commit_and_exit`) and a relaunch's may
/// still be running when this opens, so neither leaves `clean_stop = true`, and a handoff's start
/// with no crash event that new marks the kept checkpoint clean the same way. A crash event no newer
/// than the kept strip dates nothing: the strip's writer ran past it (a panic it caught and
/// survived, or another aterm's crash).
fn open_strip(
    home: Option<&Path>,
    now_ms: i64,
    prior: PriorRun,
    lock: &StripLock,
) -> (StripStore, SavedStrip) {
    let (file, mut kept) = StripStore::open(
        home.map(strip::strip_dir),
        home.map(|h| h.join(strip::LEGACY_FILE)),
    );
    let written_ms = file.written_ms().unwrap_or(0);
    if lock.peer_holds() {
        kept.clean_stop = true;
        return (file, kept);
    }
    let newer = if kept.clean_stop {
        kept.last_wall_ms.max(written_ms)
    } else {
        kept.last_wall_ms
    };
    let crash_ms = prior
        .crashed_at
        .filter(|&crash_ms| newer < crash_ms && crash_ms < now_ms);
    // The stops are dated in the order they came. A crash event older than the killed run's launch
    // is an earlier run's, which that run, killed before its startup scan, never took; the kill
    // dated after it moves the strip past the crash, so the crash's call below changes nothing.
    if let Some(crash_ms) = crash_ms.filter(|&ms| {
        prior
            .killed_since
            .is_some_and(|launched_ms| ms < launched_ms)
    }) {
        crate::vitals_startup::date_crash(&mut kept, crash_ms, now_ms);
    }
    if let Some(launched_ms) = prior.killed_since {
        crate::vitals_startup::date_unseen_kill(&mut kept, launched_ms, now_ms);
    }
    if let Some(crash_ms) = crash_ms {
        crate::vitals_startup::date_crash(&mut kept, crash_ms, now_ms);
    } else if prior.handed_off {
        kept.clean_stop = true;
    }
    (file, kept)
}

/// [`open_strip`]'s kept strip restored into `model` at `now` (`now_ms` on the wall clock), and the
/// checkpoint it is kept in. Where that strip is unclean and this thread holds [`STRIP_LOCK`], its
/// base is written at once, before any reading: until then the file holds the dead run's
/// unclean checkpoint, which an aterm started beside this live holder would mark clean and draw as
/// a pause where the restore here draws a crash. The checkpoint written carries that crash gap and
/// its "aterm crash" event, with the open as its last reading, so such a start draws the crash and
/// then a pause from the open. An unclean strip whose last reading is older than the strips' six
/// hours draws no crash here or beside, and is written back as restored, with that reading still
/// its last. After that write the checkpoint is armed again, as at its open, so the first
/// reading's base, which carries the marks the launch noted (its "restarted" event), is due at
/// once too. A clean kept strip (one that stopped clean on disk, or one [`open_strip`] marked
/// clean for a handoff or a live peer) is not written at once, and its first checkpoint waits for
/// the first reading: one written at once, after a stop under a 10 s bucket ago, would give the
/// earlier run's last reading as this run's unclean one. After a handoff the file keeps the
/// predecessor's unclean checkpoint until that first reading's checkpoint, so a kill before it
/// draws the handoff's span as a crash ([`crate::vitals_startup::date_unseen_kill`]), as before the
/// lock.
fn restore_strip(
    home: Option<&Path>,
    now: Instant,
    now_ms: i64,
    prior: PriorRun,
    lock: &mut StripLock,
    model: &mut aterm_vitals::Vitals,
) -> StripStore {
    let (mut file, kept) = open_strip(home, now_ms, prior, lock);
    let crashed = !kept.clean_stop;
    model.restore_strip_history(kept, now_ms);
    if crashed && matches!(lock, StripLock::Held { .. }) {
        file.keep(model, now, lock);
        file.arm();
    }
    file
}

/// Whether this probe thread writes the strip's checkpoint and [`PEAKS_FILE`]. Every aterm of one state
/// home reads them (an installed app and a dev app beside it, an update's successor while its
/// predecessor runs), and only the one whose probe thread holds [`STRIP_LOCK`] writes them. A
/// writer that crashes or is killed lets go with its process.
enum StripLock {
    /// This thread holds the lock, until it drops this after its stop's write.
    Held {
        _guard: crate::native_document_host::HeldAdvisoryLock,
    },
    /// Another live aterm holds it: its checkpoint is no crash ([`open_strip`]), and this thread
    /// writes neither file until a later checkpoint takes the lock, on this kept description.
    Peer(std::fs::File),
    /// No lock to be had (no state folder, a lock file that would not open, a filesystem without
    /// locks): this thread writes as though it held one.
    Unlocked,
}

impl StripLock {
    /// The lock at `path` taken without waiting.
    fn take(path: Option<PathBuf>) -> Self {
        path.and_then(|p| open_lock_file(&p).ok())
            .map_or(Self::Unlocked, Self::try_take)
    }

    /// `file`'s lock taken without waiting: held, a live peer's (`WouldBlock`), or, on any other
    /// refusal, none.
    fn try_take(file: std::fs::File) -> Self {
        match crate::native_document_host::HeldAdvisoryLock::try_lock_keeping(file) {
            Ok(guard) => Self::Held { _guard: guard },
            Err((file, std::fs::TryLockError::WouldBlock)) => Self::Peer(file),
            Err((_, std::fs::TryLockError::Error(_))) => Self::Unlocked,
        }
    }

    const fn peer_holds(&self) -> bool {
        matches!(self, Self::Peer(_))
    }

    /// Whether this thread may write now, trying a peer's lock again first.
    fn writes(&mut self) -> bool {
        *self = match std::mem::replace(self, Self::Unlocked) {
            Self::Peer(file) => Self::try_take(file),
            kept => kept,
        };
        !self.peer_holds()
    }
}

/// A lock file beside a kept file ([`STRIP_LOCK`], the closed-tab record's
/// [`crate::vitals_left_closed::LOCK`]) opened to be locked, read and write and created when
/// missing; on unix created 0600, never through a link and never waiting on what is there.
/// Anything but a regular file is refused.
pub(crate) fn open_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut open = std::fs::OpenOptions::new();
    open.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = open.open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the lock is not a regular file",
        ));
    }
    Ok(file)
}

impl CpuReportFile {
    fn open(
        path: Option<PathBuf>,
        now: Instant,
        now_ms: i64,
    ) -> (Self, Option<aterm_vitals::sys::json::Jv>) {
        Self::read(
            path,
            now.checked_sub(CPU_REPORT_EVERY).unwrap_or(now),
            CPU_REPORT_EVERY,
            CPU_REPORT_MAX_BYTES,
            "the historical CPU diagnostic report",
            cpu_report_json,
            |text| cpu_report_of(text, now_ms),
        )
    }
}

/// Serialize this process only; previous captures are never recursively copied into a new one.
fn cpu_report_record(snap: &Snapshot) -> Option<aterm_vitals::sys::json::Jv> {
    use aterm_vitals::sys::json::Jv;
    if snap.cpu_diagnostics.is_none() && snap.authorized_cpu_policy.is_none() {
        return None;
    }
    Some(Jv::obj([
        ("schema", Jv::Int(1)),
        (
            "kind",
            Jv::s(if snap.cpu_diagnostics.is_some() {
                "historical_anomaly"
            } else {
                "historical_cpu_policy"
            }),
        ),
        ("captured_unix_ms", Jv::Int(snap.at_unix_ms)),
        ("origin_pid", Jv::u(u64::from(std::process::id()))),
        ("origin_build", Jv::s(aterm_stamp::app_version())),
        (
            "origin_executable",
            std::env::current_exe()
                .ok()
                .map_or_else(|| Jv::why("not_sampled"), |p| Jv::s(p.to_string_lossy())),
        ),
        (
            "report",
            aterm_vitals::sys::hardware::current_report_json(snap),
        ),
    ]))
}

fn cpu_report_json(value: &Option<aterm_vitals::sys::json::Jv>) -> Option<String> {
    let text = value.as_ref()?.to_text();
    if text.len() as u64 > CPU_REPORT_MAX_BYTES {
        aterm_log::warn!(
            "system vitals: CPU diagnostic report exceeds the bounded file; retaining the earlier capture"
        );
        return None;
    }
    Some(text)
}

/// Validate a bounded historical document without feeding its monotonic state to the detector.
fn cpu_report_of(text: &str, now_ms: i64) -> Option<Option<aterm_vitals::sys::json::Jv>> {
    use aterm_json::Value;
    if text.len() as u64 > CPU_REPORT_MAX_BYTES {
        return None;
    }
    let value: Value = aterm_json::from_str(text).ok()?;
    if value["schema"].as_i64() != Some(1)
        || !matches!(
            value["kind"].as_str(),
            Some("historical_anomaly" | "historical_cpu_policy")
        )
        || value["origin_build"].as_str().is_none()
        || value["origin_pid"].as_u64().is_none_or(|pid| pid == 0)
    {
        return None;
    }
    let captured = value["captured_unix_ms"].as_i64()?;
    let report = &value["report"];
    if captured <= 0
        || captured > now_ms
        || report["version"].as_i64() != Some(1)
        || report["at_unix_ms"].as_i64() != Some(captured)
    {
        return None;
    }
    if value["kind"].as_str() == Some("historical_anomaly") {
        if report["anomaly"]["version"].as_i64() != Some(1) {
            return None;
        }
        let frames = report["anomaly"]["frames"].as_array()?;
        if frames.is_empty()
            || frames.len() > 8
            || frames.iter().any(|frame| {
                frame["at_unix_ms"]
                    .as_i64()
                    .is_none_or(|at| at <= 0 || at > captured)
            })
        {
            return None;
        }
    } else {
        let policy = &report["authorized_cpu_policy"];
        if policy["schema"].as_u64() != Some(1)
            || policy["operation"].as_str() != Some("cpu_policy")
            || policy["observed_unix_ms"]
                .as_i64()
                .is_none_or(|at| at <= 0 || at > captured)
            || policy["state"].as_str().is_none()
        {
            return None;
        }
    }
    Some(Some(convert_cpu_report_value(value, 0, &mut 200_000)?))
}

#[cfg(any(target_os = "macos", test))]
fn take_cpu_policy_report(
    current: &mut Option<(u64, u64)>,
    value: aterm_json::Value,
) -> Option<aterm_vitals::sys::json::Jv> {
    if value["schema"].as_u64() != Some(1)
        || value["operation"].as_str() != Some("cpu_policy")
        || value["state"].as_str().is_none()
        || value["observed_unix_ms"].as_u64().is_none_or(|at| at == 0)
    {
        return None;
    }
    let id = value["request_id"].as_u64()?;
    let revision = value["revision"].as_u64()?;
    let report = convert_cpu_report_value(value, 0, &mut 200_000)?;
    accept_cpu_policy_generation(current, id, revision).then_some(report)
}

#[cfg(any(target_os = "macos", test))]
fn accept_cpu_policy_generation(current: &mut Option<(u64, u64)>, id: u64, revision: u64) -> bool {
    let Some((expected, seen)) = current.as_mut() else {
        return false;
    };
    if *expected != id || !matches!(revision, 1 | 2) || revision <= *seen {
        return false;
    }
    *seen = revision;
    true
}

fn convert_cpu_report_value(
    v: aterm_json::Value,
    depth: usize,
    left: &mut usize,
) -> Option<aterm_vitals::sys::json::Jv> {
    use aterm_json::Value;
    use aterm_vitals::sys::json::Jv;
    if depth > 32 || *left == 0 {
        return None;
    }
    *left -= 1;
    Some(match v {
        Value::Bool(v) => Jv::Bool(v),
        Value::Number(v) => Jv::Int(v.as_i64()?),
        Value::String(v) => Jv::Str(v),
        Value::Array(rows) => Jv::Arr(
            rows.into_iter()
                .map(|v| convert_cpu_report_value(v, depth + 1, left))
                .collect::<Option<_>>()?,
        ),
        Value::Object(rows) => Jv::Obj(
            rows.into_iter()
                .map(|(k, v)| Some((k, convert_cpu_report_value(v, depth + 1, left)?)))
                .collect::<Option<_>>()?,
        ),
        Value::Null => return None,
    })
}

/// The text of the regular file at `path`; `None` when it does not open, holds more than
/// `max_bytes` or is not UTF-8.
pub(crate) fn read_bounded(path: &Path, max_bytes: u64) -> Option<String> {
    let (file, meta) = crate::logging::open_regular_nonblock_with_meta(path).ok()?;
    if meta.len() > max_bytes {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(max_bytes + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= max_bytes).then_some(text)
}

/// The peaks file. A `plan` array an earlier build wrote beside the peaks is an unknown field,
/// which the reader skips.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PeaksDoc {
    schema: u32,
    peaks: Vec<PeakDoc>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PeakDoc {
    name: String,
    read_s: u32,
    days: Vec<(u32, f64)>,
}

fn peaks_json(peaks: &Peaks) -> Option<String> {
    let doc = PeaksDoc {
        schema: PEAKS_SCHEMA,
        peaks: peaks
            .to_pairs()
            .into_iter()
            .map(|(name, read_s, days)| PeakDoc { name, read_s, days })
            .collect(),
    };
    aterm_json::to_string(&doc).ok()
}

pub(crate) fn peaks_of(text: &str) -> Option<Peaks> {
    let doc: PeaksDoc = aterm_json::from_str(text).ok()?;
    (doc.schema == PEAKS_SCHEMA).then(|| {
        Peaks::from_pairs(
            doc.peaks
                .iter()
                .map(|p| (p.name.as_str(), p.read_s, p.days.as_slice())),
        )
    })
}

/// The wall clock, unix ms; 0 where unreadable.
fn unix_ms_now() -> i64 {
    aterm_time::unix_millis()
        .and_then(|ms| i64::try_from(ms).ok())
        .unwrap_or(0)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct FreeDoc {
    schema: u32,
    mount: String,
    total: u64,
    /// `(bucket start in unix ms, τ ms, readings, over ms, mean, least and most free bytes)`,
    /// oldest first.
    buckets: Vec<(i64, u32, u16, u32, f64, f64, f64)>,
}

/// A free series as its file holds it ([`FreeDoc`]) at `schema`; `None` with nothing read.
fn series_json<const M: i64, const N: usize>(
    series: &FreeSeries<M, N>,
    schema: u32,
) -> Option<String> {
    let saved = series.saved()?;
    let doc = FreeDoc {
        schema,
        mount: saved.volume.mount,
        total: saved.volume.total,
        buckets: saved.rows.iter().map(strip::agg_doc).collect(),
    };
    aterm_json::to_string(&doc).ok()
}

/// The free series a file of `schema` holds, restored at `now_unix_ms`; `None` for a damaged file
/// or another schema.
fn series_of<const M: i64, const N: usize>(
    text: &str,
    schema: u32,
    now_unix_ms: i64,
) -> Option<FreeSeries<M, N>> {
    let doc: FreeDoc = aterm_json::from_str(text).ok()?;
    (doc.schema == schema).then(|| {
        let saved = SavedFree {
            volume: VolumeId {
                mount: doc.mount,
                total: doc.total,
            },
            rows: doc.buckets.into_iter().map(strip::agg_of).collect(),
        };
        FreeSeries::restored(&saved, now_unix_ms)
    })
}

fn free_json(history: &FreeHistory) -> Option<String> {
    series_json(history, FREE_SCHEMA)
}

fn free_of(text: &str, now_unix_ms: i64) -> Option<FreeHistory> {
    series_of(text, FREE_SCHEMA, now_unix_ms)
}

/// [`SPOTLIGHT_FILE`]'s form: one bucket list a figure, each as [`FreeDoc`]'s.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SpotlightDoc {
    schema: u32,
    cpu_mc: Vec<(i64, u32, u16, u32, f64, f64, f64)>,
    read_bps: Vec<(i64, u32, u16, u32, f64, f64, f64)>,
    footprint: Vec<(i64, u32, u16, u32, f64, f64, f64)>,
}

/// The Spotlight family's day as its file holds it; `None` with nothing read.
fn spotlight_json(history: &SpotlightHistory) -> Option<String> {
    let saved = history.saved()?;
    let doc = SpotlightDoc {
        schema: SPOTLIGHT_SCHEMA,
        cpu_mc: saved.cpu_mc.iter().map(strip::agg_doc).collect(),
        read_bps: saved.read_bps.iter().map(strip::agg_doc).collect(),
        footprint: saved.footprint.iter().map(strip::agg_doc).collect(),
    };
    aterm_json::to_string(&doc).ok()
}

/// The Spotlight day a file holds, restored at `now_unix_ms`; `None` for a damaged file or
/// another schema.
fn spotlight_of(text: &str, now_unix_ms: i64) -> Option<SpotlightHistory> {
    let doc: SpotlightDoc = aterm_json::from_str(text).ok()?;
    (doc.schema == SPOTLIGHT_SCHEMA).then(|| {
        let saved = SavedSpotlight {
            cpu_mc: doc.cpu_mc.into_iter().map(strip::agg_of).collect(),
            read_bps: doc.read_bps.into_iter().map(strip::agg_of).collect(),
            footprint: doc.footprint.into_iter().map(strip::agg_of).collect(),
        };
        SpotlightHistory::restored(&saved, now_unix_ms)
    })
}

/// Read every local macOS volume's Spotlight indexing without root, under bounded run and
/// reap intervals. A child that outlives those intervals is left to the reaper; other
/// platforms report unsupported.
fn mdutil_status_all() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        use aterm_types::macos_setup::spotlight_scope::MDUTIL;
        let mut status = aterm_uds::spawnfd::command(MDUTIL);
        status.args(aterm_vitals::spotlight::MDUTIL_ARGS);
        let out = aterm_uds::deadline::outputs_until(
            &mut [&mut status],
            Instant::now() + MDUTIL_WAIT,
            MDUTIL_REAP,
        )
        .pop()
        .unwrap_or(Err(aterm_uds::deadline::RunError::TimedOut))
        .map_err(|error| format!("{MDUTIL} -s -a: {error}"))?;
        if out.status.success() {
            String::from_utf8(out.stdout).map_err(|error| error.to_string())
        } else {
            Err(format!("{MDUTIL} -s -a: {}", out.status))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("unsupported".into())
    }
}

/// The live arm of the Spotlight volumes' port ([`aterm_vitals::spotlight::MdutilProbe`]).
const MDUTIL_LIVE: aterm_vitals::spotlight::MdutilProbe = aterm_vitals::spotlight::MdutilProbe {
    status_all: mdutil_status_all,
};

/// Spotlight's stores as aterm's root LAN broker last walked them (SD-94), in the model's terms:
/// each store and the answer's age. No broker installed, one too old to know the request, and
/// the broker's error are each no answer.
#[cfg(target_os = "macos")]
fn spotlight_stores_asked() -> aterm_vitals::spotlight::StoreAnswer {
    use aterm_uds::lan::StoreBound;
    use aterm_vitals::spotlight::{Store, StoreCut};
    let answer = aterm_uds::lan::spotlight_stores().map_err(|error| error.to_string())?;
    let stores = answer
        .volumes
        .into_iter()
        .map(|v| Store {
            volume: v.volume,
            bytes: v.bytes,
            entries: v.entries,
            cut: v.partial.map(|bound| match bound {
                StoreBound::Entries => StoreCut::Entries,
                StoreBound::Time => StoreCut::Time,
                StoreBound::Unreadable => StoreCut::Unreadable,
            }),
        })
        .collect();
    Ok((stores, answer.age_ms))
}

/// The live arm of the Spotlight stores' port ([`aterm_vitals::spotlight::StoreProbe`]): the root
/// broker, on macOS alone, which alone has Spotlight.
const STORE_LIVE: aterm_vitals::spotlight::StoreProbe = aterm_vitals::spotlight::StoreProbe {
    #[cfg(target_os = "macos")]
    ask: Some(spotlight_stores_asked),
    #[cfg(not(target_os = "macos"))]
    ask: None,
};

fn weeks_json(weeks: &FreeWeeks) -> Option<String> {
    series_json(weeks, WEEKS_SCHEMA)
}

fn weeks_of(text: &str, now_unix_ms: i64) -> Option<FreeWeeks> {
    series_of(text, WEEKS_SCHEMA, now_unix_ms)
}

/// The disk journal's rows as the Disk lens lists them, each with the session id its row names,
/// and its removals by hour.
pub(crate) type JournalDeeds = (Vec<(String, DiskDeed)>, Vec<(i64, u64)>);

/// What the disk journal's `text` (its tail, oldest row first) says the software did in the 3
/// weeks ending at `now_unix_ms` (`docs/DESIGN-disk-custody-2026-11-12.md` §4): its deeds, newest
/// first, at most [`DEEDS_KEPT`], each run of adjacent rows of one kind by one session one deed,
/// with the session's id; and the bytes removed in each hour, oldest first. A line that is not a
/// row, or has no kind or time, is skipped. An intent a later outcome closed is dropped (the
/// outcome says it): a build-cache sweep's by count, as `disk::sweep_in_flight` counts them, since
/// the sweep writes an outcome for each intent but under its stage's path where part of it
/// stayed; any other kind's by its path. A row of a kind this does not know is a deed of its kind
/// and action, with its bytes, and never a removal. A row that finds the list full breaks the run
/// the list ends with, so no older row joins it; removals are counted on. The walk stops at the
/// first row older than the 3 weeks.
pub(crate) fn deeds_of(text: &str, now_unix_ms: i64) -> JournalDeeds {
    use aterm_agent::harness::disk::{SWEEP_ROW_KIND, SweepAction};
    let edge = now_unix_ms.saturating_sub(DEEDS_REACH_MS);
    let mut deeds: Vec<(String, DiskDeed)> = Vec::new();
    let mut full = false;
    let mut closed: HashSet<String> = HashSet::new();
    // The sweep outcomes met so far (newest first) that no older sweep intent has taken.
    let mut sweeps_closing = 0_usize;
    let mut hours: BTreeMap<i64, u64> = BTreeMap::new();
    for line in text.lines().rev() {
        let Ok(row) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let word = |key: &str| row.get(key).and_then(aterm_json::Value::as_str);
        let Some(kind) = word("kind") else {
            continue;
        };
        let Some(at) = word("ts")
            .and_then(aterm_types::rfc3339::parse_utc)
            .map(|s| s.saturating_mul(1_000))
        else {
            continue;
        };
        if at < edge {
            break;
        }
        let action = word("action");
        if kind == SWEEP_ROW_KIND {
            match action.and_then(SweepAction::parse) {
                Some(a) if a.closes() => sweeps_closing = sweeps_closing.saturating_add(1),
                Some(_) if sweeps_closing > 0 => {
                    sweeps_closing = sweeps_closing.saturating_sub(1);
                    continue;
                }
                _ => {}
            }
        } else if let Some(path) = word("path") {
            let outcome = matches!(kind, "removed" | "denial")
                || matches!(action, Some("removed" | "kept" | "failed" | "refused"));
            let intent = kind == "removing" || action == Some("removing");
            if outcome {
                closed.insert(path.to_owned());
            } else if intent && closed.contains(path) {
                continue;
            }
        }
        let read = deed_read(kind, action, &row);
        if read.removal
            && let Some(bytes) = read.bytes.filter(|&b| b > 0)
        {
            let hour = at.div_euclid(3_600_000).saturating_mul(3_600_000);
            let sum = hours.entry(hour).or_insert(0);
            *sum = sum.saturating_add(bytes);
        }
        if full {
            continue;
        }
        let sid = word("sid").unwrap_or("");
        let run = deeds
            .last_mut()
            .filter(|(by, deed)| by.as_str() == sid && deed.kind == read.kind);
        if let Some((_, deed)) = run {
            deed.n = deed.n.saturating_add(1);
            deed.bytes = match (deed.bytes, read.bytes) {
                (Some(a), Some(b)) => Some(a.saturating_add(b)),
                (a, b) => a.or(b),
            };
            deed.free = deed.free.or(read.free);
        } else if deeds.len() < DEEDS_KEPT {
            deeds.push((
                sid.to_owned(),
                DiskDeed {
                    at_unix_ms: at,
                    n: 1,
                    kind: read.kind,
                    by: DeedBy::Aterm,
                    bytes: read.bytes,
                    free: read.free,
                },
            ));
        } else {
            full = true;
        }
    }
    (deeds, hours.into_iter().collect())
}

/// One journal row as [`deeds_of`] reads it.
struct DeedRead {
    kind: DeedKind,
    bytes: Option<u64>,
    free: Option<u64>,
    /// Whether its bytes went off the disk by the software's hand: the chart marks it.
    removal: bool,
}

/// How [`deeds_of`] reads a row of `kind` with `action`, as its writer in `aterm_agent::harness`
/// wrote it. A removal is a `removed` row, a build cache's sweep, a worktree step's or a
/// `disk-free` row that removed (or failed part-way with bytes gone), and a released reserve; a
/// `stopped` row's released bytes repeat its pass's own `removed` rows, and a `free-ask` row's
/// rise in free space is no removal by the software (no build writes that row now; a ledger keeps
/// an earlier build's until its own bound drops them). A `poke` row's session is the one that
/// ran short of space, and a `free-ask` row's the one its step concerned (the one asked, chosen
/// or that freed space), so each row's subject is its session, else aterm.
fn deed_read(kind: &str, action: Option<&str>, row: &aterm_json::Value) -> DeedRead {
    let num = |key: &str| row.get(key).and_then(aterm_json::Value::as_u64);
    let word = |key: &str| row.get(key).and_then(aterm_json::Value::as_str);
    let what = || deed_what(word("class"), word("path"));
    let plain = |kind: DeedKind, bytes: Option<u64>, free: Option<u64>| DeedRead {
        kind,
        bytes,
        free,
        removal: false,
    };
    let removed = |kind: DeedKind, bytes: Option<u64>| DeedRead {
        kind,
        bytes,
        free: None,
        removal: true,
    };
    let other = || {
        let name = action.map_or_else(|| kind.to_owned(), |a| format!("{kind} {a}"));
        plain(
            DeedKind::Other(clipped_front(&name, 32)),
            num("freed_bytes")
                .or_else(|| num("bytes"))
                .or_else(|| num("released_bytes")),
            None,
        )
    };
    let builds = || crate::disk_board::BUILD_CACHES.to_owned();
    let wt = || format!("worktree {}", path_tail(word("path").unwrap_or(""), 1, 40));
    match (kind, action) {
        ("report", _) => plain(DeedKind::Checked, None, num("free_bytes")),
        ("removing", _) => plain(DeedKind::Began(what()), None, None),
        ("removed", _) => removed(DeedKind::Removed(what()), num("freed_bytes")),
        ("denial", _) => {
            let failed = matches!(word("reason"), Some("remove-failed" | "unjournalled"));
            let deed = if failed {
                DeedKind::CouldNotRemove(what())
            } else {
                DeedKind::Refused(what())
            };
            plain(deed, None, None)
        }
        ("stopped", _) => plain(DeedKind::Stopped, None, num("free_bytes")),
        ("reserve", Some("released")) => removed(DeedKind::ReserveReleased, num("bytes")),
        ("reserve", Some("written")) => plain(DeedKind::ReserveWritten, num("bytes"), None),
        ("reserve", Some("refused")) => plain(DeedKind::ReserveRefused, None, None),
        ("build-cache-sweep", Some("removing")) => plain(DeedKind::Began(builds()), None, None),
        ("build-cache-sweep", Some("removed")) => {
            removed(DeedKind::Removed(builds()), num("bytes"))
        }
        ("build-cache-sweep", Some("kept")) => plain(DeedKind::Kept(builds()), None, None),
        ("worktree", Some("removing")) => plain(DeedKind::Began(wt()), None, None),
        ("worktree", Some("removed")) => removed(DeedKind::Removed(wt()), num("freed_bytes")),
        ("worktree", Some("kept")) => plain(DeedKind::Kept(wt()), None, None),
        ("poke", _) => plain(DeedKind::RanShort, None, num("free_bytes")),
        ("free-ask", _) => {
            // A turn-break step says its word in `step` (`typed`, `wait:<why>`, …).
            let said = match word("said") {
                Some("step") => {
                    let step = word("step").unwrap_or("");
                    format!("step {}", step.split_once(':').map_or(step, |(w, _)| w))
                }
                said => said.unwrap_or("").to_owned(),
            };
            plain(
                DeedKind::Asked(clipped_front(&said, 24)),
                num("freed_bytes"),
                num("free_bytes"),
            )
        }
        ("disk-free", Some("removing")) => plain(DeedKind::Began(what()), None, None),
        ("disk-free", Some("removed")) => removed(DeedKind::Removed(what()), num("bytes")),
        ("disk-free", Some("failed")) => removed(DeedKind::CouldNotRemove(what()), num("bytes")),
        ("disk-free", Some("refused")) => plain(DeedKind::Refused(what()), num("bytes"), None),
        _ => other(),
    }
}

/// What a journal row names: its class's words, else its path's last two components (at most
/// 48 characters, the end kept), else nothing.
fn deed_what(class: Option<&str>, path: Option<&str>) -> String {
    if let Some(class) = class.and_then(aterm_agent::harness::disk::Class::parse) {
        return crate::disk_board::freed_what(class).to_owned();
    }
    path.map_or_else(String::new, |path| path_tail(path, 2, 48))
}

/// `path`'s last `parts` components joined by `/`, cut to at most `most` characters with its end
/// kept and `…` opening a cut.
fn path_tail(path: &str, parts: usize, most: usize) -> String {
    let named: Vec<_> = Path::new(path)
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect();
    let tail = named[named.len().saturating_sub(parts)..].join("/");
    let n = tail.chars().count();
    if n <= most {
        return tail;
    }
    std::iter::once('…')
        .chain(tail.chars().skip(n - most.saturating_sub(1)))
        .collect()
}

/// `text` cut to at most `most` characters, its start kept and `…` closing a cut.
fn clipped_front(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    text.chars()
        .take(most.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

/// [`deeds_of`]'s deeds with their owners: aterm for a row of no session, the tab of a session
/// that runs here (its pane in `sids`, that pane's tab in `tabs`), else a tab this window set
/// cannot name. Every non-empty journal sid is an aterm session's (`ATERM_PARENT_SESSION_ID`, a
/// poke's or an ask's session).
pub(crate) fn deeds_owned(
    rows: &[(String, DiskDeed)],
    sids: &[(u64, String)],
    tabs: &[TabRoot],
) -> Vec<DiskDeed> {
    rows.iter()
        .map(|(sid, deed)| {
            let by = if sid.is_empty() {
                DeedBy::Aterm
            } else {
                let tab = |pane: u64| tabs.iter().find(|t| t.key == pane).map_or(pane, |t| t.tab);
                DeedBy::Tab(
                    sids.iter()
                        .find(|(_, s)| s == sid)
                        .map(|&(pane, _)| tab(pane)),
                )
            };
            DiskDeed { by, ..deed.clone() }
        })
        .collect()
}

/// The disk journal's tail as the probe thread keeps it: read again only once the file changed
/// (its length or its time), at most every [`DEEDS_EVERY`] while a Disk lens is open and every
/// [`DEEDS_IDLE_EVERY`] while none is, and never while every window is hidden. Nothing reads it on
/// the main or paint thread: the lens draws what the Snapshot carries.
struct DeedTail {
    /// The journal (`<harness state>/disk.jsonl`); `None` in a unit test's reader.
    path: Option<PathBuf>,
    /// The file's length and time when last read.
    seen: Option<(u64, Option<SystemTime>)>,
    /// The last tail-read attempt, successful or not: retries use the same cadence.
    read_at: Option<Instant>,
    /// Whether the journal was read, or found missing: until then its empty list is not an
    /// empty journal ([`DiskFacts::journal_read`]).
    read: bool,
    rows: Vec<(String, DiskDeed)>,
    removals: Vec<(i64, u64)>,
}

impl DeedTail {
    const fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            seen: None,
            read_at: None,
            read: false,
            rows: Vec::new(),
            removals: Vec::new(),
        }
    }

    /// Reads the journal's tail again at `now` (`now_unix_ms` on the wall clock) if it is due:
    /// `open` while a window holds a Disk lens open or draws the bars, `hidden` while every
    /// window is hidden.
    fn refresh(&mut self, open: bool, hidden: bool, now: Instant, now_unix_ms: i64) {
        self.refresh_with(
            open,
            hidden,
            now,
            now_unix_ms,
            |path| std::fs::metadata(path).map(|meta| (meta.len(), meta.modified().ok())),
            |path| aterm_agent::harness::codex_rollout::tail_of(path, DEEDS_READ_BYTES),
        );
    }

    /// Commit the file's marker and facts together after a successful read. Transient errors
    /// retain the last facts, while a known missing file and a successfully read empty file
    /// clear them. A failed read remains eligible for the next paced attempt at the same marker.
    fn refresh_with(
        &mut self,
        open: bool,
        hidden: bool,
        now: Instant,
        now_unix_ms: i64,
        metadata: impl FnOnce(&Path) -> std::io::Result<(u64, Option<SystemTime>)>,
        read: impl FnOnce(&Path) -> Option<String>,
    ) {
        if hidden {
            return;
        }
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let mark = match metadata(path) {
            Ok(mark) => mark,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.seen = None;
                self.read = true;
                self.rows.clear();
                self.removals.clear();
                return;
            }
            Err(_) => return,
        };
        let seen = Some(mark);
        let every = if open { DEEDS_EVERY } else { DEEDS_IDLE_EVERY };
        let due = self
            .read_at
            .is_none_or(|at| now.saturating_duration_since(at) >= every);
        if seen == self.seen || !due {
            return;
        }
        self.read_at = Some(now);
        let Some(text) = read(path) else {
            return;
        };
        self.seen = seen;
        self.read = true;
        (self.rows, self.removals) = deeds_of(&text, now_unix_ms);
    }

    /// The deeds of the 3 weeks ending at `now_unix_ms` with their owners among `sids` and `tabs`
    /// ([`deeds_owned`]), and the removals. A journal that stays unchanged is not read again, so a
    /// deed leaves the list here once it is older than the 3 weeks; the chart keeps its own span
    /// of the removals.
    fn facts(
        &self,
        sids: &[(u64, String)],
        tabs: &[TabRoot],
        now_unix_ms: i64,
    ) -> (Vec<DiskDeed>, Vec<(i64, u64)>) {
        let edge = now_unix_ms.saturating_sub(DEEDS_REACH_MS);
        let mut deeds = deeds_owned(&self.rows, sids, tabs);
        deeds.retain(|d| d.at_unix_ms >= edge);
        (deeds, self.removals.clone())
    }
}

/// How the probe thread gathers `sys disk held`'s rows ([`crate::sys_host::held_for`]): from
/// every session aterm's table holds and home.
type HeldGather = fn(&[(u64, String)], Option<&Path>) -> aterm_vitals::sys::held::Holdings;

/// `sys disk held`'s rows as the probe thread keeps them for the Disk lens: gathered again at
/// most every [`HELD_EVERY`] while a Disk lens is open and every [`HELD_IDLE_EVERY`] while none
/// is, and never while every window is hidden. A gather reads the disk walker's last board, the
/// build-start record and aterm.toml; it demands no walk and waits on none. Nothing gathers on
/// the main or paint thread: the lens draws what the Snapshot carries, so a row is as old as
/// the last gather, and the Snapshot carries that age beside the rows ([`Self::age_s`]).
struct HoldingsRead {
    /// The gather; `None` in a unit test's reader.
    gather: Option<HeldGather>,
    read_at: Option<Instant>,
    rows: Option<Arc<aterm_vitals::sys::held::Holdings>>,
}

impl HoldingsRead {
    const fn new(gather: Option<HeldGather>) -> Self {
        Self {
            gather,
            read_at: None,
            rows: None,
        }
    }

    /// Gathers the rows again at `now` if they are due: `open` while a window holds a Disk lens
    /// open, `hidden` while every window is; `live` is every session aterm's table holds
    /// ([`Request::live_ids`]).
    fn refresh(&mut self, open: bool, hidden: bool, now: Instant, live: &[(u64, String)]) {
        if hidden {
            return;
        }
        let Some(gather) = self.gather else {
            return;
        };
        let every = if open { HELD_EVERY } else { HELD_IDLE_EVERY };
        let due = self
            .read_at
            .is_none_or(|at| now.saturating_duration_since(at) >= every);
        if !due {
            return;
        }
        let home = aterm_types::dirs::home_dir();
        self.rows = Some(Arc::new(gather(live, home.as_deref())));
        self.read_at = Some(now);
    }

    /// The rows last gathered; `None` before the first gather.
    fn rows(&self) -> Option<Arc<aterm_vitals::sys::held::Holdings>> {
        self.rows.clone()
    }

    /// How old those rows are at `now`, whole seconds; `None` before the first gather.
    fn age_s(&self, now: Instant) -> Option<u64> {
        self.read_at
            .map(|at| now.saturating_duration_since(at).as_secs())
    }
}

impl crate::App {
    pub(crate) fn vitals_audience(&self) -> Audience {
        if self.proxy.is_none() && self.vitals.tx.is_none() {
            return Audience::None;
        }
        let bars = self.stats_bars_form().shown();
        let drawing = || {
            self.windows
                .values()
                .filter(|ws| crate::stats_deck::stats_drawn_on(ws))
        };
        let panel = self.windows.values().any(|ws| {
            ws.dock.roster_panel && ws.dock.view.is_some() && ws.dock.cells > 0 && !ws.occluded
        });
        let page = self.allocation_visible()
            || self.tabs_page_on_screen()
            || self.resources_page_on_screen()
            || panel;
        let in_view = page
            || self.deck_in_view()
            || (self.tabs_page_shown() && self.windows.values().any(|ws| !ws.occluded));
        // With a focused window, the rest and a lapsed attention slow the one shared stream only
        // while no page reads it and no window gives it a person's audience
        // ([`attends_readings`], [`crate::window_rest`]): aterm in front reads as in the
        // background once neither its focused window nor any other window on screen is awake
        // and attended, unless a page on screen reads the stream, which keeps the cadence a
        // focused window gives it. An open stats lens keeps its own cadence either way
        // ([`Audience::cadence`]).
        let front = self.windows.values().any(|ws| ws.focused);
        let focused =
            front && (page || self.windows.values().any(|ws| attends_readings(ws, false)));
        Audience::of(in_view, bars && drawing().next().is_some(), focused)
    }

    /// An event at `now` can have made a window give the readings a person's audience
    /// ([`attends_readings`]): a wake of an attended window that is focused or on screen
    /// (`App::stir_rest`, output's wake in `App::admit_content_redraws`), a touch that ended a
    /// lapse of an awake window that is focused, focused by the event or on screen, or the focus
    /// coming to an awake, attended window (`App::touch_rest`), or the uncover of an awake,
    /// attended window (`App::on_occlusion_changed`). Where aterm has a focused window, or
    /// `focusing` says the event focuses one, that can turn the audience [`Audience::Shown`],
    /// and the next reading is brought forward ([`Self::vitals_ask_now`]). With no focused
    /// window the audience stays what it was, and a reading brought forward would be one more
    /// for nothing.
    pub(crate) fn vitals_ask_for_attention(&mut self, now: Instant, focusing: bool) {
        if focusing || self.windows.values().any(|ws| ws.focused) {
            self.vitals_ask_now(now);
        }
    }

    /// An event at `now` can have given the readings a person's audience
    /// ([`Self::vitals_ask_for_attention`]): while a window in view shows the bars, the next
    /// reading is due at once, or at the focused cadence's next half-slot after the last one
    /// asked if that is later ([`due_on_phase`]; a focused period after it where the wall clock
    /// does not read), so the deck is live again from the next reading rather than the
    /// background cadence's next.
    pub(crate) fn vitals_ask_now(&mut self, now: Instant) {
        if !self.vitals.windowed {
            return;
        }
        let paced = due_on_phase(
            self.vitals.asked_at,
            None,
            FOCUSED_PERIOD,
            now,
            self.vitals.wall.at(now),
        )
        .max(now);
        self.vitals.next_due = Some(self.vitals.next_due.map_or(paced, |due| due.min(paced)));
    }

    fn deck_in_view(&self) -> bool {
        self.stats_bars_form().shown()
            && self
                .windows
                .values()
                .any(|ws| crate::stats_deck::stats_drawn_on(ws) && !ws.occluded)
    }

    /// The moment the loop last asked for a reading, while a window in view shows the bars and
    /// the next ask is armed ([`Self::vitals_deadline`] wakes the loop for it): `Some(None)`
    /// before the first ask. `None` where no reading is asked on a timer.
    pub(crate) const fn timed_reading_asked(&self) -> Option<Option<Instant>> {
        if self.vitals.windowed && self.vitals.next_due.is_some() {
            Some(self.vitals.asked_at)
        } else {
            None
        }
    }

    /// The deadline the loop folds for the next reading.
    pub(crate) fn vitals_deadline(&self) -> Option<Instant> {
        [
            self.vitals.next_due,
            self.vitals.edge_at(Instant::now()),
            self.vitals.jobs_owed.map(|owed| owed.retry_at),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// A level judged every wake.
    pub(crate) fn service_vitals(&mut self, now: Instant) {
        if let Some(report) = self.vitals.pending_cpu_policy.take()
            && !self.vitals.offer(Msg::CpuPolicy(report.clone()))
        {
            self.vitals.pending_cpu_policy = Some(report);
        }
        self.vitals.take_proc_edge();
        self.serve_vitals(now);
        self.vitals.arm_proc_edge();
    }

    fn serve_vitals(&mut self, now: Instant) {
        if let Some(proxy) = self.proxy.as_ref() {
            self.vitals.ensure_thread(proxy);
        }
        self.vitals.require_changed_jobs(now);
        let audience = self.vitals_audience();
        // A close's reading stays owed until it is sent: a full queue asks it at the next
        // service.
        if self.vitals.tab_closed && self.vitals.reader_runs() {
            let period = audience
                .cadence(self.any_stats_lens_open())
                .map_or(Duration::ZERO, |(period, _)| period);
            if self.ask_reading(now, period, false, true) {
                self.vitals.tab_closed = false;
                self.vitals.asked_at = Some(now);
            }
        }
        let shown = audience.in_view();
        let delegated = crate::resource_jobs::active();
        let chips_shown = self.stats_bars_form().shown();
        let adopted = self.adopt_vitals(audience, now, delegated);
        if (adopted && shown) || chips_shown != self.vitals.chips_shown {
            self.vitals.chips_shown = chips_shown;
            self.show_vitals_reading();
        }
        let Some((period, machine_only)) = audience
            .cadence(self.any_stats_lens_open())
            .filter(|_| shown)
        else {
            self.serve_windowless_vitals(audience, now, delegated);
            return;
        };
        // A call at no progress is told only from a view at most 5 s old: while one stands, a
        // window slower than the engine reads a beat past its cadence (`vitals_jobs::told_after`).
        let period = self
            .vitals
            .snapshot
            .as_ref()
            .and_then(|snap| snap.jobs.as_deref())
            .and_then(crate::vitals_jobs::told_after)
            .map_or(period, |told| told.min(period));
        if !std::mem::replace(&mut self.vitals.windowed, true) {
            self.vitals.next_due = None;
            self.vitals.asked_at = None;
        }
        self.vitals.disk_pull = false;
        self.vitals.edge = false;
        let wall_ms = self.vitals.wall.at(now);
        let due = due_on_phase(
            self.vitals.asked_at,
            self.vitals.next_due,
            period,
            now,
            wall_ms,
        );
        let jobs_now = self
            .vitals
            .jobs_owed
            .is_some_and(|owed| owed.retry_at <= now);
        if due > now && !jobs_now {
            self.vitals.next_due = Some(due);
            self.send_layout(now, period, machine_only);
            return;
        }
        self.vitals.asked_at = Some(now);
        self.vitals.next_due = Some(due_on_phase(Some(now), None, period, now, wall_ms));
        // Age is a UI fact even if the worker is delayed or its queue is full.
        // Reuse this visible cadence; do not start a separate polling timer.
        if self.allocation_visible() {
            self.publish_allocation();
        }
        self.ask_reading(now, period, machine_only && !jobs_now, jobs_now);
        if self.tabs_page_shown() && self.harness_screen.fleet_sweep_due(now) {
            self.start_harness_stats_refresh_at(now);
        }
    }

    /// Which CPU policy operation is this App's: the one it reads as pending, cancels and reaps.
    #[cfg(target_os = "macos")]
    pub(crate) const fn cpu_policy_owner(&self) -> crate::cpu_policy_authorization::Owner {
        self.vitals.cpu_policy_owner
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn begin_cpu_policy_report(&mut self, id: u64) {
        self.vitals.cpu_policy_generation = Some((id, 0));
    }

    /// The ordinary unprivileged report writer owns persistence, including denied/partial results.
    #[cfg(target_os = "macos")]
    pub(crate) fn accept_cpu_policy_report(&mut self, report: aterm_json::Value) {
        if let Some(report) = take_cpu_policy_report(&mut self.vitals.cpu_policy_generation, report)
        {
            self.vitals.pending_cpu_policy = Some(report);
            self.service_vitals(Instant::now());
        }
    }

    /// Asks the thread for a reading: false when its queue was full or the thread is gone.
    fn ask_reading(
        &mut self,
        now: Instant,
        period: Duration,
        machine_only: bool,
        jobs_now: bool,
    ) -> bool {
        if jobs_now {
            self.vitals.owe_jobs(now, false);
            if let Some(owed) = &mut self.vitals.jobs_owed {
                owed.retry_at = now + JOBS_FLOOR;
            }
        }
        let mut req = self.vitals_request(period, machine_only);
        req.jobs_now = jobs_now;
        req.note_gaps = std::mem::take(&mut self.vitals.pending_gaps);
        req.note_events = std::mem::take(&mut self.vitals.pending_events);
        let layout = self.layout_fingerprint();
        self.vitals.layout_looked = Some(now);
        let sent = self.vitals.offer(Msg::Read(Box::new(req)));
        if sent {
            self.vitals.sent_layout = Some(layout);
            crate::metrics::note_reading(crate::metrics::ReadingStep::Asked);
        }
        sent
    }

    fn send_layout(&mut self, now: Instant, period: Duration, machine_only: bool) {
        if self.vitals.tx.is_none()
            || self
                .vitals
                .layout_looked
                .is_some_and(|t| now.saturating_duration_since(t) < LAYOUT_EVERY)
        {
            return;
        }
        self.vitals.layout_looked = Some(now);
        let layout = self.layout_fingerprint();
        if self.vitals.sent_layout == Some(layout) {
            return;
        }
        let req = self.vitals_request(period, machine_only);
        if self.vitals.offer(Msg::Facts(Box::new(req))) {
            self.vitals.sent_layout = Some(layout);
        }
    }

    fn serve_windowless_vitals(&mut self, audience: Audience, now: Instant, delegated: bool) {
        self.vitals.require_changed_jobs(now);
        let was_delegated = std::mem::replace(&mut self.vitals.delegated_active, delegated);
        let was_windowed = std::mem::replace(&mut self.vitals.windowed, false);
        if was_windowed || self.vitals.jobs_owed.is_some() || !self.vitals.reader_runs() {
            self.vitals.jobs_due = self.vitals.windowless_due();
        }
        if was_windowed || was_delegated != delegated {
            self.vitals.delegated_due = self
                .vitals
                .full_adopted_at
                .filter(|_| delegated)
                .map(|at| at + BACKGROUND_PERIOD);
        }
        let pulled = std::mem::take(&mut self.vitals.disk_pull);
        let edge = self.vitals.edge_at(now).is_some_and(|at| at <= now);
        let hidden = audience.cadence(false);
        let first_job = delegated && !was_delegated && self.vitals.full_adopted_at.is_none();
        if pulled
            || edge
            || first_job
            || self.vitals.jobs_due.is_some_and(|due| due <= now)
            || self.vitals.delegated_due.is_some_and(|due| due <= now)
        {
            self.vitals.owe_jobs(now, pulled || first_job);
            // The engine reads the sweep: a whole reading. Beside a hidden
            // deck it keeps the deck's cadence for the gap rule; with no deck
            // it has none, so it starts a new baseline and draws no rate.
            if edge {
                self.vitals.edge = false;
                self.vitals.last_edge_read = Some(now);
            }
            self.vitals.jobs_due = None;
            self.vitals.delegated_due = None;
            self.vitals.asked_at = Some(now);
            let period = hidden.map_or(Duration::ZERO, |(period, _)| period);
            let period = if delegated {
                if period.is_zero() {
                    BACKGROUND_PERIOD
                } else {
                    period.min(BACKGROUND_PERIOD)
                }
            } else {
                period
            };
            self.ask_reading(now, period, false, true);
            self.vitals.jobs_due = self.vitals.windowless_due();
        } else if let Some((period, machine_only)) = hidden
            && due_on_phase(
                self.vitals.asked_at,
                None,
                period,
                now,
                self.vitals.wall.at(now),
            ) <= now
        {
            self.vitals.asked_at = Some(now);
            self.ask_reading(now, period, machine_only, false);
        } else {
            let period = hidden.map_or(Duration::ZERO, |(period, _)| period);
            self.send_layout(now, period, hidden.is_some_and(|(_, m)| m));
        }
        self.vitals.next_due = self
            .vitals
            .windowless_deadline(hidden.map(|(period, _)| period), now);
    }

    /// Handles output from `session` on `Wake::Output` after clearing the session's output latch.
    pub(crate) fn vitals_output_edge(&mut self, session: u64, now: Instant) {
        match self.vitals_output_kind(session, now) {
            OutputEdge::Edge => {
                self.vitals.note_edge();
                if self.vitals.edge_at(now).is_some_and(|at| at <= now) {
                    self.service_vitals(now);
                }
            }
            OutputEdge::Recheck => {
                let due = self
                    .vitals
                    .last_edge_read
                    .is_none_or(|t| t + JOBS_FLOOR <= now);
                if due {
                    if let Some(park) = self.vitals.parked.get_mut(&session) {
                        park.read_at = now;
                    }
                    self.vitals.note_edge();
                    self.service_vitals(now);
                }
            }
            OutputEdge::Quiet => {}
        }
    }

    /// Classifies a write of `session` for the stuck engine at `now`.
    pub(crate) fn vitals_output_kind(&mut self, session: u64, now: Instant) -> OutputEdge {
        if self.vitals.windowed || self.vitals.tx.is_none() {
            return OutputEdge::Edge;
        }
        let Some(s) = self.pool.get(session) else {
            return OutputEdge::Edge;
        };
        let moves = s.ctx.screen_moves.count_after_clear();
        let moved = match self.vitals.screens.insert(session, moves) {
            Some(before) => before != moves,
            None => {
                let pool = &self.pool;
                self.vitals
                    .screens
                    .retain(|key, _| pool.get(*key).is_some());
                true
            }
        };
        if !moved {
            return OutputEdge::Quiet;
        }
        let Some(park) = self.vitals.parked.get(&session).copied() else {
            return OutputEdge::Edge;
        };
        let sink = &s.ctx.sink;
        let fg = crate::quit_safety::foreground_pgrp(sink.master());
        let mode = sink.tty_echo().map(|e| (e.canonical, e.echo));
        if !park.holds(fg, mode, sink.driven_input()) {
            self.vitals.parked.remove(&session);
            return OutputEdge::Edge;
        }
        if now.saturating_duration_since(park.read_at) >= PARKED_RECHECK {
            OutputEdge::Recheck
        } else {
            OutputEdge::Quiet
        }
    }

    /// Requests a whole reading when the disk watch looked with no bars in view.
    pub(crate) fn vitals_disk_looked(&mut self, now: Instant) {
        if self.vitals_audience().in_view() {
            return;
        }
        self.vitals.disk_pull = true;
        self.service_vitals(now);
    }

    fn record_job_events(&mut self) {
        let events = self.vitals.take_job_events();
        for e in events {
            let aterm_vitals::events::EventKind::Job {
                tab,
                id,
                to,
                class,
                task,
                exit,
                ..
            } = e.kind
            else {
                continue;
            };
            let Some(timeline) = self.pool.get(tab).and_then(|p| {
                let aterm_vitals::events::EventScope::Session(source) = &e.scope else {
                    return None;
                };
                (source.local == tab
                    && source.sid == p.ctx.self_id.as_str()
                    && source.nonce == p.ctx.nonce.to_hex())
                .then(|| Arc::clone(&p.ctx.timeline))
            }) else {
                continue;
            };
            let rev = self.vitals.next_job_rev(tab);
            let payload =
                crate::sys_host::job_event_payload(to, &id, class, rev, task.as_deref(), exit);
            timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .record_row(crate::sys_host::JOB_CHANGE, payload);
        }
    }

    /// Adopts and presents a snapshot left by the probe thread on `Wake::Vitals`.
    pub(crate) fn on_vitals_wake(&mut self) {
        let audience = self.vitals_audience();
        let now = Instant::now();
        let delegated = crate::resource_jobs::active();
        if self.adopt_vitals(audience, now, delegated) && audience.in_view() {
            self.show_vitals_reading();
        }
        if !audience.in_view() {
            self.serve_windowless_vitals(audience, now, delegated);
        }
    }

    fn adopt_vitals(&mut self, audience: Audience, now: Instant, delegated: bool) -> bool {
        let before = self.vitals.snapshot.as_ref().and_then(|s| s.jobs.clone());
        let adopted = self.vitals.adopt(now);
        self.record_job_events();
        self.note_health_episodes();
        if !adopted {
            return false;
        }
        if self.allocation_visible() {
            self.publish_allocation();
        }
        self.publish_resources();
        self.vitals.landed_period = audience
            .cadence(self.any_stats_lens_open())
            .map(|(period, _)| period);
        self.retarget_needle(now);
        self.post_jobs_reading();
        self.note_held_episode(before.as_deref());
        self.note_left_tabs();
        self.note_leftovers();
        if !audience.in_view() {
            if self
                .vitals
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.machine_only)
            {
                self.vitals.jobs_due = self.vitals.windowless_due();
                self.vitals.delegated_due = self
                    .vitals
                    .full_adopted_at
                    .filter(|_| delegated)
                    .map(|at| at + BACKGROUND_PERIOD);
            }
            self.vitals.next_due = self
                .vitals
                .windowless_deadline(audience.cadence(false).map(|(period, _)| period), now);
        }
        true
    }

    fn show_vitals_reading(&mut self) {
        self.redraw_stats_decks();
        // The frame this asks for draws the band as the last poll adopted it.
        self.network_ride.drawn();
        if self.tabs_page_shown() {
            self.publish_native_harness_state();
        }
    }

    /// Records each closed tab the view just adopted is the first to hold with processes left:
    /// one `proc.left` record per close ([`crate::message_reporters::newly_left`]).
    fn note_left_tabs(&mut self) {
        let Some(after) = self.vitals.snapshot.as_ref().and_then(|s| s.left.clone()) else {
            return;
        };
        let mut told = false;
        for tab in crate::message_reporters::newly_left(&mut self.vitals.left_told, &after) {
            let _ = self.post_message(crate::message_reporters::left_record(tab, &after));
            told = true;
        }
        // At once, so the pass keeps them told before an update's Commit can end this process;
        // every request carries them too.
        if told {
            let told = self.vitals.left_told.iter().copied().collect();
            self.vitals.offer(Msg::Told(told));
        }
    }

    /// States the `leftovers` Entries the view just adopted makes, the family's whole answer
    /// ([`crate::message_reporters::leftovers_entries`]; none ends those it held), and records
    /// each dangling link it is the first to hold ([`crate::message_reporters::newly_dangling`]).
    /// A view that names none while the digest holds none states nothing.
    pub(crate) fn note_leftovers(&mut self) {
        use aterm_messages::exceptions::Family;
        let Some(view) = self.vitals.snapshot.as_ref().and_then(|s| s.left.clone()) else {
            return;
        };
        let entries = crate::message_reporters::leftovers_entries(
            &view,
            crate::message_reporters::left_entry_after_ms(),
        );
        if !entries.is_empty()
            || !self
                .messages
                .digest_keys_under(&Family::Vitals.prefix())
                .is_empty()
        {
            self.except_replace(Family::Vitals, &entries);
        }
        for link in crate::message_reporters::newly_dangling(&mut self.vitals.links_told, &view) {
            let _ = self.post_message(crate::message_reporters::link_record(link));
        }
    }

    /// Posts the record of each health episode's edge the probe thread handed over (a memory
    /// starvation episode's, [`crate::message_reporters::starving_record`]); the thread wrote its
    /// aterm.log line and tried to order it to disk as it read it.
    fn note_health_episodes(&mut self) {
        for e in self.vitals.take_health_events() {
            if let aterm_vitals::events::EventKind::Starving(edge) = &e.kind {
                let _ = self.post_message(crate::message_reporters::starving_record(edge));
            }
        }
    }

    /// Records what the view just adopted says of macOS's program check: one `program-check.`
    /// record per episode edge it makes against `before` ([`held_edge`]), one naming the programs
    /// it first says have not started but does not call held ([`newly_not_started`]), one naming the
    /// programs it first says are stranded ([`newly_stranded`]) while it names the syspolicyd they
    /// were judged against, the `vitals.stranded` digest entry
    /// ([`crate::message_reporters::stranded_entry`]) for those it names stranded at least
    /// [`aterm_uds::deadline::FIRST_EXEC_HANG`] while it names that syspolicyd, handed only when
    /// those programs or that syspolicyd move (a view that names no syspolicyd leaves the entry as
    /// it stands, and one that names one and no such program ends it; a view carried into a later
    /// snapshot says what it said), one when it first names programs aterm started after its
    /// Developer Tools grant took ([`after_grant_edge`]), and, once the view stands in no episode,
    /// the verdict refresh held back ([`Self::start_devtools_refresh`]): one an episode held
    /// ([`Self::refresh_devtools_verdict`]) or one that did not settle since, held first, so it is
    /// asked again at the first view that stands in none, this one included.
    fn note_held_episode(&mut self, before: Option<&aterm_vitals::jobs::JobsView>) {
        let Some(after) = self.vitals.snapshot.as_ref().and_then(|s| s.jobs.clone()) else {
            return;
        };
        if let Some(edge) = held_edge(before.map(|v| &v.findings), &after.findings) {
            let _ = self.post_message(crate::message_reporters::program_check_episode(
                edge,
                after.findings.program_check_aterm.0,
            ));
        }
        let fresh = newly_not_started(before.map(|v| &v.findings), &after.findings);
        if !fresh.is_empty() {
            let _ = self.post_message(crate::message_reporters::not_started_record(&fresh));
        }
        let stranded = newly_stranded(before.map(|v| &v.findings), &after.findings);
        if let Some(daemon) = after.findings.program_check_daemon
            && !stranded.is_empty()
        {
            let _ = self.post_message(crate::message_reporters::stranded_record(&stranded, daemon));
        }
        if let Some(daemon) = after.findings.program_check_daemon {
            let long: Vec<&aterm_vitals::jobs::Named> = after
                .findings
                .program_check_stranded
                .iter()
                .filter(|n| Duration::from_millis(n.for_ms) >= aterm_uds::deadline::FIRST_EXEC_HANG)
                .collect();
            let said = (long.iter().map(|n| n.pid).collect(), daemon);
            if self.vitals.stranded_said.as_ref() != Some(&said) {
                let entries: Vec<_> = crate::message_reporters::stranded_entry(&long, daemon)
                    .into_iter()
                    .collect();
                self.except_replace(aterm_messages::exceptions::Family::Stranded, &entries);
                self.vitals.stranded_said = Some(said);
            }
        }
        if let Some(n) = after_grant_edge(before.map(|v| &v.findings), &after.findings) {
            let _ = self.post_message(crate::message_reporters::program_check_after_grant(n));
        }
        if self.devtools_helper.take_unsettled() {
            aterm_log::info!(
                "devtools: asking macOS about aterm again did not settle; it is asked again once \
                 macOS's program check catches up"
            );
            self.devtools_watch.hold_prime();
        }
        if after.findings.program_check_episode.is_none() && self.devtools_watch.take_held_prime() {
            self.start_devtools_refresh();
        }
    }

    /// Asks macOS about aterm again ([`crate::devtools_helper::Lane::refresh_verdict`]) unless the
    /// adopted jobs view stands in a backed-up episode of macOS's program check: the prime is
    /// itself a new program and would wait in that line, so it is held until the next view that
    /// stands in none ([`Self::note_held_episode`]).
    pub(crate) fn refresh_devtools_verdict(&mut self) {
        // Losing freshness is not a drain witness. Only a later engine view ends the hold.
        let backed_up = self
            .vitals
            .snapshot
            .as_ref()
            .and_then(|snap| snap.jobs.as_ref())
            .is_some_and(|view| view.findings.program_check_episode.is_some());
        if backed_up {
            aterm_log::info!(
                "devtools: macOS's program check is backed up; asking macOS about aterm again \
                 waits until it catches up"
            );
            self.devtools_watch.hold_prime();
        } else {
            self.start_devtools_refresh();
        }
    }

    /// Starts a verdict refresh, or holds it when none starts (one is still out, or its worker
    /// could not start), so the next view that stands in no episode asks again
    /// ([`Self::note_held_episode`]).
    fn start_devtools_refresh(&mut self) {
        if !self.devtools_helper.refresh_verdict() {
            self.devtools_watch.hold_prime();
        }
    }

    fn post_jobs_reading(&self) {
        self.vitals.board.post_left(
            self.vitals
                .snapshot
                .as_ref()
                .and_then(|snap| snap.left.clone()),
        );
        let Some(view) = self
            .vitals
            .snapshot
            .as_ref()
            .and_then(|snap| snap.jobs.clone())
        else {
            return;
        };
        let sids = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            view.rows
                .iter()
                .filter_map(|r| {
                    store
                        .by_local(r.tab)
                        .map(|h| (r.tab, h.sid.as_str().to_string()))
                })
                .collect()
        };
        // A new view is as old as the reading that made it, not its adoption: the probe's read and
        // the wait for the loop count toward the facts gate. A held view keeps its first post.
        let read = self.vitals.read_began.unwrap_or_else(Instant::now);
        // A quiet agent call's wait is named before anything is said of it.
        crate::auto_naming::name_quiet_calls(&view);
        self.vitals.board.post(view, sids, read);
    }

    /// Starts a "where" survey when the Disk lens opens.
    pub(crate) fn disk_lens_opened(&self) {
        let mut ask = crate::disk_where::Ask {
            home: aterm_types::dirs::home_dir(),
            ..crate::disk_where::Ask::default()
        };
        for ws in self.windows.values() {
            for tab in ws.tab_set.tabs() {
                tab.root.visit(&mut |&view| {
                    let Some(crate::tab_model::View::Terminal(view)) =
                        self.view_store.get(view).copied()
                    else {
                        return;
                    };
                    let Some(s) = self.pool.get(view.session) else {
                        return;
                    };
                    if let Ok(pid) = u32::try_from(s.pid)
                        && pid > 0
                    {
                        ask.shells.push(pid);
                    }
                    let tl = s
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    let agent = tl.agent();
                    if agent.reader.is_some()
                        && let Ok(leader) = u32::try_from(agent.program_pgid)
                        && leader > 0
                    {
                        ask.agents.push(leader);
                    }
                });
            }
        }
        crate::disk_where::start(
            &self.vitals.survey,
            ask,
            Instant::now(),
            aterm_time::unix_millis().and_then(|ms| i64::try_from(ms).ok()),
        );
    }

    /// Asks a frame of each window that draws the stats bars on screen for the reading just
    /// adopted: none with the bars off, and none of an occluded window (hidden, minimised or
    /// covered), which redraws when it shows again (`App::on_occlusion_changed`).
    pub(crate) fn redraw_stats_decks(&self) {
        if self.stats_bars_form() == crate::stats_deck::StatsBars::Off {
            return;
        }
        for ws in self.windows.values() {
            if crate::stats_deck::stats_drawn_on(ws) && !ws.occluded {
                ws.request_redraw();
            }
        }
    }

    fn for_each_pane(&self, mut f: impl FnMut(u64, usize, u64, u32, bool)) {
        let front = self.front_window();
        for (wid, ws) in &self.windows {
            let active = ws.tab_set.active_index();
            for (i, tab) in ws.tab_set.tabs().iter().enumerate() {
                tab.root.visit(&mut |leaf: &crate::tab_model::ViewId| {
                    let Some(session) = self
                        .view_store
                        .get(*leaf)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                    else {
                        return;
                    };
                    let Some(pid) = self
                        .pool
                        .get(session)
                        .and_then(|s| u32::try_from(s.pid).ok())
                        .filter(|pid| *pid != 0)
                    else {
                        return;
                    };
                    let front_pane = Some(*wid) == front && active == Some(i) && *leaf == tab.focus;
                    f(wid.0, i, session, pid, front_pane);
                });
            }
        }
    }

    fn front_window(&self) -> Option<crate::WindowId> {
        self.frontmost_window
            .filter(|wid| self.windows.contains_key(wid))
            .or_else(|| self.windows.keys().next().copied())
    }

    fn layout_fingerprint(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.for_each_pane(|window, tab, session, pid, front| {
            (window, tab, session, pid, front).hash(&mut h);
        });
        h.finish()
    }

    fn vitals_request(&self, period: Duration, machine_only: bool) -> Request {
        let mut tabs = Vec::new();
        let mut layout = Vec::new();
        let mut tokens = Vec::new();
        let mut pane_token_observations = Vec::new();
        let mut token_panes = std::collections::BTreeSet::new();
        let mut accounts: Vec<AccountKey> = Vec::new();
        let mut front_plan = None;
        let mut front_dir = None;
        // A pane shown in two tabs (Open Active Session in New Window) is one root, in the tab
        // listed first; each tab then takes the least key of the panes it keeps, so a split tab
        // whose least pane a twin listed first shows keeps its other panes under their own.
        let mut seen = std::collections::HashSet::new();
        let front = self.front_window();
        for (wid, ws) in &self.windows {
            let active = ws.tab_set.active_index();
            for (i, tab) in ws.tab_set.tabs().iter().enumerate() {
                let terminal = |view| match self.view_store.get(view).copied() {
                    Some(crate::tab_model::View::Terminal(view)) => Some(view.session),
                    _ => None,
                };
                let focused = terminal(tab.focus);
                let in_front = Some(*wid) == front && active == Some(i);
                if in_front {
                    front_dir = focused
                        .and_then(|k| self.pool.get(k))
                        .and_then(|s| crate::sys_host::local_cwd_of(&s.term))
                        .map(PathBuf::from);
                }
                let first = tabs.len();
                let name = self.tab_name(tab, i);
                for pane in self.tab_sessions(tab) {
                    let Some(s) = self.pool.get(pane) else {
                        continue;
                    };
                    token_panes.insert(pane);
                    let tl = s
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    if let Ok(shell_pid) = u32::try_from(s.pid)
                        && shell_pid > 0
                    {
                        if seen.insert(pane) {
                            tabs.push(TabRoot {
                                key: pane,
                                tab: pane,
                                shell_pid,
                                label: name.clone(),
                                program: tl.agent().program.clone().unwrap_or_default(),
                                tab_id: Some(tab.id.get()),
                                agent_leader: agent_leader(tl.agent()),
                            });
                        }
                        layout.push(PaneFact {
                            key: pane,
                            window: wid.0,
                            tab: i + 1,
                            tab_id: Some(tab.id),
                        });
                    }
                    let facts = tl.claude_footer();
                    if let Some(p) = facts.and_then(|f| f.plan) {
                        if !accounts.contains(&p.account) {
                            accounts.push(p.account);
                        }
                        if in_front && front_plan.is_none() {
                            front_plan = Some(p);
                        }
                    }
                }
                let roots = &mut tabs[first..];
                if let Some(least) = roots.iter().map(|t| t.key).min() {
                    for root in roots {
                        root.tab = least;
                    }
                }
            }
        }
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        let windowless = tabs.len();
        let mut session_ids = Vec::new();
        let mut pane_sources = Vec::new();
        let mut live_ids = Vec::new();
        for handle in store.live_handles() {
            live_ids.push((handle.local_id, handle.sid.as_str().to_string()));
            let Some(session) = self.pool.get(handle.local_id) else {
                continue;
            };
            let Ok(pid) = u32::try_from(session.pid) else {
                continue;
            };
            if pid == 0 {
                continue;
            }
            if seen.insert(handle.local_id) {
                tabs.push(TabRoot {
                    key: handle.local_id,
                    tab: handle.local_id,
                    shell_pid: pid,
                    label: format!("Session {}", handle.local_id),
                    program: String::new(),
                    tab_id: None,
                    agent_leader: None,
                });
            }
            session_ids.push((handle.local_id, handle.sid.as_str().to_string()));
            pane_sources.push(crate::vitals_host::memory::PaneSource {
                local: handle.local_id,
                sid: handle.sid.clone(),
                nonce: handle.nonce,
            });
        }
        drop(store);
        // A view can already hold its terminal while registry publication follows. Parked
        // terminals have the opposite shape, and a mirrored terminal occurs in both sets.
        token_panes.extend(live_ids.iter().map(|(pane, _)| *pane));
        for pane in token_panes {
            let Some(session) = self.pool.get(pane) else {
                continue;
            };
            let tl = session
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(f) = tl.claude_footer() {
                tokens.extend(tab_tokens(pane, f, tl.claude_footer_read_at()));
            }
            if !machine_only {
                pane_token_observations.extend(tl.panel_token_observation(pane, session.ctx.nonce));
            }
        }
        for root in &mut tabs[windowless..] {
            root.agent_leader = self.pool.get(root.key).and_then(|s| {
                agent_leader(
                    s.ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .agent(),
                )
            });
        }
        let tty_sources: Vec<TtySource> = tabs
            .iter()
            .filter_map(|t| {
                let s = self.pool.get(t.key)?;
                Some(TtySource::new(t.key, &s.ctx.sink, &s.ctx.screen_moves))
            })
            .collect();
        let ttys = tty_sources.iter().filter_map(TtySource::read).collect();
        let feeds = tabs
            .iter()
            .filter_map(|t| Some((t.key, Arc::clone(&self.pool.get(t.key)?.ctx.job_feed))))
            .collect();
        let inputs = tabs
            .iter()
            .filter_map(|t| Some((t.key, self.pool.get(t.key)?.ctx.sink.driven_input_watch())))
            .collect();
        let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
        let grant = self.vitals_grant(&tabs);
        let shell_identities = tabs
            .iter()
            .filter_map(|root| {
                self.pool
                    .get(root.key)
                    .map(|session| (root.key, session.shell_identity))
            })
            .collect();
        Request {
            allocation: self.allocation_visible(),
            self_pid: std::process::id(),
            tabs,
            shell_identities,
            ttys,
            tty_sources,
            feeds,
            grant,
            inputs,
            layout,
            session_ids,
            pane_sources,
            live_ids,
            cadence_ms: duration_ms(period),
            machine_only,
            jobs_now: false,
            pull: false,
            hardware: false,
            scheduling: false,
            self_cpu: false,
            deck: self.deck_in_view() || self.resources_page_on_screen(),
            in_view: self.vitals_audience().in_view(),
            tokens: each_session_once(tokens),
            pane_token_observations,
            plan: front_plan.and_then(|p| plan_fact(&p, &accounts, now)),
            lenses: self.open_lens_rows(),
            note_gaps: Vec::new(),
            note_events: Vec::new(),
            front_dir,
            left_told: self.vitals.left_told.iter().copied().collect(),
            assessment_only: false,
        }
    }

    /// What the jobs pass sets against a held program's fork time
    /// (`aterm_vitals::jobs::Findings::program_check_after_grant`): when aterm's Developer Tools
    /// grant took effect, the end of the last verdict refresh that took
    /// ([`crate::devtools_helper::Lane::took_at`]), only while the lane reads aterm granted; and
    /// the roots of `tabs` whose session another aterm process handed over.
    pub(crate) fn vitals_grant(&self, tabs: &[TabRoot]) -> crate::vitals_jobs::GrantFacts {
        let granted = self
            .devtools_helper
            .reading()
            .is_some_and(|r| r.grant == aterm_containment::DevToolGrant::Granted);
        crate::vitals_jobs::GrantFacts {
            grant_ms: granted.then(|| self.devtools_helper.took_at()).flatten(),
            adopted: tabs
                .iter()
                .filter(|t| {
                    self.pool.get(t.key).is_some_and(|s| {
                        crate::consent_state::session_attribution(s)
                            == aterm_containment::consent::Attribution::Adopted
                    })
                })
                .map(|t| t.key)
                .collect(),
        }
    }

    /// This window's front tab: its focused pane's session id and its label (`Tab 2`, or for a
    /// worker `Workers ▸ bench`).
    pub(crate) fn deck_tab(&self, wid: crate::WindowId) -> (Option<u64>, String) {
        let Some(ws) = self.windows.get(&wid) else {
            return (None, "Tab".into());
        };
        let Some(i) = ws.tab_set.active_index() else {
            return (None, "Tab".into());
        };
        let Some(tab) = ws.tab_set.tab_at(i) else {
            return (None, "Tab".into());
        };
        let key = match self.view_store.get(tab.focus).copied() {
            Some(crate::tab_model::View::Terminal(view)) => Some(view.session),
            _ => None,
        };
        (key, self.tab_name(tab, i))
    }

    /// The sessions of this window's front tab ([`Self::tab_sessions`]): the
    /// tab the deck, its lens and Why now take as "this tab", as `sys why`
    /// takes its caller's (a tab split into panes is one,
    /// [`aterm_vitals::model::TabRoot::tab`]).
    pub(crate) fn deck_tab_panes(&self, wid: crate::WindowId) -> Vec<u64> {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index().and_then(|i| ws.tab_set.tab_at(i)))
            .map_or_else(Vec::new, |tab| self.tab_sessions(tab))
    }

    /// The terminal sessions of `tab`, its focused pane first, then the rest
    /// in layout order: the panes the vitals request sends as one tab.
    fn tab_sessions(&self, tab: &crate::tab_model::Tab) -> Vec<u64> {
        let mut sessions = Vec::with_capacity(tab.root.len());
        if let Some(focused) = self.terminal_session(tab.focus) {
            sessions.push(focused);
        }
        tab.root.visit(&mut |&view| {
            if view != tab.focus
                && let Some(session) = self.terminal_session(view)
            {
                sessions.push(session);
            }
        });
        sessions
    }

    /// The terminal sessions of `tab` in layout order, whichever has the focus:
    /// the disk walker's panes, numbered `pane=` in that order by `sys disk`.
    pub(crate) fn tab_layout_sessions(&self, tab: &crate::tab_model::Tab) -> Vec<u64> {
        let mut sessions = Vec::with_capacity(tab.root.len());
        tab.root.visit(&mut |&view| {
            if let Some(session) = self.terminal_session(view) {
                sessions.push(session);
            }
        });
        sessions
    }

    fn terminal_session(&self, view: crate::tab_model::ViewId) -> Option<u64> {
        match self.view_store.get(view).copied() {
            Some(crate::tab_model::View::Terminal(view)) => Some(view.session),
            _ => None,
        }
    }
}

// Every test that calls it is unix-only.
#[cfg(all(test, unix))]
impl VitalsHost {
    /// A test's stand-in for the reader thread: the host asks it its readings, so the deck has
    /// an audience whose cadence other modules' tests can read.
    pub(crate) fn attach_test_reader(&mut self) -> std::sync::mpsc::Receiver<Msg> {
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        self.tx = Some(tx);
        rx
    }
}

#[path = "vitals_strip.rs"]
mod strip;

#[cfg(test)]
#[path = "vitals_host_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "vitals_boot_schedule_tests.rs"]
mod boot_schedule_tests;

#[cfg(test)]
#[path = "vitals_host_split_tests.rs"]
mod split_tests;

#[cfg(test)]
#[path = "vitals_host_lanes_tests.rs"]
mod lanes_tests;

#[cfg(all(test, unix))]
#[path = "vitals_host_attended_tests.rs"]
mod attended_tests;

#[cfg(test)]
#[path = "one_engine_processes_tests.rs"]
mod one_engine_processes_tests;
