// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-GUI SUPERVISOR host: every agent session this instance owns — Claude
//! Code's and Codex's — is supervised by default, under the owner's `[harness]` policy
//! ([`aterm_agent::supervise::SupervisorConfig`]), with nothing typed to start
//! it. Sibling of [`crate::operator_host`], whose threading and shutdown shape
//! it copies: nothing here runs on the winit thread, and shutdown is an
//! explicit bounded join from `main_entry` after the event loop exits.
//!
//! **Discovery, without a poll.** One host thread (`aterm-harness-host`)
//! parks on [`ring`]: the store rings it when its membership moves
//! (`SessionStore::record_roster`), a session's timeline when its published
//! `program=` moves (`SessionTimeline::set_program`/`note_foreground_group`)
//! or its agent's phase does (`publish_agent`: a phase-only wake follows the
//! changed tab's conversation for a relaunch, [`follow_snapshot`]),
//! a supervisor claim when it is released or lapses, a worker when it ends,
//! and the host's own handle when the config changes or shutdown begins.
//! Each wake re-reads the store in-process: a session whose server-published
//! program is an agent it supervises gets a worker; one whose program left,
//! or that closed, loses it. Which agents get one is ONE predicate,
//! aterm-phase's ([`aterm_phase::Program::supervisable`]: an agent whose
//! reader is measured — Claude Code and Codex), applied where the published
//! program is read ([`agent_of`]). The relaunch on exit below is Claude
//! Code's alone; the upgrade is Claude Code's and — by its Codex branch, the
//! same step at the same idle points — Codex's.
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
//! ([`restart_backoff`], on the bell, cut short by a stop) — FOR EVER:
//! within [`RESTART_BUDGET`] failures an hour per SESSION (the count
//! outlives a worker, a program flap and the session leaving; a changed
//! policy forgives it) on pauses of a second to a minute, and past it on
//! pauses that grow to an hour, the session's keyed attention
//! (`owner=supervisor`) saying so while it waits — information, cleared as
//! the next run starts. Only a configured limit switches supervision off
//! (the philosophy review of 2026-09-25: the budget used to turn it OFF
//! until someone edited `[harness]`, a give-up that waited on a person).
//! The panic itself is filed by `logging.rs` as a harness fault record, not
//! as a crash of aterm (the process keeps running).
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
//!
//! **The worker is the one place a session is acted on** (owner, 2026-09-24:
//! *"UNLESS aterm is configured otherwise, it is in FULLY AUTOMATIC mode"*).
//! Besides the loop, a worker does the two things a loop cannot, each behind
//! its `[harness]` key and each only where the loop is not acting:
//!
//! * THE LIVE UPGRADE (`[harness] upgrade`). When the worker attaches, and
//!   whenever a newer Claude Code or Codex is installed — atpkg's activation
//!   notice, or Claude's own native updater repointing `~/.local/bin/claude`
//!   (a push: one kqueue on the directories holding both,
//!   [`aterm_agent::harness::upgrade_wake`], rings the host) — the worker asks
//!   whether its session has somewhere to go
//!   ([`aterm_agent::harness::upgrade_drive::due`]) and asks its loop for
//!   the session's next idle point ([`WorkerIdle`], the loop's
//!   [`IdleHost`]). There, IN the loop, it takes ONE step of the
//!   cooperative upgrade ([`aterm_agent::harness::upgrade_drive::step`]:
//!   announce and ask for READY; once READY, end the agent and relaunch it
//!   on its conversation), and the loop runs on — over the relaunched agent,
//!   with its claim, its badges and its policy's memory. A CODEX session's
//!   step is the same step's Codex branch
//!   ([`aterm_agent::harness::upgrade_codex`]): the shared app-server daemon
//!   first, by the vendor's own verb, once nothing runs in it and every
//!   Codex tab it serves has been asked; then the tab's TUI, ended by a typed
//!   `/exit` — never a signal — and relaunched by `codex resume` of the same
//!   conversation through the one relaunch line; an embedded session gets
//!   the notice and READY first, and its carry-on is typed where the loop
//!   parks next, as a Claude Code's is. A session busy only with its OWN
//!   BACKGROUND WORK (a workflow it waits on, a shell it left, a Codex
//!   background terminal) is at a natural break: its loop offers that break
//!   ([`IdleHost::at_background`]) and the worker types only a NOTICE there,
//!   and ends nothing (owner, 2026-09-26: "The notice interrupts the
//!   agent's orchestration once, and the restart still never kills running
//!   work"). Only a whole `REASK_S` of that work running on earns a re-ask
//!   there, naming what runs. After `MAX_ASKS` notices the upgrade gives up.
//!   A tab whose poll loops could never end was otherwise told once and then
//!   waited on for days (2026-09-26). While the agent
//!   winds down (READY given, the restart imminent) the upgrade OWNS the
//!   session's turn ends ([`IdleHost::owns_turn_end`], read from the step's
//!   own result, never from the screen) and nothing is typed; an agent that
//!   answered without READY, an upgrade that gave up, failed or was
//!   switched off, a release still owed, and a drain of background work
//!   past [`upgrade_drive::OWNED_BACKGROUND_LOOKS`] looks own nothing
//!   ([`upgrade_drive::owns_turn_ends`]), and the worker is continued as at
//!   any turn end — the loop decides the point it left to
//!   the upgrade again the moment the upgrade owns nothing. A step that must
//!   wait is looked at again on a growing pause (the host thread's timed
//!   bell), never on a sweep; a turn the session runs in between starts the
//!   pauses over ([`IdleHost::turn_ran`]). A look whose READ FAILED — the
//!   socket refused it, Claude Code's records or the agent's process could
//!   not be read whole, a launch's record is not written yet, a build the
//!   agent could move to did not answer its `--version` in time
//!   ([`aterm_agent::harness::upgrade_drive::Due::Unread`]) — decides
//!   nothing: it is journaled (`wait:no-socket`, `wait:no-process`,
//!   `wait:session-files-unreadable`, `wait:no-record`, `wait:no-version`)
//!   and looked at again on a short pause of its own ([`UNREAD_LOOK`]), what
//!   the upgrade owned left as it stood. Only a look that reads whole and
//!   finds nothing the upgrade could act on lets it go: no build or model to
//!   move to, a Codex it does not move, an agent past its launch with no
//!   record of its own ([`aterm_agent::harness::upgrade_drive::RECORD_WINDOW_S`]).
//! * RELAUNCH ON EXIT (`[harness] relaunch`). While the agent runs, the
//!   worker records what a relaunch needs
//!   ([`aterm_agent::harness::relaunch::snapshot`]); when the session's
//!   program leaves the agent and the tab lives on, the host hands the
//!   worker the exit instead of just stopping it. An exit a person owns (a
//!   keystroke within `[harness] human_grace_s`, `status` `human_ms=`: their
//!   `/exit` is theirs) or a holder owns (`hold=1`, a lease, a named
//!   driver's turn) is left to them, and one that was the launch's own end
//!   (a `-p` run, a subcommand, a launch that never registered a
//!   conversation) is only journaled, as is a graceful one (Claude Code
//!   removed its own session record: someone's `/exit`, a `kill`). Whether
//!   the record survived is read AS THE EXIT IS SEEN — within a quarter
//!   second, before the back-off ([`aterm_agent::harness::relaunch::exit_record`])
//!   — and handed to every attempt: any Claude Code that starts removes dead
//!   agents' records, so a crash's record read after the back-off could say
//!   "graceful" (D2 of the 2026-09-26 live test). Any other is relaunched on its
//!   conversation ([`aterm_agent::harness::relaunch::after_exit`]) on a
//!   growing back-off kept per session ([`Relaunches`]); the session's keyed
//!   attention says a relaunch the owner limited, one that cannot be made
//!   and one that keeps failing — never a silent give-up.
//! * THE RESTART IN PLACE (`[harness] relaunch`). Where the loop meets a
//!   point nothing typed can answer — Claude Code's critical-memory banner —
//!   it asks its worker to restart the agent there ([`IdleHost::restart`]):
//!   ended at that idle point and relaunched on its conversation
//!   ([`aterm_agent::harness::relaunch::restart_here`]), the loop running on
//!   over the new process and carrying it on at its next idle point.
//!
//! **The owner sees the upgrade** (gap audit 2026-09-24): the host thread
//! keeps the window's view of its tabs' upgrades
//! ([`aterm_agent::harness::upgrade_drive::View`], [`Hooks::upgrade_view`]) —
//! each tab's `upgrade=`, the waiting record, a row and the tab's
//! `owner=upgrade` attention for a STALLED one — looked at again after a
//! worker acts ([`note_upgrade_act`]), at an activation notice or the
//! owner's word (`aterm harness upgrade <sid> --now|--defer|--skip`, whose
//! marker the activation wake watches), when the roster changes, and at the
//! instant the view itself names (an upgrade turning overdue, a word running
//! out): never on a timer of its own.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_agent::harness::relaunch::{
    self, ExitLook, ExitRecord, Foreground, OnExit, Outcome, Relaunches, Restart, Say, Snapshot,
};
use aterm_agent::harness::upgrade_drive::{self, After, Behind, Due};
use aterm_agent::harness::upgrade_wake::{ActivationWake, WakeTrigger};
use aterm_agent::supervise::{
    Ctl, CtlReply, Endpoint, HostStep, IdleHost, Interrupter, RelayCtl, Session, SuperviseOpts,
    SupervisorConfig,
};

use aterm_phase::Program;

use crate::session_store::{SessionState, Store};

/// `cfg` with every key the engine does not read at its default: what the
/// host runs under and compares, so an edit that changes nothing the engine
/// reads restarts nothing. Those are the two `[harness]` keys the HOST reads
/// and the loop does not: `upgrade` and `relaunch` — read from the same
/// parse before they are masked ([`Switches`], live in every worker), so the
/// table has one parser.
pub(crate) fn effective(cfg: &SupervisorConfig) -> SupervisorConfig {
    let d = SupervisorConfig::default();
    SupervisorConfig {
        upgrade: d.upgrade,
        relaunch: d.relaunch,
        ..cfg.clone()
    }
}

/// The host's own `[harness]` switches — the master switch and each act's —
/// as the window last parsed them, read live by every worker: switching one
/// off lands at the worker's next act, with no restart.
#[derive(Default)]
pub(crate) struct Switches {
    upgrade: AtomicBool,
    relaunch: AtomicBool,
}

impl Switches {
    fn set(&self, cfg: &SupervisorConfig) {
        self.upgrade
            .store(cfg.enabled && cfg.upgrade, Ordering::SeqCst);
        self.relaunch
            .store(cfg.enabled && cfg.relaunch, Ordering::SeqCst);
    }

    fn upgrade(&self) -> bool {
        self.upgrade.load(Ordering::SeqCst)
    }

    fn relaunch(&self) -> bool {
        self.relaunch.load(Ordering::SeqCst)
    }
}

/// Restarts per session per [`RESTART_WINDOW`] on the short pauses; past
/// it the pauses grow to an hour and the session is badged while it waits
/// ([`restart_backoff`]) — never turned off. Counted per SESSION, across
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

/// A restored tab's relaunch ([`HostHandle::relaunch_restored`]) is tried at
/// most this many times — a shell that is still starting says `NotYet` —
/// pausing [`RESTORED_FIRST_PAUSE`] first, doubling up to
/// [`RESTORED_MAX_PAUSE`]: about two minutes in all.
const RESTORED_TRIES: u32 = 8;
const RESTORED_FIRST_PAUSE: Duration = Duration::from_secs(1);
const RESTORED_MAX_PAUSE: Duration = Duration::from_secs(30);
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

