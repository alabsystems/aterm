// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-GUI SUPERVISOR host: every Claude Code session this instance owns
//! is supervised by default, under the owner's `[harness]` policy
//! ([`aterm_agent::supervise::SupervisorConfig`]), with nothing typed to start
//! it. Sibling of [`crate::operator_host`], whose threading and shutdown shape
//! it copies: nothing here runs on the winit thread, and shutdown is an
//! explicit bounded join from `main_entry` after the event loop exits.
//!
//! **Discovery, without a poll.** One host thread (`aterm-harness-host`)
//! parks on [`ring`]: the store rings it when its membership moves
//! (`SessionStore::record_roster`), a session's timeline when its published
//! `program=` moves (`SessionTimeline::set_program`/`note_foreground_group`),
//! a supervisor claim when it is released or lapses, a worker when it ends,
//! and the host's own handle when the config changes or shutdown begins.
//! Each wake re-reads the store in-process: a session whose server-published
//! program is `claude` gets a worker; one whose program left, or that closed,
//! loses it. Codex is not supervised in this wave (journaled once per
//! session: `codex: not supervised yet`).
//!
//! **One worker per session** (`aterm-harness-<sid>`), running aterm-agent's
//! [`Session::run_hosted`] under [`SuperviseOpts::hosted_with`] over ONE
//! persistent connection to THIS instance's own control socket — the control
//! socket is the one interface; the token beside it authenticates, exactly as
//! for any other client. The LOOP claims the session, and only the loop: its
//! own lease (`meta set supervisor aterm-harness@<pid> ttl=…`, renewed by its
//! requests, given back holder-conditionally at its end — aterm-agent's
//! `claim.rs`), under this host's name ([`holder`], via
//! `Session::set_supervisor_name`). Another holder's live claim ends the loop
//! (`SuperviseOpts::yield_when_held`, [`CLAIM_HELD`]) and parks this session
//! until a claim is released or lapses — exactly one supervisor per session.
//! The transport ([`HostCtl`]) claims nothing: a second claim of the host's
//! own, under any name, would put its own loop behind it (measured on the
//! merged tree: `WATCHING another supervisor holds this session:
//! aterm-harness@<pid>`, and nothing pressed).
//!
//! **Faults.** Each run is caught ([`std::panic::catch_unwind`]) and a run
//! that fails or panics is restarted — after a growing pause
//! ([`restart_backoff`], on the bell, cut short by a stop) —
//! [`RESTART_BUDGET`] times an hour per SESSION (the count outlives a
//! worker, a program flap and the session leaving; a changed policy
//! forgives it); the next failure turns the session's supervisor OFF
//! (faulted) until the policy changes or the agent leaves and comes back,
//! and says so on its keyed attention (`owner=supervisor`), so the menu bar
//! shows it. The
//! panic itself is filed by `logging.rs` as a harness fault record, not as a
//! crash of aterm (the process keeps running).
//!
//! **Stopping.** A worker is stopped by its flag AND its connection's
//! interrupter: the parked wait ends at once, and the loop's badges are
//! cleared on a fresh connection ([`HostCtl`] redials after the cut for those
//! `meta` requests alone — a `key`, `send` or `turn` after the cut is refused
//! and never reaches the wire), so switching `[harness] enabled` off takes
//! effect within one wait, leaves no badge of its own behind, and presses
//! nothing once it is stopped.
//!
//! **Handoff.** A seamless update's outgoing instance suspends its host at
//! Commit ([`HostHandle::suspend`]); the incoming instance starts suspended
//! and resumes when the Commit activates it, so a session is supervised by
//! one process at a time.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_agent::supervise::{
    Ctl, CtlReply, Endpoint, Interrupter, RelayCtl, Session, SuperviseOpts, SupervisorConfig,
};

use crate::session_store::{SessionState, Store};

/// `cfg` with every key the engine does not read at its default: what the
/// host runs under and compares, so an edit that changes nothing the engine
/// reads restarts nothing. That is the one `[harness]` key that is not the
/// SUPERVISOR's at all: `upgrade`, the live agent upgrade's switch — read
/// from the same parse before it is masked ([`upgrade_enabled`]), so the
/// table has one parser.
pub(crate) fn effective(cfg: &SupervisorConfig) -> SupervisorConfig {
    let d = SupervisorConfig::default();
    SupervisorConfig {
        upgrade: d.upgrade,
        ..cfg.clone()
    }
}

/// Restarts allowed per session per [`RESTART_WINDOW`]; the next failure
/// marks the session's supervisor off (faulted). Counted per SESSION, across
/// its workers ([`FaultHistory`]): a program flap (ctrl-z, the agent's
/// `$EDITOR`) or a session leaving and coming back gives a crash-looping
/// engine no fresh budget; only a changed policy forgives it.
pub(crate) const RESTART_BUDGET: usize = 5;
/// The window [`RESTART_BUDGET`] counts over.
const RESTART_WINDOW: Duration = Duration::from_secs(3600);
/// How long shutdown waits for the workers (after cutting their waits), and
/// then for the host thread.
const WORKER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const HOST_JOIN_TIMEOUT: Duration = Duration::from_secs(3);
/// Every harness thread's name starts with this (`logging.rs` routes their
/// panics by it).
pub(crate) const THREAD_PREFIX: &str = "aterm-harness-";
/// The keyed-attention owner this host writes (`meta set attention
/// owner=supervisor …`): the engine's own, one constant.
pub(crate) use aterm_agent::supervise::ATTENTION_OWNER;
/// A loop that ended behind another holder's claim: aterm-agent's
/// [`aterm_agent::supervise::CLAIM_HELD`], followed by the holder.
use aterm_agent::supervise::CLAIM_HELD;

// ---------------------------------------------------------------------------
// The bell: what the host thread parks on.
// ---------------------------------------------------------------------------

/// A generation counter and its condition variable: [`ring`] bumps it, the
/// host waits for it to move past what it last saw. A leaf lock: nothing is
/// taken while it is held.
struct Bell {
    rung: Mutex<u64>,
    cv: Condvar,
}

static BELL: Bell = Bell {
    rung: Mutex::new(0),
    cv: Condvar::new(),
};

