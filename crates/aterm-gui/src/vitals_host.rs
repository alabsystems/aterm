// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE VITALS HOST: the one probe thread that reads the machine for the stats
//! Deck (`crate::stats_deck`), and the loop's side of it.
//!
//! The thread owns the platform readers (`aterm_sysprobe`) and the model
//! (`aterm_vitals::Vitals`). The loop sends it a [`Request`] when a reading is
//! due ([`App::service_vitals`]): this process's pid, each tab's shell, the
//! cadence in force, and what only the loop holds: each tab's Claude Code
//! token totals and the front tab's plan usage, copied from the footers' facts
//! (`docs/DESIGN-stats-deck-v2-2026-10-20.md` §8.3, §8.4). The thread reads,
//! folds the reading into the trends, and leaves the new [`Snapshot`] in a
//! shared slot before posting `Wake::Vitals`. The loop takes the slot on that
//! wake AND on every due tick, because a wake can be dropped (an update
//! handoff's gate) and the Deck must not stall behind it.
//!
//! Cadence (v2 §5.3): two readings a second while a lens is open, one a second
//! while a focused window shows the bars (the Deck or the sill), one every five
//! seconds while aterm is in the background, and one every ten seconds while a
//! window draws the bars but every such window is hidden or occluded
//! ([`Audience::Hidden`]). A hidden reading is the machine counters alone
//! ([`Reader::read_machine`]: no process sweep), so the strips keep filling
//! while the aterm and Tab figures wait for the next full reading. A reading
//! is due one period of the cadence in force after the last one was asked; a
//! window that shows the bars again is read at once. Readings are cheap
//! (design §15.1) but not free, so a deck no one sees schedules none of the
//! in-view cadences; a pending reading may still finish.
//!
//! READINGS WITH NO WINDOW (the hung-call design's D1, leftover §9): while no
//! window in view shows the bars, the jobs engine still has to read, or the
//! jobs board the harness workers' stall canary reads goes stale and a hung
//! agent call is never named (no one had the bars open for 26 h in I4). So the
//! harness's disk watch, at each of its minute looks, tells the window it
//! looked (`HostNote::DiskLooked`), and the window takes ONE whole reading on
//! it ([`App::vitals_disk_looked`]); while the view that reading gave holds a
//! suspect agent call, the next is due at the engine's own cadence
//! ([`crate::vitals_jobs::windowless_after`]: its `Demand`), carried by the
//! loop's deadline fold like any other reading, beside a hidden deck's machine
//! counters. No new timer and no new thread: the reading is this probe
//! thread's, the beat is the disk watch's. A reading no window shows draws
//! nothing: it is posted to the jobs board, and no Deck is asked for a frame
//! and no strip's meters move. A hidden deck's machine reading carries the
//! engine's last view, which the board keeps at its first age. A whole
//! reading's cost is about 1.5 ms of one core (leftover §9).
//!
//! Each reading carries the clocks the gap rule needs (v2 §5.4): the wall
//! clock, the monotonic one (which stops while the Mac sleeps) and the cadence
//! it was asked at. The disk row reads the home volume (v2 §6.4), the disk
//! board's facts for it while a disk watch runs (the board is watched: the
//! harness is on; v2 §7.1, §7.6) and the "where" survey's rows
//! (`crate::disk_where`). Token totals come from every pane of every tab, each
//! with the time its footer's fold read them, so a rate spans the folds, not
//! the readings (v2 §8.3). The network and token peaks live in
//! `<state>/vitals-peaks.json` (v2 §8.2): read when the thread starts, written
//! from it at most every ten minutes while they change, and when it stops.
//! The quit stops it ([`VitalsHost::stop_at_exit`]), since the App is never
//! dropped; a seamless update's handoff, which ends the process without the
//! quit, keeps what was last written.
//!
//! THE AGENTS' SIDE (`aterm ctl sys`, `crate::sys_host`): the loop starts the
//! thread on its first wake whether or not a window shows the Deck (an idle
//! thread blocked on its channel reads nothing), and sends it the pane layout
//! ([`Msg::Facts`]) whenever that changes. A control worker sends
//! [`Msg::Pull`] when its request finds the newest snapshot too old; the
//! thread then takes a whole reading with the layout it holds, twice
//! [`COLD_PAIR`] apart when its last reading is older than [`COLD_AFTER`] (a
//! rate needs an interval, and a minutes-long one says nothing about now).
//! Every snapshot also lands in the workers' copy (`crate::sys_host::SYS`),
//! which the loop never locks. A reading the loop asked posts `Wake::Vitals`;
//! a pull posts it only when a job's verdict moved, so a pull costs the loop
//! no wake. The quit takes the workers' sender back
//! ([`VitalsHost::stop_at_exit`]), or the thread would never hear the hang-up.
//!
//! JOB DEMAND WITH NO WINDOW (design §6.1, kind 3), so `status job=`, the
//! `brief` jobs line and `EVENT … job` speak for now with no Deck on screen:
//! while the held jobs view has a live job and the engine is not idle, the
//! next whole reading is due at the engine's cadence ([`job_demand_after`]:
//! 2 s while one runs or can accrue suspicion, 30 s while every live job sits
//! in a legitimate wait); nothing is due while every live job waits only for
//! the person, which the engine knows from the terminal mode word each
//! request carries ([`TtyWord`]: an idle Claude Code, vim or less at its
//! prompt reads `tty-raw`), and such a parked view keeps speaking for now
//! while the thread runs ([`crate::vitals_jobs::speaks_for_now`]). EDGES (what
//! brings job demand back, [`VitalsHost::note_edge`]): output from a session
//! (a tab just opened prints its prompt, a job that ends lets its shell print
//! one), and a fork, `exec` or exit its process watch heard ([`ProcEdge`]: a
//! command forks after its echo, and a shell can start one silently), each ask
//! one whole reading, at most every [`JOBS_FLOOR`], because a job may have
//! started or ended; the engine looks at every such reading, even one that
//! lands inside its own 2 s ([`Request::jobs_now`]). A job's verdict that
//! moved, in any reading, reaches the loop in the hand-over
//! ([`Handover::job_events`]), which records it on the session's timeline
//! (`subscribe … events` pushes `EVENT <local> job …`, `status job=`, the
//! `brief` jobs line).
//!
//! A pull is warm when the thread's last WHOLE reading is younger than
//! [`COLD_AFTER`] (a hidden Deck's machine counters measure no process rate),
//! and its window is at least [`MIN_WINDOW`] (design §6.4).
//!
//! Explicit delegated jobs require a full sweep every five seconds even
//! without a visible Deck; the engine and delegated clocks stay independent.

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
use aterm_vitals::model::{
    AccountIx, ClusterSpec, CommitSample, DiskFacts, DiskLevel, GpuSample, Hog, LinkKind,
    LinkSample, MemCounters, MemSample, PlanFact, Pressure, ProcLimits, ProcSample, ProcState,
    Sample, Snapshot, TabRoot, TabTokens, Thermal, Throttle, Ticks, VolumeSample,
};
use aterm_vitals::sys::Machine;

