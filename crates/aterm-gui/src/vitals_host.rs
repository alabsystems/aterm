// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE VITALS HOST: the one probe thread that reads the machine for the stats Deck (`crate::stats_deck`), and the loop's side of it.
//!
//! THE SPLIT SET (`docs/DESIGN-resource-details-2026-10-20.md` §3.1–§3.2,
//! §5.1): what `sys why` and the Deck's cause clause split the busy CPU by
//! (`aterm_vitals::explain`), read on macOS beside a whole reading: each
//! swept process's resource coalition (in the sweep), every coalition's
//! counters, each swept process's birth (kept only while its known kernel
//! start and name agree), each coalition lead's current identity (an exec
//! can keep its pid, birth and short name; a refused read replaces the last
//! identity with unknown) and the machine's timeline (`kern.waketime`
//! each time, the rest when it moved). The sweep, the coalitions and the
//! cores' ticks are read back to back, so each end of a window reads them
//! together. A reading that reads it is a SPLIT TICK, and only some are
//! ([`split_due`]): the bars' reading while a window in view shows them, as
//! the Deck or the Sill ([`Request::deck`]), every [`SPLIT_EVERY`] (every
//! [`SPLIT_EVERY_LENS`] while a lens is open), and a worker's pull while a
//! `sys why` arms the split ([`SPLIT_ARMED`]), which pulls a cold pair when
//! the last split tick is missing or too old for a window
//! ([`SplitReader::cold`]), so its reply has a window. The jobs engine's
//! readings (an edge, the disk watch's look, its demand, a delegated sweep),
//! a hidden deck's machine counters, the Fleet Monitor tab's readings with no
//! bars in view and every other pull read none, and nothing arms a timer for
//! one: with no bars in view and no `sys why`, no coalition is read. The lane
//! is decided once, on this thread, by the kernel's own self-check
//! ([`SplitReader::new`]), and goes off for good where the model finds a read
//! misread (`aterm_vitals::explain`); Linux and Windows have none, and a unit
//! test's reader has the inert arm ([`CoalProbe::INERT`]), as its thermal
//! probe has.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::harness::profile::{AccountKey, Bucket, PlanUsage};
use aterm_sysprobe::jobs::RunState;
use aterm_sysprobe::volumes::VolumeKey;
use aterm_vitals::Peaks;
use aterm_vitals::explain::{CoalLane, CoalSample, CoalUsage, LeadIdentity, TimelineSample};
use aterm_vitals::model::{
    AccountIx, ClusterSpec, CommitSample, DiskFacts, DiskLevel, GpuSample, Hog, LinkKind,
    LinkSample, MemCounters, MemSample, PlanFact, Pressure, ProcLimits, ProcSample, ProcState,
    Sample, Snapshot, TabRoot, TabTokens, Thermal, Throttle, Ticks, VolumeSample,
};
use aterm_vitals::sys::Machine;

use crate::disk_board::{Clock, DiskFactsRaw, Shelf};
use crate::disk_where::Where;
use crate::occupancy_host::Arming;
use crate::sys_host::PaneFact;

/// A reading a second while a focused window shows the Deck.
pub(crate) const FOCUSED_PERIOD: Duration = Duration::from_secs(1);
/// One every five seconds while aterm is in the background.
pub(crate) const BACKGROUND_PERIOD: Duration = Duration::from_secs(5);
/// The machine counters every ten seconds while every window that draws the bars is hidden (v2 §5.3).
pub(crate) const HIDDEN_PERIOD: Duration = Duration::from_secs(10);
/// The peaks are written at most this often while they change (v2 §8.2).
const PEAKS_EVERY: Duration = Duration::from_secs(600);
/// The peaks' file under aterm's state root.
const PEAKS_FILE: &str = "vitals-peaks.json";
/// The peaks file's layout; a file of another is read as no peaks.
const PEAKS_SCHEMA: u32 = 1;
/// The largest peaks file read: three peaks of seven days are well under a
/// KiB, so a larger file is damaged and read as no peaks.
const PEAKS_MAX_BYTES: u64 = 16 * 1024;
/// How long the quit waits for the probe thread to end and write the peaks.
pub(crate) const STOP_BOUND: Duration = Duration::from_millis(500);

/// A pull whose thread's last reading is older than this reads twice.
pub(crate) const COLD_AFTER: Duration = Duration::from_secs(3);
/// …this far apart, so the rates cover an interval that says what is happening now.
pub(crate) const COLD_PAIR: Duration = Duration::from_millis(500);

/// An edge's readings with no window showing the bars come at most this often.
pub(crate) const JOBS_FLOOR: Duration = Duration::from_secs(2);
/// The shortest rate window a pull measures (design §6.4): a pull that comes sooner after the last whole reading waits out the rest.
pub(crate) const MIN_WINDOW: Duration = Duration::from_millis(250);
/// Between readings the loop looks at the pane layout at most this often (a reading carries it whole), so a wake costs no walk of every pane.
pub(crate) const LAYOUT_EVERY: Duration = Duration::from_millis(250);
/// The job events the hand-over keeps for the loop before the oldest go.
const JOB_EVENTS_CAP: usize = 256;
/// The bars' reading reads the split set at most this often: the split's
/// window (module docs, THE SPLIT SET)…
pub(crate) const SPLIT_EVERY: Duration = Duration::from_secs(5);
/// …or this often while a lens is open (a cadence of at most
/// [`crate::stats_lens::LENS_PERIOD`]).
pub(crate) const SPLIT_EVERY_LENS: Duration = Duration::from_secs(1);
/// The longest window a split reads: the model's one limit, past which a
/// base only starts the next window.
pub(crate) const SPLIT_WINDOW_MAX: Duration =
    Duration::from_nanos(aterm_vitals::explain::WINDOW_MAX_NS);
/// How far inside [`SPLIT_WINDOW_MAX`] the last split tick must be for an
/// armed pull to read once ([`SplitReader::cold`]): the model measures the
/// window between the two readings' stamps, each taken after its sweep, so
/// a base a hair inside the limit when the pull looks is past it by then.
pub(crate) const SPLIT_COLD_MARGIN: Duration = Duration::from_secs(1);
/// The development seam (`aterm_types::dev_seam!`) that reads a recorded
/// scene of `aterm_vitals::explain::fixtures` in place of the machine:
/// `spotlight` (RD's 10:32 moment) or `calm` ([`WhyFixture`]). Only a debug
/// build has the scenes; a `dev-seams` build without debug assertions warns
/// that it reads the machine.
const WHY_FIXTURE_SEAM: &str = "ATERM_DEV_WHY_FIXTURE";

/// THE PULLS THAT ASK FOR THE SPLIT: while one is armed (a `sys why`), a
/// worker's pull reads the split set ([`split_due`]), twice when its last
/// split tick is missing or too old for a window ([`Source::split_cold`]).
/// It logs no edge: the slots' are `lab slot-lane`'s to count.
pub(crate) static SPLIT_ARMED: Arming = Arming::new();

/// HOW SOON A WHOLE READING IS DUE AGAIN with no window showing the bars,
/// after one that gave `view` (design §6.1, kind 3): the engine's cadence
/// ([`aterm_vitals::jobs::Demand::every_ms`], never under [`JOBS_FLOOR`])
/// while a job is live and the engine is not idle; `None` when every live
/// job waits only for the person, or none is live (an exited row, kept a
/// minute for its event, arms nothing).
fn job_demand_after(view: &aterm_vitals::jobs::JobsView) -> Option<Duration> {
    let live = view
        .rows
        .iter()
        .any(|r| r.verdict != aterm_vitals::jobs::Verdict::Exited);
    let every = view.demand.every_ms().filter(|_| live)?;
    Some(Duration::from_millis(every).max(JOBS_FLOOR))
}

/// THE KERNEL EDGE (design §6.1, "What brings job demand back": `NOTE_EXIT`, `NOTE_FORK`).
pub(crate) struct ProcEdge {
    /// The loop wants an edge and holds none heard.
    armed: AtomicBool,
    /// An event was heard since the loop last took one.
    fired: AtomicBool,
    /// Where [`Self::heard`] posts: the loop's proxy, once the thread starts.
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

    /// A session's process watch heard a fork, an `exec` or an exit (a PTY gather).
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

    /// The loop, each turn: armed exactly while it wants an edge and holds none heard.
    pub(crate) fn arm(&self, want: bool) {
        let on = want && !self.fired.load(Ordering::Acquire);
        self.armed.store(on, Ordering::Release);
    }

    /// The loop: whether an event was heard since it last took one.
    pub(crate) fn take(&self) -> bool {
        self.fired.swap(false, Ordering::AcqRel)
    }

    /// Where [`Self::heard`] posts from now on.
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

/// The process's kernel edge: every session's gather tells it, the App's [`VitalsHost`] arms and takes it (a test App its own).
pub(crate) static PROC_EDGE: ProcEdge = ProcEdge::new();

/// What the thread leaves the loop under one leaf lock: its newest snapshot and the job events raised since the loop last took them.
#[derive(Default)]
pub(crate) struct Handover {
    /// The newest snapshot (taken by [`VitalsHost::adopt`]).
    snap: Option<Arc<Snapshot>>,
    /// The terminal words of the request that snapshot was read with
    /// ([`TtyWord::seen`]): what its jobs view named each park from.
    words: Vec<WordSeen>,
    /// Job verdicts that moved, oldest first, at most [`JOB_EVENTS_CAP`].
    job_events: Vec<aterm_vitals::events::Event>,
}

/// Who would see a reading now (v2 §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Audience {
    /// A window in view shows the bars or the Fleet Monitor tab, and one of aterm's windows is focused.
    Shown,
    /// They are in view, with aterm in the background.
    Background,
    /// A window draws the bars, but every such window is hidden or occluded.
    Hidden,
    /// No window draws the bars or the Monitor tab.
    None,
}

