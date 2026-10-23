// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one probe thread that reads the machine for the stats Deck, and the loop's side of it.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
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
/// The machine counters every ten seconds while every window that draws the bars is hidden.
pub(crate) const HIDDEN_PERIOD: Duration = Duration::from_secs(10);
const PEAKS_EVERY: Duration = Duration::from_secs(600);
const PEAKS_FILE: &str = "vitals-peaks.json";
const PEAKS_SCHEMA: u32 = 1;
const PEAKS_MAX_BYTES: u64 = 16 * 1024;
/// How long the quit waits for the probe thread to end and write the peaks.
pub(crate) const STOP_BOUND: Duration = Duration::from_millis(500);

/// A pull whose thread's last reading is older than this reads twice.
pub(crate) const COLD_AFTER: Duration = Duration::from_secs(3);
/// Spacing between a cold pull's two readings.
pub(crate) const COLD_PAIR: Duration = Duration::from_millis(500);

/// An edge's readings with no window showing the bars come at most this often.
pub(crate) const JOBS_FLOOR: Duration = Duration::from_secs(2);
/// The shortest rate window a pull measures.
pub(crate) const MIN_WINDOW: Duration = Duration::from_millis(250);
/// Minimum interval between pane-layout checks between readings.
pub(crate) const LAYOUT_EVERY: Duration = Duration::from_millis(250);
const JOB_EVENTS_CAP: usize = 256;
/// How often, at most, the bars' reading reads the split set.
pub(crate) const SPLIT_EVERY: Duration = Duration::from_secs(5);
/// How often, at most, the bars' reading reads the split set while a lens is open.
pub(crate) const SPLIT_EVERY_LENS: Duration = Duration::from_secs(1);
/// The longest window a split reads.
pub(crate) const SPLIT_WINDOW_MAX: Duration =
    Duration::from_nanos(aterm_vitals::explain::WINDOW_MAX_NS);
/// Safety margin inside [`SPLIT_WINDOW_MAX`] before an armed pull reads twice.
pub(crate) const SPLIT_COLD_MARGIN: Duration = Duration::from_secs(1);
const WHY_FIXTURE_SEAM: &str = "ATERM_DEV_WHY_FIXTURE";

/// Pulls that arm the split set (`sys why`).
pub(crate) static SPLIT_ARMED: Arming = Arming::new();