/// Generic wakes retain the old full snapshot follow. A phase-only wake can
/// use the per-session publication stamp already read with the roster. Both
/// counters are broadcast cursors, never drained by one of several hosts.
static FULL_FOLLOW_EPOCH: AtomicU64 = AtomicU64::new(0);
static PHASE_FOLLOW_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Bumped each time a supervisor claim is released or lapses: a session held
/// by another supervisor is retried only once this has moved.
static CLAIM_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Bumped each time a worker took an upgrade's or a relaunch's step (its
/// state files may have moved): the owner's view is looked at again
/// ([`look_at_upgrades`]).
static UPGRADE_ACTS: AtomicU64 = AtomicU64::new(0);

/// A worker took an upgrade's or a relaunch's step: the owner's view is owed
/// a look ([`UPGRADE_ACTS`]), and the host is woken to take it.
fn note_upgrade_act() {
    UPGRADE_ACTS.fetch_add(1, Ordering::SeqCst);
    ring();
}

/// Wake the host: something it decides from moved (the roster, a program, a
/// claim, a worker, the config). Cheap, never blocks on anything but the
/// bell's own leaf lock, so it is safe under the store's or a timeline's lock.
pub(crate) fn ring() {
    FULL_FOLLOW_EPOCH.fetch_add(1, Ordering::SeqCst);
    ring_bell();
}

/// A changed agent phase wakes the host, but only the tab whose publication
/// stamp moved needs its Claude session file followed. Two phase wakes that
/// coalesce are treated as a full follow: the same tab could have moved away
/// and back between host reads.
pub(crate) fn ring_phase() {
    PHASE_FOLLOW_EPOCH.fetch_add(1, Ordering::SeqCst);
    ring_bell();
}

fn ring_bell() {
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
    wait_for(deadline, || done.load(Ordering::SeqCst))
}

/// [`wait_done`] for any condition over flags whose setters ring the bell.
fn wait_for(deadline: Instant, done: impl Fn() -> bool) -> bool {
    let mut rung = BELL.rung.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if done() {
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

/// Park until the bell has moved past `seen`, or `until` passes (the next
/// look a worker's upgrade asked for); returns where the bell stands.
fn bell_wait(seen: u64, until: Option<Instant>) -> (u64, bool) {
    let mut rung = BELL.rung.lock().unwrap_or_else(PoisonError::into_inner);
    let mut timed_out = false;
    while *rung == seen {
        match until {
            None => rung = BELL.cv.wait(rung).unwrap_or_else(PoisonError::into_inner),
            Some(at) => {
                let left = at.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    timed_out = true;
                    break;
                }
                rung = BELL
                    .cv
                    .wait_timeout(rung, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }
    (*rung, timed_out)
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
    /// The agent the session's program named when the worker started: the
    /// relaunch on exit is written for Claude Code alone, the upgrade for
    /// Claude Code and (its Codex branch) Codex.
    pub(crate) agent: Program,
    pub(crate) opts: SuperviseOpts,
    pub(crate) stop: Arc<AtomicBool>,
    /// The live connection's cut, published by the body so a stop ends a
    /// parked wait at once.
    pub(crate) interrupt: Arc<Mutex<Option<Interrupter>>>,
    /// Set before `stop` when a new worker takes the session straight after
    /// (a changed policy): the loop leaves its badges for that one to adopt.
    pub(crate) handover: Arc<AtomicBool>,
    /// Clear the badge a previous worker's relaunch left before running.
    pub(crate) clear_badge: bool,
    /// The session's failed runs ([`FaultHistory`]), shared with the host.
    pub(crate) faults: FaultHistory,
    /// The request for the loop's next idle point ([`WorkerIdle`], the
    /// loop's `opts.idle_host`): set by the worker for an upgrade it has to
    /// take, and by the host at an activation notice or at a look the worker
    /// asked for ([`Self::look_at`]).
    pub(crate) park: Arc<AtomicBool>,
    /// When the worker wants its session looked at again: an upgrade step
    /// that waited. The host sets [`Self::park`] then.
    pub(crate) look_at: Arc<Mutex<Option<Instant>>>,
    /// Set by the host, before it stops the worker, when the session's agent
    /// LEFT a tab that lives on (not when the tab closed or the policy
    /// stopped it); cleared if the agent is back. The worker then handles
    /// the exit ([`on_agent_left`]).
    pub(crate) left: Arc<AtomicBool>,
    /// Set while the worker takes an upgrade step: the agent it ends and
    /// relaunches is ITS act, so the host neither stops it nor hands it the
    /// exit.
    pub(crate) acting: Arc<AtomicBool>,
    /// The worker's loop holds for a stall the server published
    /// ([`IdleHost::stalled`]): an exit while it is set was the stall's
    /// remedy's (`signal term|kill`), no person's or holder's — relaunched
    /// on its conversation (U1).
    pub(crate) stalled: Arc<AtomicBool>,
    /// The host's own `[harness]` switches, live.
    pub(crate) switches: Arc<Switches>,
    /// What the host keeps of the session for a relaunch ([`Kept`]),
    /// across its workers.
    pub(crate) kept: Arc<Mutex<Kept>>,
    /// The session's note behind still to be taken ([`Owed`]).
    note: Note,
}

/// What the host keeps of one open session for a relaunch, across its
/// workers: a worker that attaches to an agent already gone (its program
/// read a moment longer than it ran) must still find what the last one read
/// while the agent ran.
#[derive(Default)]
pub(crate) struct Kept {
    /// The session's relaunch state.
    relaunches: Relaunches,
    /// What a relaunch of its agent needs, read while it ran and FOLLOWED
    /// while it runs ([`follow_snapshot`]): replaced by every read that
    /// succeeds, kept through one that fails only while it still describes
    /// the tab (its agent, or the shell its exit left), and forgotten once
    /// that agent's exit is handled — never carried onto the next agent.
    snapshot: Option<Snapshot>,
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
}

/// A tab's publication as read under its timeline lock with the roster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FollowStamp {
    group: i32,
    reader: Option<Program>,
    rev: u64,
}

type RosterFn = dyn Fn() -> Vec<(String, Program, FollowStamp)> + Send + Sync;
type WantedFn = dyn Fn(&str) -> bool + Send + Sync;
/// What the upgrade reads of a session ([`upgrade_drive::due`]).
type DueFn = dyn Fn(&str) -> Due + Send + Sync;
type BodyFn = dyn Fn(&WorkerJob) -> BodyEnd + Send + Sync;
type BadgeFn = dyn Fn(&str, Option<&str>) + Send + Sync;
type EpochFn = dyn Fn() -> u64 + Send + Sync;
type StepFn = dyn Fn(&str, u32) -> String + Send + Sync;
/// The session noted behind since a unix second
/// ([`upgrade_drive::note_behind`]).
type BehindFn = dyn Fn(&str, u64) -> Behind + Send + Sync;
/// The upgrade's clocks held until a unix second ([`Acts::hold`]): `false`
/// while another sweep holds the lock, to be tried again.
type HoldFn = dyn Fn(&str, u64) -> bool + Send + Sync;
type SnapshotFn = dyn Fn(&str) -> Option<Snapshot> + Send + Sync;
type FollowFn = dyn Fn(&str, &mut Snapshot) -> Foreground + Send + Sync;
type StatusFn = dyn Fn(&str) -> Option<String> + Send + Sync;
type ExitLookFn = dyn Fn(&str, &Snapshot) -> Option<ExitLook> + Send + Sync;
type RelaunchFn = dyn Fn(&str, u32, &Snapshot, &ExitRecord, bool, bool) -> String + Send + Sync;
/// The relaunch of an agent whose tab a crash of aterm took, in the tab the
/// next launch reopened ([`relaunch::after_host_ended`]): `(sid, snapshot,
/// upgrade)` to the step's word.
type RestoredFn = dyn Fn(&str, &Snapshot, bool) -> String + Send + Sync;
/// The owner's view of the upgrades ([`Hooks::upgrade_view`]): `true` looks
/// again and answers the unix second it next changes by time alone, `false`
/// stands it down.
type ViewFn = dyn Fn(bool) -> Option<u64> + Send + Sync;
type RestartFn = dyn Fn(&str, u32, &Restart) -> String + Send + Sync;
/// Whether the conversation a tab's agent holds has a task
/// ([`upgrade_drive::tasked`], the tab's supervisor's own turns none), read
/// on from where the worker's last read of it ended: `(sid, session, memo)`.
type TaskedFn = dyn Fn(&str, &str, &mut upgrade_drive::Tasked) -> Option<bool> + Send + Sync;
type BackoffFn = dyn Fn(usize) -> Duration + Send + Sync;
type PauseFn = dyn Fn(Duration) -> Duration + Send + Sync;
/// One look of the disk watch over the supervised sessions ([`Hooks::disk`]).
type DiskFn = dyn Fn(&[String]) + Send + Sync;

/// The host's seams onto the process: which sessions run an agent, whether
/// one still does, one run of the loop, the faulted badge, how many
/// supervisor claims have been released (a held session's retry gate), and
/// the worker's acts beyond the loop ([`Acts`]), how long a failed run waits
/// before its restart, and how long a pause the upgrade's or the relaunch's
/// own table names is waited. The production set reads the store, talks to
/// the control socket and waits [`restart_backoff`] and each pause as named
/// ([`start_default`]); the tests inject their own, so a test chooses the
/// waits it stops instead of racing a schedule compiled into the build.
#[derive(Clone)]
pub(crate) struct Hooks {
    pub(crate) roster: Arc<RosterFn>,
    pub(crate) still_wanted: Arc<WantedFn>,
    pub(crate) body: Arc<BodyFn>,
    pub(crate) badge: Arc<BadgeFn>,
    pub(crate) claim_epoch: Arc<EpochFn>,
    pub(crate) acts: Acts,
    pub(crate) backoff: Arc<BackoffFn>,
    pub(crate) pause: Arc<PauseFn>,
    /// THE OWNER'S VIEW of this instance's upgrades
    /// ([`upgrade_drive::View`]): looked at by the host thread when
    /// [`look_at_upgrades`] says, its rows handed to the app, headless or
    /// not (`Wake::AgentUpgrade`), and its stalled tabs marked.
    pub(crate) upgrade_view: Arc<ViewFn>,
    /// THE DISK WATCH (design §5.5's one timer, [`look_at_disk`]): handed
    /// the supervised sessions every [`DISK_TICK`]; below the automatic
    /// floor it reclaims the stale build directories where they work
    /// ([`live_disk`], `aterm_agent::harness::cli::disk_tick`).
    pub(crate) disk: Arc<DiskFn>,
}

/// The worker's acts beyond the loop, each over one session (`sid`): the
/// upgrade's ([`upgrade_drive::due`], one [`upgrade_drive::step`] under
/// `human_grace_s`), the continuation a relaunched agent is owed
/// ([`relaunch::owed`], typed by [`relaunch::resume`] where its loop parks
/// at idle), and the relaunch's (the [`relaunch::snapshot`] taken
/// while the agent runs, and FOLLOWED against the tab's foreground job
/// ([`relaunch::foreground`], [`relaunch::follow`]), the session's `status`
/// line — its `human_ms=`,
/// `hold=` and `hand=` say whose an exit was — one look at what the exit left
/// of Claude's own record ([`relaunch::look_at_exit`]), and one
/// [`relaunch::after_exit`] step) — and whether a session is still OPEN,
/// which tells an agent that left its tab from a tab that closed.
#[derive(Clone)]
pub(crate) struct Acts {
    pub(crate) open: Arc<WantedFn>,
    pub(crate) due: Arc<DueFn>,
    /// The session is behind — from its worker's attach, and from each
    /// activation notice: its upgrade state minted with its age from the
    /// second given, as the first step would mint it
    /// ([`upgrade_drive::note_behind`]; asked until it decides, [`Owed`]).
    pub(crate) behind: Arc<BehindFn>,
    pub(crate) step: Arc<StepFn>,
    /// The upgrade's step at a break of the agent's own background work: a
    /// notice alone (the first, or a re-ask once `REASK_S` has passed), or a
    /// give-up past `MAX_ASKS`. Nothing is ended
    /// ([`upgrade_drive::Opts::background`]).
    pub(crate) notice: Arc<StepFn>,
    pub(crate) owed: Arc<WantedFn>,
    pub(crate) resume: Arc<StepFn>,
    pub(crate) snapshot: Arc<SnapshotFn>,
    pub(crate) follow: Arc<FollowFn>,
    pub(crate) status: Arc<StatusFn>,
    /// One look at an agent that left: whether its process still runs, and
    /// Claude's own record of it now (`None`: nothing can be read). Taken
    /// as the exit is seen, before the back-off ([`on_agent_left`]).
    pub(crate) exit_look: Arc<ExitLookFn>,
    pub(crate) relaunch: Arc<RelaunchFn>,
    /// The relaunch of a restored tab's agent after aterm itself ended
    /// ([`HostHandle::relaunch_restored`]).
    pub(crate) relaunch_restored: Arc<RestoredFn>,
    pub(crate) restart: Arc<RestartFn>,
    /// Whether the conversation the session's agent holds has a task — a
    /// prompt of a person's or an orchestrator's; the harness's own turns
    /// are none ([`WorkerIdle::taskless`]).
    pub(crate) tasked: Arc<TaskedFn>,
    /// The live upgrade's clocks held through the loop's limit episode
    /// ([`upgrade_drive::hold_clock`], [`WorkerIdle::limited`]).
    pub(crate) hold: Arc<HoldFn>,
}

/// Stop one worker: its flag, then its connection's cut. A stop is never an
/// agent's exit: `left` is cleared first, so a worker waiting out a
/// relaunch's pause ends it.
fn request_stop(stop: &AtomicBool, left: &AtomicBool, interrupt: &Mutex<Option<Interrupter>>) {
    left.store(false, Ordering::SeqCst);
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

/// The badge a session whose supervisor keeps failing carries while it
/// waits to restart it: information, never an "off" — the restart is
/// coming, and the badge is cleared as it starts.
fn failing_badge(why: &str, restarts: usize, pause: Duration) -> String {
    let why: String = why.split_whitespace().collect::<Vec<_>>().join(" ");
    // The whole badge within the server's keyed-attention cap (200 bytes).
    let mut end = why.len().min(80);
    while !why.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "supervisor keeps failing ({restarts} restarts this hour); restarting in {} min; last: {}",
        pause.as_secs().div_ceil(60),
        &why[..end]
    )
}

/// How long a worker waits before its `n`-th restart (1-based): the
/// failures within [`RESTART_BUDGET`] an hour on pauses of a second to a
/// minute, so a transient one of under a second no longer spends the whole
/// budget in that second (the reliability review of 2026-09-24); past the
/// budget on pauses that grow to an hour — never a give-up. The wait is on
/// the host's bell, cut short by a stop — nothing polls. The worker reads it
/// through [`Hooks::backoff`], so a test chooses the length of the wait it
/// stops instead of racing a schedule compiled into the build.
fn restart_backoff(n: usize) -> Duration {
    const STEPS: [Duration; 9] = [
        Duration::from_secs(1),
        Duration::from_secs(5),
        Duration::from_secs(15),
        Duration::from_secs(30),
        Duration::from_secs(60),
        Duration::from_secs(5 * 60),
        Duration::from_secs(15 * 60),
        Duration::from_secs(30 * 60),
        Duration::from_secs(60 * 60),
    ];
    aterm_agent::supervise::ladder::Ladder(&STEPS).step(n.saturating_sub(1))
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

/// One worker thread's life: attaches ([`attach`]), runs its loop — which
/// takes the upgrade's step in place at the idle points it is asked for
/// ([`WorkerIdle`]) — restarts it after a failure on a growing pause, for
/// ever ([`restart_backoff`]; badged while it waits once past
/// [`RESTART_BUDGET`] an hour), and ends stopped — handling its agent's exit
/// first when the host says the agent LEFT ([`on_agent_left`]) — or held.
fn worker_main(mut job: WorkerJob, hooks: &Hooks) -> WorkerExit {
    attach(&job, hooks);
    // Consecutive failures past the budget: each pauses longer.
    let mut over = 0usize;
    // This worker's own failing badge is up.
    let mut badged = false;
    let unbadge = |badged: &mut bool| {
        if std::mem::take(badged) {
            (hooks.badge)(&job.sid, None);
        }
    };
    loop {
        let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (hooks.body)(&job)))
            .unwrap_or_else(|payload| BodyEnd::Failed(panic_text(payload.as_ref())));
        job.clear_badge = false;
        *job.interrupt.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let why = match end {
            BodyEnd::Stopped if job.stop.load(Ordering::SeqCst) => {
                unbadge(&mut badged);
                return stopped(&job, hooks);
            }
            BodyEnd::Held(other) => {
                unbadge(&mut badged);
                aterm_log::info!("harness @{}: not supervised: {CLAIM_HELD}{other}", job.sid);
                return WorkerExit::Held;
            }
            BodyEnd::Stopped => "the loop ended unasked".to_string(),
            BodyEnd::Failed(why) => why,
        };
        if job.stop.load(Ordering::SeqCst) || !(hooks.still_wanted)(&job.sid) {
            unbadge(&mut badged);
            return stopped(&job, hooks);
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
        over = if failed > RESTART_BUDGET { over + 1 } else { 0 };
        let pause = (hooks.backoff)(failed.min(RESTART_BUDGET) + over);
        if over > 0 {
            aterm_log::warn!(
                "harness @{}: the supervisor keeps failing ({RESTART_BUDGET}+ restarts this hour); \
                 restarting in {} s: {why}",
                job.sid,
                pause.as_secs(),
            );
            (hooks.badge)(&job.sid, Some(&failing_badge(&why, failed, pause)));
            badged = true;
        } else {
            aterm_log::warn!(
                "harness @{}: restarting the supervisor ({failed} of {RESTART_BUDGET} this hour) \
                 in {} ms: {why}",
                job.sid,
                pause.as_millis(),
            );
        }
        if wait_done(&job.stop, Instant::now() + pause) {
            unbadge(&mut badged);
            return stopped(&job, hooks);
        }
        // The restart: its badge goes as it starts; a failure raises it again.
        unbadge(&mut badged);
    }
}

/// A stopped worker's end: its agent's exit handled first when that is why
/// it was stopped.
fn stopped(job: &WorkerJob, hooks: &Hooks) -> WorkerExit {
    if job.left.load(Ordering::SeqCst) {
        on_agent_left(job, hooks);
    }
    WorkerExit::Stopped
}

/// The worker's first act, before its loop: record what a relaunch of the
/// agent needs, and park the loop at the session's first idle point when the
/// agent is owed its continuation (it was just relaunched: the loop answers
/// whatever it opened with first, [`relaunch::owed`]) or has an upgrade to
/// take — or when the upgrade could not read the session yet
/// ([`Due::Unread`]: the first idle point looks again), or when Claude has
/// not yet registered the conversation the record must name (it has by the
/// first idle point, where [`WorkerIdle::at_idle`] reads it again).
fn attach(job: &WorkerJob, hooks: &Hooks) {
    if !upgrades(job.agent) {
        return;
    }
    let due = if job.switches.upgrade() {
        (hooks.acts.due)(&job.sid)
    } else {
        Due::No
    };
    // A launch's record not written yet is every fresh launch's shape, not news.
    if let Due::Unread(why) = due
        && why != upgrade_drive::UNREAD_RECORD
    {
        aterm_log::info!(
            "harness @{}: the upgrade could not read the session as it attached ({why}); \
             the first idle point looks again",
            job.sid
        );
    }
    let looks = due != Due::No;
    // Behind from the attach (N3): the owner sees the row, and its age,
    // before the session's first idle point — minutes later after a busy
    // first turn. Owed while an upgrade is due or could not be read yet (a
    // launch's record is written a moment after it starts), asked now, and
    // asked again by the host until it decides ([`Owed`]).
    if looks {
        owe_note(&job.note, crate::upgrade_host::now_s());
        if matches!(
            ask_note(&job.sid, &job.note, hooks),
            Some(Behind::Busy | Behind::Unread(_))
        ) {
            ring();
        }
    }
    if job.agent == Program::Codex {
        // The Codex branch of the upgrade (and its relaunch's owed
        // carry-on); a Codex's relaunch on exit is not built, so nothing of
        // one is recorded.
        if (hooks.acts.owed)(&job.sid) || looks {
            job.park.store(true, Ordering::SeqCst);
        }
        return;
    }
    refresh_snapshot(&job.sid, &job.kept, hooks);
    let incomplete = with_kept(job, |k| {
        k.snapshot.as_ref().is_none_or(|s| s.session.is_none())
    });
    if incomplete || (hooks.acts.owed)(&job.sid) || looks {
        job.park.store(true, Ordering::SeqCst);
    }
}

/// Whether the live upgrade is written for `agent`: Claude Code's restart,
/// and the Codex branch of the same step (the daemon first, then a typed
/// `/exit` and `codex resume`, never a signal).
fn upgrades(agent: Program) -> bool {
    matches!(agent, Program::Claude | Program::Codex)
}

/// How soon a break of the agent's own background work is looked at again
/// for the notice after one that typed nothing ([`WorkerIdle::at_background`]):
/// the loop offers such a break at every read of it, and each look reads the
/// session's builds, its tab and its screen.
const BACKGROUND_LOOK: Duration = Duration::from_secs(60);

/// A SESSION'S NOTE BEHIND STILL TO BE TAKEN (N3 of the live re-tests of
/// 2026-09-26): owed from the worker's attach — an upgrade due, or one it
/// could not read yet — and from every activation notice, and asked
/// ([`ask_note`], [`Acts::behind`]) until it decides, noted or nothing to
/// note, with the second the host first saw the session behind: the state it
/// mints is behind from THEN. At a fresh launch Claude Code writes its record
/// a moment after the worker attaches (1.2 s in the re-test of 155c72a28,
/// where the note was asked once, at the attach, and never again before the
/// first idle point: the column read `-` through the whole first turn, and
/// "behind" counted from the first step). So a note that could not read
/// ([`Behind::Unread`]) is asked again: at the host's wakes — the agent's
/// first verdicts ring it as its REPL comes up — no sooner than [`NOTE_GAP`]
/// after the last ask, and on the [`NOTE_AGAIN`] ladder at the latest; past
/// the ladder, only at the loop's idle points, before their step — which
/// then finds the state minted with its age from the attach.
#[derive(Clone, Copy, Debug)]
struct Owed {
    /// Behind since this unix second: the attach's, or the notice's.
    since: u64,
    /// The asks in a row that could not read ([`NOTE_AGAIN`]'s rung).
    unread: u32,
    /// The last ask.
    asked: Option<Instant>,
    /// The host asks again by this instant; `None` once [`NOTE_AGAIN`] is
    /// spent.
    by: Option<Instant>,
}

/// A worker's owed note ([`Owed`]): asked by the worker (at its attach, at
/// its idle points) and by the host thread (at its wakes). A leaf lock,
/// never held over an ask.
type Note = Arc<Mutex<Option<Owed>>>;

/// The pauses before the host asks again a note that could not read
/// ([`Owed`]): a launch's record comes within seconds; a control socket
/// whose lanes are all held may take a minute and more (the re-test of
/// 155c72a28: 90 s), and the idle points go on asking after the last.
const NOTE_AGAIN: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

/// How soon after an ask that could not read a host wake asks again
/// ([`Owed`]): a wake is the event — the agent's verdict moving as its REPL
/// comes up — and this keeps a burst of them from asking at each.
const NOTE_GAP: Duration = Duration::from_secs(1);

/// Owe the session's note behind since `since` — the earlier second kept
/// when one is owed already — to be asked at once.
fn owe_note(note: &Mutex<Option<Owed>>, since: u64) {
    let mut owed = note.lock().unwrap_or_else(PoisonError::into_inner);
    let since = owed.map_or(since, |o| o.since.min(since));
    *owed = Some(Owed {
        since,
        unread: 0,
        asked: None,
        by: Some(Instant::now()),
    });
}

/// Ask the session's owed note once ([`Acts::behind`], with no lock held
/// over the ask) and keep what it decides: nothing more owed once it noted
/// the session behind — the owner's view looked at again — or found nothing
/// to note; asked again after [`relaunch::BUSY`] while another actor holds
/// the state's lock, and on [`NOTE_AGAIN`] while the session cannot be read.
/// What it said; `None` when no note is owed.
fn ask_note(sid: &str, note: &Mutex<Option<Owed>>, hooks: &Hooks) -> Option<Behind> {
    let lock = || note.lock().unwrap_or_else(PoisonError::into_inner);
    let since = lock().as_ref()?.since;
    let said = (hooks.acts.behind)(sid, since);
    let now = Instant::now();
    let mut owed = lock();
    match (said, owed.as_mut()) {
        (Behind::Noted | Behind::Nothing, _) => *owed = None,
        (Behind::Busy, Some(o)) => {
            o.asked = Some(now);
            o.by = Some(now + (hooks.pause)(relaunch::BUSY));
        }
        (Behind::Unread(_), Some(o)) => {
            o.asked = Some(now);
            o.by = NOTE_AGAIN
                .get(o.unread as usize)
                .map(|pause| now + (hooks.pause)(*pause));
            o.unread = o.unread.saturating_add(1);
        }
        (_, None) => {}
    }
    drop(owed);
    if said == Behind::Noted {
        note_upgrade_act();
    }
    Some(said)
}

/// The host thread's ask of a worker's owed note at a wake ([`Owed`]):
/// asked once its instant has come, or — while it could not read — once the
/// host wakes [`NOTE_GAP`] or more after the last ask. The instant the host
/// is to wake for it, if any.
fn host_note(
    sid: &str,
    note: &Mutex<Option<Owed>>,
    now: Instant,
    hooks: &Hooks,
) -> Option<Instant> {
    let asks = {
        let owed = note.lock().unwrap_or_else(PoisonError::into_inner);
        let o = owed.as_ref()?;
        let by = o.by?;
        by <= now
            || (o.unread > 0
                && o.asked
                    .is_some_and(|at| now >= at + (hooks.pause)(NOTE_GAP)))
    };
    if asks {
        ask_note(sid, note, hooks);
    }
    note.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|o| o.by)
}

/// One worker's upgrade, between its steps.
#[derive(Default)]
struct UpgradeRun {
    /// The steps in a row that waited, on anything: what bounds how long the
    /// upgrade owns the session's turn ends ([`upgrade_drive::owns_turn_ends`]).
    waits: u32,
    /// The steps in a row that waited on the SAME word ([`Self::last`]): each
    /// next look waits longer ([`upgrade_drive::LATER`]). A wait on something
    /// new starts the pauses over — the step moved on (a Codex daemon went
    /// first, and now its client settles): measured live on 2026-09-26, the
    /// daemon's `held` and `settling` and then the client's own `settling`
    /// climbed the ladder together, and the client's twenty-second settle
    /// was looked at again ten minutes later. So does a turn the session ran
    /// in between ([`WorkerIdle::turn_ran`]): its end is a point of its own
    /// (its [`Self::waits`] kept).
    same: u32,
    /// The word the last step waited on (empty after anything but a wait).
    last: String,
    /// A step that could not run was said: said once, until one runs.
    broken: bool,
    /// The looks in a row whose read failed ([`UNREAD_LOOK`]'s rung). They
    /// are no waits of the step's: [`Self::waits`] and [`Self::same`] are
    /// left as they stand.
    unread: u32,
}

impl UpgradeRun {
    /// [`upgrade_drive::after`] for one step's word, the counts kept: the
    /// pause climbs only while the step waits on the same word.
    fn after(&mut self, step: &str) -> After {
        if self.last != step {
            self.same = 0;
        }
        let after = upgrade_drive::after(step, self.same);
        // Only a WAIT counts toward the bounded hold on turn ends. A step that
        // acted and then looks again later (a re-arm, a stop resting until its
        // next round) starts every count over, as an act does: a new round that
        // inherited the rest's two hours of looks would own no turn end, its
        // twenty-second settle would never complete, and its first notice would
        // never be typed (the no-stall review of 2026-09-27, B3).
        if matches!(after, After::Later(_)) && step.starts_with("wait:") {
            self.waits = self.waits.saturating_add(1);
            self.same = self.same.saturating_add(1);
            step.clone_into(&mut self.last);
        } else {
            self.waits = 0;
            self.same = 0;
            self.last.clear();
        }
        after
    }

    /// A turn ran since the last look: the ladder starts over at the new
    /// point, the waits that bound the upgrade's hold on the turn ends kept.
    fn new_point(&mut self) {
        self.same = 0;
        self.last.clear();
    }

    /// Nothing to step: every count starts over.
    fn reset(&mut self) {
        self.waits = 0;
        self.same = 0;
        self.last.clear();
        self.unread = 0;
    }

    /// A look whose read failed: the pause before the next one
    /// ([`UNREAD_LOOK`]), the step's own counts untouched.
    fn unread(&mut self) -> Duration {
        let pause = aterm_agent::supervise::ladder::Ladder(&UNREAD_LOOK)
            .step(usize::try_from(self.unread).unwrap_or(usize::MAX));
        self.unread = self.unread.saturating_add(1);
        pause
    }
}

/// How soon a look whose read failed ([`Due::Unread`]) is taken again: a
/// short ladder of its own, a minute at most, for as long as the read fails
/// (the review of the silent-drop fix). A socket whose lanes were all held
/// gave them back within a minute and a half (the re-test of 155c72a28), a
/// process read or a record caught half-written reads at the next try, and
/// a session that answered READY is not left idle for the step's ten-minute
/// rung ([`upgrade_drive::LATER`]) once it can be read. Nor does an unread
/// look count as one of the step's waits: those bound how long the upgrade
/// owns a READY'd session's turn ends
/// ([`upgrade_drive::OWNED_BACKGROUND_LOOKS`]), and an
/// outage must not spend them.
const UNREAD_LOOK: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// THE WORKER'S PART AT ITS LOOP'S IDLE POINTS ([`IdleHost`]): the request
/// for the next idle point ([`WorkerJob::park`]), the step taken there — the
/// continuation a relaunched agent is owed first ([`relaunch::resume`],
/// whatever `[harness] upgrade` says), else the upgrade's — and whether the
/// upgrade owns the session's turn ends, from that step's own result.
pub(crate) struct WorkerIdle {
    sid: String,
    agent: Program,
    grace: u32,
    park: Arc<AtomicBool>,
    look_at: Arc<Mutex<Option<Instant>>>,
    acting: Arc<AtomicBool>,
    stalled: Arc<AtomicBool>,
    switches: Arc<Switches>,
    kept: Arc<Mutex<Kept>>,
    hooks: Hooks,
    run: Mutex<UpgradeRun>,
    /// The last step left the upgrade owning the session's turn ends.
    owns: AtomicBool,
    /// Not before this instant is a break of the agent's own background work
    /// looked at again for the notice ([`BACKGROUND_LOOK`]).
    background_at: Mutex<Option<Instant>>,
    /// Whether the session's conversation has a task, as far as its record
    /// has been read ([`WorkerIdle::taskless`]).
    tasked: Mutex<upgrade_drive::Tasked>,
    /// The session's note behind still to be taken ([`Owed`]).
    note: Note,
    /// The unix second the loop last knew the session at its limit, not yet
    /// applied to the upgrade's clocks ([`Acts::hold`]: another sweep held
    /// the lock) — applied before the next step.
    clock_hold: Mutex<Option<u64>>,
}

impl std::fmt::Debug for WorkerIdle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerIdle")
            .field("sid", &self.sid)
            .field("park", &self.park)
            .field("owns", &self.owns)
            .finish_non_exhaustive()
    }
}

impl IdleHost for WorkerIdle {
    fn wants(&self) -> bool {
        self.park.load(Ordering::SeqCst)
    }

    fn owns_turn_end(&self) -> bool {
        self.owns.load(Ordering::SeqCst) && self.switches.upgrade()
    }

    /// Where the loop is at idle for the host: read the relaunch record
    /// again, and take ONE step when the session has one to take, then say
    /// when to look again ([`upgrade_drive::after`]): at the next idle point
    /// after an announcement, an adoption or a continuation, on a growing
    /// pause after a wait, never after the last word. A look whose read
    /// FAILED ([`Due::Unread`]) decides nothing — journaled `upgrade
    /// step=wait:<word>` and looked at again on its own short ladder
    /// ([`UNREAD_LOOK`]), what the upgrade owned and the step's counts left
    /// as they stood (it read nothing to change them) — never "nothing to
    /// take": only a look that reads the session whole and finds nothing
    /// lets the upgrade go (the live re-test of 155c72a28: a look refused by
    /// a full control socket let four sessions go for good, one that had
    /// answered READY among them). The request is cleared FIRST, so a notice
    /// that lands during the step asks again.
    fn at_idle(&self) -> Option<HostStep> {
        self.park.store(false, Ordering::SeqCst);
        if !upgrades(self.agent) {
            return None;
        }
        let hooks = &self.hooks;
        let claude = self.agent == Program::Claude;
        if claude {
            refresh_snapshot(&self.sid, &self.kept, hooks);
        }
        // A limit stamp the lock kept out goes on the record before the step
        // reads it.
        self.hold_clock();
        // A note behind still owed (N3) is asked first: a step that mints the
        // state then finds it minted with its age from the attach.
        if self.switches.upgrade() {
            ask_note(&self.sid, &self.note, hooks);
        }
        let mut run = self.run.lock().unwrap_or_else(PoisonError::into_inner);
        let due = || {
            if self.switches.upgrade() {
                (hooks.acts.due)(&self.sid)
            } else {
                Due::No
            }
        };
        let (what, step) = if (hooks.acts.owed)(&self.sid) {
            // Types into the agent, never ends it: its exit, meanwhile, is one.
            ("carry-on", (hooks.acts.resume)(&self.sid, self.grace))
        } else {
            match due() {
                Due::Yes => {
                    self.acting.store(true, Ordering::SeqCst);
                    let step = (hooks.acts.step)(&self.sid, self.grace);
                    self.acting.store(false, Ordering::SeqCst);
                    ("upgrade", step)
                }
                Due::Unread(why) => {
                    // Read nothing, moved nothing: what the upgrade owned,
                    // its counts and what the owner sees all stand.
                    let pause = run.unread();
                    *self.look_at.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(Instant::now() + (hooks.pause)(pause));
                    ring();
                    return Some(HostStep {
                        line: format!("upgrade step=wait:{why}"),
                        moved: false,
                        typed: false,
                    });
                }
                Due::No => {
                    run.reset();
                    self.owns.store(false, Ordering::SeqCst);
                    return None;
                }
            }
        };
        run.unread = 0;
        self.owns.store(
            upgrade_drive::owns_turn_ends(&step, run.waits),
            Ordering::SeqCst,
        );
        // The host decides again what the session is now (its agent may have
        // been ended and relaunched by the step), and what the owner sees.
        note_upgrade_act();
        if claude {
            refresh_snapshot(&self.sid, &self.kept, hooks);
        }
        match run.after(&step) {
            After::NextIdle => {
                run.broken = false;
                self.park.store(true, Ordering::SeqCst);
            }
            After::Later(pause) => {
                run.broken = false;
                *self.look_at.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(Instant::now() + (hooks.pause)(pause));
                ring();
            }
            After::Finished => {
                run.broken = false;
            }
            After::Broken => {
                if !std::mem::replace(&mut run.broken, true) {
                    aterm_log::warn!(
                        "harness @{}: the live upgrade cannot run: {step} (its state directory \
                         cannot hold the lock); it is tried again at the next activation notice",
                        self.sid
                    );
                }
            }
        }
        Some(HostStep {
            line: format!("{what} step={step}"),
            moved: upgrade_drive::moved(&step),
            typed: upgrade_drive::typed(&step),
        })
    }

    /// A BREAK OF THE AGENT'S OWN BACKGROUND WORK its loop offers (the owner's
    /// answer of 2026-09-26: "Busy agentic sessions get upgraded at their
    /// next natural break. The notice interrupts the agent's orchestration
    /// once, and the restart still never kills running work"). Where an
    /// upgrade is due and no carry-on is owed, the step is taken as a NOTICE
    /// ALONE ([`Acts::notice`]). That is the first notice, or a re-ask once
    /// `REASK_S` has passed with the work still running (a tab whose poll
    /// loops could never end was otherwise told once and then waited on for
    /// days). Past `MAX_ASKS` the upgrade gives up here. Nothing is ended.
    /// Once a notice is typed, the upgrade owns the session's turn ends, as
    /// after any notice. A last word, or a word that holds the upgrade,
    /// releases them ([`upgrade_drive::released_at_break`]). The restart and a
    /// carry-on stay
    /// the next idle point's (the park is left set for it). A break that
    /// typed nothing is looked at again after [`BACKGROUND_LOOK`].
    fn at_background(&self) -> Option<String> {
        if !upgrades(self.agent) || !self.switches.upgrade() {
            return None;
        }
        let lock = || {
            self.background_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        };
        if lock().is_some_and(|at| Instant::now() < at) {
            return None;
        }
        *lock() = Some(Instant::now() + BACKGROUND_LOOK);
        let hooks = &self.hooks;
        // Nothing read decides nothing here either: the idle point the park
        // keeps looks again.
        if (hooks.acts.owed)(&self.sid) || (hooks.acts.due)(&self.sid) != Due::Yes {
            return None;
        }
        self.acting.store(true, Ordering::SeqCst);
        let step = (hooks.acts.notice)(&self.sid, self.grace);
        self.acting.store(false, Ordering::SeqCst);
        // What the owner sees moves with the step, typed or waiting.
        note_upgrade_act();
        if upgrade_drive::released_at_break(&step) {
            self.owns.store(false, Ordering::SeqCst);
        }
        if !step.starts_with("announced:") {
            return None;
        }
        // Typed: the answer and the restart are the idle point's — a later
        // break has nothing to add before the pause.
        self.owns.store(true, Ordering::SeqCst);
        Some(format!("upgrade step={step}"))
    }

    /// THE RESTART IN PLACE its loop asks for ([`Restart`]): the agent ended
    /// at this idle point and relaunched on its conversation
    /// ([`relaunch::restart_here`], one step under `human_grace_s`), taken
    /// as the worker's own act — the agent leaving and coming back during
    /// it is no exit ([`WorkerJob::acting`]) — and, once the new process is
    /// the loop's (`adopted`), the next idle point asked for, where its
    /// continuation is typed. None for an agent the relaunch is not written
    /// for, or under `[harness] relaunch = false` (read live): the point is
    /// then the person's.
    fn restart(&self, why: &Restart) -> Option<String> {
        if !self.can_restart() {
            return None;
        }
        let hooks = &self.hooks;
        self.acting.store(true, Ordering::SeqCst);
        let step = (hooks.acts.restart)(&self.sid, self.grace, why);
        self.acting.store(false, Ordering::SeqCst);
        if step == "adopted" {
            self.park.store(true, Ordering::SeqCst);
        }
        // The host decides again what the session is now, and what the owner
        // sees.
        note_upgrade_act();
        refresh_snapshot(&self.sid, &self.kept, hooks);
        Some(step)
    }

    /// A Claude Code under `[harness] relaunch` (read live): the agent the
    /// relaunch is written for, where the owner has not taken it away.
    fn can_restart(&self) -> bool {
        self.agent == Program::Claude && self.switches.relaunch()
    }

    /// The loop holds for a stall the server published, or it lifted: kept
    /// for [`on_agent_left`] — an exit while it is held was the stall's
    /// remedy's (U1).
    fn stalled(&self, held: bool) {
        self.stalled.store(held, Ordering::SeqCst);
    }

    /// A TURN RAN since the loop's last point ([`IdleHost::turn_ran`]): the
    /// pause the upgrade climbed was of a point that is gone, so its ladder
    /// starts over ([`UpgradeRun::new_point`]) and the new point is looked at
    /// as it comes, its first pause the first rung again (D3 of the live E2E
    /// of 2026-09-26: the settle's pause climbed across the Stage-1 turn, and
    /// the turn end after it was looked at +0.3 s, +61 s and +366 s). Its
    /// waits in a row still count: they bound how long it owns the session's
    /// turn ends ([`upgrade_drive::owns_turn_ends`]), which a self-waking agent must not renew.
    /// A step that did not wait left nothing to start over.
    fn turn_ran(&self) {
        let mut run = self.run.lock().unwrap_or_else(PoisonError::into_inner);
        if run.same == 0 {
            return;
        }
        run.new_point();
        let pending = self
            .look_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .is_some();
        if pending {
            self.park.store(true, Ordering::SeqCst);
        }
    }

    /// NOBODY HAS ASKED ITS CONVERSATION ANYTHING (D1 of the live E2E of
    /// 2026-09-26): the conversation a Claude Code holds — the kept record,
    /// followed across an in-app `/clear` ([`follow_snapshot`]) — read through
    /// its transcript from where the last read ended ([`Acts::tasked`]): no
    /// prompt of a person's or an orchestrator's — the harness's own notices
    /// and carry-ons, and what this session's own loop typed (its ledger),
    /// being none. The turn-end policy types nothing into it. A Codex, a
    /// conversation not registered yet and a record nobody can read are left
    /// to the screen.
    fn taskless(&self) -> bool {
        if self.agent != Program::Claude {
            return false;
        }
        let session = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .as_ref()
            .and_then(|s| s.session.clone());
        let Some(session) = session else {
            return false;
        };
        let mut memo = self.tasked.lock().unwrap_or_else(PoisonError::into_inner);
        (self.hooks.acts.tasked)(&self.sid, &session, &mut memo) == Some(false)
    }

    /// The loop's LIMIT EPISODE opened or closed: the live upgrade owns none
    /// of the session's turn ends while one stands (its gates wait `limited`,
    /// and the wall is the loop's to wait out), and its clocks are held to
    /// now — at the open and again at the close, so a notice whose wind-down
    /// turn hit the limit gets its whole window once the agent can read
    /// again ([`upgrade_drive::hold_clock`]; the review of 2026-09-26: no look
    /// of the upgrade's own happens during an episode, so its window had run
    /// out by the first idle point after the reset). A stamp the lock kept
    /// out is applied before the next step ([`Self::hold_clock`]).
    fn limited(&self, open: bool) {
        if !upgrades(self.agent) {
            return;
        }
        if open {
            self.owns.store(false, Ordering::SeqCst);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        *self
            .clock_hold
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(now);
        self.hold_clock();
    }
}

impl WorkerIdle {
    /// Apply the limit stamp [`IdleHost::limited`] left, if any
    /// ([`Acts::hold`]); kept for the next try while another sweep holds the
    /// lock.
    fn hold_clock(&self) {
        let mut pending = self
            .clock_hold
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(until) = *pending
            && (self.hooks.acts.hold)(&self.sid, until)
        {
            *pending = None;
        }
    }
}

/// Read the relaunch record again. A read that fails keeps the last one only
/// while it still describes the tab ([`follow_snapshot`]).
fn refresh_snapshot(sid: &str, kept: &Mutex<Kept>, hooks: &Hooks) {
    match (hooks.acts.snapshot)(sid) {
        Some(snap) => {
            publish_snapshot(sid, Some(&snap));
            kept.lock().unwrap_or_else(PoisonError::into_inner).snapshot = Some(snap);
        }
        None => {
            follow_snapshot(sid, kept, hooks);
        }
    }
}

/// THE SNAPSHOTS A CRASH CANNOT TAKE (2026-09-27): every session's kept
/// relaunch snapshot, published as it changes so the window's crash journal
/// ([`crate::crash_journal`]) carries it — what the next launch needs to
/// relaunch the agent in its restored tab after aterm itself ended without
/// quitting. Keyed by session id; a leaf lock (taken after, never around,
/// a session's [`Kept`]).
static PUBLISHED: Mutex<Option<HashMap<String, Snapshot>>> = Mutex::new(None);

/// Publish `sid`'s kept snapshot (`None`: it has none).
fn publish_snapshot(sid: &str, snap: Option<&Snapshot>) {
    let mut map = PUBLISHED.lock().unwrap_or_else(PoisonError::into_inner);
    let map = map.get_or_insert_with(HashMap::new);
    match snap {
        Some(snap) => {
            map.insert(sid.to_string(), snap.clone());
        }
        None => {
            map.remove(sid);
        }
    }
}

/// The relaunch snapshot the host keeps for session `sid`, as last
/// published ([`publish_snapshot`]).
pub(crate) fn published_snapshot(sid: &str) -> Option<Snapshot> {
    PUBLISHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|m| m.get(sid).cloned())
}

/// FOLLOW the session's kept snapshot against the tab's foreground job,
/// cheaply — no socket, no `ps` ([`Acts::follow`]): while its agent holds
/// the tab, take the conversation Claude's own record names now (an in-app
/// `/clear` moves it); while the shell holds it, keep it for the exit still
/// to be handled; once another job holds it, forget it — it describes an
/// agent that is gone, and a wrapper started there must never be relaunched
/// as that agent. Run by the host for due workers and by a worker whose own
/// read failed. The action runs with no lock held; a snapshot a worker wrote
/// meanwhile wins. Returns whether a snapshot existed to follow.
fn follow_snapshot(sid: &str, kept: &Mutex<Kept>, hooks: &Hooks) -> bool {
    let lock = || kept.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(before) = lock().snapshot.clone() else {
        return false;
    };
    let mut snap = before.clone();
    let fg = (hooks.acts.follow)(sid, &mut snap);
    let mut session = lock();
    if session.snapshot.as_ref() != Some(&before) {
        return false;
    }
    match fg {
        Foreground::Agent => {
            publish_snapshot(sid, Some(&snap));
            session.snapshot = Some(snap);
        }
        Foreground::Shell => {}
        Foreground::Other => {
            publish_snapshot(sid, None);
            session.snapshot = None;
        }
    }
    true
}

/// The agent's exit is handled: what was read of it goes with it.
fn forget_snapshot(job: &WorkerJob) {
    publish_snapshot(&job.sid, None);
    with_kept(job, |k| k.snapshot = None);
}

/// `f` over the session's [`Kept`], under its lock (a leaf: `f` takes no
/// other lock).
fn with_kept<R>(job: &WorkerJob, f: impl FnOnce(&mut Kept) -> R) -> R {
    let mut kept = job.kept.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut kept)
}