/// Bumped each time a supervisor claim is released or lapses: a session held
/// by another supervisor is retried only once this has moved.
static CLAIM_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Wake the host: something it decides from moved (the roster, a program, a
/// claim, a worker, the config). Cheap, never blocks on anything but the
/// bell's own leaf lock, so it is safe under the store's or a timeline's lock.
pub(crate) fn ring() {
    let mut rung = BELL.rung.lock().unwrap_or_else(PoisonError::into_inner);
    *rung = rung.wrapping_add(1);
    drop(rung);
    BELL.cv.notify_all();
}

/// A supervisor claim was released or lapsed: a session parked on another
/// holder's claim may be claimed now.
pub(crate) fn note_claim_released() {
    CLAIM_EPOCH.fetch_add(1, Ordering::SeqCst);
    ring();
}

fn bell_now() -> u64 {
    *BELL.rung.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Park on the bell until `done` is set or `deadline` passes; whether it was
/// set. Whoever sets a flag this waits on rings the bell after the store, and
/// the flag is read under the bell's lock, so no set is missed between the
/// read and the wait.
fn wait_done(done: &AtomicBool, deadline: Instant) -> bool {
    let mut rung = BELL.rung.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if done.load(Ordering::SeqCst) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        rung = BELL
            .cv
            .wait_timeout(rung, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// Park until the bell has moved past `seen`; returns where it stands.
fn bell_wait(seen: u64) -> u64 {
    let mut rung = BELL.rung.lock().unwrap_or_else(PoisonError::into_inner);
    while *rung == seen {
        rung = BELL.cv.wait(rung).unwrap_or_else(PoisonError::into_inner);
    }
    *rung
}

// ---------------------------------------------------------------------------
// The worker's transport: one connection that holds the session's claim.
// ---------------------------------------------------------------------------

/// The holder name this process claims sessions under.
pub(crate) fn holder() -> String {
    format!("aterm-harness@{}", std::process::id())
}

/// What a STOPPED worker is still allowed to send: a bare `meta` read and a
/// `meta unset …` — the loop's last act, reading and clearing the badges it
/// raised and giving its claim back. Everything else, above all
/// `key`/`send`/`paste`/`turn`/`feed-bin`, is refused after the cut
/// ([`HostCtl`]), so a stop that lands between the loop's last look and its
/// press can never type into the session.
fn cleanup_request(args: &[&str]) -> bool {
    let mut words = args.iter().copied().skip_while(|a| a.starts_with('@'));
    matches!(
        (words.next(), words.next()),
        (Some("meta"), None | Some("unset"))
    )
}

/// The answer to every non-cleanup request after the cut: the words
/// [`RelayCtl`] gives, so the loop reads it as the stop it is.
const STOPPED: &str = "the supervisor was stopped";

/// [`RelayCtl`] to this instance's own socket. It claims NOTHING (the loop
/// holds the session's claim, [`hosted_body`]); what it adds is the STOP:
/// after the interrupter cut the connection, only [`cleanup_request`]s go
/// out — on a FRESH connection, so the loop's last acts (clearing the badges
/// it raised, giving its claim back) still land — and every other request is
/// refused as [`RelayCtl`] refuses it: the interrupter ends the request in
/// flight and every one after it.
pub(crate) struct HostCtl {
    endpoint: Endpoint,
    token: Option<String>,
    inner: RelayCtl,
    cut: Arc<AtomicBool>,
    redialed: bool,
}

impl HostCtl {
    pub(crate) fn new(endpoint: Endpoint) -> Self {
        Self::with_token(endpoint, None)
    }

    /// [`Self::new`] with the token given up front (the tests' scratch
    /// servers); `None` reads it beside the socket, as every client does.
    fn with_token(endpoint: Endpoint, token: Option<String>) -> Self {
        Self {
            inner: RelayCtl::new(endpoint.clone(), token.clone()),
            endpoint,
            token,
            cut: Arc::default(),
            redialed: false,
        }
    }
}

impl Ctl for HostCtl {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        if self.cut.load(Ordering::SeqCst) {
            if !cleanup_request(args) {
                return Err(STOPPED.to_string());
            }
            if !self.redialed {
                self.inner = RelayCtl::new(self.endpoint.clone(), self.token.clone());
                self.redialed = true;
            }
        }
        self.inner.call(args)
    }

    fn interrupter(&self) -> Option<Interrupter> {
        let inner = self.inner.interrupter()?;
        let cut = Arc::clone(&self.cut);
        Some(Box::new(move || {
            cut.store(true, Ordering::SeqCst);
            inner();
        }))
    }
}

/// The loop's lines, into the log one by one (`harness @<sid>: …`).
struct LogLines {
    sid: String,
    buf: Vec<u8>,
}

impl Write for LogLines {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        while let Some(at) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=at).collect();
            let line = String::from_utf8_lossy(&line);
            aterm_log::info!("harness @{}: {}", self.sid, line.trim_end());
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Workers.
// ---------------------------------------------------------------------------

/// One session's failed runs inside [`RESTART_WINDOW`], oldest first, at most
/// [`RESTART_BUDGET`]` + 1` kept: shared by every worker the host starts for
/// the session until the policy changes or the entries age out.
pub(crate) type FaultHistory = Arc<Mutex<VecDeque<Instant>>>;

/// What one worker runs under.
pub(crate) struct WorkerJob {
    pub(crate) sid: String,
    pub(crate) opts: SuperviseOpts,
    pub(crate) stop: Arc<AtomicBool>,
    /// The live connection's cut, published by the body so a stop ends a
    /// parked wait at once.
    pub(crate) interrupt: Arc<Mutex<Option<Interrupter>>>,
    /// Set before `stop` when a new worker takes the session straight after
    /// (a changed policy): the loop leaves its badges for that one to adopt.
    pub(crate) handover: Arc<AtomicBool>,
    /// Clear the faulted badge a previous worker left before running.
    pub(crate) clear_badge: bool,
    /// The session's failed runs ([`FaultHistory`]), shared with the host.
    pub(crate) faults: FaultHistory,
}

/// How one run of the loop ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodyEnd {
    /// It was stopped.
    Stopped,
    /// Another supervisor holds the session's claim.
    Held(String),
    /// It ended with this reason (or panicked with this message).
    Failed(String),
}

/// How a worker thread ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkerExit {
    Stopped,
    Held,
    Faulted(String),
}

/// The agent a session's published program names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Agent {
    Claude,
    Codex,
}

type RosterFn = dyn Fn() -> Vec<(String, Agent)> + Send + Sync;
type WantedFn = dyn Fn(&str) -> bool + Send + Sync;
type BodyFn = dyn Fn(&WorkerJob) -> BodyEnd + Send + Sync;
type BadgeFn = dyn Fn(&str, Option<&str>) + Send + Sync;
type EpochFn = dyn Fn() -> u64 + Send + Sync;

/// The host's seams onto the process: which sessions run an agent, whether
/// one still does, one run of the loop, the faulted badge, and how many
/// supervisor claims have been released (a held session's retry gate). The
/// production set reads the store and talks to the control socket
/// ([`start_default`]); the tests inject their own.
#[derive(Clone)]
pub(crate) struct Hooks {
    pub(crate) roster: Arc<RosterFn>,
    pub(crate) still_wanted: Arc<WantedFn>,
    pub(crate) body: Arc<BodyFn>,
    pub(crate) badge: Arc<BadgeFn>,
    pub(crate) claim_epoch: Arc<EpochFn>,
}

/// Stop one worker: its flag, then its connection's cut.
fn request_stop(stop: &AtomicBool, interrupt: &Mutex<Option<Interrupter>>) {
    stop.store(true, Ordering::SeqCst);
    // A worker waiting out its restart backoff parks on the bell.
    ring();
    if let Some(cut) = interrupt
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        cut();
    }
}

/// The text of a panic payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panicked".to_string())
}

/// The badge a faulted session carries.
fn faulted_badge(why: &str) -> String {
    let why: String = why.split_whitespace().collect::<Vec<_>>().join(" ");
    // The whole badge within the server's keyed-attention cap (200 bytes).
    let mut end = why.len().min(80);
    while !why.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "supervisor off (faulted) until [harness] changes or the agent restarts: \
         {RESTART_BUDGET} restarts in an hour; last: {}",
        &why[..end]
    )
}

/// How long a worker waits before its `n`-th restart this hour (1-based):
/// a failure that repeats at once is not retried at once, so a transient
/// one of under a second no longer spends the whole budget in that second
/// and faults the session (the reliability review of 2026-09-24). The wait
/// is on the host's bell, cut short by a stop — nothing polls.
fn restart_backoff(n: usize) -> Duration {
    #[cfg(not(test))]
    const STEPS: [Duration; 5] = [
        Duration::from_secs(1),
        Duration::from_secs(5),
        Duration::from_secs(15),
        Duration::from_secs(30),
        Duration::from_secs(60),
    ];
    #[cfg(test)]
    const STEPS: [Duration; 5] = [
        Duration::from_millis(10),
        Duration::from_millis(20),
        Duration::from_millis(30),
        Duration::from_millis(40),
        Duration::from_millis(50),
    ];
    STEPS[n.saturating_sub(1).min(STEPS.len() - 1)]
}

/// Drop the entries of a session's fault history older than [`RESTART_WINDOW`].
fn age_faults(faults: &mut VecDeque<Instant>, now: Instant) {
    while faults
        .front()
        .is_some_and(|at| now.duration_since(*at) >= RESTART_WINDOW)
    {
        faults.pop_front();
    }
}

/// One worker thread's life: runs, restarts within the session's budget, and
/// ends stopped, held or faulted.
fn worker_main(mut job: WorkerJob, hooks: &Hooks) -> WorkerExit {
    loop {
        let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (hooks.body)(&job)))
            .unwrap_or_else(|payload| BodyEnd::Failed(panic_text(payload.as_ref())));
        job.clear_badge = false;
        *job.interrupt.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let why = match end {
            BodyEnd::Stopped if job.stop.load(Ordering::SeqCst) => return WorkerExit::Stopped,
            BodyEnd::Held(other) => {
                aterm_log::info!("harness @{}: not supervised: {CLAIM_HELD}{other}", job.sid);
                return WorkerExit::Held;
            }
            BodyEnd::Stopped => "the loop ended unasked".to_string(),
            BodyEnd::Failed(why) => why,
        };
        if job.stop.load(Ordering::SeqCst) || !(hooks.still_wanted)(&job.sid) {
            return WorkerExit::Stopped;
        }
        let now = Instant::now();
        let failed = {
            let mut faults = job.faults.lock().unwrap_or_else(PoisonError::into_inner);
            age_faults(&mut faults, now);
            faults.push_back(now);
            while faults.len() > RESTART_BUDGET + 1 {
                faults.pop_front();
            }
            faults.len()
        };
        if failed > RESTART_BUDGET {
            aterm_log::warn!(
                "harness @{}: supervisor off (faulted) after {RESTART_BUDGET} restarts in an \
                 hour; last: {why}",
                job.sid
            );
            (hooks.badge)(&job.sid, Some(&faulted_badge(&why)));
            return WorkerExit::Faulted(why);
        }
        let pause = restart_backoff(failed);
        aterm_log::warn!(
            "harness @{}: restarting the supervisor ({failed} of {RESTART_BUDGET} this hour) in \
             {} ms: {why}",
            job.sid,
            pause.as_millis(),
        );
        if wait_done(&job.stop, Instant::now() + pause) {
            return WorkerExit::Stopped;
        }
    }
}

struct Worker {
    join: Option<JoinHandle<WorkerExit>>,
    /// Set (then the bell rung) as the thread's last act: it is reaped, and
    /// joined without a wait of any length that matters.
    done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handover: Arc<AtomicBool>,
    interrupt: Arc<Mutex<Option<Interrupter>>>,
    cfg_gen: u64,
    /// The claim epoch when it started: a held session is retried only past it.
    epoch: u64,
}

impl Worker {
    fn stop(&self) {
        request_stop(&self.stop, &self.interrupt);
    }

    /// Stop it for a worker that takes the session straight after: its
    /// badges stay for that one to adopt, never cleared and raised again
    /// (the reliability review of 2026-09-24: a policy edit dropped every
    /// idle-point escalation and re-notified every box).
    fn hand_over(&self) {
        self.handover.store(true, Ordering::SeqCst);
        self.stop();
    }
}