use crate::disk_board::{Clock, DiskFactsRaw, Shelf};
use crate::disk_where::Where;
use crate::sys_host::PaneFact;

/// A reading a second while a focused window shows the Deck.
pub(crate) const FOCUSED_PERIOD: Duration = Duration::from_secs(1);
/// One every five seconds while aterm is in the background.
pub(crate) const BACKGROUND_PERIOD: Duration = Duration::from_secs(5);
/// The machine counters every ten seconds while every window that draws the
/// bars is hidden (v2 §5.3).
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
/// …this far apart, so the rates cover an interval that says what is
/// happening now.
pub(crate) const COLD_PAIR: Duration = Duration::from_millis(500);

/// An edge's readings with no window showing the bars come at most this often.
pub(crate) const JOBS_FLOOR: Duration = Duration::from_secs(2);
/// The shortest rate window a pull measures (design §6.4): a pull that comes
/// sooner after the last whole reading waits out the rest.
pub(crate) const MIN_WINDOW: Duration = Duration::from_millis(250);
/// Between readings the loop looks at the pane layout at most this often (a
/// reading carries it whole), so a wake costs no walk of every pane.
pub(crate) const LAYOUT_EVERY: Duration = Duration::from_millis(250);
/// The job events the hand-over keeps for the loop before the oldest go.
const JOB_EVENTS_CAP: usize = 256;

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