/// RELAUNCH ON EXIT: the session's agent left a tab that lives on. Whose
/// exit it was decides ([`relaunch::on_exit`]): a person's (a keystroke
/// within the grace) or a holder's (a halt, a lease, a named driver's turn)
/// is theirs, and the tab is left to them; one the owner limited — `[harness]
/// relaunch = false`, or an agent the relaunch is not written for — is said
/// once on the session's attention; any other is relaunched on its
/// conversation after the session's back-off, cut short when the agent is
/// back or the worker is stopped for good, and asked about again before each
/// try — trying again on the growing pause until it lands, until the exit
/// proves to be the launch's own end (journaled, nothing said), or until it
/// can never land (said).
fn on_agent_left(job: &WorkerJob, hooks: &Hooks) {
    let sid = job.sid.as_str();
    let grace = job.opts.policy.human_grace_s;
    // Its loop held for a stall when it left: the stall's remedy ended it
    // (U1). Taken: the next agent's stall is its own.
    let stalled = job.stalled.swap(false, Ordering::SeqCst);
    let decide = || {
        let allowed = job.agent == Program::Claude && job.switches.relaunch();
        relaunch::on_exit(allowed, (hooks.acts.status)(sid).as_deref(), grace, stalled)
    };
    let decision = decide();
    let mut pause = match with_kept(job, |k| k.relaunches.exited(decision, Instant::now())) {
        Ok(pause) => pause,
        Err(said) => {
            forget_snapshot(job);
            left_alone(job, hooks, decision, said);
            return;
        }
    };
    let snap = with_kept(job, |k| k.snapshot.clone());
    let Some(snap) = snap else {
        let why = "nothing-recorded";
        with_kept(job, |k| {
            k.relaunches
                .attempted(&Outcome::Left(why.to_string()), Instant::now())
        });
        aterm_log::info!(
            "harness @{sid}: the agent exited; not relaunched: {why} (nothing about it could be \
             read while it ran: a wrapper or a script, not a launch this relaunches)"
        );
        return;
    };
    // WHAT THE EXIT LEFT of Claude's own record — a crash leaves it, a
    // graceful exit removes it — read NOW, within a short settle of the exit
    // and before the back-off, and kept for every attempt: any Claude Code
    // started meanwhile removes a dead agent's record, and a crash read
    // after the back-off was a graceful exit, silently never relaunched (D2
    // of the 2026-09-26 live test).
    let left = relaunch::exit_record(
        || (hooks.acts.exit_look)(sid, &snap),
        |step| {
            !wait_for(Instant::now() + (hooks.pause)(step), || {
                !job.left.load(Ordering::SeqCst)
            })
        },
    );
    aterm_log::info!(
        "harness @{sid}: the agent exited; its session record {} (read at the exit)",
        left.word()
    );
    loop {
        if wait_for(Instant::now() + (hooks.pause)(pause), || {
            !job.left.load(Ordering::SeqCst)
        }) {
            if (hooks.still_wanted)(sid) {
                let said = with_kept(job, |k| k.relaunches.running());
                tell(hooks, sid, said, "");
            }
            return;
        }
        let decision = decide();
        if let Some(said) = with_kept(job, |k| k.relaunches.decide(decision)) {
            forget_snapshot(job);
            left_alone(job, hooks, decision, said);
            return;
        }
        // On the build the upgrade would move it to only while
        // `[harness] upgrade` allows (read live, as the relaunch switch is).
        let step = (hooks.acts.relaunch)(sid, grace, &snap, &left, job.switches.upgrade(), stalled);
        note_upgrade_act();
        let outcome = relaunch::outcome(&step);
        let said = with_kept(job, |k| k.relaunches.attempted(&outcome, Instant::now()));
        tell(hooks, sid, said, &step);
        match outcome {
            Outcome::NotYet(_) => pause = with_kept(job, |k| k.relaunches.pause()),
            Outcome::Busy => pause = relaunch::BUSY,
            Outcome::Relaunched => {
                // What a relaunch of the NEW agent needs, read while it runs
                // (if it is already gone, the one kept still names its
                // conversation and its shell).
                refresh_snapshot(&job.sid, &job.kept, hooks);
                return;
            }
            Outcome::Left(why) => {
                forget_snapshot(job);
                aterm_log::info!("harness @{sid}: the agent exited; not relaunched: {why}");
                return;
            }
            Outcome::Cannot(_) => {
                forget_snapshot(job);
                return;
            }
        }
    }
}

/// An exit the host leaves alone ([`relaunch::on_exit`] said `decision`):
/// journaled, and said on the session's attention when the relaunch was
/// LIMITED — by the owner's `[harness]`, or for an agent it is not written
/// for.
fn left_alone(job: &WorkerJob, hooks: &Hooks, decision: OnExit, said: Say) {
    let sid = job.sid.as_str();
    let why = match decision {
        OnExit::PersonAsked => "a person typed into the session just before: the exit is theirs",
        OnExit::Held => {
            "the session is held (a halt, a lease or a driver's turn): the exit is theirs"
        }
        // No configuration took this away: the relaunch is not built for
        // this agent yet (the philosophy review of 2026-09-25: it was worded
        // as a limit). What it printed is the way back.
        OnExit::Limited if job.agent != Program::Claude => {
            "relaunch not built for this agent yet (no [harness] limit); use its resume line"
        }
        OnExit::Limited => "[harness] relaunch = false",
        OnExit::Relaunch => "",
    };
    aterm_log::info!(
        "harness @{sid}: {} exited; not relaunched: {why}",
        job.agent.name()
    );
    tell(hooks, sid, said, why);
}

/// Say a relaunch's word on the session's attention (`why`: the step, or the
/// limit, that said it).
fn tell(hooks: &Hooks, sid: &str, said: Say, why: &str) {
    let why: String = why.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = why.len().min(80);
    while !why.is_char_boundary(end) {
        end -= 1;
    }
    let why = &why[..end];
    let text = match said {
        Say::Nothing => return,
        Say::Clear => {
            (hooks.badge)(sid, None);
            return;
        }
        Say::Cannot => format!("the agent exited and cannot be relaunched: {why}"),
        Say::Failing => {
            format!("the agent exited and its relaunch keeps failing (still retrying): {why}")
        }
        Say::Limited => format!("the agent exited and is not relaunched: {why}"),
    };
    aterm_log::warn!("harness @{sid}: {text}");
    (hooks.badge)(sid, Some(&text));
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
    /// [`WorkerJob::park`], [`WorkerJob::look_at`], [`WorkerJob::left`] and
    /// [`WorkerJob::acting`].
    park: Arc<AtomicBool>,
    look_at: Arc<Mutex<Option<Instant>>>,
    left: Arc<AtomicBool>,
    acting: Arc<AtomicBool>,
    /// The host's activation notices this worker was told of.
    activations: u64,
    /// [`WorkerJob::note`]: asked at the host's wakes.
    note: Note,
}

impl Worker {
    fn stop(&self) {
        request_stop(&self.stop, &self.left, &self.interrupt);
    }