/// The host thread's workers; dropping it (a panic in the host) stops them.
#[derive(Default)]
struct Workers(HashMap<String, Worker>);

impl Drop for Workers {
    fn drop(&mut self) {
        for w in self.0.values() {
            w.stop();
        }
    }
}

// ---------------------------------------------------------------------------
// The host.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct State {
    cfg: SupervisorConfig,
    /// Bumped by every [`HostHandle::set_config`] that changed the policy.
    cfg_gen: u64,
    suspended: bool,
    shutting_down: bool,
    /// The sessions with a live worker, as the host thread last left them.
    live: Vec<String>,
    /// Their stop flags and interrupters, published under this lock with the
    /// starts that made them, so [`HostHandle::suspend`] stops every one.
    stops: Vec<StopHandle>,
}

/// A worker's stop flag and its connection's interrupter slot.
type StopHandle = (Arc<AtomicBool>, Arc<Mutex<Option<Interrupter>>>);

struct Shared {
    state: Mutex<State>,
    headless: bool,
    /// Every session's [`FaultHistory`], kept by the host thread across its
    /// workers (taken alone, or under `state` — never the other way).
    faults: Mutex<HashMap<String, FaultHistory>>,
    /// Set (then the bell rung) when the host thread's loop has returned.
    host_done: AtomicBool,
    /// Host threads started again after one ended in a panic
    /// ([`HOST_RESTARTS`] at most).
    host_restarts: AtomicU64,
}

/// `[harness] enabled && upgrade` as the window last parsed it: the live
/// agent upgrade's gate (`aterm_agent::harness::upgrade_drive::host` asks it
/// every tick), set at the host's start and at every policy change — one
/// parser of the table in this process, not a second line reader of the file
/// (the laws review of 2026-09-24). A launch that could not load the file
/// starts escalate-only, whose `upgrade` is off.
static UPGRADE_ENABLED: AtomicBool = AtomicBool::new(false);

/// [`UPGRADE_ENABLED`]'s value.
pub(crate) fn upgrade_enabled() -> bool {
    UPGRADE_ENABLED.load(Ordering::SeqCst)
}

/// The live upgrade's gate under `cfg`: the master switch and its own.
fn upgrade_gate(cfg: &SupervisorConfig) -> bool {
    cfg.enabled && cfg.upgrade
}

fn note_upgrade_switch(cfg: &SupervisorConfig) {
    UPGRADE_ENABLED.store(upgrade_gate(cfg), Ordering::SeqCst);
}

/// How many times a host thread that ended in a panic is started again (by
/// the next policy change or resume, [`HostHandle::ensure_thread`]) before
/// supervision stays off for the process.
const HOST_RESTARTS: u64 = 3;

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the policy supervises anything in this instance now.
    fn active(state: &State, headless: bool) -> bool {
        state.cfg.enabled && (!headless || state.cfg.headless) && !state.suspended
    }
}

/// The host's handle, held by `App` and `main_entry`.
#[derive(Clone)]
pub(crate) struct HostHandle {
    shared: Arc<Shared>,
    thread: Arc<Mutex<Option<JoinHandle<()>>>>,
    hooks: Hooks,
}

impl HostHandle {
    /// A host under `cfg`. Its thread starts only once the policy is active
    /// (`[harness] enabled`, and `headless = true` in a headless instance, and
    /// not `suspended`): a test or agent-private instance runs no host at all.
    pub(crate) fn start(
        cfg: SupervisorConfig,
        headless: bool,
        suspended: bool,
        hooks: Hooks,
    ) -> Self {
        note_upgrade_switch(&cfg);
        let handle = Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    cfg: effective(&cfg),
                    suspended,
                    ..State::default()
                }),
                headless,
                faults: Mutex::default(),
                host_done: AtomicBool::new(false),
                host_restarts: AtomicU64::new(0),
            }),
            thread: Arc::default(),
            hooks,
        };
        handle.ensure_thread();
        handle
    }

    /// Whether the host thread runs.
    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// The sessions with a live worker, as the host thread last left them.
    #[cfg(test)]
    fn live(&self) -> Vec<String> {
        self.shared.lock().live.clone()
    }

    /// How many failed runs `sid`'s fault history holds.
    #[cfg(test)]
    fn faults_of(&self, sid: &str) -> usize {
        let map = self
            .shared
            .faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        map.get(sid).map_or(0, |h| {
            h.lock().unwrap_or_else(PoisonError::into_inner).len()
        })
    }

    fn ensure_thread(&self) {
        let state = self.shared.lock();
        if !Shared::active(&state, self.shared.headless) || state.shutting_down {
            return;
        }
        drop(state);
        let mut slot = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = slot.as_ref() {
            // The host loop returns only at shutdown (checked above), so a
            // thread that has FINISHED ended in a panic: every worker it ran
            // was stopped with it and nothing was supervised any more — the
            // slot still held its handle and nothing started another (the
            // reliability review of 2026-09-24). Start it again, within a
            // budget.
            if !running.is_finished()
                || self.shared.host_restarts.fetch_add(1, Ordering::SeqCst) >= HOST_RESTARTS
            {
                return;
            }
            if let Some(ended) = slot.take() {
                let _ = ended.join();
            }
            self.shared.host_done.store(false, Ordering::SeqCst);
            aterm_log::warn!("harness host started again after a panic");
        }
        let shared = Arc::clone(&self.shared);
        let shared_done = Arc::clone(&self.shared);
        let hooks = self.hooks.clone();
        match std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}host"))
            .spawn(move || {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    host_loop(&shared, &hooks);
                }));
                if let Err(payload) = run {
                    aterm_log::warn!(
                        "harness host stopped after an internal panic: {}",
                        panic_text(payload.as_ref())
                    );
                }
                shared_done.host_done.store(true, Ordering::SeqCst);
                ring();
            }) {
            Ok(join) => *slot = Some(join),
            Err(e) => aterm_log::warn!("harness host could not start: {e}; nothing is supervised"),
        }
    }

    /// A reloaded `[harness]` policy: workers restart under it, or all stop
    /// (within one wait) when it is off.
    pub(crate) fn set_config(&self, cfg: SupervisorConfig) {
        note_upgrade_switch(&cfg);
        let cfg = effective(&cfg);
        let mut state = self.shared.lock();
        if state.cfg == cfg {
            return;
        }
        state.cfg = cfg;
        state.cfg_gen += 1;
        drop(state);
        self.ensure_thread();
        ring();
    }

    /// Stop every worker and supervise nothing until [`Self::resume`]: a
    /// seamless update's Commit hands the sessions to the successor.
    ///
    /// SYNCHRONOUS where it matters and never blocking: every live worker's
    /// flag is set and its connection cut before this returns, and a cut
    /// [`HostCtl`] refuses every request but its badge cleanup — so from
    /// here no worker presses or types (a request already on the wire is the
    /// one exception). The host thread starts workers only under the same
    /// lock hold that reads `suspended`, so none starts after it either. The
    /// threads themselves are reaped later, off this caller.
    pub(crate) fn suspend(&self) {
        let mut state = self.shared.lock();
        state.suspended = true;
        for (stop, interrupt) in &state.stops {
            request_stop(stop, interrupt);
        }
        drop(state);
        ring();
    }

    /// Supervise again (the successor's Commit activated it, or a Commit
    /// failed and this process keeps its sessions).
    pub(crate) fn resume(&self) {
        self.shared.lock().suspended = false;
        self.ensure_thread();
        ring();
    }

    /// Stop every worker and the host thread, bounded: past
    /// [`HOST_JOIN_TIMEOUT`] the thread is left to the process exit.
    pub(crate) fn shutdown_and_join(&self) {
        self.shared.lock().shutting_down = true;
        ring();
        let join = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(join) = join {
            if wait_done(&self.shared.host_done, Instant::now() + HOST_JOIN_TIMEOUT) {
                let _ = join.join();
            } else {
                aterm_log::warn!("harness host did not stop within its bound; left to the exit");
            }
        }
    }
}

fn spawn_worker(
    sid: &str,
    job: WorkerJob,
    done: Arc<AtomicBool>,
    hooks: &Hooks,
) -> Option<JoinHandle<WorkerExit>> {
    let hooks = hooks.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}{sid}"))
        .spawn(move || {
            let exit = worker_main(job, &hooks);
            done.store(true, Ordering::SeqCst);
            ring();
            exit
        });
    match spawned {
        Ok(join) => Some(join),
        Err(e) => {
            aterm_log::warn!("harness @{sid}: could not start a supervisor thread: {e}");
            None
        }
    }
}

/// Clear a faulted badge off-thread (the session's agent left).
fn clear_badge_later(sid: &str, hooks: &Hooks) {
    let badge = Arc::clone(&hooks.badge);
    let owned = sid.to_string();
    let _ = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}{sid}-clear"))
        .spawn(move || badge(&owned, None));
}

