// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SESSION JOURNAL (`docs/DESIGN-session-log-2026-10-15.md` §5.3, §5.4;
//! phase 2B): every session's log, appended to disk by one thread,
//! `aterm-session-log`, so `log @<sid>` and `brief @<sid>` answer for a
//! session from an earlier run or a crash (`session_log_journal_read`).
//!
//! ON BY DEFAULT (D3, ruled 2026-10-19; Phase 6 ruling U21): the journal
//! follows the desk ([`gate`]: `restore_session` and `Desk::kept`, whose lab
//! seam gives a private headless instance a window's desk), on macOS and
//! Linux. `[session_log] journal = false` is the opt-out (ruling R10): then,
//! as headless, with `restore_session` off and on Windows, [`Lane::new`]
//! makes an inert lane: no directory, no file, no thread, no wake installed
//! in any log, no read, no deletion, on every path. No data migration: an
//! older build after a downgrade neither writes nor prunes the folder.
//!
//! THE WRITER pulls. A producer's append does no I/O and touches no channel:
//! the log rings the lane's [`aterm_slog::Notify`] (ruling R6: inside the
//! log, so no producer path bypasses it), one relaxed swap and at most one
//! unpark per writer pass. The thread parks while clean (an idle machine arms
//! nothing), coalesces wakes to one pass per [`COALESCE`], and ticks once a
//! second only while a retry is owed. A pass:
//! 1. takes the store's write lock with `try_write`, clones live timelines
//!    and moves the bounded retirement handoff out atomically (a session
//!    that retired before its first snapshot is included, ruling R7);
//! 2. for each track takes its timeline with `try_lock` (busy: skipped and
//!    owed), copies at most [`BATCH_RECORDS`] records past its cursor
//!    (`aterm_slog::journal::take`), and lets go: the lock is held for the
//!    copy only, never across I/O;
//! 3. encodes the lines (`journal::take_frames`: a snap restating the open
//!    path first in each segment and after any loss) and appends them in one
//!    `write(2)`.
//!
//! Every process writes only its own fresh files (ruling R3): segments
//! `<inst>.<n>.slog` created exclusive, 0600, rotated at
//! `journal::SEGMENT_BYTES` (a pass stops adding lines at that size, so no
//! segment passes it by more than one take's lines, and every segment is one
//! a reader reads). A segment's `#seg` header goes out with its first lines,
//! in the same `write(2)`. After a short or failed write the segment is
//! abandoned, never repaired: the cursors of lines not wholly written stay,
//! and the next pass writes them into a new segment (a reader drops the torn
//! tail). The instance's liveness is its `<inst>.lock`, taken before any
//! segment and held for the journal's life (ruling R2); no `flock`, no
//! journal. Retention (ruling R9) runs at start and at each rotation, under
//! `prune.lock`, removing eligible inactive files toward
//! `journal::BUDGET_BYTES` and `journal::FILES_MAX`. Live or locked files
//! can keep the directory above those targets.
//!
//! ERRORS. EINTR is retried (at most three times); EINTR past that and
//! EAGAIN wrote nothing, so the segment is kept and the pass retried at the
//! next tick, and [`TRANSIENT_TO_BACKOFF`] of them in a row back off as EIO
//! does (no new file for any of it); a full disk (ENOSPC, EDQUOT, a short
//! write, or free space under [`FREE_FLOOR`]) backs off 30 s doubling to
//! 10 min; EIO backs off, and stops the journal after three in a row;
//! anything else (EACCES, EPERM, EROFS, a symlinked or foreign directory, a
//! caught panic) stops it. None of it touches a shell: the worst a disk does
//! is stop the journal. Each is a fact for later (`exceptions::Entry`, never
//! an alert).
//!
//! THE RING AND THE WRITER. Each take tells its log how far the writer has
//! read (`Log::note_journaled`), so a ring past its bounds evicts what the
//! journal already holds before what it has not reached: a burst between two
//! coalesced passes loses nothing the writer could still write.
//!
//! ACROSS AN UPDATE (ruling R5) the outgoing process drains each fenced log
//! to its carry's fence and ends its chain with `#handed fence=<H>` per
//! session and `#fin why=handoff`; the Commit never waits on it. The
//! successor is ARMED until its Commit lands: it writes nothing, takes no
//! lock and lists nothing before, and a candidate that never commits writes
//! nothing on any exit path. Its journal then starts each carried session
//! after the carried head (`SessionLog::journal_start`).
//!
//! A CLEAN QUIT (ruling R14) drains, writes each open session's `out=app-quit`
//! closes (each stamped by its own session's clock, so each says how long it
//! lived) and `#fin why=quit`, within [`QUIT_BUDGET`] on the writer thread,
//! only once every session is journaled to its log's head; the exit never
//! waits past it, and a missed deadline or an unfinished drain writes
//! neither and reads as a crash. A
//! writer backing off from a full disk tries once more at the quit; a
//! stopped one writes nothing, and its chain reads as a crash.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, TryLockError, Weak};
use std::time::{Duration, Instant};

use aterm_slog::ShortId;
use aterm_slog::journal::{
    self, EndWhy, FORMAT, Frame, Inst, Liveness, encode_frame, parse_lock_name, parse_segment_name,
    segment_name,
};

use crate::session_timeline::SessionTimeline;

#[path = "session_log_journal_fs.rs"]
pub(crate) mod fs;
#[path = "session_log_journal_read.rs"]
pub(crate) mod read;
#[cfg(test)]
#[cfg(unix)]
#[path = "session_log_journal_tests.rs"]
mod tests;

use fs::{Appender, Held, JournalFs};

/// The errnos the journal tells apart (module docs). A target without POSIX
/// errnos (Windows) gets values no raw OS error carries, so there only the
/// `io::ErrorKind` arms and the defaults decide.
#[cfg(unix)]
mod errno {
    pub(super) use libc::{EAGAIN, EDQUOT, EINTR, EIO, ENOLCK, ENOSPC, ENOTSUP, EWOULDBLOCK};
}
#[cfg(not(unix))]
mod errno {
    pub(super) const EAGAIN: i32 = -1;
    pub(super) const EDQUOT: i32 = -2;
    pub(super) const EINTR: i32 = -3;
    pub(super) const EIO: i32 = -4;
    pub(super) const ENOLCK: i32 = -5;
    pub(super) const ENOSPC: i32 = -6;
    pub(super) const ENOTSUP: i32 = -7;
    pub(super) const EWOULDBLOCK: i32 = -8;
}

/// The most records one take copies under a timeline's lock.
pub(crate) const BATCH_RECORDS: usize = 512;
/// A pass stops reading live sessions past this many bytes (and owes the
/// rest); a closed session's lines are written whatever the size.
pub(crate) const PASS_BYTES: usize = 1 << 20;
/// Wakes closer than this coalesce into one pass.
pub(crate) const COALESCE: Duration = Duration::from_millis(250);
/// The retry tick, armed only while a retry is owed.
pub(crate) const RETRY_TICK: Duration = Duration::from_secs(1);
/// A full disk's first wait, doubled to [`BACKOFF_MAX`].
pub(crate) const BACKOFF_FIRST: Duration = Duration::from_secs(30);
/// See [`BACKOFF_FIRST`].
pub(crate) const BACKOFF_MAX: Duration = Duration::from_secs(600);
/// The most retired sessions held unwritten (ruling R7's backstop): past it
/// the oldest gives up its timeline and keeps only what its `#gap
/// why=dropped` says, until that is written.
pub(crate) const RETIRED_MAX: usize = 4096;
/// The most sessions held only to say their loss (a few hundred bytes each,
/// while a disk refuses every write); past it the oldest is forgotten,
/// counted.
pub(crate) const DROPPED_MAX: usize = 1 << 16;
/// Transient write errors in a row (EAGAIN, EINTR past its retries) before
/// the writer backs off.
pub(crate) const TRANSIENT_TO_BACKOFF: u32 = 3;
/// A volume with less free than this pauses the journal before ENOSPC does.
pub(crate) const FREE_FLOOR: u64 = 1 << 30;
/// What a clean quit may spend on the journal's last lines.
pub(crate) const QUIT_BUDGET: Duration = Duration::from_millis(500);
const EINTR_RETRIES: u32 = 3;
/// The most passes a quit's drain runs, beside its deadline: a drain that
/// only ever restates (a segment filled by snaps alone) ends too, unfinished,
/// so it writes no closes and no `#fin` and reads as the crash it was.
pub(crate) const FINISH_PASSES: usize = 64;
const EEXIST_RETRIES: u32 = 16;
const IO_ERRORS_TO_STOP: u32 = 3;
/// How often a running journal looks at the volume's free space.
const FREE_CHECK_EVERY: Duration = Duration::from_secs(60);