    /// Stop it because its agent LEFT a tab that lives on: the worker
    /// handles the exit ([`on_agent_left`]) as it ends.
    fn leave(&self) {
        self.left.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        ring();
        if let Some(cut) = self
            .interrupt
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            cut();
        }
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

/// A worker's stop flag, its exit flag and its connection's interrupter slot.
type StopHandle = (
    Arc<AtomicBool>,
    Arc<AtomicBool>,
    Arc<Mutex<Option<Interrupter>>>,
);

struct Shared {
    state: Mutex<State>,
    headless: bool,
    /// Every session's [`FaultHistory`], kept by the host thread across its
    /// workers (taken alone, or under `state` — never the other way).
    faults: Mutex<HashMap<String, FaultHistory>>,
    /// Every open session's [`Relaunches`], kept across its workers (taken
    /// alone, never under `state`).
    kept: Mutex<HashMap<String, Arc<Mutex<Kept>>>>,
    /// The host's `upgrade` and `relaunch` switches ([`Switches`]).
    switches: Arc<Switches>,
    /// Activation notices seen ([`HostHandle::note_activation`]).
    activations: Arc<AtomicU64>,
    /// Wakes the activation thread's parked wait at shutdown
    /// ([`spawn_activation_wake`]; set once, when it starts).
    wake: std::sync::OnceLock<WakeTrigger>,
    /// Set (then the bell rung) when the host thread's loop has returned.
    host_done: AtomicBool,
    /// Host threads started again after one ended in a panic
    /// ([`HOST_RESTARTS`] at most).
    host_restarts: AtomicU64,
}

/// How many times a host thread that ended in a panic is started again (by
/// the next policy change or resume, [`HostHandle::ensure_thread`]) before
/// supervision stays off for the process.
const HOST_RESTARTS: u64 = 3;

/// The disk watch's period: a free-space figure has no event to hang on
/// (design §5.5, §4.2 row 18), so this is the one timer the host keeps.
const DISK_TICK: Duration = Duration::from_secs(6 * 3600);
/// The first look, a while after the host starts rather than in its first
/// breath.
const DISK_FIRST_LOOK: Duration = Duration::from_secs(120);

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
    /// (`[harness] enabled`, and not `headless = false` in a headless
    /// instance, and not `suspended`).
    pub(crate) fn start(
        cfg: SupervisorConfig,
        headless: bool,
        suspended: bool,
        hooks: Hooks,
    ) -> Self {
        let switches = Arc::new(Switches::default());
        switches.set(&cfg);
        let handle = Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    cfg: effective(&cfg),
                    suspended,
                    ..State::default()
                }),
                headless,
                faults: Mutex::default(),
                kept: Mutex::default(),
                switches,
                activations: Arc::default(),
                wake: std::sync::OnceLock::new(),
                host_done: AtomicBool::new(false),
                host_restarts: AtomicU64::new(0),
            }),
            thread: Arc::default(),
            hooks,
        };
        handle.ensure_thread();
        handle
    }

    /// Whether the host supervises agent sessions now: its policy on (and
    /// on for a headless instance's sessions where this is one), and not
    /// suspended for a handoff. The menu bar reads it: an agent's box on a
    /// session the host supervises is the host's to answer or to escalate
    /// (keyed attention), never a row of its own — even in the moment
    /// before the host's loop has taken the session's claim.
    pub(crate) fn supervising(&self) -> bool {
        let state = self.shared.lock();
        Shared::active(&state, self.shared.headless)
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

    /// THE AGENTS A CRASH OF ATERM TOOK (2026-09-27): the next launch
    /// reopened their tabs (the crash journal, or a restart's quit layout),
    /// each `(sid, snapshot)` naming its new tab and shell and what the
    /// layout carried of its agent.
    /// Each is relaunched there on its conversation
    /// ([`relaunch::after_host_ended`]) on a thread of its own — never this
    /// caller's, the event loop — while `[harness] relaunch` allows it: a
    /// step that is not yet possible (the shell still starting) is tried
    /// again on a growing pause, [`RESTORED_TRIES`] times at most; a
    /// relaunch, a launch that ends there, and one that cannot be made are
    /// journaled, never retried. A headless instance relaunches nothing.
    pub(crate) fn relaunch_restored(&self, restored: Vec<(String, Snapshot)>) {
        if restored.is_empty() || self.shared.headless || !self.shared.switches.relaunch() {
            return;
        }
        let act = Arc::clone(&self.hooks.acts.relaunch_restored);
        let switches = Arc::clone(&self.shared.switches);
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}restored"))
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                for (sid, snap) in restored {
                    let mut pause = RESTORED_FIRST_PAUSE;
                    for _ in 0..RESTORED_TRIES {
                        if !switches.relaunch() {
                            return;
                        }
                        let step = act(&sid, &snap, switches.upgrade());
                        match relaunch::outcome(&step) {
                            Outcome::NotYet(_) | Outcome::Busy => {
                                std::thread::sleep(pause);
                                pause = (pause * 2).min(RESTORED_MAX_PAUSE);
                            }
                            _ => break,
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            aterm_log::warn!("harness: the restored tabs' agents could not be relaunched: {e}");
        }
    }

    /// A reloaded `[harness]` policy: workers restart under it, or all stop
    /// (within one wait) when it is off. The host's own switches
    /// (`upgrade`, `relaunch`) land at each worker's next act, restarting
    /// nothing.
    pub(crate) fn set_config(&self, cfg: SupervisorConfig) {
        self.shared.switches.set(&cfg);
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
    #[cfg(any(unix, test))]
    pub(crate) fn suspend(&self) {
        let mut state = self.shared.lock();
        state.suspended = true;
        for (stop, left, interrupt) in &state.stops {
            request_stop(stop, left, interrupt);
        }
        drop(state);
        ring();
    }

    /// A newer agent build may be installed (atpkg's activation notice, or
    /// Claude's own native updater repointing its link). Every worker parks
    /// its loop at its session's next idle point and asks whether its
    /// session has an upgrade to take.
    pub(crate) fn note_activation(&self) {
        self.shared.activations.fetch_add(1, Ordering::SeqCst);
        ring();
    }

    /// Supervise again (the successor's Commit activated it, or a Commit
    /// failed and this process keeps its sessions).
    #[cfg(any(unix, test))]
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
        if let Some(wake) = self.shared.wake.get() {
            wake.pull();
        }
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

/// What the host thread keeps across its wakes.
struct HostState {
    workers: Workers,
    /// Last publication actually followed for each worker. An absent entry
    /// makes its first look unconditional, including after a worker restart.
    followed: HashMap<String, FollowStamp>,
    full_epoch_seen: u64,
    phase_epoch_seen: u64,
    /// sid -> the claim epoch its refused worker started at: retried only
    /// past it.
    held: HashMap<String, u64>,
    /// The policy generation whose change last forgave every history.
    forgiven_gen: u64,
    /// What the owner's view was last looked at over ([`look_at_upgrades`]).
    view: ViewLook,
    /// When the disk watch looks next ([`look_at_disk`]).
    disk_at: Instant,
}

/// What the host last looked at the owner's view of the upgrades over
/// ([`look_at_upgrades`]): the workers' acts and the activation notices seen,
/// the supervised tabs, and when the view said it changes next. `None`: it
/// is not shown (never looked at, or stood down).
#[derive(Default)]
struct ViewLook {
    shown: Option<(u64, u64, Vec<String>)>,
    next: Option<Instant>,
}

fn host_loop(shared: &Shared, hooks: &Hooks) {
    let mut host = HostState {
        workers: Workers::default(),
        followed: HashMap::new(),
        full_epoch_seen: FULL_FOLLOW_EPOCH.load(Ordering::SeqCst),
        phase_epoch_seen: PHASE_FOLLOW_EPOCH.load(Ordering::SeqCst),
        held: HashMap::new(),
        forgiven_gen: shared.lock().cfg_gen,
        view: ViewLook::default(),
        disk_at: Instant::now() + DISK_FIRST_LOOK,
    };
    let mut seen = bell_now();
    let mut timed_out = true;
    loop {
        // Capture wake cursors BEFORE reading the roster. A publication
        // arriving during that read remains owed on the next loop even if
        // this loop happened to see its new stamp.
        let full_now = FULL_FOLLOW_EPOCH.load(Ordering::SeqCst);
        let phase_now = PHASE_FOLLOW_EPOCH.load(Ordering::SeqCst);
        let follow_all = full_follow_due(
            timed_out,
            host.full_epoch_seen,
            full_now,
            host.phase_epoch_seen,
            phase_now,
        );
        host.full_epoch_seen = full_now;
        host.phase_epoch_seen = phase_now;
        let (cfg, cfg_gen, active, shutting_down) = {
            let state = shared.lock();
            (
                state.cfg.clone(),
                state.cfg_gen,
                Shared::active(&state, shared.headless),
                state.shutting_down,
            )
        };
        reap(&mut host);
        if shutting_down {
            shutdown_workers(host.workers);
            return;
        }
        // Every agent the roster names is one the ONE predicate supervises
        // ([`agent_of`]).
        let (wanted, stamps): (HashMap<String, Program>, HashMap<String, FollowStamp>) = if active {
            (hooks.roster)().into_iter().fold(
                (HashMap::new(), HashMap::new()),
                |(mut wanted, mut stamps), (sid, program, stamp)| {
                    wanted.insert(sid.clone(), program);
                    stamps.insert(sid, stamp);
                    (wanted, stamps)
                },
            )
        } else {
            (HashMap::new(), HashMap::new())
        };
        host.held.retain(|sid, _| wanted.contains_key(sid));
        age_and_forgive(shared, &mut host, cfg_gen, &wanted);
        forget_closed(shared, &host.workers, &wanted, hooks);
        follow_workers(shared, &mut host, &wanted, &stamps, follow_all, hooks);
        let next_look = visit_workers(shared, &mut host.workers, &wanted, active, cfg_gen, hooks);
        start_workers(shared, &mut host, &wanted, &cfg, cfg_gen, hooks);
        let view_at = look_at_upgrades(
            &mut host.view,
            &wanted,
            active && shared.switches.upgrade(),
            (
                UPGRADE_ACTS.load(Ordering::SeqCst),
                shared.activations.load(Ordering::SeqCst),
            ),
            hooks,
        );
        let disk_at = look_at_disk(&mut host.disk_at, Instant::now(), active, &wanted, hooks);
        let until = [next_look, view_at, disk_at].into_iter().flatten().min();
        (seen, timed_out) = bell_wait(seen, until);
    }
}

/// A generic wake always owes the old full follow. A timer does too, so a
/// missed or unreadable phase publication cannot leave a conversation stale.
/// Multiple phase wakes may have changed one tab and changed it back; compare
/// stamps only for exactly one phase wake. A wrapped counter is also unknown.
fn full_follow_due(
    timed_out: bool,
    full_before: u64,
    full_now: u64,
    phase_before: u64,
    phase_now: u64,
) -> bool {
    timed_out || full_before != full_now || phase_now < phase_before || phase_now - phase_before > 1
}

/// THE DISK WATCH's timer: when [`HostState::disk_at`] has come, the
/// supervised sessions are handed to [`Hooks::disk`] and the next look is
/// [`DISK_TICK`] on. Nothing is looked at while supervision is off or no
/// agent session is hosted — the watch is over where the agents work — and
/// then no instant is named. The instant to look again, if one.
fn look_at_disk(
    disk_at: &mut Instant,
    now: Instant,
    active: bool,
    wanted: &HashMap<String, Program>,
    hooks: &Hooks,
) -> Option<Instant> {
    if !active || wanted.is_empty() {
        return None;
    }
    if now >= *disk_at {
        let mut sids: Vec<String> = wanted.keys().cloned().collect();
        sids.sort();
        (hooks.disk)(&sids);
        *disk_at = now + DISK_TICK;
    }
    Some(*disk_at)
}

/// THE OWNER'S VIEW, looked at when something it shows may have moved: a
/// worker's upgrade or relaunch step and an activation notice or the owner's
/// word (`(acts, activations)`: [`UPGRADE_ACTS`], the host's count), the
/// supervised tabs, or the instant the view last named (an upgrade turning
/// overdue, a word running out). Nothing else looks: no timer. Stood down —
/// its rows gone from the window, its marks lowered — while the policy or
/// `[harness] upgrade` is off. The instant to look again, if the view named
/// one.
fn look_at_upgrades(
    view: &mut ViewLook,
    wanted: &HashMap<String, Program>,
    active: bool,
    (acts, activations): (u64, u64),
    hooks: &Hooks,
) -> Option<Instant> {
    if !active {
        if view.shown.take().is_some() {
            let _ = (hooks.upgrade_view)(false);
        }
        view.next = None;
        return None;
    }
    let mut tabs: Vec<String> = wanted.keys().cloned().collect();
    tabs.sort();
    let now = Instant::now();
    let seen = Some((acts, activations, tabs));
    if view.shown != seen || view.next.is_some_and(|at| at <= now) {
        let next = (hooks.upgrade_view)(true);
        let now_s = crate::upgrade_host::now_s();
        view.next = next.map(|at| now + Duration::from_secs(at.saturating_sub(now_s)));
        view.shown = seen;
    }
    view.next
}

/// Reap the workers that ended (their last act set `done`, so the join
/// waits on nothing but the thread's return); one held behind another
/// claim is remembered with the epoch it started at.
fn reap(host: &mut HostState) {
    let ended: Vec<String> = host
        .workers
        .0
        .iter()
        .filter(|(_, w)| w.join.is_none() || w.done.load(Ordering::SeqCst))
        .map(|(sid, _)| sid.clone())
        .collect();
    for sid in ended {
        let Some(mut w) = host.workers.0.remove(&sid) else {
            continue;
        };
        match w.join.take().map(JoinHandle::join) {
            Some(Ok(WorkerExit::Held)) => {
                host.held.insert(sid, w.epoch);
            }
            Some(Ok(WorkerExit::Stopped)) | None => {}
            // worker_main catches every run; a panic past it is the thread's
            // own bookkeeping: the session is started again at this wake.
            Some(Err(_)) => {
                aterm_log::warn!("harness @{sid}: the supervisor thread panicked; restarting it");
            }
        }
    }
}

/// A changed policy forgives every session's failures; otherwise a history
/// outlives its workers (and a flap of the program) until its entries age
/// out of the window.
fn age_and_forgive(
    shared: &Shared,
    host: &mut HostState,
    cfg_gen: u64,
    wanted: &HashMap<String, Program>,
) {
    let mut histories = shared.faults.lock().unwrap_or_else(PoisonError::into_inner);
    if cfg_gen != host.forgiven_gen {
        histories.clear();
        host.forgiven_gen = cfg_gen;
    }
    let now = Instant::now();
    histories.retain(|sid, history| {
        let mut h = history.lock().unwrap_or_else(PoisonError::into_inner);
        age_faults(&mut h, now);
        !h.is_empty() || wanted.contains_key(sid) || host.workers.0.contains_key(sid)
    });
}

/// What is kept of a session for a relaunch lives as long as its tab. (The
/// tab is asked about with the map's lock released.)
fn forget_closed(
    shared: &Shared,
    workers: &Workers,
    wanted: &HashMap<String, Program>,
    hooks: &Hooks,
) {
    let known: Vec<String> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .keys()
        .cloned()
        .collect();
    let closed: Vec<String> = known
        .into_iter()
        .filter(|sid| {
            !wanted.contains_key(sid) && !workers.0.contains_key(sid) && !(hooks.acts.open)(sid)
        })
        .collect();
    if !closed.is_empty() {
        let mut kept = shared.kept.lock().unwrap_or_else(PoisonError::into_inner);
        for sid in &closed {
            kept.remove(sid);
        }
    }
}

/// Follow each worker whose publication moved, and every worker after a
/// generic wake, coalesced phase wakes, or the old timer deadline.
fn follow_workers(
    shared: &Shared,
    host: &mut HostState,
    wanted: &HashMap<String, Program>,
    stamps: &HashMap<String, FollowStamp>,
    follow_all: bool,
    hooks: &Hooks,
) {
    let following: Vec<(String, Arc<Mutex<Kept>>)> = {
        let kept = shared.kept.lock().unwrap_or_else(PoisonError::into_inner);
        host.workers
            .0
            .keys()
            .filter(|sid| wanted.contains_key(*sid))
            .filter_map(|sid| kept.get(sid).map(|k| (sid.clone(), Arc::clone(k))))
            .collect()
    };
    host.followed
        .retain(|sid, _| host.workers.0.contains_key(sid));
    follow_entries(&following, stamps, &mut host.followed, follow_all, hooks);
}

/// Select and follow from the roster-backed candidates. The hook performs
/// the real session-file read in production and is counted in the test.
fn follow_entries(
    following: &[(String, Arc<Mutex<Kept>>)],
    stamps: &HashMap<String, FollowStamp>,
    followed: &mut HashMap<String, FollowStamp>,
    follow_all: bool,
    hooks: &Hooks,
) {
    for (sid, kept) in following {
        let stamp = stamps.get(sid).copied();
        if (follow_all || stamp.is_none() || followed.get(sid).copied() != stamp)
            && follow_snapshot(sid, kept, hooks)
            && let Some(stamp) = stamp
        {
            followed.insert(sid.clone(), stamp);
        }
    }
}

/// Each worker, against what the session is now: one whose agent is no
/// longer wanted is stopped — or handed the exit when the agent LEFT a tab
/// that lives on, unless its own upgrade step ended that agent — one whose
/// agent is back has its exit over, one under an older policy hands over,
/// and one told of a new activation, or whose look is due, is asked for its
/// loop's next idle point. The earliest look still to come.
fn visit_workers(
    shared: &Shared,
    workers: &mut Workers,
    wanted: &HashMap<String, Program>,
    active: bool,
    cfg_gen: u64,
    hooks: &Hooks,
) -> Option<Instant> {
    let activations = shared.activations.load(Ordering::SeqCst);
    let now = Instant::now();
    let mut next_look: Option<Instant> = None;
    for (sid, w) in &mut workers.0 {
        if !wanted.contains_key(sid) {
            // The agent a worker's own upgrade step ends is that step's: it
            // comes back relaunched. Otherwise an agent that left a tab that
            // lives on is the worker's to handle; a tab that closed (or a
            // policy that stopped) just stops it.
            if !w.acting.load(Ordering::SeqCst) {
                if active && (hooks.acts.open)(sid) {
                    if !w.stop.load(Ordering::SeqCst) {
                        w.leave();
                    }
                } else {
                    w.stop();
                }
            }
            continue;
        }
        if w.left.load(Ordering::SeqCst) {
            // The agent is back (relaunched, or started by someone): the exit
            // is over.
            w.left.store(false, Ordering::SeqCst);
            ring();
        } else if w.cfg_gen != cfg_gen {
            w.hand_over();
        }
        // A build installed mid-session (N3): the session is behind from the
        // notice, not from its next idle point — a busy turn's length later.
        if w.activations != activations {
            w.activations = activations;
            w.park.store(true, Ordering::SeqCst);
            if shared.switches.upgrade() && upgrades(wanted[sid]) {
                owe_note(&w.note, crate::upgrade_host::now_s());
            }
        }
        // A note still owed — from the notice, or from the attach — asked
        // until it decides ([`Owed`]).
        if shared.switches.upgrade()
            && let Some(at) = host_note(sid, &w.note, now, hooks)
        {
            next_look = Some(next_look.map_or(at, |n| n.min(at)));
        }
        let mut look = w.look_at.lock().unwrap_or_else(PoisonError::into_inner);
        match *look {
            Some(at) if at <= now => {
                *look = None;
                w.park.store(true, Ordering::SeqCst);
            }
            Some(at) => next_look = Some(next_look.map_or(at, |n| n.min(at))),
            None => {}
        }
    }
    next_look
}

/// Start a worker for every wanted session with none, not held behind a
/// claim not yet released — under the one hold of the state lock that
/// re-reads the policy and publishes the stop handles: a suspend() either
/// came first (nothing starts) or finds every worker started here.
fn start_workers(
    shared: &Shared,
    host: &mut HostState,
    wanted: &HashMap<String, Program>,
    cfg: &SupervisorConfig,
    cfg_gen: u64,
    hooks: &Hooks,
) {
    let epoch = (hooks.claim_epoch)();
    let activations = shared.activations.load(Ordering::SeqCst);
    let mut starts: Vec<&String> = wanted
        .keys()
        .filter(|sid| !host.workers.0.contains_key(*sid))
        .filter(|sid| host.held.get(*sid).is_none_or(|e| epoch > *e))
        .collect();
    starts.sort();
    let mut state = shared.lock();
    if !Shared::active(&state, shared.headless) || state.shutting_down {
        starts.clear();
    }
    for sid in starts {
        // A replacement worker attaches a fresh snapshot, even if the tab's
        // published phase stayed put across the handover.
        host.followed.remove(sid);
        host.held.remove(sid);
        let faults = Arc::clone(
            shared
                .faults
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(sid.clone())
                .or_default(),
        );
        let kept = Arc::clone(
            shared
                .kept
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(sid.clone())
                .or_default(),
        );
        // An agent runs in the tab: nothing of a relaunch is due, and a word
        // one raised goes.
        let relaunch_word = kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .relaunches
            .running()
            == Say::Clear;
        let park = Arc::new(AtomicBool::new(false));
        let look_at: Arc<Mutex<Option<Instant>>> = Arc::default();
        let acting: Arc<AtomicBool> = Arc::default();
        let stalled: Arc<AtomicBool> = Arc::default();
        let note: Note = Arc::default();
        let idle = Arc::new(WorkerIdle {
            sid: sid.clone(),
            agent: wanted[sid],
            grace: cfg.human_grace_s,
            park: Arc::clone(&park),
            look_at: Arc::clone(&look_at),
            acting: Arc::clone(&acting),
            stalled: Arc::clone(&stalled),
            switches: Arc::clone(&shared.switches),
            kept: Arc::clone(&kept),
            hooks: hooks.clone(),
            run: Mutex::default(),
            owns: AtomicBool::new(false),
            background_at: Mutex::default(),
            tasked: Mutex::default(),
            note: Arc::clone(&note),
            clock_hold: Mutex::default(),
        });
        let job = WorkerJob {
            sid: sid.clone(),
            agent: wanted[sid],
            opts: SuperviseOpts {
                idle_host: Some(idle),
                ..hosted_opts(cfg, sid, aterm_types::dirs::state_dir().as_deref())
            },
            stop: Arc::default(),
            interrupt: Arc::default(),
            handover: Arc::default(),
            clear_badge: relaunch_word,
            faults,
            park,
            look_at,
            left: Arc::default(),
            acting,
            stalled,
            switches: Arc::clone(&shared.switches),
            kept,
            note: Arc::clone(&note),
        };
        let (stop, interrupt) = (Arc::clone(&job.stop), Arc::clone(&job.interrupt));
        let handover = Arc::clone(&job.handover);
        let (park, look_at) = (Arc::clone(&job.park), Arc::clone(&job.look_at));
        let (left, acting) = (Arc::clone(&job.left), Arc::clone(&job.acting));
        let done = Arc::new(AtomicBool::new(false));
        if let Some(join) = spawn_worker(sid, job, Arc::clone(&done), hooks) {
            aterm_log::info!("harness @{sid}: supervising");
            host.workers.0.insert(
                sid.clone(),
                Worker {
                    join: Some(join),
                    done,
                    stop,
                    handover,
                    interrupt,
                    cfg_gen,
                    epoch,
                    park,
                    look_at,
                    left,
                    acting,
                    activations,
                    note,
                },
            );
        }
    }
    let mut live: Vec<String> = host.workers.0.keys().cloned().collect();
    live.sort();
    state.live = live;
    state.stops = host
        .workers
        .0
        .values()
        .map(|w| {
            (
                Arc::clone(&w.stop),
                Arc::clone(&w.left),
                Arc::clone(&w.interrupt),
            )
        })
        .collect();
}

/// A worker's options: [`SuperviseOpts::hosted_with`] the policy, with the
/// session's journal beside its approval ledger
/// ([`aterm_agent::supervise::approvals::journal_beside`]) — the lines only
/// the journal carries (`SKIPPED`, `WAITING`, `ESCALATED … attention=`),
/// readable for every supervised session (the live E2E's D5).
fn hosted_opts(cfg: &SupervisorConfig, sid: &str, state: Option<&Path>) -> SuperviseOpts {
    use aterm_agent::supervise::approvals;
    // The ledger and its journal, both under THIS instance's state root.
    let ledger = state.and_then(|root| approvals::path_under(root, Some(sid)));
    SuperviseOpts {
        journal: ledger.as_deref().map(approvals::journal_beside),
        ledger,
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

/// The supervised agent a session's publication says it is: its program BY
/// NAME ([`aterm_phase::program_of`], the one name table), else the agent
/// its last verdict's reader identified by the screen — a Claude Code
/// started as `node` (the laws review of 2026-09-24: the server published
/// its verdict and raised its rows while this host, keyed on the argv0 word
/// alone, never supervised it) — and only one the ONE predicate supervises
/// ([`aterm_phase::Program::supervisable`]).
pub(crate) fn agent_of(publication: &crate::session_timeline::AgentPublication) -> Option<Program> {
    publication
        .program
        .as_deref()
        .and_then(aterm_phase::program_of)
        .or(publication.reader)
        .filter(|p| p.supervisable())
}

/// Every live session whose server-published program is an agent, read
/// in-process: the store's handles, then each one's timeline.
fn roster_of(store: &Store) -> Vec<(String, Program, FollowStamp)> {
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
            let publication = tl.agent();
            let agent = agent_of(publication)?;
            Some((
                h.sid.as_str().to_string(),
                agent,
                FollowStamp {
                    group: publication.program_pgid,
                    reader: publication.reader,
                    rev: publication.rev,
                },
            ))
        })
        .collect()
}

/// The process group `sid`'s program is published for (`None`: no such
/// session, or none resolved yet).
fn foreground_of(store: &Store, sid: &str) -> Option<i64> {
    let guard = store.read().unwrap_or_else(PoisonError::into_inner);
    let h = guard.by_sid(&aterm_session::SessionId::new(sid))?;
    let pgid = h
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .agent()
        .program_pgid;
    (pgid > 0).then_some(i64::from(pgid))
}

/// Whether `sid` is still open (not closed, not exited).
fn open(store: &Store, sid: &str) -> bool {
    let guard = store.read().unwrap_or_else(PoisonError::into_inner);
    guard
        .by_sid(&aterm_session::SessionId::new(sid))
        .is_some_and(|h| h.state != SessionState::Exited)
}

/// Whether `sid` is still a live session of an agent the host supervises.
fn still_supervisable(store: &Store, sid: &str) -> bool {
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
    agent_of(tl.agent()).is_some()
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
    body_end(session.run_hosted(&job.opts, Arc::clone(&job.stop), &mut out))
}

/// What one run of the loop came to: a stop, or the reason it ended (held
/// behind another claim, or failed). The host's steps at idle points are
/// taken inside the run ([`WorkerIdle`]), so no end of a run is one.
fn body_end(result: Result<(), String>) -> BodyEnd {
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

/// The worker's acts over this instance's own socket: the upgrade's and the
/// relaunch's steps for one tab, under the harness state directory and the
/// user's home (Claude's records live under it). No home or no state
/// directory: no upgrade is ever due and no relaunch can be made (said).
fn live_acts(store: Store, sock: String) -> Acts {
    let home = aterm_primer::home_dir();
    let place = home.clone().zip(harness_state());
    let exit_home = home.clone();
    let fg_store = store.clone();
    let opts = move |sid: &str, grace: u32| {
        place.clone().map(|(home, state)| upgrade_drive::Opts {
            home,
            state,
            sock: Some(sock.clone()),
            only_sid: Some(sid.to_string()),
            dry_run: false,
            human_grace_s: grace,
            // The worker's loop takes every relaunched agent, and types its
            // continuation at its first idle point.
            hand_back: true,
            background: false,
            // Where each worker's loop ledgers what it types
            // (`approvals::default_path`): its turns are no task.
            aterm_state: aterm_agent::operator::default_state_root().ok(),
        })
    };
    let opts = Arc::new(opts);
    let (o1, o2, o3, o4, o5) = (
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
    );
    let (o6, o7, o8, o9, o10, o11, o12, o13) = (
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        opts,
    );
    Acts {
        open: Arc::new(move |sid| open(&store, sid)),
        due: Arc::new(move |sid| o1(sid, 0).map_or(Due::No, |o| upgrade_drive::due(&o))),
        behind: Arc::new(move |sid, since| {
            o11(sid, 0).map_or(Behind::Nothing, |o| upgrade_drive::note_behind(&o, since))
        }),
        step: Arc::new(move |sid, grace| {
            let Some(o) = o2(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = upgrade_drive::step(&o);
            if r.is_act() {
                aterm_log::info!("harness @{sid}: {}", r.line());
            }
            r.step
        }),
        notice: Arc::new(move |sid, grace| {
            let Some(o) = o9(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = upgrade_drive::step(&upgrade_drive::Opts {
                background: true,
                ..o
            });
            if r.is_act() {
                aterm_log::info!("harness @{sid}: {}", r.line_as("upgrade-at-break"));
            }
            r.step
        }),
        owed: Arc::new(move |sid| o6(sid, 0).is_some_and(|o| relaunch::owed(&o))),
        resume: Arc::new(move |sid, grace| {
            let Some(o) = o7(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::resume(&o);
            if r.is_act() {
                aterm_log::info!("harness @{sid}: {}", r.line_as("carry-on"));
            }
            r.step
        }),
        snapshot: Arc::new(move |sid| o3(sid, 0).and_then(|o| relaunch::snapshot(&o).ok())),
        follow: Arc::new(move |sid, snap| {
            // An unknown group (none resolved yet) proves nothing: kept.
            let Some(fg) = foreground_of(&fg_store, sid) else {
                return Foreground::Shell;
            };
            let at = relaunch::foreground(snap, fg);
            if at == Foreground::Agent
                && let Some(home) = home.as_deref()
            {
                relaunch::follow(home, snap);
            }
            at
        }),
        status: Arc::new(move |sid| {
            let sock = o4(sid, 0).and_then(|o| o.sock)?;
            let mut ctl = RelayCtl::new(Endpoint::Socket(sock), None);
            let reply = ctl.call(&[&format!("@{sid}"), "status"]).ok()?;
            reply
                .stdout
                .lines()
                .find(|l| l.starts_with("OK"))
                .map(str::to_string)
        }),
        exit_look: Arc::new(move |_, snap| {
            exit_home
                .as_deref()
                .map(|home| relaunch::look_at_exit(home, snap))
        }),
        relaunch: Arc::new(move |sid, grace, snap, left, upgrade, stalled| {
            let Some(o) = o5(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::after_exit(&o, snap, left, upgrade, stalled);
            aterm_log::info!("harness @{sid}: {}", r.line_as("relaunch"));
            r.step
        }),
        relaunch_restored: Arc::new(move |sid, snap, upgrade| {
            let Some(o) = o13(sid, 0) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::after_host_ended(&o, snap, upgrade);
            aterm_log::info!("harness @{sid}: {}", r.line_as("relaunch-after-host-ended"));
            r.step
        }),
        restart: Arc::new(move |sid, grace, why| {
            let Some(o) = o8(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::restart_here(&o, why);
            aterm_log::info!(
                "harness @{sid}: {}",
                r.line_as(&format!("restart:{}", why.word()))
            );
            r.step
        }),
        tasked: Arc::new(move |sid, session, memo| {
            let o = o10(sid, 0)?;
            memo.of(&o.home, session, &upgrade_drive::supervisor_typed(&o, sid))
        }),
        hold: Arc::new(move |sid, until| {
            o12(sid, 0).is_none_or(|o| upgrade_drive::hold_clock(&o, until))
        }),
    }
}

/// THE DISK WATCH over the store ([`Hooks::disk`]): each look takes the
/// supervised sessions' working directories (what their shells last reported,
/// OSC 7) and, off the host thread and one look at a time, hands the build
/// directories found there to `aterm_agent::harness::cli::disk_tick` — which,
/// below `[disk] auto_free_gib` of free space on the home volume, removes the
/// ones ON THAT VOLUME stale by both their clocks and laid by a build tool
/// (one on another volume would free nothing there), and journals each
/// removal with its witness in `<harness state>/disk.jsonl`. Above the floor
/// it reads the config and the free figure and nothing else.
fn live_disk(store: Store) -> Arc<DiskFn> {
    static LOOKING: AtomicBool = AtomicBool::new(false);
    Arc::new(move |sids: &[String]| {
        let cwds = working_dirs(&store, sids);
        if LOOKING.swap(true, Ordering::SeqCst) {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}disk"))
            .spawn(move || {
                disk_look(&cwds);
                LOOKING.store(false, Ordering::SeqCst);
            });
        if let Err(e) = spawned {
            LOOKING.store(false, Ordering::SeqCst);
            aterm_log::warn!("harness disk watch could not start: {e}");
        }
    })
}

/// The working directories `sids` last reported, each once.
fn working_dirs(store: &Store, sids: &[String]) -> Vec<std::path::PathBuf> {
    use crate::cwd_native::ReportedCwd as _;
    let handles: Vec<_> = {
        let g = store.read().unwrap_or_else(PoisonError::into_inner);
        sids.iter()
            .filter_map(|sid| g.by_sid(&aterm_session::SessionId::new(sid)).cloned())
            .collect()
    };
    let mut dirs: Vec<std::path::PathBuf> = handles
        .iter()
        .filter_map(|h| {
            let t = h.term.try_lock().ok()?;
            t.native_working_directory()
                .filter(|c| !c.is_empty())
                .map(|c| std::path::PathBuf::from(c.into_owned()))
        })
        .filter(|p| p.is_absolute())
        .collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/// The build directories directly under `dir`: its `target`, `target-<x>` and
/// `target.<x>` (`target.noindex`) children that are real directories (a link is not
/// followed: removing through one would take the link and leave the tree).
/// Whether each is a stale build directory is `disk_tick`'s witness to find.
fn build_dirs_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<std::path::PathBuf> = read
        .flatten()
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                n == "target" || n.starts_with("target-") || n.starts_with("target.")
            })
        })
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// One look of the disk watch over `cwds` (off the host thread).
fn disk_look(cwds: &[std::path::PathBuf]) {
    let (Some(home), Some(state)) = (aterm_primer::home_dir(), harness_state()) else {
        return;
    };
    let config =
        aterm_agent::harness::cli::disk_config_at(crate::app_config::config_path().as_deref());
    let free = atpkg::freespace::available_bytes(&home);
    if !config.below_auto_floor(free) {
        return;
    }
    let targets: Vec<std::path::PathBuf> = cwds.iter().flat_map(|d| build_dirs_under(d)).collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let transcripts = home.join(".claude").join("projects");
    let look = aterm_agent::harness::cli::DiskLook {
        state: &state,
        volume: &home,
        free,
        targets: &targets,
        transcripts: Some(&transcripts),
        now,
    };
    let tick = aterm_agent::harness::cli::disk_tick(
        &look,
        config,
        &mut aterm_agent::harness::disk::remove_tree,
    );
    if let aterm_agent::harness::cli::DiskTick::Reclaimed(done) = tick
        && !done.removed.is_empty()
    {
        aterm_log::info!(
            "harness disk watch: below {} GiB free, removed {} stale build director{} ({}); \
             each row is in {}",
            config.auto_free_gib,
            done.removed.len(),
            if done.removed.len() == 1 { "y" } else { "ies" },
            aterm_agent::harness::disk::human_bytes(done.freed_bytes),
            state.join(aterm_agent::harness::cli::DISK_LEDGER).display(),
        );
    }
}

/// The harness state directory the upgrade and the relaunch file their
/// records under (`<aterm state>/harness/<HARNESS>`), or `None` without one.
/// The owner's word from the window is written under it too
/// (`upgrade_host::live_word_writer`).
pub(crate) fn harness_state() -> Option<std::path::PathBuf> {
    aterm_agent::operator::default_state_root()
        .ok()
        .map(|root| {
            root.join("harness")
                .join(aterm_agent::harness::cli::HARNESS)
        })
}

/// THE OWNER'S VIEW over this instance's own socket ([`Hooks::upgrade_view`],
/// [`upgrade_drive::View`]): its rows handed to the app as
/// `Wake::AgentUpgrade` through `proxy` — a headless instance's too, whose
/// loop applies them the same way: each tab's `upgrade=` on `status` and
/// `sessions`, the records and the stalled rows in its message log (D6 of the
/// live E2E of 2026-09-26: headless, the column read `-` for every tab the
/// upgrade was moving, and `aterm help harness` made no headless exception).
/// No home or no state directory: nothing is ever recorded, so nothing is
/// shown.
fn live_view(sock: String, proxy: winit::event_loop::EventLoopProxy<crate::Wake>) -> Arc<ViewFn> {
    let place = aterm_primer::home_dir().zip(harness_state());
    let view = Mutex::new(upgrade_drive::View::default());
    Arc::new(move |look| {
        let mut view = view.lock().unwrap_or_else(PoisonError::into_inner);
        let mut hand = |rows: &[upgrade_drive::Row]| {
            let _ = proxy.send_event(crate::Wake::AgentUpgrade {
                rows: rows.to_vec(),
            });
        };
        if !look {
            view.stand_down(&mut hand);
            return None;
        }
        let (home, state) = place.clone()?;
        view.refresh(
            &upgrade_drive::Opts {
                home,
                state,
                sock: Some(sock.clone()),
                only_sid: None,
                dry_run: false,
                human_grace_s: 0,
                hand_back: true,
                background: false,
                aterm_state: None,
            },
            &mut hand,
        )
    })
}

/// How long the activation wake parks before it re-reads where atpkg's
/// notice lives (a Settings edit may move the package prefix). A newer build
/// wakes it at once, and so does the host's shutdown. Off macOS there is no
/// directory watch and this is how often the workers look — a known poll.
const WAKE_FALLBACK: Duration = Duration::from_secs(600);

/// Park on the pushes that say a newer Claude Code is installed — atpkg's
/// activation notice, and the native install's link repointed by Claude's
/// own updater ([`aterm_agent::harness::upgrade_wake`]) — and hand each one
/// to the host ([`HostHandle::note_activation`]), until the host shuts down
/// (its trigger ends the wait at once). One kqueue, no timer of its own but
/// the fallback.
fn spawn_activation_wake(host: HostHandle) {
    let home = aterm_primer::home_dir();
    let mut wake = ActivationWake::new(home.as_deref())
        .with_word(harness_state().map(|state| upgrade_drive::word_marker(&state)));
    let _ = host.shared.wake.set(wake.trigger());
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}activation"))
        .spawn(move || {
            while !host.shared.lock().shutting_down {
                if wake.wait(WAKE_FALLBACK) && !host.shared.lock().shutting_down {
                    host.note_activation();
                }
            }
        });
    if let Err(e) = spawned {
        aterm_log::warn!(
            "harness: the activation wake could not start: {e}; sessions are looked at for an \
             upgrade only when their supervisor attaches"
        );
    }
}

/// The production host: this process's store, its own control socket,
/// atpkg's activation notice and the owner's word, and the app loop
/// (`proxy`, headless or not) the owner's view of the upgrades is handed to.
pub(crate) fn start_default(
    store: Store,
    sock: String,
    cfg: SupervisorConfig,
    headless: bool,
    suspended: bool,
    proxy: winit::event_loop::EventLoopProxy<crate::Wake>,
) -> HostHandle {
    let (s1, s2, s3, s4) = (store.clone(), store.clone(), store.clone(), store);
    let (k1, k2, k3, k4) = (sock.clone(), sock.clone(), sock.clone(), sock);
    let host = HostHandle::start(
        cfg,
        headless,
        suspended,
        Hooks {
            roster: Arc::new(move || roster_of(&s1)),
            still_wanted: Arc::new(move |sid| still_supervisable(&s2, sid)),
            body: Arc::new(move |job| hosted_body(&k1, job)),
            badge: Arc::new(move |sid, text| set_badge(&k2, sid, text)),
            claim_epoch: Arc::new(|| CLAIM_EPOCH.load(Ordering::SeqCst)),
            acts: live_acts(s3, k3),
            backoff: Arc::new(restart_backoff),
            pause: Arc::new(|pause| pause),
            upgrade_view: live_view(k4, proxy),
            disk: live_disk(s4),
        },
    );
    spawn_activation_wake(host.clone());
    host
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
    use aterm_agent::harness::upgrade::SessionFile;
    use std::collections::HashSet;
    use std::sync::atomic::AtomicUsize;

    /// A single phase wake reads only its tab's Claude record. The same two
    /// workers remain the negative controls for generic and coalesced wakes:
    /// both records are read, including one whose stamp did not move.
    #[test]
    fn phase_follow_reads_only_the_changed_workers_record() {
        let world = Arc::new(World::default());
        let mut hooks = hooks(&world, parking_body(&world));
        let reads = Arc::new(Mutex::new(Vec::<String>::new()));
        let counted = Arc::clone(&reads);
        hooks.acts.follow = Arc::new(move |sid, _| {
            counted.lock().unwrap().push(sid.to_string());
            Foreground::Agent
        });
        let following: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|sid| {
                (
                    sid.to_string(),
                    Arc::new(Mutex::new(Kept {
                        snapshot: Some(snap(sid)),
                        ..Kept::default()
                    })),
                )
            })
            .collect();
        let stamp = FollowStamp {
            group: 12,
            reader: Some(Program::Claude),
            rev: 1,
        };
        let mut stamps = HashMap::from([("a".to_string(), stamp), ("b".to_string(), stamp)]);
        let mut followed = HashMap::new();
        follow_entries(&following, &stamps, &mut followed, true, &hooks);
        assert_eq!(*reads.lock().unwrap(), ["a", "b"]);

        reads.lock().unwrap().clear();
        stamps.get_mut("a").unwrap().rev += 1;
        let phase_only = full_follow_due(false, 8, 8, 4, 5);
        assert!(!phase_only, "one phase wake");
        follow_entries(&following, &stamps, &mut followed, phase_only, &hooks);
        assert_eq!(*reads.lock().unwrap(), ["a"]);

        reads.lock().unwrap().clear();
        let generic_and_phase = full_follow_due(false, 8, 9, 4, 5);
        assert!(generic_and_phase);
        follow_entries(
            &following,
            &stamps,
            &mut followed,
            generic_and_phase,
            &hooks,
        );
        assert_eq!(*reads.lock().unwrap(), ["a", "b"]);

        reads.lock().unwrap().clear();
        let coalesced = full_follow_due(false, 8, 8, 4, 6);
        assert!(coalesced);
        follow_entries(&following, &stamps, &mut followed, coalesced, &hooks);
        assert_eq!(*reads.lock().unwrap(), ["a", "b"]);

        reads.lock().unwrap().clear();
        stamps.remove("b");
        follow_entries(&following, &stamps, &mut followed, false, &hooks);
        assert_eq!(*reads.lock().unwrap(), ["b"], "unknown stamp falls back");
        assert!(full_follow_due(false, 8, 8, u64::MAX, 0), "wrap");
        assert!(full_follow_due(true, 8, 8, 4, 4), "timer");
    }

    /// The host's own acts are gated by the window's parsed `[harness]`
    /// table — the master switch and each act's key — not by a second reader
    /// of the file, and live: neither key restarts a worker ([`effective`]
    /// masks both). NEGATIVE CONTROL: each key limits only its own act.
    #[test]
    fn the_host_switches_are_the_parsed_table() {
        let read = |cfg: &SupervisorConfig| {
            let s = Switches::default();
            s.set(cfg);
            (s.upgrade(), s.relaunch())
        };
        assert_eq!(read(&on()), (true, true));
        assert_eq!(read(&off()), (false, false));
        let mut no_upgrade = on();
        no_upgrade.set("upgrade", "false").unwrap();
        assert_eq!(read(&no_upgrade), (false, true));
        let mut no_relaunch = on();
        no_relaunch.set("relaunch", "false").unwrap();
        assert_eq!(read(&no_relaunch), (true, false));
        assert_eq!(effective(&no_upgrade), on());
        assert_eq!(effective(&no_relaunch), on());
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
            Some(Program::Claude)
        );
        assert_eq!(
            agent_of(&publication(Some("claude-code"), None)),
            Some(Program::Claude)
        );
        assert_eq!(
            agent_of(&publication(Some("codex"), None)),
            Some(Program::Codex)
        );
        assert_eq!(
            agent_of(&publication(Some("node"), Some(Program::Claude))),
            Some(Program::Claude),
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
            Some(Program::Codex)
        );
    }

    /// A fake roster the test moves, plus the calls the seams saw.
    #[derive(Default)]
    struct World {
        roster: Mutex<Vec<(String, Program)>>,
        runs: AtomicUsize,
        badges: Mutex<Vec<(String, Option<String>)>>,
        /// This world's own claim epoch: the process-wide one moves with
        /// every other test's claim release.
        released: AtomicU64,
    }

    impl World {
        fn set(&self, roster: &[(&str, Program)]) {
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
            roster: Arc::new(move || {
                w1.roster
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(sid, program)| (sid.clone(), *program, FollowStamp::default()))
                    .collect()
            }),
            still_wanted: Arc::new(move |sid| {
                w2.roster
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(s, a)| s == sid && *a == Program::Claude)
            }),
            body,
            badge: Arc::new(move |sid, text| {
                w3.badges
                    .lock()
                    .unwrap()
                    .push((sid.to_string(), text.map(str::to_string)));
            }),
            claim_epoch: Arc::new(move || w4.released.load(Ordering::SeqCst)),
            acts: Acts::inert(),
            backoff: Arc::new(quick_backoff),
            pause: Arc::new(quick_pause),
            upgrade_view: Arc::new(|_| None),
            disk: Arc::new(|_| {}),
        }
    }

    impl Acts {
        /// Acts over no real session: every tab closed, no upgrade due, no
        /// snapshot, nobody typing, and a step or relaunch that answers
        /// nothing it could act on.
        fn inert() -> Self {
            Acts {
                open: Arc::new(|_| false),
                due: Arc::new(|_| Due::No),
                behind: Arc::new(|_, _| Behind::Nothing),
                step: Arc::new(|_, _| "current".to_string()),
                notice: Arc::new(|_, _| "current".to_string()),
                owed: Arc::new(|_| false),
                resume: Arc::new(|_, _| "current".to_string()),
                snapshot: Arc::new(|_| None),
                follow: Arc::new(|_, _| Foreground::Shell),
                status: Arc::new(|_| None),
                exit_look: Arc::new(|_, _| None),
                relaunch: Arc::new(|_, _, _, _, _, _| "refused:inert".to_string()),
                relaunch_restored: Arc::new(|_, _, _| "refused:inert".to_string()),
                restart: Arc::new(|_, _, _| "refused:inert".to_string()),
                tasked: Arc::new(|_, _, _| None),
                hold: Arc::new(|_, _| true),
            }
        }
    }

    /// The restart schedule the tests run under: [`restart_backoff`]'s shape
    /// — growing, capped at the fifth — in tens of milliseconds, so a
    /// session's whole budget is spent within a test.
    fn quick_backoff(n: usize) -> Duration {
        const STEPS: [u64; 5] = [10, 20, 30, 40, 50];
        Duration::from_millis(STEPS[n.saturating_sub(1).min(STEPS.len() - 1)])
    }

    /// The upgrade's and the relaunch's pauses as the tests wait them: a
    /// thousandth of each (the 10 s relaunch back-off, 10 ms).
    fn quick_pause(pause: Duration) -> Duration {
        pause / 1000
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

    /// THE RESTORED TABS' AGENTS (2026-09-27): each `(tab, snapshot)` a cold
    /// restore of the live layout hands the host is relaunched there by the
    /// host's own act, off the caller's thread, once for a relaunch that
    /// lands. NEGATIVE CONTROLS: `[harness] relaunch = false` relaunches
    /// nothing, and neither does a headless instance.
    #[test]
    fn a_restored_tabs_agent_is_relaunched_while_relaunch_is_on() {
        let snap = |tab: &str| Snapshot {
            tab: tab.to_string(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".to_string(),
            shell: 4343,
            program: std::path::PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".to_string()),
            cwd: "/".to_string(),
            version: None,
        };
        let run = |cfg: SupervisorConfig, headless: bool| {
            let world = Arc::new(World::default());
            let seen: Arc<Mutex<Vec<String>>> = Arc::default();
            let mut h = hooks(&world, parking_body(&world));
            let s = Arc::clone(&seen);
            h.acts.relaunch_restored = Arc::new(move |sid, snap, _| {
                assert_eq!(snap.tab, sid);
                s.lock().unwrap().push(sid.to_string());
                "adopted".to_string()
            });
            let host = HostHandle::start(cfg, headless, false, h);
            host.relaunch_restored(vec![
                ("s-a".to_string(), snap("s-a")),
                ("s-b".to_string(), snap("s-b")),
            ]);
            (host, seen)
        };
        let (_host, seen) = run(on(), false);
        until("both relaunched", || seen.lock().unwrap().len() == 2);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(*seen.lock().unwrap(), ["s-a", "s-b"], "once each");

        let mut limited = on();
        limited.set("relaunch", "false").unwrap();
        let (_host, seen) = run(limited, false);
        std::thread::sleep(Duration::from_millis(100));
        assert!(seen.lock().unwrap().is_empty(), "relaunch = false");
        let (_host, seen) = run(on(), true);
        std::thread::sleep(Duration::from_millis(100));
        assert!(seen.lock().unwrap().is_empty(), "headless");
    }

    #[test]
    fn config_off_runs_no_host_and_no_workers() {
        let world = Arc::new(World::default());
        world.set(&[("s-a", Program::Claude)]);
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

    /// THE DISK WATCH'S ONE TIMER ([`look_at_disk`]): nothing is handed
    /// over before its instant, then the supervised sessions once, and the
    /// next look is a [`DISK_TICK`] on. NEGATIVE CONTROLS: with supervision
    /// off, or no agent session hosted, nothing is looked at and no instant
    /// is named (the host parks on its bell alone).
    #[test]
    fn the_disk_watch_looks_once_per_tick_over_the_supervised_sessions() {
        let world = Arc::new(World::default());
        let looks: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let l = Arc::clone(&looks);
        let hooks = Hooks {
            disk: Arc::new(move |sids| l.lock().unwrap().push(sids.to_vec())),
            ..hooks(&world, parking_body(&world))
        };
        let t0 = Instant::now();
        let mut at = t0 + DISK_FIRST_LOOK;
        let two: HashMap<String, Program> = [
            ("s-b".to_string(), Program::Codex),
            ("s-a".to_string(), Program::Claude),
        ]
        .into();

        assert_eq!(look_at_disk(&mut at, t0, true, &two, &hooks), Some(at));
        assert!(looks.lock().unwrap().is_empty(), "not before its instant");
        let due = t0 + DISK_FIRST_LOOK;
        assert_eq!(
            look_at_disk(&mut at, due, true, &two, &hooks),
            Some(due + DISK_TICK)
        );
        assert_eq!(
            *looks.lock().unwrap(),
            [vec!["s-a".to_string(), "s-b".to_string()]]
        );
        assert_eq!(
            look_at_disk(&mut at, due + Duration::from_secs(60), true, &two, &hooks),
            Some(due + DISK_TICK),
            "one look per tick"
        );
        assert_eq!(looks.lock().unwrap().len(), 1);

        let late = due + DISK_TICK * 2;
        assert_eq!(look_at_disk(&mut at, late, false, &two, &hooks), None);
        assert_eq!(
            look_at_disk(&mut at, late, true, &HashMap::new(), &hooks),
            None
        );
        assert_eq!(looks.lock().unwrap().len(), 1, "off, or nothing hosted");
    }

    /// Where the disk watch looks under an agent's working directory: its
    /// build-directory children that are real directories. A file, a link (removing
    /// through it would take the link and leave the tree) and another name
    /// are not candidates; nor is a directory the watch cannot read. Unix-pinned:
    /// the link case needs `std::os::unix::fs::symlink`.
    #[cfg(unix)]
    #[test]
    fn the_disk_watch_takes_the_real_target_directories_under_a_cwd() {
        let dir = std::env::temp_dir().join(format!("aterm-disk-under-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in [
            "target",
            "target-tippy",
            "target.noindex",
            "src",
            "targetless/x",
        ] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("target-notes.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("target"), dir.join("target-link")).unwrap();
        let found: Vec<String> = build_dirs_under(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            found,
            ["target", "target-tippy", "target.noindex"],
            "every real `target`, `target-<x>` and `target.<x>` directory, and nothing else"
        );
        assert!(build_dirs_under(&dir.join("absent")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE OWNER'S VIEW IS LOOKED AT WHEN SOMETHING IT SHOWS MAY HAVE MOVED
    /// ([`look_at_upgrades`]), and at the instant it names — never on a timer:
    /// the first look, a worker's act, an activation notice or the owner's
    /// word, a changed roster, the named instant; stood down ONCE when the
    /// policy or `[harness] upgrade` is off, and looked at afresh when it is
    /// back. NEGATIVE CONTROL: looking again with nothing moved and no instant
    /// named looks at nothing, and names no wait.
    #[test]
    fn the_owners_view_is_looked_at_only_when_something_it_shows_may_have_moved() {
        let world = Arc::new(World::default());
        let looks: Arc<Mutex<Vec<bool>>> = Arc::default();
        let next: Arc<Mutex<Option<u64>>> = Arc::default();
        let (l, n) = (Arc::clone(&looks), Arc::clone(&next));
        let hooks = Hooks {
            upgrade_view: Arc::new(move |look| {
                l.lock().unwrap().push(look);
                *n.lock().unwrap()
            }),
            ..hooks(&world, parking_body(&world))
        };
        let taken = || std::mem::take(&mut *looks.lock().unwrap());
        let mut view = ViewLook::default();
        let one: HashMap<String, Program> = [("s-a".to_string(), Program::Claude)].into();
        let two: HashMap<String, Program> = [
            ("s-a".to_string(), Program::Claude),
            ("s-b".to_string(), Program::Claude),
        ]
        .into();
        assert_eq!(
            look_at_upgrades(&mut view, &one, true, (0, 0), &hooks),
            None
        );
        assert_eq!(taken(), [true], "the first look");
        assert_eq!(
            look_at_upgrades(&mut view, &one, true, (0, 0), &hooks),
            None
        );
        assert!(taken().is_empty(), "nothing moved: no look");
        let _ = look_at_upgrades(&mut view, &one, true, (1, 0), &hooks);
        assert_eq!(taken(), [true], "a worker acted");
        let _ = look_at_upgrades(&mut view, &one, true, (1, 1), &hooks);
        assert_eq!(taken(), [true], "an activation, or the owner's word");
        let _ = look_at_upgrades(&mut view, &two, true, (1, 1), &hooks);
        assert_eq!(taken(), [true], "the roster changed");
        // The view names an instant: the host looks again then.
        *next.lock().unwrap() = Some(crate::upgrade_host::now_s());
        let _ = look_at_upgrades(&mut view, &one, true, (1, 1), &hooks);
        assert_eq!(taken(), [true]);
        *next.lock().unwrap() = None;
        assert!(
            look_at_upgrades(&mut view, &one, true, (1, 1), &hooks).is_none(),
            "looked at again at its instant, and named none after"
        );
        assert_eq!(taken(), [true]);
        // Off: stood down once; back on: looked at afresh.
        let _ = look_at_upgrades(&mut view, &one, false, (1, 1), &hooks);
        let _ = look_at_upgrades(&mut view, &one, false, (2, 2), &hooks);
        assert_eq!(taken(), [false], "stood down once");
        let _ = look_at_upgrades(&mut view, &one, true, (2, 2), &hooks);
        assert_eq!(taken(), [true], "back on");
    }

    /// A headless instance supervises what it hosts by default (the owner's
    /// rule: every session aterm hosts), and `[harness] headless = false`
    /// takes that away. NEGATIVE CONTROL: the same limit in a windowed
    /// instance changes nothing.
    #[test]
    fn a_headless_instance_runs_a_host_unless_the_policy_limits_it() {
        let world = Arc::new(World::default());
        world.set(&[("s-h", Program::Claude)]);
        let mut limited = on();
        limited.set("headless", "false").unwrap();
        let host = HostHandle::start(
            limited.clone(),
            true,
            false,
            hooks(&world, parking_body(&world)),
        );
        assert!(
            !host.is_running(),
            "headless under [harness] headless = false"
        );
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), 0);
        let host2 = HostHandle::start(on(), true, false, hooks(&world, parking_body(&world)));
        assert!(host2.is_running(), "headless by default");
        until("a worker for s-h", || host2.live() == ["s-h"]);
        host2.shutdown_and_join();
        host.shutdown_and_join();
        let windowed =
            HostHandle::start(limited, false, false, hooks(&world, parking_body(&world)));
        assert!(windowed.is_running());
        windowed.shutdown_and_join();
    }

    /// Workers follow the published program, for every agent the one
    /// predicate supervises (aterm-phase's `Program::supervisable`: Claude
    /// Code and Codex, both measured) — a Codex session gets its worker as a
    /// Claude Code session does.
    #[test]
    fn workers_follow_the_published_program_and_the_supervisable_predicate() {
        assert!(
            aterm_phase::Program::Claude.supervisable()
                && aterm_phase::Program::Codex.supervisable()
        );
        let world = Arc::new(World::default());
        world.set(&[("s-c", Program::Claude), ("s-x", Program::Codex)]);
        let host = HostHandle::start(on(), false, false, hooks(&world, parking_body(&world)));
        until("a worker for each agent", || host.live() == ["s-c", "s-x"]);
        // The program left: the worker is stopped (through its interrupter).
        world.set(&[]);
        until("detached", || host.live().is_empty());
        // A reaped worker's body has returned, so this count is final: one
        // run for each agent, no restart after the stop.
        assert_eq!(world.runs.load(Ordering::SeqCst), 2);
        world.set(&[("s-c", Program::Claude)]);
        // `live` moves when the host SPAWNS a worker; the body counts its
        // run when that thread is first scheduled, which a loaded machine
        // does later. The attach is a fresh run of the loop, so wait for the
        // run, not only the thread.
        until("attached again", || {
            host.live() == ["s-c"] && world.runs.load(Ordering::SeqCst) >= 3
        });
        // …and no further run follows it.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), 3);
        host.shutdown_and_join();
    }

    /// The relaunch on exit is built for Claude Code: another supervised
    /// agent that leaves its tab unasked is not relaunched — said once on its
    /// attention as a capability not built yet (never as a `[harness]` limit,
    /// the philosophy review of 2026-09-25) — and nothing of a relaunch is
    /// read for it. The UPGRADE is built for Codex too — the Codex branch of
    /// the same step — so a Codex with one due is parked for its idle point,
    /// and one with none is not. NEGATIVE CONTROL: the same exit of a Claude
    /// Code is relaunched.
    #[test]
    fn only_a_claude_code_is_relaunched_and_a_codex_is_upgraded_too() {
        let a = Arc::new(Acting::default());
        a.due.store(true, Ordering::SeqCst);
        let hooks = a.hooks();
        let job = |agent: Program| WorkerJob {
            sid: "s-k".to_string(),
            agent,
            opts: SuperviseOpts::hosted(),
            stop: Arc::new(AtomicBool::new(true)),
            interrupt: Arc::default(),
            handover: Arc::default(),
            clear_badge: false,
            faults: Arc::default(),
            park: Arc::default(),
            look_at: Arc::default(),
            left: Arc::new(AtomicBool::new(true)),
            acting: Arc::default(),
            stalled: Arc::default(),
            switches: {
                let s = Arc::new(Switches::default());
                s.set(&on());
                s
            },
            kept: Arc::default(),
            note: Arc::default(),
        };
        let codex = job(Program::Codex);
        attach(&codex, &hooks);
        assert!(
            codex.park.load(Ordering::SeqCst),
            "due: parked for its step"
        );
        assert!(with_kept(&codex, |k| k.snapshot.is_none()), "nothing read");
        // NEGATIVE CONTROLS: a Codex with no upgrade due is not parked for,
        // and neither is an agent the upgrade is not written for.
        a.due.store(false, Ordering::SeqCst);
        let idle = job(Program::Codex);
        attach(&idle, &hooks);
        assert!(!idle.park.load(Ordering::SeqCst), "nothing due: not parked");
        a.due.store(true, Ordering::SeqCst);
        let other = job(Program::Generic);
        attach(&other, &hooks);
        assert!(!other.park.load(Ordering::SeqCst), "not written for it");
        on_agent_left(&codex, &hooks);
        assert!(a.relaunched.lock().unwrap().is_empty());
        // Not relaunched, and said — as a capability not built yet, never as
        // a [harness] limit.
        let badges = a.badges.lock().unwrap().clone();
        assert!(
            matches!(&badges[..], [Some(b)] if b.contains("not built for this agent yet (no [harness] limit)")),
            "{badges:?}"
        );
        let claude = job(Program::Claude);
        attach(&claude, &hooks);
        assert!(claude.park.load(Ordering::SeqCst), "due: parked for");
        on_agent_left(&claude, &hooks);
        assert_eq!(*a.relaunched.lock().unwrap(), ["s-k"]);
    }

    #[test]
    fn a_reload_restarts_workers_and_switching_off_stops_them() {
        let world = Arc::new(World::default());
        world.set(&[("s-r", Program::Claude)]);
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
        world.set(&[("s-i", Program::Claude)]);
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

    /// THE APPROVAL LEVEL reaches the engine (owner, 2026-09-24): `approve`
    /// survives [`effective`] into the worker's `SuperviseOpts::hosted_with`,
    /// and changing it — the Settings row `harness.approve`, saved — restarts
    /// the worker under the new policy, which is what makes the row live.
    /// `approve = "safe"` is decision 1's proven rules, still answering what
    /// they prove, never the approvals off. NEGATIVE CONTROL: re-saving the
    /// same policy restarts nothing; clearing it back to full power restarts
    /// once more.
    #[test]
    fn the_approve_level_reaches_the_engine_and_changing_it_restarts_the_worker() {
        use aterm_agent::supervise::config::Approve;
        let world = Arc::new(World::default());
        world.set(&[("s-y", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, hooks(&world, parking_body(&world)));
        until("first worker", || world.runs.load(Ordering::SeqCst) == 1);
        let opts = SuperviseOpts::hosted_with(&effective(&on()));
        assert_eq!(opts.policy.approve, Approve::All, "the default answers all");

        let mut safe = on();
        safe.set("approve", "safe").unwrap();
        assert_eq!(effective(&safe), safe, "the engine reads approve");
        let opts = SuperviseOpts::hosted_with(&effective(&safe));
        assert_eq!(opts.policy.approve, Approve::Safe);
        host.set_config(safe.clone());
        until("restarted under the safe rules", || {
            world.runs.load(Ordering::SeqCst) == 2
        });
        // An unchanged policy restarts nothing…
        host.set_config(safe);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(world.runs.load(Ordering::SeqCst), 2);
        // …and back to full power restarts once more.
        host.set_config(on());
        until("restarted at full power", || {
            world.runs.load(Ordering::SeqCst) == 3
        });
        host.shutdown_and_join();
    }

    /// THE PHILOSOPHY REVIEW OF 2026-09-25 (major): past RESTART_BUDGET
    /// failures in an hour the session's supervisor was turned OFF —
    /// "faulted until [harness] changes or the agent restarts" — a give-up
    /// that waited on a person. A panicking loop is now restarted FOR EVER:
    /// within the budget on the short pauses, past it on pauses that grow to
    /// an hour, the session badged while it waits (information: the badge
    /// names the failure and the next restart) and cleared as the next run
    /// starts. NEGATIVE CONTROL: within the budget, no badge at all.
    #[test]
    fn a_panicking_worker_is_restarted_past_its_budget_and_never_turned_off() {
        let world = Arc::new(World::default());
        world.set(&[("s-p", Program::Claude)]);
        let w = Arc::clone(&world);
        let body: Arc<BodyFn> = Arc::new(move |_job: &WorkerJob| {
            w.runs.fetch_add(1, Ordering::SeqCst);
            panic!("boom in the loop");
        });
        let host = HostHandle::start(on(), false, false, hooks(&world, body));
        until("restarted past the budget", || {
            world.runs.load(Ordering::SeqCst) >= RESTART_BUDGET + 3
        });
        assert_eq!(host.live(), ["s-p"], "never turned off");
        let badges = world.badges.lock().unwrap().clone();
        let raised: Vec<&str> = badges.iter().filter_map(|(_, t)| t.as_deref()).collect();
        assert!(!raised.is_empty(), "{badges:?}");
        assert!(
            raised
                .iter()
                .all(|t| t.starts_with("supervisor keeps failing (")
                    && t.contains("restarting in")
                    && t.contains("boom in the loop")
                    && t.len() <= 200),
            "{raised:?}"
        );
        assert!(
            badges.iter().any(|(_, t)| t.is_none()),
            "cleared as the next run starts: {badges:?}"
        );
        // Each raise is cleared as the run it waited for starts: never two up.
        let mut up = 0i32;
        for (_, t) in &badges {
            up += if t.is_some() { 1 } else { -1 };
            assert!((0..=1).contains(&up), "{badges:?}");
        }
        host.shutdown_and_join();

        // NEGATIVE CONTROL: failures within the budget raise nothing.
        let world = Arc::new(World::default());
        world.set(&[("s-q", Program::Claude)]);
        let w = Arc::clone(&world);
        let body: Arc<BodyFn> = Arc::new(move |_job: &WorkerJob| {
            let n = w.runs.fetch_add(1, Ordering::SeqCst);
            if n < RESTART_BUDGET {
                panic!("boom");
            }
            while !_job.stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
            BodyEnd::Stopped
        });
        let host = HostHandle::start(on(), false, false, hooks(&world, body));
        until("running after its restarts", || {
            world.runs.load(Ordering::SeqCst) == RESTART_BUDGET + 1
        });
        std::thread::sleep(Duration::from_millis(30));
        assert!(world.badges.lock().unwrap().is_empty());
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
        assert_eq!(restart_backoff(RESTART_BUDGET), Duration::from_secs(60));
        assert!(restart_backoff(RESTART_BUDGET + 1) > restart_backoff(RESTART_BUDGET));
        assert_eq!(restart_backoff(99), Duration::from_secs(3600));
        let world = Arc::new(World::default());
        world.set(&[("s-b", Program::Claude)]);
        let w = Arc::clone(&world);
        let started = Instant::now();
        let body: Arc<BodyFn> = Arc::new(move |_job: &WorkerJob| {
            w.runs.fetch_add(1, Ordering::SeqCst);
            BodyEnd::Failed("transient".to_string())
        });
        let host = HostHandle::start(on(), false, false, hooks(&world, body));
        until("past the budget: badged", || {
            !world.badges.lock().unwrap().is_empty()
        });
        let spent = started.elapsed();
        let floor: Duration = (1..=RESTART_BUDGET).map(quick_backoff).sum();
        assert!(spent >= floor, "{spent:?} < {floor:?}");
        host.shutdown_and_join();

        // A stop during a backoff ends the worker without waiting it out.
        // The backoff here is an hour, so only the stop can end the worker
        // within `until`'s bound. The first failure enters the session's
        // history before the wait begins, and the wait reads the stop flag
        // before it parks, so a stop that lands in between is seen too. The
        // tests' 10 to 50 ms schedule raced the test thread instead: one held
        // past their 150 ms sum by a loaded machine let a sixth run fault the
        // session, and a wait no stop could cut would have passed.
        let world = Arc::new(World::default());
        world.set(&[("s-c", Program::Claude)]);
        let w = Arc::clone(&world);
        let body: Arc<BodyFn> = Arc::new(move |_job: &WorkerJob| {
            w.runs.fetch_add(1, Ordering::SeqCst);
            BodyEnd::Failed("transient".to_string())
        });
        let mut h = hooks(&world, body);
        h.backoff = Arc::new(|_| Duration::from_secs(3600));
        let host = HostHandle::start(on(), false, false, h);
        until("in the first backoff", || host.faults_of("s-c") >= 1);
        world.set(&[]);
        until("worker gone", || host.live().is_empty());
        assert_eq!(
            world.runs.load(Ordering::SeqCst),
            1,
            "the stop ended the first backoff; no restart ran"
        );
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
        world.set(&[("s-h", Program::Claude)]);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut h = hooks(&world, parking_body(&world));
        let (w, c) = (Arc::clone(&world), Arc::clone(&calls));
        h.roster = Arc::new(move || {
            assert!(
                c.fetch_add(1, Ordering::SeqCst) > 0,
                "the first roster read panics"
            );
            w.roster
                .lock()
                .unwrap()
                .iter()
                .map(|(sid, program)| (sid.clone(), *program, FollowStamp::default()))
                .collect()
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
        world.set(&[("s-held", Program::Claude)]);
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
        world.set(&[("s-s", Program::Claude)]);
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
        world.set(&[("s-g", Program::Claude)]);
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
            w.roster
                .lock()
                .unwrap()
                .iter()
                .map(|(sid, program)| (sid.clone(), *program, FollowStamp::default()))
                .collect()
        });
        let (g, w) = (Arc::clone(&gate), Arc::clone(&world));
        // parking_body, with an interrupter that also counts its cuts, and a
        // run counted once that interrupter is published.
        hooks.body = Arc::new(move |job: &WorkerJob| {
            let g = Arc::clone(&g);
            let me = std::thread::current();
            *job.interrupt.lock().unwrap() = Some(Box::new(move || {
                g.cuts.fetch_add(1, Ordering::SeqCst);
                me.unpark();
            }));
            w.runs.fetch_add(1, Ordering::SeqCst);
            while !job.stop.load(Ordering::SeqCst) {
                std::thread::park();
            }
            BodyEnd::Stopped
        });
        let host = HostHandle::start(on(), false, false, hooks);
        // `live` moves at the spawn; only a published interrupter can be
        // cut. A worker not yet scheduled has none, and suspend's flag alone
        // stops it (the body reads the flag after publishing), so the count
        // below would be 0 for a reason that is not the defect.
        until("a worker for s-g, its interrupter published", || {
            host.live() == ["s-g"] && world.runs.load(Ordering::SeqCst) >= 1
        });
        assert_eq!(world.runs.load(Ordering::SeqCst), 1);
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
    /// A run that ends Ok is a stop ([`body_end`]): the host's steps at idle
    /// points are taken inside the run ([`WorkerIdle`]), so none ends it.
    #[test]
    fn a_run_ends_stopped_held_or_failed() {
        assert_eq!(body_end(Ok(())), BodyEnd::Stopped);
        assert_eq!(
            body_end(Err(format!("{CLAIM_HELD}other"))),
            BodyEnd::Held("other".to_string())
        );
    }

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
    // The worker's acts beyond the loop: the upgrade at a park, the relaunch
    // on an exit.
    // -----------------------------------------------------------------------

    /// A world whose loop takes the host's step at an idle point whenever
    /// the host asks for one (the engine's contract: `IdleHost::at_idle` at
    /// the next authoritative idle point — here at once, as for a session
    /// that is already idle — and the loop runs on), and whose acts record
    /// what the worker asked of them.
    #[derive(Default)]
    struct Acting {
        roster: Mutex<Vec<(String, Program)>>,
        /// Sessions still open whatever their program.
        open: Mutex<HashSet<String>>,
        runs: AtomicUsize,
        parks: AtomicUsize,
        due: AtomicBool,
        /// What the upgrade reads of the session ([`Acts::due`]), in turn;
        /// once spent, [`Self::due`] says.
        due_says: Mutex<VecDeque<Due>>,
        /// The step each idle point's look journaled (`HOST <line>`).
        idle_said: Mutex<Vec<String>>,
        /// The times the session was noted behind ([`Acts::behind`]).
        behinds: AtomicUsize,
        /// The second each note was asked with, and the steps taken before
        /// it.
        behind_since: Mutex<Vec<(u64, usize)>>,
        /// The loop reaches no idle point: the agent's first turn runs.
        busy: AtomicBool,
        /// What each note answers, in turn; `Noted` once spent.
        behind_says: Mutex<VecDeque<Behind>>,
        /// The steps the upgrade answers, in turn; `current` once spent.
        steps: Mutex<VecDeque<&'static str>>,
        stepped: AtomicUsize,
        /// What the relaunch answers, in turn; `done` once spent.
        relaunches: Mutex<VecDeque<&'static str>>,
        relaunched: Mutex<Vec<String>>,
        /// The `[harness] upgrade` switch each relaunch was asked under.
        upgrading: Mutex<Vec<bool>>,
        human_ms: Mutex<Option<u64>>,
        /// The session reads `hold=1`.
        held: AtomicBool,
        badges: Mutex<Vec<Option<String>>>,
        /// Run inside a step (the agent the step ends and relaunches).
        during_step: Mutex<Option<Box<dyn Fn() + Send>>>,
        /// The agent is gone: nothing about it can be read any more.
        gone: AtomicBool,
        /// Where the tab's foreground job stands against the kept snapshot
        /// (`None`: its agent still holds it).
        fg: Mutex<Option<Foreground>>,
        /// The conversation Claude's own record names now, when it moved.
        conversation: Mutex<Option<String>>,
        /// The conversation each relaunch was asked to resume.
        resumed: Mutex<Vec<Option<String>>>,
        /// A relaunched agent is owed its continuation (an `adopted` relaunch
        /// sets it, a `done` carry-on clears it — the state file's part).
        owed: AtomicBool,
        /// What the carry-on answers, in turn; `done` once spent.
        carry: Mutex<VecDeque<&'static str>>,
        carried: AtomicUsize,
        /// When `Some`, whether the upgrade owned the turn ends after each
        /// step taken at an idle point.
        owns_seen: Mutex<Option<Vec<bool>>>,
        /// The loop asks its host for a restart in place once (the memory
        /// banner at a point), and what it answered.
        ask_restart: AtomicBool,
        restart_said: Mutex<Vec<Option<String>>>,
        /// What each restart answers, in turn; `adopted` once spent.
        restart_steps: Mutex<VecDeque<&'static str>>,
        /// The restarts the host made, by their word.
        restarts: Mutex<Vec<&'static str>>,
        /// The loop holds a stall ([`IdleHost::stalled`]) before each look.
        stall_held: AtomicBool,
        /// Whether each relaunch on exit was asked as the stall's remedy's.
        stalled_exits: Mutex<Vec<bool>>,
        /// The loop offers breaks of the agent's own background work
        /// ([`IdleHost::at_background`]) instead of idle points.
        at_break: AtomicBool,
        /// What the notice at a break answers, in turn; `wait:background`
        /// once spent.
        notices: Mutex<VecDeque<&'static str>>,
        noticed: AtomicUsize,
        /// What the host said at each break it acted at.
        break_said: Mutex<Vec<String>>,
        /// What the conversation's record says of its task (`None`: nobody
        /// can say), and each conversation it was asked about.
        tasked: Mutex<Option<bool>>,
        tasked_asked: Mutex<Vec<String>>,
        /// What the worker's loop last read of [`IdleHost::taskless`].
        taskless_seen: Mutex<Option<bool>>,
        /// The loop says a turn ran ([`IdleHost::turn_ran`]) after every
        /// step it takes: the session was busy between the looks.
        turns_between: AtomicBool,
        /// Every pause the host was asked to wait out, as named.
        pauses: Mutex<Vec<Duration>>,
        /// Claude's own record of the agent that left is gone: removed by its
        /// exit, or by another Claude Code's start.
        record_gone: AtomicBool,
        /// The exit is GRACEFUL: it removes its own record in its last
        /// moments — here after the first look at it.
        graceful: AtomicBool,
        /// Another Claude Code starts during the relaunch's back-off and
        /// removes the dead agent's record.
        sweep_in_backoff: AtomicBool,
        /// The looks taken at exits.
        exit_looks: AtomicUsize,
        /// What the exit left, as each relaunch was handed it.
        exit_records: Mutex<Vec<&'static str>>,
    }

    fn snap(sid: &str) -> Snapshot {
        Snapshot {
            tab: sid.to_string(),
            pid: 4242,
            start: "Thu Sep 24 01:02:03 2026".to_string(),
            shell: 4241,
            program: std::path::PathBuf::from("/opt/claude"),
            argv: vec!["/opt/claude".to_string()],
            session: Some("0badf00d-1111-2222-3333-444455556666".to_string()),
            cwd: "/".to_string(),
            version: Some("2.1.281".to_string()),
        }
    }

    impl Acting {
        fn set(&self, roster: &[(&str, Program)]) {
            *self.roster.lock().unwrap() =
                roster.iter().map(|(s, a)| ((*s).to_string(), *a)).collect();
            ring();
        }

        fn hooks(self: &Arc<Self>) -> Hooks {
            let w = |w: &Arc<Self>| Arc::clone(w);
            let (w1, w2, w3, w4) = (w(self), w(self), w(self), w(self));
            let (a1, a2, a3, a4, a5, a6) = (w(self), w(self), w(self), w(self), w(self), w(self));
            let (a7, a8, a9, a10, a11) = (w(self), w(self), w(self), w(self), w(self));
            let (a12, a13, a14, a15) = (w(self), w(self), w(self), w(self));
            Hooks {
                roster: Arc::new(move || {
                    w1.roster
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(sid, program)| (sid.clone(), *program, FollowStamp::default()))
                        .collect()
                }),
                still_wanted: Arc::new(move |sid| {
                    w2.roster.lock().unwrap().iter().any(|(s, _)| s == sid)
                }),
                body: Arc::new(move |job: &WorkerJob| {
                    w3.runs.fetch_add(1, Ordering::SeqCst);
                    loop {
                        if job.stop.load(Ordering::SeqCst) {
                            return BodyEnd::Stopped;
                        }
                        if let Some(host) = job.opts.idle_host.as_ref() {
                            host.stalled(w3.stall_held.load(Ordering::SeqCst));
                            *w3.taskless_seen.lock().unwrap() = Some(host.taskless());
                        }
                        if let Some(host) = job.opts.idle_host.as_ref()
                            && w3.ask_restart.swap(false, Ordering::SeqCst)
                        {
                            let said = host.restart(&Restart::Memory);
                            w3.restart_said.lock().unwrap().push(said);
                        }
                        if let Some(host) = job.opts.idle_host.as_ref().filter(|h| h.wants())
                            && w3.at_break.load(Ordering::SeqCst)
                        {
                            if let Some(line) = host.at_background() {
                                w3.break_said.lock().unwrap().push(line);
                                if let Some(owns) = w3.owns_seen.lock().unwrap().as_mut() {
                                    owns.push(host.owns_turn_end());
                                }
                            }
                        } else if let Some(host) = job
                            .opts
                            .idle_host
                            .as_ref()
                            .filter(|h| h.wants() && !w3.busy.load(Ordering::SeqCst))
                        {
                            w3.parks.fetch_add(1, Ordering::SeqCst);
                            if let Some(said) = host.at_idle() {
                                w3.idle_said.lock().unwrap().push(said.line);
                            }
                            if w3.turns_between.load(Ordering::SeqCst) {
                                host.turn_ran();
                            }
                            if let Some(owns) = w3.owns_seen.lock().unwrap().as_mut() {
                                owns.push(host.owns_turn_end());
                            }
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }),
                badge: Arc::new(move |_, text| {
                    w4.badges.lock().unwrap().push(text.map(str::to_string));
                }),
                claim_epoch: Arc::new(|| 0),
                acts: Acts {
                    open: Arc::new(move |sid| a1.open.lock().unwrap().contains(sid)),
                    due: Arc::new(move |_| {
                        a2.due_says.lock().unwrap().pop_front().unwrap_or(
                            if a2.due.load(Ordering::SeqCst) {
                                Due::Yes
                            } else {
                                Due::No
                            },
                        )
                    }),
                    behind: Arc::new(move |_, since| {
                        a15.behinds.fetch_add(1, Ordering::SeqCst);
                        a15.behind_since
                            .lock()
                            .unwrap()
                            .push((since, a15.stepped.load(Ordering::SeqCst)));
                        a15.behind_says
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or(Behind::Noted)
                    }),
                    owed: Arc::new(move |_| a8.owed.load(Ordering::SeqCst)),
                    resume: Arc::new(move |_, _| {
                        a9.carried.fetch_add(1, Ordering::SeqCst);
                        let step = a9.carry.lock().unwrap().pop_front().unwrap_or("done");
                        if step.starts_with("done") {
                            a9.owed.store(false, Ordering::SeqCst);
                        }
                        step.to_string()
                    }),
                    notice: Arc::new(move |_, _| {
                        a11.noticed.fetch_add(1, Ordering::SeqCst);
                        a11.notices
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or("wait:background")
                            .to_string()
                    }),
                    step: Arc::new(move |_, _| {
                        a3.stepped.fetch_add(1, Ordering::SeqCst);
                        if let Some(during) = a3.during_step.lock().unwrap().as_ref() {
                            during();
                        }
                        a3.steps
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or("current")
                            .to_string()
                    }),
                    snapshot: Arc::new(move |sid| {
                        (!a6.gone.load(Ordering::SeqCst)).then(|| snap(sid))
                    }),
                    follow: Arc::new(move |_, snap| {
                        let fg = a7.fg.lock().unwrap().unwrap_or(Foreground::Agent);
                        if fg == Foreground::Agent
                            && let Some(c) = a7.conversation.lock().unwrap().clone()
                        {
                            snap.session = Some(c);
                        }
                        fg
                    }),
                    status: Arc::new(move |_| {
                        let human = a4
                            .human_ms
                            .lock()
                            .unwrap()
                            .map_or("-".to_string(), |ms| ms.to_string());
                        let hold = u8::from(a4.held.load(Ordering::SeqCst));
                        Some(format!(
                            "OK program=claude hold={hold} hand=- human_ms={human}"
                        ))
                    }),
                    exit_look: Arc::new(move |_, snap| {
                        let look = a12.exit_looks.fetch_add(1, Ordering::SeqCst);
                        if look > 0 && a12.graceful.load(Ordering::SeqCst) {
                            a12.record_gone.store(true, Ordering::SeqCst);
                        }
                        let record =
                            (!a12.record_gone.load(Ordering::SeqCst)).then(|| SessionFile {
                                pid: snap.pid,
                                session_id: snap.session.clone().unwrap_or_default(),
                                cwd: snap.cwd.clone(),
                                version: "2.1.283".to_string(),
                                status: "busy".to_string(),
                                status_updated_at_ms: 1,
                                proc_start: snap.start.clone(),
                                kind: "interactive".to_string(),
                                entrypoint: "cli".to_string(),
                            });
                        Some(ExitLook {
                            running: false,
                            record,
                        })
                    }),
                    relaunch_restored: Arc::new(|_, _, _| "refused:inert".to_string()),
                    relaunch: Arc::new(move |sid, _, snap, left, upgrade, stalled| {
                        assert_eq!(snap.tab, sid, "the snapshot of the session that left");
                        a5.exit_records.lock().unwrap().push(left.word());
                        // `after_exit`'s decision on what the exit left: a
                        // record read at the exit, else the record now.
                        let survived = match left {
                            ExitRecord::Survived(_) => true,
                            ExitRecord::Removed => false,
                            ExitRecord::Unread => !a5.record_gone.load(Ordering::SeqCst),
                        };
                        if !survived && !stalled {
                            return "ended:graceful-exit".to_string();
                        }
                        a5.relaunched.lock().unwrap().push(sid.to_string());
                        a5.stalled_exits.lock().unwrap().push(stalled);
                        a5.upgrading.lock().unwrap().push(upgrade);
                        a5.resumed.lock().unwrap().push(snap.session.clone());
                        let step = a5
                            .relaunches
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or("adopted");
                        if step == "adopted" {
                            a5.owed.store(true, Ordering::SeqCst);
                        }
                        step.to_string()
                    }),
                    restart: Arc::new(move |sid, _, why| {
                        a10.restarts.lock().unwrap().push(why.word());
                        // The agent the restart ends leaves the tab and
                        // comes back during it: the step's act, no exit.
                        a10.set(&[]);
                        std::thread::sleep(Duration::from_millis(40));
                        a10.set(&[(sid, Program::Claude)]);
                        let step = a10
                            .restart_steps
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or("adopted");
                        if step == "adopted" {
                            a10.owed.store(true, Ordering::SeqCst);
                        }
                        step.to_string()
                    }),
                    tasked: Arc::new(move |_, session, _| {
                        let mut asked = a14.tasked_asked.lock().unwrap();
                        if !asked.iter().any(|c| c == session) {
                            asked.push(session.to_string());
                        }
                        *a14.tasked.lock().unwrap()
                    }),
                    hold: Arc::new(|_, _| true),
                },
                backoff: Arc::new(quick_backoff),
                // Every pause is recorded as named. The relaunch's back-off
                // (a second or more; the look at an exit waits in steps of
                // 25 ms) is when another Claude Code may start and remove the
                // dead agent's record.
                pause: Arc::new(move |pause| {
                    a13.pauses.lock().unwrap().push(pause);
                    if pause >= relaunch::BACKOFF[0] && a13.sweep_in_backoff.load(Ordering::SeqCst)
                    {
                        a13.record_gone.store(true, Ordering::SeqCst);
                    }
                    quick_pause(pause)
                }),
                upgrade_view: Arc::new(|_| None),
                disk: Arc::new(|_| {}),
            }
        }
    }

    /// THE LOOP'S LIMIT EPISODE, IN THE WINDOW'S OWN WIRING
    /// ([`WorkerIdle::limited`], [`WorkerIdle::hold_clock`]; the second
    /// review of 2026-09-26: every test host stubbed `hold` to accept, so none
    /// of this had run). An episode's open clears the upgrade's ownership of
    /// the session's turn ends at once and stamps the clock hold; a lock
    /// another sweep holds REFUSES the stamp, which is kept and applied FIRST
    /// at the next idle point — the same second, before that point's step;
    /// the episode's close stamps again. NEGATIVE CONTROL: a stamp taken at
    /// once is not offered again at the next idle point, and a host that
    /// upgrades nothing stamps nothing.
    #[test]
    fn a_limit_episode_clears_ownership_and_keeps_its_stamp_past_a_held_lock() {
        let world = Arc::new(World::default());
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let refuse = Arc::new(AtomicBool::new(true));
        let (l1, l2, r1) = (Arc::clone(&log), Arc::clone(&log), Arc::clone(&refuse));
        let mut hooks = hooks(&world, parking_body(&world));
        hooks.acts = Acts {
            due: Arc::new(|_| Due::Yes),
            step: Arc::new(move |_, _| {
                l1.lock().unwrap().push("step".to_string());
                "announced:1".to_string()
            }),
            hold: Arc::new(move |sid, until| {
                l2.lock().unwrap().push(format!("hold {sid} {until}"));
                // Refused once: another sweep holds the lock.
                !r1.swap(false, Ordering::SeqCst)
            }),
            ..Acts::inert()
        };
        let switches = Arc::new(Switches::default());
        switches.set(&on());
        let host = |agent: Program| WorkerIdle {
            sid: "s-lim".to_string(),
            agent,
            grace: 0,
            park: Arc::default(),
            look_at: Arc::default(),
            acting: Arc::default(),
            stalled: Arc::default(),
            switches: Arc::clone(&switches),
            kept: Arc::default(),
            hooks: hooks.clone(),
            run: Mutex::default(),
            owns: AtomicBool::new(false),
            background_at: Mutex::default(),
            tasked: Mutex::default(),
            note: Arc::default(),
            clock_hold: Mutex::default(),
        };
        let taken = || std::mem::take(&mut *log.lock().unwrap());
        let idle = host(Program::Claude);
        assert_eq!(
            idle.at_idle().map(|s| s.line).as_deref(),
            Some("upgrade step=announced:1")
        );
        assert!(idle.owns_turn_end(), "the notice owns the turn ends");
        assert_eq!(taken(), ["step"], "no stamp, no hold");

        // The episode opens: ownership cleared at once, the stamp refused.
        idle.limited(true);
        assert!(!idle.owns_turn_end(), "the limit is the loop's to wait out");
        let opened = taken();
        assert_eq!(opened.len(), 1, "{opened:?}");
        assert!(opened[0].starts_with("hold s-lim "), "{opened:?}");
        assert!(
            idle.clock_hold.lock().unwrap().is_some(),
            "kept for the next try"
        );

        // The next idle point applies the SAME stamp, before its step.
        idle.at_idle();
        assert_eq!(taken(), [opened[0].clone(), "step".to_string()]);
        assert!(idle.clock_hold.lock().unwrap().is_none(), "applied");

        // The episode closes: stamped again, taken at once — and not offered
        // again at the next idle point.
        idle.limited(false);
        let closed = taken();
        assert_eq!(closed.len(), 1, "{closed:?}");
        assert!(closed[0].starts_with("hold s-lim "), "{closed:?}");
        idle.at_idle();
        assert_eq!(taken(), ["step"]);

        // A host that upgrades nothing (a generic program) stamps nothing.
        let other = host(Program::Generic);
        other.limited(true);
        assert!(taken().is_empty());
        assert!(other.clock_hold.lock().unwrap().is_none());
    }

    /// THE UPGRADE IS A STEP OF THE WORKER, TAKEN IN ITS LOOP: an activation
    /// notice asks the loop for its next idle point; there the worker takes
    /// ONE step; an announcement asks again for the next idle point (the
    /// agent's answer), and the last word leaves the loop running — the SAME
    /// run throughout, never ended and started again (the elegance review of
    /// 2026-09-25). NEGATIVE CONTROLS: before the notice nothing steps; a
    /// notice with nothing due steps nothing; and with `[harness] upgrade =
    /// false` a notice reaches the idle point but no step is taken — without
    /// restarting the worker.
    #[test]
    fn an_upgrade_notice_parks_the_loop_and_the_step_is_taken_there() {
        let a = Arc::new(Acting::default());
        a.set(&[("s-u", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || a.runs.load(Ordering::SeqCst) == 1);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(
            a.parks.load(Ordering::SeqCst),
            0,
            "a complete record and nothing due: the loop is not parked"
        );
        host.note_activation();
        until("a park for the notice", || {
            a.parks.load(Ordering::SeqCst) == 1
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(a.stepped.load(Ordering::SeqCst), 0, "nothing due: no step");
        *a.steps.lock().unwrap() = ["announced:1", "done"].into();
        a.due.store(true, Ordering::SeqCst);
        host.note_activation();
        until("announced, then parked again and done", || {
            a.stepped.load(Ordering::SeqCst) == 2
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(a.stepped.load(Ordering::SeqCst), 2, "the last word ends it");
        assert_eq!(a.runs.load(Ordering::SeqCst), 1, "one run throughout");
        // The switch off: the notice parks, and no step is taken.
        let mut no_upgrade = on();
        no_upgrade.set("upgrade", "false").unwrap();
        host.set_config(no_upgrade);
        let parks = a.parks.load(Ordering::SeqCst);
        host.note_activation();
        until("parked", || a.parks.load(Ordering::SeqCst) > parks);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(a.stepped.load(Ordering::SeqCst), 2);
        assert_eq!(host.live(), ["s-u"], "the switch restarted nothing");
        host.shutdown_and_join();
    }

    /// N3 OF THE LIVE RE-TEST OF 2026-09-26: A SESSION BEHIND AS ITS WORKER
    /// ATTACHES IS BEHIND FROM THEN. The worker notes it ([`Acts::behind`]:
    /// its upgrade state minted with its age from now) at attach, before
    /// its loop's first idle point — which, after a busy first turn, came
    /// minutes later, and the column read `-` until it. Claude Code and
    /// Codex alike (the Codex worker returned before it until the review of
    /// the N3 fix). NEGATIVE CONTROLS: nothing due, and `[harness] upgrade =
    /// false`, note nothing.
    #[test]
    fn a_session_behind_at_attach_is_noted_behind_then() {
        for (agent, due, upgrade, want) in [
            (Program::Claude, true, true, 1),
            (Program::Codex, true, true, 1),
            (Program::Claude, false, true, 0),
            (Program::Claude, true, false, 0),
            (Program::Codex, true, false, 0),
        ] {
            let a = Arc::new(Acting::default());
            a.set(&[("s-n", agent)]);
            a.due.store(due, Ordering::SeqCst);
            let mut cfg = on();
            if !upgrade {
                cfg.set("upgrade", "false").unwrap();
            }
            let host = HostHandle::start(cfg, false, false, a.hooks());
            until("attached", || a.runs.load(Ordering::SeqCst) == 1);
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(
                a.behinds.load(Ordering::SeqCst),
                want,
                "{agent:?}, due {due}, upgrade {upgrade}"
            );
            host.shutdown_and_join();
        }
    }

    /// N3 AS THE RE-TEST OF 155c72a28 FOUND IT: A FRESH LAUNCH. The worker
    /// attaches before Claude Code has written its record (there 1.2 s
    /// before it), so the upgrade cannot be read yet, and the agent's first
    /// turn runs — no idle point for a minute or more. The note behind is
    /// owed from the attach and asked again until the record reads: noted
    /// BEFORE the first idle point, with the attach's second — every ask
    /// carries it — and the upgrade's step is taken at that idle point. (The
    /// first cut asked once, at the attach, only when due read `true`; its
    /// host test stubbed that `true`, and live the column read `-` until the
    /// first step.)
    #[test]
    fn a_fresh_launch_is_behind_from_its_attach_once_its_record_reads() {
        for agent in [Program::Claude, Program::Codex] {
            let a = Arc::new(Acting::default());
            a.busy.store(true, Ordering::SeqCst);
            *a.due_says.lock().unwrap() = [Due::Unread(upgrade_drive::UNREAD_RECORD)].into();
            a.due.store(true, Ordering::SeqCst);
            *a.behind_says.lock().unwrap() = [
                Behind::Unread(upgrade_drive::UNREAD_RECORD),
                Behind::Unread(upgrade_drive::UNREAD_RECORD),
            ]
            .into();
            *a.steps.lock().unwrap() = ["announced:1"].into();
            let before = crate::upgrade_host::now_s();
            a.set(&[("s-f", agent)]);
            let host = HostHandle::start(on(), false, false, a.hooks());
            until("noted, the record read", || {
                a.behinds.load(Ordering::SeqCst) == 3
            });
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(a.behinds.load(Ordering::SeqCst), 3, "{agent:?}: decided");
            assert_eq!(
                a.parks.load(Ordering::SeqCst),
                0,
                "{agent:?}: before the first idle point"
            );
            let asked = a.behind_since.lock().unwrap().clone();
            let since = asked[0].0;
            assert!(
                (before..=crate::upgrade_host::now_s()).contains(&since),
                "{agent:?}: the attach's second"
            );
            assert!(
                asked.iter().all(|&(s, stepped)| s == since && stepped == 0),
                "{agent:?}: {asked:?}"
            );
            // The first turn ends: the idle point takes the upgrade's step.
            a.busy.store(false, Ordering::SeqCst);
            until("the step", || a.stepped.load(Ordering::SeqCst) >= 1);
            host.shutdown_and_join();
        }
    }

    /// N3 PAST THE HOST'S LADDER: a record still unread when the ladder is
    /// spent is asked at the loop's idle point, BEFORE its step, with the
    /// attach's second — so the step finds the state minted with its age from
    /// the attach, not its own. NEGATIVE CONTROL: a note that reads nothing
    /// to note is not asked again, and one with nothing due at the attach is
    /// never owed.
    #[test]
    fn a_note_the_ladder_could_not_read_is_asked_at_the_idle_point_before_the_step() {
        let a = Arc::new(Acting::default());
        a.busy.store(true, Ordering::SeqCst);
        *a.due_says.lock().unwrap() = [Due::Unread(upgrade_drive::UNREAD_SOCKET)].into();
        a.due.store(true, Ordering::SeqCst);
        *a.behind_says.lock().unwrap() =
            [Behind::Unread(upgrade_drive::UNREAD_SOCKET); 1 + NOTE_AGAIN.len()].into();
        a.set(&[("s-l", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("the ladder spent", || {
            a.behinds.load(Ordering::SeqCst) == 1 + NOTE_AGAIN.len()
        });
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(
            a.behinds.load(Ordering::SeqCst),
            1 + NOTE_AGAIN.len(),
            "not asked again by the host"
        );
        a.busy.store(false, Ordering::SeqCst);
        until("the step", || a.stepped.load(Ordering::SeqCst) >= 1);
        let asked = a.behind_since.lock().unwrap().clone();
        let (since, _) = asked[0];
        assert_eq!(
            asked[1 + NOTE_AGAIN.len()..],
            [(since, 0)],
            "asked once more at the idle point, before the step, with the attach's second"
        );
        host.shutdown_and_join();
        // NEGATIVE CONTROLS.
        for (at_attach, says, want) in [
            (
                Due::Unread(upgrade_drive::UNREAD_RECORD),
                Behind::Nothing,
                1,
            ),
            (Due::No, Behind::Noted, 0),
        ] {
            let a = Arc::new(Acting::default());
            a.busy.store(true, Ordering::SeqCst);
            *a.due_says.lock().unwrap() = [at_attach].into();
            *a.behind_says.lock().unwrap() = [says].into();
            a.set(&[("s-c", Program::Claude)]);
            let host = HostHandle::start(on(), false, false, a.hooks());
            until("attached", || a.runs.load(Ordering::SeqCst) == 1);
            std::thread::sleep(Duration::from_millis(80));
            assert_eq!(a.behinds.load(Ordering::SeqCst), want, "{at_attach:?}");
            host.shutdown_and_join();
        }
    }

    /// N3, MID-SESSION (the review of the N3 fix): A BUILD INSTALLED WHILE A
    /// SESSION RUNS makes it behind FROM THE ACTIVATION NOTICE — the common
    /// case, which the note at attach alone left reading `-` until the
    /// session's next idle point, a busy turn's length later. The host notes
    /// each worker's session behind at the notice ([`Acts::behind`]),
    /// Claude Code and Codex alike; a step holding the state's lock
    /// ([`Behind::Busy`]) is asked again after a pause, and a note that
    /// minted or found nothing to mint is not asked again. NEGATIVE
    /// CONTROLS: nothing is noted before the notice (nothing due at
    /// attach), and under `[harness] upgrade = false` the notice notes
    /// nothing.
    #[test]
    fn a_build_installed_mid_session_is_noted_behind_at_its_notice() {
        for agent in [Program::Claude, Program::Codex] {
            let a = Arc::new(Acting::default());
            a.set(&[("s-m", agent)]);
            *a.behind_says.lock().unwrap() = [Behind::Busy, Behind::Noted].into();
            let host = HostHandle::start(on(), false, false, a.hooks());
            until("attached", || a.runs.load(Ordering::SeqCst) == 1);
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(a.behinds.load(Ordering::SeqCst), 0, "{agent:?}");
            host.note_activation();
            until("noted, once the lock is let go", || {
                a.behinds.load(Ordering::SeqCst) == 2
            });
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(
                a.behinds.load(Ordering::SeqCst),
                2,
                "{agent:?}: noted, not asked again"
            );
            *a.behind_says.lock().unwrap() = [Behind::Nothing].into();
            host.note_activation();
            until("asked at the next notice", || {
                a.behinds.load(Ordering::SeqCst) == 3
            });
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(a.behinds.load(Ordering::SeqCst), 3, "{agent:?}");
            let mut no_upgrade = on();
            no_upgrade.set("upgrade", "false").unwrap();
            host.set_config(no_upgrade);
            let parks = a.parks.load(Ordering::SeqCst);
            host.note_activation();
            until("parked", || a.parks.load(Ordering::SeqCst) > parks);
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(a.behinds.load(Ordering::SeqCst), 3, "{agent:?}: off");
            host.shutdown_and_join();
        }
    }

    /// A LOOK THAT CANNOT READ THE SESSION IS A WAIT, NEVER "NOTHING TO TAKE"
    /// (the live re-test of 155c72a28, 2026-09-26: every control lane held,
    /// the looks' `who` was refused, `due` read `false`, and four of seven
    /// sessions were let go for good, nothing journaled). Read so at attach
    /// (a launch whose record is not written yet) the loop is parked all the
    /// same; read so at an idle point the look is journaled `upgrade
    /// step=wait:no-socket` and taken again on the unread ladder's first
    /// rung ([`UNREAD_LOOK`]) — and the step is taken there once the session
    /// reads due. Claude Code and Codex alike.
    #[test]
    fn a_look_that_cannot_read_the_session_looks_again_and_steps_once_it_can() {
        for agent in [Program::Claude, Program::Codex] {
            let a = Arc::new(Acting::default());
            *a.due_says.lock().unwrap() = [
                Due::Unread(upgrade_drive::UNREAD_RECORD),
                Due::Unread(upgrade_drive::UNREAD_SOCKET),
            ]
            .into();
            a.due.store(true, Ordering::SeqCst);
            *a.steps.lock().unwrap() = ["done"].into();
            a.set(&[("s-r", agent)]);
            let host = HostHandle::start(on(), false, false, a.hooks());
            // The line is journaled once `at_idle` returns, after its step.
            until("the step at the later look, journaled", || {
                a.idle_said.lock().unwrap().len() >= 2
            });
            assert_eq!(
                a.idle_said.lock().unwrap()[..2],
                ["upgrade step=wait:no-socket", "upgrade step=done"],
                "{agent:?}"
            );
            assert!(
                a.pauses.lock().unwrap().contains(&UNREAD_LOOK[0]),
                "{agent:?}: looked at again on the unread ladder's first rung"
            );
            host.shutdown_and_join();
        }
    }

    /// A SESSION THAT ANSWERED READY IS NEVER ABANDONED FOR A LOOK THAT
    /// COULD NOT READ IT (the same re-test: a tab left over eight minutes
    /// after its READY to "aterm will restart this Claude Code"). The look
    /// after the READY is refused: journaled, looked at again — the upgrade
    /// still owning the session's turn ends, since it read nothing to let
    /// them go — and the restart is taken at the next look.
    #[test]
    fn a_session_that_answered_ready_is_restarted_after_a_look_that_could_not_read_it() {
        let a = Arc::new(Acting::default());
        *a.due_says.lock().unwrap() = [
            Due::Yes,
            Due::Yes,
            Due::Unread(upgrade_drive::UNREAD_SOCKET),
            Due::Yes,
        ]
        .into();
        *a.steps.lock().unwrap() = ["announced:1", "adopted"].into();
        *a.owns_seen.lock().unwrap() = Some(Vec::new());
        a.set(&[("s-y", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("the restart, journaled", || {
            a.idle_said.lock().unwrap().len() >= 3
        });
        assert_eq!(
            a.idle_said.lock().unwrap()[..3],
            [
                "upgrade step=announced:1",
                "upgrade step=wait:no-socket",
                "upgrade step=adopted"
            ]
        );
        assert_eq!(
            a.owns_seen.lock().unwrap().as_deref().map(|o| &o[..2]),
            Some(&[true, true][..]),
            "the READY answer's turn ends stay the upgrade's through the unread look"
        );
        host.shutdown_and_join();
    }

    /// A LOOK WHOSE READ FAILED IS NO WAIT OF THE STEP'S (the review of the
    /// silent-drop fix): its own short ladder ([`UNREAD_LOOK`]: 5 s, 15 s,
    /// 30 s, a minute), never the step's ([`upgrade_drive::LATER`]: 20 s,
    /// 60 s, 300 s, 600 s — a 90 s lane outage left a READY'd session idle
    /// until +380 s), and none of the step's waits spent: those bound how
    /// long the upgrade owns a READY'd session's turn ends while its
    /// background work drains ([`upgrade_drive::OWNED_BACKGROUND_LOOKS`]), and four refused
    /// looks after the READY used them all up, so the drain after them owned
    /// nothing. NEGATIVE CONTROL: the step's own waits after them climb the
    /// step's ladder from its first rung, and still bound the hold.
    #[test]
    fn unread_looks_climb_their_own_short_ladder_and_spend_none_of_the_steps_waits() {
        let a = Arc::new(Acting::default());
        let unread = Due::Unread(upgrade_drive::UNREAD_SOCKET);
        *a.due_says.lock().unwrap() = [
            Due::Yes,
            Due::Yes,
            unread,
            unread,
            unread,
            unread,
            Due::Yes,
            Due::Yes,
            Due::Yes,
            Due::Yes,
        ]
        .into();
        *a.steps.lock().unwrap() = [
            "announced:1",
            "wait:background",
            "wait:background",
            "wait:background",
            "wait:background",
        ]
        .into();
        *a.owns_seen.lock().unwrap() = Some(Vec::new());
        a.set(&[("s-w", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("every step, its ownership read", || {
            a.owns_seen
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|o| o.len() >= 9)
        });
        let owns = a.owns_seen.lock().unwrap().clone().unwrap_or_default();
        assert_eq!(
            owns[..9],
            [true, true, true, true, true, true, true, true, false],
            "owned through the refused looks, and for OWNED_BACKGROUND_LOOKS drains after"
        );
        let pauses = a.pauses.lock().unwrap().clone();
        assert_eq!(
            pauses[..7],
            [
                UNREAD_LOOK[0],
                UNREAD_LOOK[1],
                UNREAD_LOOK[2],
                UNREAD_LOOK[3],
                upgrade_drive::LATER[0],
                upgrade_drive::LATER[1],
                upgrade_drive::LATER[2],
            ],
            "the unread looks on their own ladder; the step's waits from its first rung"
        );
        host.shutdown_and_join();
    }

    /// NEGATIVE CONTROL: a look that READS the session and finds nothing to
    /// take still lets the upgrade go — nothing journaled, nothing looked at
    /// again, no step.
    #[test]
    fn a_look_that_reads_nothing_to_take_still_lets_the_upgrade_go() {
        let a = Arc::new(Acting::default());
        *a.due_says.lock().unwrap() = [Due::Yes, Due::No].into();
        a.set(&[("s-z", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("the first idle point", || {
            a.parks.load(Ordering::SeqCst) == 1
        });
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(a.parks.load(Ordering::SeqCst), 1, "not looked at again");
        assert_eq!(a.stepped.load(Ordering::SeqCst), 0);
        assert!(a.idle_said.lock().unwrap().is_empty());
        assert!(a.pauses.lock().unwrap().is_empty(), "no later look");
        host.shutdown_and_join();
    }

    /// THE NOTICE AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK (the owner's
    /// answer of 2026-09-26: "Busy agentic sessions get upgraded at their
    /// next natural break. The notice interrupts the agent's orchestration
    /// once, and the restart still never kills running work"): a session
    /// whose loop offers only such breaks — its turn over, work it started
    /// still running — is told of its upgrade there: the notice alone (no
    /// step, nothing ended; a re-ask comes only after a whole `REASK_S`, the
    /// reducer's), the upgrade owning its turn ends after it,
    /// and the park left for the idle point, where the step goes on once the
    /// work is done. Claude Code and Codex alike. NEGATIVE CONTROLS: nothing
    /// due, the switch off, and a carry-on owed each ask for no notice; an
    /// agent the upgrade is not written for is never asked.
    #[test]
    fn a_break_of_the_agents_background_work_takes_the_notice_alone() {
        for agent in [Program::Claude, Program::Codex] {
            let a = Arc::new(Acting::default());
            a.due.store(true, Ordering::SeqCst);
            a.at_break.store(true, Ordering::SeqCst);
            *a.notices.lock().unwrap() = ["announced:1"].into();
            *a.owns_seen.lock().unwrap() = Some(Vec::new());
            *a.steps.lock().unwrap() = ["wait:not-ready"].into();
            a.set(&[("s-b", agent)]);
            let host = HostHandle::start(on(), false, false, a.hooks());
            until("the notice at the break", || {
                a.noticed.load(Ordering::SeqCst) == 1
            });
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(a.noticed.load(Ordering::SeqCst), 1, "{agent:?}: once");
            assert_eq!(
                *a.break_said.lock().unwrap(),
                ["upgrade step=announced:1"],
                "{agent:?}"
            );
            assert_eq!(
                a.owns_seen.lock().unwrap().clone(),
                Some(vec![true]),
                "{agent:?}: the notice's answer is the upgrade's"
            );
            assert_eq!(
                a.stepped.load(Ordering::SeqCst),
                0,
                "{agent:?}: nothing ended"
            );
            // The work done: the idle point takes the step the park kept.
            a.at_break.store(false, Ordering::SeqCst);
            until("the idle point's step", || {
                a.stepped.load(Ordering::SeqCst) == 1
            });
            assert_eq!(a.runs.load(Ordering::SeqCst), 1, "{agent:?}: one run");
            host.shutdown_and_join();
        }
        // NEGATIVE CONTROLS.
        let asked = |due: bool, upgrade: bool, owed: bool, agent: Program| {
            let a = Arc::new(Acting::default());
            a.due.store(due, Ordering::SeqCst);
            a.owed.store(owed, Ordering::SeqCst);
            a.at_break.store(true, Ordering::SeqCst);
            *a.notices.lock().unwrap() = ["announced:1"].into();
            *a.carry.lock().unwrap() = ["wait:held"; 64].into();
            a.set(&[("s-n", agent)]);
            let mut cfg = on();
            if !upgrade {
                cfg.set("upgrade", "false").unwrap();
            }
            let host = HostHandle::start(cfg, false, false, a.hooks());
            until("attached", || a.runs.load(Ordering::SeqCst) == 1);
            host.note_activation();
            std::thread::sleep(Duration::from_millis(80));
            let n = a.noticed.load(Ordering::SeqCst);
            host.shutdown_and_join();
            n
        };
        assert_eq!(asked(false, true, false, Program::Claude), 0, "nothing due");
        assert_eq!(
            asked(true, false, false, Program::Claude),
            0,
            "switched off"
        );
        assert_eq!(
            asked(true, true, true, Program::Codex),
            0,
            "a carry-on owed"
        );
        assert_eq!(
            asked(true, true, false, Program::Generic),
            0,
            "not written for it"
        );
    }

    /// A CODEX SESSION'S UPGRADE IS THE SAME STEP OF THE SAME WORKER: parked
    /// at attach for the Codex branch that is due, the step taken at the idle
    /// point (its `/exit` and relaunch — the Codex leaving the tab and coming
    /// back during the step, which is no exit), the relaunched TUI handed
    /// back (`adopted`) and its carry-on typed at the next idle point through
    /// the relaunch primitive's continuation step — one run throughout, and
    /// nothing said on the tab. NEGATIVE CONTROL: with nothing due, a Codex
    /// is never stepped.
    #[test]
    fn a_codex_sessions_worker_takes_the_codex_branch_and_its_carry_on_at_idle_points() {
        let a = Arc::new(Acting::default());
        a.open.lock().unwrap().insert("s-cx".to_string());
        a.due.store(true, Ordering::SeqCst);
        *a.steps.lock().unwrap() = ["adopted"].into();
        let flap = Arc::clone(&a);
        *a.during_step.lock().unwrap() = Some(Box::new(move || {
            // The typed `/exit` ends the TUI; `codex resume` brings it back.
            flap.set(&[]);
            std::thread::sleep(Duration::from_millis(40));
            flap.set(&[("s-cx", Program::Codex)]);
            flap.owed.store(true, Ordering::SeqCst);
        }));
        a.set(&[("s-cx", Program::Codex)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("stepped and carried on", || {
            a.carried.load(Ordering::SeqCst) == 1
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(a.stepped.load(Ordering::SeqCst), 1, "one step");
        assert!(!a.owed.load(Ordering::SeqCst), "the carry-on is typed");
        assert!(
            a.relaunched.lock().unwrap().is_empty(),
            "the step's own `/exit` is no exit"
        );
        assert!(
            a.badges.lock().unwrap().iter().all(Option::is_none),
            "nothing said on the tab: {:?}",
            a.badges.lock().unwrap()
        );
        assert_eq!(a.runs.load(Ordering::SeqCst), 1, "one run throughout");
        host.shutdown_and_join();
        // NEGATIVE CONTROL: nothing due — the Codex is never stepped.
        let a = Arc::new(Acting::default());
        a.set(&[("s-cy", Program::Codex)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || a.runs.load(Ordering::SeqCst) == 1);
        host.note_activation();
        until("parked for the notice", || {
            a.parks.load(Ordering::SeqCst) >= 1
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(a.stepped.load(Ordering::SeqCst), 0);
        host.shutdown_and_join();
    }

    /// D1 OF THE LIVE E2E OF 2026-09-26: THE WORKER SAYS WHETHER NOBODY HAS
    /// ASKED ITS SESSION ANYTHING, from the conversation its Claude Code holds
    /// — the kept record's, read through the transcript ([`Acts::tasked`]) —
    /// so the loop's turn-end policy types nothing into one whose record
    /// holds only the harness's own turns. NEGATIVE CONTROLS: a conversation
    /// someone asked something, and one nobody can read, are the screen's to
    /// judge; a Codex is never asked about.
    #[test]
    fn the_worker_says_its_session_has_no_task_from_its_conversations_record() {
        let seen = |agent: Program, tasked: Option<bool>| {
            let a = Arc::new(Acting::default());
            *a.tasked.lock().unwrap() = tasked;
            a.set(&[("s-t", agent)]);
            let host = HostHandle::start(on(), false, false, a.hooks());
            until("the loop read it", || {
                a.taskless_seen.lock().unwrap().is_some()
            });
            let seen = a.taskless_seen.lock().unwrap().unwrap_or_default();
            let asked = a.tasked_asked.lock().unwrap().clone();
            host.shutdown_and_join();
            (seen, asked)
        };
        let (taskless, asked) = seen(Program::Claude, Some(false));
        assert!(taskless, "only the harness's own turns: no task");
        assert!(
            asked
                .iter()
                .all(|c| c == "0badf00d-1111-2222-3333-444455556666"),
            "the kept conversation's record: {asked:?}"
        );
        assert!(!seen(Program::Claude, Some(true)).0, "asked something");
        assert!(
            !seen(Program::Claude, None).0,
            "nobody can say: the screen's"
        );
        let (taskless, asked) = seen(Program::Codex, Some(false));
        assert!(!taskless && asked.is_empty(), "a Codex is not asked about");
    }

    /// D3(a) OF THE LIVE E2E OF 2026-09-26: A TURN BETWEEN TWO LOOKS STARTS
    /// THE PAUSES OVER. The upgrade's pause climbed while its step waited on
    /// the same word — also across a turn of the session's in between, so the
    /// Stage-1 end was looked at +0.3 s, then +61 s, then +366 s. Told a turn
    /// ran ([`IdleHost::turn_ran`]), the worker forgets the looks it counted
    /// and the pause it waited out, and looks at the new point as it comes:
    /// every pause is the first rung. What bounds the upgrade's hold on the
    /// session's turn ends is NOT started over (review of 2026-09-26): its
    /// waits in a row still count toward [`upgrade_drive::OWNED_SETTLE_LOOKS`], so an agent
    /// that keeps waking itself during a drain cannot keep the turn ends
    /// owned for ever. NEGATIVE CONTROL: with no turn between, the same waits
    /// climb the ladder as before, and lapse the same.
    #[test]
    fn a_turn_between_two_looks_starts_the_pauses_over() {
        use upgrade_drive::LATER;
        let looks = |turns: bool| {
            let a = Arc::new(Acting::default());
            *a.owns_seen.lock().unwrap() = Some(Vec::new());
            *a.steps.lock().unwrap() = ["wait:settling"; 3].into();
            a.due.store(true, Ordering::SeqCst);
            a.turns_between.store(turns, Ordering::SeqCst);
            a.set(&[("s-r", Program::Claude)]);
            let host = HostHandle::start(on(), false, false, a.hooks());
            until("three looks", || a.stepped.load(Ordering::SeqCst) >= 3);
            until("three seen", || {
                a.owns_seen
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|v| v.len() >= 3)
            });
            host.shutdown_and_join();
            let owns = a.owns_seen.lock().unwrap().clone().unwrap_or_default();
            (a.pauses.lock().unwrap().clone(), owns)
        };
        let lapses = [true, true, false];
        let (pauses, owns) = looks(true);
        assert_eq!(pauses[..3], [LATER[0]; 3], "a turn between each");
        assert_eq!(owns[..3], lapses, "a turn between: the hold still lapses");
        let (pauses, owns) = looks(false);
        assert_eq!(
            pauses[..3],
            [LATER[0], LATER[1], LATER[2]],
            "no turn between: the ladder"
        );
        assert_eq!(owns[..3], lapses, "no turn between");
    }

    /// A WAIT ON SOMETHING NEW STARTS THE PAUSES OVER (live, 2026-09-26: a
    /// Codex tab's daemon waited `held`, then `settling`, and then its client
    /// `settling` — three words, one climbing ladder, and the client's
    /// twenty-second settle was looked at again after five minutes, then
    /// ten). The pause climbs only while the step waits on the same word;
    /// the ownership bound still counts every wait in a row. NEGATIVE
    /// CONTROL: the same word again climbs, as before; an act starts all
    /// over.
    #[test]
    fn a_wait_on_something_new_starts_the_pauses_over() {
        use upgrade_drive::LATER;
        let mut run = UpgradeRun::default();
        let pauses: Vec<After> = [
            "wait:daemon-first:held",
            "wait:daemon-first:settling",
            "wait:settling",
            "wait:settling",
            "wait:settling",
        ]
        .iter()
        .map(|s| run.after(s))
        .collect();
        assert_eq!(
            pauses,
            [
                After::Later(LATER[0]),
                After::Later(LATER[0]),
                After::Later(LATER[0]),
                After::Later(LATER[1]),
                After::Later(LATER[2]),
            ]
        );
        assert_eq!(run.waits, 5, "the ownership bound counts every wait");
        assert_eq!(run.after("announced:1"), After::NextIdle);
        assert_eq!((run.waits, run.same), (0, 0));
        assert_eq!(run.after("wait:settling"), After::Later(LATER[0]));
    }

    /// A RE-ARMED ROUND OWNS ITS FIRST SETTLE (the no-stall review of
    /// 2026-09-27, B3): the looks a stopped round took while it rested are not
    /// waits of the new round. A round that gave up, rested through many looks
    /// and re-armed must own its next settle's turn end, or the supervisor
    /// continues the worker, the settle never completes and the new round's
    /// notice is never typed. NEGATIVE CONTROL: the new round's own settles
    /// still count toward the bound.
    #[test]
    fn a_re_armed_round_owns_its_first_settle() {
        let mut run = UpgradeRun::default();
        for step in ["gave-up", "released:gave-up"] {
            run.after(step);
        }
        for _ in 0..20 {
            run.after("wait:failed");
        }
        assert!(
            run.waits >= upgrade_drive::OWNED_SETTLE_LOOKS,
            "the rest looked many times"
        );
        run.after("rearmed:unanswered");
        assert_eq!(
            (run.waits, run.same),
            (0, 0),
            "the re-arm starts the counts over"
        );
        run.after("wait:settling");
        assert!(upgrade_drive::owns_turn_ends("wait:settling", run.waits));
        for _ in 0..upgrade_drive::OWNED_SETTLE_LOOKS {
            run.after("wait:settling");
        }
        assert!(!upgrade_drive::owns_turn_ends("wait:settling", run.waits));
    }

    /// THE UPGRADE OWNS A SESSION'S TURN ENDS BY ITS OWN STEP'S WORD (the
    /// philosophy review of 2026-09-25, blocking; the hazards review of the
    /// same day): after the announcement, and while READY is given and the
    /// restart's gate only settles or drains background work — bounded —
    /// and never after an answer without READY, a hold, or a last word
    /// (done, failed, gave up); and only while `[harness] upgrade` allows it.
    /// The screen pattern it replaces held a worker idle for ever on each of
    /// those. NEGATIVE CONTROLS: past the bounded looks, a drain or a settle
    /// owns nothing; the switch off owns nothing.
    #[test]
    fn the_upgrade_owns_turn_ends_only_while_its_step_says_so() {
        assert!(upgrade_drive::owns_turn_ends("announced", 0));
        assert!(upgrade_drive::owns_turn_ends("announced:2", 0));
        assert!(upgrade_drive::owns_turn_ends("wait:settling", 0));
        assert!(!upgrade_drive::owns_turn_ends(
            "wait:settling",
            upgrade_drive::OWNED_SETTLE_LOOKS
        ));
        assert!(upgrade_drive::owns_turn_ends(
            "wait:background",
            upgrade_drive::OWNED_BACKGROUND_LOOKS - 1
        ));
        assert!(!upgrade_drive::owns_turn_ends(
            "wait:background",
            upgrade_drive::OWNED_BACKGROUND_LOOKS
        ));
        for last in [
            "wait:awaiting-ready",
            "held-back:person",
            "done",
            "gave-up",
            "failed:x",
            "adopted",
            "continued",
            "current",
        ] {
            assert!(!upgrade_drive::owns_turn_ends(last, 0), "{last}");
        }
        // At a break (2026-09-26): a wait on the agent's own work keeps the
        // notice's claim; a last word, a hold, the limit, a void and a
        // release give it back at once — a busy screen reaches no idle point
        // to recompute it. NEGATIVE CONTROL: the notice and the wait keep it.
        for gives_back in [
            "gave-up",
            "wait:skipped",
            "wait:deferred",
            "wait:attended",
            "wait:held",
            "wait:limited",
            "failed:signal-refused",
            "refused:--bogus",
            "held-back:terminal:tmux",
            "drain-expired:box",
            "released:gave-up",
            "wait:release:not-idle",
        ] {
            assert!(upgrade_drive::released_at_break(gives_back), "{gives_back}");
        }
        for keeps in ["announced:2", "wait:background", "wait:not-idle"] {
            assert!(!upgrade_drive::released_at_break(keeps), "{keeps}");
        }

        // In the host: announced, then an answer without READY.
        let a = Arc::new(Acting::default());
        *a.owns_seen.lock().unwrap() = Some(Vec::new());
        *a.steps.lock().unwrap() = ["announced:1", "wait:awaiting-ready"].into();
        a.due.store(true, Ordering::SeqCst);
        a.set(&[("s-o", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("two steps", || a.stepped.load(Ordering::SeqCst) == 2);
        until("both seen", || {
            a.owns_seen
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|v| v.len() >= 2)
        });
        assert_eq!(
            a.owns_seen
                .lock()
                .unwrap()
                .as_ref()
                .map(|v| v[..2].to_vec()),
            Some(vec![true, false]),
            "announced owns; an answer without READY is an ordinary turn end"
        );
        host.shutdown_and_join();

        // The switch off: a step that would own owns nothing.
        let a = Arc::new(Acting::default());
        *a.owns_seen.lock().unwrap() = Some(Vec::new());
        *a.steps.lock().unwrap() = ["announced:1"].into();
        a.due.store(true, Ordering::SeqCst);
        a.set(&[("s-p", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("announced", || {
            a.owns_seen
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|v| !v.is_empty())
        });
        assert_eq!(
            a.owns_seen
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|v| v.first().copied()),
            Some(true)
        );
        let seen = a.owns_seen.lock().unwrap().as_ref().map_or(0, Vec::len);
        let mut no_upgrade = on();
        no_upgrade.set("upgrade", "false").unwrap();
        host.set_config(no_upgrade);
        host.note_activation();
        until("seen again", || {
            a.owns_seen
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|v| v.len() > seen)
        });
        assert_eq!(
            a.owns_seen
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|v| v.last().copied()),
            Some(false)
        );
        host.shutdown_and_join();
    }

    /// A step that WAITED is looked at again after its pause — by the host
    /// thread's timed bell, not a sweep — and each pause in a row is longer.
    /// NEGATIVE CONTROL: after the step that finishes, no look comes.
    #[test]
    fn a_step_that_waited_is_looked_at_again_after_its_pause() {
        let a = Arc::new(Acting::default());
        *a.steps.lock().unwrap() = ["wait:settling", "wait:not-ready", "done"].into();
        a.due.store(true, Ordering::SeqCst);
        a.set(&[("s-w", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("three steps", || a.stepped.load(Ordering::SeqCst) == 3);
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(
            a.stepped.load(Ordering::SeqCst),
            3,
            "no look after the last word"
        );
        host.shutdown_and_join();
    }

    /// The agent a worker's own step ends and relaunches is the step's act:
    /// the program leaving and coming back during it neither stops the
    /// worker nor hands it the exit. NEGATIVE CONTROL: the same flap outside
    /// a step does (the next test).
    #[test]
    fn the_agent_a_step_restarts_is_not_an_exit() {
        let a = Arc::new(Acting::default());
        *a.steps.lock().unwrap() = ["done"].into();
        a.due.store(true, Ordering::SeqCst);
        a.open.lock().unwrap().insert("s-f".to_string());
        let flap = Arc::clone(&a);
        *a.during_step.lock().unwrap() = Some(Box::new(move || {
            flap.set(&[]);
            std::thread::sleep(Duration::from_millis(40));
            flap.set(&[("s-f", Program::Claude)]);
        }));
        a.set(&[("s-f", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("stepped, and the same loop runs on", || {
            a.stepped.load(Ordering::SeqCst) == 1
        });
        std::thread::sleep(Duration::from_millis(60));
        assert!(a.relaunched.lock().unwrap().is_empty(), "no relaunch");
        assert_eq!(
            (
                a.runs.load(Ordering::SeqCst),
                a.parks.load(Ordering::SeqCst)
            ),
            (1, 1),
            "one worker, one run: its idle point asked for at attach, the step taken there"
        );
        assert_eq!(host.live(), ["s-f"]);
        host.shutdown_and_join();
    }

    /// THE RESTART IN PLACE (D3): the loop asks its worker to restart the
    /// agent at a point nothing typed can answer; the worker makes it —
    /// the agent leaving and coming back during it is the step's act, never
    /// an exit — and, the new process adopted, parks the loop at its next
    /// idle point, where the continuation is typed. NEGATIVE CONTROLS:
    /// under `[harness] relaunch = false` no restart is made (the loop's
    /// point is the person's), and neither for an agent the relaunch is not
    /// written for.
    #[test]
    fn a_restart_the_loop_asks_for_is_made_in_place_and_carried_on() {
        let a = Arc::new(Acting::default());
        a.open.lock().unwrap().insert("s-m".to_string());
        a.set(&[("s-m", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || host.live() == ["s-m"]);
        a.ask_restart.store(true, Ordering::SeqCst);
        until("restarted and carried on", || {
            a.carried.load(Ordering::SeqCst) == 1
        });
        assert_eq!(*a.restarts.lock().unwrap(), ["memory"]);
        assert_eq!(
            *a.restart_said.lock().unwrap(),
            [Some("adopted".to_string())]
        );
        assert!(
            a.relaunched.lock().unwrap().is_empty(),
            "the restart's own leaving is no exit"
        );
        assert_eq!(a.runs.load(Ordering::SeqCst), 1, "the same loop ran on");
        // `[harness] relaunch = false`: none made, and none asked of the hook.
        let mut no_relaunch = on();
        no_relaunch.set("relaunch", "false").unwrap();
        // Read live, as the relaunch on exit reads it: no worker restarts.
        host.set_config(no_relaunch);
        std::thread::sleep(Duration::from_millis(50));
        until("attached under the limit", || host.live() == ["s-m"]);
        a.ask_restart.store(true, Ordering::SeqCst);
        until("asked", || a.restart_said.lock().unwrap().len() == 2);
        assert_eq!(a.restart_said.lock().unwrap()[1], None);
        assert_eq!(a.restarts.lock().unwrap().len(), 1);
        host.shutdown_and_join();
        // Codex: the relaunch is not written for it.
        let a = Arc::new(Acting::default());
        a.open.lock().unwrap().insert("s-x".to_string());
        a.set(&[("s-x", Program::Codex)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || host.live() == ["s-x"]);
        a.ask_restart.store(true, Ordering::SeqCst);
        until("asked", || a.restart_said.lock().unwrap().len() == 1);
        assert_eq!(a.restart_said.lock().unwrap()[0], None);
        assert!(a.restarts.lock().unwrap().is_empty());
        host.shutdown_and_join();
    }

    /// U1: an agent that left while its loop held a stall the server
    /// published was ended by the stall's remedy (`signal term|kill`), and
    /// is relaunched on its conversation, the relaunch told so — even with
    /// a person's keystroke just before, which nothing read. NEGATIVE
    /// CONTROL: the same exit with no stall held is the person's.
    #[test]
    fn an_agent_its_stalls_remedy_ended_is_relaunched() {
        let a = Arc::new(Acting::default());
        a.open.lock().unwrap().insert("s-z".to_string());
        a.set(&[("s-z", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || host.live() == ["s-z"]);
        *a.human_ms.lock().unwrap() = Some(1_500);
        a.stall_held.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        a.set(&[]);
        until("relaunched", || a.relaunched.lock().unwrap().len() == 1);
        assert_eq!(*a.stalled_exits.lock().unwrap(), [true]);
        // The relaunched agent: its own loop, no stall held.
        a.stall_held.store(false, Ordering::SeqCst);
        a.set(&[("s-z", Program::Claude)]);
        until("attached again", || host.live() == ["s-z"]);
        std::thread::sleep(Duration::from_millis(30));
        a.set(&[]);
        until("detached", || host.live().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            a.relaunched.lock().unwrap().len(),
            1,
            "the person's exit, with no stall held"
        );
        host.shutdown_and_join();
    }

    /// RELAUNCH ON EXIT: the agent left a tab that lives on, with no person
    /// at it — relaunched once, on the snapshot of that session, and the
    /// host attaches to the agent that comes back. NEGATIVE CONTROLS: a
    /// person's keystroke just before the exit leaves the tab to them, and so
    /// does a halt (`hold=1`: the holder's exit) — neither said; a tab that
    /// CLOSED is no exit; `[harness] relaunch = false` relaunches nothing and
    /// SAYS so, once (configuration took the power away); and under
    /// `[harness] upgrade = false` the relaunch is asked to stay on the build
    /// the agent ran.
    #[test]
    fn an_agent_that_left_without_a_person_is_relaunched_and_a_persons_exit_is_not() {
        let a = Arc::new(Acting::default());
        a.open.lock().unwrap().insert("s-e".to_string());
        a.set(&[("s-e", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || host.live() == ["s-e"]);
        a.set(&[]); // the program left; the tab lives on
        until("relaunched", || a.relaunched.lock().unwrap().len() == 1);
        assert_eq!(*a.upgrading.lock().unwrap(), [true], "onto a newer build");
        a.set(&[("s-e", Program::Claude)]); // the relaunched agent
        until("attached again, and carried on", || {
            host.live() == ["s-e"] && a.carried.load(Ordering::SeqCst) == 1
        });
        // `[harness] upgrade = false`: the next relaunch stays on its build.
        let mut no_upgrade = on();
        no_upgrade.set("upgrade", "false").unwrap();
        host.set_config(no_upgrade);
        a.set(&[]);
        until("relaunched", || a.relaunched.lock().unwrap().len() == 2);
        assert_eq!(
            *a.upgrading.lock().unwrap(),
            [true, false],
            "on its own build"
        );
        host.set_config(on());
        // As in `the_snapshot_follows_its_agent_and_goes_with_its_exit`: until
        // the relaunching worker is reaped, a roster naming the agent again
        // reads as that worker's, and the person's exit below would reach no
        // worker — its assertion then held of nothing.
        until("the relaunching worker reaped", || host.live().is_empty());
        a.set(&[("s-e", Program::Claude)]);
        until("attached again", || host.live() == ["s-e"]);
        // A person typed just before the next exit: theirs.
        *a.human_ms.lock().unwrap() = Some(1_500);
        a.set(&[]);
        until("detached", || host.live().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(a.relaunched.lock().unwrap().len(), 2, "a person's exit");
        // A halted session: the holder's exit.
        *a.human_ms.lock().unwrap() = None;
        a.held.store(true, Ordering::SeqCst);
        a.set(&[("s-e", Program::Claude)]);
        until("attached", || host.live() == ["s-e"]);
        a.set(&[]);
        until("detached", || host.live().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(a.relaunched.lock().unwrap().len(), 2, "a held session");
        assert!(
            a.badges.lock().unwrap().is_empty(),
            "their exits: nothing said"
        );
        a.held.store(false, Ordering::SeqCst);
        // A tab that closed: no exit to relaunch.
        a.set(&[("s-e", Program::Claude)]);
        until("attached", || host.live() == ["s-e"]);
        a.open.lock().unwrap().clear();
        a.set(&[]);
        until("detached", || host.live().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(a.relaunched.lock().unwrap().len(), 2, "a closed tab");
        // The owner limited it.
        let mut no_relaunch = on();
        no_relaunch.set("relaunch", "false").unwrap();
        host.set_config(no_relaunch);
        a.open.lock().unwrap().insert("s-e".to_string());
        a.set(&[("s-e", Program::Claude)]);
        until("attached", || host.live() == ["s-e"]);
        a.set(&[]);
        until("detached", || host.live().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(a.relaunched.lock().unwrap().len(), 2, "relaunch = false");
        until("said once", || !a.badges.lock().unwrap().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        let badges = a.badges.lock().unwrap().clone();
        assert_eq!(badges.len(), 1, "{badges:?}");
        assert!(
            badges[0]
                .as_deref()
                .is_some_and(|b| b.contains("not relaunched: [harness] relaunch = false")),
            "{badges:?}"
        );
        host.shutdown_and_join();
    }

    /// D2 of the 2026-09-26 live test: WHAT THE EXIT LEFT of Claude's own
    /// record is read as the exit is seen — within its settle, before the
    /// back-off — and handed to every attempt: a crash whose record another
    /// Claude Code removes during the back-off (its start sweeps dead
    /// agents' records) is relaunched, handed `survived`. NEGATIVE CONTROL:
    /// a graceful exit, whose record the exit removes a moment after it is
    /// seen (the first look still saw it), is handed `removed` and left,
    /// nothing said.
    #[test]
    fn a_crash_is_read_at_its_exit_whatever_removes_its_record_during_the_back_off() {
        let a = Arc::new(Acting::default());
        a.open.lock().unwrap().insert("s-r".to_string());
        a.set(&[("s-r", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || host.live() == ["s-r"]);
        a.sweep_in_backoff.store(true, Ordering::SeqCst);
        a.set(&[]); // SIGKILLed: its record left behind
        until("relaunched", || a.relaunched.lock().unwrap().len() == 1);
        assert!(
            a.record_gone.load(Ordering::SeqCst),
            "another Claude Code removed it during the back-off"
        );
        assert_eq!(*a.exit_records.lock().unwrap(), ["survived"]);
        a.set(&[("s-r", Program::Claude)]); // the relaunched agent
        until("attached again, and carried on", || {
            host.live() == ["s-r"] && a.carried.load(Ordering::SeqCst) == 1
        });
        // NEGATIVE CONTROL: a graceful exit.
        a.sweep_in_backoff.store(false, Ordering::SeqCst);
        a.record_gone.store(false, Ordering::SeqCst);
        a.exit_looks.store(0, Ordering::SeqCst);
        a.graceful.store(true, Ordering::SeqCst);
        a.set(&[]);
        until("handed", || a.exit_records.lock().unwrap().len() == 2);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(*a.exit_records.lock().unwrap(), ["survived", "removed"]);
        assert_eq!(a.exit_looks.load(Ordering::SeqCst), 2, "seen, then removed");
        assert_eq!(a.relaunched.lock().unwrap().len(), 1, "left alone");
        assert!(a.badges.lock().unwrap().is_empty(), "nothing said");
        host.shutdown_and_join();
    }

    /// THE RELAUNCHED AGENT IS ITS LOOP'S AT ONCE: the relaunch ends when
    /// the new process holds the conversation (`adopted`), a worker attaches
    /// to it and runs its loop — which answers whatever the new process opened
    /// with — and where that loop first parks at idle the continuation is
    /// typed ([`relaunch::resume`]); one typed before its model was read
    /// (`continued`) is read at the next idle point, and `done` owes nothing
    /// more. Under `[harness] upgrade = false` too: the continuation is the
    /// relaunch's, not the upgrade's. NEGATIVE CONTROL: with nothing owed, a
    /// park takes no carry-on step.
    #[test]
    fn a_relaunched_agent_is_carried_on_where_its_loop_parks() {
        let a = Arc::new(Acting::default());
        *a.carry.lock().unwrap() = ["continued", "done"].into();
        a.open.lock().unwrap().insert("s-h".to_string());
        a.set(&[("s-h", Program::Claude)]);
        let mut no_upgrade = on();
        no_upgrade.set("upgrade", "false").unwrap();
        let host = HostHandle::start(no_upgrade, false, false, a.hooks());
        until("attached", || host.live() == ["s-h"]);
        host.note_activation();
        until("a park for the notice", || {
            a.parks.load(Ordering::SeqCst) == 1
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(a.carried.load(Ordering::SeqCst), 0, "nothing owed");
        a.set(&[]);
        until("adopted", || a.owed.load(Ordering::SeqCst));
        a.set(&[("s-h", Program::Claude)]); // the relaunched agent
        until("carried on, its model read at the next idle point", || {
            a.carried.load(Ordering::SeqCst) == 2
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(a.carried.load(Ordering::SeqCst), 2, "done: nothing more");
        assert!(!a.owed.load(Ordering::SeqCst));
        assert_eq!(a.stepped.load(Ordering::SeqCst), 0, "no upgrade step");
        host.shutdown_and_join();
    }

    /// The session's program can read as the agent a moment after it died
    /// (its last frame is still on the screen), so a worker can attach to an
    /// agent already gone — measured live: that worker then had nothing to
    /// relaunch from. What the LAST worker read while the agent ran is the
    /// session's, kept across workers: here a worker started after the agent
    /// died (a policy reload restarts it) relaunches on it, once. NEGATIVE
    /// CONTROL: a session nothing was ever read of — a wrapper, a script, not
    /// a launch the relaunch is for — is neither relaunched nor said.
    #[test]
    fn a_worker_that_attached_to_a_dead_agent_relaunches_on_what_was_read() {
        let a = Arc::new(Acting::default());
        a.open.lock().unwrap().insert("s-d".to_string());
        a.set(&[("s-d", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || a.runs.load(Ordering::SeqCst) == 1);
        a.gone.store(true, Ordering::SeqCst);
        let mut changed = on();
        changed.set("dismiss_surveys", "false").unwrap();
        host.set_config(changed);
        // (Nothing could be read at its attach, and nothing is missing: the
        // session's record is the last worker's, complete — no park.)
        until("a second worker, attached to what is gone", || {
            a.runs.load(Ordering::SeqCst) == 2
        });
        assert_eq!(a.parks.load(Ordering::SeqCst), 0);
        a.set(&[]);
        until("relaunched", || a.relaunched.lock().unwrap().len() == 1);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(a.relaunched.lock().unwrap().len(), 1, "once");
        assert!(a.badges.lock().unwrap().is_empty(), "nothing to say");
        host.shutdown_and_join();
        // NEGATIVE CONTROL: never read at all.
        let b = Arc::new(Acting::default());
        b.gone.store(true, Ordering::SeqCst);
        b.open.lock().unwrap().insert("s-n".to_string());
        b.set(&[("s-n", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, b.hooks());
        until(
            "attached, its idle point asked for once to read again",
            || b.runs.load(Ordering::SeqCst) == 1 && b.parks.load(Ordering::SeqCst) == 1,
        );
        b.set(&[]);
        until("detached", || host.live().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert!(b.relaunched.lock().unwrap().is_empty());
        assert!(b.badges.lock().unwrap().is_empty(), "nothing to say");
        host.shutdown_and_join();
    }

    /// THE SNAPSHOT IS OF THE AGENT THAT LEFT, never of another. (1) A
    /// person ends agent A, then starts B through a wrapper nothing can be
    /// read of: A's snapshot went with A's handled exit, so B's crash is
    /// relaunched as nobody (journaled), never as A. (2) While an agent runs,
    /// an in-app `/clear` moves its conversation: the snapshot follows it at
    /// the host's next wake, and the relaunch resumes the new one. (3)
    /// Another job holds the tab while the program still reads as the agent:
    /// the snapshot is forgotten. NEGATIVE CONTROL: a worker that attached to
    /// an agent already gone, the shell back in front, still relaunches on
    /// what was read (the test above).
    #[test]
    fn the_snapshot_follows_its_agent_and_goes_with_its_exit() {
        let a = Arc::new(Acting::default());
        a.open.lock().unwrap().insert("s-s".to_string());
        a.set(&[("s-s", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || host.live() == ["s-s"]);
        // (2) `/clear` under the same process: followed at the next wake.
        let cleared = "0badf00d-7777-2222-3333-444455556666".to_string();
        *a.conversation.lock().unwrap() = Some(cleared.clone());
        ring();
        std::thread::sleep(Duration::from_millis(30));
        a.set(&[]);
        until("relaunched", || a.relaunched.lock().unwrap().len() == 1);
        assert_eq!(*a.resumed.lock().unwrap(), [Some(cleared)], "the new one");
        // Its worker ends once the relaunch returns: until it is reaped, a
        // roster naming the agent again reads as that worker's, and the
        // person's exit below would reach no worker at all (the snapshot it
        // refreshed then carried onto the wrapper's exit: a flake, 1 in 15).
        until("the relaunching worker reaped", || host.live().is_empty());
        // (1) A person's exit; then a wrapper nothing can be read of.
        a.set(&[("s-s", Program::Claude)]);
        until("attached again", || host.live() == ["s-s"]);
        *a.human_ms.lock().unwrap() = Some(1_000);
        a.set(&[]);
        until("detached", || host.live().is_empty());
        *a.human_ms.lock().unwrap() = None;
        a.gone.store(true, Ordering::SeqCst);
        a.set(&[("s-s", Program::Claude)]);
        until("attached to the wrapper", || host.live() == ["s-s"]);
        a.set(&[]);
        until("detached", || host.live().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(a.relaunched.lock().unwrap().len(), 1, "B is not A");
        // (3) A readable agent, then another job in front of the tab.
        a.gone.store(false, Ordering::SeqCst);
        a.set(&[("s-s", Program::Claude)]);
        until("attached", || host.live() == ["s-s"]);
        *a.fg.lock().unwrap() = Some(Foreground::Other);
        a.gone.store(true, Ordering::SeqCst);
        ring();
        std::thread::sleep(Duration::from_millis(30));
        a.set(&[]);
        until("detached", || host.live().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(a.relaunched.lock().unwrap().len(), 1, "forgotten");
        assert!(a.badges.lock().unwrap().is_empty(), "nothing said");
        host.shutdown_and_join();
    }

    /// A relaunch that keeps failing is tried again on the growing back-off,
    /// said on the session's attention once it has missed three in a row,
    /// and the word is cleared when it lands; one that can never be made is
    /// said at once and not tried again. NEGATIVE CONTROLS: an exit that was
    /// the launch's own end (a `-p` run) is tried once and never said; a
    /// person who comes back to the tab during the back-off ends it without
    /// a try.
    #[test]
    fn a_failing_relaunch_is_retried_and_said_and_an_impossible_one_is_said() {
        let a = Arc::new(Acting::default());
        *a.relaunches.lock().unwrap() = [
            "wait:shell-prompt",
            "failed:no-resume",
            "wait:resume",
            "adopted",
        ]
        .into();
        a.open.lock().unwrap().insert("s-x".to_string());
        a.set(&[("s-x", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || {
            host.live() == ["s-x"] && a.runs.load(Ordering::SeqCst) == 1
        });
        // A badge is published before the exiting worker has returned. The
        // same session id in `live` can still name that old worker, so each
        // later exit must wait for a new body's observed start.
        let attach_again = || {
            until("previous worker detached", || host.live().is_empty());
            let prior_runs = a.runs.load(Ordering::SeqCst);
            a.set(&[("s-x", Program::Claude)]);
            until("new worker attached", || {
                host.live() == ["s-x"] && a.runs.load(Ordering::SeqCst) > prior_runs
            });
        };
        a.set(&[]);
        until("four tries", || a.relaunched.lock().unwrap().len() == 4);
        until("said, then cleared", || a.badges.lock().unwrap().len() == 2);
        {
            let badges = a.badges.lock().unwrap();
            assert!(
                badges[0]
                    .as_deref()
                    .is_some_and(|b| b.contains("keeps failing") && b.contains("wait:resume")),
                "{badges:?}"
            );
            assert_eq!(badges[1], None, "cleared when it landed");
        }
        attach_again();
        // Never possible: said, and not tried again.
        *a.relaunches.lock().unwrap() = ["refused:not-resumable --worktree"].into();
        a.set(&[]);
        until("said", || a.badges.lock().unwrap().len() == 3);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(a.relaunched.lock().unwrap().len(), 5, "one try");
        assert!(
            a.badges.lock().unwrap()[2]
                .as_deref()
                .is_some_and(|b| b.contains("cannot be relaunched")),
        );
        // The launch's own end: one try, nothing said.
        attach_again();
        *a.relaunches.lock().unwrap() = ["ended:one-shot"].into();
        a.set(&[]);
        until("one try", || a.relaunched.lock().unwrap().len() == 6);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(a.relaunched.lock().unwrap().len(), 6, "never tried again");
        assert_eq!(a.badges.lock().unwrap().len(), 3, "nothing said");
        // NEGATIVE CONTROL: a person back at the tab during the pause.
        attach_again();
        *a.relaunches.lock().unwrap() = ["wait:shell-prompt"].into();
        a.set(&[]);
        until("one miss", || a.relaunched.lock().unwrap().len() == 7);
        *a.human_ms.lock().unwrap() = Some(10);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(a.relaunched.lock().unwrap().len(), 7, "a person's tab now");
        host.shutdown_and_join();
    }

    /// ANOTHER ACTOR ON THE UPGRADE LOCK (an upgrade step in another tab, a
    /// hand-run sweep) is waited out, never counted: four busy looks, then
    /// the relaunch lands, and nothing was said. NEGATIVE CONTROL: the
    /// previous test's three real misses in a row ARE said.
    #[test]
    fn a_relaunch_behind_another_actor_waits_and_says_nothing() {
        let a = Arc::new(Acting::default());
        *a.relaunches.lock().unwrap() = [
            "busy:another-sweep",
            "busy:another-sweep",
            "busy:another-sweep",
            "busy:another-sweep",
            "adopted",
        ]
        .into();
        a.open.lock().unwrap().insert("s-b".to_string());
        a.set(&[("s-b", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || host.live() == ["s-b"]);
        a.set(&[]);
        until("five tries", || a.relaunched.lock().unwrap().len() == 5);
        std::thread::sleep(Duration::from_millis(50));
        assert!(a.badges.lock().unwrap().is_empty(), "nothing said");
        host.shutdown_and_join();
    }

    /// THE WINDOW'S MINUTE SWEEP IS GONE (2026-09-24): no thread of the
    /// window sweeps its tabs for the upgrade, the upgrade driver has no
    /// host loop to run one, and the hand-run verb has no loop either. The
    /// sources are the evidence. NEGATIVE CONTROL: the per-session step the
    /// host takes instead is there.
    #[test]
    fn the_sweep_thread_is_gone() {
        let lib = include_str!("lib.rs");
        let drive = include_str!("../../aterm-agent/src/harness/upgrade_drive.rs");
        let cli = include_str!("../../aterm-agent/src/harness/cli.rs");
        let host = include_str!("harness_host.rs");
        assert!(!lib.contains(&["upgrade_drive", "::host("].concat()));
        assert!(!lib.contains(&["spawn_agent_", "live_upgrade"].concat()));
        assert!(!drive.contains(&["pub fn ", "host("].concat()));
        assert!(!drive.contains(&["HOST_", "EVERY"].concat()));
        assert!(!cli.contains(&["\"--", "every\""].concat()));
        assert!(drive.contains("pub fn step(opts: &Opts) -> Report"));
        assert!(host.contains(&["upgrade_drive", "::step("].concat()));
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
        roster: Mutex<Vec<(String, Program)>>,
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
                roster: Arc::new(move || {
                    p1.roster
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(sid, program)| (sid.clone(), *program, FollowStamp::default()))
                        .collect()
                }),
                still_wanted: Arc::new(move |sid| {
                    p2.roster.lock().unwrap().iter().any(|(s, _)| s == sid)
                }),
                body: self.body(),
                badge: Arc::new(move |_sid, text| {
                    p3.badge_on.store(text.is_some(), Ordering::SeqCst)
                }),
                claim_epoch: Arc::new(move || p4.released.load(Ordering::SeqCst)),
                acts: Acts::inert(),
                backoff: Arc::new(quick_backoff),
                pause: Arc::new(quick_pause),
                upgrade_view: Arc::new(|_| None),
                disk: Arc::new(|_| {}),
            }
        }

        fn kick(&self, plan: Plan) {
            *self.kick.lock().unwrap() = Some(plan);
            self.kicked.notify_all();
        }

        /// The observed state, projected onto the model's variables; `faults`
        /// is the host's own per-session history and `live` whether the host
        /// holds a worker for the session. `cur` is the newest worker, alive
        /// and not asked to stop — running its loop, or waiting out its
        /// restart pause (still the session's supervisor); `old` a body asked
        /// to stop that still runs.
        fn project(
            &self,
            faults: usize,
            live: bool,
        ) -> std::collections::BTreeMap<&'static str, i64> {
            let wanted = i64::from(!self.roster.lock().unwrap().is_empty());
            let running = self.running.lock().unwrap();
            let workers = self.workers.lock().unwrap();
            let newest = workers.last();
            let stopped = newest.is_some_and(|stop| stop.load(Ordering::SeqCst));
            let in_body = newest.is_some_and(|n| running.iter().any(|r| Arc::ptr_eq(r, n)));
            // Asked to stop: a body still running, or the newest worker on its
            // way out of its restart pause.
            let old = running.iter().filter(|s| s.load(Ordering::SeqCst)).count() as i64
                + i64::from(live && stopped && !in_body);
            let cur = i64::from(live && newest.is_some() && !stopped);
            drop(workers);
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
    /// and a projection of a badged session nobody supervises (the give-up
    /// the host no longer makes) is rejected by an invariant, so the check is
    /// not vacuous.
    #[test]
    fn the_real_host_conforms_to_the_worker_lifecycle_model() {
        let model = aterm_spec::derive::harness_worker_lifecycle_model();
        let budget = model.consts.iter().find(|c| c.0 == "Budget").unwrap().1;
        assert_eq!(
            budget, RESTART_BUDGET as i64,
            "the model's budget is the host's"
        );
        let probe = Arc::new(Probe::default());
        // Past the budget the restart pause is long, so the state it waits in
        // — badged, and still the session's supervisor — is observed whole.
        let mut hooks = probe.hooks();
        hooks.backoff = Arc::new(|n| {
            if n > RESTART_BUDGET {
                Duration::from_secs(30)
            } else {
                quick_backoff(n)
            }
        });
        let host = HostHandle::start(on(), false, false, hooks);
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
                let seen = probe.project(host.faults_of("s-t1"), host.live() == ["s-t1"]);
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
            *probe.roster.lock().unwrap() = vec![("s-t1".to_string(), Program::Claude)];
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
        // Past the budget: badged, and still supervised (waiting to restart).
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
            &["Reload", "Exit", "Start"],
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
        step("leave again", &leave, &["Leave", "Exit"]);
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
        let mut gave_up = after_reload.clone();
        gave_up.insert("old", 0);
        gave_up.insert("faulted", 1);
        assert!(!model.check_invariant("NeverGivesUp", &gave_up));
    }

    /// The world one real worker is driven over by
    /// [`the_real_look_conforms_to_the_upgrade_look_model`]: what its looks
    /// and its note read, the state the note or the first step mints (and
    /// whether with the attach's second), and the upgrade its steps take (the
    /// notice, READY folded in, then the restart and its last word).
    #[derive(Default)]
    struct LookWorld {
        due: AtomicBool,
        readable: AtomicBool,
        ready: AtomicBool,
        done: AtomicBool,
        noted: AtomicBool,
        aged: AtomicBool,
        /// The second the first note was asked with: the attach's.
        attached: Mutex<Option<u64>>,
    }

    impl LookWorld {
        /// A worker attached — by the real [`attach`] — to a fresh launch
        /// whose record is not written yet, and its loop's [`WorkerIdle`].
        fn attached(world: &Arc<Self>) -> WorkerIdle {
            world.due.store(true, Ordering::SeqCst);
            let (w1, w2, w3) = (Arc::clone(world), Arc::clone(world), Arc::clone(world));
            let mut acts = Acts::inert();
            acts.due = Arc::new(move |_| {
                if !w1.readable.load(Ordering::SeqCst) {
                    Due::Unread(upgrade_drive::UNREAD_SOCKET)
                } else if w1.due.load(Ordering::SeqCst) {
                    Due::Yes
                } else {
                    Due::No
                }
            });
            // `note_behind`: minted where no state stands, with the second asked.
            acts.behind = Arc::new(move |_, since| {
                let attached = *w2.attached.lock().unwrap().get_or_insert(since);
                if !w2.readable.load(Ordering::SeqCst) {
                    Behind::Unread(upgrade_drive::UNREAD_RECORD)
                } else if w2.due.load(Ordering::SeqCst) && !w2.noted.swap(true, Ordering::SeqCst) {
                    w2.aged.store(since == attached, Ordering::SeqCst);
                    Behind::Noted
                } else {
                    Behind::Nothing
                }
            });
            // The step: a state it finds none of is minted with its own age.
            acts.step = Arc::new(move |_, _| {
                if !w3.noted.swap(true, Ordering::SeqCst) {
                    w3.aged.store(false, Ordering::SeqCst);
                }
                if w3.ready.swap(true, Ordering::SeqCst) {
                    w3.done.store(true, Ordering::SeqCst);
                    "done".to_string()
                } else {
                    "announced:1".to_string()
                }
            });
            acts.snapshot = Arc::new(|sid| Some(snap(sid)));
            let hooks = Hooks {
                roster: Arc::new(Vec::new),
                still_wanted: Arc::new(|_| true),
                body: Arc::new(|_| BodyEnd::Stopped),
                badge: Arc::new(|_, _| {}),
                claim_epoch: Arc::new(|| 0),
                acts,
                backoff: Arc::new(quick_backoff),
                pause: Arc::new(quick_pause),
                upgrade_view: Arc::new(|_| None),
                disk: Arc::new(|_| {}),
            };
            let switches = Arc::new(Switches::default());
            switches.set(&on());
            let job = WorkerJob {
                sid: "s-look".to_string(),
                agent: Program::Claude,
                opts: SuperviseOpts::hosted(),
                stop: Arc::default(),
                interrupt: Arc::default(),
                handover: Arc::default(),
                clear_badge: false,
                faults: Arc::default(),
                park: Arc::default(),
                look_at: Arc::default(),
                left: Arc::default(),
                acting: Arc::default(),
                stalled: Arc::default(),
                switches,
                kept: Arc::default(),
                note: Arc::default(),
            };
            attach(&job, &hooks);
            WorkerIdle {
                sid: job.sid,
                agent: job.agent,
                grace: 120,
                park: job.park,
                look_at: job.look_at,
                acting: job.acting,
                stalled: job.stalled,
                switches: job.switches,
                kept: job.kept,
                hooks,
                run: Mutex::default(),
                owns: AtomicBool::new(false),
                background_at: Mutex::default(),
                tasked: Mutex::default(),
                note: job.note,
                clock_hold: Mutex::default(),
            }
        }

        /// The model's variables as the real worker and its world stand.
        fn project(&self, idle: &WorkerIdle) -> aterm_spec::interp::State {
            let flag = |b: &AtomicBool| i64::from(b.load(Ordering::SeqCst));
            let later = idle
                .look_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some();
            let owed = idle
                .note
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some();
            [
                ("due", flag(&self.due)),
                ("readable", flag(&self.readable)),
                ("ready", flag(&self.ready)),
                ("looks", i64::from(idle.wants() || later)),
                ("done", flag(&self.done)),
                ("owed", i64::from(owed)),
                ("noted", flag(&self.noted)),
                ("aged", flag(&self.aged)),
            ]
            .into_iter()
            .collect()
        }

        /// One action of `HarnessUpgradeLook`, done to the world or taken by
        /// the real worker: a look — the one it owes comes (a later look is
        /// the host's park, as [`visit_workers`] makes it), and the loop's
        /// idle point takes it — or an ask of the owed note ([`ask_note`], as
        /// the host's wakes and the attach ask it). What a look journaled.
        fn act(&self, idle: &WorkerIdle, action: &str) -> Option<String> {
            match action {
                "Lose" => self.readable.store(false, Ordering::SeqCst),
                "Regain" => self.readable.store(true, Ordering::SeqCst),
                "Settle" => self.due.store(false, Ordering::SeqCst),
                "Note" | "NoteUnread" => {
                    assert!(
                        ask_note(&idle.sid, &idle.note, &idle.hooks).is_some(),
                        "{action}: no note owed"
                    );
                }
                "Look" | "NotDue" | "Unread" => {
                    if idle
                        .look_at
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take()
                        .is_some()
                    {
                        idle.park.store(true, Ordering::SeqCst);
                    }
                    assert!(idle.wants(), "{action}: no look owed");
                    return idle.at_idle().map(|s| s.line);
                }
                other => panic!("unmodelled action {other}"),
            }
            None
        }
    }

    /// Tier-1 of `HarnessUpgradeLook`: EVERY transition of the model, from
    /// every reachable state (each reached by its shortest path), replayed
    /// through a real worker — the real [`attach`] to a launch whose record
    /// is not written yet, its [`WorkerIdle`]'s `at_idle` deciding from what
    /// [`Acts::due`] reads, and the real [`ask_note`] — and the worker's park,
    /// later look and owed note projected onto `looks` and `owed` after each;
    /// every invariant checked on every observed state, and each look's
    /// journal line the model's word (`wait:no-socket` for an unread look,
    /// none for a look that let it go). NEGATIVE CONTROLS: the host of
    /// 155c72a28 (`Buggy=1`) would owe nothing after a READY'd session's
    /// refused look, and would give the note up at the attach and mint the
    /// state at the first step — neither of which the real worker does, and
    /// each of which breaks an invariant.
    #[test]
    fn the_real_look_conforms_to_the_upgrade_look_model() {
        let model = aterm_spec::derive::harness_upgrade_look_model();
        // The reachable states, each with the shortest path to it.
        let mut paths = vec![(model.init_state(), Vec::<&str>::new())];
        let mut at = 0;
        while at < paths.len() {
            let (state, path) = paths[at].clone();
            for action in model.actions.iter().map(|a| a.name) {
                let mut next = state.clone();
                if model.fire(action, &mut next) && !paths.iter().any(|(s, _)| *s == next) {
                    let mut longer = path.clone();
                    longer.push(action);
                    paths.push((next, longer));
                }
            }
            at += 1;
        }
        assert!(paths.len() >= 12, "the space reached: {}", paths.len());
        let mut replayed = HashSet::new();
        for (state, path) in &paths {
            for action in model.actions.iter().map(|a| a.name) {
                let mut expect = state.clone();
                if !model.fire(action, &mut expect) {
                    continue;
                }
                replayed.insert(action);
                let world = Arc::new(LookWorld::default());
                let idle = LookWorld::attached(&world);
                assert_eq!(world.project(&idle), model.init_state());
                let mut said = None;
                for step in path.iter().copied().chain([action]) {
                    said = world.act(&idle, step);
                }
                let seen = world.project(&idle);
                assert_eq!(seen, expect, "{path:?} then {action}");
                for inv in &model.invariants {
                    assert!(model.check_invariant(inv.name, &seen), "{}", inv.name);
                }
                match action {
                    "Unread" => assert_eq!(said.as_deref(), Some("upgrade step=wait:no-socket")),
                    "NotDue" => assert_eq!(said, None),
                    _ => {}
                }
            }
        }
        assert_eq!(
            replayed.len(),
            model.actions.len(),
            "every action replayed: {replayed:?}"
        );

        // NEGATIVE CONTROLS.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        for (path, broken) in [
            (&["Regain", "Look", "Lose", "Unread"][..], "NeverDropped"),
            (&["NoteUnread", "Regain", "Look"][..], "BehindFromTheAttach"),
        ] {
            let world = Arc::new(LookWorld::default());
            let idle = LookWorld::attached(&world);
            let mut b = buggy.init_state();
            for action in path {
                assert!(buggy.fire(action, &mut b), "{action}");
                world.act(&idle, action);
            }
            let seen = world.project(&idle);
            assert_ne!(
                seen, b,
                "{path:?}: the real worker is not the host of 155c72a28"
            );
            assert!(model.check_invariant(broken, &seen), "{seen:?}");
            assert!(!model.check_invariant(broken, &b), "{b:?}");
        }
    }
}