/// THE KERNEL EDGE (design §6.1, "What brings job demand back": `NOTE_EXIT`,
/// `NOTE_FORK`): each session's process watch (`aterm_uds::procwatch`, which
/// its PTY gather already polls, `crate::exec_edges`) hears the shell fork and
/// its children `exec` and exit. Output alone misses jobs a parked engine must
/// see: a command typed at a prompt forks after its echo, so the reading the
/// echo asked can sweep before the job exists, and a shell can start one with
/// no output at all (`read -t 3 x; sleep 600`). While no window shows the
/// bars, the probe thread runs and no edge is owed ([`VitalsHost::arm_proc_edge`]),
/// the latch is armed; the first process event disarms it and posts ONE
/// `Wake::Vitals`, which the loop takes as an edge ([`VitalsHost::note_edge`]):
/// its reading comes at once, or [`JOBS_FLOOR`] after the last, and the latch
/// is armed again once that reading is asked. So process events cost at most
/// one wake a reading, none while the bars read or an edge is owed, and none
/// at rest, where nothing forks. No new thread and no new timer. Darwin only
/// (no other OS has an unprivileged fork event), and a session whose watch
/// could not be made (`EMFILE`) has output edges alone.
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

    /// A session's process watch heard a fork, an `exec` or an exit (a PTY
    /// gather): while armed, disarm, mark it heard and post the loop one
    /// wake. True when it did. O(1) and lock-free while disarmed. Built where
    /// its caller, `exec_edges`' gather, is (unix and tests).
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

    /// The loop, each turn: armed exactly while it wants an edge and holds
    /// none heard.
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

/// The process's kernel edge: every session's gather tells it, the App's
/// [`VitalsHost`] arms and takes it (a test App its own).
pub(crate) static PROC_EDGE: ProcEdge = ProcEdge::new();

/// What the thread leaves the loop under one leaf lock: its newest snapshot
/// and the job events raised since the loop last took them.
#[derive(Default)]
pub(crate) struct Handover {
    /// The newest snapshot (taken by [`VitalsHost::adopt`]).
    snap: Option<Arc<Snapshot>>,
    /// Job verdicts that moved, oldest first, at most [`JOB_EVENTS_CAP`].
    job_events: Vec<aterm_vitals::events::Event>,
}

/// Who would see a reading now (v2 §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Audience {
    /// A window in view shows the bars or the Fleet Monitor tab, and one of
    /// aterm's windows is focused.
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

    /// Whether a window in view shows the bars or the Monitor tab: a reading
    /// is drawn, the bars' cadence reads, and the disk watch's look asks
    /// nothing ([`crate::App::vitals_disk_looked`]).
    pub(crate) const fn in_view(self) -> bool {
        matches!(self, Self::Shown | Self::Background)
    }

    /// The reading it asks for: how often, and whether the machine counters
    /// alone. `lens` is whether a lens is open. `None`: nothing is read at a
    /// cadence of the bars (the jobs engine may still read, module docs).
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
/// The first reading is due `now`.
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
    /// Every pane's shell: each terminal session a tab holds is its own root,
    /// so a split pane's processes count in that pane.
    pub(crate) tabs: Vec<TabRoot>,
    /// Those roots' terminal mode words, where read ([`TtyWord`]).
    pub(crate) ttys: Vec<TtyWord>,
    /// Where each of those sessions sits (window, tab), for `sys`.
    pub(crate) layout: Vec<PaneFact>,
    /// Authenticated global session ids for those process-local tab keys.
    pub(crate) session_ids: Vec<(u64, String)>,
    /// The cadence this reading was asked at, ms: the gap rule's (v2 §5.4).
    pub(crate) cadence_ms: u32,
    /// Read the machine counters alone: every window that draws the bars is
    /// hidden (v2 §5.3).
    pub(crate) machine_only: bool,
    /// The stuck engine looks at this reading even inside its own cadence: a
    /// whole reading no window shows asked it (an edge, the disk watch's
    /// look, the engine's own demand), and the loop paces those on its clock
    /// ([`crate::vitals_jobs::JobsPass::tick`]'s `now`). A worker's pull and
    /// the bars' readings keep the engine's cadence.
    pub(crate) jobs_now: bool,
    /// Every agent session's token totals, each session once.
    pub(crate) tokens: Vec<TokenRow>,
    /// The front tab's plan usage, while current.
    pub(crate) plan: Option<PlanFact>,
}