fn job_demand_after(view: &aterm_vitals::jobs::JobsView) -> Option<Duration> {
    let live = view
        .rows
        .iter()
        .any(|r| r.verdict != aterm_vitals::jobs::Verdict::Exited);
    let every = view.demand.every_ms().filter(|_| live)?;
    Some(Duration::from_millis(every).max(JOBS_FLOOR))
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

/// Newest snapshot and job events handed from the probe thread to the loop.
#[derive(Default)]
pub(crate) struct Handover {
    snap: Option<Arc<Snapshot>>,
    words: Vec<WordSeen>,
    job_events: Vec<aterm_vitals::events::Event>,
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
    pub(crate) deck: bool,
    /// Every agent session's token totals, each session once.
    pub(crate) tokens: Vec<TokenRow>,
    /// The front tab's plan usage, while current.
    pub(crate) plan: Option<PlanFact>,
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
    /// Reads the terminal mode word of session `key` behind `sink`.
    pub(crate) fn read(
        key: u64,
        sink: &aterm_session::sink::SinkWriter,
        output: &Arc<ScreenMoves>,
    ) -> Option<Self> {
        Self::read_seamed(key, sink, output, || {})
    }

    fn read_seamed(
        key: u64,
        sink: &aterm_session::sink::SinkWriter,
        output: &Arc<ScreenMoves>,
        between: impl FnOnce(),
    ) -> Option<Self> {
        let output_seen = output.count();
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

    /// Returns the snapshot of this word kept by the loop once a reading lands.
    pub(crate) const fn seen(&self) -> WordSeen {
        WordSeen {
            key: self.key,
            canonical: self.canonical,
            echo: self.echo,
            input: self.input_seen,
        }
    }

    /// Returns the engine's word for the tick at `at_ms`, if output and input are unchanged.
    pub(crate) fn current(&self, at_ms: u64) -> Option<(u64, aterm_vitals::jobs::TtyMode)> {
        (self.output.count() == self.output_seen && self.input.get() == self.input_seen).then_some(
            (
                self.key,
                aterm_vitals::jobs::TtyMode {
                    canonical: self.canonical,
                    echo: self.echo,
                    stamped_ms: at_ms,
                },
            ),
        )
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
    /// When a reading last looked at the session: the view that parked it, or its own write's [`OutputEdge::Recheck`].
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

/// HOW MANY OF THE PTY READER'S FEEDS MOVED THE SCREEN (design §6.1, "What brings job demand
/// back"): counted by the reader under its own hold of the term lock, after each feed, where the
/// screen's generation ([`crate::control::screen_gen`]) is not the one it saw after its last feed.
/// The output wake reads it WITHOUT the term lock ([`crate::App::vitals_output_kind`]), and a
/// reading's terminal mode word holds while it stands ([`TtyWord`]). The wake used to read the
/// generation itself and, meeting the lock held (a control worker's `text` or `status`, a watcher
/// the same burst rang), counted the write as moved; the word used to fall to any read. Either way
/// Muse Code's colour reset at rest, which moves no cell, armed `system_vitals`
/// (`docs/measured/vitals-3-2026-10-22.md`, V1).
#[derive(Debug, Default)]
pub(crate) struct ScreenMoves {
    /// The generation the reader saw after its last feed; only the feeding side takes it.
    seen: Mutex<Option<crate::control::ScreenGen>>,
    moves: AtomicU64,
}

impl ScreenMoves {
    /// A feed left the screen at `now` (the reader, holding the term lock).
    pub(crate) fn fed(&self, now: crate::control::ScreenGen) {
        let moved = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(now)
            != Some(now);
        if moved {
            self.moves.fetch_add(1, Ordering::Release);
            // THE WAKE PROTOCOL'S STORE-BUFFER PAIR, with [`Self::count_after_clear`]: the reader
            // counts this move, then reads the session's wake latch to post or coalesce its wake;
            // the output wake clears that latch, then reads this count. Without a fence on each
            // side both reads may take the value from before the other's write (the store-buffer
            // outcome, which Release and Acquire allow): the reader coalesces into a wake whose
            // pass reads the old count, and the move is no edge until the next one. The term lock
            // the wake took to read the screen used to order the two.
            fence(Ordering::SeqCst);
        }
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
}

/// The loop's side of the probe thread.
pub(crate) struct VitalsHost {
    tx: Option<SyncSender<Msg>>,
    sent_layout: Option<u64>,
    layout_looked: Option<Instant>,
    sys: Option<&'static crate::sys_host::Shared>,
    stopping: Arc<AtomicBool>,
    thread: Option<(std::thread::JoinHandle<()>, Receiver<()>)>,
    tried: bool,
    latest: Arc<Mutex<Handover>>,
    /// The snapshot the Deck draws.
    pub(crate) snapshot: Option<Arc<Snapshot>>,
    /// When the held snapshot's jobs view was adopted: where a reading no window shows counts its next from ([`Self::windowless_due`]).
    pub(crate) adopted_at: Option<Instant>,
    full_adopted_at: Option<Instant>,
    /// When the next reading is due: the bars' cadence while a window in view shows them.
    pub(crate) next_due: Option<Instant>,
    asked_at: Option<Instant>,
    jobs_due: Option<Instant>,
    delegated_due: Option<Instant>,
    windowed: bool,
    disk_pull: bool,
    edge: bool,
    last_edge_read: Option<Instant>,
    /// Each session's [`ScreenMoves::count`] at its last output wake.
    screens: std::collections::HashMap<u64, u64>,
    parked: std::collections::HashMap<u64, Parked>,
    job_events: Vec<aterm_vitals::events::Event>,
    job_revs: std::collections::HashMap<u64, u64>,
    delegated_active: bool,
    chips_shown: bool,
    survey: Arc<Where>,
    board: &'static crate::vitals_jobs::JobsBoard,
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
    /// Whether a "where" survey is running or has completed.
    #[cfg(all(test, unix))]
    pub(crate) fn survey_asked(&self) -> bool {
        let (last, running) = self.survey.read();
        running || last.is_some()
    }

    /// Adopts the thread's newest snapshot if newer than the held one, along with its job events.
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

    fn windowless_due(&self) -> Option<Instant> {
        let view = self.snapshot.as_ref()?.jobs.as_deref()?;
        let after =
            crate::vitals_jobs::windowless_after(view).or_else(|| job_demand_after(view))?;
        Some(self.adopted_at? + after)
    }

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

    /// Marks an output or process-event edge when no window shows the bars.
    pub(crate) const fn note_edge(&mut self) {
        if !self.windowed && self.tx.is_some() {
            self.edge = true;
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

    /// Stops the probe thread on application exit and waits up to `bound` for peak persistence.
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

trait Source {
    fn reading(&mut self, req: &Request) -> Snapshot;
    fn model(&mut self) -> &mut aterm_vitals::Vitals;
    fn machine(&self) -> Option<&Machine> {
        None
    }
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

fn pull(source: &mut impl Source, req: &Request, last_whole: Option<Instant>) -> Snapshot {
    if let Some(t) = last_whole {
        std::thread::sleep(MIN_WINDOW.saturating_sub(t.elapsed()));
    }
    let cold = last_whole.is_none_or(|t| t.elapsed() > COLD_AFTER) || source.split_cold();
    let mut pull = req.clone();
    pull.machine_only = false;
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

fn duration_ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

fn keep_warm_events(warm: Snapshot, snap: &mut Snapshot) {
    let mut events = warm.events;
    events.append(&mut snap.events);
    snap.events = events;
}

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

/// The thermal level and Low Power Mode probe.
pub(crate) type ThermalProbe = fn() -> Option<Thermal>;

fn platform_thermal() -> Option<Thermal> {
    #[cfg(target_os = "macos")]
    {
        use aterm_objc::{class, sel};
        // SAFETY: `+processInfo` is an immortal singleton; both getters are
        // plain scalar properties, documented thread-safe (as `spare.rs`'s
        // `power_constrained` reads them).
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
    /// A process's pid version ([`pid_version`]), which an exec moves: what
    /// a lead's identity is kept under between split ticks.
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
        version: pid_version,
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
        version: |_| None,
        timeline: || None,
        waketime_ms: || None,
    };
}

/// `pid`'s pid version (`p_idversion`, `PROC_PIDUNIQIDENTIFIERINFO`, which
/// any process may read of any other): an exec moves it while the pid, the
/// birth and often the short name stay. `None` off macOS.
fn pid_version(pid: u32) -> Option<i32> {
    #[cfg(target_vendor = "apple")]
    {
        aterm_uds::process::uniq_identifier(i32::try_from(pid).ok()?).map(|i| i.p_idversion)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = pid;
        None
    }
}

/// Threshold above which a split tick reads the whole process table for births.
pub(crate) const BIRTHS_AT_ONCE: usize = 64;

/// Each process's kernel `p_comm` and start (unix ms) by pid.
pub(crate) type Births = HashMap<u32, (String, i64)>;

fn name_and_birth(r: aterm_sysprobe::kproc::Record) -> Option<(String, i64)> {
    let ms = i64::try_from(r.start_s)
        .ok()?
        .checked_mul(1000)?
        .checked_add(i64::from(r.start_us / 1000))?;
    Some((r.comm, ms))
}

fn names_the_same(record: &str, swept: &str) -> bool {
    const SHORT: usize = 15;
    record == swept
        || record
            .as_bytes()
            .get(..SHORT)
            .is_some_and(|cut| String::from_utf8_lossy(cut) == swept)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Birth {
    name: String,
    start: Option<u64>,
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
    vouches: bool,
    armed: &'static Arming,
    last: Option<Instant>,
    births: HashMap<u32, Birth>,
    /// Each lead's identity by pid, kept while it leads and its birth and
    /// pid version stay ([`Self::identity`]).
    idents: HashMap<u32, Ident>,
    wake_seen: Option<Option<i64>>,
    timeline: Option<TimelineSample>,
}

impl SplitReader {
    /// Constructs a split reader and runs the launch self-check on this process.
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
            idents: HashMap::new(),
            wake_seen: None,
            timeline: None,
        }
    }

    fn due(&self, req: &Request, now: Instant) -> bool {
        self.lane.on() && split_due(self.last, req, self.armed.armed(), now)
    }

    fn cold(&self, now: Instant) -> bool {
        self.lane.on()
            && self.armed.armed()
            && self.last.is_none_or(|t| {
                now.saturating_duration_since(t) + SPLIT_COLD_MARGIN > SPLIT_WINDOW_MAX
            })
    }

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
            self.births.clear();
            self.idents.clear();
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
        let mut kept = HashMap::new();
        for (_, pid) in aterm_vitals::explain::leads(&members) {
            let Some(&birth) = born.get(&pid) else {
                continue;
            };
            let id = self.identity(pid, birth, &mut kept);
            sample
                .leads
                .push(lead_identity(pid, birth, id, self.vouches));
        }
        self.idents = kept;
        sample.coalitions = Some(coal_sample(read));
        sample.timeline = self.timeline();
        self.last = Some(now);
    }

    /// Lead `pid`'s identity (born at `birth`), kept into `kept`: the one
    /// read under its pid version now, read again only once an exec moved
    /// that version or the birth moved. A read an exec raced (the version
    /// moved across it) and a read that missed are unknown and kept for no
    /// later tick; with no version read, it is read at every split tick.
    fn identity(
        &mut self,
        pid: u32,
        birth: i64,
        kept: &mut HashMap<u32, Ident>,
    ) -> aterm_sysprobe::identity::Identity {
        let version = (self.probe.version)(pid);
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

    fn known(&self, p: &ProcSample) -> Option<i64> {
        let start = p.start?;
        self.births
            .get(&p.pid)
            .filter(|b| b.name == p.name && b.start == Some(start))
            .map(|b| b.ms)
    }

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

/// A recorded `explain::fixtures` scene read in place of the machine in debug builds.
#[cfg(debug_assertions)]
pub(crate) struct WhyFixture {
    scene: aterm_vitals::explain::fixtures::Scene,
    t0: Option<Instant>,
}

/// No fixture in a build without debug assertions.
#[cfg(not(debug_assertions))]
pub(crate) enum WhyFixture {}

#[cfg(debug_assertions)]
impl WhyFixture {
    fn from_seam() -> Option<Self> {
        let named = aterm_types::dev_seam!(WHY_FIXTURE_SEAM)?;
        let calm = aterm_vitals::explain::fixtures::calm_1510();
        let scene = match named.to_str()? {
            "spotlight" => aterm_vitals::explain::fixtures::Scene {
                mem: calm.mem,
                thermal: calm.thermal,
                ..aterm_vitals::explain::fixtures::spotlight_1032()
            },
            "calm" => calm,
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

    fn reading(&mut self, live: &Sample, split: bool) -> Sample {
        let t0 = *self.t0.get_or_insert_with(Instant::now);
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
        let mut scene = self.scene.reading(at_ms, split);
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
    jobs: crate::vitals_jobs::JobsPass,
    epoch: Instant,
    home: PathBuf,
    scratch: Vec<u8>,
    uid: Option<u32>,
    survey: Arc<Where>,
    job_watch: aterm_vitals::events::JobWatch,
    watched: Option<Arc<aterm_vitals::jobs::JobsView>>,
    thermal: ThermalProbe,
    counter_throttle: Option<(Instant, Option<Thermal>)>,
    slots: aterm_sysprobe::slots::SlotReader,
    /// The split set's lane and what it keeps between split ticks.
    pub(crate) split: SplitReader,
    fixture: Option<WhyFixture>,
    /// The machine's words, read once.
    pub(crate) machine: Machine,
}

/// Steps the stuck engine over one whole reading at `at_ms`.
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

    fn read_scene(&mut self, fixture: Option<WhyFixture>) {
        if fixture.is_some() {
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

    fn watch_jobs(&mut self, snap: &mut Snapshot) {
        if let Some(view) = snap.jobs.as_ref()
            && !self.watched.as_ref().is_some_and(|w| Arc::ptr_eq(w, view))
        {
            let events = self.job_watch.step(view, snap.seq, snap.at_ns);
            snap.events.extend(events);
            self.watched = Some(Arc::clone(view));
        }
    }

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
        let now = Instant::now();
        let split = self.split.due(req, now);
        let live_split = split && self.fixture.is_none();
        let sweep = (self.split.probe.sweep)(aterm_sysprobe::SweepOpts {
            coalitions: live_split,
        });
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
        let scene = self.fixture.as_mut().map(|fixture| {
            if split {
                self.split.last = Some(now);
            }
            fixture.reading(&sample, split)
        });
        let mut snap = self.vitals.absorb(scene.as_ref().unwrap_or(&sample));
        if self.split.lane.on() && snap.coal_lane == CoalLane::Off && scene.is_none() {
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

    /// Takes a hidden deck's reading (machine counters alone, no process sweep).
    pub(crate) fn read_machine(&mut self, req: &Request) -> Snapshot {
        let mut sample = self.machine_sample(req);
        sample.machine_only = true;
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
            coalitions: None,
            leads: Vec::new(),
            timeline: None,
            coal_lane: self.split.lane,
        }
    }
}

fn mono_ms_of(at: Instant, epoch: Instant) -> i64 {
    match at.checked_duration_since(epoch) {
        Some(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        None => i64::try_from(epoch.duration_since(at).as_millis()).map_or(i64::MIN, |ms| -ms),
    }
}

/// Converts a swept [`aterm_sysprobe::ProcessReading`] into a [`ProcSample`].
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
        birth_ms: None,
    }
}

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

const fn proc_limits(l: aterm_sysprobe::ProcessLimits) -> ProcLimits {
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

#[cfg(not(windows))]
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

struct PeakFile {
    path: Option<PathBuf>,
    held: Peaks,
    at: Instant,
}

impl PeakFile {
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

fn read_small(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(PEAKS_MAX_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= PEAKS_MAX_BYTES).then_some(text)
}

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
            .map(|(name, read_s, days)| PeakDoc {
                name: name.to_owned(),
                read_s,
                days,
            })
            .collect(),
    };
    aterm_json::to_string(&doc).ok()
}

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

    fn deck_in_view(&self) -> bool {
        self.stats_bars_form().shown()
            && self
                .windows
                .values()
                .any(|ws| crate::stats_deck::stats_drawn_on(ws) && !ws.occluded)
    }

    /// The deadline the loop folds for the next reading.
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

    fn ask_reading(&mut self, now: Instant, period: Duration, machine_only: bool, jobs_now: bool) {
        let mut req = self.vitals_request(period, machine_only);
        req.jobs_now = jobs_now;
        let layout = self.layout_fingerprint();
        self.vitals.layout_looked = Some(now);
        if self.vitals.offer(Msg::Read(Box::new(req))) {
            self.vitals.sent_layout = Some(layout);
        }
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

    fn show_vitals_reading(&mut self) {
        self.redraw_stats_decks();
        self.push_chip_loads();
        if self.tabs_page_shown() {
            self.publish_native_harness_state();
        }
    }

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

    /// Starts a "where" survey when the Disk lens opens.
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

    /// One tab's load step for its chip's meter.
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

    /// Hands each native strip its chips' load steps and the meters' inks after a reading.
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

    fn redraw_stats_decks(&self) {
        for ws in self.windows.values() {
            if let Some(window) = ws.os_window.as_ref() {
                window.request_redraw();
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
                let first = tabs.len();
                for pane in self.tab_sessions(tab) {
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
                TtyWord::read(t.key, &s.ctx.sink, &s.ctx.screen_moves)
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

    /// The sessions of this window's front tab ([`Self::tab_sessions`]): the
    /// tab Why now takes as "this tab", as `sys why` takes its caller's
    /// (a tab split into panes is one, [`aterm_vitals::model::TabRoot::tab`]).
    pub(crate) fn deck_tab_panes(&self, wid: crate::WindowId) -> Vec<u64> {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index().and_then(|i| ws.tab_set.tab_at(i)))
            .map_or_else(Vec::new, |tab| self.tab_sessions(tab))
    }

    /// The terminal sessions of `tab`, its focused pane first, then the rest
    /// in layout order: the panes the vitals request sends as one tab.
    fn tab_sessions(&self, tab: &crate::tab_model::Tab) -> Vec<u64> {
        std::iter::once(tab.focus)
            .chain(tab.root.leaves().into_iter().filter(|v| *v != tab.focus))
            .filter_map(|view| match self.view_store.get(view).copied() {
                Some(crate::tab_model::View::Terminal(view)) => Some(view.session),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "vitals_host_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "vitals_host_split_tests.rs"]
mod split_tests;
