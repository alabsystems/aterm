// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE HEAD WATCH'S OWN LOOP, for a host that has none (gap #28, 2026-09-26). The window
//! rides the watch on its package loop's parks, between the bump, the index probe and its
//! own passes; a terminal session has no such loop, so on a Mac with no window open a new
//! Claude Code or Codex release waited for the index cadence of hours. [`Runner`] is the
//! same watch driven alone, one [`SLICE`] at a time, on the window's rules: the
//! [`HeadWatch::slice`] every host runs (the minute's cadence, the unreachable retry, the
//! wake grace and the re-offer leases are all the watch's own), no wait while a head is
//! due, one targeted `update <program> --head-watch` pass at a time and no head asked while
//! it runs, its end reported to the watch ([`HeadWatch::pass_ended`]), and `[packages]`
//! read live before every look. Its clocks, its park, its settings and its passes are
//! injected ([`Clock`], [`Settings`], [`PassLauncher`]), so a test steps it without
//! sleeping.
//!
//! THE HELD TOOLCHAIN MOVE, re-checked here too (2026-09-26): the seated session — the one
//! that holds the watch while no window does — looks at a Trust toolchain update held for
//! quiet, or a rustup view a flip left behind, every [`crate::quiet::RECHECK`] by the
//! window's own rule ([`crate::quiet::Recheck`]), and starts the whole unattended pass
//! (`update --defer-busy-flip`, the window's) once it is due. Before, only a window's park
//! looked, so with terminal sessions alone a flip held for quiet landed at the next pass
//! after its ceiling — whenever a session next started one — and the doctor's "by <time> at
//! the latest" held only while a window was open. Each look re-admits the seat itself
//! ([`HeadWatch::begin_session_update_look`]): a round of head checks used to be the only
//! thing that did, so on a store with no vendor program to watch no session sat and
//! nothing looked.
//!
//! THE INDEX HEAD HINT also runs here while a window is absent: one seated session
//! starts one detached near/far HEAD worker, using the store's shared lock and cooldown.
//! Its near positive wakes this loop before the far HEAD finishes, but this loop
//! re-admits its host claim and checks the verified floor and pass stamps before it
//! launches a whole update. A window that opened meanwhile owns that decision.
//!
//! [`run_host`] starts it on its own thread for a terminal session ([`Host::Session`]):
//! every interactive session starts one, the store's seat lets one of them watch, and each
//! thread ends with its process. That is the session's clean exit: the kernel releases the
//! claim, the next host takes the watch within a minute, and a pass the runner started —
//! detached, never its child — finishes on its own.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use super::{CADENCE, ConcurrentVendorGetFn, HeadWatch, Host};
use crate::store::Layout;

/// How long one slice of a runner's park sleeps before it looks again: the window's
/// package loop's own slice.
pub const SLICE: Duration = Duration::from_secs(5);

/// How long a runner follows a pass it started before it looks at the heads again anyway:
/// the pass's own bound on the store lock, and an hour for the install. A pass still
/// running then finishes on its own; the watch's lease offers the head again if the store
/// is still behind it.
const PASS_FOLLOW_LIMIT: Duration =
    Duration::from_secs(aterm_update_core::pkg_check::PASS_WAIT_LOCK_SECS + 60 * 60);

/// The clocks a runner reads and the park it sleeps in.
pub trait Clock: Send {
    /// The wall clock: the heads' schedule is on it.
    fn wall(&self) -> SystemTime;
    /// The monotonic clock, from any origin: a slice over which the wall clock outran it
    /// by more than [`super::WAKE_GAP`] is a wake.
    fn mono(&self) -> Duration;
    /// Park up to `slice` — sooner when a head in flight completes
    /// ([`HeadWatch::park_for_hint`]) or the runner's host stops it.
    fn park(&mut self, watch: &mut HeadWatch, slice: Duration);
}

/// The real clocks and the real park.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    fn mono(&self) -> Duration {
        self.origin.elapsed()
    }

    fn park(&mut self, watch: &mut HeadWatch, slice: Duration) {
        watch.park_for_hint(slice);
    }
}

/// `[packages]` as a runner reads it before every look.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gate {
    /// Automatic updates (`[packages] enabled`): off, the runner asks no head, starts no
    /// pass, and lets its claim go.
    pub on: bool,
    /// `[packages].exclude`: never watched here.
    pub exclude: Vec<String>,
}

/// Where a runner reads its [`Gate`].
pub trait Settings: Send {
    fn gate(&mut self) -> Gate;
}

impl<F: FnMut() -> Gate + Send> Settings for F {
    fn gate(&mut self) -> Gate {
        self()
    }
}

/// `aterm.toml`'s `[packages]`, re-read whenever the file changes — the window's live
/// switch, read the same way: a missing file is the defaults, and a file that exists but
/// cannot be read keeps the gate last read, because a transient failure is not an answer.
#[derive(Debug)]
pub struct LiveSettings {
    path: Option<PathBuf>,
    /// The file's `(mtime, length)` at the last read; `Some(None)` for an absent file,
    /// `None` before the first read.
    seen: Option<Option<(Option<SystemTime>, u64)>>,
    gate: Gate,
}

impl LiveSettings {
    /// The user's config file ([`crate::config::config_path`]), starting from `gate` —
    /// the table the host read at launch.
    #[must_use]
    pub fn new(gate: Gate) -> Self {
        Self::at(crate::config::config_path(), gate)
    }

    fn at(path: Option<PathBuf>, gate: Gate) -> Self {
        Self {
            path,
            seen: None,
            gate,
        }
    }
}

impl Settings for LiveSettings {
    fn gate(&mut self) -> Gate {
        let Some(path) = self.path.as_deref() else {
            return self.gate.clone();
        };
        let stamp = std::fs::metadata(path)
            .ok()
            .map(|m| (m.modified().ok(), m.len()));
        if self.seen != Some(stamp) {
            self.seen = Some(stamp);
            if let Some(cfg) = crate::config::load_live(path)
                && !cfg.unreadable
            {
                self.gate = Gate {
                    on: cfg.enabled(),
                    exclude: cfg.exclude().to_vec(),
                };
            }
        }
        self.gate.clone()
    }
}

/// A targeted pass a runner started.
pub trait RunningPass: Send {
    /// `None` while it runs; once it ended, whether its host saw it succeed — `true` where
    /// the host cannot see its exit, and the store then says whether the head was reached
    /// ([`HeadWatch::pass_ended`]).
    fn ended(&mut self) -> Option<bool>;
}

/// How a runner's host starts the targeted pass a moved head asks for.
pub trait PassLauncher: Send {
    /// Start `update <program>` through the head watch's door
    /// ([`crate::cli::HEAD_WATCH_FLAG`]).
    ///
    /// # Errors
    /// The pass could not be started.
    fn launch(&mut self, program: &'static str) -> io::Result<Box<dyn RunningPass>>;

    /// Start the whole unattended pass — `update` holding a busy toolchain's flip for quiet
    /// ([`crate::cli::DEFER_BUSY_FLIP_FLAG`]), the window's own — that a held toolchain move
    /// asks for once it is due ([`crate::quiet::due`]). A launcher that cannot start one
    /// says so, and the move waits for the next pass as it did before.
    ///
    /// # Errors
    /// The pass could not be started.
    fn launch_whole(&mut self) -> io::Result<Box<dyn RunningPass>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this host starts no whole pass",
        ))
    }

    /// Start a whole pass for an untrusted published-index hint. The build is
    /// only a pass-record/scheduling witness; the update still resolves and
    /// verifies the signed index itself.
    ///
    /// # Errors
    /// The pass could not be started.
    fn launch_index(&mut self, _build: u64) -> io::Result<Box<dyn RunningPass>> {
        self.launch_whole()
    }
}

/// A terminal session's passes: `exe lead… update <program>`, argv for argv the window's
/// targeted pass, started DETACHED and followed by pid.
#[derive(Clone, Debug)]
pub struct DetachedPasses {
    exe: PathBuf,
    lead: Vec<OsString>,
    progress_file: PathBuf,
}

impl DetachedPasses {
    /// Passes run as `exe lead… update <program> …` — `aterm pkg update …` from the one
    /// binary — reporting through `layout`'s progress file, which a window tails.
    #[must_use]
    pub fn new(exe: PathBuf, lead: Vec<OsString>, layout: &Layout) -> Self {
        Self {
            exe,
            lead,
            progress_file: layout.progress_file(),
        }
    }

    /// The pass's argv after `exe`: the lead, `update <program>`, the head watch's door,
    /// and the flags every scheduled pass carries
    /// ([`aterm_update_core::pkg_check::pass_flags`]). Pure for the test.
    #[must_use]
    pub fn args(&self, program: &str) -> Vec<OsString> {
        let mut args = self.lead.clone();
        args.push("update".into());
        args.push(program.into());
        args.push(crate::cli::HEAD_WATCH_FLAG.into());
        args.extend(aterm_update_core::pkg_check::pass_flags(Some(
            &self.progress_file,
        )));
        args
    }

    /// The whole pass's argv after `exe`: the lead, `update`, the unattended lanes' opt-in
    /// to hold a busy toolchain's flip ([`crate::cli::DEFER_BUSY_FLIP_FLAG`]), and the flags
    /// every scheduled pass carries — the window's whole pass, and the session's detached
    /// launch pass, argv for argv. Pure for the test.
    #[must_use]
    pub fn whole_args(&self) -> Vec<OsString> {
        self.whole_args_with_index(None)
    }

    /// The window's published-index pass argv: the untrusted build hint is
    /// recorded with the pass end even when its signed index lookup fails.
    #[must_use]
    pub fn index_args(&self, build: u64) -> Vec<OsString> {
        self.whole_args_with_index(Some(build))
    }

    fn whole_args_with_index(&self, build: Option<u64>) -> Vec<OsString> {
        let mut args = self.lead.clone();
        args.push("update".into());
        if let Some(build) = build {
            args.push(crate::cli::PUBLISHED_INDEX_HINT_FLAG.into());
            args.push(build.to_string().into());
        }
        args.push(crate::cli::DEFER_BUSY_FLIP_FLAG.into());
        args.extend(aterm_update_core::pkg_check::pass_flags(Some(
            &self.progress_file,
        )));
        args
    }
}

impl PassLauncher for DetachedPasses {
    fn launch(&mut self, program: &'static str) -> io::Result<Box<dyn RunningPass>> {
        spawn_detached(&self.exe, &self.args(program))
    }

    fn launch_whole(&mut self) -> io::Result<Box<dyn RunningPass>> {
        spawn_detached(&self.exe, &self.whole_args())
    }

    fn launch_index(&mut self, build: u64) -> io::Result<Box<dyn RunningPass>> {
        spawn_detached(&self.exe, &self.index_args(build))
    }
}