/// One agent session's token totals as the loop copies them from its
/// footer's facts, and when the footer's resolver last read them (each read
/// folds the session anew): the thread puts that on the model's clock
/// ([`TabTokens::read_ms`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TokenRow {
    pub(crate) totals: TabTokens,
    pub(crate) read_at: Option<Instant>,
}

/// A session's TERMINAL MODE WORD (design §6.6), read by the loop for a
/// reading: the slave's `ICANON` and `ECHO` through the master its sink holds
/// (one `tcgetattr`; the probe thread is never handed a master fd, whose
/// number a close can reuse), and the session's output stamp and driven-input
/// count as they stood just before that read. The thread hands the word to the
/// stuck engine only while neither has moved ([`Self::current`]): a word older
/// than the job's last output names no tty wait, so the wait stays unnamed and
/// is never misnamed (§6.6, §11.5); and the tty names need an empty input
/// queue (§11.5), which the read checks (`FIONREAD`: with typed input queued
/// unread there is no word) and which input driven since the read may have
/// filled, so that voids the word too. A key the program leaves unread is the
/// input gate's to judge. Both matter most to a worker's pull, which reads with
/// the request the thread last held, however old.
///
/// The word is how the engine learns that a program waits for the person at
/// its prompt (an idle Claude Code, vim, less: `tty-raw`; a password prompt:
/// `tty-password`), which arms nothing (§6.1, "What arms nothing"). Without
/// it every such program read as an unnamed wait, suspect, and the engine's
/// job demand read it every 2 s with no window for as long as it sat there:
/// the 9 to 10 timer wakes, every one a `system_vitals` fire, that forever
/// land's END counted in its 20 s at rest.
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
    /// The word of session `key`'s terminal behind `sink`, whose output stamp
    /// is `output`: `None` where the master is no terminal (a stub, ConPTY)
    /// or typed input is queued unread.
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
        // The stamps first: output or input landing between them and the
        // terminal's reads moves one past what the word saw, and the word
        // goes unused.
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

    /// The engine's word for the tick at `at_ms`, stamped then: it still
    /// holds there while no output came and no input was driven since it was
    /// read. `None` once either has.
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

/// What the thread is sent. The requests are boxed: a slot of the channel,
/// and the error a full one hands back, stay a pointer wide.
#[derive(Debug)]
pub(crate) enum Msg {
    /// A reading the loop asked (the bars' cadence, the disk watch's look,
    /// the engine's demand, an edge), with the layout now.
    Read(Box<Request>),
    /// The layout changed; no reading (the loop).
    Facts(Box<Request>),
    /// A whole reading now with the layout held (a control worker's `sys`).
    Pull,
}

/// The loop's side of the probe thread.
pub(crate) struct VitalsHost {
    /// The request channel; `None` until the thread starts (on the loop's
    /// first wake) or when it could not start. A test hands its own.
    tx: Option<SyncSender<Msg>>,
    /// The layout last sent to the thread ([`crate::App::layout_fingerprint`]).
    sent_layout: Option<u64>,
    /// When the loop last looked at the layout ([`LAYOUT_EVERY`]).
    layout_looked: Option<Instant>,
    /// The workers' copy the thread handed its sender to: the quit takes it
    /// back ([`Self::stop_at_exit`]). `None` for a test's thread.
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
    /// When the held snapshot's jobs view was adopted: where a reading no
    /// window shows counts its next from ([`Self::windowless_due`]). A reading
    /// that carries the view already held (a hidden deck's machine counters,
    /// or a whole one the engine was not due at) leaves it. `status job=`
    /// says nothing of a view older than the board's freshness.
    pub(crate) adopted_at: Option<Instant>,
    /// Last full-sweep adoption, independent of the jobs engine's held view.
    full_adopted_at: Option<Instant>,
    /// When the next reading is due: the bars' cadence while a window in view
    /// shows them; with none, the engine's own while the held view has a
    /// suspect agent call ([`Self::jobs_due`]), or a hidden deck's machine
    /// counters, whichever is sooner; `None` when nothing is due.
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
    /// Whether a "where" survey was asked for: one runs, or one left its
    /// rows (the Disk lens's behaviour tests, unix only, read it).
    #[cfg(all(test, unix))]
    pub(crate) fn survey_asked(&self) -> bool {
        let (last, running) = self.survey.read();
        running || last.is_some()
    }