/// Why a requested journal cannot run on the launch's configuration and desk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inactive {
    Disabled,
    Unsupported,
    RestoreOff,
    NoDesk,
}

impl Inactive {
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::Disabled => "the journal switch is off",
            Self::Unsupported => "this platform has no journal writer",
            Self::RestoreOff => "session restore is off",
            Self::NoDesk => "this instance does not save a desktop",
        }
    }
}

/// One admission policy for the writer and Settings' next-launch disclosure.
pub(crate) fn eligibility(
    requested: bool,
    restore: bool,
    desk: crate::desk::Desk,
    supported: bool,
) -> Option<Inactive> {
    if !requested {
        Some(Inactive::Disabled)
    } else if !supported {
        Some(Inactive::Unsupported)
    } else if !restore {
        Some(Inactive::RestoreOff)
    } else if !desk.kept() {
        Some(Inactive::NoDesk)
    } else {
        None
    }
}

/// Read once at launch; changing the switch does not stop an existing writer.
pub(crate) fn gate(config: &crate::app_config::Config, desk: crate::desk::Desk) -> bool {
    eligibility(
        config.session_log_journal(),
        config.restore_session_or_default(),
        desk,
        cfg!(unix),
    )
    .is_none()
}

// ---- the wake and the lane's shared state ----------------------------

/// The writer's wake (ruling R6): a dirty flag and the writer's thread.
#[derive(Default)]
pub(crate) struct Wake {
    dirty: AtomicBool,
    thread: OnceLock<std::thread::Thread>,
}

impl Wake {
    /// A record was appended or evicted: mark the journal dirty and, the
    /// first time since the writer last looked, unpark it.
    pub(crate) fn ring(&self) {
        if !self.dirty.swap(true, Ordering::AcqRel)
            && let Some(t) = self.thread.get()
        {
            t.unpark();
        }
    }

    fn take(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }
}

/// What the writer tells the UI thread for an `exceptions::Entry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attention {
    /// Nothing to say.
    None,
    /// Paused: the disk is full (or failing), and the writer retries.
    Paused(Cause),
    /// Stopped for this run.
    Stopped(Cause),
}

/// Why the journal paused or stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    /// No room on the volume.
    Full,
    /// The disk failed a write.
    Io,
    /// The directory or a file was refused (permission, a symlink, a
    /// foreign owner, a read-only volume).
    Denied,
    /// The filesystem takes no `flock`: liveness cannot be told.
    Unsupported,
    /// The writer panicked.
    Panic,
    /// The writer thread could not start.
    Spawn,
}

impl Cause {
    /// The word the entry and the log line say.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Cause::Full => "the disk is full",
            Cause::Io => "the disk failed a write",
            Cause::Denied => "its folder or a file was refused",
            Cause::Unsupported => "the folder's filesystem has no file locks",
            Cause::Panic => "its writer failed",
            Cause::Spawn => "its writer could not start",
        }
    }

    const fn code(self) -> u8 {
        match self {
            Cause::Full => 1,
            Cause::Io => 2,
            Cause::Denied => 3,
            Cause::Unsupported => 4,
            Cause::Panic => 5,
            Cause::Spawn => 6,
        }
    }

    const fn of(code: u8) -> Option<Cause> {
        match code {
            1 => Some(Cause::Full),
            2 => Some(Cause::Io),
            3 => Some(Cause::Denied),
            4 => Some(Cause::Unsupported),
            5 => Some(Cause::Panic),
            6 => Some(Cause::Spawn),
            _ => None,
        }
    }
}

/// The writer's counters and its word for the UI thread: atomics only.
#[derive(Default)]
pub(crate) struct Report {
    pub(crate) lines: AtomicU64,
    pub(crate) bytes: AtomicU64,
    pub(crate) passes: AtomicU64,
    pub(crate) segments: AtomicU64,
    pub(crate) pruned: AtomicU64,
    pub(crate) busy_skips: AtomicU64,
    pub(crate) err_full: AtomicU64,
    pub(crate) err_io: AtomicU64,
    pub(crate) err_denied: AtomicU64,
    pub(crate) err_transient: AtomicU64,
    pub(crate) dropped: AtomicU64,
    /// Conservative retirement handoff overflow, counted before checking whether
    /// an existing writer track already holds stronger data. Not an exact count
    /// of lost sessions or records.
    pub(crate) forgotten: AtomicU64,
    /// 0 none, 1 paused, 2 stopped; the cause's code in the high nibble.
    attention: AtomicU8,
    attention_gen: AtomicU32,
}

impl Report {
    fn bump(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }

    fn say(&self, a: Attention) {
        let word = match a {
            Attention::None => 0,
            Attention::Paused(c) => 1 | (c.code() << 4),
            Attention::Stopped(c) => 2 | (c.code() << 4),
        };
        if self.attention.swap(word, Ordering::AcqRel) != word {
            self.attention_gen.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The writer's word, and how many times it changed.
    pub(crate) fn attention(&self) -> (u32, Attention) {
        let changes = self.attention_gen.load(Ordering::Acquire);
        let word = self.attention.load(Ordering::Acquire);
        let cause = Cause::of(word >> 4).unwrap_or(Cause::Io);
        let a = match word & 0xf {
            1 => Attention::Paused(cause),
            2 => Attention::Stopped(cause),
            _ => Attention::None,
        };
        (changes, a)
    }
}

const REQ_COMMIT: u8 = 1;
const REQ_FINISH: u8 = 2;
const REQ_HANDING: u8 = 4;
const REQ_STOOD_DOWN: u8 = 8;

/// The lane's phase as the thread publishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Armed = 0,
    Running = 1,
    Backoff = 2,
    Stopped = 3,
    Handed = 4,
}

struct Shared {
    wake: Arc<Wake>,
    notify: aterm_slog::Notify,
    req: AtomicU8,
    phase: AtomicU8,
    report: Arc<Report>,
    ack: Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
    finish_budget_ms: AtomicU64,
    /// The armed lane's passes that found no Commit and parked: how a test
    /// knows the lane chose to wait, not merely that it has yet to run.
    #[cfg(test)]
    armed_waits: AtomicU64,
}

impl Shared {
    /// A lane's shared state, published nowhere yet.
    fn new() -> Arc<Shared> {
        let wake = Arc::new(Wake::default());
        let ring = Arc::clone(&wake);
        Arc::new(Shared {
            notify: aterm_slog::Notify::new(move || ring.ring()),
            wake,
            req: AtomicU8::new(0),
            phase: AtomicU8::new(Phase::Armed as u8),
            report: Arc::new(Report::default()),
            ack: Mutex::new(None),
            finish_budget_ms: AtomicU64::new(0),
            #[cfg(test)]
            armed_waits: AtomicU64::new(0),
        })
    }

    fn request(&self, r: u8) {
        self.req.fetch_or(r, Ordering::AcqRel);
        if let Some(t) = self.wake.thread.get() {
            t.unpark();
        }
    }

    fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Acquire) {
            1 => Phase::Running,
            2 => Phase::Backoff,
            3 => Phase::Stopped,
            4 => Phase::Handed,
            _ => Phase::Armed,
        }
    }
}

/// This process's lane, once a journal-keeping launch made one.
static LANE: OnceLock<Arc<Shared>> = OnceLock::new();

/// The wake a new session log installs ([`aterm_slog::Log::set_notify`]):
/// `None` while this process keeps no journal, so a log rings nothing.
pub(crate) fn notify() -> Option<aterm_slog::Notify> {
    LANE.get().map(|s| s.notify.clone())
}

/// The successor's Commit landed: an armed lane starts (the commit waiter's
/// `release_committed_readers`).
#[cfg(unix)]
pub(crate) fn commit_landed() {
    if let Some(s) = LANE.get() {
        s.request(REQ_COMMIT);
    }
}

/// The handoff worker fenced every carried log: the writer drains each to
/// its fence and ends its chain `#handed` (ruling R5).
#[cfg(any(unix, test))]
pub(crate) fn note_handing() {
    if let Some(s) = LANE.get() {
        s.request(REQ_HANDING);
    }
}

/// The update stood down: the writer goes on, in a new segment.
#[cfg(unix)]
pub(crate) fn note_stood_down() {
    if let Some(s) = LANE.get() {
        s.request(REQ_STOOD_DOWN);
    }
}