/// Start `exe args` DETACHED — stdio on `/dev/null`, its own process group, and never
/// this process's child — and follow it by its pid. The session reaps its shell by pid,
/// never `waitpid(-1)`, but a pass must also outlive the session that started it: so, as
/// the router's detached pass does, a `/bin/sh` middle process backgrounds it, prints its
/// pid and exits at once, and reaping that shell here re-parents the pass to launchd/init.
/// It names no spawner ([`crate::cli::SPAWNER_DETACHED`]): a waiter that read the
/// detaching shell's exit as its window going would stand aside instead of queueing.
#[cfg(unix)]
fn spawn_detached(exe: &Path, args: &[OsString]) -> io::Result<Box<dyn RunningPass>> {
    use std::os::unix::process::CommandExt as _;
    let out = std::process::Command::new("/bin/sh")
        .args(["-c", "\"$@\" </dev/null >/dev/null 2>&1 & echo $!", "sh"])
        .arg(exe)
        .args(args)
        .env(crate::cli::SPAWNER_PID_ENV, crate::cli::SPAWNER_DETACHED)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "the detaching shell {}",
            out.status
        )));
    }
    let pid = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|_| io::Error::other("the detaching shell named no pid"))?;
    Ok(Box::new(Detached { pid }))
}

/// Windows has no zombie to fear and no re-parenting: the pass is an ordinary child with
/// no console of ours, and its exit is read.
#[cfg(not(unix))]
fn spawn_detached(exe: &Path, args: &[OsString]) -> io::Result<Box<dyn RunningPass>> {
    let child = std::process::Command::new(exe)
        .args(args)
        .env(crate::cli::SPAWNER_PID_ENV, crate::cli::SPAWNER_DETACHED)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(Box::new(Owned(child)))
}

/// A detached pass, followed by its pid ([`crate::progress::pid_alive`]). Its exit status
/// is launchd's to read, not ours, so an end reads as `true` and the store decides.
#[cfg(unix)]
struct Detached {
    pid: u32,
}

#[cfg(unix)]
impl RunningPass for Detached {
    fn ended(&mut self) -> Option<bool> {
        (!crate::progress::pid_alive(self.pid)).then_some(true)
    }
}

#[cfg(not(unix))]
struct Owned(std::process::Child);

#[cfg(not(unix))]
impl RunningPass for Owned {
    fn ended(&mut self) -> Option<bool> {
        match self.0.try_wait() {
            Ok(Some(status)) => Some(status.success()),
            Ok(None) => None,
            Err(_) => Some(true),
        }
    }
}

/// Whether a held toolchain move is due at a Unix second ([`crate::quiet::due`]'s shape).
type HeldDue = dyn FnMut(&Layout, i64) -> Option<crate::quiet::DueFlip> + Send;

/// The store-scoped HEAD pair. The production function takes the index probe's
/// lock and shared cooldown; tests replace only its network answer.
type IndexProbe = dyn Fn(&Layout, &mut dyn FnMut(u64)) -> crate::index_probe::Probe + Send + Sync;

struct PendingIndex {
    worker: JoinHandle<crate::index_probe::Probe>,
    started_mono: Duration,
    started_at: Instant,
    near: Arc<AtomicU64>,
    done: Arc<AtomicBool>,
    completed_at: Arc<std::sync::OnceLock<Instant>>,
    /// A near answer already handled by the runner. A higher far answer may
    /// still ask for a pass after the first pass finishes.
    offered: u64,
}

#[derive(Clone, Copy, Debug)]
struct IndexAttempt {
    build: u64,
    since: Duration,
    /// Zero for a launch error, then one, two, three-or-more actual passes.
    passes: u8,
}

impl IndexAttempt {
    fn retry_after(self) -> Duration {
        match self.passes {
            0 => crate::index_probe::INTERVAL,
            1 => Duration::from_secs(5 * 60),
            2 => Duration::from_secs(15 * 60),
            _ => Duration::from_secs(60 * 60),
        }
    }

    fn holds(self, build: u64, now: Duration) -> bool {
        self.build >= build && now.saturating_sub(self.since) < self.retry_after()
    }
}

/// One network worker at a time, paced from completion (the shared stamp's
/// time), with a hint retained while a window owns the rendezvous.
struct IndexWatch {
    probe: Arc<IndexProbe>,
    eligible: Arc<dyn Fn(&Layout) -> bool + Send + Sync>,
    pending: Option<PendingIndex>,
    /// A failed local work check is cheap to repeat at the next park slice;
    /// it is not a completed network probe and must not start its cooldown.
    last_ineligible: Option<Duration>,
    last_completed: Option<Duration>,
    /// A sibling's fresh shared stamp can expire sooner (or, after an error,
    /// later) than one more local thirty-second interval.
    shared_retry_at: Option<Duration>,
    ready: u64,
    /// An incomplete or untrusted release cannot run a full pass every HEAD
    /// cooldown. Its retry lengthens from five to fifteen to sixty minutes;
    /// a newer build always bypasses it.
    last_attempt: Option<IndexAttempt>,
}

impl IndexWatch {
    fn new() -> Self {
        Self {
            probe: Arc::new(crate::index_probe::successor_reporting_near_with),
            eligible: Arc::new(|layout| {
                crate::index_probe::probes_this_source() && crate::cli::update_pass_has_work(layout)
            }),
            pending: None,
            last_ineligible: None,
            last_completed: None,
            shared_retry_at: None,
            ready: 0,
            last_attempt: None,
        }
    }

    fn due(&self, now: Duration) -> bool {
        self.pending.is_none()
            && self
                .last_ineligible
                .is_none_or(|last| now.saturating_sub(last) >= SLICE)
            && self.shared_retry_at.map_or_else(
                || {
                    self.last_completed
                        .is_none_or(|last| now.saturating_sub(last) >= crate::index_probe::INTERVAL)
                },
                |due| now >= due,
            )
    }

    fn start(&mut self, layout: &Layout, now: Duration) -> io::Result<()> {
        let layout = layout.clone();
        let probe = Arc::clone(&self.probe);
        let near = Arc::new(AtomicU64::new(0));
        let worker_near = Arc::clone(&near);
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = Arc::clone(&done);
        let completed_at = Arc::new(std::sync::OnceLock::new());
        let worker_completed_at = Arc::clone(&completed_at);
        let owner = std::thread::current();
        let started_at = Instant::now();
        let worker = std::thread::Builder::new()
            .name("atpkg-index-probe".into())
            .spawn(move || {
                struct Wake {
                    done: Arc<AtomicBool>,
                    completed_at: Arc<std::sync::OnceLock<Instant>>,
                    owner: std::thread::Thread,
                }
                impl Drop for Wake {
                    fn drop(&mut self) {
                        let _ = self.completed_at.set(Instant::now());
                        self.done.store(true, Ordering::Release);
                        self.owner.unpark();
                    }
                }
                let wake = Wake {
                    done: worker_done,
                    completed_at: worker_completed_at,
                    owner,
                };
                let mut on_near = |build| {
                    if worker_near.fetch_max(build, Ordering::Release) < build {
                        wake.owner.unpark();
                    }
                };
                probe(&layout, &mut on_near)
            })?;
        self.pending = Some(PendingIndex {
            worker,
            started_mono: now,
            started_at,
            near,
            done,
            completed_at,
            offered: 0,
        });
        // Pending owns the slot; harvest replaces this start time with the
        // worker's completion time before the next due check.
        self.last_completed = Some(now);
        self.shared_retry_at = None;
        Ok(())
    }

    fn harvest(&mut self, now: Duration) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let near = pending.near.load(Ordering::Acquire);
        if near > pending.offered {
            self.ready = self.ready.max(near);
        }
        if !pending.done.load(Ordering::Acquire) {
            return;
        }
        let pending = self.pending.take().expect("checked pending");
        let completed = pending
            .completed_at
            .get()
            .and_then(|at| at.checked_duration_since(pending.started_at))
            .map(|elapsed| pending.started_mono.saturating_add(elapsed))
            .unwrap_or(now);
        self.last_completed = Some(completed);
        match pending.worker.join() {
            Ok(crate::index_probe::Probe::Published(build)) if build > pending.offered => {
                self.ready = self.ready.max(build);
            }
            Ok(crate::index_probe::Probe::Suppressed(remaining)) => {
                self.shared_retry_at = Some(completed.saturating_add(remaining));
            }
            _ => {}
        }
    }

    fn offered(&mut self, build: u64) {
        self.ready = 0;
        if let Some(pending) = &mut self.pending {
            pending.offered = pending.offered.max(build);
        }
    }

    fn note_attempt(&mut self, build: u64, now: Duration, started: bool) {
        let passes = if started {
            self.last_attempt
                .filter(|prior| prior.build == build)
                .map_or(1, |prior| prior.passes.saturating_add(1).min(3))
        } else {
            0
        };
        self.last_attempt = Some(IndexAttempt {
            build,
            since: now,
            passes,
        });
    }
}

/// The pass a runner follows.
struct Following {
    /// The program a moved head's pass updates; `None` for the whole pass a held toolchain
    /// move asked for.
    program: Option<&'static str>,
    index_build: Option<u64>,
    pass: Box<dyn RunningPass>,
    /// [`Clock::mono`] when it started.
    since: Duration,
}

/// A host's head watch, driven alone (the module doc).
pub struct Runner {
    layout: Layout,
    watch: HeadWatch,
    get: Arc<ConcurrentVendorGetFn<'static>>,
    clock: Box<dyn Clock>,
    settings: Box<dyn Settings>,
    launcher: Box<dyn PassLauncher>,
    log: Box<dyn FnMut(&str) + Send>,
    stop: Arc<AtomicBool>,
    /// The gate as the last look read it.
    on: bool,
    /// Moved heads waiting for their pass, in the watch's order.
    queue: VecDeque<&'static str>,
    following: Option<Following>,
    /// The re-check of a held toolchain move ([`crate::quiet::Recheck`]), and the instant
    /// its monotonic clock counts from (the runner's [`Clock::mono`] rides on it).
    held: crate::quiet::Recheck,
    held_origin: Instant,
    /// Whether a held toolchain move is due at a Unix second ([`crate::quiet::due`]).
    held_due: Box<HeldDue>,
    index: IndexWatch,
    /// One deadline for both update looks. A standing second tab waits a
    /// minute for the seat; a seated session behind a window retries a slice.
    admit_after: Duration,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("layout", &self.layout)
            .field("watch", &self.watch)
            .field("on", &self.on)
            .field("queue", &self.queue)
            .field("following", &self.following.as_ref().map(|f| f.program))
            .field("held", &self.held)
            .finish_non_exhaustive()
    }
}