    /// Take the thread's newest snapshot if it is newer than the one held,
    /// at `now`, and the job events it raised. True when the Deck has
    /// something new to draw.
    pub(crate) fn adopt(&mut self, now: Instant) -> bool {
        let (fresh, events) = {
            let mut h = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
            (h.snap.take(), std::mem::take(&mut h.job_events))
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

    /// Output from a session, or a process event its watch heard
    /// ([`ProcEdge`]): with no window showing the bars, a job may have
    /// started or ended, so an edge is noted ([`Self::edge_at`] decides
    /// whether it asks); with no thread to read, none. O(1): the loop calls
    /// it on every output wake.
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

    /// Whether the probe thread runs: a parked jobs view speaks for now only
    /// while it does ([`crate::vitals_jobs::speaks_for_now`]).
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
    /// pulls. A test App has no event loop, so none starts; only a reader a
    /// test handed over is asked.
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
        // Room for the Deck's reading and a layout or a pull behind it.
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        // Held by the thread and dropped as it ends, however it ends: the
        // quit's wait hears the hang-up.
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

    /// The quit (the App is never dropped): hang up on the probe thread (the
    /// loop's sender, and the one the workers' copy holds) and wait at most
    /// `bound` for it to end, which writes the peaks that moved since the
    /// last write (v2 §8.2). Whether it ended within the bound; a thread that
    /// never started has nothing to write.
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
                // The thread is gone (it panicked): stop asking.
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
                // The layout and roots; the loop's cadence stays.
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
/// newest layout always. At most two messages wait behind a read; while the
/// queue is full the producers keep the older ones. The peaks are read from
/// their file at `peaks` first and written back when the loop hangs up or is
/// gone (`post` false). With `workers`, it hands that copy its own sender, so
/// control workers can pull, and publishes every snapshot there.
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
        // Reader construction can finish after the quit already uninstalled
        // the workers' sender. Do not leave this late sender holding our own
        // receiver open; queued readings still drain before the final write.
        if stopping.load(Ordering::Acquire) {
            shared.uninstall();
        }
        shared
    });
    let (mut peaks, restored) = PeakFile::open(peaks, Instant::now());
    source.model().restore_peaks(restored);
    let no_machine = Machine::default();
    let mut req = Request::default();
    // The last whole reading: a pull's rates span the time since it.
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
        let cold = last_whole.is_none_or(|t| t.elapsed() > COLD_AFTER);
        let snap = if asked.pull {
            // A worker's reading is whole, and its rates span the pull's own
            // interval: the gap rule takes the pair's spacing, or the longest
            // a warm pull waits, as its cadence.
            let mut pull = req.clone();
            pull.machine_only = false;
            // A pull keeps the engine's cadence, whatever the held request
            // asked of it.
            pull.jobs_now = false;
            pull.cadence_ms =
                duration_ms(if cold { COLD_PAIR } else { COLD_AFTER }).max(req.cadence_ms);
            let warm = cold.then(|| {
                let warm = source.reading(&pull);
                std::thread::sleep(COLD_PAIR);
                warm
            });
            if let Some(t) = last_whole.filter(|_| !cold) {
                // A window under the floor is noise: wait out the rest.
                std::thread::sleep(MIN_WINDOW.saturating_sub(t.elapsed()));
            }
            let mut snap = source.reading(&pull);
            if let Some(warm) = warm {
                keep_warm_events(warm, &mut snap);
            }
            snap
        } else {
            source.reading(&req)
        };
        let snap = Arc::new(snap);
        if !snap.machine_only {
            last_whole = Some(Instant::now());
        }
        peaks.keep(source.model().peaks(), Instant::now(), false);
        let jobs_moved = hand_over(latest, &snap);
        if let Some(sys) = sys {
            sys.publish(snap, source.machine().unwrap_or(&no_machine));
        }
        if (asked.read || jobs_moved) && !post() {
            break;
        }
    }
    peaks.keep(source.model().peaks(), Instant::now(), true);
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