fn host_loop(shared: &Shared, hooks: &Hooks) {
    let mut workers = Workers::default();
    // sid -> the policy generation it faulted under: off until the policy
    // changes or the agent leaves.
    let mut faulted: HashMap<String, u64> = HashMap::new();
    let mut badged: HashSet<String> = HashSet::new();
    // sid -> the claim epoch its refused worker started at.
    let mut held: HashMap<String, u64> = HashMap::new();
    let mut codex_told: HashSet<String> = HashSet::new();
    let mut forgiven_gen = shared.lock().cfg_gen;
    let mut seen = bell_now();
    loop {
        let (cfg, cfg_gen, active, shutting_down) = {
            let state = shared.lock();
            (
                state.cfg.clone(),
                state.cfg_gen,
                Shared::active(&state, shared.headless),
                state.shutting_down,
            )
        };
        // Reap the workers that ended (their last act set `done`, so the
        // join below waits on nothing but the thread's return).
        let ended: Vec<String> = workers
            .0
            .iter()
            .filter(|(_, w)| w.join.is_none() || w.done.load(Ordering::SeqCst))
            .map(|(sid, _)| sid.clone())
            .collect();
        for sid in ended {
            let Some(mut w) = workers.0.remove(&sid) else {
                continue;
            };
            let exit = w.join.take().map(JoinHandle::join);
            match exit {
                Some(Ok(WorkerExit::Faulted(_))) => {
                    faulted.insert(sid.clone(), w.cfg_gen);
                    badged.insert(sid);
                }
                Some(Ok(WorkerExit::Held)) => {
                    held.insert(sid, w.epoch);
                }
                Some(Ok(WorkerExit::Stopped)) | None => {}
                Some(Err(_)) => {
                    // worker_main catches every run; a panic past it is the
                    // thread's own bookkeeping, and counts as a fault.
                    faulted.insert(sid.clone(), w.cfg_gen);
                }
            }
        }
        if shutting_down {
            shutdown_workers(workers);
            return;
        }
        let roster = if active { (hooks.roster)() } else { Vec::new() };
        let mut wanted: HashSet<String> = HashSet::new();
        let mut codex: HashSet<String> = HashSet::new();
        for (sid, agent) in roster {
            match agent {
                Agent::Claude => {
                    wanted.insert(sid);
                }
                Agent::Codex => {
                    if !codex_told.contains(&sid) {
                        aterm_log::info!("harness @{sid}: codex: not supervised yet");
                    }
                    codex.insert(sid);
                }
            }
        }
        codex_told = codex;
        faulted.retain(|sid, _| wanted.contains(sid));
        held.retain(|sid, _| wanted.contains(sid));
        {
            // A changed policy forgives every session's failures; otherwise a
            // history outlives its workers (and a flap of the program) until
            // its entries age out of the window.
            let mut histories = shared.faults.lock().unwrap_or_else(PoisonError::into_inner);
            if cfg_gen != forgiven_gen {
                histories.clear();
                forgiven_gen = cfg_gen;
            }
            let now = Instant::now();
            histories.retain(|sid, history| {
                let mut h = history.lock().unwrap_or_else(PoisonError::into_inner);
                age_faults(&mut h, now);
                !h.is_empty() || wanted.contains(sid) || workers.0.contains_key(sid)
            });
        }
        let gone: Vec<String> = badged
            .iter()
            .filter(|sid| !wanted.contains(*sid))
            .cloned()
            .collect();
        for sid in gone {
            badged.remove(&sid);
            clear_badge_later(&sid, hooks);
        }
        for (sid, w) in &workers.0 {
            if !wanted.contains(sid) {
                w.stop();
            } else if w.cfg_gen != cfg_gen {
                w.hand_over();
            }
        }
        let epoch = (hooks.claim_epoch)();
        let mut starts: Vec<&String> = wanted
            .iter()
            .filter(|sid| !workers.0.contains_key(*sid))
            .filter(|sid| faulted.get(*sid).is_none_or(|g| *g != cfg_gen))
            .filter(|sid| held.get(*sid).is_none_or(|e| epoch > *e))
            .collect();
        starts.sort();
        // Starts happen under the one hold of the state lock that re-reads
        // the policy and publishes the stop handles: a suspend() either came
        // first (nothing starts) or finds every worker started here.
        let mut state = shared.lock();
        if !Shared::active(&state, shared.headless) || state.shutting_down {
            starts.clear();
        }
        for sid in starts {
            held.remove(sid);
            faulted.remove(sid);
            let faults = Arc::clone(
                shared
                    .faults
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .entry(sid.clone())
                    .or_default(),
            );
            let job = WorkerJob {
                sid: sid.clone(),
                opts: hosted_opts(&cfg, sid, aterm_types::dirs::state_dir().as_deref()),
                stop: Arc::default(),
                interrupt: Arc::default(),
                handover: Arc::default(),
                clear_badge: badged.remove(sid),
                faults,
            };
            let (stop, interrupt) = (Arc::clone(&job.stop), Arc::clone(&job.interrupt));
            let handover = Arc::clone(&job.handover);
            let done = Arc::new(AtomicBool::new(false));
            if let Some(join) = spawn_worker(sid, job, Arc::clone(&done), hooks) {
                aterm_log::info!("harness @{sid}: supervising");
                workers.0.insert(
                    sid.clone(),
                    Worker {
                        join: Some(join),
                        done,
                        stop,
                        handover,
                        interrupt,
                        cfg_gen,
                        epoch,
                    },
                );
            }
        }
        let mut live: Vec<String> = workers.0.keys().cloned().collect();
        live.sort();
        state.live = live;
        state.stops = workers
            .0
            .values()
            .map(|w| (Arc::clone(&w.stop), Arc::clone(&w.interrupt)))
            .collect();
        drop(state);
        seen = bell_wait(seen);
    }
}

/// A worker's options: [`SuperviseOpts::hosted_with`] the policy, with the
/// session's journal beside its approval ledger
/// ([`aterm_agent::supervise::approvals::journal_beside`]) — the lines only
/// the journal carries (`SKIPPED`, `WAITING`, `ESCALATED … attention=`),
/// readable for every supervised session (the live E2E's D5).
fn hosted_opts(cfg: &SupervisorConfig, sid: &str, state: Option<&Path>) -> SuperviseOpts {
    use aterm_agent::supervise::approvals;
    SuperviseOpts {
        journal: state
            .and_then(|root| approvals::path_under(root, Some(sid)))
            .map(|ledger| approvals::journal_beside(&ledger)),
        ..SuperviseOpts::hosted_with(cfg)
    }
}

/// Stop every worker (flag and cut), then join them within
/// [`WORKER_JOIN_TIMEOUT`]; one past it is left to the process exit.
fn shutdown_workers(mut workers: Workers) {
    for w in workers.0.values() {
        w.stop();
    }
    let deadline = Instant::now() + WORKER_JOIN_TIMEOUT;
    for (sid, w) in &mut workers.0 {
        let Some(join) = w.join.take() else {
            continue;
        };
        if wait_done(&w.done, deadline) {
            let _ = join.join();
        } else {
            aterm_log::warn!("harness @{sid}: the supervisor did not stop within its bound");
        }
    }
}

// ---------------------------------------------------------------------------
// Production seams.
// ---------------------------------------------------------------------------

/// The agent a session's publication says it is: its program BY NAME
/// ([`aterm_phase::program_of`], the one name table), else the agent its
/// last verdict's reader identified by the screen — a Claude Code started as
/// `node` (the laws review of 2026-09-24: the server published its verdict
/// and raised its rows while this host, keyed on the argv0 word alone,
/// never supervised it).
pub(crate) fn agent_of(publication: &crate::session_timeline::AgentPublication) -> Option<Agent> {
    let program = publication
        .program
        .as_deref()
        .and_then(aterm_phase::program_of)
        .or(publication.reader)?;
    match program {
        aterm_phase::Program::Claude => Some(Agent::Claude),
        aterm_phase::Program::Codex => Some(Agent::Codex),
        aterm_phase::Program::Generic => None,
    }
}

/// Every live session whose server-published program is an agent, read
/// in-process: the store's handles, then each one's timeline.
pub(crate) fn roster_of(store: &Store) -> Vec<(String, Agent)> {
    let handles = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .snapshot();
    handles
        .iter()
        .filter(|h| h.state != SessionState::Exited)
        .filter_map(|h| {
            let tl = h
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let agent = agent_of(tl.agent())?;
            Some((h.sid.as_str().to_string(), agent))
        })
        .collect()
}

/// Whether `sid` is still a live Claude session.
fn still_claude(store: &Store, sid: &str) -> bool {
    let guard = store.read().unwrap_or_else(PoisonError::into_inner);
    let Some(h) = guard.by_sid(&aterm_session::SessionId::new(sid)) else {
        return false;
    };
    if h.state == SessionState::Exited {
        return false;
    }
    let tl = h
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    agent_of(tl.agent()) == Some(Agent::Claude)
}

/// One run of the loop over this instance's own socket.
fn hosted_body(sock: &str, job: &WorkerJob) -> BodyEnd {
    let mut ctl = HostCtl::new(Endpoint::Socket(sock.to_string()));
    *job.interrupt.lock().unwrap_or_else(PoisonError::into_inner) = ctl.interrupter();
    if job.stop.load(Ordering::SeqCst) {
        return BodyEnd::Stopped;
    }
    let sel = format!("@{}", job.sid);
    if job.clear_badge {
        let owner = format!("owner={ATTENTION_OWNER}");
        if let Err(e) = ctl.call(&[&sel, "meta", "unset", "attention", &owner]) {
            return held_or_failed(e);
        }
    }
    let mut out = LogLines {
        sid: job.sid.clone(),
        buf: Vec::new(),
    };
    // The loop takes the session's claim under this host's name, renews it
    // and gives it back holder-conditionally as it ends (a stop included:
    // `meta unset` is a cleanup request the cut still lets out).
    let mut session = Session::new(&mut ctl, Some(sel));
    session.set_supervisor_name(Some(holder()));
    session.set_handover(Arc::clone(&job.handover));
    let result = session.run_hosted(&job.opts, Arc::clone(&job.stop), &mut out);
    match result {
        Ok(()) => BodyEnd::Stopped,
        Err(e) => held_or_failed(e),
    }
}