impl Audience {
    /// From what the windows show: `in_view`, a window in view draws the bars
    /// or the Monitor tab; `drawn`, some window draws the bars, in view or not;
    /// `focused`, one of aterm's windows has the focus.
    fn of(in_view: bool, drawn: bool, focused: bool) -> Self {
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

    /// The reading it asks for: how often, and whether the machine counters alone.
    pub(crate) fn cadence(self, lens: bool) -> Option<(Duration, bool)> {
        match self {
            Self::None => None,
            Self::Hidden => Some((HIDDEN_PERIOD, true)),
            Self::Shown | Self::Background if lens => Some((crate::stats_lens::LENS_PERIOD, false)),
            Self::Shown => Some((FOCUSED_PERIOD, false)),
            Self::Background => Some((BACKGROUND_PERIOD, false)),
        }
    }
}

/// When the next reading is due: one `period` after the last was asked
/// (`asked`), or at `armed` where that is sooner (a lens asked for one now).
fn due_at(
    asked: Option<Instant>,
    armed: Option<Instant>,
    period: Duration,
    now: Instant,
) -> Instant {
    let paced = asked.map_or(now, |t| t + period);
    armed.map_or(paced, |a| a.min(paced))
}

/// What the loop asks the thread to read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Request {
    /// This process's pid (aterm's tree is rooted here).
    pub(crate) self_pid: u32,
    /// Every pane's shell: each terminal session a tab holds is its own root, so a split pane's processes count in that pane.
    pub(crate) tabs: Vec<TabRoot>,
    /// Those roots' terminal mode words, where read ([`TtyWord`]).
    pub(crate) ttys: Vec<TtyWord>,
    /// Where each of those sessions sits (window, tab), for `sys`.
    pub(crate) layout: Vec<PaneFact>,
    /// Authenticated global session ids for those process-local tab keys.
    pub(crate) session_ids: Vec<(u64, String)>,
    /// The cadence this reading was asked at, ms: the gap rule's (v2 §5.4).
    pub(crate) cadence_ms: u32,
    /// Read the machine counters alone: every window that draws the bars is hidden (v2 §5.3).
    pub(crate) machine_only: bool,
    /// The stuck engine looks at this reading even inside its own cadence.
    pub(crate) jobs_now: bool,
    /// A control worker's pull ([`Msg::Pull`]; the thread sets it): it reads
    /// the split set only while a `sys why` arms it ([`SPLIT_ARMED`]).
    pub(crate) pull: bool,
    /// A window in view shows the bars, as the Deck or the Sill (the Fleet
    /// Monitor tab alone shows none): only then is the bars' reading a split
    /// tick ([`split_due`]). The Sill draws no cause clause, but a close-up
    /// opens from it as from the Deck, and a close-up's causes are the
    /// episodes already open.
    /// Every request the loop sends says it as of then, a layout's too.
    pub(crate) deck: bool,
    /// Every agent session's token totals, each session once.
    pub(crate) tokens: Vec<TokenRow>,
    /// The front tab's plan usage, while current.
    pub(crate) plan: Option<PlanFact>,
}

/// One agent session's token totals as the loop copies them from its footer's facts, and when the footer's resolver last read them (each read folds the session anew).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TokenRow {
    pub(crate) totals: TabTokens,
    pub(crate) read_at: Option<Instant>,
}

/// A session's TERMINAL MODE WORD (design §6.6), read by the loop for a reading.
#[derive(Clone, Debug)]
pub(crate) struct TtyWord {
    /// The session's key ([`TabRoot::key`]).
    pub(crate) key: u64,
    /// `ICANON`.
    pub(crate) canonical: bool,
    /// `ECHO`.
    pub(crate) echo: bool,
    /// The session's output stamp just before the word was read.
    output_seen: u64,
    /// The session's output stamp (`Session::latest_output_activity_ns`),
    /// which its PTY reader moves on every burst.
    output: Arc<AtomicU64>,
    /// The sink's driven input just before the word was read.
    input_seen: aterm_session::sink::DrivenInput,
    /// The sink's driven-input count, which every key, paste, report and
    /// control verb's bytes move (no descriptor: [`aterm_session::sink::DrivenInputWatch`]).
    input: aterm_session::sink::DrivenInputWatch,
}

impl TtyWord {
    /// The word of session `key`'s terminal behind `sink`, whose output stamp is `output`.
    pub(crate) fn read(
        key: u64,
        sink: &aterm_session::sink::SinkWriter,
        output: &Arc<AtomicU64>,
    ) -> Option<Self> {
        Self::read_seamed(key, sink, output, || {})
    }

    /// [`Self::read`], running `between` after the stamps are loaded and
    /// before the terminal is read (a test moves them there).
    fn read_seamed(
        key: u64,
        sink: &aterm_session::sink::SinkWriter,
        output: &Arc<AtomicU64>,
        between: impl FnOnce(),
    ) -> Option<Self> {
        let output_seen = output.load(Ordering::Acquire);
        let input = sink.driven_input_watch();
        let input_seen = input.get();
        between();
        let echo = sink.tty_echo()?;
        if aterm_pty::input_queue_len(sink.master()).is_some_and(|n| n > 0) {
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

    /// What the loop keeps of the word once a reading made with it lands ([`Handover::words`]).
    pub(crate) fn seen(&self) -> WordSeen {
        WordSeen {
            key: self.key,
            canonical: self.canonical,
            echo: self.echo,
            input: self.input_seen,
        }
    }

    /// The engine's word for the tick at `at_ms`, stamped then.
    pub(crate) fn current(&self, at_ms: u64) -> Option<(u64, aterm_vitals::jobs::TtyMode)> {
        (self.output.load(Ordering::Acquire) == self.output_seen
            && self.input.get() == self.input_seen)
            .then_some((
                self.key,
                aterm_vitals::jobs::TtyMode {
                    canonical: self.canonical,
                    echo: self.echo,
                    stamped_ms: at_ms,
                },
            ))
    }
}

/// A [`TtyWord`] as a reading's request carried it ([`TtyWord::seen`]).
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

/// A parked session's own write asks one reading once this long passed since a reading looked at the session, in its own output wake and only when no edge's floor stands ([`OutputEdge::Recheck`]).
pub(crate) const PARKED_RECHECK: Duration = JOBS_FLOOR;

/// A test's stand-in for a PTY reader's feed of session `u64`.
#[cfg(test)]
pub(crate) type ReaderFeed = Box<dyn Fn(&crate::App, u64)>;

#[cfg(test)]
thread_local! {
    /// Run by [`crate::App::on_output_wake`] just before the pass clears the session's output latch.
    pub(crate) static BEFORE_LATCH_CLEAR: std::cell::RefCell<Option<ReaderFeed>> =
        const { std::cell::RefCell::new(None) };
}

/// A SESSION PARKED ON THE PERSON'S READ (design §6.1, "What brings job demand back").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Parked {
    /// The parked job's process group.
    pub(crate) pgid: u32,
    /// The word the engine named the read from.
    pub(crate) word: WordSeen,
    /// When a reading last looked at the session: the view that parked it, or its own write's [`OutputEdge::Recheck`].
    pub(crate) read_at: Instant,
}

impl Parked {
    /// What `view` parks, as the request `words` named it, at `now`.
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

/// What one write of a session is to the stuck engine with no window showing the bars ([`crate::App::vitals_output_kind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputEdge {
    /// A job may have started or ended: a reading at once, or at the floor.
    Edge,
    /// Nothing new for the engine.
    Quiet,
    /// A parked screen's redraw whose session no reading looked at for [`PARKED_RECHECK`].
    Recheck,
}

/// What the thread is sent.
#[derive(Debug)]
pub(crate) enum Msg {
    /// A reading the loop asked (the bars' cadence, the disk watch's look, the engine's demand, an edge), with the layout now.
    Read(Box<Request>),
    /// The layout changed; no reading (the loop).
    Facts(Box<Request>),
    /// A whole reading now with the layout held (a control worker's `sys`).
    Pull,
}

/// The loop's side of the probe thread.
pub(crate) struct VitalsHost {
    /// The request channel; `None` until the thread starts (on the loop's
    /// first wake) or when it could not start.
    tx: Option<SyncSender<Msg>>,
    /// The layout last sent to the thread ([`crate::App::layout_fingerprint`]).
    sent_layout: Option<u64>,
    /// When the loop last looked at the layout ([`LAYOUT_EVERY`]).
    layout_looked: Option<Instant>,
    /// The workers' copy the thread handed its sender to: the quit takes it
    /// back ([`Self::stop_at_exit`]).
    sys: Option<&'static crate::sys_host::Shared>,
    /// The quit can precede construction of the thread's readers. A sender
    /// handed to the workers after that quit must be taken back too.
    stopping: Arc<AtomicBool>,
    /// The running thread and the channel that hangs up when it ends: the
    /// quit's bounded wait for its last write ([`Self::stop_at_exit`]).
    thread: Option<(std::thread::JoinHandle<()>, Receiver<()>)>,
    /// Set once a start was tried, so a failed start is not retried every tick.
    tried: bool,
    /// Where the thread leaves its newest snapshot and job events.
    latest: Arc<Mutex<Handover>>,
    /// The snapshot the Deck draws.
    pub(crate) snapshot: Option<Arc<Snapshot>>,
    /// When the held snapshot's jobs view was adopted: where a reading no window shows counts its next from ([`Self::windowless_due`]).
    pub(crate) adopted_at: Option<Instant>,
    /// Last full-sweep adoption, independent of the jobs engine's held view.
    full_adopted_at: Option<Instant>,
    /// When the next reading is due: the bars' cadence while a window in view shows them.
    pub(crate) next_due: Option<Instant>,
    /// When the last reading was asked for: the next is due one period of
    /// the cadence in force after it ([`due_at`]).
    asked_at: Option<Instant>,
    /// With no window in view showing the bars, when the jobs engine's next
    /// whole reading is due ([`Self::windowless_due`]); `None` while one is
    /// read (it is named again when it lands) or none is due.
    jobs_due: Option<Instant>,
    /// A delegated full sweep's next deadline; absent while a sweep is pending.
    delegated_due: Option<Instant>,
    /// Whether the last judgement found a window in view showing the bars:
    /// the hand-over between the two cadences happens where it turns.
    windowed: bool,
    /// The disk watch looked while no window in view showed the bars, and its
    /// one reading is not asked yet ([`crate::App::vitals_disk_looked`]).
    disk_pull: bool,
    /// Output came, or a process event, with no window showing the bars
    /// ([`Self::note_edge`]): a reading is owed while the held view is
    /// parked.
    edge: bool,
    /// When the loop last asked an edge's reading ([`JOBS_FLOOR`]).
    last_edge_read: Option<Instant>,
    /// Each session's screen generation at its last output wake with no
    /// window showing the bars: a write that leaves it where it was moved no
    /// cell, and is no edge ([`crate::App::vitals_output_kind`]).
    screens: std::collections::HashMap<u64, crate::control::ScreenGen>,
    /// The sessions the held jobs view parks on the person's read
    /// ([`Parked::of`], named again with each new view): their redraws are
    /// no edge while the park holds ([`Parked::holds`]).
    parked: std::collections::HashMap<u64, Parked>,
    /// Job events taken from the hand-over, for the timelines.
    job_events: Vec<aterm_vitals::events::Event>,
    /// Each session's count of job events recorded (`EVENT … job rev=`).
    job_revs: std::collections::HashMap<u64, u64>,
    /// Last hidden-read judgement, so the first delegated registration asks
    /// once even with no prior snapshot, without accumulating pending reads.
    delegated_active: bool,
    /// Whether the chips' load meters were last handed over with the stats
    /// bars shown ([`crate::App::push_chip_loads`]): turning the bars off
    /// clears the native strips' meters on the next wake.
    chips_shown: bool,
    /// The "where" survey's slot (`crate::disk_where`): the loop starts a
    /// survey, the thread copies its rows into each reading.
    survey: Arc<Where>,
    /// Where each adopted jobs view is posted
    /// ([`crate::App::post_jobs_reading`]): the process's
    /// [`crate::vitals_jobs::BOARD`]; a test's own in a test.
    board: &'static crate::vitals_jobs::JobsBoard,
    /// The kernel edge the sessions' gathers tell: the process's
    /// [`PROC_EDGE`]; each test App its own, which no gather tells.
    proc_edge: &'static ProcEdge,
}

impl Default for VitalsHost {
    fn default() -> Self {
        Self {
            tx: None,
            sent_layout: None,
            layout_looked: None,
            sys: None,
            stopping: Arc::default(),
            thread: None,
            tried: false,
            latest: Arc::default(),
            snapshot: None,
            adopted_at: None,
            full_adopted_at: None,
            next_due: None,
            asked_at: None,
            jobs_due: None,
            delegated_due: None,
            windowed: false,
            disk_pull: false,
            edge: false,
            last_edge_read: None,
            screens: std::collections::HashMap::new(),
            parked: std::collections::HashMap::new(),
            job_events: Vec::new(),
            job_revs: std::collections::HashMap::new(),
            delegated_active: false,
            chips_shown: false,
            survey: Arc::default(),
            board: &crate::vitals_jobs::BOARD,
            proc_edge: if cfg!(test) {
                Box::leak(Box::new(ProcEdge::new()))
            } else {
                &PROC_EDGE
            },
        }
    }
}

impl VitalsHost {
    /// Whether a "where" survey was asked for: one runs, or one left its rows (the Disk lens's behaviour tests, unix only, read it).
    #[cfg(all(test, unix))]
    pub(crate) fn survey_asked(&self) -> bool {
        let (last, running) = self.survey.read();
        running || last.is_some()
    }