/// Whether this process's journal is writing (or retrying), so a carried
/// session's records up to its fence will be on disk: what the carry's
/// `journal v=1` host line says to the successor.
#[cfg(any(unix, test))]
pub(crate) fn journals_to_fence() -> bool {
    LANE.get()
        .is_some_and(|s| matches!(s.phase(), Phase::Running | Phase::Backoff))
}

/// A memory-only observation. Running says the writer is active, not that every
/// dirty record has already reached disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) phase: Option<Phase>,
    pub(crate) attention: Attention,
}

/// The App's handle on its journal lane (`App::session_log_journal`).
pub(crate) struct Lane {
    shared: Option<Arc<Shared>>,
    logs: Option<PathBuf>,
    armed: bool,
    started: bool,
}

impl Lane {
    /// A lane that does nothing: `journal = false`, headless,
    /// `restore_session` off, Windows.
    pub(crate) fn inert() -> Lane {
        Lane {
            shared: None,
            logs: None,
            armed: false,
            started: false,
        }
    }

    /// This launch's lane: inert unless `logs` names the logs directory (the
    /// [`gate`] passed). On, it publishes the wake every session log made
    /// from now on installs, and the reader, before any session exists; it
    /// touches no file until [`Lane::start`]'s thread runs, and an `armed`
    /// lane (an update's candidate) none before its Commit.
    pub(crate) fn new(logs: Option<PathBuf>, armed: bool) -> Lane {
        let Some(logs) = logs else {
            return Lane::inert();
        };
        let shared = Shared::new();
        if LANE.set(Arc::clone(&shared)).is_err() {
            aterm_log::warn!("session journal: a second lane in one process; it stays inert");
            return Lane::inert();
        }
        read::install(logs.clone());
        Lane {
            shared: Some(shared),
            logs: Some(logs),
            armed,
            started: false,
        }
    }

    /// Start the writer thread over `store`'s sessions. A spawn failure
    /// leaves the lane stopped.
    pub(crate) fn start(&mut self, store: crate::session_store::Store) {
        self.start_with(store, |writer| {
            std::thread::Builder::new()
                .name("aterm-session-log".to_string())
                .spawn(writer)
                .map(|_| ())
        });
    }

    fn start_with(
        &mut self,
        store: crate::session_store::Store,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
    ) {
        let (Some(shared), Some(logs)) = (self.shared.clone(), self.logs.clone()) else {
            return;
        };
        if self.started {
            return;
        }
        self.started = true;
        let Some(inst) = Inst::new(crate::control_auth::process_instance_id()) else {
            aterm_log::warn!("session journal: this instance's id is no journal name; no journal");
            shared.phase.store(Phase::Stopped as u8, Ordering::Release);
            return;
        };
        // Arm before spawning and before the first session registration.
        // The guard moves into the closure; a failed spawn drops it here.
        let retirement = RetirementLease::arm(Arc::clone(&store), shared.notify.clone());
        let armed = self.armed;
        let worker_shared = Arc::clone(&shared);
        if let Err(error) = spawn(Box::new(move || {
            let _retirement = retirement;
            run(&worker_shared, &logs, store, inst, armed);
        })) {
            shared.report.say(Attention::Stopped(Cause::Spawn));
            shared.phase.store(Phase::Stopped as u8, Ordering::Release);
            aterm_log::warn!("session journal: the writer did not start ({error}); no journal");
        }
    }

    pub(crate) fn status(&self) -> Status {
        Status {
            phase: self.shared.as_ref().map(|shared| shared.phase()),
            attention: self
                .attention()
                .map_or(Attention::None, |(_, attention)| attention),
        }
    }

    /// THE QUIT (ruling R14): ask the writer for its last lines and wait at
    /// most `budget` for them. An armed, stopped or inert lane answers at
    /// once, having written nothing.
    pub(crate) fn finish_at_quit(&self, budget: Duration) {
        let Some(shared) = &self.shared else {
            return;
        };
        if !self.started || shared.wake.thread.get().is_none() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        *shared.ack.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
        shared.finish_budget_ms.store(
            u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
            Ordering::Release,
        );
        shared.request(REQ_FINISH);
        let _ = rx.recv_timeout(budget);
    }

    /// The writer's word for the owner, and how many times it changed.
    pub(crate) fn attention(&self) -> Option<(u32, Attention)> {
        self.shared.as_ref().map(|s| s.report.attention())
    }
}

fn ack(shared: &Shared) {
    if let Some(tx) = shared.ack.lock().unwrap_or_else(|p| p.into_inner()).take() {
        let _ = tx.try_send(());
    }
}

/// The real clock: monotonic from the lane's start, wall from the host.
struct HostClock(Instant);

impl Clock for HostClock {
    fn mono(&self) -> Duration {
        self.0.elapsed()
    }

    fn wall_ms(&self) -> u64 {
        aterm_types::current_time_ms().unwrap_or(0)
    }
}

/// THE WRITER THREAD's loop (module docs). `park_timeout` is its only wait.
fn run(
    shared: &Arc<Shared>,
    logs: &std::path::Path,
    store: crate::session_store::Store,
    inst: Inst,
    armed: bool,
) {
    let _ = shared.wake.thread.set(std::thread::current());
    let mut armed = armed;
    let mut writer: Option<Writer<crate::session_store::Store>> = None;
    let mut last_pass: Option<Instant> = None;
    let mut store = Some(store);
    // The first pass restates every session, dirty or not.
    shared.wake.ring();
    loop {
        let req = shared.req.swap(0, Ordering::AcqRel);
        if req & REQ_FINISH != 0 {
            if !armed && let Some(w) = writer.as_mut() {
                let budget = Duration::from_millis(shared.finish_budget_ms.load(Ordering::Acquire));
                let deadline = w.clock.mono() + budget.mul_f32(0.9);
                guarded(w, |w| w.finish(deadline));
            }
            ack(shared);
            return;
        }
        if armed {
            if req & REQ_COMMIT == 0 {
                #[cfg(test)]
                shared.armed_waits.fetch_add(1, Ordering::AcqRel);
                std::thread::park_timeout(Duration::from_secs(60));
                continue;
            }
            armed = false;
        }
        let w = match writer.as_mut() {
            Some(w) => w,
            None => {
                let Some(targets) = store.take() else {
                    return;
                };
                let mut w = Writer::new(
                    open_dir(logs),
                    targets,
                    inst.clone(),
                    Arc::new(HostClock(Instant::now())),
                    WriterLimits::default(),
                    Arc::clone(&shared.report),
                );
                guarded(&mut w, Writer::start);
                writer.insert(w)
            }
        };
        if req & REQ_HANDING != 0 {
            w.begin_handing();
        }
        if req & REQ_STOOD_DOWN != 0 {
            w.stood_down();
        }
        shared.phase.store(w.phase() as u8, Ordering::Release);
        if w.is_stopped() || w.handed_done {
            std::thread::park_timeout(Duration::from_secs(3600));
            continue;
        }
        if let Some(wait) = w.wait() {
            std::thread::park_timeout(wait);
            continue;
        }
        let now = Instant::now();
        let dirty = shared.wake.take();
        let retry = w.owed && last_pass.is_none_or(|t| now >= t + RETRY_TICK);
        if !dirty && !retry {
            std::thread::park_timeout(if w.owed {
                RETRY_TICK
            } else {
                Duration::from_secs(3600)
            });
            continue;
        }
        if let Some(t) = last_pass
            && now < t + COALESCE
        {
            if dirty {
                shared.wake.dirty.store(true, Ordering::Release);
            }
            std::thread::park_timeout(t + COALESCE - now);
            continue;
        }
        guarded(w, |w| {
            let _ = w.pass();
        });
        last_pass = Some(Instant::now());
        shared.phase.store(w.phase() as u8, Ordering::Release);
    }
}