/// Leave `snap` for the loop, with its job events. True when a job's verdict
/// moved: the loop records it even when it asked no reading (a pull's).
fn hand_over(latest: &Mutex<Handover>, snap: &Arc<Snapshot>) -> bool {
    let events: Vec<_> = snap
        .events
        .iter()
        .filter(|e| matches!(e.kind, aterm_vitals::events::EventKind::Job { .. }))
        .cloned()
        .collect();
    let mut h = latest.lock().unwrap_or_else(PoisonError::into_inner);
    h.snap = Some(Arc::clone(snap));
    let moved = !events.is_empty();
    h.job_events.extend(events);
    if h.job_events.len() > JOB_EVENTS_CAP {
        let over = h.job_events.len() - JOB_EVENTS_CAP;
        h.job_events.drain(..over);
    }
    moved
}

/// The thermal level and Low Power Mode, where the OS says (macOS
/// `NSProcessInfo`; elsewhere never). Injected into [`Reader`], whose test arm
/// is inert.
pub(crate) type ThermalProbe = fn() -> Option<Thermal>;

/// The product's [`ThermalProbe`].
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

/// `NSProcessInfoThermalState` (nominal 0, fair 1, serious 2, critical 3) as
/// the portable level (design §8.1). PURE. Only macOS reports one.
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
/// why). PURE.
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
    /// The machine's words, read once.
    pub(crate) machine: Machine,
}

