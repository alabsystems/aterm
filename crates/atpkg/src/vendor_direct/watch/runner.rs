// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The head watch's own loop, for a host that has none.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use super::{CADENCE, ConcurrentVendorGetFn, HeadWatch, Host, Rest, Ring, Told, woke_between};
use crate::store::Layout;

/// The bounded look a package host keeps where nothing it waits for rings it: no wake source
/// tells it of a sleep while it watches (a terminal session), its file watch did not start, it
/// follows a detached pass to an exit nothing announces, a sibling's pass holds a published index
/// it owes a pass, or a window watches and no waiter could be armed. It is the rate every host's
/// wait ran at before it waited on events, so no host notices a sleep, a release or a due install
/// later than it did.
pub const BLIND_LOOK: Duration = Duration::from_secs(5);

const PASS_FOLLOW_LIMIT: Duration =
    Duration::from_secs(aterm_update_core::pkg_check::PASS_OPERATION_LIMIT_SECS);

/// Clocks a runner reads and the rest it waits on.
pub trait Clock: Send {
    /// Current wall-clock time.
    fn wall(&self) -> SystemTime;
    /// Elapsed monotonic time from an arbitrary origin.
    fn mono(&self) -> Duration;
    /// Wait on `rest` at most `within` (`None`: until it rings) and take what rang. `watch` is
    /// the host's head watch, whose round in flight rings `rest` at its news: a virtual clock
    /// lets that round ring before it passes time, as real time would.
    fn wait(&mut self, rest: &Rest, watch: &HeadWatch, within: Option<Duration>) -> Option<Told>;
}

/// The production clocks and wait.
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

    fn wait(&mut self, rest: &Rest, _watch: &HeadWatch, within: Option<Duration>) -> Option<Told> {
        rest.wait(within)
    }
}

/// `[packages]` gate read before every look.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gate {
    /// Whether automatic updates are enabled.
    pub on: bool,
    /// Excluded package names.
    pub exclude: Vec<String>,
}

/// Source of a runner's [`Gate`].
pub trait Settings: Send {
    fn gate(&mut self) -> Gate;
}

impl<F: FnMut() -> Gate + Send> Settings for F {
    fn gate(&mut self) -> Gate {
        self()
    }
}

/// Live `[packages]` reader backed by `aterm.toml`.
#[derive(Debug)]
pub struct LiveSettings {
    path: Option<PathBuf>,
    /// The file's `(mtime, length)` at the last read; `Some(None)` for an absent file,
    /// `None` before the first read.
    seen: Option<Option<(Option<SystemTime>, u64)>>,
    gate: Gate,
}

impl LiveSettings {
    /// Create a reader for the user's config file starting from `gate`.
    #[must_use]
    pub fn new(gate: Gate) -> Self {
        Self::at(crate::config::config_path(), gate)
    }

    const fn at(path: Option<PathBuf>, gate: Gate) -> Self {
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
        let stamp = std::fs::symlink_metadata(path)
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

/// A targeted pass started by a runner.
pub trait RunningPass: Send {
    /// Return `None` while running, or `Some(succeeded)` once ended.
    fn ended(&mut self) -> Option<bool>;
}

/// Launcher for passes requested by a moved head.
pub trait PassLauncher: Send {
    /// Start `update <program>` through the head watch's door.
    /// # Errors
    /// The pass could not be started.
    fn launch(&mut self, program: &'static str) -> io::Result<Box<dyn RunningPass>>;

    /// Start the whole unattended pass once a held toolchain move is due.
    /// # Errors
    /// The pass could not be started.
    fn launch_whole(&mut self) -> io::Result<Box<dyn RunningPass>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this host starts no whole pass",
        ))
    }

    /// Start a whole pass for an untrusted published-index hint.
    /// # Errors
    /// The pass could not be started.
    fn launch_index(&mut self, _build: u64) -> io::Result<Box<dyn RunningPass>> {
        self.launch_whole()
    }
}

/// Detached pass launcher for a terminal session.
#[derive(Clone, Debug)]
pub struct DetachedPasses {
    exe: PathBuf,
    lead: Vec<OsString>,
    progress_file: PathBuf,
}

impl DetachedPasses {
    /// Create a launcher for `exe lead…` reporting through `layout`'s progress file.
    #[must_use]
    pub fn new(exe: PathBuf, lead: Vec<OsString>, layout: &Layout) -> Self {
        Self {
            exe,
            lead,
            progress_file: layout.progress_file(),
        }
    }