/// The directory a running lane writes, or a stand-in that refuses
/// everything (a symlinked, foreign or unreadable one stops the journal at
/// its first step).
fn open_dir(logs: &std::path::Path) -> Arc<dyn JournalFs> {
    #[cfg(unix)]
    match fs::PinnedJournalFs::open(logs, true) {
        Ok(Some(dir)) => return Arc::new(dir),
        Ok(None) => {}
        Err(error) => {
            aterm_log::warn!(
                "session journal: {}/session-log refused ({error}); no journal",
                logs.display()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = logs;
    Arc::new(Refused)
}

/// A directory that refuses every write: what a refused directory becomes.
struct Refused;

impl JournalFs for Refused {
    fn lock_instance(&self, _: &Inst) -> io::Result<Box<dyn Held>> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
    fn unlock_instance(&self, _: &Inst, _: Box<dyn Held>) {}
    fn create_segment(&self, _: &str) -> io::Result<Box<dyn Appender>> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
    fn scan(&self, _: usize, _: &mut dyn FnMut(&str)) -> io::Result<bool> {
        Ok(false)
    }
    fn meta(&self, _: &str) -> io::Result<fs::Meta> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }
    #[cfg(unix)]
    fn read(&self, _: &str, _: u64, _: u64) -> io::Result<(Vec<u8>, fs::Meta)> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }
    fn remove(&self, _: &str, _: fs::Meta) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
    fn probe(&self, _: &str) -> Liveness {
        Liveness::Unknown
    }
    fn try_prune_lock(&self) -> io::Result<Option<Box<dyn Held>>> {
        Ok(None)
    }
    fn sync_dir(&self) -> io::Result<()> {
        Ok(())
    }
    fn free_bytes(&self) -> Option<u64> {
        None
    }
}

/// Run one writer step; a panic stops the journal (its attention raised),
/// and the producers go on appending.
fn guarded<T: Targets>(w: &mut Writer<T>, f: impl FnOnce(&mut Writer<T>)) {
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(w)));
    if caught.is_err() {
        aterm_log::warn!("session journal: the writer panicked; the journal stops for this run");
        w.stop(Cause::Panic);
    }
}

// ---- the writer -------------------------------------------------------

/// The writer's clock: monotonic for its own waits, wall for the lines.
pub(crate) trait Clock: Send + Sync {
    /// Since the writer began.
    fn mono(&self) -> Duration;
    /// Unix ms.
    fn wall_ms(&self) -> u64;
}

/// A store snapshot: live sessions and retirements taken under one lock.
#[derive(Default)]
pub(crate) struct TargetSnapshot {
    live: Vec<(ShortId, Arc<Mutex<SessionTimeline>>)>,
    retired: Vec<RetiredTarget>,
    forgotten: u64,
}

/// Queues moved out in constant time under Store, expanded after unlock.
#[derive(Default)]
pub(crate) struct RetirementBatch {
    timelines: VecDeque<(ShortId, Arc<Mutex<SessionTimeline>>)>,
    dropped: VecDeque<(ShortId, Weak<Mutex<SessionTimeline>>, Box<Dropped>)>,
    forgotten: u64,
}

impl RetirementBatch {
    fn with_live(self, live: Vec<(ShortId, Arc<Mutex<SessionTimeline>>)>) -> TargetSnapshot {
        TargetSnapshot {
            live,
            retired: self
                .timelines
                .into_iter()
                .map(|(sid, timeline)| RetiredTarget::Timeline(sid, timeline))
                .chain(
                    self.dropped.into_iter().map(|(sid, timeline, dropped)| {
                        RetiredTarget::Dropped(sid, timeline, dropped)
                    }),
                )
                .collect(),
            forgotten: self.forgotten,
        }
    }
}

/// A retirement the writer has not yet taken from the store.
enum RetiredTarget {
    Timeline(ShortId, Arc<Mutex<SessionTimeline>>),
    Dropped(ShortId, Weak<Mutex<SessionTimeline>>, Box<Dropped>),
}

/// A bounded handoff at the actual deregistration funnel. Full timelines
/// and compact losses have separate caps, as the writer's backstop does.
/// These are additional staging caps, not a global RETIRED_MAX heap bound.
pub(crate) struct Retirements {
    owner: Arc<()>,
    wake: Option<aterm_slog::Notify>,
    timelines: VecDeque<(ShortId, Arc<Mutex<SessionTimeline>>)>,
    dropped: VecDeque<(ShortId, Weak<Mutex<SessionTimeline>>, Box<Dropped>)>,
    forgotten: u64,
    timeline_max: usize,
    dropped_max: usize,
}

impl Retirements {
    pub(crate) fn new(
        wake: Option<aterm_slog::Notify>,
        timeline_max: usize,
        dropped_max: usize,
    ) -> Retirements {
        Retirements {
            owner: Arc::new(()),
            wake,
            timelines: VecDeque::new(),
            dropped: VecDeque::new(),
            forgotten: 0,
            timeline_max,
            dropped_max,
        }
    }

    pub(crate) fn owner(&self) -> Arc<()> {
        Arc::clone(&self.owner)
    }

    pub(crate) fn owned_by(&self, owner: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.owner, owner)
    }

    /// Called while the CURRENT timeline's existing close lock stands.
    /// Overflow never locks another timeline or copies a record batch.
    pub(crate) fn retire(
        &mut self,
        sid: &aterm_session::SessionId,
        timeline: &Arc<Mutex<SessionTimeline>>,
        session: &crate::session_log::SessionLog,
    ) {
        let Some(sid) = ShortId::new(sid.as_str()) else {
            return;
        };
        if self.timelines.len() < self.timeline_max {
            self.timelines.push_back((sid, Arc::clone(timeline)));
        } else {
            let log = session.log();
            let after = session.journal_start().unwrap_or(0);
            let through = log.last_seq();
            let dropped = Box::new(Dropped {
                take: journal::take(log, after, 0, true),
                through,
                lost: log.held_between(after, through),
            });
            if self.dropped_max == 0 {
                self.forgotten = self.forgotten.saturating_add(1);
            } else {
                if self.dropped.len() == self.dropped_max {
                    self.dropped.pop_front();
                    self.forgotten = self.forgotten.saturating_add(1);
                }
                self.dropped
                    .push_back((sid, Arc::downgrade(timeline), dropped));
            }
        }
        // An idempotent close need not append; enqueuing itself must wake.
        if let Some(wake) = &self.wake {
            wake.ring();
        }
    }

    pub(crate) fn take(&mut self) -> RetirementBatch {
        RetirementBatch {
            timelines: std::mem::take(&mut self.timelines),
            dropped: std::mem::take(&mut self.dropped),
            forgotten: std::mem::take(&mut self.forgotten),
        }
    }

    pub(crate) fn pending(&self) -> bool {
        !self.timelines.is_empty() || !self.dropped.is_empty() || self.forgotten > 0
    }
}

/// Caller-owned before spawn, worker-owned afterward. The owner token
/// prevents a late old guard from disarming a newer writer on that store.
struct RetirementLease {
    store: crate::session_store::Store,
    owner: Arc<()>,
}

impl RetirementLease {
    fn arm(store: crate::session_store::Store, wake: aterm_slog::Notify) -> RetirementLease {
        let owner = store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .arm_session_journal(Some(wake), RETIRED_MAX, DROPPED_MAX);
        RetirementLease { store, owner }
    }
}

impl Drop for RetirementLease {
    fn drop(&mut self) {
        release_retirements(&self.store, &self.owner);
    }
}

/// Detach under the store lock, destroy timelines only after releasing it.
fn release_retirements(store: &crate::session_store::Store, owner: &Arc<()>) {
    let retired = store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .disarm_session_journal(owner);
    drop(retired);
}

/// Where the writer finds the sessions. One writer owns a target source.
pub(crate) trait Targets: Send {
    fn begin_targets(&self, _retired_max: usize) -> Option<Arc<()>> {
        None
    }
    fn end_targets(&self, _owner: &Arc<()>) {}
    /// The live+retired snapshot, or None while the registry is busy.
    fn try_targets(&self) -> Option<TargetSnapshot>;
    /// True also while its lock is busy: no final marker over unseen work.
    fn pending_targets(&self) -> bool {
        false
    }
}

impl Targets for crate::session_store::Store {
    fn begin_targets(&self, retired_max: usize) -> Option<Arc<()>> {
        Some(
            self.write()
                .unwrap_or_else(|p| p.into_inner())
                .arm_session_journal(None, retired_max, DROPPED_MAX),
        )
    }

    fn end_targets(&self, owner: &Arc<()>) {
        release_retirements(self, owner);
    }

    fn try_targets(&self) -> Option<TargetSnapshot> {
        let mut guard = match self.try_write() {
            Ok(g) => g,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
        };
        let live = guard
            .live_handles()
            .filter_map(|h| {
                ShortId::new(h.sid.as_str()).map(|sid| (sid, Arc::clone(&h.ctx.timeline)))
            })
            .collect();
        let retired = guard.take_session_journal_retirements();
        drop(guard);
        Some(retired.with_live(live))
    }

    fn pending_targets(&self) -> bool {
        match self.try_read() {
            Ok(guard) => guard.session_journal_pending(),
            Err(TryLockError::Poisoned(p)) => p.into_inner().session_journal_pending(),
            Err(TryLockError::WouldBlock) => true,
        }
    }
}