/// THE STUCK ENGINE'S STEP OVER ONE WHOLE READING at `at_ms` (the probe
/// thread's, [`Reader::read`]): the request's terminal words still current
/// then ([`TtyWord::current`]: no output and no input since the loop read
/// them), which name a person's own wait at the prompt, handed to the pass
/// with the request's roots, and the pass made to look now where the request
/// asks it ([`Request::jobs_now`]).
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
        Self {
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
            // The inert test arm: a unit test never asks AppKit's process info.
            thermal: if cfg!(test) {
                || None
            } else {
                platform_thermal
            },
            counter_throttle: None,
            machine: Machine {
                cpu: words.cpu,
                os: words.os,
                family: os_family(),
            },
        }
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
    /// thread's reader proved them. The slots are read again at most every
    /// [`aterm_sysprobe::slots::PERIOD`] while an `await sys` names them
    /// ([`crate::occupancy_host::SLOTS_ARMED`]), else every
    /// [`aterm_sysprobe::slots::IDLE_PERIOD`]; between, the last reading
    /// rides on with its age, which a wait judges by. The memory figure is
    /// one host statistics call, read each time.
    fn set_occupancy_facts(&mut self, snap: &mut Snapshot) {
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
    }

    /// One full reading folded into the trends: the machine counters and the
    /// process sweep, and the stuck engine's pass over that sweep.
    pub(crate) fn read(&mut self, req: &Request) -> Snapshot {
        let sweep = aterm_sysprobe::processes();
        let mut sample = self.machine_sample(req);
        sample.machine_only = false;
        sample.procs_truncated = sweep.as_ref().is_some_and(|s| s.truncated);
        let observed = sweep
            .as_ref()
            .map(|sweep| crate::resource_jobs::sample(sweep, &req.session_ids, self.epoch))
            .unwrap_or_default();
        sample.procs = observed.procs;
        sample.delegated = observed.delegated;
        let mut snap = self.vitals.absorb(&sample);
        self.set_occupancy_facts(&mut snap);
        crate::resource_jobs::publish(
            sample
                .procs
                .as_ref()
                .and(snap.aterm.as_ref())
                .map(|_| snap.delegated.clone()),
        );
        // The stuck engine reads the same sweep, at its own cadence; a
        // critical memory pressure is the machine thrashing (§9.2), so a wait
        // for pages reads slow, never stuck.
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

    /// A hidden deck's reading (v2 §5.3): the machine counters alone, no
    /// process sweep. Every Global row fills; the aterm and Tab figures, the
    /// busiest names and the writers wait for the next full reading, and the
    /// stuck engine's last view rides on.
    pub(crate) fn read_machine(&mut self, req: &Request) -> Snapshot {
        let mut sample = self.machine_sample(req);
        sample.machine_only = true;
        let mut snap = self.vitals.absorb(&sample);
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
        // The board's facts are asked only while a disk watch runs (the
        // harness is on): with none, its volume's key is not even resolved.
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
            // Windows and Linux count no one user's processes; the uid is
            // asked of none there.
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

/// One process of a sweep as the model takes it, its read time on the
/// model's clock (whose origin is `epoch`).
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
/// and its kind in the model's words. The probe classes tunnels and bridges
/// as other links; their names say which they are.
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

/// The volume holding a tab's directory, for `sys disk`, read as the home
/// volume is: through the mount table on unix ([`local_volume`]), by the
/// drive's spelling and type on Windows.
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
/// the row the watch wrote. Unix reads it through the mount table
/// ([`local_volume`]); Windows has no table to complete, so the drive is
/// judged from the path's spelling and its drive type, and only a fixed or
/// RAM drive is asked (`aterm_sysprobe::volume`). No board runs there.
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
/// The tab's directory is used for lexical lookup, never passed to the filesystem
/// reader: it may contain a symlink or lie on an unavailable network mount.
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
/// With no watch the board says nothing and no clock is read (`watch` false:
/// no level, no lines, no stuck count, no freed tally, v2 §7.6); the survey's
/// rows still come.
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
/// Times become unix ms; paths print under `~` of `home`.
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
/// (`files`) is its own. `None` for a level not measured.
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
/// (`read_at`). `None` with no usage, or with no session id to key the
/// session's baseline by.
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
    /// every [`PEAKS_EVERY`], or now when the thread is `stopping`. A failed
    /// write is tried again a period later.
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
    /// whether aterm is in front ([`Audience::of`]). A test App has no loop to
    /// start the probe thread on, so none sees a reading unless the test
    /// handed it a reader.
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
        let in_view = (bars && drawing().any(|ws| !ws.occluded))
            || (self.tabs_page_shown() && self.windows.values().any(|ws| !ws.occluded));
        let focused = self.windows.values().any(|ws| ws.focused);
        Audience::of(in_view, bars && drawing().next().is_some(), focused)
    }

    /// The deadline the loop folds for the next reading: the bars' cadence
    /// while a window in view shows the Deck or Fleet Monitor tab; with none,
    /// the jobs engine's own while a suspect agent call stands (module docs),
    /// or a hidden deck's machine counters, or an edge's while the held view
    /// is parked; `None` when nothing is due.
    pub(crate) fn vitals_deadline(&self) -> Option<Instant> {
        match (self.vitals.next_due, self.vitals.edge_at(Instant::now())) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// A level judged every wake: start the probe thread (for `sys`, Deck or
    /// no Deck), adopt a snapshot the thread left, send the thread the pane
    /// layout when it changed, and when a reading is due, ask for the next
    /// one. With no window in view showing the bars, the cadence is the jobs
    /// engine's, a hidden deck's and an edge's
    /// ([`Self::serve_windowless_vitals`]); a pull still reads. A process
    /// event the kernel edge heard is an edge ([`ProcEdge`]), and the latch is
    /// armed again for the next turn.
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
        // A reading no window shows draws nothing; turning the bars off
        // still clears the strips' meters.
        if (adopted && shown) || chips_shown != self.vitals.chips_shown {
            self.vitals.chips_shown = chips_shown;
            self.show_vitals_reading();
        }
        // With no thread (none could start), every ask below is a no-op and
        // the deadlines still move on, so the loop never spins on them.
        let Some((period, machine_only)) = audience
            .cadence(self.any_stats_lens_open())
            .filter(|_| shown)
        else {
            self.serve_windowless_vitals(audience, now, delegated);
            return;
        };
        if !std::mem::replace(&mut self.vitals.windowed, true) {
            // A window shows the bars again: read at once, as at the first.
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
    /// `jobs_now` ([`Request::jobs_now`]). A full queue keeps the older
    /// message, and the layout goes again on a later look.
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
    /// sent (for `sys`); no reading. Looked at most every [`LAYOUT_EVERY`]:
    /// the look walks every pane, and a wake (a keystroke, output) must not.
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
    /// view's engine demand is due. The next is named when that reading lands
    /// ([`Self::adopt_vitals`]), from the view it gave; until then the engine
    /// asks nothing more. The bars just hidden, their cadence stops and the
    /// held view's demand goes on. A deck drawn only in windows that are
    /// hidden or occluded ([`Audience::Hidden`]) is read besides, the machine
    /// counters alone every [`HIDDEN_PERIOD`] (v2 §5.3). Output while the
    /// held view is parked asks a whole reading too, at most every
    /// [`JOBS_FLOOR`] (module docs, EDGES).
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

    /// OUTPUT FROM A SESSION (the `Wake::Output` arm, at its clock `now`): an
    /// edge ([`VitalsHost::note_edge`]), and one due now is asked in this
    /// wake. Left to the next turn it would be the loop's deadline, armed at
    /// now and fired at once: a `system_vitals` timer wake for every write a
    /// program makes at rest (Muse Code's SGR reset every 10 s), which
    /// Autopilot Land's END counts against the instance. Within the floor the
    /// edge waits for its deadline as before.
    pub(crate) fn vitals_output_edge(&mut self, now: Instant) {
        self.vitals.note_edge();
        if self.vitals.edge_at(now).is_some_and(|at| at <= now) {
            self.service_vitals(now);
        }
    }

    /// THE DISK WATCH LOOKED (`HostNote::DiskLooked`, once a
    /// `harness_host::DISK_TICK`): with no window in view showing the bars,
    /// take one whole reading now, so the jobs board speaks of now (module
    /// docs). With one, the bars' own cadence reads, and the look changes
    /// nothing.
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
    /// `task=<id>` for an agent's call and `code=<n>` once it exited. A
    /// session that has closed records nothing.
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
                .record(crate::sys_host::JOB_CHANGE, payload);
        }
    }

    /// `Wake::Vitals`: the thread left a snapshot (a reading the loop asked,
    /// or a pull's whose job verdicts moved).
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
    /// ([`VitalsHost::windowless_due`]). True when one was adopted.
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
    /// A snapshot with no view posts nothing: the board's last view ages out
    /// on its own ([`crate::vitals_jobs::JOBS_FRESH`]).
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

    /// The Disk lens opened (v2 §7.4): start the "where" survey on a thread
    /// of its own, unless one runs or one started within the hour, over
    /// `$HOME`, every pane's working directory and each agent in front of a
    /// pane (its working directory and its `CARGO_TARGET_DIR`). It reads only.
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

    /// One tab's load step for its chip's meter (design §12.4): its focused
    /// session's cores-worth on the machine's capacity
    /// ([`crate::tab_bar::chip_load_step`]). 0 (no meter) in a one-tab window
    /// (its chip is the window title), while the stats bars are off (nothing
    /// is read, so a reading would be stale), and before the first reading.
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

    /// Hand each native strip its chips' load steps and the meters' inks
    /// after a reading: a repaint of the chips whose step moved, never a
    /// strip rebuild. The in-grid strip reads the same steps through
    /// `stamp_presence_chips` on the frame the reading asks for.
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
    /// it is the front tab's focused pane)`. A pane without a live shell pid
    /// is skipped.
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
    /// holds changes exactly when this does. Allocates nothing; it walks every
    /// pane, so it runs at a reading or a rate-limited look
    /// ([`Self::send_layout`]), never every wake.
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
    /// split pane's processes count in that pane), and where each sits (for
    /// `sys`); each
    /// agent session's token totals from the Claude Code footer of every pane
    /// of every tab (v2 §8.1: aterm's rate is over every agent session, split
    /// or not) keyed to the tab that holds it, and the front tab's plan usage
    /// (its focused pane's, else its first pane's that has one), all copied
    /// under each pane's timeline lock. The front tab is the frontmost
    /// window's active one, which stays put while aterm is in the background.
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
                // The focused pane first: its plan is the front tab's, and a
                // session two panes show is counted by the first.
                let panes = std::iter::once(tab.focus)
                    .chain(tab.root.leaves().into_iter().filter(|v| *v != tab.focus));
                for pane in panes.filter_map(terminal) {
                    let Some(s) = self.pool.get(pane) else {
                        continue;
                    };
                    let tl = s
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    // Each pane's shell and the program in front of it.
                    if let Ok(shell_pid) = u32::try_from(s.pid)
                        && shell_pid > 0
                    {
                        tabs.push(TabRoot {
                            key: pane,
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
            }
        }
        // A windowless session still owns its explicitly delegated work.
        // Each pool session appears only once even if several views show it.
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
        // Each root's terminal mode word, once a session: a `tcgetattr` and
        // a `FIONREAD` on the master its sink holds.
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