    /// Take the thread's newest snapshot if it is newer than the one held, at `now`, and the job events it raised.
    pub(crate) fn adopt(&mut self, now: Instant) -> bool {
        let (fresh, words, events) = {
            let mut h = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            (
                h.snap.take(),
                std::mem::take(&mut h.words),
                std::mem::take(&mut h.job_events),
            )
        };
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
                    self.parked = snap
                        .jobs
                        .as_deref()
                        .map(|view| Parked::of(view, &words, now))
                        .unwrap_or_default();
                }
                if !snap.machine_only {
                    self.full_adopted_at = Some(now);
                }
                self.snapshot = Some(snap);
                true
            }
            _ => false,
        }
    }

    /// When a reading no window shows is due again: the engine's own cadence
    /// from when the held jobs view was adopted, while that view holds a
    /// suspect agent call ([`crate::vitals_jobs::windowless_after`]); `None`
    /// otherwise, and the disk watch's next look asks.
    fn windowless_due(&self) -> Option<Instant> {
        let view = self.snapshot.as_ref()?.jobs.as_deref()?;
        let after =
            crate::vitals_jobs::windowless_after(view).or_else(|| job_demand_after(view))?;
        Some(self.adopted_at? + after)
    }

    /// The loop's deadline with no window in view showing the bars: the
    /// engine's next whole reading ([`Self::jobs_due`]), or a hidden deck's
    /// next machine reading (`hidden`, its period) where that is sooner.
    fn windowless_deadline(&self, hidden: Option<Duration>, now: Instant) -> Option<Instant> {
        let paced = hidden.map(|period| due_at(self.asked_at, None, period, now));
        let full = match (self.jobs_due, self.delegated_due) {
            (Some(engine), Some(delegated)) => Some(engine.min(delegated)),
            (engine, delegated) => engine.or(delegated),
        };
        match (full, paced) {
            (Some(engine), Some(paced)) => Some(engine.min(paced)),
            (engine, paced) => engine.or(paced),
        }
    }

    /// Output from a session, or a process event its watch heard ([`ProcEdge`]).
    pub(crate) fn note_edge(&mut self) {
        if !self.windowed && self.tx.is_some() {
            self.edge = true;
        }
    }

    /// Take a process event the kernel edge heard as an edge.
    fn take_proc_edge(&mut self) {
        if self.proc_edge.take() {
            self.note_edge();
        }
    }

    /// Arm the kernel edge while an edge would be new: no window shows the
    /// bars, the thread runs, and none is owed.
    fn arm_proc_edge(&self) {
        self.proc_edge
            .arm(!self.windowed && self.tx.is_some() && !self.edge);
    }

    /// Whether the probe thread runs: a parked jobs view speaks for now only while it does ([`crate::vitals_jobs::speaks_for_now`]).
    pub(crate) fn reader_runs(&self) -> bool {
        self.tx.is_some()
    }

    /// When the edge's reading is due: at once, or [`JOBS_FLOOR`] after the
    /// last one; `None` with no edge or a window showing the bars.
    fn edge_at(&self, now: Instant) -> Option<Instant> {
        if !self.edge || self.windowed {
            return None;
        }
        Some(
            self.last_edge_read
                .map_or(now, |t| (t + JOBS_FLOOR).max(now)),
        )
    }

    /// The job events taken from the thread, for the loop to record.
    pub(crate) fn take_job_events(&mut self) -> Vec<aterm_vitals::events::Event> {
        std::mem::take(&mut self.job_events)
    }

    /// The next `rev=` of session `key`'s job events.
    pub(crate) fn next_job_rev(&mut self, key: u64) -> u64 {
        let rev = self.job_revs.entry(key).or_insert(0);
        *rev += 1;
        *rev
    }

    /// Start the probe thread once, on the loop's first wake. It waits on
    /// its channel, reading nothing until the loop asks a reading or a worker
    /// pulls.
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

    /// Start the probe thread, once: `source` makes its readers on the
    /// thread, `post` tells the loop a snapshot waits (false once the loop is
    /// gone), the peaks live at `peaks`, and `sys` is the workers' copy the
    /// thread publishes to and hands a sender (none for a test's thread).
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
        let latest = Arc::clone(&self.latest);
        let workers = sys.map(|shared| (shared, tx.clone(), Arc::clone(&self.stopping)));
        match crate::qos::spawn_lane_thread(
            "aterm-vitals",
            crate::qos::Role::Background,
            move || {
                let _ended = ended;
                run(&rx, &latest, &post, source(), peaks, workers);
            },
        ) {
            Ok(handle) => {
                self.tx = Some(tx);
                self.sys = sys;
                self.thread = Some((handle, done));
            }
            Err(error) => {
                aterm_log::warn!("system vitals: the probe thread did not start: {error}");
            }
        }
    }

    /// The quit (the App is never dropped).
    pub(crate) fn stop_at_exit(&mut self, bound: Duration) -> bool {
        self.stopping.store(true, Ordering::Release);
        self.tx = None;
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
                     the peaks since the last write are lost"
                );
                false
            }
        }
    }

    /// Offer one message without waiting for the platform probe. A full queue
    /// keeps the older messages until the reader takes them; `false` when this
    /// one was not queued.
    fn offer(&mut self, msg: Msg) -> bool {
        let Some(tx) = self.tx.as_ref() else {
            return false;
        };
        match tx.try_send(msg) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                self.tx = None;
                false
            }
        }
    }
}