/// The writer's bounds (tests shrink them).
#[derive(Clone, Copy, Debug)]
pub(crate) struct WriterLimits {
    pub(crate) segment_bytes: u64,
    pub(crate) budget_bytes: u64,
    pub(crate) files_max: usize,
    pub(crate) pass_bytes: usize,
    pub(crate) batch: usize,
    pub(crate) retired_max: usize,
    pub(crate) free_floor: u64,
}

impl Default for WriterLimits {
    fn default() -> Self {
        WriterLimits {
            segment_bytes: journal::SEGMENT_BYTES,
            budget_bytes: journal::BUDGET_BYTES,
            files_max: journal::FILES_MAX,
            pass_bytes: PASS_BYTES,
            batch: BATCH_RECORDS,
            retired_max: RETIRED_MAX,
            free_floor: FREE_FLOOR,
        }
    }
}

/// One session the writer follows.
struct Track {
    sid: ShortId,
    /// Its timeline; `None` once the backstop let it go ([`Track::dropped`]).
    timeline: Option<Arc<Mutex<SessionTimeline>>>,
    /// Still in the store; `false`: retired, kept until its close is written.
    in_store: bool,
    /// When it retired (order for the backstop).
    retired: u64,
    /// The last seq journaled (or accounted); `None` until first taken.
    cursor: Option<u64>,
    /// The seq space it was last written in: (base, ep).
    space: Option<(u32, u32)>,
    /// The segment its snap was written in.
    snapped: Option<u32>,
    /// Its session's close is written: nothing more to say.
    done: bool,
    /// Its `#handed` is written (an update's drain).
    handed: bool,
    /// The fence its log stood at when last taken.
    fence: Option<u64>,
    /// Let go by the backstop (ruling R7): what its `#gap why=dropped`
    /// says, kept until that is written whole.
    dropped: Option<Box<Dropped>>,
}

/// A session the backstop let go: a probe of its log when it went (its
/// space, the open path), the seq it had reached and how many it held past
/// the writer's cursor.
struct Dropped {
    take: journal::Take,
    through: u64,
    lost: u64,
}

struct Seg {
    n: u32,
    file: Box<dyn Appender>,
    size: u64,
    /// Its `#seg` line, until the first write takes it out with the lines.
    header: Option<Vec<u8>>,
}

enum State {
    Running,
    Backoff { until: Duration },
    Stopped,
}

/// What one track's lines in a batch end at, and what writing them proves.
struct LineMark {
    end: usize,
    key: String,
    next: u64,
    space: (u32, u32),
    snapped: u32,
    done: bool,
    handed: bool,
    fence: Option<u64>,
}

/// THE WRITER: one per process, on the `aterm-session-log` thread (tests
/// drive it directly, with an injected clock and directory).
pub(crate) struct Writer<T: Targets> {
    fs: Arc<dyn JournalFs>,
    targets: T,
    target_owner: Option<Arc<()>>,
    inst: Inst,
    build: String,
    clock: Arc<dyn Clock>,
    limits: WriterLimits,
    report: Arc<Report>,
    lock: Option<Box<dyn Held>>,
    seg: Option<Seg>,
    next_n: u32,
    tracks: BTreeMap<String, Track>,
    retired_seq: u64,
    state: State,
    /// The backoff's delay so far, kept across a pass that fails again.
    delay: Option<Duration>,
    io_errors: u32,
    /// Transient write errors in a row.
    transient: u32,
    /// A retry is owed: a busy lock, a cut batch, a transient error.
    pub(crate) owed: bool,
    handing: bool,
    pub(crate) handed_done: bool,
    last_free_check: Option<Duration>,
    /// Every take saw every live track this pass (none busy).
    saw_all: bool,
}

impl<T: Targets> Writer<T> {
    pub(crate) fn new(
        fs: Arc<dyn JournalFs>,
        targets: T,
        inst: Inst,
        clock: Arc<dyn Clock>,
        limits: WriterLimits,
        report: Arc<Report>,
    ) -> Writer<T> {
        let target_owner = targets.begin_targets(limits.retired_max);
        Writer {
            fs,
            targets,
            target_owner,
            inst,
            build: crate::build_info::version().to_string(),
            clock,
            limits,
            report,
            lock: None,
            seg: None,
            next_n: 1,
            tracks: BTreeMap::new(),
            retired_seq: 0,
            state: State::Running,
            delay: None,
            io_errors: 0,
            transient: 0,
            owed: false,
            handing: false,
            handed_done: false,
            last_free_check: None,
            saw_all: false,
        }
    }

    /// THE START: this instance's lock, before any segment (ruling R2), then
    /// retention. Without the lock there is no journal.
    pub(crate) fn start(&mut self) {
        match self.fs.lock_instance(&self.inst) {
            Ok(held) => self.lock = Some(held),
            Err(error) => {
                let cause = match error.raw_os_error() {
                    Some(errno::ENOTSUP | errno::ENOLCK) => Cause::Unsupported,
                    _ if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::Unsupported =>
                    {
                        Cause::Unsupported
                    }
                    _ => Cause::Denied,
                };
                aterm_log::warn!(
                    "session journal: the instance lock was not taken ({error}); no journal"
                );
                Report::bump(&self.report.err_denied, 1);
                self.stop(cause);
                return;
            }
        }
        self.retention();
    }