fn held_or_failed(e: String) -> BodyEnd {
    match e.find(CLAIM_HELD) {
        Some(at) => BodyEnd::Held(e[at + CLAIM_HELD.len()..].trim().to_string()),
        None => BodyEnd::Failed(e),
    }
}

/// Set (`Some`) or clear the session's `owner=supervisor` attention.
fn set_badge(sock: &str, sid: &str, text: Option<&str>) {
    let mut ctl = RelayCtl::new(Endpoint::Socket(sock.to_string()), None);
    let sel = format!("@{sid}");
    let owner = format!("owner={ATTENTION_OWNER}");
    let reply = match text {
        Some(t) => ctl.call(&[&sel, "meta", "set", "attention", &owner, t]),
        None => ctl.call(&[&sel, "meta", "unset", "attention", &owner]),
    };
    match reply {
        Ok(r) if r.ok() => {}
        Ok(r) => aterm_log::warn!("harness @{sid}: badge not written: {}", r.stderr.trim()),
        Err(e) => aterm_log::warn!("harness @{sid}: badge not written: {e}"),
    }
}

/// The production host: this process's store, its own control socket.
pub(crate) fn start_default(
    store: Store,
    sock: String,
    cfg: SupervisorConfig,
    headless: bool,
    suspended: bool,
) -> HostHandle {
    let (s1, s2) = (store.clone(), store);
    let (k1, k2) = (sock.clone(), sock);
    HostHandle::start(
        cfg,
        headless,
        suspended,
        Hooks {
            roster: Arc::new(move || roster_of(&s1)),
            still_wanted: Arc::new(move |sid| still_claude(&s2, sid)),
            body: Arc::new(move |job| hosted_body(&k1, job)),
            badge: Arc::new(move |sid, text| set_badge(&k2, sid, text)),
            claim_epoch: Arc::new(|| CLAIM_EPOCH.load(Ordering::SeqCst)),
        },
    )
}