/// What the probe thread reads with: the platform readers and the model
/// ([`Reader`]), or a test's stand-in.
trait Source {
    /// The reading `req` asks for, folded into the trends.
    fn reading(&mut self, req: &Request) -> Snapshot;
    /// The model, whose peaks the thread restores and keeps.
    fn model(&mut self) -> &mut aterm_vitals::Vitals;
    /// The machine's words, for the workers' copy; a stand-in has none.
    fn machine(&self) -> Option<&Machine> {
        None
    }
    /// Whether a pull now reads twice for the split's window: a `sys why`
    /// arms the split and the last split tick is missing or too old for a
    /// window ([`SplitReader::cold`]); a stand-in reads no split.
    fn split_cold(&self) -> bool {
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

    fn model(&mut self) -> &mut aterm_vitals::Vitals {
        &mut self.vitals
    }

    fn machine(&self) -> Option<&Machine> {
        Some(&self.machine)
    }

    fn split_cold(&self) -> bool {
        self.split.cold(Instant::now())
    }
}

/// What one drain of the channel asks for.
#[derive(Default)]
struct Asked {
    read: bool,
    pull: bool,
    facts: bool,
}

impl Asked {
    fn take(&mut self, msg: Msg, req: &mut Request) {
        match msg {
            Msg::Read(r) => {
                *req = *r;
                self.facts = true;
                self.read = true;
            }
            Msg::Facts(r) => {
                let (cadence_ms, machine_only) = (req.cadence_ms, req.machine_only);
                *req = *r;
                req.cadence_ms = cadence_ms;
                req.machine_only = machine_only;
                self.facts = true;
            }
            Msg::Pull => self.pull = true,
        }
    }
}

/// The thread: one [`Source`], a reading per loop request or pull, the
/// newest layout always.
fn run(
    rx: &Receiver<Msg>,
    latest: &Mutex<Handover>,
    post: &dyn Fn() -> bool,
    mut source: impl Source,
    peaks: Option<PathBuf>,
    workers: Option<(
        &'static crate::sys_host::Shared,
        SyncSender<Msg>,
        Arc<AtomicBool>,
    )>,
) {
    let sys = workers.map(|(shared, pulls, stopping)| {
        shared.install(pulls);
        if stopping.load(Ordering::Acquire) {
            shared.uninstall();
        }
        shared
    });
    let (mut peaks, restored) = PeakFile::open(peaks, Instant::now());
    source.model().restore_peaks(restored);
    let no_machine = Machine::default();
    let mut req = Request::default();
    let mut last_whole: Option<Instant> = None;
    while let Ok(first) = rx.recv() {
        let mut asked = Asked::default();
        asked.take(first, &mut req);
        while let Ok(more) = rx.try_recv() {
            asked.take(more, &mut req);
        }
        if asked.facts
            && let Some(sys) = sys
        {
            sys.set_facts(req.layout.clone());
        }
        if !(asked.read || asked.pull) {
            continue;
        }
        if req.self_pid == 0 {
            // A pull before the loop's first layout: this process is the root.
            req.self_pid = std::process::id();
        }
        let snap = if asked.pull {
            pull(&mut source, &req, last_whole)
        } else {
            source.reading(&req)
        };
        let snap = Arc::new(snap);
        if !snap.machine_only {
            last_whole = Some(Instant::now());
        }
        peaks.keep(source.model().peaks(), Instant::now(), false);
        let words = req.ttys.iter().map(TtyWord::seen).collect();
        let jobs_moved = hand_over(latest, &snap, words);
        if let Some(sys) = sys {
            sys.publish(snap, source.machine().unwrap_or(&no_machine));
        }
        if (asked.read || jobs_moved) && !post() {
            break;
        }
    }
    peaks.keep(source.model().peaks(), Instant::now(), true);
}

/// A worker's pull ([`Msg::Pull`]) with the layout `req` holds, the thread's
/// last whole reading at `last_whole`. It is whole, and its rates span the
/// pull's own interval: its first reading at least [`MIN_WINDOW`] after the
/// last, then a second [`COLD_PAIR`] on when it is cold (no whole reading
/// within [`COLD_AFTER`], or a `sys why` arms a split whose last tick is
/// missing or too old for a window, [`Source::split_cold`], judged after
/// that wait). The gap rule takes the pair's spacing, or the longest a warm
/// pull waits, as its cadence.
fn pull(source: &mut impl Source, req: &Request, last_whole: Option<Instant>) -> Snapshot {
    if let Some(t) = last_whole {
        // A window under the floor is noise: wait out the rest, a pair's
        // first reading too (cold for the split alone, the last whole
        // reading can be a few ms old).
        std::thread::sleep(MIN_WINDOW.saturating_sub(t.elapsed()));
    }
    let cold = last_whole.is_none_or(|t| t.elapsed() > COLD_AFTER) || source.split_cold();
    let mut pull = req.clone();
    pull.machine_only = false;
    // A pull keeps the engine's cadence, whatever the held request asked of
    // it.
    pull.jobs_now = false;
    pull.pull = true;
    pull.cadence_ms = duration_ms(if cold { COLD_PAIR } else { COLD_AFTER }).max(req.cadence_ms);
    let warm = cold.then(|| {
        let warm = source.reading(&pull);
        std::thread::sleep(COLD_PAIR);
        warm
    });
    let mut snap = source.reading(&pull);
    if let Some(warm) = warm {
        keep_warm_events(warm, &mut snap);
    }
    snap
}

/// `d` in whole ms, saturating.
fn duration_ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

/// A cold pull's first reading is never published, but what it saw happen
/// (a job's verdict that moved, a tab that closed) happened: the detectors
/// moved on with it, so its events ride the reading that is published,
/// first, or they are lost to `sys since=`, `subscribe … events` and
/// `await sys`.
fn keep_warm_events(warm: Snapshot, snap: &mut Snapshot) {
    let mut events = warm.events;
    events.append(&mut snap.events);
    snap.events = events;
}

/// Leave `snap` for the loop, with its job events and the terminal `words`
/// of the request it was read with.
fn hand_over(latest: &Mutex<Handover>, snap: &Arc<Snapshot>, words: Vec<WordSeen>) -> bool {
    let events: Vec<_> = snap
        .events
        .iter()
        .filter(|e| matches!(e.kind, aterm_vitals::events::EventKind::Job { .. }))
        .cloned()
        .collect();
    let mut h = latest.lock().unwrap_or_else(PoisonError::into_inner);
    h.snap = Some(Arc::clone(snap));
    h.words = words;
    let moved = !events.is_empty();
    h.job_events.extend(events);
    if h.job_events.len() > JOB_EVENTS_CAP {
        let over = h.job_events.len() - JOB_EVENTS_CAP;
        h.job_events.drain(..over);
    }
    moved
}

/// The thermal level and Low Power Mode, where the OS says (macOS `NSProcessInfo`; elsewhere never).
pub(crate) type ThermalProbe = fn() -> Option<Thermal>;

/// The product's [`ThermalProbe`].
fn platform_thermal() -> Option<Thermal> {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{class, sel};
        // SAFETY: `+processInfo` is an immortal singleton.
        unsafe {
            let info =
                aterm_objc::send::send_id(class(c"NSProcessInfo").as_id(), sel!(processInfo));
            if info.is_null() {
                return None;
            }
            let low = aterm_objc::send::send_bool(info, sel!(isLowPowerModeEnabled));
            let raw = aterm_objc::send::send_isize(info, sel!(thermalState));
            Some(thermal_of(raw, Some(low)))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// `NSProcessInfoThermalState` (nominal 0, fair 1, serious 2, critical 3) as the portable level (design §8.1).
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

/// How often the counter reader's throttle level is read: its doc asks no
/// caller to poll it every second (up to 1,600 sysfs reads on the largest
/// machine), and a level does not move faster than a thermal zone warms.
const COUNTER_THROTTLE_EVERY: Duration = Duration::from_secs(10);

/// The counter reader's level ([`aterm_sysprobe::Probe::throttle`]: Linux's
/// heuristic from thermal zones and throttle counters) as the model's
/// thermal state; `None` where it gives none (macOS and Windows, which say
/// why).
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

/// What the OS family is called in `sys`'s words.
fn os_family() -> &'static str {
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

/// WHETHER A WHOLE READING `req` AT `now` IS A SPLIT TICK, the last having
/// read at `last` (module docs, THE SPLIT SET): a pull's only while a `sys
/// why` arms the split (`armed`), at least [`MIN_WINDOW`] after the last;
/// otherwise only the bars' while a window in view shows them
/// ([`Request::deck`]; no engine's reading: an edge, the disk watch's look,
/// its demand or a delegated sweep, [`Request::jobs_now`]), every
/// [`SPLIT_EVERY`], or every [`SPLIT_EVERY_LENS`] at a lens's cadence, within
/// half the cadence so a reading a hair early is not put off a whole period.
/// The first is due at once, and a base too old for a window only starts the
/// next one (`aterm_vitals::explain::split::CoalBase::window_after`). PURE.
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

/// THE COALITION LANE'S READERS (module docs, THE SPLIT SET): the kernel's
/// ([`Self::KERNEL`]), or the inert arm a unit test's reader gets
/// ([`Self::INERT`]), as its [`ThermalProbe`] is; a test hands its own
/// stand-ins.
#[derive(Clone, Copy)]
pub(crate) struct CoalProbe {
    /// The launch self-check, asked of this process
    /// ([`aterm_sysprobe::coalitions::self_check`]).
    pub(crate) self_check: fn(u32) -> Result<u64, &'static str>,
    /// The process sweep, each process's coalition read in it where the
    /// options ask ([`aterm_sysprobe::processes_with`]): the kernel's in
    /// every arm, so a test's stand-in can count what a reading asks.
    pub(crate) sweep: fn(aterm_sysprobe::SweepOpts) -> Option<aterm_sysprobe::Sweep>,
    /// Every coalition's counters ([`aterm_sysprobe::coalitions::coalitions`]).
    pub(crate) coalitions: fn() -> Option<aterm_sysprobe::coalitions::CoalitionRead>,
    /// A process's short name and birth, unix ms, from its kernel record
    /// ([`aterm_sysprobe::kproc::record`]).
    pub(crate) birth: fn(u32) -> Option<(String, i64)>,
    /// Every process's, by pid, in one read of the table
    /// ([`aterm_sysprobe::kproc::records`]): cheaper than [`Self::birth`]
    /// past [`BIRTHS_AT_ONCE`] pids.
    pub(crate) births: fn() -> Option<Births>,
    /// A process's image and signature ([`aterm_sysprobe::identity::identity`]).
    pub(crate) identity: fn(u32) -> Option<aterm_sysprobe::identity::Identity>,
    /// The machine's timeline ([`aterm_sysprobe::timeline::timeline`]).
    pub(crate) timeline: fn() -> Option<aterm_sysprobe::timeline::Timeline>,
    /// When the machine last woke ([`aterm_sysprobe::timeline::waketime_ms`]).
    pub(crate) waketime_ms: fn() -> Option<i64>,
}

impl CoalProbe {
    /// The kernel's readers (macOS; every other platform's lane answers
    /// none and fails the self-check `unsupported`).
    pub(crate) const KERNEL: Self = Self {
        self_check: aterm_sysprobe::coalitions::self_check,
        sweep: aterm_sysprobe::processes_with,
        coalitions: aterm_sysprobe::coalitions::coalitions,
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
        timeline: aterm_sysprobe::timeline::timeline,
        waketime_ms: aterm_sysprobe::timeline::waketime_ms,
    };

    /// The inert arm: no OS lane, nothing read but the sweep.
    pub(crate) const INERT: Self = Self {
        self_check: |_| Err("unsupported"),
        sweep: aterm_sysprobe::processes_with,
        coalitions: || None,
        birth: |_| None,
        births: || None,
        identity: |_| None,
        timeline: || None,
        waketime_ms: || None,
    };
}

/// The births a split tick reads in one read of the process table
/// ([`CoalProbe::births`]) rather than one record each: past this many pids
/// not yet read, the table is cheaper (m22, 2026-10-22, a debug build: 0.36
/// ms for 680 records against about 6 µs a record).
pub(crate) const BIRTHS_AT_ONCE: usize = 64;

/// Each process's kernel `p_comm` and start, unix ms, by pid
/// ([`CoalProbe::births`]).
pub(crate) type Births = HashMap<u32, (String, i64)>;

/// A kernel record's `p_comm` and start, unix ms ([`CoalProbe::KERNEL`]).
fn name_and_birth(r: aterm_sysprobe::kproc::Record) -> Option<(String, i64)> {
    let ms = i64::try_from(r.start_s)
        .ok()?
        .checked_mul(1000)?
        .checked_add(i64::from(r.start_us / 1000))?;
    Some((r.comm, ms))
}

/// Whether a kernel record's `p_comm` `record` names the process the sweep
/// named `swept`: the sweep's short info holds 15 bytes of it, the record 16
/// (`MAXCOMLEN`), so a 16-byte name is swept cut by one, decoded as the
/// record's first 15 bytes are (a letter the cut splits a U+FFFD); any other
/// name is swept whole. PURE.
fn names_the_same(record: &str, swept: &str) -> bool {
    /// The most of `p_comm` the short info holds (`MAXCOMLEN - 1`).
    const SHORT: usize = 15;
    record == swept
        || record
            .as_bytes()
            .get(..SHORT)
            .is_some_and(|cut| String::from_utf8_lossy(cut) == swept)
}

/// One swept process's birth as read ([`SplitReader::births`]), with the
/// sweep's name and kernel start for its pid then.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Birth {
    name: String,
    start: Option<u64>,
    ms: i64,
}

/// THE SPLIT SET'S HOST SIDE (module docs): the lane the self-check found,
/// when the last split tick read, and what is read once and kept.
pub(crate) struct SplitReader {
    probe: CoalProbe,
    /// The lane, from the launch self-check.
    lane: CoalLane,
    /// Whether pid 1 read as Apple's (the code-signing reads read as the
    /// readers hold them): without it every lead's voucher is unknown.
    vouches: bool,
    /// The pulls that arm the split: [`SPLIT_ARMED`], or a test's own.
    armed: &'static Arming,
    /// When the last split tick read.
    last: Option<Instant>,
    /// Each swept process's birth, by pid: kept while the sweep's name and
    /// known kernel start agree; read again with no start, after either
    /// moves, or after a split tick over [`SPLIT_WINDOW_MAX`] after the last.
    /// Gone when the pid leaves the sweep.
    births: HashMap<u32, Birth>,
    /// The `kern.waketime` the timeline was last read at; `None` until one
    /// read.
    wake_seen: Option<Option<i64>>,
    /// The timeline as read then.
    timeline: Option<TimelineSample>,
}

impl SplitReader {
    /// `probe`'s lane, from its self-check of this process (and whether pid
    /// 1 reads as Apple's), the split armed by `armed`. Run once, on the
    /// probe thread: the answer is a field, never a lazy static.
    pub(crate) fn new(probe: CoalProbe, armed: &'static Arming) -> Self {
        let lane = match (probe.self_check)(std::process::id()) {
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
            vouches,
            armed,
            last: None,
            births: HashMap::new(),
            wake_seen: None,
            timeline: None,
        }
    }

    /// Whether the whole reading `req` at `now` is a split tick
    /// ([`split_due`]): never with the lane off.
    fn due(&self, req: &Request, now: Instant) -> bool {
        self.lane.on() && split_due(self.last, req, self.armed.armed(), now)
    }

    /// Whether a pull at `now` must read twice for a window: a `sys why`
    /// arms the split, and the last split tick is missing or, within
    /// [`SPLIT_COLD_MARGIN`], older than a window can be
    /// ([`SPLIT_WINDOW_MAX`]).
    fn cold(&self, now: Instant) -> bool {
        self.lane.on()
            && self.armed.armed()
            && self.last.is_none_or(|t| {
                now.saturating_duration_since(t) + SPLIT_COLD_MARGIN > SPLIT_WINDOW_MAX
            })
    }

    /// The split tick at `now` of `sample`, whose sweep is its processes, and
    /// `read`, the coalitions read beside its cores: each swept process's
    /// birth, each selected lead's current identity, and the timeline.
    /// With no sweep, no split tick (the model needs both ends).
    fn fill(
        &mut self,
        sample: &mut Sample,
        read: aterm_sysprobe::coalitions::CoalitionRead,
        now: Instant,
    ) {
        let Some(procs) = sample.procs.as_mut() else {
            return;
        };
        if self
            .last
            .is_some_and(|t| now.saturating_duration_since(t) > SPLIT_WINDOW_MAX)
        {
            // No cached birth spans a long gap, even where the next sweep
            // supplies the same name and start token.
            self.births.clear();
        }
        let swept: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
        self.births.retain(|pid, _| swept.contains(pid));
        let mut unread = Vec::new();
        for (i, p) in procs.iter_mut().enumerate() {
            p.birth_ms = self.known(p);
            if p.birth_ms.is_none() {
                unread.push(i);
            }
        }
        // Many to read (the first tick, a build's storm): the whole table.
        let table = (unread.len() > BIRTHS_AT_ONCE)
            .then(self.probe.births)
            .flatten();
        for i in unread {
            procs[i].birth_ms = self.read_birth(&procs[i], table.as_ref());
        }
        let born: HashMap<u32, i64> = procs
            .iter()
            .filter_map(|p| Some((p.pid, p.birth_ms?)))
            .collect();
        let members: Vec<(u32, u64, i64)> = procs
            .iter()
            .filter_map(|p| Some((p.pid, p.coalition?, p.birth_ms?)))
            .collect();
        for (_, pid) in aterm_vitals::explain::leads(&members) {
            let Some(&birth) = born.get(&pid) else {
                continue;
            };
            // The short name is not an image identity: even a same-name
            // exec must refresh it. A missed read clears the model's old
            // image and signature, and the next split tick asks again.
            let id = (self.probe.identity)(pid).unwrap_or_default();
            sample
                .leads
                .push(lead_identity(pid, birth, id, self.vouches));
        }
        sample.coalitions = Some(coal_sample(read));
        sample.timeline = self.timeline();
        self.last = Some(now);
    }

    /// The birth read for `p`'s pid, while the sweep still names it the
    /// same and gives it the same kernel start.
    fn known(&self, p: &ProcSample) -> Option<i64> {
        let start = p.start?;
        self.births
            .get(&p.pid)
            .filter(|b| b.name == p.name && b.start == Some(start))
            .map(|b| b.ms)
    }

    /// `p`'s birth, unix ms, from its kernel record (`table`'s, where the
    /// tick read the whole table), kept for the ticks after
    /// ([`Self::known`]); `None` where that record is gone or is another
    /// process's (an exec or a reused pid between the sweep and the read),
    /// which the next tick reads again.
    fn read_birth(&mut self, p: &ProcSample, table: Option<&Births>) -> Option<i64> {
        let read = match table {
            Some(table) => table.get(&p.pid).cloned(),
            None => (self.probe.birth)(p.pid),
        };
        let Some((_, ms)) = read.filter(|(name, _)| names_the_same(name, &p.name)) else {
            self.births.remove(&p.pid);
            return None;
        };
        self.births.insert(
            p.pid,
            Birth {
                name: p.name.clone(),
                start: p.start,
                ms,
            },
        );
        Some(ms)
    }

    /// The timeline: read again only when `kern.waketime` moved since it
    /// was (or until one read).
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

/// The model's word for a lead `pid` born at `birth_ms` whose identity the
/// kernel answered `id`: its voucher unknown unless the code-signing reads
/// vouch (`vouches`, [`SplitReader::vouches`]). PURE.
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

/// The coalitions `read` as the model takes them. PURE.
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

/// THE WHY FIXTURE (the [`WHY_FIXTURE_SEAM`] dev seam, a debug build only):
/// a recorded scene of `aterm_vitals::explain::fixtures` the model reads in
/// place of the machine's clocks, CPU, processes (this one and its user among
/// them), tabs, delegated jobs and coalition lane, at the time since its
/// first reading and with the host's own split ticks, so a lab instance
/// shows the scene's causes on demand (the Deck's clause, `sys why`). The
/// rest of each reading (memory where the scene has none, the GPU, disk,
/// network, tokens) stays the machine's, and so does what the host does
/// with a reading beside the model (the build slots' holders, the stuck
/// engine's step); the delegated jobs' registry is published none. The
/// scene's figures hold over any window: every one of its processes keeps
/// one rate.
#[cfg(debug_assertions)]
pub(crate) struct WhyFixture {
    scene: aterm_vitals::explain::fixtures::Scene,
    /// The first reading's instant: the scene's origin.
    t0: Option<Instant>,
}

/// No fixture in a build without debug assertions.
#[cfg(not(debug_assertions))]
pub(crate) enum WhyFixture {}

#[cfg(debug_assertions)]
impl WhyFixture {
    /// The scene the seam names, if it names one.
    fn from_seam() -> Option<Self> {
        let named = aterm_types::dev_seam!(WHY_FIXTURE_SEAM)?;
        let scene = match named.to_str()? {
            "spotlight" => aterm_vitals::explain::fixtures::spotlight_1032(),
            "calm" => aterm_vitals::explain::fixtures::calm_1510(),
            other => {
                aterm_log::warn!("system vitals: {WHY_FIXTURE_SEAM}={other} names no scene");
                return None;
            }
        };
        aterm_log::info!(
            "system vitals: reading the {} scene",
            named.to_string_lossy()
        );
        Some(Self { scene, t0: None })
    }

    /// `live` with the scene's reading now in its place ([`Self::at`]).
    fn reading(&mut self, live: &Sample, split: bool) -> Sample {
        let t0 = *self.t0.get_or_insert_with(Instant::now);
        self.at(
            live,
            split,
            u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
        )
    }

    /// `live` with the scene's reading `at_ms` after its origin in its place:
    /// the clocks, the cores (one cluster of them), the processes (where
    /// `live` swept), the tabs and delegated jobs, the coalition lane and,
    /// on a split tick (`split`), its coalitions, leads and timeline; memory
    /// and the thermal state where the scene has them.
    fn at(&self, live: &Sample, split: bool, at_ms: u64) -> Sample {
        let scene = self.scene.reading(at_ms, split);
        let cores = scene.cores.as_ref().map_or(0, Vec::len);
        Sample {
            clusters: vec![ClusterSpec {
                name: "CPU".into(),
                logical: (0..cores).filter_map(|c| u16::try_from(c).ok()).collect(),
            }],
            procs: live.procs.as_ref().and(scene.procs),
            mem: scene.mem.or(live.mem),
            thermal: scene.thermal.or(live.thermal),
            gpu: live.gpu.clone(),
            volume: live.volume.clone(),
            cadence_ms: live.cadence_ms,
            machine_only: live.machine_only,
            load1: live.load1,
            load: live.load,
            swapped_kib: live.swapped_kib,
            links: live.links.clone(),
            proc_limits: live.proc_limits,
            pid_counts: live.pid_counts,
            disk: live.disk.clone(),
            tokens: live.tokens.clone(),
            plan: live.plan.clone(),
            ..scene
        }
    }
}

#[cfg(not(debug_assertions))]
impl WhyFixture {
    /// None: the scenes are a debug build's (`explain::fixtures`). A
    /// `dev-seams` build reads the seam and says it reads the machine.
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
}

/// The readers and the model: one per thread.
pub(crate) struct Reader {
    probe: aterm_sysprobe::Probe,
    clusters: Vec<ClusterSpec>,
    vitals: aterm_vitals::Vitals,
    /// The stuck engine's pass over each reading's sweep.
    jobs: crate::vitals_jobs::JobsPass,
    epoch: Instant,
    home: PathBuf,
    /// `net_links_into`'s buffer, kept from one reading to the next.
    scratch: Vec<u8>,
    /// This process's user (unix): the per-user process count's and the
    /// writers'.
    uid: Option<u32>,
    /// The "where" survey's slot.
    survey: Arc<Where>,
    /// The `job` events: a verdict that moved between two of its views.
    job_watch: aterm_vitals::events::JobWatch,
    /// The view the watch last stepped (the same one rides each snapshot
    /// until the engine is due again).
    watched: Option<Arc<aterm_vitals::jobs::JobsView>>,
    thermal: ThermalProbe,
    /// The counter reader's last level and when it was read, where the
    /// platform probe gives none.
    counter_throttle: Option<(Instant, Option<Thermal>)>,
    /// The build slots' reader (`aterm_sysprobe::slots`, without a lock):
    /// this thread's one, so `await sys slots>=…` and the occupancy board read
    /// one proof ([`Self::set_occupancy_facts`]).
    slots: aterm_sysprobe::slots::SlotReader,
    /// The split set's lane and what it keeps between split ticks.
    pub(crate) split: SplitReader,
    /// The why fixture's scene, read in place of the machine (a debug build
    /// with the seam set; never from the seam under test).
    fixture: Option<WhyFixture>,
    /// The machine's words, read once.
    pub(crate) machine: Machine,
}

/// THE STUCK ENGINE'S STEP OVER ONE WHOLE READING at `at_ms` (the probe thread's, [`Reader::read`]).
pub(crate) fn jobs_step(
    pass: &mut crate::vitals_jobs::JobsPass,
    at_ms: u64,
    sweep: Option<&[aterm_sysprobe::procs::ProcessReading]>,
    req: &Request,
    thrashing: bool,
) -> Option<Arc<aterm_vitals::jobs::JobsView>> {
    let ttys: Vec<_> = req.ttys.iter().filter_map(|w| w.current(at_ms)).collect();
    pass.tick(
        at_ms,
        sweep,
        &req.tabs,
        &ttys,
        req.self_pid,
        thrashing,
        req.jobs_now,
    )
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
        // The inert test arm: a unit test reads no coalition and no scene
        // unless it hands the reader its own.
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
            probe: aterm_sysprobe::Probe::new(),
            clusters,
            vitals: aterm_vitals::Vitals::new(),
            jobs: crate::vitals_jobs::JobsPass::new(),
            epoch: Instant::now(),
            slots: aterm_sysprobe::slots::SlotReader::new(aterm_sysprobe::slots::default_dir(
                &home,
            )),
            home,
            scratch: Vec::new(),
            uid: cfg!(unix).then(aterm_agent::harness::disk::discover::own_uid),
            survey,
            job_watch: aterm_vitals::events::JobWatch::default(),
            watched: None,
            thermal: if cfg!(test) {
                || None
            } else {
                platform_thermal
            },
            counter_throttle: None,
            split,
            fixture: None,
            machine: Machine {
                cpu: words.cpu,
                os: words.os,
                family: os_family(),
            },
        };
        reader.read_scene(fixture);
        reader
    }

    /// Read `fixture`'s scene, where there is one, in place of the machine,
    /// under the scene's lane whatever the machine's is.
    fn read_scene(&mut self, fixture: Option<WhyFixture>) {
        if fixture.is_some() {
            self.split.lane = CoalLane::On;
        }
        self.fixture = fixture;
    }

    /// The counter reader's throttle level, read at most every
    /// [`COUNTER_THROTTLE_EVERY`] (between reads, the last one).
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

    /// Step the job watch over the snapshot's jobs view when it is a new
    /// one: its `job` events join the snapshot's.
    fn watch_jobs(&mut self, snap: &mut Snapshot) {
        if let Some(view) = snap.jobs.as_ref()
            && !self.watched.as_ref().is_some_and(|w| Arc::ptr_eq(w, view))
        {
            let events = self.job_watch.step(view, snap.seq, snap.at_ns);
            snap.events.extend(events);
            self.watched = Some(Arc::clone(view));
        }
    }

    /// THE OCCUPANCY FACTS a snapshot carries beside the model's
    /// (`docs/DESIGN-harness-occupancy-2026-10-20.md` §2, §4.2), set after
    /// [`aterm_vitals::Vitals::absorb`], which leaves them unread: the memory
    /// the machine can hand out without paging (`build-slot`'s admission
    /// figure, [`aterm_sysprobe::machine_memory`]) and the build slots as this
    /// thread's reader proved them.
    fn set_occupancy_facts(&mut self, snap: &mut Snapshot) -> aterm_sysprobe::slots::SlotsRead {
        snap.avail =
            aterm_sysprobe::machine_memory().map(|m| m.available_mib.saturating_mul(1 << 20));
        let machine = aterm_sysprobe::slots::Machine {
            load5: snap.load.map(|l| l[1]),
            ncpu: snap.total_cores(),
        };
        let now_us = aterm_time::since_epoch()
            .and_then(|d| u64::try_from(d.as_micros()).ok())
            .unwrap_or(0);
        let now = Instant::now();
        let armed = crate::occupancy_host::SLOTS_ARMED.armed();
        let read = self.slots.read(now, now_us, &machine, armed);
        snap.slots = Some(crate::occupancy_host::slots_view(&read));
        snap.slots_age_ms = self.slots.read_at().map(|at| {
            u64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(u64::MAX)
        });
        read
    }

    /// One full reading folded into the trends: the machine counters and the
    /// process sweep, the split set where it is a split tick
    /// ([`SplitReader::due`]), and the stuck engine's pass over that sweep.
    pub(crate) fn read(&mut self, req: &Request) -> Snapshot {
        let now = Instant::now();
        let split = self.split.due(req, now);
        let live_split = split && self.fixture.is_none();
        let sweep = (self.split.probe.sweep)(aterm_sysprobe::SweepOpts {
            coalitions: live_split,
        });
        // Between the sweep and the machine's sample, which takes its stamp,
        // then the home volume, the load average and the cores' ticks: each
        // end of the split's window reads the sweep, the coalitions and the
        // stamp and ticks after them together.
        let coalitions = (live_split && sweep.is_some())
            .then(self.split.probe.coalitions)
            .flatten();
        let mut sample = self.machine_sample(req);
        sample.machine_only = false;
        sample.procs_truncated = sweep.as_ref().is_some_and(|s| s.truncated);
        let observed = sweep
            .as_ref()
            .map(|sweep| crate::resource_jobs::sample(sweep, &req.session_ids, self.epoch))
            .unwrap_or_default();
        sample.procs = observed.procs;
        sample.delegated = observed.delegated;
        if let Some(read) = coalitions {
            self.split.fill(&mut sample, read, now);
        }
        // The why fixture's scene is the model's reading; the build slots'
        // holders and the stuck engine keep the machine's.
        let scene = self.fixture.as_mut().map(|fixture| {
            if split {
                self.split.last = Some(now);
            }
            fixture.reading(&sample, split)
        });
        let mut snap = self.vitals.absorb(scene.as_ref().unwrap_or(&sample));
        if self.split.lane.on() && snap.coal_lane == CoalLane::Off && scene.is_none() {
            // The model found a read misread: the lane is off for good, so
            // nothing more is read for it.
            aterm_log::warn!(
                "system vitals: the coalitions lane is off: a read did not read as the \
                 record's layout"
            );
            self.split.lane = CoalLane::Off;
        }
        let slots = self.set_occupancy_facts(&mut snap);
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
        // A scene's delegated jobs are not the machine's: none is published.
        crate::resource_jobs::publish(
            sample
                .procs
                .as_ref()
                .and(snap.aterm.as_ref())
                .filter(|_| scene.is_none())
                .map(|_| snap.delegated.clone()),
        );
        let thrashing = sample.mem.is_some_and(|m| m.pressure == Pressure::Critical);
        snap.jobs = jobs_step(
            &mut self.jobs,
            sample.at_ns / 1_000_000,
            sweep.as_ref().map(|s| s.procs.as_slice()),
            req,
            thrashing,
        );
        self.watch_jobs(&mut snap);
        snap
    }

    /// A hidden deck's reading (v2 §5.3): the machine counters alone, no process sweep.
    pub(crate) fn read_machine(&mut self, req: &Request) -> Snapshot {
        let mut sample = self.machine_sample(req);
        sample.machine_only = true;
        // The why fixture's scene is the model's reading; the stuck engine
        // keeps the machine's clock.
        let scene = self
            .fixture
            .as_mut()
            .map(|fixture| fixture.reading(&sample, false));
        let mut snap = self.vitals.absorb(scene.as_ref().unwrap_or(&sample));
        self.set_occupancy_facts(&mut snap);
        snap.jobs = self.jobs.tick(
            sample.at_ns / 1_000_000,
            None,
            &req.tabs,
            &[],
            req.self_pid,
            false,
            false,
        );
        self.watch_jobs(&mut snap);
        snap
    }

    /// Everything but the process sweep: core ticks, memory and swap, the
    /// GPU, the home volume and the disk facts, the links, the load average,
    /// the process counts and limits, and what the loop copied (tokens, the
    /// plan), with the clocks the gap rule reads.
    fn machine_sample(&mut self, req: &Request) -> Sample {
        let since = self.epoch.elapsed();
        let shelf = &crate::disk_board::SHELF;
        let (volume, key) = home_volume(&self.home, shelf.watched());
        let load = aterm_sysprobe::load_average();
        Sample {
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
            mem: aterm_sysprobe::memory_detail().map(mem_sample),
            gpu: aterm_sysprobe::gpu().map(gpu_sample),
            volume: volume.map(|v| VolumeSample {
                total: v.total,
                free: v.free,
                available: v.available,
                mount: v.mount.display().to_string(),
            }),
            procs: None,
            self_pid: req.self_pid,
            tabs: req.tabs.clone(),
            delegated: Vec::new(),
            at_unix_ms: aterm_time::unix_millis()
                .and_then(|ms| i64::try_from(ms).ok())
                .unwrap_or(0),
            mono_ms: u64::try_from(since.as_millis()).unwrap_or(u64::MAX),
            cadence_ms: req.cadence_ms,
            machine_only: req.machine_only,
            load1: load.map(|l| l[0]),
            load,
            thermal: (self.thermal)().or_else(|| self.counter_throttle()),
            swapped_kib: aterm_sysprobe::swapped_kib(),
            links: aterm_sysprobe::net_links_into(&mut self.scratch)
                .map(|links| links.iter().map(link_sample).collect()),
            proc_limits: proc_limits(aterm_sysprobe::process_limits()),
            pid_counts: aterm_sysprobe::pid_counts(self.uid.unwrap_or(0)),
            self_uid: self.uid,
            disk: Some(disk_facts(
                shelf,
                key.as_ref(),
                Clock::read,
                &self.survey,
                &self.home,
            )),
            tokens: req
                .tokens
                .iter()
                .map(|row| TabTokens {
                    read_ms: row.read_at.map(|at| mono_ms_of(at, self.epoch)),
                    ..row.totals.clone()
                })
                .collect(),
            plan: req.plan.clone(),
            procs_truncated: false,
            // A split tick's whole reading adds them ([`SplitReader::fill`]).
            coalitions: None,
            leads: Vec::new(),
            timeline: None,
            coal_lane: self.split.lane,
        }
    }
}

/// `at` on the model's monotonic clock, whose origin is `epoch`: ms since
/// it, negative for an instant before it.
fn mono_ms_of(at: Instant, epoch: Instant) -> i64 {
    match at.checked_duration_since(epoch) {
        Some(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        None => i64::try_from(epoch.duration_since(at).as_millis()).map_or(i64::MIN, |ms| -ms),
    }
}

/// One process of a sweep as the model takes it, its read time on the model's clock (whose origin is `epoch`).
pub(crate) fn proc_sample(p: &aterm_sysprobe::ProcessReading, epoch: Instant) -> ProcSample {
    ProcSample {
        pid: p.pid,
        start: p.start,
        ppid: p.ppid,
        cpu_ns: p.cpu_ns,
        footprint: p.footprint,
        name: p.name.clone(),
        disk_written: p.disk_written,
        uid: p.uid,
        pgid: p.pgid,
        state: match p.state {
            RunState::Running | RunState::Sleeping | RunState::Uninterruptible | RunState::Idle => {
                ProcState::Run
            }
            RunState::Stopped => ProcState::Stopped,
            RunState::Zombie => ProcState::Zombie,
            RunState::Unknown => ProcState::Unknown,
        },
        runnable_ns: p.runnable_ns,
        peak: p.peak_footprint,
        at_ns: p.read_at.map(|t| {
            u64::try_from(t.saturating_duration_since(epoch).as_nanos()).unwrap_or(u64::MAX)
        }),
        coalition: p.coalition,
        // A split tick reads it ([`SplitReader::fill`]).
        birth_ms: None,
    }
}

/// The machine's memory as the model takes it. A class this OS does not
/// report adds nothing; Linux's shared memory is in use, never cache (design
/// §9.6), so it joins `app`.
fn mem_sample(m: aterm_sysprobe::MemoryDetail) -> MemSample {
    MemSample {
        total: m.total,
        app: m.app.saturating_add(m.shared.unwrap_or(0)),
        wired: m.wired,
        compressed: m.compressed,
        cached: m.file_backed.saturating_add(m.purgeable.unwrap_or(0)),
        free: m.free,
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
        }),
        kernel_available_pct: m.kernel_available_pct,
    }
}