    fn phase(&self) -> Phase {
        match self.state {
            State::Running if self.handed_done => Phase::Handed,
            State::Running => Phase::Running,
            State::Backoff { .. } => Phase::Backoff,
            State::Stopped => Phase::Stopped,
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        matches!(self.state, State::Stopped)
    }

    /// How long until a backoff lets the next pass run, if one holds.
    pub(crate) fn wait(&self) -> Option<Duration> {
        match self.state {
            State::Backoff { until, .. } => until.checked_sub(self.clock.mono()),
            _ => None,
        }
    }

    /// Stop the journal for this run: no more lines, no session held. The
    /// instance lock stays held, so a reader never reads this live process
    /// as a crash.
    pub(crate) fn stop(&mut self, cause: Cause) {
        if !matches!(self.state, State::Stopped) {
            aterm_log::warn!("session journal stopped: {}", cause.word());
        }
        self.state = State::Stopped;
        if let Some(owner) = self.target_owner.take() {
            self.targets.end_targets(&owner);
        }
        self.tracks.clear();
        self.seg = None;
        self.report.say(Attention::Stopped(cause));
    }

    fn backoff(&mut self, cause: Cause) {
        let delay = self
            .delay
            .map_or(BACKOFF_FIRST, |d| (d * 2).min(BACKOFF_MAX));
        self.delay = Some(delay);
        self.state = State::Backoff {
            until: self.clock.mono() + delay,
        };
        self.owed = true;
        aterm_log::warn!(
            "session journal paused for {}s: {}",
            delay.as_secs(),
            cause.word()
        );
        self.report.say(Attention::Paused(cause));
    }

    /// What an errno does (module docs).
    fn failed(&mut self, error: &io::Error) {
        match error.raw_os_error() {
            Some(errno::ENOSPC | errno::EDQUOT) => {
                Report::bump(&self.report.err_full, 1);
                self.backoff(Cause::Full);
            }
            Some(errno::EIO) => {
                Report::bump(&self.report.err_io, 1);
                self.io_errors += 1;
                if self.io_errors >= IO_ERRORS_TO_STOP {
                    self.stop(Cause::Io);
                } else {
                    self.backoff(Cause::Io);
                }
            }
            Some(e) if is_transient(e) => {
                // Nothing was written: the segment is kept, and the pass is
                // retried at the next tick, until too many in a row.
                Report::bump(&self.report.err_transient, 1);
                self.owed = true;
                self.transient += 1;
                if self.transient >= TRANSIENT_TO_BACKOFF {
                    self.transient = 0;
                    self.backoff(Cause::Io);
                }
            }
            _ => {
                Report::bump(&self.report.err_denied, 1);
                self.stop(Cause::Denied);
            }
        }
    }

    pub(crate) fn begin_handing(&mut self) {
        self.handing = true;
    }

    /// The update stood down. Every session restates itself on the next
    /// pass (a snap voids the `#handed` of an update that did not happen,
    /// however far its drain got), and a chain whose drain was whole goes on
    /// in a new segment, so its newest segment never holds that update's
    /// `#fin`.
    pub(crate) fn stood_down(&mut self) {
        let was = self.handing || self.handed_done || self.tracks.values().any(|t| t.handed);
        self.handing = false;
        if was {
            for t in self.tracks.values_mut() {
                t.handed = false;
                t.snapped = None;
            }
        }
        if self.handed_done {
            self.handed_done = false;
            self.seg = None;
        }
    }

    /// Merge the store's sessions into the tracks: a new one starts, one
    /// that left retires (its timeline kept until its close is written).
    fn refresh(&mut self) {
        let Some(targets) = self.targets.try_targets() else {
            self.owed = true;
            return;
        };
        Report::bump(&self.report.forgotten, targets.forgotten);
        // Import retirements first: an actually live replacement timeline in
        // the same snapshot retains the existing live-replacement precedence.
        for retired in targets.retired {
            self.import_retired(retired);
        }
        let live: std::collections::BTreeSet<String> = targets
            .live
            .iter()
            .map(|(s, _)| s.as_str().to_string())
            .collect();
        for (sid, timeline) in targets.live {
            let key = sid.as_str().to_string();
            match self.tracks.get_mut(&key) {
                Some(t)
                    if t.timeline
                        .as_ref()
                        .is_some_and(|tl| Arc::ptr_eq(tl, &timeline)) =>
                {
                    t.in_store = true;
                }
                _ => {
                    self.tracks.insert(
                        key,
                        Track {
                            sid,
                            timeline: Some(timeline),
                            in_store: true,
                            retired: 0,
                            cursor: None,
                            space: None,
                            snapped: None,
                            done: false,
                            handed: false,
                            fence: None,
                            dropped: None,
                        },
                    );
                }
            }
        }
        for (key, t) in &mut self.tracks {
            if t.in_store && !live.contains(key) {
                t.in_store = false;
                self.retired_seq += 1;
                t.retired = self.retired_seq;
            }
        }
    }

    /// Keep the stronger existing cursor/Arc when the queued packet names
    /// that exact timeline; a compact fallback must not re-count its records.
    fn import_retired(&mut self, retired: RetiredTarget) {
        let (sid, timeline, dropped) = match retired {
            RetiredTarget::Timeline(sid, timeline) => (sid, Some(timeline), None),
            RetiredTarget::Dropped(sid, identity, dropped) => {
                let key = sid.as_str();
                if self
                    .tracks
                    .get(key)
                    .and_then(|t| t.timeline.as_ref())
                    .is_some_and(|t| Weak::ptr_eq(&Arc::downgrade(t), &identity))
                {
                    self.retire_track(key);
                    return;
                }
                Report::bump(&self.report.dropped, 1);
                (sid, None, Some(dropped))
            }
        };
        let key = sid.as_str().to_string();
        if self
            .tracks
            .get(&key)
            .and_then(|t| t.timeline.as_ref())
            .zip(timeline.as_ref())
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
        {
            self.retire_track(&key);
            return;
        }
        self.retired_seq = self.retired_seq.saturating_add(1);
        let cursor = dropped.as_ref().map(|d| d.take.cursor);
        self.tracks.insert(
            key,
            Track {
                sid,
                timeline,
                in_store: false,
                retired: self.retired_seq,
                cursor,
                space: None,
                snapped: None,
                done: false,
                handed: false,
                fence: None,
                dropped,
            },
        );
    }

    fn retire_track(&mut self, key: &str) {
        if let Some(track) = self.tracks.get_mut(key)
            && track.in_store
        {
            track.in_store = false;
            self.retired_seq = self.retired_seq.saturating_add(1);
            track.retired = self.retired_seq;
        }
    }

    /// A new segment: the old one flushed, the volume's room checked, the
    /// next number created exclusive, its header written, every session to
    /// restate itself, then retention.
    fn rotate(&mut self) -> bool {
        if let Some(mut old) = self.seg.take() {
            let _ = old.file.sync();
        }
        if !self.room() {
            return false;
        }
        let mut made = None;
        for _ in 0..EEXIST_RETRIES {
            let n = self.next_n;
            self.next_n = self.next_n.saturating_add(1);
            match self.fs.create_segment(&segment_name(&self.inst, n)) {
                Ok(file) => {
                    made = Some((n, file));
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    self.failed(&e);
                    return false;
                }
            }
        }
        let Some((n, file)) = made else {
            self.stop(Cause::Denied);
            return false;
        };
        let header = encode_frame(&Frame::Seg {
            v: FORMAT,
            inst: self.inst.clone(),
            n,
            t: self.clock.wall_ms(),
            build: self.build.clone(),
        });
        let Some(mut header) = header else {
            self.stop(Cause::Denied);
            return false;
        };
        header.push('\n');
        Report::bump(&self.report.segments, 1);
        self.seg = Some(Seg {
            n,
            file,
            size: 0,
            header: Some(header.into_bytes()),
        });
        for t in self.tracks.values_mut() {
            t.snapped = None;
        }
        let _ = self.fs.sync_dir();
        self.retention();
        true
    }

    /// Whether the volume has room: checked at each rotation and once a
    /// minute; under [`FREE_FLOOR`] the journal pauses before ENOSPC.
    fn room(&mut self) -> bool {
        let now = self.clock.mono();
        if self
            .last_free_check
            .is_some_and(|t| now < t + FREE_CHECK_EVERY && self.seg.is_some())
        {
            return true;
        }
        self.last_free_check = Some(now);
        match self.fs.free_bytes() {
            Some(free) if free < self.limits.free_floor => {
                Report::bump(&self.report.err_full, 1);
                self.backoff(Cause::Full);
                false
            }
            _ => true,
        }
    }

    /// ONE PASS (module docs). Returns the bytes written.
    pub(crate) fn pass(&mut self) -> usize {
        match self.state {
            State::Stopped => return 0,
            State::Backoff { until, .. } if self.clock.mono() < until => {
                self.owed = true;
                return 0;
            }
            State::Backoff { .. } => self.state = State::Running,
            State::Running => {}
        }
        if self.handed_done || self.lock.is_none() {
            return 0;
        }
        Report::bump(&self.report.passes, 1);
        self.owed = false;
        self.refresh();
        let rotate = self
            .seg
            .as_ref()
            .is_none_or(|s| s.size >= self.limits.segment_bytes);
        if rotate && !self.rotate() {
            return 0;
        }
        if self.seg.is_some() && !self.room() {
            return 0;
        }
        let Some((n, size)) = self.seg.as_ref().map(|s| (s.n, s.size)) else {
            return 0;
        };
        let (buf, marks) = self.batch(n, size);
        let written = self.write(&buf, &marks);
        self.drop_finished();
        if self.handing && !self.handed_done && written == buf.len() {
            self.maybe_hand_off();
        }
        written
    }

    /// The pass's lines: retired tracks first (exempt from the pass's byte
    /// bound, ruling R7), then the live ones; no track's lines once the
    /// segment (`size` so far) would pass its rotation size, so a segment is
    /// never more than one take's lines past it.
    fn batch(&mut self, n: u32, size: u64) -> (Vec<u8>, Vec<LineMark>) {
        let mut buf: Vec<u8> = Vec::new();
        let mut marks: Vec<LineMark> = Vec::new();
        let mut order: Vec<(bool, u64, String)> = self
            .tracks
            .iter()
            .filter(|(_, t)| !t.done)
            .map(|(k, t)| (t.in_store, t.retired, k.clone()))
            .collect();
        order.sort();
        self.saw_all = true;
        let wall = self.clock.wall_ms();
        for (in_store, _, key) in order {
            let full = size.saturating_add(buf.len() as u64) >= self.limits.segment_bytes;
            if !buf.is_empty() && (full || (in_store && buf.len() >= self.limits.pass_bytes)) {
                self.owed = true;
                self.saw_all = false;
                break;
            }
            let Some(t) = self.tracks.get_mut(&key) else {
                continue;
            };
            let Some(timeline) = t.timeline.clone() else {
                // Let go by the backstop: its loss, said in its space.
                if let Some(d) = &t.dropped {
                    let frames = journal::dropped_frames(
                        t.sid,
                        &d.take,
                        d.through,
                        d.lost,
                        t.snapped != Some(n),
                        wall,
                    );
                    if frames.is_empty() {
                        // Nothing left to say: let go.
                        t.done = true;
                        continue;
                    }
                    for f in &frames {
                        if let Some(line) = encode_frame(f) {
                            buf.extend_from_slice(line.as_bytes());
                            buf.push(b'\n');
                        }
                    }
                    marks.push(LineMark {
                        end: buf.len(),
                        key,
                        next: d.through,
                        space: (d.take.base, d.take.ep),
                        snapped: n,
                        done: true,
                        handed: false,
                        fence: None,
                    });
                }
                continue;
            };
            let mut guard = match timeline.try_lock() {
                Ok(g) => g,
                Err(TryLockError::WouldBlock) => {
                    self.owed = true;
                    self.saw_all = false;
                    Report::bump(&self.report.busy_skips, 1);
                    continue;
                }
                Err(TryLockError::Poisoned(p)) => p.into_inner(),
            };
            // A log made before the lane published its wake gets it now.
            if guard.slog().log().notify().is_none()
                && let Some(wake) = notify()
            {
                guard
                    .slog_mut()
                    .log_mut_for_journal()
                    .set_notify(Some(wake));
            }
            let start = guard.slog().journal_start();
            let log = guard.slog().log();
            let space = (log.base_ep(), log.ep());
            if t.space.is_some_and(|(base, _)| base != space.0) {
                // The log was replaced (a new seq space): from its start.
                t.cursor = Some(0);
                t.snapped = None;
            }
            let cursor = *t.cursor.get_or_insert(start.unwrap_or(0));
            let snap = t.snapped != Some(n) || t.space.is_some_and(|s| s != space);
            let take = journal::take(log, cursor, self.limits.batch, snap);
            // The ring evicts what the writer holds before what it has not
            // reached.
            guard
                .slog_mut()
                .log_mut_for_journal()
                .note_journaled(take.next);
            drop(guard);
            t.fence = take.fence;
            let mut frames = journal::take_frames(t.sid, &take, snap, wall);
            let mut handed = t.handed;
            if self.handing
                && !handed
                && let Some(fence) = take.fence
                && take.next >= fence
            {
                frames.push(Frame::Handed { sid: t.sid, fence });
                handed = true;
            }
            if frames.is_empty() {
                // A closed session with nothing left to write is done.
                t.done |= take.done;
                continue;
            }
            for f in &frames {
                if let Some(line) = encode_frame(f) {
                    buf.extend_from_slice(line.as_bytes());
                    buf.push(b'\n');
                }
            }
            if take.more {
                self.owed = true;
            }
            marks.push(LineMark {
                end: buf.len(),
                key,
                next: take.next,
                space,
                snapped: n,
                done: take.done,
                handed,
                fence: take.fence,
            });
        }
        (buf, marks)
    }

    /// Write `buf` in one `write(2)` (EINTR retried): every mark whose lines
    /// are wholly on disk moves its track's cursor; after a short or failed
    /// write the segment is abandoned, never repaired (ruling R3).
    fn write(&mut self, buf: &[u8], marks: &[LineMark]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        let Some(seg) = self.seg.as_mut() else {
            return 0;
        };
        // A new segment's header goes out with its first lines, in one write.
        let head = seg.header.as_ref().map_or(0, Vec::len);
        let result = match &seg.header {
            Some(h) => {
                let mut all = Vec::with_capacity(h.len() + buf.len());
                all.extend_from_slice(h);
                all.extend_from_slice(buf);
                write_all_once(seg.file.as_mut(), &all)
            }
            None => write_all_once(seg.file.as_mut(), buf),
        };
        let landed = match &result {
            Ok(k) => *k,
            Err(_) => 0,
        };
        seg.size = seg.size.saturating_add(landed as u64);
        if head > 0 && landed >= head {
            seg.header = None;
        }
        // The lines' bytes on disk, the header's not counted.
        let wrote = landed.saturating_sub(head);
        for m in marks.iter().filter(|m| m.end <= wrote) {
            if let Some(t) = self.tracks.get_mut(&m.key) {
                t.cursor = Some(m.next);
                t.space = Some(m.space);
                t.snapped = Some(m.snapped);
                t.done |= m.done;
                t.handed = m.handed;
                t.fence = m.fence;
            }
        }
        let lines = buf[..wrote.min(buf.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        Report::bump(&self.report.lines, lines as u64);
        Report::bump(&self.report.bytes, landed as u64);
        match result {
            Ok(k) if k == head + buf.len() => {
                self.io_errors = 0;
                self.transient = 0;
                if self.delay.take().is_some() {
                    aterm_log::info!("session journal resumed");
                }
                if !matches!(self.report.attention().1, Attention::None) {
                    self.report.say(Attention::None);
                }
            }
            Ok(_) => {
                // A short write: the disk is full. Abandon the segment.
                self.seg = None;
                Report::bump(&self.report.err_full, 1);
                self.backoff(Cause::Full);
            }
            Err(e) => {
                // A transient error wrote nothing: the segment is kept.
                if !e.raw_os_error().is_some_and(is_transient) {
                    self.seg = None;
                }
                self.failed(&e);
            }
        }
        wrote
    }

    /// Let go of retired tracks whose close is written (or whose loss is).
    /// Past the backstop (ruling R7) the oldest retired session gives up its
    /// timeline and keeps only what its `#gap why=dropped` says, written by
    /// the next pass (a snap first, in its space) and only then forgotten:
    /// no session is let go unsaid. Past [`DROPPED_MAX`] of those, the
    /// oldest is forgotten, counted.
    fn drop_finished(&mut self) {
        self.tracks.retain(|_, t| t.in_store || !t.done);
        let mut retired: Vec<(u64, String)> = self
            .tracks
            .iter()
            .filter(|(_, t)| !t.in_store && t.timeline.is_some())
            .map(|(k, t)| (t.retired, k.clone()))
            .collect();
        if retired.len() > self.limits.retired_max {
            retired.sort();
            let over = retired.len() - self.limits.retired_max;
            for (_, key) in retired.into_iter().take(over) {
                let Some(t) = self.tracks.get_mut(&key) else {
                    continue;
                };
                let Some(timeline) = t.timeline.clone() else {
                    continue;
                };
                let Ok(guard) = timeline.try_lock() else {
                    // Busy: let go on a later pass.
                    self.owed = true;
                    continue;
                };
                let log = guard.slog().log();
                let after = t
                    .cursor
                    .unwrap_or_else(|| guard.slog().journal_start().unwrap_or(0));
                let through = log.last_seq();
                let dropped = Dropped {
                    take: journal::take(log, after, 0, true),
                    through,
                    lost: log.held_between(after, through),
                };
                drop(guard);
                t.cursor = Some(after);
                t.timeline = None;
                t.dropped = Some(Box::new(dropped));
                Report::bump(&self.report.dropped, 1);
                self.owed = true;
            }
        }
        let mut gone: Vec<(u64, String)> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.dropped.is_some())
            .map(|(k, t)| (t.retired, k.clone()))
            .collect();
        if gone.len() > DROPPED_MAX {
            gone.sort();
            let over = gone.len() - DROPPED_MAX;
            for (_, key) in gone.into_iter().take(over) {
                self.tracks.remove(&key);
                Report::bump(&self.report.forgotten, 1);
            }
            aterm_log::warn!("session journal: sessions forgotten unsaid past the backstop");
        }
    }

    /// The update's drain is whole when every fenced session is `#handed`:
    /// then the chain's trailer, and nothing more.
    fn maybe_hand_off(&mut self) {
        if self.owed || !self.saw_all {
            return;
        }
        if self.targets.pending_targets() {
            self.owed = true;
            return;
        }
        let pending = self
            .tracks
            .values()
            .any(|t| t.in_store && t.fence.is_some() && !t.handed);
        if pending {
            return;
        }
        let t = self.clock.wall_ms();
        if let Some(mut line) = encode_frame(&Frame::End {
            why: EndWhy::Handoff,
            t,
        }) {
            line.push('\n');
            if self.write(line.as_bytes(), &[]) == line.len() {
                if let Some(seg) = self.seg.as_mut() {
                    let _ = seg.file.sync();
                }
                self.handed_done = true;
            }
        }
    }

    /// THE QUIT (ruling R14): drain until a pass has nothing left and every
    /// session is wholly journaled ([`Writer::quit_lines`]), then each open
    /// session's closes and `#fin why=quit`, all before `deadline` on this
    /// writer's clock and within [`FINISH_PASSES`]. Anything cut short, the
    /// drain unfinished included, writes no closes and no `#fin`: a reader
    /// then reads a crash after what the chain holds, which is what it was,
    /// never a clean end over records it never held. A writer backing off (a
    /// full disk) tries once more, now: the disk may have room again. A
    /// stopped one writes nothing, and its chain reads as a crash.
    pub(crate) fn finish(&mut self, deadline: Duration) {
        if matches!(self.state, State::Backoff { .. }) {
            self.state = State::Running;
        }
        if !matches!(self.state, State::Running) || self.handed_done || self.lock.is_none() {
            return;
        }
        let mut last = None;
        for _ in 0..FINISH_PASSES {
            if self.clock.mono() >= deadline || !matches!(self.state, State::Running) {
                return;
            }
            let wrote = self.pass();
            if !matches!(self.state, State::Running) {
                return;
            }
            if wrote == 0 && !self.owed {
                // Behind again (a producer appended, a lock was busy): one
                // more pass.
                last = self.quit_lines();
                if last.is_some() {
                    break;
                }
            }
        }
        let Some(mut buf) = last else {
            return;
        };
        if self.clock.mono() >= deadline {
            return;
        }
        if let Some(line) = encode_frame(&Frame::End {
            why: EndWhy::Quit,
            t: self.clock.wall_ms(),
        }) {
            buf.extend_from_slice(line.as_bytes());
            buf.push(b'\n');
        }
        if self.write(&buf, &[]) != buf.len() {
            return;
        }
        if let Some(seg) = self.seg.as_mut() {
            let _ = seg.file.sync();
        }
        if let Some(held) = self.lock.take() {
            self.fs.unlock_instance(&self.inst, held);
        }
        let _ = self.fs.sync_dir();
    }

    /// The quit's closes, only while every session is wholly journaled: each
    /// track not done holds its timeline, its cursor at its log's head in the
    /// space it last wrote, and its snap in this segment (so a reader places
    /// the closes); a retired session not done (its close or its loss not yet
    /// written) is behind too. `None` when one is behind or busy: closes and
    /// `#fin` now would end the chain over records it never held, read then
    /// as pruned ranges or not at all.
    fn quit_lines(&self) -> Option<Vec<u8>> {
        if self.targets.pending_targets() {
            return None;
        }
        let n = self.seg.as_ref()?.n;
        let mut buf = Vec::new();
        for t in self.tracks.values().filter(|t| !t.done) {
            let guard = match t.timeline.as_ref()?.try_lock() {
                Ok(g) => g,
                Err(TryLockError::Poisoned(p)) => p.into_inner(),
                Err(TryLockError::WouldBlock) => return None,
            };
            let log = guard.slog().log();
            let whole = t.cursor == Some(log.last_seq())
                && t.space == Some((log.base_ep(), log.ep()))
                && t.snapped == Some(n);
            if !whole {
                return None;
            }
            // On the session's own clock, so each close measures how long
            // its node lived.
            let closes = log.quit_closes(guard.slog().now());
            drop(guard);
            for rec in closes {
                if let Some(line) = encode_frame(&Frame::Rec {
                    sid: t.sid,
                    rec,
                    restated: false,
                }) {
                    buf.extend_from_slice(line.as_bytes());
                    buf.push(b'\n');
                }
            }
        }
        Some(buf)
    }

    /// RETENTION (ruling R9), under `prune.lock` (busy: skipped this time):
    /// only private files of the exact segment pattern count; the oldest are
    /// removed, through the pinned directory, until the journal's own files
    /// are within the byte budget AND the count budget
    /// (`journal::FILES_MAX`: segments and lock files, so however small its
    /// segments, one listing sees the whole journal): never a segment of
    /// another live instance, and of this writer's own only those it has
    /// rotated past; a vanished file counts as freed. The lock files of dead
    /// instances with no segment left go too. The directory is visited in
    /// one streaming pass of at most `journal::RETENTION_SCAN_MAX` names,
    /// holding only the journal's own.
    pub(crate) fn retention(&mut self) {
        let Ok(Some(_prune)) = self.fs.try_prune_lock() else {
            return;
        };
        let mut seg_names: Vec<(Inst, u32, String)> = Vec::new();
        let mut locks: Vec<(Inst, String)> = Vec::new();
        let scanned = self.fs.scan(journal::RETENTION_SCAN_MAX, &mut |name| {
            if let Some((inst, n)) = parse_segment_name(name) {
                seg_names.push((inst, n, name.to_string()));
            } else if let Some(inst) = parse_lock_name(name) {
                locks.push((inst, name.to_string()));
            }
        });
        if scanned.is_err() {
            return;
        }
        let mut segs: Vec<(u64, Inst, u32, String, fs::Meta)> = seg_names
            .into_iter()
            .filter_map(|(inst, n, name)| {
                let meta = self.fs.meta(&name).ok().filter(|m| m.private)?;
                Some((meta.mtime_ms, inst, n, name, meta))
            })
            .collect();
        let mut live: BTreeMap<Inst, bool> = BTreeMap::new();
        for inst in segs.iter().map(|s| &s.1).chain(locks.iter().map(|l| &l.0)) {
            if live.contains_key(inst) {
                continue;
            }
            let alive = *inst == self.inst
                || !matches!(self.fs.probe(&journal::lock_name(inst)), Liveness::Dead);
            live.insert(inst.clone(), alive);
        }
        let mut left: BTreeMap<Inst, usize> = BTreeMap::new();
        for s in &segs {
            *left.entry(s.1.clone()).or_insert(0) += 1;
        }
        let mut total: u64 = segs.iter().map(|s| s.4.size).sum();
        let mut count = segs.len() + locks.len();
        segs.sort_by(|a, b| (a.0, &a.1, a.2).cmp(&(b.0, &b.1, b.2)));
        let lock_of: BTreeMap<Inst, String> = locks.iter().cloned().collect();
        let mut locks_gone: std::collections::BTreeSet<Inst> = std::collections::BTreeSet::new();
        let current = self.seg.as_ref().map(|s| s.n);
        for (_, inst, n, name, meta) in &segs {
            if total <= self.limits.budget_bytes && count <= self.limits.files_max {
                break;
            }
            // Never another live instance's segment; of its own, this writer
            // removes only the ones it has rotated past (a process that runs
            // for weeks stays within the budget too).
            let mine = *inst == self.inst;
            if (mine && current == Some(*n)) || (!mine && live.get(inst).copied().unwrap_or(true)) {
                continue;
            }
            let removed = match self.fs.remove(name, *meta) {
                Ok(()) => {
                    Report::bump(&self.report.pruned, 1);
                    true
                }
                Err(e) => e.kind() == io::ErrorKind::NotFound,
            };
            if !removed {
                continue;
            }
            total = total.saturating_sub(meta.size);
            count = count.saturating_sub(1);
            let now_left = left.get_mut(inst).map_or(0, |k| {
                *k = k.saturating_sub(1);
                *k
            });
            // A dead instance's last segment gone: its lock file goes too.
            if now_left == 0
                && !mine
                && let Some(lock) = lock_of.get(inst)
                && self.remove_dead_lock(lock)
            {
                count = count.saturating_sub(1);
                locks_gone.insert(inst.clone());
            }
        }
        for (inst, name) in &locks {
            if live.get(inst).copied().unwrap_or(true)
                || locks_gone.contains(inst)
                || left.get(inst).copied().unwrap_or(0) > 0
            {
                continue;
            }
            let _ = self.remove_dead_lock(name);
        }
        let _ = self.fs.sync_dir();
    }

    /// Remove a dead instance's lock file `name`, if it is a private file.
    fn remove_dead_lock(&self, name: &str) -> bool {
        match self.fs.meta(name) {
            Ok(meta) if meta.private => self.fs.remove(name, meta).is_ok(),
            _ => false,
        }
    }
}

impl<T: Targets> Drop for Writer<T> {
    fn drop(&mut self) {
        if let Some(owner) = self.target_owner.take() {
            self.targets.end_targets(&owner);
        }
    }
}

/// Whether `errno` wrote nothing and may pass: EAGAIN, EWOULDBLOCK, EINTR.
fn is_transient(errno: i32) -> bool {
    errno == self::errno::EAGAIN || errno == self::errno::EWOULDBLOCK || errno == self::errno::EINTR
}

/// One `write(2)`, retried on EINTR at most [`EINTR_RETRIES`] times.
fn write_all_once(file: &mut dyn Appender, bytes: &[u8]) -> io::Result<usize> {
    let mut tries = 0;
    loop {
        match file.write(bytes) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted && tries < EINTR_RETRIES => {
                tries += 1;
            }
            other => return other,
        }
    }
}

impl crate::App {
    /// The journal writer's word for the owner, read once per memory sweep
    /// turn: a pause or a stop posts its `exceptions::Entry` (never an
    /// on-screen alert); a resume clears it.
    pub(crate) fn poll_session_journal(&mut self) {
        let Some((changes, attention)) = self.session_log_journal.attention() else {
            return;
        };
        if changes == self.session_log_journal_seen {
            return;
        }
        self.session_log_journal_seen = changes;
        use crate::message_reporters as mr;
        match attention {
            Attention::None => {
                self.except_clear(mr::SESSION_JOURNAL_PAUSED_KEY);
            }
            Attention::Paused(cause) => {
                self.except(&mr::session_journal_paused_entry(cause.word()));
            }
            Attention::Stopped(cause) => {
                self.except_clear(mr::SESSION_JOURNAL_PAUSED_KEY);
                self.except(&mr::session_journal_stopped_entry(cause.word()));
            }
        }
    }
}