    /// Argv after `exe` for a targeted `update <program>` pass.
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

    /// Argv after `exe` for a whole unattended pass.
    #[must_use]
    pub fn whole_args(&self) -> Vec<OsString> {
        self.whole_args_with_index(None)
    }

    /// Argv after `exe` for a published-index pass.
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

/// Start `exe args` detached — stdio on `/dev/null`, its own process group, and never
/// this process's child — and follow it by its pid.
#[cfg(unix)]
fn spawn_detached(exe: &Path, args: &[OsString]) -> io::Result<Box<dyn RunningPass>> {
    Ok(Box::new(Detached {
        pid: detach(exe, args)?,
    }))
}

/// [`spawn_detached`]'s detaching shell. The pid it reports is the pass's own: the
/// clamp's launcher execs the pass in place.
#[cfg(unix)]
fn detach(exe: &Path, args: &[OsString]) -> io::Result<u32> {
    let out = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "\"$@\" </dev/null >/dev/null 2>&1 & echo $!", "sh"])
        .args(crate::platform::utility_clamped(exe.as_os_str()))
        .args(args)
        .env(crate::cli::SPAWNER_PID_ENV, crate::cli::SPAWNER_DETACHED)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .process_group(0)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "the detaching shell {}",
            out.status
        )));
    }
    let raw_pid = String::from_utf8_lossy(&out.stdout);
    let pid = raw_pid.trim();
    (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        .then(|| pid.parse::<u32>().ok())
        .flatten()
        .ok_or_else(|| io::Error::other("the detaching shell named no pid"))
}

/// Windows has no zombie to fear and no re-parenting: the pass is an ordinary child, and
/// its exit is read.
#[cfg(not(unix))]
fn spawn_detached(exe: &Path, args: &[OsString]) -> io::Result<Box<dyn RunningPass>> {
    let child = aterm_uds::spawnfd::command(exe)
        .args(args)
        .env(crate::cli::SPAWNER_PID_ENV, crate::cli::SPAWNER_DETACHED)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(Box::new(Owned(child)))
}

/// A detached pass, followed by kernel process presence. Its exit status
/// is its reaper's to read, not ours, so an end reads as `true` and the store decides.
/// An exited, unreaped process is complete; an unreadable presence remains pending.
#[cfg(unix)]
struct Detached {
    pid: u32,
}

#[cfg(unix)]
impl RunningPass for Detached {
    fn ended(&mut self) -> Option<bool> {
        use aterm_uds::process::{ProcessPresence, process_presence};
        let pid = i32::try_from(self.pid).ok()?;
        matches!(
            process_presence(pid),
            ProcessPresence::Absent | ProcessPresence::Exited
        )
        .then_some(true)
    }
}

#[cfg(not(unix))]
struct Owned(std::process::Child);

#[cfg(not(unix))]
impl RunningPass for Owned {
    fn ended(&mut self) -> Option<bool> {
        owned_pass_end(self.0.try_wait())
    }
}

/// A failed status read is not evidence that an owned pass ended. Keep following
/// it under the existing follow limit, as an unknown detached presence does.
#[cfg(any(not(unix), test))]
fn owned_pass_end(status: io::Result<Option<std::process::ExitStatus>>) -> Option<bool> {
    match status {
        Ok(Some(status)) => Some(status.success()),
        Ok(None) | Err(_) => None,
    }
}