/// The GPU as the model takes it. A card the OS put to sleep (Linux runtime
/// PM) is doing no work: 0 %, known without waking it.
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
    }
}

/// One network link as the model takes it: its counters, its rate, whether
/// its bytes are the machine's own (`aterm_sysprobe::counts_toward_machine`),
/// and its kind in the model's words.
fn link_sample(l: &aterm_sysprobe::LinkReading) -> LinkSample {
    let named = |prefixes: &[&str]| prefixes.iter().any(|p| l.name.starts_with(p));
    LinkSample {
        name: l.name.clone(),
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
        counts: aterm_sysprobe::counts_toward_machine(l),
        rx: l.rx.bytes,
        tx: l.tx.bytes,
        rate_bps: l.rate_bps,
    }
}

/// The process limits as the model takes them.
fn proc_limits(l: aterm_sysprobe::ProcessLimits) -> ProcLimits {
    ProcLimits {
        system: l.system,
        per_user: l.per_user,
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

/// The home volume, the disk row's (v2 §6.4: it never follows the focused
/// tab), and its key on the disk board when `keyed` (a disk watch runs): the
/// key of the reading the disk watch takes of it
/// (`aterm_agent::harness::disk::volume_of`), so the deck asks the board for
/// the row the watch wrote.
fn home_volume(
    home: &Path,
    keyed: bool,
) -> (Option<aterm_sysprobe::VolumeReading>, Option<VolumeKey>) {
    #[cfg(windows)]
    {
        let _ = keyed;
        (aterm_sysprobe::volume(home), None)
    }
    #[cfg(not(windows))]
    {
        let table = aterm_sysprobe::volumes::table();
        let key = keyed
            .then(|| aterm_agent::harness::disk::volume_of(home, &table).ok())
            .flatten()
            .map(|r| r.key);
        (local_volume(home, &table, aterm_sysprobe::volume), key)
    }
}

/// Read only a mount root that a complete table calls local and not automounted.
#[cfg(any(not(windows), test))]
fn local_volume(
    path: &Path,
    table: &aterm_sysprobe::volumes::Table,
    mut read: impl FnMut(&Path) -> Option<aterm_sysprobe::VolumeReading>,
) -> Option<aterm_sysprobe::VolumeReading> {
    if !table.complete {
        return None;
    }
    let mount = aterm_sysprobe::volumes::mount_of(path, table)?;
    if !mount.local || mount.automounted || mount.fstype == "autofs" {
        return None;
    }
    read(&mount.on)
}

/// The deck's disk facts (v2 §7.1): the board's for the home volume (its
/// key `home_key`, the clocks read by `now` only when they are asked) while
/// a disk watch runs ([`Shelf::watched`]: the harness is on), mapped onto the
/// model's own types, and the "where" survey's rows and whether one runs.
fn disk_facts(
    shelf: &Shelf,
    home_key: Option<&VolumeKey>,
    now: impl FnOnce() -> Clock,
    survey: &Where,
    home: &Path,
) -> DiskFacts {
    let raw = match home_key {
        Some(key) if shelf.watched() => shelf.deck_facts(key, &now()),
        _ => DiskFactsRaw::default(),
    };
    let (found, sizing) = survey.read();
    deck_disk(raw, found, sizing, home)
}

/// The board's facts `raw` in the model's types, with the "where" survey's
/// rows `found` in place of the last pass's where the pass named none, or
/// where the survey is newer and whole (it sized something, and neither its
/// search nor its sizing ran out of time), and `sizing` while a survey runs.
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
    }
}