impl Runner {
    /// A runner over `layout` for the watch `heads` ([`super::for_host`]), on `clock`,
    /// reading `settings` before every look, starting passes through `launcher`, and
    /// saying each note and each pass through `log` — the host's log, never a terminal.
    #[must_use]
    pub fn new(
        layout: Layout,
        heads: (HeadWatch, Arc<ConcurrentVendorGetFn<'static>>),
        clock: Box<dyn Clock>,
        settings: Box<dyn Settings>,
        launcher: Box<dyn PassLauncher>,
        log: Box<dyn FnMut(&str) + Send>,
    ) -> Self {
        let (watch, get) = heads;
        Self {
            layout,
            watch,
            get,
            clock,
            settings,
            launcher,
            log,
            stop: Arc::new(AtomicBool::new(false)),
            on: true,
            queue: VecDeque::new(),
            following: None,
            held: crate::quiet::Recheck::default(),
            held_origin: Instant::now(),
            held_due: Box::new(crate::quiet::due),
            index: IndexWatch::new(),
            admit_after: Duration::ZERO,
        }
    }

    /// This runner with `due` answering whether a held toolchain move is due — a test's
    /// record, in place of the store's ([`crate::quiet::due`]).
    #[must_use]
    pub fn with_held_move_due(
        mut self,
        due: impl FnMut(&Layout, i64) -> Option<crate::quiet::DueFlip> + Send + 'static,
    ) -> Self {
        self.held_due = Box::new(due);
        self
    }

    /// Replace the store-scoped network probe for a deterministic test. Its
    /// near callback is called on the worker thread, as in production.
    #[cfg(test)]
    fn with_index_probe(
        mut self,
        probe: impl Fn(&Layout, &mut dyn FnMut(u64)) -> crate::index_probe::Probe
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.index.probe = Arc::new(probe);
        self.index.eligible = Arc::new(|_| true);
        self
    }

    #[cfg(test)]
    fn without_index_probe(mut self) -> Self {
        self.index.eligible = Arc::new(|_| false);
        self
    }

    /// Run [`Self::step`] until the host stops it ([`HostHandle::stop`]); the claim is
    /// released as the runner drops.
    pub fn run(mut self) {
        while !self.stop.load(Ordering::Acquire) {
            self.step();
        }
    }

    /// [`Self::run`] on its own thread.
    ///
    /// # Errors
    /// The thread could not be spawned.
    pub fn spawn(self) -> io::Result<HostHandle> {
        let stop = Arc::clone(&self.stop);
        let thread = std::thread::Builder::new()
            .name("atpkg-head-watch".into())
            .spawn(move || self.run())?;
        Ok(HostHandle { stop, thread })
    }

    /// ONE SLICE, the window's park slice alone: park (no wait while a head is due and
    /// nothing is in flight), read `[packages]`, follow the pass in flight — no head is
    /// asked meanwhile, as none is while the window's lane runs a pass, though a round the
    /// pass interrupted is harvested as its answers land — or start the next
    /// one a moved head queued, else the watch's own [`HeadWatch::slice`] over the slice's
    /// two clocks.
    pub fn step(&mut self) {
        let wall_before = self.clock.wall();
        let mono_before = self.clock.mono();
        let idle = self.following.is_none() && self.queue.is_empty();
        // A refused claim or a round started moves every head off due, so a zero wait is
        // one look, never a spin.
        let wait = if self.on
            && idle
            && mono_before >= self.admit_after
            && !self.watch.has_pending()
            && self.watch.due(wall_before)
        {
            Duration::ZERO
        } else {
            SLICE
        };
        self.clock.park(&mut self.watch, wait);
        if self.stop.load(Ordering::Acquire) {
            return;
        }
        let gate = self.settings.gate();
        let wall = self.clock.wall();
        if !gate.on {
            self.stand_down(wall);
            return;
        }
        if !self.on {
            (self.log)("vendor head watch: Automatic updates turned on \u{2014} watching again");
            self.on = true;
            self.admit_after = Duration::ZERO;
        }
        self.watch.set_exclude(&gate.exclude);
        // THE ROUND A PASS INTERRUPTED is harvested as it lands (review, 2026-09-26): the
        // watch hands back a moved head while its peer's GET is still in flight, and the
        // pass starts at once. Left unharvested, that GET kept the session's round — the
        // rendezvous EXCLUSIVE, a window refused for the whole pass — and its completion
        // cut every park short, so the thread spun a core until the pass ended. With a head
        // in flight the watch starts no round, so this asks nothing.
        if self.following.is_some() && self.watch.has_pending() {
            self.harvest(wall);
        }
        if !self.follow(wall) {
            return;
        }
        if let Some(program) = self.queue.pop_front() {
            self.launch(program, wall);
            return;
        }
        // A HELD TOOLCHAIN MOVE, looked at as the window's park looks at it — by the seated
        // session only, and never with a pass in flight or queued.
        if self.check_held_move(wall) {
            return;
        }
        if self.check_index(wall) {
            return;
        }
        // A refused seat/window claim already chose its next admission time.
        // The vendor slice must not make a second flock attempt in this park,
        // or keep a due head spinning the runner's zero-wait branch.
        if self.clock.mono() < self.admit_after && !self.watch.has_pending() {
            return;
        }
        let moved = self.watch.slice(
            &self.layout,
            wall,
            wall.duration_since(wall_before).ok(),
            self.clock.mono().saturating_sub(mono_before),
            &self.get,
            false,
        );
        self.flush_notes();
        if let Some(programs) = moved {
            self.queue.extend(programs);
            if let Some(program) = self.queue.pop_front() {
                self.launch(program, wall);
            }
        }
    }

    /// Look for a held toolchain move once per re-check interval. Re-admit through
    /// the head watch's rendezvous before reading the record: a window may have
    /// started watching after this session's last head round. Keep that claim
    /// through the pass launch, then let the window take over.
    fn check_held_move(&mut self, wall: SystemTime) -> bool {
        let now = self.held_origin + self.clock.mono();
        if !self.held.look_due(now) {
            return false;
        }
        let owns_watch = self.begin_update_look(wall);
        if !owns_watch {
            // Only the nonblocking rendezvous was read. The common admission
            // deadline makes a window retry on the next slice and another
            // session retry after the seat's minute cadence.
            return false;
        }
        let unix = wall
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let layout = &self.layout;
        let due = &mut self.held_due;
        let wake = self.held.wake(now, || due(layout, unix));
        if wake {
            self.launch_whole();
        }
        self.watch.end_session_update_look();
        wake
    }

    fn begin_update_look(&mut self, wall: SystemTime) -> bool {
        // A shared, unhosted watch exists only as the negative control for the
        // host rendezvous. It has no session claim to pace; its vendor slice
        // must keep running so the test catches a missing host gate.
        if self.watch.host != Some(Host::Session) {
            return false;
        }
        let now = self.clock.mono();
        if now < self.admit_after {
            return false;
        }
        let admitted = self.watch.begin_session_update_look(&self.layout, wall);
        self.flush_notes();
        self.admit_after = if admitted {
            now
        } else if self.watch.session_behind_window() {
            now.saturating_add(SLICE)
        } else {
            now.saturating_add(CADENCE)
        };
        admitted
    }

    /// Start only one HEAD pair for the seated session. Its near answer wakes
    /// the parked runner, but the network worker never launches a pass: the
    /// runner must re-admit after a window may have taken the watch.
    fn check_index(&mut self, wall: SystemTime) -> bool {
        let now = self.clock.mono();
        self.index.harvest(now);
        if self.index.ready > 0 {
            let build = self.index.ready;
            if crate::index_probe::verified_floor(&self.layout) >= build {
                self.index.offered(build);
            } else if !self
                .index
                .last_attempt
                .is_some_and(|attempt| attempt.holds(build, now))
            {
                let owns_watch = self.begin_update_look(wall);
                if owns_watch {
                    let unix = wall
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
                    let stamps = crate::status::pass_stamps(&self.layout, unix);
                    if stamps.last_index_build >= build {
                        self.index.offered(build);
                    } else if !stamps.attempted_recently(unix)
                        || stamps.completed_older_index_pass(build)
                    {
                        match self.launcher.launch_index(build) {
                            Ok(pass) => {
                                self.index.note_attempt(build, now, true);
                                self.index.offered(build);
                                self.following = Some(Following {
                                    program: None,
                                    index_build: Some(build),
                                    pass,
                                    since: now,
                                });
                                (self.log)(&format!(
                                    "atpkg index {build} appeared; started a signed update pass"
                                ));
                            }
                            Err(error) => {
                                self.index.note_attempt(build, now, false);
                                (self.log)(&format!(
                                    "atpkg index {build} appeared but its update pass could not \
                                     start: {error}"
                                ));
                            }
                        }
                    }
                    self.watch.end_session_update_look();
                    if self.following.is_some() {
                        return true;
                    }
                }
            }
        }
        // Keep discovering higher builds while an older hint waits behind an
        // in-flight pass or its spacing. A retained 43 must not hide a new 44.
        if self.index.due(now) {
            let owns_watch = self.begin_update_look(wall);
            if owns_watch {
                if !(self.index.eligible)(&self.layout) {
                    // No HEAD was sent. Recheck cheap local eligibility after
                    // one park: a managed install may make this source useful.
                    self.index.last_ineligible = Some(now);
                    self.watch.end_session_update_look();
                    return false;
                }
                self.index.last_ineligible = None;
                if let Err(error) = self.index.start(&self.layout, now) {
                    self.index.last_completed = Some(now);
                    (self.log)(&format!("atpkg index probe could not start: {error}"));
                }
                self.watch.end_session_update_look();
            }
        }
        false
    }

    /// Start the whole pass a due held toolchain move asks for, and follow it like a head's.
    fn launch_whole(&mut self) {
        match self.launcher.launch_whole() {
            Ok(pass) => {
                (self.log)(
                    "vendor head watch: started the update pass for the Trust toolchain move \
                     held for quiet \u{2014} nothing is using the toolchain now, or its wait \
                     reached the ceiling",
                );
                self.following = Some(Following {
                    program: None,
                    index_build: None,
                    pass,
                    since: self.clock.mono(),
                });
            }
            Err(error) => (self.log)(&format!(
                "vendor head watch: could not start the update pass for the held Trust \
                 toolchain move: {error} \u{2014} the next update pass makes it"
            )),
        }
    }

    /// Automatic updates off: no head asked, no pass started, and the claim let go — a
    /// round in flight is harvested first and never acted on; a pass already running is
    /// still followed, so its end is reported.
    fn stand_down(&mut self, wall: SystemTime) {
        if self.on {
            (self.log)(
                "vendor head watch: Automatic updates turned off \u{2014} this session's \
                 watch stands down",
            );
            self.on = false;
        }
        self.queue.clear();
        if self.watch.has_pending() {
            let _ = self.watch.check_pending(&self.layout, wall, &self.get);
            self.flush_notes();
        }
        self.watch.release_host();
        let _ = self.follow(wall);
    }

    /// Follow the pass in flight: `true` once none is — it ended and the watch was told,
    /// or it outran [`PASS_FOLLOW_LIMIT`] and is left to finish on its own.
    fn follow(&mut self, wall: SystemTime) -> bool {
        let Some(following) = self.following.as_mut() else {
            return true;
        };
        let ended = following.pass.ended();
        let followed = self.clock.mono().saturating_sub(following.since);
        let Some(program) = following.program else {
            // A whole pass asked for by a held flip or an index hint. The
            // store's records say how it went; the watch has no vendor head in it.
            return match ended {
                Some(_) => {
                    let index_build = following.index_build;
                    self.following = None;
                    if let Some(build) = index_build {
                        (self.log)(&format!("atpkg index {build} update pass ended"));
                    } else {
                        (self.log)(
                            "vendor head watch: the update pass for the held Trust toolchain \
                             move ended",
                        );
                    }
                    true
                }
                None if followed >= PASS_FOLLOW_LIMIT => {
                    self.following = None;
                    true
                }
                None => false,
            };
        };
        match ended {
            Some(ran_ok) => {
                self.following = None;
                self.watch.pass_ended(&self.layout, program, ran_ok, wall);
                let line = if self.watch.head_reached(&self.layout, program) {
                    format!("vendor head watch: the {program} update pass reached the head")
                } else {
                    format!(
                        "vendor head watch: the {program} update pass ended short of the \
                         head \u{2014} the watch offers it again"
                    )
                };
                (self.log)(&line);
                true
            }
            None if followed >= PASS_FOLLOW_LIMIT => {
                self.following = None;
                (self.log)(&format!(
                    "vendor head watch: the {program} update pass is still running after {} \
                     min \u{2014} the watch looks at the heads again and leaves it to finish",
                    followed.as_secs() / 60
                ));
                true
            }
            None => false,
        }
    }

    fn launch(&mut self, program: &'static str, wall: SystemTime) {
        match self.launcher.launch(program) {
            Ok(pass) => {
                (self.log)(&format!(
                    "vendor head watch: started the {program} update pass for its vendor's \
                     new head"
                ));
                self.following = Some(Following {
                    program: Some(program),
                    index_build: None,
                    pass,
                    since: self.clock.mono(),
                });
            }
            Err(error) => {
                (self.log)(&format!(
                    "vendor head watch: could not start the {program} update pass: {error}"
                ));
                self.watch.note_pass_result(program, false, wall);
            }
        }
    }

    /// Harvest the round in flight — never start one: [`HeadWatch::check_pending`] with a
    /// head pending only collects answers — and queue a moved head behind the pass that
    /// is running, once.
    fn harvest(&mut self, wall: SystemTime) {
        let moved = self.watch.check_pending(&self.layout, wall, &self.get);
        self.flush_notes();
        for program in moved {
            let running = self
                .following
                .as_ref()
                .is_some_and(|following| following.program == Some(program));
            if !running && !self.queue.contains(&program) {
                self.queue.push_back(program);
            }
        }
    }

    fn flush_notes(&mut self) {
        for line in self.watch.take_notes() {
            (self.log)(&line);
        }
    }
}

/// A runner on its own thread ([`Runner::spawn`]). Dropped, it leaves the runner running
/// for the rest of the process — what a terminal session wants: the watch ends with it.
#[derive(Debug)]
pub struct HostHandle {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl HostHandle {
    /// Stop the runner within one slice — a GET in flight finishes on its own worker —
    /// and wait for it; its claim is free when this returns.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Release);
        self.thread.thread().unpark();
        let _ = self.thread.join();
    }
}

/// THE HEAD WATCH FOR A HOST WITH NO LOOP OF ITS OWN — a terminal session (gap #28): the
/// watch a session may run ([`super::for_host`] as [`Host::Session`]) on the real clocks,
/// reading `aterm.toml`'s `[packages]` live from `gate` (the table the host read at
/// launch), starting detached passes as `exe lead… update <program> …`, and saying its
/// notes through `log`. `None` where the lane would reach no vendor (the manager off, a
/// `dir:` registry). The thread runs until [`HostHandle::stop`] or the process exits.
#[must_use]
pub fn run_host(
    layout: &Layout,
    gate: Gate,
    exe: PathBuf,
    lead: Vec<OsString>,
    log: impl FnMut(&str) + Send + 'static,
) -> Option<io::Result<HostHandle>> {
    let heads = super::for_host(Host::Session, &gate.exclude)?;
    let runner = Runner::new(
        layout.clone(),
        heads,
        Box::new(SystemClock::new()),
        Box::new(LiveSettings::new(gate)),
        Box::new(DetachedPasses::new(exe, lead, layout)),
        Box::new(log),
    );
    Some(runner.spawn())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU64;

    use super::super::tests::{install, layout};
    use super::super::{AFTER_WAKE, CADENCE, FAILED_PASS_RETRY, Watcher, watcher};
    use super::*;
    use crate::flow::VendorGet;

    const CLAUDE_HEAD: &str = "https://downloads.claude.ai/claude-code-releases/latest";

    /// The two clocks a test moves by hand: every park advances both by its slice, and a
    /// sleep the next park takes moves the wall clock alone.
    #[derive(Clone, Default)]
    struct Time {
        wall_ms: Arc<AtomicU64>,
        mono_ms: Arc<AtomicU64>,
        nap_ms: Arc<AtomicU64>,
    }

    impl Time {
        fn wall(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
                + Duration::from_secs(1_790_000_000)
                + Duration::from_millis(self.wall_ms.load(Ordering::SeqCst))
        }

        fn advance(&self, by: Duration) {
            let ms = u64::try_from(by.as_millis()).unwrap();
            self.wall_ms.fetch_add(ms, Ordering::SeqCst);
            self.mono_ms.fetch_add(ms, Ordering::SeqCst);
        }

        /// The machine sleeps through the next park: the wall clock moves, the monotonic
        /// one does not.
        fn sleep_in_next_park(&self, by: Duration) {
            let ms = u64::try_from(by.as_millis()).unwrap();
            self.nap_ms.store(ms, Ordering::SeqCst);
        }
    }

    struct FakeClock(Time);

    impl Clock for FakeClock {
        fn wall(&self) -> SystemTime {
            self.0.wall()
        }

        fn mono(&self) -> Duration {
            Duration::from_millis(self.0.mono_ms.load(Ordering::SeqCst))
        }

        /// A head in flight is waited for on the real clock — its worker's completion
        /// unparks this thread; the bound is only a backstop — then the slice passes.
        fn park(&mut self, watch: &mut HeadWatch, slice: Duration) {
            if watch.has_pending() {
                watch.park_for_hint(Duration::from_secs(30));
            }
            self.0.advance(slice);
            let nap = self.0.nap_ms.swap(0, Ordering::SeqCst);
            self.0.wall_ms.fetch_add(nap, Ordering::SeqCst);
        }
    }

    /// The vendors every host in a test reads, and who asked them.
    #[derive(Default)]
    struct Vendors {
        claude: Mutex<String>,
        /// Every GET, by the asking host's label.
        gets: Mutex<Vec<&'static str>>,
        /// A GET waits while this is set, until it is cleared.
        hold: Mutex<bool>,
        released: std::sync::Condvar,
    }

    impl Vendors {
        fn new(claude: &str) -> Arc<Self> {
            let vendors = Self::default();
            *vendors.claude.lock().unwrap() = claude.to_string();
            Arc::new(vendors)
        }

        fn count(&self, host: &str) -> usize {
            self.gets
                .lock()
                .unwrap()
                .iter()
                .filter(|h| **h == host)
                .count()
        }

        fn release(&self) {
            *self.hold.lock().unwrap() = false;
            self.released.notify_all();
        }

        /// The GET `host` reads the vendors through.
        fn get(self: &Arc<Self>, host: &'static str) -> Arc<ConcurrentVendorGetFn<'static>> {
            let vendors = Arc::clone(self);
            Arc::new(move |program: &str, url: &str, _cap, _etag: Option<&str>| {
                assert_eq!((program, url), ("claude", CLAUDE_HEAD));
                vendors.gets.lock().unwrap().push(host);
                let held = vendors.hold.lock().unwrap();
                let (held, _) = vendors
                    .released
                    .wait_timeout_while(held, Duration::from_secs(30), |held| *held)
                    .unwrap();
                assert!(!*held, "the test released the held GET");
                drop(held);
                Ok(VendorGet::Body {
                    bytes: vendors.claude.lock().unwrap().clone().into_bytes(),
                    etag: None,
                    effective_url: url.to_string(),
                })
            })
        }
    }

    /// Passes a test ends by hand.
    #[derive(Clone, Default)]
    struct Passes {
        launched: Arc<Mutex<Vec<&'static str>>>,
        /// The pass in flight ends when this is set.
        end: Arc<AtomicBool>,
    }

    struct FakePass(Arc<AtomicBool>);

    impl RunningPass for FakePass {
        fn ended(&mut self) -> Option<bool> {
            self.0.swap(false, Ordering::SeqCst).then_some(true)
        }
    }

    impl PassLauncher for Passes {
        fn launch(&mut self, program: &'static str) -> io::Result<Box<dyn RunningPass>> {
            self.launched.lock().unwrap().push(program);
            Ok(Box::new(FakePass(Arc::clone(&self.end))))
        }

        fn launch_whole(&mut self) -> io::Result<Box<dyn RunningPass>> {
            self.launched.lock().unwrap().push(WHOLE);
            Ok(Box::new(FakePass(Arc::clone(&self.end))))
        }
    }

    /// What [`Passes`] records for the whole pass a held toolchain move asks for.
    const WHOLE: &str = "<the whole pass>";

    impl Passes {
        fn launched(&self) -> Vec<&'static str> {
            self.launched.lock().unwrap().clone()
        }
    }

    /// A session runner over `l` on `time`, reading the vendors as `host`, with the gate
    /// `on` reads.
    fn session(
        l: &Layout,
        time: &Time,
        vendors: &Arc<Vendors>,
        host: &'static str,
        passes: &Passes,
        on: &Arc<AtomicBool>,
    ) -> Runner {
        let on = Arc::clone(on);
        Runner::new(
            l.clone(),
            (
                HeadWatch::hosted(&[String::from("codex")], Host::Session),
                vendors.get(host),
            ),
            Box::new(FakeClock(time.clone())),
            Box::new(move || Gate {
                on: on.load(Ordering::SeqCst),
                exclude: vec![String::from("codex")],
            }),
            Box::new(passes.clone()),
            Box::new(|_: &str| {}),
        )
        .without_index_probe()
    }

    fn on() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(true))
    }

    /// The window's own slice over the same clock, as its package loop runs it.
    fn window_slice(
        window: &mut HeadWatch,
        l: &Layout,
        time: &Time,
        get: &Arc<ConcurrentVendorGetFn<'static>>,
    ) {
        if window.has_pending() {
            window.park_for_hint(Duration::from_secs(30));
        }
        let moved = window.slice(l, time.wall(), Some(SLICE), SLICE, get, false);
        assert!(moved.is_none(), "the head in these scenes has not moved");
    }

    /// GAP #28, THE HEADLINE: with no window open a terminal session watches the heads at
    /// the window's cadence; a window that opens takes the watch over — the session asks
    /// nothing while it is up — and when it closes the session watches again within a
    /// minute. The NEGATIVE CONTROL runs the same scene with a watch no host runs in the
    /// session's place: it goes on asking beside the window, so the claim, not the shared
    /// per-vendor schedule, is what keeps the session out.
    #[test]
    fn a_session_watches_only_while_no_window_does() {
        for hosted in [true, false] {
            let l = layout(&format!("session-vs-window-{hosted}"));
            install(&l, "claude", "2.1.280");
            let time = Time::default();
            let vendors = Vendors::new("2.1.280");
            let passes = Passes::default();
            let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
            if !hosted {
                runner.watch = HeadWatch::shared(&[String::from("codex")]);
            }
            for _ in 0..3 {
                runner.step();
            }
            assert_eq!(vendors.count("session"), 1, "the session asked at once");
            if hosted {
                assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
            }

            // A window opens and watches; the session steps beside it for five minutes,
            // each slice ahead of the window, so a session free to ask wins every minute.
            let window_get = vendors.get("window");
            let mut window = HeadWatch::hosted(&[String::from("codex")], Host::Window);
            window_slice(&mut window, &l, &time, &window_get);
            let session_before = vendors.count("session");
            for _ in 0..(5 * 60 / SLICE.as_secs()) {
                runner.step();
                window_slice(&mut window, &l, &time, &window_get);
            }
            if !hosted {
                assert!(
                    vendors.count("session") > session_before,
                    "negative control: a watch no host runs goes on asking beside the window"
                );
                let _ = std::fs::remove_dir_all(&l.prefix);
                continue;
            }
            assert_eq!(watcher(&l), Watcher::Window);
            assert_eq!(
                vendors.count("session"),
                session_before,
                "the session asked nothing while the window watched"
            );
            assert!(
                (4..=6).contains(&vendors.count("window")),
                "the window asked once a minute: {}",
                vendors.count("window")
            );

            // The window closes: the session watches again within a minute and a slice.
            drop(window);
            let closed = time.wall();
            while vendors.count("session") == session_before {
                assert!(
                    time.wall() <= closed + CADENCE + SLICE * 2,
                    "the session took the watch back within a minute"
                );
                runner.step();
            }
            assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
            let _ = std::fs::remove_dir_all(&l.prefix);
        }
    }

    /// A WINDOW THAT OPENS MID-ROUND waits for the session's round, not for good: its
    /// check is refused while the session's GET is in flight — no GET of its own — and
    /// asked again a slice later, when it takes the watch and the session stands by.
    #[test]
    fn a_window_opening_mid_round_waits_one_round_then_watches() {
        let l = layout("mid-round");
        install(&l, "claude", "2.1.280");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
        *vendors.hold.lock().unwrap() = true;
        runner.step();
        assert_eq!(
            runner.watch.pending_count(),
            1,
            "the session's round is in flight"
        );
        let window_get = vendors.get("window");
        let mut window = HeadWatch::hosted(&[String::from("codex")], Host::Window);
        window_slice(&mut window, &l, &time, &window_get);
        assert_eq!(
            window.pending_count(),
            0,
            "refused while the round holds the watch"
        );
        assert!(
            !window.due(time.wall()),
            "and not asked again at every slice"
        );
        assert!(window.due(time.wall() + super::super::BUSY_RETRY));
        vendors.release();
        runner.step();
        assert_eq!(runner.watch.pending_count(), 0, "the round was harvested");
        time.advance(super::super::BUSY_RETRY);
        window_slice(&mut window, &l, &time, &window_get);
        assert_eq!(watcher(&l), Watcher::Window);
        let asked = vendors.count("session");
        for _ in 0..(3 * 60 / SLICE.as_secs()) {
            window_slice(&mut window, &l, &time, &window_get);
            runner.step();
        }
        assert_eq!(vendors.count("session"), asked, "the session stands by");
        assert!(vendors.count("window") >= 2);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// EXACTLY ONE SESSION WATCHES: two sessions on one store share the seat — only one
    /// ever asks — and when the watcher's process goes (its runner dropped: the kernel
    /// releases a dead process's locks the same way) the other takes the seat within a
    /// minute and asks.
    #[test]
    fn one_session_watches_and_the_next_takes_the_seat_when_it_goes() {
        let l = layout("two-sessions");
        install(&l, "claude", "2.1.280");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let mut first = session(&l, &time, &vendors, "first", &passes, &on());
        let mut second = session(&l, &time, &vendors, "second", &passes, &on());
        // Both park on the one clock, so each pair of steps is up to two slices.
        let start = time.wall();
        while time.wall() < start + Duration::from_secs(5 * 60) {
            first.step();
            second.step();
        }
        assert!(
            (5..=6).contains(&vendors.count("first")),
            "the seat holder asked each minute"
        );
        assert_eq!(vendors.count("second"), 0, "the other stood by");
        assert!(
            second.watch.session_admission_attempts() <= 6,
            "a second tab tries the seat at most once a minute, not every slice: {}",
            second.watch.session_admission_attempts()
        );
        drop(first);
        let gone = time.wall();
        while vendors.count("second") == 0 {
            assert!(
                time.wall() <= gone + CADENCE + SLICE * 2,
                "the next session took the seat within a minute"
            );
            second.step();
        }
        assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A MOVED HEAD, THE WINDOW'S WAY: one targeted pass, no head asked while it runs (not
    /// for minutes), its end reported — a pass that reached the head is not offered again;
    /// one that ended short gets the one short retry five minutes on, then the hourly
    /// lease.
    #[test]
    fn a_moved_head_runs_one_pass_and_its_end_is_reported() {
        let l = layout("moved");
        install(&l, "claude", "2.1.280");
        let time = Time::default();
        let vendors = Vendors::new("2.1.281");
        let passes = Passes::default();
        let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
        while passes.launched().is_empty() {
            assert!(
                time.wall() < Time::default().wall() + CADENCE,
                "launched at once"
            );
            runner.step();
        }
        assert_eq!(passes.launched(), ["claude"]);
        let asked = vendors.count("session");
        for _ in 0..(10 * 60 / SLICE.as_secs()) {
            runner.step();
        }
        assert_eq!(
            vendors.count("session"),
            asked,
            "no head asked while the pass runs"
        );
        assert_eq!(passes.launched(), ["claude"], "and no second pass");

        // It ends short of the head (the store still holds 2.1.280): offered again after
        // the short retry, and not before.
        passes.end.store(true, Ordering::SeqCst);
        let ended = time.wall();
        runner.step();
        assert!(runner.following.is_none(), "the end was read");
        while passes.launched().len() < 2 {
            assert!(
                time.wall() <= ended + FAILED_PASS_RETRY + CADENCE,
                "the short retry came"
            );
            runner.step();
        }
        assert!(
            time.wall() >= ended + FAILED_PASS_RETRY,
            "not before its five minutes"
        );

        // The retry reaches the head: reported, and never offered again, hours on.
        install(&l, "claude", "2.1.281");
        passes.end.store(true, Ordering::SeqCst);
        for _ in 0..(2 * 3600 / SLICE.as_secs()) {
            runner.step();
        }
        assert_eq!(passes.launched(), ["claude", "claude"]);
        assert!(
            vendors.count("session") > asked + 100,
            "the watch went on each minute"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE WAKE, THE WINDOW'S RULE: a sleep that carries the wall clock an hour past a due
    /// head asks it after the network's grace, not in the slice the machine woke in.
    #[test]
    fn a_wake_asks_the_heads_after_the_grace() {
        let l = layout("wake");
        install(&l, "claude", "2.1.280");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let mut runner = session(&l, &time, &vendors, "session", &passes, &on());
        for _ in 0..3 {
            runner.step();
        }
        let asked = vendors.count("session");
        assert_eq!(asked, 1);
        time.sleep_in_next_park(Duration::from_secs(3600));
        runner.step();
        let woke = time.wall();
        assert_eq!(
            vendors.count("session"),
            asked,
            "not in the slice it woke in"
        );
        while vendors.count("session") == asked {
            runner.step();
        }
        assert!(time.wall() >= woke + AFTER_WAKE, "after the grace");
        assert!(time.wall() <= woke + AFTER_WAKE + SLICE * 2);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// AUTOMATIC UPDATES, LIVE: off, the session asks nothing and lets the seat go — a
    /// report says nobody watches; on again, it watches at once.
    #[test]
    fn the_switch_stands_the_session_down_and_frees_the_seat() {
        let l = layout("switch");
        install(&l, "claude", "2.1.280");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let gate = on();
        let mut runner = session(&l, &time, &vendors, "session", &passes, &gate);
        for _ in 0..3 {
            runner.step();
        }
        assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        gate.store(false, Ordering::SeqCst);
        let asked = vendors.count("session");
        for _ in 0..(5 * 60 / SLICE.as_secs()) {
            runner.step();
        }
        assert_eq!(vendors.count("session"), asked);
        assert_eq!(watcher(&l), Watcher::Nobody);
        gate.store(true, Ordering::SeqCst);
        runner.step();
        runner.step();
        assert_eq!(vendors.count("session"), asked + 1);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE CLEAN EXIT: a runner on its own thread, on the real clocks, watches at once —
    /// the report names this process — and stopped, it returns and frees the watch.
    #[test]
    fn a_stopped_host_frees_the_watch() {
        let l = layout("stop");
        install(&l, "claude", "2.1.280");
        let (asked_tx, asked_rx) = std::sync::mpsc::channel();
        let asked_tx = Mutex::new(asked_tx);
        let get: Arc<ConcurrentVendorGetFn<'static>> = Arc::new(move |_, url, _, _| {
            let _ = asked_tx.lock().unwrap().send(());
            Ok(VendorGet::Body {
                bytes: b"2.1.280".to_vec(),
                etag: None,
                effective_url: url.to_string(),
            })
        });
        let runner = Runner::new(
            l.clone(),
            (
                HeadWatch::hosted(&[String::from("codex")], Host::Session),
                get,
            ),
            Box::new(SystemClock::new()),
            Box::new(|| Gate {
                on: true,
                exclude: vec![String::from("codex")],
            }),
            Box::new(Passes::default()),
            Box::new(|_: &str| {}),
        );
        let handle = runner.spawn().unwrap();
        asked_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        handle.stop();
        assert_eq!(watcher(&l), Watcher::Nobody);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A PASS STARTED MID-ROUND (review, 2026-09-26). One round asks both vendors, and the
    /// one whose head moved is handed back while its peer's GET is still in flight — the
    /// watch's own rule, so a slow host never delays an install — and the runner starts its
    /// pass at once. The peer's answer must still be harvested when it lands, pass or no
    /// pass: until the round is harvested the session holds the rendezvous EXCLUSIVE, so a
    /// window opening then was refused for as long as the pass ran (an hour's bound, not one
    /// GET), and the unharvested completion cut every park short, so the thread spun a core
    /// for the whole pass. The peer's own moved head is not lost either: its pass runs when
    /// the first one ends. Real clocks, a real thread, a pass the test ends.
    #[test]
    fn a_pass_started_mid_round_still_harvests_the_round() {
        use super::super::BUSY_RETRY;
        use super::super::host::{Admit, HostClaim};
        let l = layout("pass-mid-round");
        install(&l, "claude", "2.1.280");
        install(&l, "codex", "0.156.0");
        let codex = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (answered_tx, answered_rx) = std::sync::mpsc::channel();
        let answered_tx = Mutex::new(answered_tx);
        let get: Arc<ConcurrentVendorGetFn<'static>> = {
            let codex = Arc::clone(&codex);
            Arc::new(move |program: &str, url: &str, _cap, _etag: Option<&str>| {
                if program == "claude" {
                    return Ok(VendorGet::Body {
                        bytes: b"2.1.281".to_vec(),
                        etag: None,
                        effective_url: url.to_string(),
                    });
                }
                let (go, released) = &*codex;
                let go = go.lock().unwrap();
                let (go, _) = released
                    .wait_timeout_while(go, Duration::from_secs(60), |go| !*go)
                    .unwrap();
                assert!(*go, "the test released codex's GET");
                let _ = answered_tx.lock().unwrap().send(());
                Ok(VendorGet::Body {
                    bytes: br#"{"tag_name":"rust-v0.157.0","assets":[]}"#.to_vec(),
                    etag: None,
                    effective_url: url.to_string(),
                })
            })
        };
        struct Launched(
            Mutex<std::sync::mpsc::Sender<&'static str>>,
            Arc<AtomicBool>,
        );
        impl PassLauncher for Launched {
            fn launch(&mut self, program: &'static str) -> io::Result<Box<dyn RunningPass>> {
                let _ = self.0.lock().unwrap().send(program);
                Ok(Box::new(FakePass(Arc::clone(&self.1))))
            }
        }
        let (launched_tx, launched_rx) = std::sync::mpsc::channel();
        let end = Arc::new(AtomicBool::new(false));
        let looks = Arc::new(AtomicU64::new(0));
        let runner = Runner::new(
            l.clone(),
            (HeadWatch::hosted(&[], Host::Session), get),
            Box::new(SystemClock::new()),
            Box::new({
                let looks = Arc::clone(&looks);
                move || {
                    looks.fetch_add(1, Ordering::SeqCst);
                    Gate {
                        on: true,
                        exclude: Vec::new(),
                    }
                }
            }),
            Box::new(Launched(Mutex::new(launched_tx), Arc::clone(&end))),
            Box::new(|_: &str| {}),
        );
        let handle = runner.spawn().unwrap();
        assert_eq!(
            launched_rx.recv_timeout(Duration::from_secs(20)),
            Ok("claude"),
            "claude's pass started while codex's GET was in flight"
        );
        // The round is in flight: a window opening now waits for it, as it should.
        let mut window = HostClaim::new(Host::Window, &l);
        assert_eq!(window.admit().0, Admit::Later(BUSY_RETRY));

        // Codex answers. Over the next second the runner looks at most twice (a slice is
        // five), and a window is admitted as soon as the answer is harvested.
        {
            let (go, released) = &*codex;
            *go.lock().unwrap() = true;
            released.notify_all();
        }
        answered_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        let before = looks.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_secs(1));
        let spun = looks.load(Ordering::SeqCst) - before;
        let deadline = Instant::now() + Duration::from_secs(20);
        let admitted = loop {
            if window.admit().0 == Admit::Yes {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Claude's pass ends: codex's, queued behind it, starts.
        end.store(true, Ordering::SeqCst);
        let next = launched_rx.recv_timeout(Duration::from_secs(20));
        handle.stop();
        assert!(
            spun <= 2,
            "the runner looked {spun} times in one second while its pass ran: it spun"
        );
        assert!(
            admitted,
            "the round was never harvested while the pass ran: the session kept the \
             rendezvous and the window was refused"
        );
        assert_eq!(
            next,
            Ok("codex"),
            "the peer's moved head ran after the pass"
        );
        drop(window);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// `[packages]`, live: an edit is read at the next look, a file that cannot be read
    /// keeps the gate last read, and a missing file is the defaults (on, nothing
    /// excluded).
    #[test]
    fn live_settings_hear_an_edit_and_keep_the_gate_through_an_unreadable_file() {
        let l = layout("live-settings");
        let path = l.prefix.join("aterm.toml");
        let mut live = LiveSettings::at(Some(path.clone()), Gate::default());
        assert_eq!(
            live.gate(),
            Gate {
                on: true,
                exclude: Vec::new()
            },
            "no file: the defaults"
        );
        std::fs::write(
            &path,
            "[packages]\nenabled = false\nexclude = [\"codex\"]\n",
        )
        .unwrap();
        assert_eq!(
            live.gate(),
            Gate {
                on: false,
                exclude: vec![String::from("codex")]
            }
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            !live.gate().on,
            "a file that cannot be read keeps the last gate"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The session's pass is the window's targeted pass, argv for argv after the lead —
    /// its whole pass for a held toolchain move the window's whole pass, and
    /// its index pass the window's published-build scheduling witness. All
    /// unattended whole passes hold a busy flip for quiet.
    #[test]
    fn a_detached_pass_carries_the_windows_argv() {
        let l = layout("argv");
        let passes = DetachedPasses::new(PathBuf::from("/x/aterm"), vec!["pkg".into()], &l);
        let mut want: Vec<OsString> = ["pkg", "update", "claude", crate::cli::HEAD_WATCH_FLAG]
            .map(OsString::from)
            .to_vec();
        want.extend(aterm_update_core::pkg_check::pass_flags(Some(
            &l.progress_file(),
        )));
        assert_eq!(passes.args("claude"), want);
        let mut whole: Vec<OsString> = ["pkg", "update", crate::cli::DEFER_BUSY_FLIP_FLAG]
            .map(OsString::from)
            .to_vec();
        whole.extend(aterm_update_core::pkg_check::pass_flags(Some(
            &l.progress_file(),
        )));
        assert_eq!(passes.whole_args(), whole);
        let mut index: Vec<OsString> = [
            "pkg",
            "update",
            crate::cli::PUBLISHED_INDEX_HINT_FLAG,
            "43",
            crate::cli::DEFER_BUSY_FLIP_FLAG,
        ]
        .map(OsString::from)
        .to_vec();
        index.extend(aterm_update_core::pkg_check::pass_flags(Some(
            &l.progress_file(),
        )));
        assert_eq!(passes.index_args(43), index);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// THE HELD TOOLCHAIN MOVE WITH NO WINDOW OPEN (2026-09-26): the seated session looks
    /// at it by the window's rule — two quiet looks a minute apart — and starts the whole
    /// pass once, following it to its end with no head asked meanwhile. The NEGATIVE
    /// CONTROL: with a window watching, the session never wakes it — the window's park
    /// does — however long it steps.
    #[test]
    fn the_seated_session_wakes_a_held_toolchain_move_and_a_standing_one_does_not() {
        use crate::quiet::{Due, DueFlip};
        let due = |_: &Layout, _: i64| {
            Some(DueFlip {
                why: Due::Quiet,
                checked: 1,
            })
        };
        // Seated: no window.
        let l = layout("held-seated");
        install(&l, "claude", "2.1.280");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let mut runner =
            session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
        let mut steps = 0;
        while passes.launched().is_empty() {
            runner.step();
            steps += 1;
            assert!(
                steps
                    <= 2 + usize::try_from(crate::quiet::RECHECK.as_secs() / SLICE.as_secs())
                        .unwrap()
                        + 2,
                "two looks a minute apart, then the pass: {runner:?}"
            );
        }
        assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        assert!(
            !runner.watch.seated_session(),
            "the session released the exclusive claim after launching"
        );
        assert_eq!(passes.launched(), [WHOLE], "the whole pass, once");
        // Followed to its end: no second pass for the same look, no head asked meanwhile.
        let asked = vendors.count("session");
        for _ in 0..20 {
            runner.step();
        }
        assert_eq!(passes.launched(), [WHOLE]);
        assert_eq!(
            vendors.count("session"),
            asked,
            "no head asked while it runs"
        );
        passes.end.store(true, Ordering::SeqCst);
        for _ in 0..30 {
            runner.step();
        }
        assert_eq!(
            passes.launched(),
            [WHOLE],
            "the same look of the record wakes nothing again"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);

        // Standing by: a window watches, and its park is the one that looks.
        let l = layout("held-window");
        install(&l, "claude", "2.1.280");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let window_get = vendors.get("window");
        let mut window = HeadWatch::hosted(&[String::from("codex")], Host::Window);
        window_slice(&mut window, &l, &time, &window_get);
        let mut runner =
            session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
        for _ in 0..(5 * 60 / SLICE.as_secs()) {
            runner.step();
            window_slice(&mut window, &l, &time, &window_get);
        }
        assert!(!runner.watch.seated_session());
        assert!(
            passes.launched().is_empty(),
            "a session standing by never wakes it: {:?}",
            passes.launched()
        );
        drop(window);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Tier-1 for `AtpkgSessionIndexHandoff`: a near HEAD is offered while
    /// the far HEAD is still blocked; another tab cannot ask it, and a window
    /// that opens before the near answer takes the pass decision. The real
    /// runner is projected onto the derived model at each decision.
    #[test]
    fn a_seated_session_offers_a_near_index_once_after_window_handoff() {
        use aterm_spec::derive::atpkg_session_index_handoff_model;
        use std::sync::mpsc;

        let l = layout("session-index-window-handoff");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let requests = Arc::new(AtomicU64::new(0));
        let (near_tx, near_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let probe_requests = Arc::clone(&requests);
        let probe_release = Arc::clone(&release);
        let mut first = session(&l, &time, &vendors, "first", &passes, &on()).with_index_probe(
            move |_, on_near| {
                probe_requests.fetch_add(1, Ordering::SeqCst);
                on_near(43);
                near_tx.send(()).unwrap();
                let (go, wake) = &*probe_release;
                let held = go.lock().unwrap();
                let (held, _) = wake
                    .wait_timeout_while(held, Duration::from_secs(20), |go| !*go)
                    .unwrap();
                assert!(*held, "the test released the far HEAD");
                crate::index_probe::Probe::Published(43)
            },
        );
        let second_requests = Arc::clone(&requests);
        let mut second =
            session(&l, &time, &vendors, "second", &passes, &on()).with_index_probe(move |_, _| {
                second_requests.fetch_add(1, Ordering::SeqCst);
                crate::index_probe::Probe::Published(43)
            });
        let model = atpkg_session_index_handoff_model();
        let mut state = model.init_state();

        first.step();
        assert!(model.fire("Start", &mut state));
        near_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert!(model.fire("NearPublished", &mut state));
        second.step();
        assert!(model.fire("OtherSessionStarts", &mut state));
        assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
        assert_eq!(
            second.watch.session_admission_attempts(),
            1,
            "one refused seat look"
        );
        assert_eq!(first.index.pending.is_some(), state["probe"] == 1);

        let mut window = HeadWatch::hosted(&[], Host::Window);
        assert!(window.sit(&l), "the session released the HEAD launch claim");
        assert!(model.fire("WindowOpens", &mut state));
        first.step();
        assert!(model.fire("TryLaunch", &mut state));
        assert_eq!(passes.launched().len(), state["launches"] as usize);
        assert!(passes.launched().is_empty(), "the window owns this hint");
        drop(window);
        assert!(model.fire("WindowCloses", &mut state));
        first.step();
        assert!(model.fire("TryLaunch", &mut state));
        assert_eq!(passes.launched(), [WHOLE]);
        assert_eq!(passes.launched().len(), state["launches"] as usize);

        let (go, wake) = &*release;
        *go.lock().unwrap() = true;
        wake.notify_all();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !first
            .index
            .pending
            .as_ref()
            .expect("the far HEAD is still owned")
            .done
            .load(Ordering::Acquire)
        {
            assert!(Instant::now() < deadline, "the far HEAD completed");
            std::thread::sleep(Duration::from_millis(1));
        }
        passes.end.store(true, Ordering::SeqCst);
        for _ in 0..3 {
            first.step();
        }
        assert!(model.fire("Complete", &mut state));
        assert!(model.fire("FarReplay", &mut state));
        assert_eq!(passes.launched().len(), state["launches"] as usize);
        assert_eq!(
            passes.launched(),
            [WHOLE],
            "the far answer is not another pass"
        );
        drop(second);
        drop(first);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Tier-1 for `AtpkgSessionIndexEligibility`: an empty store gets cheap
    /// local looks at park slices but sends no HEAD. Once work appears, the
    /// next slice starts a probe without an artificial thirty-second warmup.
    #[test]
    fn an_empty_index_source_becomes_probe_eligible_on_the_next_slice() {
        use aterm_spec::derive::atpkg_session_index_eligibility_model;
        use std::sync::mpsc;

        let l = layout("session-index-eligibility");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let eligible = Arc::new(AtomicBool::new(false));
        let checks = Arc::new(AtomicU64::new(0));
        let requests = Arc::new(AtomicU64::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let probe_requests = Arc::clone(&requests);
        let mut runner = session(&l, &time, &vendors, "session", &passes, &on()).with_index_probe(
            move |_, _| {
                probe_requests.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                crate::index_probe::Probe::Missing
            },
        );
        let eligible_for_runner = Arc::clone(&eligible);
        let checks_for_runner = Arc::clone(&checks);
        runner.index.eligible = Arc::new(move |_| {
            checks_for_runner.fetch_add(1, Ordering::SeqCst);
            eligible_for_runner.load(Ordering::SeqCst)
        });
        let model = atpkg_session_index_eligibility_model();
        let mut state = model.init_state();

        assert!(!runner.check_index(time.wall()));
        assert!(model.fire("EmptyLook", &mut state));
        assert_eq!(checks.load(Ordering::SeqCst), 1);
        assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
        assert_eq!(runner.index.last_completed, None);
        assert_eq!(runner.index.last_ineligible, Some(Duration::ZERO));

        time.advance(SLICE / 2);
        assert!(!runner.check_index(time.wall()));
        assert_eq!(
            checks.load(Ordering::SeqCst),
            1,
            "local look is slice-paced"
        );
        time.advance(SLICE / 2);
        assert!(model.fire("Tick", &mut state));
        assert!(!runner.check_index(time.wall()));
        assert!(model.fire("EmptyLook", &mut state));
        assert_eq!(checks.load(Ordering::SeqCst), 2);
        assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
        assert_eq!(runner.index.last_completed, None);

        eligible.store(true, Ordering::SeqCst);
        assert!(model.fire("Enable", &mut state));
        time.advance(SLICE / 2);
        assert!(!runner.check_index(time.wall()));
        assert_eq!(checks.load(Ordering::SeqCst), 2);
        assert!(runner.index.pending.is_none());
        time.advance(SLICE / 2);
        assert!(model.fire("Tick", &mut state));
        assert!(!runner.check_index(time.wall()));
        assert!(model.fire("FirstEligibleLook", &mut state));
        assert!(
            runner.index.pending.is_some(),
            "the HEAD worker starts at the next slice"
        );
        started_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(checks.load(Ordering::SeqCst), 3);
        assert_eq!(requests.load(Ordering::SeqCst), state["requests"] as u64);
        assert_eq!(runner.index.last_ineligible, None);

        let deadline = Instant::now() + Duration::from_secs(20);
        while runner.index.pending.is_some() {
            runner
                .index
                .harvest(Duration::from_millis(time.mono_ms.load(Ordering::SeqCst)));
            assert!(Instant::now() < deadline, "the injected HEAD completed");
            std::thread::sleep(Duration::from_millis(1));
        }
        let completed = runner
            .index
            .last_completed
            .expect("network completion stamp");
        assert!(
            !runner
                .index
                .due(completed + crate::index_probe::INTERVAL - Duration::from_millis(1))
        );
        assert!(runner.index.due(completed + crate::index_probe::INTERVAL));
        assert!(passes.launched().is_empty());
        drop(runner);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Tier-1 for `AtpkgIndexSharedHandoff`: a seated session that arrived
    /// just before a sibling's stamp expires retries at that shared expiry,
    /// without another full local interval or an early network request.
    #[test]
    fn a_suppressed_session_index_probe_uses_the_shared_expiry() {
        use aterm_spec::derive::atpkg_index_shared_handoff_model;

        let l = layout("session-index-shared-handoff");
        let time = Time::default();
        time.advance(SLICE * 5);
        let now = || Duration::from_millis(time.mono_ms.load(Ordering::SeqCst));
        let probes = Arc::new(AtomicU64::new(0));
        let asked = Arc::clone(&probes);
        let mut index = IndexWatch::new();
        index.probe = Arc::new(move |_, _| {
            if asked.fetch_add(1, Ordering::SeqCst) == 0 {
                crate::index_probe::Probe::Suppressed(SLICE)
            } else {
                crate::index_probe::Probe::Missing
            }
        });
        let model = atpkg_index_shared_handoff_model();
        let mut state = model.init_state();
        assert!(model.fire("Stamp", &mut state));
        for _ in 0..5 {
            assert!(model.fire("TickBeforeHandoff", &mut state));
        }
        assert!(index.due(now()));
        index.start(&l, now()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while index.pending.is_some() {
            index.harvest(now());
            assert!(Instant::now() < deadline, "the suppressed worker completed");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(model.fire("Handoff", &mut state));
        let completed = index.last_completed.expect("worker completion");
        assert_eq!(index.shared_retry_at, Some(completed + SLICE));
        assert!(!index.due(completed + SLICE - Duration::from_millis(1)));
        time.advance(SLICE);
        assert!(model.fire("TickAfterHandoff", &mut state));
        assert!(index.due(completed + SLICE));
        assert!(model.fire("AtExpiry", &mut state));
        index.start(&l, completed + SLICE).unwrap();
        while index.pending.is_some() {
            index.harvest(completed + SLICE);
            assert!(Instant::now() < deadline, "the due worker completed");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(probes.load(Ordering::SeqCst) - 1, state["requests"] as u64);
        drop(index);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A verified build is a terminal answer, including when a worker began
    /// before another host raised the floor. A second probe uses the previous
    /// completion's time, not its network start, as its local thirty-second
    /// cadence.
    #[test]
    fn verified_index_near_hint_starts_no_pass_and_probe_is_completion_paced() {
        use std::sync::mpsc;

        let l = layout("session-index-floor");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let probes = Arc::new(AtomicU64::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let probe_count = Arc::clone(&probes);
        let mut runner = session(&l, &time, &vendors, "session", &passes, &on()).with_index_probe(
            move |_, on_near| {
                probe_count.fetch_add(1, Ordering::SeqCst);
                on_near(43);
                started_tx.send(()).unwrap();
                crate::index_probe::Probe::Published(43)
            },
        );
        runner.step();
        started_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        crate::sig::Floor::new(l.floor())
            .check_and_record(43)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while runner.index.pending.is_some() {
            runner.step();
            assert!(Instant::now() < deadline, "the probe completed");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(passes.launched().is_empty(), "verified floor covers 43");
        let completed = runner.index.last_completed.expect("the probe ended");
        for _ in 0..3 {
            runner.step();
        }
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        while runner.index.pending.is_none() {
            runner.step();
            assert!(
                time.mono_ms.load(Ordering::SeqCst)
                    <= (completed + crate::index_probe::INTERVAL + SLICE * 2).as_millis() as u64,
                "the next probe is due at completion plus thirty seconds"
            );
        }
        assert!(
            time.mono_ms.load(Ordering::SeqCst)
                >= (completed + crate::index_probe::INTERVAL).as_millis() as u64,
            "a second probe cannot start before its predecessor's completion stamp"
        );
        started_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        assert_eq!(probes.load(Ordering::SeqCst), 2);
        assert!(passes.launched().is_empty());
        drop(runner);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A release one build newer than this session's recent pass bypasses its
    /// own same-build spacing. The old answer remains remembered while the
    /// thirty-second HEAD cadence keeps looking for a higher build.
    #[test]
    fn a_higher_index_wakes_promptly_after_an_older_index_pass() {
        let l = layout("session-index-higher");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let probes = Arc::new(AtomicU64::new(0));
        let count = Arc::clone(&probes);
        let mut runner = session(&l, &time, &vendors, "session", &passes, &on()).with_index_probe(
            move |_, on_near| {
                let build = 43 + count.fetch_add(1, Ordering::SeqCst);
                on_near(build);
                crate::index_probe::Probe::Published(build)
            },
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while passes.launched().is_empty() {
            runner.step();
            assert!(Instant::now() < deadline, "the near 43 launched a pass");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(passes.launched(), [WHOLE]);
        passes.end.store(true, Ordering::SeqCst);
        let started = time.mono_ms.load(Ordering::SeqCst);
        while passes.launched().len() == 1 {
            runner.step();
            assert!(Instant::now() < deadline, "the higher 44 launched a pass");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(passes.launched(), [WHOLE, WHOLE]);
        assert!(
            time.mono_ms.load(Ordering::SeqCst) - started
                < aterm_update_core::pkg_check::PASS_SPACING_SECS * 1_000,
            "the higher build did not wait out the old build's spacing"
        );
        drop(runner);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The same published asset can remain visible for hours while its roster
    /// or artifacts are incomplete. Each failed full pass has more time to
    /// recover before the next one, but a higher build never inherits that wait.
    #[test]
    fn a_same_index_uses_the_bounded_retry_ladder() {
        let model = aterm_spec::derive::atpkg_session_index_retry_model();
        let mut state = model.init_state();
        let mut index = IndexWatch::new();
        let minute = Duration::from_secs(60);
        index.note_attempt(43, Duration::ZERO, true);
        assert!(model.fire("First", &mut state));
        assert_eq!(
            state["passes"],
            i64::from(index.last_attempt.unwrap().passes)
        );
        assert!(model.action_enabled("EarlySame", &state));
        assert!(index.last_attempt.unwrap().holds(43, 5 * minute - SLICE));
        assert!(model.fire("Tick", &mut state));
        assert!(model.action_enabled("RetryFirst", &state));
        assert!(!index.last_attempt.unwrap().holds(43, 5 * minute));
        index.note_attempt(43, 5 * minute, true);
        assert!(model.fire("RetryFirst", &mut state));
        assert_eq!(
            state["passes"],
            i64::from(index.last_attempt.unwrap().passes)
        );
        for _ in 0..2 {
            assert!(model.fire("Tick", &mut state));
            assert!(model.action_enabled("EarlySame", &state));
        }
        assert!(index.last_attempt.unwrap().holds(43, 20 * minute - SLICE));
        assert!(model.fire("Tick", &mut state));
        assert!(model.action_enabled("RetrySecond", &state));
        assert!(!index.last_attempt.unwrap().holds(43, 20 * minute));
        index.note_attempt(43, 20 * minute, true);
        assert!(model.fire("RetrySecond", &mut state));
        assert_eq!(
            state["passes"],
            i64::from(index.last_attempt.unwrap().passes)
        );
        assert!(index.last_attempt.unwrap().holds(43, 80 * minute - SLICE));
        assert!(!index.last_attempt.unwrap().holds(43, 80 * minute));
        assert!(!index.last_attempt.unwrap().holds(44, 20 * minute + SLICE));
        index.note_attempt(44, 20 * minute + SLICE, true);
        assert!(model.fire("NewerHint", &mut state));
        assert_eq!(
            state["passes"],
            i64::from(index.last_attempt.unwrap().passes)
        );
        assert_eq!(index.last_attempt.unwrap().passes, 1);
        index.note_attempt(44, 25 * minute, false);
        assert!(index.last_attempt.unwrap().holds(44, 25 * minute + SLICE));
        assert!(
            !index
                .last_attempt
                .unwrap()
                .holds(44, 25 * minute + crate::index_probe::INTERVAL)
        );
    }

    /// A window can take the shared rendezvous between a session's vendor-head
    /// rounds. The session still holds the seat and remembers that it last
    /// watched, but the window owns the held-flip look now. Replaying that exact
    /// handoff must not launch a second whole pass from the session.
    #[test]
    fn a_window_arriving_after_a_session_round_owns_the_held_move_look() {
        use crate::quiet::{Due, DueFlip};
        let l = layout("held-handoff");
        install(&l, "claude", "2.1.280");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let looks = Arc::new(AtomicU64::new(0));
        let due_looks = Arc::clone(&looks);
        let due = move |_: &Layout, _: i64| {
            due_looks.fetch_add(1, Ordering::SeqCst);
            Some(DueFlip {
                why: Due::Quiet,
                checked: 1,
            })
        };
        let mut runner =
            session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
        for _ in 0..3 {
            runner.step();
        }
        assert_eq!(vendors.count("session"), 1, "the first round completed");
        assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        assert!(
            !runner.watch.seated_session(),
            "the exclusive round ended but the seat remains"
        );

        let window_get = vendors.get("window");
        let mut window = HeadWatch::hosted(&[String::from("codex")], Host::Window);
        window_slice(&mut window, &l, &time, &window_get);
        assert_eq!(watcher(&l), Watcher::Window);
        let looked_before_window = looks.load(Ordering::SeqCst);
        for _ in 0..(3 * 60 / SLICE.as_secs()) {
            runner.step();
        }
        assert_eq!(
            looks.load(Ordering::SeqCst),
            looked_before_window,
            "the standing session never scans the flip record or process table"
        );
        assert!(
            passes.launched().is_empty(),
            "the session must not wake the window's held move: {:?}",
            passes.launched()
        );
        drop(window);
        // The window is gone. The next slice takes the exclusive claim and
        // reads the pending record at once, instead of borrowing the old
        // window-refused attempt as a 60-second quiet-check delay.
        runner.step();
        assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        assert_eq!(
            looks.load(Ordering::SeqCst),
            looked_before_window + 1,
            "the session reads the record on its first slice back"
        );
        for _ in 0..(60 / SLICE.as_secs()) + 2 {
            runner.step();
            if !passes.launched().is_empty() {
                break;
            }
        }
        assert_eq!(passes.launched(), [WHOLE], "the seat resumed promptly");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A held toolchain may exist even when neither vendor-direct program is
    /// installed. The session must take the seat for its quiet look without a
    /// vendor-head round to admit it first.
    #[test]
    fn a_session_wakes_a_held_toolchain_without_vendor_programs() {
        use crate::quiet::{Due, DueFlip};
        let l = layout("held-no-vendors");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let due = |_: &Layout, _: i64| {
            Some(DueFlip {
                why: Due::Quiet,
                checked: 1,
            })
        };
        let mut runner =
            session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
        for _ in 0..(2 * 60 / SLICE.as_secs()) {
            runner.step();
            if !passes.launched().is_empty() {
                break;
            }
        }
        assert_eq!(passes.launched(), [WHOLE]);
        assert_eq!(vendors.count("session"), 0, "no vendor GET was needed");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A HELD TOOLCHAIN MOVE ON A STORE WITH NO VENDOR PROGRAM TO WATCH (review of 1c202aa93,
    /// 2026-09-26): nothing asks a head there, and a round of head checks was the only thing
    /// that took the seat, so no session ever looked, and "by <time> at the latest while an
    /// aterm window or terminal session is open" — the held flip's row and pass line — was
    /// untrue. The session's look takes the seat itself ([`HeadWatch::begin_session_update_look`]) — while the
    /// move is still busy, so doctor names the session that will wake it — and wakes the
    /// whole pass once it is due, with no head asked. The NEGATIVE CONTROL: a window holding
    /// its claim (its park takes it every slice, a head asked or not) keeps the session
    /// standing by.
    #[test]
    fn a_held_move_takes_the_seat_on_a_store_with_no_vendor_program() {
        use crate::quiet::{Due, DueFlip};
        let quiet = Arc::new(AtomicBool::new(false));
        let now_quiet = Arc::clone(&quiet);
        let due = move |_: &Layout, _: i64| {
            now_quiet.load(Ordering::SeqCst).then_some(DueFlip {
                why: Due::Quiet,
                checked: 1,
            })
        };
        let looks =
            2 + usize::try_from(crate::quiet::RECHECK.as_secs() / SLICE.as_secs()).unwrap() + 2;
        let l = layout("held-no-vendor");
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let mut runner =
            session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due.clone());
        assert_eq!(watcher(&l), Watcher::Nobody, "before the first look");
        for _ in 0..looks {
            runner.step();
        }
        assert_eq!(
            watcher(&l),
            Watcher::Session(Some(std::process::id())),
            "the session sits while the move is still busy"
        );
        assert!(passes.launched().is_empty(), "busy: nothing woken");
        quiet.store(true, Ordering::SeqCst);
        let mut steps = 0;
        while passes.launched().is_empty() {
            runner.step();
            steps += 1;
            assert!(
                steps <= 2 * looks,
                "the next look, a second a minute later, then the pass: {runner:?}"
            );
        }
        assert!(
            !runner.watch.seated_session(),
            "the exclusive look ended after launching"
        );
        assert_eq!(watcher(&l), Watcher::Session(Some(std::process::id())));
        assert_eq!(passes.launched(), [WHOLE]);
        assert_eq!(vendors.count("session"), 0, "no head was asked");
        drop(runner);
        let _ = std::fs::remove_dir_all(&l.prefix);

        let l = layout("held-no-vendor-window");
        let mut window = HeadWatch::hosted(&[], Host::Window);
        assert!(window.sit(&l), "the window takes the shared claim");
        assert_eq!(watcher(&l), Watcher::Window);
        let time = Time::default();
        let vendors = Vendors::new("2.1.280");
        let passes = Passes::default();
        let mut runner =
            session(&l, &time, &vendors, "session", &passes, &on()).with_held_move_due(due);
        for _ in 0..(5 * 60 / SLICE.as_secs()) {
            runner.step();
            assert!(window.sit(&l));
        }
        assert_eq!(watcher(&l), Watcher::Window);
        assert!(!runner.watch.seated_session());
        assert!(
            passes.launched().is_empty(),
            "a session beside a window never wakes it: {:?}",
            passes.launched()
        );
        drop(window);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A DETACHED PASS IS NOT THIS PROCESS'S CHILD — a session reaping its shell can never
    /// take the pass's exit for the shell's, and the pass outlives the session — it names
    /// no spawner, and it is followed to its end by pid.
    #[cfg(unix)]
    #[test]
    fn a_detached_pass_is_nobodys_child_and_is_followed_to_its_end() {
        use std::os::unix::fs::PermissionsExt as _;
        let l = layout("detached");
        let dir = l.prefix.join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("aterm");
        std::fs::write(
            &exe,
            "#!/bin/sh\nd=$(dirname \"$0\")\nprintf '%s %s %s\\n' \"$$\" \"$ATPKG_SPAWNER_PID\" \
             \"$*\" >\"$d/said.tmp\"\nmv \"$d/said.tmp\" \"$d/said\"\nwhile [ ! -e \"$d/go\" ]; \
             do sleep 0.05; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut passes = DetachedPasses::new(exe, vec!["pkg".into()], &l);
        let mut pass = passes.launch("claude").unwrap();
        let said = dir.join("said");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !said.exists() {
            assert!(Instant::now() < deadline, "the pass started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let said = std::fs::read_to_string(&said).unwrap();
        let (pid, rest) = said.split_once(' ').unwrap();
        assert!(
            rest.starts_with("detached pkg update claude --head-watch --wait-lock"),
            "{said}"
        );
        assert_eq!(pass.ended(), None, "running");
        let pid: libc::pid_t = pid.parse().unwrap();
        // SAFETY: `waitpid` with WNOHANG on one pid only asks the kernel about that pid.
        let reaped = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
        assert_eq!(
            (reaped, std::io::Error::last_os_error().raw_os_error()),
            (-1, Some(libc::ECHILD)),
            "the running pass is not this process's child"
        );
        std::fs::write(dir.join("go"), b"").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let ended = loop {
            if let Some(ok) = pass.ended() {
                break ok;
            }
            assert!(Instant::now() < deadline, "the pass's end was seen");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(ended, "a detached pass's end is the store's to judge");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