type HeldDue = dyn FnMut(&Layout, i64) -> Option<crate::quiet::DueFlip> + Send;

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
    const fn retry_after(self) -> Duration {
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

struct IndexWatch {
    probe: Arc<IndexProbe>,
    eligible: Arc<dyn Fn(&Layout) -> bool + Send + Sync>,
    pending: Option<PendingIndex>,
    /// A failed local work check is cheap to repeat a [`BLIND_LOOK`] later (work can appear in
    /// the store without a file the rest watches changing); it is not a completed network probe
    /// and must not start the probe's cooldown.
    last_ineligible: Option<Duration>,
    last_completed: Option<Duration>,
    /// A sibling's fresh shared stamp can expire sooner (or, after an error,
    /// later) than one more local thirty-second interval.
    shared_retry_at: Option<Duration>,
    ready: u64,
    /// An incomplete or untrusted release cannot run a full pass every HEAD
    /// cooldown.
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
        self.next_due().is_some_and(|at| at <= now)
    }

    /// When the next probe is due on the runner's monotonic clock: `None` while one is in
    /// flight, whose near answer and end ring the rest. Else the later of a [`BLIND_LOOK`] past
    /// an ineligible look and the probe's own cooldown: a sibling's shared stamp, or one
    /// interval past the last probe.
    fn next_due(&self) -> Option<Duration> {
        if self.pending.is_some() {
            return None;
        }
        let eligible_at = self
            .last_ineligible
            .map_or(Duration::ZERO, |last| last.saturating_add(BLIND_LOOK));
        let cooled_at = self.shared_retry_at.unwrap_or_else(|| {
            self.last_completed.map_or(Duration::ZERO, |last| {
                last.saturating_add(crate::index_probe::INTERVAL)
            })
        });
        Some(eligible_at.max(cooled_at))
    }

    fn start(&mut self, layout: &Layout, now: Duration, ring: Ring) -> io::Result<()> {
        let layout = layout.clone();
        let probe = Arc::clone(&self.probe);
        let near = Arc::new(AtomicU64::new(0));
        let worker_near = Arc::clone(&near);
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = Arc::clone(&done);
        let completed_at = Arc::new(std::sync::OnceLock::new());
        let worker_completed_at = Arc::clone(&completed_at);
        let started_at = Instant::now();
        let worker = std::thread::Builder::new()
            .name("atpkg-index-probe".into())
            .spawn(move || {
                struct Wake {
                    done: Arc<AtomicBool>,
                    completed_at: Arc<std::sync::OnceLock<Instant>>,
                    ring: Ring,
                }
                impl Drop for Wake {
                    fn drop(&mut self) {
                        let _ = self.completed_at.set(Instant::now());
                        // Published before the ring: a host woken by it finds the probe done.
                        self.done.store(true, Ordering::Release);
                        self.ring.ring();
                    }
                }
                let wake = Wake {
                    done: worker_done,
                    completed_at: worker_completed_at,
                    ring,
                };
                let mut on_near = |build| {
                    if worker_near.fetch_max(build, Ordering::Release) < build {
                        wake.ring.ring();
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

struct Following {
    /// The program a moved head's pass updates; `None` for the whole pass a held toolchain
    /// move asked for.
    program: Option<&'static str>,
    index_build: Option<u64>,
    pass: Box<dyn RunningPass>,
    /// [`Clock::mono`] when it started.
    since: Duration,
}

/// A host's head watch, driven alone.
pub struct Runner {
    layout: Layout,
    watch: HeadWatch,
    get: Arc<ConcurrentVendorGetFn<'static>>,
    clock: Box<dyn Clock>,
    /// The one wait each step takes ([`Self::next_wait`]), rung by the head watch's rounds and
    /// waiters, the index probe, the files the host watches and [`HostHandle::stop`].
    rest: Rest,
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
    /// Whether a held move's record stands, so its re-check is a deadline of the wait: its
    /// flip record or a view's leave record exists ([`crate::quiet::record_path`],
    /// [`crate::quiet::view_record_path`]), each a file the rest watches.
    held_stands: Box<dyn Fn(&Layout) -> bool + Send>,
    index: IndexWatch,
    /// One deadline for every update look while admission is refused and no waiter is armed:
    /// a standing second tab tries the seat once a minute, a seated session behind a window
    /// once every [`BLIND_LOOK`]. With a waiter armed it is now, and the waiter's ring ends the
    /// wait instead.
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
    /// Create a runner over `layout` for `heads`.
    #[must_use]
    pub fn new(
        layout: Layout,
        heads: (HeadWatch, Arc<ConcurrentVendorGetFn<'static>>),
        clock: Box<dyn Clock>,
        settings: Box<dyn Settings>,
        launcher: Box<dyn PassLauncher>,
        log: Box<dyn FnMut(&str) + Send>,
    ) -> Self {
        let (mut watch, get) = heads;
        let rest = Rest::inert();
        watch.bind_ring(rest.ringer());
        Self {
            layout,
            watch,
            get,
            clock,
            rest,
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
            held_stands: Box::new(|layout| {
                crate::quiet::record_path(layout).exists()
                    || crate::quiet::view_record_path(layout).exists()
            }),
            index: IndexWatch::new(),
            admit_after: Duration::ZERO,
        }
    }

    /// Wait on `rest` (its files, wake source and rings) in place of an inert one.
    #[must_use]
    pub fn with_rest(mut self, rest: Rest) -> Self {
        self.watch.bind_ring(rest.ringer());
        self.rest = rest;
        self
    }

    /// Override the held-toolchain-move due check, whose move then stands throughout: its
    /// re-check stays a deadline of every wait.
    #[must_use]
    pub fn with_held_move_due(
        mut self,
        due: impl FnMut(&Layout, i64) -> Option<crate::quiet::DueFlip> + Send + 'static,
    ) -> Self {
        self.held_due = Box::new(due);
        self.held_stands = Box::new(|_| true);
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

    /// Run [`Self::step`] until stopped via [`HostHandle::stop`].
    pub fn run(mut self) {
        while !self.stop.load(Ordering::Acquire) {
            self.step();
        }
    }

    /// Run [`Self::run`] on its own thread.
    /// # Errors
    /// The thread could not be spawned.
    pub fn spawn(self) -> io::Result<HostHandle> {
        let stop = Arc::clone(&self.stop);
        let ring = self.rest.stopper();
        let thread = std::thread::Builder::new()
            .name("atpkg-head-watch".into())
            .spawn(move || self.run())?;
        Ok(HostHandle { stop, ring, thread })
    }

    /// Run one iteration of the head-watch loop: one wait ([`Self::next_wait`]), then the
    /// looks in their order.
    pub fn step(&mut self) {
        let wall_before = self.clock.wall();
        let mono_before = self.clock.mono();
        let within = self.next_wait(wall_before, mono_before);
        let _told = self.clock.wait(&self.rest, &self.watch, within);
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
            (self.log)("vendor head watch: Automatic updates on \u{2014} watching");
            self.on = true;
            self.admit_after = Duration::ZERO;
        } else if woke_between(
            wall.duration_since(wall_before).ok(),
            self.clock.mono().saturating_sub(mono_before),
        ) {
            self.watch.woke(wall);
        }
        self.watch.set_exclude(&gate.exclude);
        self.queue.retain(|program| {
            !gate
                .exclude
                .iter()
                .any(|excluded| excluded.as_str() == *program)
        });
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
        if self.watch.absorb_handed() {
            // A waiter took the seat or the rendezvous and handed it over (or failed): the claim
            // takes it here, whatever the store's folder or the admission clock says, so the
            // hand-over that made this wait zero is gone before the next one. What it waited for
            // is free, so admission is asked at once; the seat stays this session's, and the
            // rendezvous goes again unless a look below takes it for its round.
            self.admit_after = Duration::ZERO;
            let _ = self.begin_update_look(wall);
            self.watch.end_session_update_look();
        }
        if self.check_held_move(wall) {
            return;
        }
        if self.check_index(wall) {
            return;
        }
        if (self.clock.mono() < self.admit_after || self.watch.admission_waits())
            && !self.watch.has_pending()
        {
            return;
        }
        let moved = self.watch.round(&self.layout, wall, &self.get, false);
        self.flush_notes();
        if let Some(programs) = moved {
            self.queue.extend(programs);
            if let Some(program) = self.queue.pop_front() {
                self.launch(program, wall);
            }
        }
    }

    /// How long the next wait may last (`None`: until the rest rings), from `wall` and `mono`
    /// before it: the earliest moment any look has work, so a due look is never hidden behind
    /// a later deadline, and none that is blocked waits zero twice. Every look that needs
    /// admission waits on the seat or hosts waiter's ring while one is armed, and on the
    /// admission retry while none could be; while admitted it is the earliest of the heads'
    /// deadline, the index probe's, a published index's retry ladder or pass spacing, and a held
    /// move's re-check while its record stands. Following a pass, or a sibling's pass holding a
    /// published index, is a [`BLIND_LOOK`]. A host whose files ring nothing, or (while it
    /// watches) whose sleeps ring nothing, waits at most [`BLIND_LOOK`].
    fn next_wait(&self, wall: SystemTime, mono: Duration) -> Option<Duration> {
        let unwatched = |wait: Option<Duration>| {
            if self.rest.watching() {
                wait
            } else {
                Some(wait.map_or(BLIND_LOOK, |w| w.min(BLIND_LOOK)))
            }
        };
        if self.following.is_some() {
            return Some(BLIND_LOOK);
        }
        if !self.on {
            // Automatic updates off: aterm.toml's change ends the wait.
            return unwatched(None);
        }
        if !self.queue.is_empty() || self.watch.admission_handed() {
            return Some(Duration::ZERO);
        }
        let session = self.watch.host == Some(Host::Session);
        if session && !self.watch.has_pending() {
            if self.watch.admission_waits() {
                // Every look needs admission: the waiter's ring, aterm.toml's change, or a
                // stop ends the wait, and no deadline stands in for them.
                return unwatched(None);
            }
            if mono < self.admit_after {
                return unwatched(Some(self.admit_after - mono));
            }
        }
        let mut next: Option<Duration> = None;
        let mut at = |deadline: Duration| {
            let wait = deadline.saturating_sub(mono);
            next = Some(next.map_or(wait, |n: Duration| n.min(wait)));
        };
        if let Some(due) = self.watch.next_due(wall) {
            at(mono.saturating_add(due.duration_since(wall).unwrap_or_default()));
        }
        if session {
            if let Some(due) = self.index.next_due() {
                at(due);
            }
            if let Some(due) = self.index_ready_due(wall, mono) {
                at(due);
            }
            if (self.held_stands)(&self.layout) {
                at(self.held.next_look().map_or(mono, |look| {
                    look.saturating_duration_since(self.held_origin)
                }));
            }
        }
        let next = if self.rest.hears_sleep() {
            next
        } else {
            Some(next.map_or(BLIND_LOOK, |n| n.min(BLIND_LOOK)))
        };
        unwatched(next)
    }

    /// When a published index this session owes a pass can next be offered one, on the
    /// monotonic clock: now when it is already verified, recorded or free to launch; its retry
    /// ladder's next rung while an earlier attempt holds it; a [`BLIND_LOOK`] while a sibling's
    /// pass installs; else the end of the machine's pass spacing. `None` with no index ready.
    fn index_ready_due(&self, wall: SystemTime, mono: Duration) -> Option<Duration> {
        let build = self.index.ready;
        if build == 0 || crate::index_probe::verified_floor(&self.layout) >= build {
            return (build != 0).then_some(mono);
        }
        if let Some(attempt) = self
            .index
            .last_attempt
            .filter(|attempt| attempt.holds(build, mono))
        {
            return Some(attempt.since.saturating_add(attempt.retry_after()));
        }
        let unix = unix_seconds(wall);
        let stamps = crate::status::pass_stamps(&self.layout, unix);
        if stamps.last_index_build >= build
            || !stamps.attempted_recently(unix)
            || stamps.completed_older_index_pass(build)
        {
            return Some(mono);
        }
        if stamps.in_flight {
            return Some(mono.saturating_add(BLIND_LOOK));
        }
        let spacing =
            i64::try_from(aterm_update_core::pkg_check::PASS_SPACING_SECS).unwrap_or(i64::MAX);
        let left = stamps
            .last_attempt
            .map_or(0, |then| then.saturating_add(spacing).saturating_sub(unix));
        Some(mono.saturating_add(Duration::from_secs(u64::try_from(left).unwrap_or(0).max(1))))
    }

    fn check_held_move(&mut self, wall: SystemTime) -> bool {
        let now = self.held_origin + self.clock.mono();
        if !self.held.look_due(now) {
            return false;
        }
        let owns_watch = self.begin_update_look(wall);
        if !owns_watch {
            return false;
        }
        let unix = unix_seconds(wall);
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
        if now < self.admit_after || self.watch.admission_waits() {
            return false;
        }
        let admitted = self.watch.begin_session_update_look(&self.layout, wall);
        self.flush_notes();
        self.admit_after = if admitted || self.watch.admission_waits() {
            now
        } else if self.watch.session_behind_window() {
            now.saturating_add(BLIND_LOOK)
        } else {
            now.saturating_add(CADENCE)
        };
        admitted
    }

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
                    let unix = unix_seconds(wall);
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
                                    "atpkg index {build} appeared; update pass did not start: \
                                     {error}"
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
        if self.index.due(now) {
            let owns_watch = self.begin_update_look(wall);
            if owns_watch {
                if !(self.index.eligible)(&self.layout) {
                    self.index.last_ineligible = Some(now);
                    self.watch.end_session_update_look();
                    return false;
                }
                self.index.last_ineligible = None;
                if let Err(error) = self.index.start(&self.layout, now, self.rest.ringer()) {
                    self.index.last_completed = Some(now);
                    (self.log)(&format!("atpkg index probe could not start: {error}"));
                }
                self.watch.end_session_update_look();
            }
        }
        false
    }

    fn launch_whole(&mut self) {
        match self.launcher.launch_whole() {
            Ok(pass) => {
                (self.log)(
                    "vendor head watch: held Trust toolchain move due \u{2014} update pass started",
                );
                self.following = Some(Following {
                    program: None,
                    index_build: None,
                    pass,
                    since: self.clock.mono(),
                });
            }
            Err(error) => (self.log)(&format!(
                "vendor head watch: held Trust toolchain move: update pass did not start: {error} \
                 \u{2014} the next update pass lands it"
            )),
        }
    }

    fn stand_down(&mut self, wall: SystemTime) {
        if self.on {
            (self.log)("vendor head watch: Automatic updates off \u{2014} standing down");
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

    fn follow(&mut self, wall: SystemTime) -> bool {
        let Some(following) = self.following.as_mut() else {
            return true;
        };
        let ended = following.pass.ended();
        let followed = self.clock.mono().saturating_sub(following.since);
        if ended.is_none() && followed >= PASS_FOLLOW_LIMIT {
            let subject = match (following.program, following.index_build) {
                (Some(program), _) => format!("vendor head watch: {program}"),
                (None, Some(build)) => format!("atpkg index {build}"),
                (None, None) => "vendor head watch: held Trust toolchain move".to_owned(),
            };
            self.following = None;
            (self.log)(&format!(
                "{subject} update pass has no observed exit after {} min \
                 \u{2014} left running, watching the heads again",
                followed.as_secs() / 60
            ));
            return true;
        }
        let Some(program) = following.program else {
            return match ended {
                Some(_) => {
                    let index_build = following.index_build;
                    self.following = None;
                    if let Some(build) = index_build {
                        (self.log)(&format!("atpkg index {build} update pass ended"));
                    } else {
                        (self.log)(
                            "vendor head watch: held Trust toolchain move: update pass ended",
                        );
                    }
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
                    format!("vendor head watch: {program} update pass reached the head")
                } else {
                    format!(
                        "vendor head watch: {program} update pass ended short of the head \
                         \u{2014} offered again"
                    )
                };
                (self.log)(&line);
                true
            }
            None => false,
        }
    }

    fn launch(&mut self, program: &'static str, wall: SystemTime) {
        match self.launcher.launch(program) {
            Ok(pass) => {
                (self.log)(&format!(
                    "vendor head watch: {program} head moved \u{2014} update pass started"
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
                    "vendor head watch: {program} update pass did not start: {error}"
                ));
                self.watch.note_pass_result(program, false, wall);
            }
        }
    }

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

/// The Unix second of `wall`, `0` before the epoch.
fn unix_seconds(wall: SystemTime) -> i64 {
    wall.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Handle to a runner on its own thread ([`Runner::spawn`]).
#[derive(Debug)]
pub struct HostHandle {
    stop: Arc<AtomicBool>,
    /// The runner's rest's stop ring.
    ring: Ring,
    thread: JoinHandle<()>,
}

impl HostHandle {
    /// Stop the runner at once, however long its wait, and wait for its thread to exit.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Release);
        self.ring.ring();
        let _ = self.thread.join();
    }
}

/// Start the head watch for a terminal session. Its rest hears `aterm.toml` and a held move's
/// two records; a terminal session registers no wake source (an IOKit client in a session that
/// job control stops would hold up every sleep of the Mac), so while it watches it keeps its
/// [`BLIND_LOOK`].
#[must_use]
pub fn run_host(
    layout: &Layout,
    gate: Gate,
    exe: PathBuf,
    lead: Vec<OsString>,
    mut log: impl FnMut(&str) + Send + 'static,
) -> Option<io::Result<HostHandle>> {
    let heads = super::for_host(Host::Session, &gate.exclude)?;
    let files = crate::config::config_path().into_iter().chain([
        crate::quiet::record_path(layout),
        crate::quiet::view_record_path(layout),
    ]);
    let rest = Rest::start("atpkg-head-watch-files", files);
    if !rest.watching() {
        log(&format!(
            "vendor head watch: no file watch \u{2014} aterm.toml and a held move's records \
             are looked at every {} s",
            BLIND_LOOK.as_secs()
        ));
    }
    let runner = Runner::new(
        layout.clone(),
        heads,
        Box::new(SystemClock::new()),
        Box::new(LiveSettings::new(gate)),
        Box::new(DetachedPasses::new(exe, lead, layout)),
        Box::new(log),
    )
    .with_rest(rest);
    Some(runner.spawn())
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