/// The watch's level in the model's words; an emergency the inode line made
/// (`files`) is its own.
fn disk_level(level: aterm_agent::harness::disk::Level, files: bool) -> Option<DiskLevel> {
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

/// One pane's token row from its Claude Code footer's `facts` (v2 §8.3),
/// keyed to the tab `key` that holds the pane: the session's cumulative
/// totals, what its fold left unread, and when the footer read them
/// (`read_at`).
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

/// Each session once: a session two panes show (a resumed conversation, a
/// pane moved) is counted by the first that shows it.
fn each_session_once(mut rows: Vec<TokenRow>) -> Vec<TokenRow> {
    let mut seen = std::collections::HashSet::with_capacity(rows.len());
    rows.retain(|t| seen.insert(t.totals.session.clone()));
    rows
}

/// The front tab's plan usage as the deck's dated fact (v2 §8.4), while it
/// is current at `now` (unix seconds): its binding window in words, its
/// share, when it was read, and its account's number among `accounts` (every
/// tab's, in tab order).
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

/// A plan window as Details names it: `5-hour`, `weekly`, `Opus weekly`.
fn window_word(window: Bucket) -> &'static str {
    match window {
        Bucket::FiveHour => "5-hour",
        Bucket::SevenDay | Bucket::Other => "weekly",
        Bucket::SevenDayOpus => "Opus weekly",
        Bucket::SevenDaySonnet => "Sonnet weekly",
        Bucket::Fable => "Fable weekly",
    }
}