/// Remove the hooks an older aterm wrote into the user's Claude Code
/// settings, off the winit thread, whatever `agents_auto_prime` says: no
/// vendor hook is installed into the agent any more (the supervisor reads the
/// screen), and one left behind runs a retired command at every turn.
pub(crate) fn sweep_legacy_hooks() {
    let Some(home) = aterm_primer::home_dir() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}hook-sweep"))
        .spawn(move || match aterm_primer::remove_aterm_hooks_in(&home) {
            Ok(aterm_primer::HookRemoval::Nothing) => {}
            Ok(removal) => aterm_log::info!("harness: legacy Claude hooks removed: {removal:?}"),
            Err(e) => aterm_log::warn!("harness: legacy Claude hooks left in place: {e}"),
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// The live upgrade is gated by the window's parsed `[harness]` table —
    /// `enabled && upgrade` — not by a second line reader of the file; a
    /// launch that could not load the file (escalate-only) sweeps nothing.
    #[test]
    fn the_upgrade_gate_is_the_parsed_table() {
        assert!(upgrade_gate(&on()));
        assert!(!upgrade_gate(&off()));
        let mut no_upgrade = on();
        no_upgrade.set("upgrade", "false").unwrap();
        assert!(!upgrade_gate(&no_upgrade));
        assert!(!upgrade_gate(&SupervisorConfig::escalate_only()));
    }

    /// The laws review of 2026-09-24: the host keyed its roster on the argv0
    /// word (`claude`|`codex`) while the server identified a Claude Code
    /// started as `node` by its frame — the rows were raised and nothing was
    /// supervised. The roster now reads aterm-phase's one name table, then
    /// the reader the server's verdict used. NEGATIVE CONTROLS: a shell with
    /// no identified reader, and a runtime whose screen read as no agent,
    /// are not supervised; a name wins over a stale reader.
    /// The live E2E's D5: the hosted loop kept no journal, so its SKIPPED,
    /// WAITING and ESCALATED lines — a refused attention, a guard that never
    /// matched — were visible nowhere. Every worker now journals beside its
    /// session's ledger. NEGATIVE CONTROL: with no state root, none (and the
    /// policy rides unchanged either way).
    #[test]
    fn every_worker_journals_beside_its_ledger() {
        let dir = std::env::temp_dir().join(format!("aterm-hh-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = SupervisorConfig::default();
        let opts = hosted_opts(&cfg, "s-0123", Some(&dir));
        assert_eq!(
            opts.journal.as_deref(),
            Some(dir.join("drive").join("s-0123.journal.jsonl").as_path())
        );
        assert_eq!(opts.policy, cfg);
        assert_eq!(hosted_opts(&cfg, "s-0123", None).journal, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_roster_takes_an_agent_identified_by_name_or_by_its_screen() {
        use crate::session_timeline::AgentPublication;
        use aterm_phase::Program;
        let publication = |program: Option<&str>, reader: Option<Program>| AgentPublication {
            program: program.map(str::to_string),
            reader,
            ..AgentPublication::default()
        };
        assert_eq!(
            agent_of(&publication(Some("claude"), None)),
            Some(Agent::Claude)
        );
        assert_eq!(
            agent_of(&publication(Some("claude-code"), None)),
            Some(Agent::Claude)
        );
        assert_eq!(
            agent_of(&publication(Some("codex"), None)),
            Some(Agent::Codex)
        );
        assert_eq!(
            agent_of(&publication(Some("node"), Some(Program::Claude))),
            Some(Agent::Claude),
            "a Claude Code started as node, identified by its frame"
        );
        assert_eq!(agent_of(&publication(Some("zsh"), None)), None);
        assert_eq!(agent_of(&publication(Some("node"), None)), None);
        assert_eq!(
            agent_of(&publication(Some("node"), Some(Program::Generic))),
            None
        );
        assert_eq!(
            agent_of(&publication(Some("codex"), Some(Program::Claude))),
            Some(Agent::Codex)
        );
    }

    /// A fake roster the test moves, plus the calls the seams saw.
    #[derive(Default)]
    struct World {
        roster: Mutex<Vec<(String, Agent)>>,
        runs: AtomicUsize,
        badges: Mutex<Vec<(String, Option<String>)>>,
        /// This world's own claim epoch: the process-wide one moves with
        /// every other test's claim release.
        released: AtomicU64,
    }

    impl World {
        fn set(&self, roster: &[(&str, Agent)]) {
            *self.roster.lock().unwrap() =
                roster.iter().map(|(s, a)| ((*s).to_string(), *a)).collect();
            ring();
        }
    }

    /// A body that parks until it is stopped — through the interrupter it
    /// publishes, the path a real stop takes.
    fn parking_body(world: &Arc<World>) -> Arc<BodyFn> {
        let world = Arc::clone(world);
        Arc::new(move |job: &WorkerJob| {
            world.runs.fetch_add(1, Ordering::SeqCst);
            let me = std::thread::current();
            *job.interrupt.lock().unwrap() = Some(Box::new(move || me.unpark()));
            while !job.stop.load(Ordering::SeqCst) {
                std::thread::park();
            }
            BodyEnd::Stopped
        })
    }

    fn hooks(world: &Arc<World>, body: Arc<BodyFn>) -> Hooks {
        let (w1, w2, w3, w4) = (
            Arc::clone(world),
            Arc::clone(world),
            Arc::clone(world),
            Arc::clone(world),
        );
        Hooks {
            roster: Arc::new(move || w1.roster.lock().unwrap().clone()),
            still_wanted: Arc::new(move |sid| {
                w2.roster
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(s, a)| s == sid && *a == Agent::Claude)
            }),
            body,
            badge: Arc::new(move |sid, text| {
                w3.badges
                    .lock()
                    .unwrap()
                    .push((sid.to_string(), text.map(str::to_string)));
            }),
            claim_epoch: Arc::new(move || w4.released.load(Ordering::SeqCst)),
        }
    }

    fn until(what: &str, pred: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pred() {
            assert!(Instant::now() < deadline, "not within 5 s: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn on() -> SupervisorConfig {
        SupervisorConfig::default()
    }

    fn off() -> SupervisorConfig {
        let mut c = SupervisorConfig::default();
        c.set("enabled", "false").unwrap();
        c
    }

    #[test]
    fn config_off_runs_no_host_and_no_workers() {
        let world = Arc::new(World::default());
        world.set(&[("s-a", Agent::Claude)]);
        let host = HostHandle::start(off(), false, false, hooks(&world, parking_body(&world)));
        assert!(!host.is_running(), "enabled=false starts no host thread");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), 0);
        // NEGATIVE CONTROL: the same world under the default policy runs one.
        host.set_config(on());
        assert!(host.is_running());
        until("a worker for s-a", || host.live() == ["s-a"]);
        host.shutdown_and_join();
    }

    #[test]
    fn a_headless_instance_runs_no_host_unless_the_policy_says_headless() {
        let world = Arc::new(World::default());
        world.set(&[("s-h", Agent::Claude)]);
        let host = HostHandle::start(on(), true, false, hooks(&world, parking_body(&world)));
        assert!(
            !host.is_running(),
            "headless without [harness] headless = true"
        );
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), 0);
        let mut headless = on();
        headless.set("headless", "true").unwrap();
        let host2 = HostHandle::start(headless, true, false, hooks(&world, parking_body(&world)));
        assert!(host2.is_running());
        until("a worker for s-h", || host2.live() == ["s-h"]);
        host2.shutdown_and_join();
        host.shutdown_and_join();
    }

    #[test]
    fn workers_follow_the_published_program_and_codex_is_not_supervised() {
        let world = Arc::new(World::default());
        world.set(&[("s-c", Agent::Claude), ("s-x", Agent::Codex)]);
        let host = HostHandle::start(on(), false, false, hooks(&world, parking_body(&world)));
        until("claude only", || host.live() == ["s-c"]);
        // The program left: the worker is stopped (through its interrupter).
        world.set(&[("s-x", Agent::Codex)]);
        until("detached", || host.live().is_empty());
        world.set(&[("s-c", Agent::Claude)]);
        until("attached again", || host.live() == ["s-c"]);
        assert_eq!(world.runs.load(Ordering::SeqCst), 2);
        host.shutdown_and_join();
    }

    #[test]
    fn a_reload_restarts_workers_and_switching_off_stops_them() {
        let world = Arc::new(World::default());
        world.set(&[("s-r", Agent::Claude)]);
        let host = HostHandle::start(on(), false, false, hooks(&world, parking_body(&world)));
        until("first worker", || world.runs.load(Ordering::SeqCst) == 1);
        // An unchanged policy restarts nothing (negative control)…
        host.set_config(on());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), 1);
        // …a changed one restarts the worker under it…
        let mut changed = on();
        changed.set("dismiss_surveys", "false").unwrap();
        host.set_config(changed);
        until("restarted", || world.runs.load(Ordering::SeqCst) == 2);
        // …and enabled=false stops it.
        host.set_config(off());
        until("stopped", || host.live().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), 2);
        host.shutdown_and_join();
    }

    /// EVERY supervisor key reaches the engine (the engine lane's merge
    /// wired the eight a "not yet applied" list once held): each survives
    /// [`effective`] into the worker's `SuperviseOpts::hosted_with` — the
    /// trust roots into the policy the approval context is built from, the
    /// continue/retry/fallback/compaction switches into the policy the
    /// turn-end decider reads — and editing one restarts the worker. A key
    /// that is not the supervisor's (`upgrade`, masked by [`effective`])
    /// restarts nothing. NEGATIVE CONTROL: the `upgrade` edit is masked, so
    /// the worker count holds until an engine key moves.
    #[test]
    fn every_supervisor_key_reaches_the_engine_and_only_those_restart_it() {
        let world = Arc::new(World::default());
        world.set(&[("s-i", Agent::Claude)]);
        let host = HostHandle::start(on(), false, false, hooks(&world, parking_body(&world)));
        until("first worker", || world.runs.load(Ordering::SeqCst) == 1);
        // Not the supervisor's: masked, no restart.
        let mut upgrade_off = on();
        upgrade_off.set("upgrade", "false").unwrap();
        assert_eq!(effective(&upgrade_off), on());
        host.set_config(upgrade_off);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), 1);
        // Every key the engine reads survives into the worker's options.
        let mut applied = on();
        for (key, value) in [
            ("trust_roots", "~/x*, /y"),
            ("continue", "false"),
            ("continue_per_hour", "1"),
            ("continue_text", "go"),
            ("rules_file", "/r"),
            ("retry_api_errors", "false"),
            ("model_fallback", ""),
            ("compact_on_context_wall", "false"),
        ] {
            applied.set(key, value).unwrap();
        }
        assert_eq!(effective(&applied), applied);
        let opts = SuperviseOpts::hosted_with(&effective(&applied));
        assert_eq!(opts.policy.trust_roots, ["~/x*", "/y"]);
        assert!(!opts.policy.continue_policy && !opts.policy.retry_api_errors);
        assert_eq!(opts.policy.continue_per_hour, 1);
        assert_eq!(opts.policy.continue_text, "go");
        assert_eq!(
            opts.policy.rules_file.as_deref(),
            Some(std::path::Path::new("/r"))
        );
        assert_eq!(opts.policy.model_fallback, None);
        assert!(!opts.policy.compact_on_context_wall);
        assert!(
            opts.yield_when_held,
            "a hosted loop parks behind another's claim"
        );
        host.set_config(applied);
        until("restarted", || world.runs.load(Ordering::SeqCst) == 2);
        host.shutdown_and_join();
    }

    #[test]
    fn a_panicking_worker_is_restarted_within_its_budget_then_faulted() {
        let world = Arc::new(World::default());
        world.set(&[("s-p", Agent::Claude)]);
        let w = Arc::clone(&world);
        let body: Arc<BodyFn> = Arc::new(move |_job: &WorkerJob| {
            w.runs.fetch_add(1, Ordering::SeqCst);
            panic!("boom in the loop");
        });
        let host = HostHandle::start(on(), false, false, hooks(&world, body));
        until("faulted badge", || !world.badges.lock().unwrap().is_empty());
        until("worker gone", || host.live().is_empty());
        // One first run and RESTART_BUDGET restarts, then off.
        assert_eq!(world.runs.load(Ordering::SeqCst), RESTART_BUDGET + 1);
        let badges = world.badges.lock().unwrap().clone();
        assert_eq!(badges.len(), 1, "{badges:?}");
        let text = badges[0].1.as_deref().unwrap_or_default();
        assert!(
            text.starts_with("supervisor off (faulted) until [harness] changes")
                && text.contains("boom in the loop"),
            "{text}"
        );
        assert!(text.len() <= 200, "{} bytes", text.len());
        // Faulted is sticky: another wake starts nothing under the same policy.
        ring();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), RESTART_BUDGET + 1);
        // The agent leaving clears the badge.
        world.set(&[]);
        until("badge cleared", || world.badges.lock().unwrap().len() == 2);
        assert_eq!(world.badges.lock().unwrap()[1], ("s-p".to_string(), None));
        host.shutdown_and_join();
    }

    /// The reliability review of 2026-09-24 (minor): a failed run was
    /// restarted AT ONCE, so a failure lasting under a second ran the budget
    /// out in that second and faulted the session. Restarts are spaced by
    /// the backoff now, and a stop during one ends the worker at once.
    /// NEGATIVE CONTROL: the backoff grows with the count.
    #[test]
    fn a_failing_worker_waits_its_backoff_and_a_stop_cuts_it_short() {
        assert!(restart_backoff(1) < restart_backoff(3));
        assert_eq!(restart_backoff(9), restart_backoff(5));
        let world = Arc::new(World::default());
        world.set(&[("s-b", Agent::Claude)]);
        let w = Arc::clone(&world);
        let started = Instant::now();
        let body: Arc<BodyFn> = Arc::new(move |_job: &WorkerJob| {
            w.runs.fetch_add(1, Ordering::SeqCst);
            BodyEnd::Failed("transient".to_string())
        });
        let host = HostHandle::start(on(), false, false, hooks(&world, body));
        until("faulted badge", || !world.badges.lock().unwrap().is_empty());
        let spent = started.elapsed();
        let floor: Duration = (1..=RESTART_BUDGET).map(restart_backoff).sum();
        assert!(spent >= floor, "{spent:?} < {floor:?}");
        host.shutdown_and_join();

        // A stop during a backoff ends the worker without waiting it out.
        let world = Arc::new(World::default());
        world.set(&[("s-c", Agent::Claude)]);
        let w = Arc::clone(&world);
        let body: Arc<BodyFn> = Arc::new(move |_job: &WorkerJob| {
            w.runs.fetch_add(1, Ordering::SeqCst);
            BodyEnd::Failed("transient".to_string())
        });
        let host = HostHandle::start(on(), false, false, hooks(&world, body));
        until("first run", || world.runs.load(Ordering::SeqCst) >= 1);
        world.set(&[]);
        until("worker gone", || host.live().is_empty());
        assert!(world.runs.load(Ordering::SeqCst) <= RESTART_BUDGET);
        host.shutdown_and_join();
    }

    /// The reliability review of 2026-09-24 (minor): a panic in the host
    /// thread ended supervision for the process — its workers stopped, the
    /// slot kept the finished handle, and neither a policy change nor a
    /// resume started another. Now the next policy change starts it again.
    /// NEGATIVE CONTROL: before that change nothing is supervised.
    #[test]
    fn a_host_thread_that_panicked_is_started_again_by_the_next_policy_change() {
        let world = Arc::new(World::default());
        world.set(&[("s-h", Agent::Claude)]);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut h = hooks(&world, parking_body(&world));
        let (w, c) = (Arc::clone(&world), Arc::clone(&calls));
        h.roster = Arc::new(move || {
            assert!(
                c.fetch_add(1, Ordering::SeqCst) > 0,
                "the first roster read panics"
            );
            w.roster.lock().unwrap().clone()
        });
        let host = HostHandle::start(on(), false, false, h);
        until("the host thread ended", || {
            host.thread
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
        });
        assert_eq!(world.runs.load(Ordering::SeqCst), 0, "nothing supervised");
        let mut changed = on();
        changed.set("continue_per_hour", "3").unwrap();
        host.set_config(changed);
        until("supervised again", || {
            world.runs.load(Ordering::SeqCst) == 1
        });
        host.shutdown_and_join();
    }

    #[test]
    fn a_session_another_supervisor_holds_is_retried_only_after_a_release() {
        let world = Arc::new(World::default());
        world.set(&[("s-held", Agent::Claude)]);
        let w = Arc::clone(&world);
        let body: Arc<BodyFn> = Arc::new(move |_job: &WorkerJob| {
            w.runs.fetch_add(1, Ordering::SeqCst);
            BodyEnd::Held("someone-else".to_string())
        });
        let host = HostHandle::start(on(), false, false, hooks(&world, body));
        until("one refused run", || world.runs.load(Ordering::SeqCst) == 1);
        // Unrelated wakes do not retry it (no busy loop against a live claim).
        for _ in 0..3 {
            ring();
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), 1);
        world.released.fetch_add(1, Ordering::SeqCst);
        ring();
        until("retried", || world.runs.load(Ordering::SeqCst) == 2);
        host.shutdown_and_join();
    }

    #[test]
    fn a_suspended_host_hands_its_sessions_over_and_resumes() {
        let world = Arc::new(World::default());
        world.set(&[("s-s", Agent::Claude)]);
        // The incoming side of a handoff starts suspended: no thread yet.
        let host = HostHandle::start(on(), false, true, hooks(&world, parking_body(&world)));
        assert!(!host.is_running());
        host.resume();
        until("attached at resume", || host.live() == ["s-s"]);
        host.suspend();
        until("stopped at suspend", || host.live().is_empty());
        host.shutdown_and_join();
    }

    /// A scratch control server: it takes each connection in turn, skips
    /// its `AUTH` line, answers every request `OK` and records it, and ends
    /// at a connection whose first request is `BYE`. Unix-pinned: the
    /// control socket it stands in for is a Unix-domain socket here.
    #[cfg(unix)]
    fn scratch_server(tag: &str) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader};
        let dir = std::env::temp_dir().join(format!("ah-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("a.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let path = sock.to_string_lossy().into_owned();
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            'conns: while let Ok((stream, _)) = listener.accept() {
                let mut w = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { continue 'conns };
                    match line.as_str() {
                        "BYE" => break 'conns,
                        l if l.starts_with("AUTH ") => {}
                        _ => {
                            let _ = w.write_all(b"OK\n");
                            seen.push(line);
                        }
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
            seen
        });
        (path, h)
    }

    /// After the interrupter's cut a stopped worker's transport refuses every
    /// request that could type into the session — the fenced press included
    /// — with nothing written to the wire, as [`RelayCtl`] refuses them.
    /// NEGATIVE CONTROL: the cleanup it redials for (a `meta` read, a
    /// `meta unset`) still lands, so the refusal is the rule, not a dead
    /// socket.
    #[cfg(unix)]
    #[test]
    fn a_stopped_worker_presses_nothing_and_still_clears_its_badges() {
        let (sock, server) = scratch_server("cut");
        let mut ctl = HostCtl::with_token(Endpoint::Socket(sock.clone()), Some("tok".to_string()));
        assert!(ctl.call(&["@s-cut", "meta"]).unwrap().ok());
        let cut = ctl.interrupter().unwrap();
        cut();
        for args in [
            &["@s-cut", "key", "if-gen=0:7", "if=^.*Do you want", "1"][..],
            &["@s-cut", "send", "keep going"],
            &["@s-cut", "paste", "x"],
            &["@s-cut", "turn", "idle=600", "keep going"],
            &["@s-cut", "feed-bin", "0d"],
            &["@s-cut", "meta", "set", "attention", "x"],
        ] {
            assert_eq!(
                ctl.call(args).map(|r| r.ok()),
                Err(STOPPED.to_string()),
                "{args:?}"
            );
        }
        // NEGATIVE CONTROL: the cleanup lands, on a fresh connection.
        let owner = format!("owner={ATTENTION_OWNER}");
        assert!(ctl.call(&["@s-cut", "meta"]).unwrap().ok());
        assert!(
            ctl.call(&["@s-cut", "meta", "unset", "attention", &owner])
                .unwrap()
                .ok()
        );
        // The loop's claim goes back through the cut too, holder-conditionally
        // (aterm-agent's `release_claim`): never another's.
        assert!(
            ctl.call(&[
                "@s-cut",
                "meta",
                "unset",
                "supervisor",
                "holder=aterm-harness@1"
            ])
            .unwrap()
            .ok()
        );
        drop(ctl);
        let mut bye = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        bye.write_all(b"BYE\n").unwrap();
        let seen = server.join().unwrap();
        assert_eq!(
            seen,
            [
                "@s-cut meta",
                "@s-cut meta",
                "@s-cut meta unset attention owner=supervisor",
                "@s-cut meta unset supervisor holder=aterm-harness@1",
            ],
            "nothing but the reads and the cleanup reached the wire — and no claim \
             of the transport's own (the loop holds the only one)"
        );
        assert!(cleanup_request(&["@s", "meta", "unset", "supervisor"]));
        assert!(!cleanup_request(&["@s", "key", "1"]));
    }

    /// `suspend()` stops every live worker BEFORE it returns — its flag set
    /// and its connection cut, after which [`HostCtl`] presses nothing — so
    /// the handoff's Commit write that follows it has no worker able to
    /// type. The host thread is held inside its roster read while suspend
    /// runs, so nothing but suspend itself can have done the cut: a suspend
    /// that only flagged the state and rang the host (the old one) leaves the
    /// worker running here. NEGATIVE CONTROL: before the suspend, nothing is
    /// cut.
    #[test]
    fn suspend_stops_every_worker_before_it_returns() {
        #[derive(Default)]
        struct Gate {
            armed: Mutex<(bool, bool)>,
            cv: Condvar,
            cuts: AtomicUsize,
        }
        let world = Arc::new(World::default());
        world.set(&[("s-g", Agent::Claude)]);
        let gate = Arc::new(Gate::default());
        let mut hooks = hooks(&world, parking_body(&world));
        let (w, g) = (Arc::clone(&world), Arc::clone(&gate));
        hooks.roster = Arc::new(move || {
            let mut armed = g.armed.lock().unwrap();
            if armed.0 {
                armed.1 = true;
                g.cv.notify_all();
                while armed.0 {
                    armed = g.cv.wait(armed).unwrap();
                }
            }
            w.roster.lock().unwrap().clone()
        });
        let g = Arc::clone(&gate);
        // parking_body, with an interrupter that also counts its cuts.
        hooks.body = Arc::new(move |job: &WorkerJob| {
            let g = Arc::clone(&g);
            let me = std::thread::current();
            *job.interrupt.lock().unwrap() = Some(Box::new(move || {
                g.cuts.fetch_add(1, Ordering::SeqCst);
                me.unpark();
            }));
            while !job.stop.load(Ordering::SeqCst) {
                std::thread::park();
            }
            BodyEnd::Stopped
        });
        let host = HostHandle::start(on(), false, false, hooks);
        until("a worker for s-g", || host.live() == ["s-g"]);
        // Hold the host thread inside its next roster read.
        gate.armed.lock().unwrap().0 = true;
        ring();
        {
            let mut armed = gate.armed.lock().unwrap();
            while !armed.1 {
                armed = gate.cv.wait(armed).unwrap();
            }
        }
        assert_eq!(gate.cuts.load(Ordering::SeqCst), 0, "nothing cut yet");
        host.suspend();
        assert_eq!(
            gate.cuts.load(Ordering::SeqCst),
            1,
            "suspend returned with a worker still able to press"
        );
        {
            let mut armed = gate.armed.lock().unwrap();
            armed.0 = false;
            gate.cv.notify_all();
        }
        until("reaped after the suspend", || host.live().is_empty());
        host.shutdown_and_join();
    }

    /// A loop that ended behind another holder's claim (aterm-agent's
    /// `CLAIM_HELD`, what `SuperviseOpts::yield_when_held` ends with) parks
    /// the session; any other end is a failure. The holder parse itself is
    /// aterm-agent's (`claim.rs`, `a_busy_reply_names_the_other_holder`).
    #[test]
    fn a_loop_behind_another_claim_is_held_not_failed() {
        assert_eq!(
            held_or_failed(format!("{CLAIM_HELD}other@host")),
            BodyEnd::Held("other@host".to_string())
        );
        assert_eq!(
            held_or_failed(format!("x: {CLAIM_HELD}other")),
            BodyEnd::Held("other".to_string())
        );
        assert_eq!(
            held_or_failed("session gone".to_string()),
            BodyEnd::Failed("session gone".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // Tier-1: the real host against `HarnessWorkerLifecycle`.
    // -----------------------------------------------------------------------

    /// How the parked run ends when the test kicks it.
    #[derive(Clone, Copy, Debug)]
    enum Plan {
        Fail,
        Held,
    }

    /// One session's world, observed from the seams: which bodies run (and
    /// which of them were asked to stop), the newest worker's failures, how
    /// its last run ended, and the badge.
    #[derive(Default)]
    struct Probe {
        roster: Mutex<Vec<(String, Agent)>>,
        released: AtomicU64,
        running: Mutex<Vec<Arc<AtomicBool>>>,
        max_running: AtomicUsize,
        kick: Mutex<Option<Plan>>,
        kicked: Condvar,
        newest: Mutex<(usize, i64, bool)>,
        /// Every worker's stop flag seen, kept alive so a freed one's address
        /// is never reused as a later worker's identity.
        workers: Mutex<Vec<Arc<AtomicBool>>>,
        badge_on: AtomicBool,
    }

    impl Probe {
        fn body(self: &Arc<Self>) -> Arc<BodyFn> {
            let p = Arc::clone(self);
            Arc::new(move |job: &WorkerJob| {
                let id = Arc::as_ptr(&job.stop) as usize;
                {
                    let mut newest = p.newest.lock().unwrap();
                    if newest.0 != id {
                        *newest = (id, 0, false);
                        p.workers.lock().unwrap().push(Arc::clone(&job.stop));
                    }
                }
                if job.clear_badge {
                    p.badge_on.store(false, Ordering::SeqCst);
                }
                {
                    let mut running = p.running.lock().unwrap();
                    running.push(Arc::clone(&job.stop));
                    p.max_running.fetch_max(running.len(), Ordering::SeqCst);
                }
                let waker = Arc::clone(&p);
                *job.interrupt.lock().unwrap() = Some(Box::new(move || {
                    let _guard = waker.kick.lock().unwrap();
                    waker.kicked.notify_all();
                }));
                let plan = {
                    let mut kick = p.kick.lock().unwrap();
                    loop {
                        if job.stop.load(Ordering::SeqCst) {
                            break None;
                        }
                        if let Some(plan) = kick.take() {
                            break Some(plan);
                        }
                        kick = p.kicked.wait(kick).unwrap();
                    }
                };
                p.running
                    .lock()
                    .unwrap()
                    .retain(|s| !Arc::ptr_eq(s, &job.stop));
                match plan {
                    None => BodyEnd::Stopped,
                    Some(Plan::Fail) => {
                        p.newest.lock().unwrap().1 += 1;
                        BodyEnd::Failed("kicked".to_string())
                    }
                    Some(Plan::Held) => {
                        p.newest.lock().unwrap().2 = true;
                        BodyEnd::Held("someone-else".to_string())
                    }
                }
            })
        }

        fn hooks(self: &Arc<Self>) -> Hooks {
            let (p1, p2, p3, p4) = (
                Arc::clone(self),
                Arc::clone(self),
                Arc::clone(self),
                Arc::clone(self),
            );
            Hooks {
                roster: Arc::new(move || p1.roster.lock().unwrap().clone()),
                still_wanted: Arc::new(move |sid| {
                    p2.roster.lock().unwrap().iter().any(|(s, _)| s == sid)
                }),
                body: self.body(),
                badge: Arc::new(move |_sid, text| {
                    p3.badge_on.store(text.is_some(), Ordering::SeqCst)
                }),
                claim_epoch: Arc::new(move || p4.released.load(Ordering::SeqCst)),
            }
        }

        fn kick(&self, plan: Plan) {
            *self.kick.lock().unwrap() = Some(plan);
            self.kicked.notify_all();
        }

        /// The observed state, projected onto the model's variables; `faults`
        /// is the host's own per-session history.
        fn project(&self, faults: usize) -> std::collections::BTreeMap<&'static str, i64> {
            let wanted = i64::from(!self.roster.lock().unwrap().is_empty());
            let running = self.running.lock().unwrap();
            let old = running.iter().filter(|s| s.load(Ordering::SeqCst)).count() as i64;
            let cur = running.len() as i64 - old;
            drop(running);
            let (_, _, held_end) = *self.newest.lock().unwrap();
            let faults = faults as i64;
            let held = i64::from(held_end && wanted == 1 && cur == 0);
            let faulted = i64::from(self.badge_on.load(Ordering::SeqCst));
            [
                ("wanted", wanted),
                ("cur", cur),
                ("old", old),
                ("faults", faults),
                ("faulted", faulted),
                ("held", held),
            ]
            .into_iter()
            .collect()
        }
    }

    /// Drive the real host through every action of `HarnessWorkerLifecycle`
    /// and check, after each, that what it did is what the model does after
    /// the same actions — every guard enabled on the way, every invariant
    /// holding on every observed state, and never two bodies running at
    /// once. NEGATIVE CONTROLS: the eager (`Buggy=1`) model would start a
    /// worker in the state a reload leaves, which the shipped guard refuses;
    /// and a projection with a worker running over a faulted session is
    /// rejected by an invariant, so the check is not vacuous.
    #[test]
    fn the_real_host_conforms_to_the_worker_lifecycle_model() {
        let model = aterm_spec::derive::harness_worker_lifecycle_model();
        let budget = model.consts.iter().find(|c| c.0 == "Budget").unwrap().1;
        assert_eq!(
            budget, RESTART_BUDGET as i64,
            "the model's budget is the host's"
        );
        let probe = Arc::new(Probe::default());
        let host = HostHandle::start(on(), false, false, probe.hooks());
        let mut expect = model.init_state();
        let mut policy = on();
        let mut step = |what: &str, act: &dyn Fn(), micro: &[&str]| {
            act();
            for action in micro {
                assert!(
                    model.fire(action, &mut expect),
                    "{what}: {action} disabled at {expect:?}"
                );
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let seen = probe.project(host.faults_of("s-t1"));
                for inv in &model.invariants {
                    assert!(
                        model.check_invariant(inv.name, &seen),
                        "{what}: {} broken by {seen:?}",
                        inv.name
                    );
                }
                if seen == expect {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "{what}: observed {seen:?}, model {expect:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let arrive = || {
            *probe.roster.lock().unwrap() = vec![("s-t1".to_string(), Agent::Claude)];
            ring();
        };
        let leave = || {
            probe.roster.lock().unwrap().clear();
            ring();
        };
        step("arrive", &arrive, &["Arrive", "Start"]);
        for n in 0..RESTART_BUDGET {
            step(&format!("fail {n}"), &|| probe.kick(Plan::Fail), &["Fail"]);
        }
        step(
            "fail past the budget",
            &|| probe.kick(Plan::Fail),
            &["Fail"],
        );
        let reload = |policy: &mut SupervisorConfig| {
            let flip = if policy.dismiss_surveys {
                "false"
            } else {
                "true"
            };
            policy.set("dismiss_surveys", flip).unwrap();
            host.set_config(policy.clone());
        };
        step(
            "reload forgives",
            &|| reload(&mut policy.clone()),
            &["Reload", "Start"],
        );
        policy.set("dismiss_surveys", "false").unwrap();
        step("held", &|| probe.kick(Plan::Held), &["Hold"]);
        step(
            "release",
            &|| {
                probe.released.fetch_add(1, Ordering::SeqCst);
                ring();
            },
            &["Release", "Start"],
        );
        step(
            "reload restarts",
            &|| {
                // The state after a restart projects like the state before
                // it: wait for the successor itself, so the next kick cannot
                // land on the worker the reload is stopping.
                let before = probe.workers.lock().unwrap().len();
                reload(&mut policy.clone());
                until("the successor runs", || {
                    probe.workers.lock().unwrap().len() == before + 1
                        && probe.running.lock().unwrap().len() == 1
                });
            },
            &["Reload", "Exit", "Start"],
        );
        // The budget is the SESSION's: failures outlive the worker and the
        // program leaving, so a flap gives no fresh budget.
        step("fail before a flap", &|| probe.kick(Plan::Fail), &["Fail"]);
        step("fail again", &|| probe.kick(Plan::Fail), &["Fail"]);
        step("leave", &leave, &["Leave", "Exit"]);
        step("arrive again", &arrive, &["Arrive", "Start"]);
        for n in 2..RESTART_BUDGET {
            step(
                &format!("fail after the flap {n}"),
                &|| probe.kick(Plan::Fail),
                &["Fail"],
            );
        }
        step(
            "the flap gave no fresh budget",
            &|| probe.kick(Plan::Fail),
            &["Fail"],
        );
        step("leave again", &leave, &["Leave"]);
        policy.set("dismiss_surveys", "true").unwrap();
        step(
            "reload forgives an absent session's history",
            &|| reload(&mut policy.clone()),
            &["Reload"],
        );
        step("arrive after the reload", &arrive, &["Arrive", "Start"]);
        step("leave last", &leave, &["Leave", "Exit"]);
        assert_eq!(
            probe.max_running.load(Ordering::SeqCst),
            1,
            "two supervisors ran at once"
        );
        host.shutdown_and_join();

        // NEGATIVE CONTROLS.
        let after_reload: std::collections::BTreeMap<&'static str, i64> = [
            ("wanted", 1),
            ("cur", 0),
            ("old", 1),
            ("faults", 0),
            ("faulted", 0),
            ("held", 0),
        ]
        .into_iter()
        .collect();
        assert!(!model.action_enabled("Start", &after_reload));
        assert!(aterm_spec::interp::with_buggy(&model, 1).action_enabled("Start", &after_reload));
        let mut corrupt = after_reload.clone();
        corrupt.insert("cur", 1);
        corrupt.insert("old", 0);
        corrupt.insert("faulted", 1);
        assert!(!model.check_invariant("FaultedIsOff", &corrupt));
    }
}