/// The peaks' file (v2 §8.2), `<state>/vitals-peaks.json`, and what it holds.
struct PeakFile {
    /// Where; `None` without a state root, and nothing is kept.
    path: Option<PathBuf>,
    /// What the file holds: as read, or as last written.
    held: Peaks,
    /// When it was read or last written.
    at: Instant,
}

impl PeakFile {
    /// The file at `path`, read at `now`, and the peaks it holds: none where
    /// it is missing, damaged, larger than [`PEAKS_MAX_BYTES`] or of another
    /// layout.
    fn open(path: Option<PathBuf>, now: Instant) -> (Self, Peaks) {
        let held = path
            .as_deref()
            .and_then(read_small)
            .and_then(|text| peaks_of(&text))
            .unwrap_or_default();
        let file = Self {
            path,
            held: held.clone(),
            at: now,
        };
        (file, held)
    }

    /// Write `peaks` where they differ from what the file holds: at most once
    /// every [`PEAKS_EVERY`], or now when the thread is `stopping`.
    fn keep(&mut self, peaks: &Peaks, now: Instant, stopping: bool) {
        if *peaks == self.held
            || (!stopping && now.saturating_duration_since(self.at) < PEAKS_EVERY)
        {
            return;
        }
        let (Some(path), Some(text)) = (self.path.as_deref(), peaks_json(peaks)) else {
            return;
        };
        self.at = now;
        match aterm_agent::harness::upgrade_models::write_atomic(path, &text) {
            Ok(()) => self.held = peaks.clone(),
            Err(error) => aterm_log::warn!(
                "system vitals: the peaks were not written to {}: {error}",
                path.display()
            ),
        }
    }
}

/// The text of the file at `path` when it holds at most [`PEAKS_MAX_BYTES`];
/// a larger file is not read past that.
fn read_small(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(PEAKS_MAX_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= PEAKS_MAX_BYTES).then_some(text)
}

/// The peaks file's document.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PeaksDoc {
    schema: u32,
    peaks: Vec<PeakDoc>,
}

/// One peak in the file: its name, the seconds it has read, and its days as
/// `[unix day, maximum]`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PeakDoc {
    name: String,
    read_s: u32,
    days: Vec<(u32, f64)>,
}

/// `peaks` as the file's text.
fn peaks_json(peaks: &Peaks) -> Option<String> {
    let doc = PeaksDoc {
        schema: PEAKS_SCHEMA,
        peaks: peaks
            .to_pairs()
            .into_iter()
            .map(|(name, read_s, days)| PeakDoc {
                name: name.to_owned(),
                read_s,
                days,
            })
            .collect(),
    };
    aterm_json::to_string(&doc).ok()
}

/// The peaks the file's `text` holds; `None` for text of another layout or
/// none at all.
fn peaks_of(text: &str) -> Option<Peaks> {
    let doc: PeaksDoc = aterm_json::from_str(text).ok()?;
    (doc.schema == PEAKS_SCHEMA).then(|| {
        Peaks::from_pairs(
            doc.peaks
                .iter()
                .map(|p| (p.name.as_str(), p.read_s, p.days.as_slice())),
        )
    })
}

impl crate::App {
    /// Who would see a reading now: whether a window in view shows the stats
    /// bars (the Deck or the sill) or the Live Fleet System Monitor tab
    /// (`/tabs` / `/monitor`), whether a window draws the bars at all, and
    /// whether aterm is in front ([`Audience::of`]).
    fn vitals_audience(&self) -> Audience {
        if self.proxy.is_none() && self.vitals.tx.is_none() {
            return Audience::None;
        }
        let bars = self.stats_bars_form().shown();
        let drawing = || {
            self.windows
                .values()
                .filter(|ws| crate::stats_deck::stats_drawn_on(ws))
        };
        let in_view = self.deck_in_view()
            || (self.tabs_page_shown() && self.windows.values().any(|ws| !ws.occluded));
        let focused = self.windows.values().any(|ws| ws.focused);
        Audience::of(in_view, bars && drawing().next().is_some(), focused)
    }

    /// Whether a window in view shows the bars, the Deck or the Sill: the
    /// Fleet Monitor tab alone shows none ([`Request::deck`]).
    fn deck_in_view(&self) -> bool {
        self.stats_bars_form().shown()
            && self
                .windows
                .values()
                .any(|ws| crate::stats_deck::stats_drawn_on(ws) && !ws.occluded)
    }

    /// The deadline the loop folds for the next reading: the bars' cadence while a window in view shows the Deck or Fleet Monitor tab.
    pub(crate) fn vitals_deadline(&self) -> Option<Instant> {
        match (self.vitals.next_due, self.vitals.edge_at(Instant::now())) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// A level judged every wake.
    pub(crate) fn service_vitals(&mut self, now: Instant) {
        self.vitals.take_proc_edge();
        self.serve_vitals(now);
        self.vitals.arm_proc_edge();
    }

    /// [`Self::service_vitals`]'s judgement.
    fn serve_vitals(&mut self, now: Instant) {
        if let Some(proxy) = self.proxy.as_ref() {
            self.vitals.ensure_thread(proxy);
        }
        let audience = self.vitals_audience();
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
        if !std::mem::replace(&mut self.vitals.windowed, true) {
            self.vitals.next_due = None;
            self.vitals.asked_at = None;
        }
        self.vitals.disk_pull = false;
        self.vitals.edge = false;
        let due = due_at(self.vitals.asked_at, self.vitals.next_due, period, now);
        if due > now {
            self.vitals.next_due = Some(due);
            self.send_layout(now, period, machine_only);
            return;
        }
        self.vitals.asked_at = Some(now);
        self.vitals.next_due = Some(now + period);
        self.ask_reading(now, period, machine_only, false);
        if self.tabs_page_shown() && self.harness_screen.fleet_sweep_due(now) {
            self.start_harness_stats_refresh_at(now);
        }
    }

    /// Ask the thread for a reading at `period` (`machine_only` while
    /// hidden) with the layout now, the stuck engine made to look at it when
    /// `jobs_now` ([`Request::jobs_now`]).
    fn ask_reading(&mut self, now: Instant, period: Duration, machine_only: bool, jobs_now: bool) {
        let mut req = self.vitals_request(period, machine_only);
        req.jobs_now = jobs_now;
        let layout = self.layout_fingerprint();
        self.vitals.layout_looked = Some(now);
        if self.vitals.offer(Msg::Read(Box::new(req))) {
            self.vitals.sent_layout = Some(layout);
        }
    }

    /// Send the thread the layout alone when it changed since it was last
    /// sent (for `sys`); no reading.
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

    /// NO WINDOW IN VIEW SHOWS THE BARS (module docs, the hung-call design's
    /// D1): ask for one whole reading when the disk watch looked or the held
    /// view's engine demand is due.
    fn serve_windowless_vitals(&mut self, audience: Audience, now: Instant, delegated: bool) {
        let was_delegated = std::mem::replace(&mut self.vitals.delegated_active, delegated);
        let was_windowed = std::mem::replace(&mut self.vitals.windowed, false);
        if was_windowed {
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
            // The engine reads the sweep: a whole reading. Beside a hidden
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
        } else if let Some((period, machine_only)) = hidden
            && due_at(self.vitals.asked_at, None, period, now) <= now
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

    /// OUTPUT FROM `session` (the `Wake::Output` arm, at its clock `now`, after the pass cleared the session's output latch: a chunk fed after the screen is read here posts a wake of its own).
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

    /// What a write of `session` is to the stuck engine at `now` (design §6.1, "What brings job demand back").
    pub(crate) fn vitals_output_kind(&mut self, session: u64, now: Instant) -> OutputEdge {
        if self.vitals.windowed || self.vitals.tx.is_none() {
            return OutputEdge::Edge;
        }
        let Some(s) = self.pool.get(session) else {
            return OutputEdge::Edge;
        };
        let moved = match crate::term_try_lock(&s.term).map(|t| crate::control::screen_gen(&t)) {
            None => true,
            Some(screen) => match self.vitals.screens.insert(session, screen) {
                Some(before) => before != screen,
                None => {
                    let pool = &self.pool;
                    self.vitals
                        .screens
                        .retain(|key, _| pool.get(*key).is_some());
                    true
                }
            },
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

    /// THE DISK WATCH LOOKED (`HostNote::DiskLooked`, once a `harness_host::DISK_TICK`).
    pub(crate) fn vitals_disk_looked(&mut self, now: Instant) {
        if self.vitals_audience().in_view() {
            return;
        }
        self.vitals.disk_pull = true;
        self.service_vitals(now);
    }

    /// Record each job event the thread handed over on its session's
    /// timeline: `EVENT <local> job <verdict> job=<id> class=<class> rev=<n>`
    /// on `subscribe … events` (the timeline's `job-change` row), with
    /// `task=<id>` for an agent's call and `code=<n>` once it exited.
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
            let Some(timeline) = self.pool.get(tab).map(|p| Arc::clone(&p.ctx.timeline)) else {
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

    /// `Wake::Vitals`: the thread left a snapshot (a reading the loop asked, or a pull's whose job verdicts moved).
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

    /// Adopt a snapshot the thread left, at `now`, record the job events it
    /// raised, and post its jobs view to the board; with no window in view
    /// showing the bars (`audience`), name the next reading from that view
    /// ([`VitalsHost::windowless_due`]).
    fn adopt_vitals(&mut self, audience: Audience, now: Instant, delegated: bool) -> bool {
        let adopted = self.vitals.adopt(now);
        self.record_job_events();
        if !adopted {
            return false;
        }
        self.post_jobs_reading();
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

    /// A new reading for whatever shows one: every Deck asked for a frame,
    /// the strips' meters, the Fleet Monitor tab.
    fn show_vitals_reading(&mut self) {
        self.redraw_stats_decks();
        self.push_chip_loads();
        if self.tabs_page_shown() {
            self.publish_native_harness_state();
        }
    }

    /// Post the adopted snapshot's jobs view to the harness workers' board
    /// (`VitalsHost`'s: [`crate::vitals_jobs::BOARD`] live), each row's tab
    /// (the pool session's local id, [`Self::vitals_request`]'s
    /// `TabRoot::key`) mapped to its session id under one read of the store.
    fn post_jobs_reading(&self) {
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
        self.vitals.board.post(view, sids, Instant::now());
    }

    /// The Disk lens opened (v2 §7.4).
    pub(crate) fn disk_lens_opened(&self) {
        let mut ask = crate::disk_where::Ask {
            home: aterm_types::dirs::home_dir(),
            ..crate::disk_where::Ask::default()
        };
        for ws in self.windows.values() {
            for view in ws.tab_set.tabs().iter().flat_map(|tab| tab.root.leaves()) {
                let Some(crate::tab_model::View::Terminal(view)) =
                    self.view_store.get(view).copied()
                else {
                    continue;
                };
                let Some(s) = self.pool.get(view.session) else {
                    continue;
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
            }
        }
        crate::disk_where::start(
            &self.vitals.survey,
            ask,
            Instant::now(),
            aterm_time::unix_millis().and_then(|ms| i64::try_from(ms).ok()),
        );
    }

    /// One tab's load step for its chip's meter (design §12.4).
    pub(crate) fn chip_load(&self, tab_count: usize, focus: crate::tab_model::ViewId) -> u8 {
        if tab_count < 2 || !self.stats_bars_form().shown() {
            return 0;
        }
        let Some(snap) = self.vitals.snapshot.as_deref() else {
            return 0;
        };
        let capacity = snap.total_cores().unwrap_or(0) as f32;
        self.view_store
            .get(focus)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
            .and_then(|session| snap.tab(session))
            .map_or(0, |tab| {
                crate::tab_bar::chip_load_step(tab.scope.cores.level, capacity)
            })
    }

    /// Hand each native strip its chips' load steps and the meters' inks after a reading.
    pub(crate) fn push_chip_loads(&self) {
        if self._toolbars.is_empty() {
            return;
        }
        let band = crate::chrome_band::band_colors(
            self.chrome_palette(crate::app_render::ChromeSurface::Header),
        );
        let inks = crate::stats_deck::cached_inks(band.bar_bg, band.value, band.accent);
        for (wid, handle) in &self._toolbars {
            let Some(ws) = self.windows.get(wid) else {
                continue;
            };
            let n = ws.tab_set.len();
            let loads: Vec<u8> = ws
                .tab_set
                .tabs()
                .iter()
                .map(|tab| self.chip_load(n, tab.focus))
                .collect();
            crate::toolbar::set_chip_loads(handle, &loads, [inks.tab, inks.aterm]);
        }
    }

    /// Ask each window showing the Deck for a frame; the Deck's fingerprint
    /// decides whether one presents.
    fn redraw_stats_decks(&self) {
        for ws in self.windows.values() {
            if let Some(window) = ws.os_window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Every terminal pane the windows hold, in window then tab order:
    /// `(window id, tab index, the pane's session, its shell's pid, whether
    /// it is the front tab's focused pane)`.
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

    /// The front window: the frontmost one, which stays put while aterm is in
    /// the background, else the first.
    fn front_window(&self) -> Option<crate::WindowId> {
        self.frontmost_window
            .filter(|wid| self.windows.contains_key(wid))
            .or_else(|| self.windows.keys().next().copied())
    }

    /// A fingerprint of [`Self::for_each_pane`]'s rows: the layout the thread
    /// holds changes exactly when this does.
    fn layout_fingerprint(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.for_each_pane(|window, tab, session, pid, front| {
            (window, tab, session, pid, front).hash(&mut h);
        });
        h.finish()
    }

    /// The thread's request at `period` (`machine_only` while hidden): every
    /// pane's shell and the program in front of it, each its own root (a
    /// split pane's processes count in that pane) carrying its tab's key, and
    /// where each sits (for `sys`); each
    /// agent session's token totals from the Claude Code footer of every pane
    /// of every tab (v2 §8.1: aterm's rate is over every agent session, split
    /// or not) keyed to the tab that holds it, and the front tab's plan usage
    /// (its focused pane's, else its first pane's that has one), all copied
    /// under each pane's timeline lock.
    fn vitals_request(&self, period: Duration, machine_only: bool) -> Request {
        let mut tabs = Vec::new();
        let mut layout = Vec::new();
        let mut tokens = Vec::new();
        let mut accounts: Vec<AccountKey> = Vec::new();
        let mut front_plan = None;
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
                let panes = std::iter::once(tab.focus)
                    .chain(tab.root.leaves().into_iter().filter(|v| *v != tab.focus));
                let first = tabs.len();
                for pane in panes.filter_map(terminal) {
                    let Some(s) = self.pool.get(pane) else {
                        continue;
                    };
                    let tl = s
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    if let Ok(shell_pid) = u32::try_from(s.pid)
                        && shell_pid > 0
                    {
                        tabs.push(TabRoot {
                            key: pane,
                            tab: pane,
                            shell_pid,
                            label: format!("Tab {}", i + 1),
                            program: tl.agent().program.clone().unwrap_or_default(),
                        });
                        layout.push(PaneFact {
                            key: pane,
                            window: wid.0,
                            tab: i + 1,
                        });
                    }
                    let facts = tl.claude_footer();
                    let key = focused.unwrap_or(pane);
                    tokens
                        .extend(facts.and_then(|f| tab_tokens(key, f, tl.claude_footer_read_at())));
                    if let Some(p) = facts.and_then(|f| f.plan) {
                        if !accounts.contains(&p.account) {
                            accounts.push(p.account);
                        }
                        if in_front && front_plan.is_none() {
                            front_plan = Some(p);
                        }
                    }
                }
                // The tab's panes go by its least pane's key.
                let roots = &mut tabs[first..];
                if let Some(least) = roots.iter().map(|t| t.key).min() {
                    for root in roots {
                        root.tab = least;
                    }
                }
            }
        }
        let store = self.store.read().unwrap_or_else(|p| p.into_inner());
        let mut session_ids = Vec::new();
        for handle in store.live_handles() {
            let Some(session) = self.pool.get(handle.local_id) else {
                continue;
            };
            let Ok(pid) = u32::try_from(session.pid) else {
                continue;
            };
            if pid == 0 {
                continue;
            }
            if !tabs.iter().any(|tab| tab.key == handle.local_id) {
                tabs.push(TabRoot {
                    key: handle.local_id,
                    tab: handle.local_id,
                    shell_pid: pid,
                    label: format!("Session {}", handle.local_id),
                    program: String::new(),
                });
            }
            session_ids.push((handle.local_id, handle.sid.as_str().to_string()));
        }
        drop(store);
        let mut seen = std::collections::HashSet::new();
        tabs.retain(|tab| seen.insert(tab.key));
        let ttys = tabs
            .iter()
            .filter_map(|t| {
                let s = self.pool.get(t.key)?;
                TtyWord::read(t.key, &s.ctx.sink, &s.latest_output_activity_ns)
            })
            .collect();
        let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
        Request {
            self_pid: std::process::id(),
            tabs,
            ttys,
            layout,
            session_ids,
            cadence_ms: duration_ms(period),
            machine_only,
            jobs_now: false,
            pull: false,
            deck: self.deck_in_view(),
            tokens: each_session_once(tokens),
            plan: front_plan.and_then(|p| plan_fact(&p, &accounts, now)),
        }
    }

    /// This window's front tab: its session id and label (`Tab 2`).
    pub(crate) fn deck_tab(&self, wid: crate::WindowId) -> (Option<u64>, String) {
        let Some(ws) = self.windows.get(&wid) else {
            return (None, "Tab".into());
        };
        let Some(i) = ws.tab_set.active_index() else {
            return (None, "Tab".into());
        };
        let label = format!("Tab {}", i + 1);
        let key =
            ws.tab_set
                .tab_at(i)
                .and_then(|tab| match self.view_store.get(tab.focus).copied() {
                    Some(crate::tab_model::View::Terminal(view)) => Some(view.session),
                    _ => None,
                });
        (key, label)
    }
}

#[cfg(test)]
#[path = "vitals_host_tests.rs"]
mod tests;

/// The split set's host side: which readings are split ticks, the cold
/// pair, and what is read once and kept.
#[cfg(test)]
#[path = "vitals_host_split_tests.rs"]
mod split_tests;
