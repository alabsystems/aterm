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
//! program is an agent it supervises gets a worker; one whose program left
//! — its agent holding the tab no longer: a name the roster could not read
//! while the agent still holds it is no exit ([`keep_holding`]) — or that
//! closed, loses it. Which agents get one is ONE predicate,
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
//! one process at a time. Work the outgoing instance had in flight crosses
//! too (round four of the 2026-09 update robustness work, plan item 7): the
//! cold restore's relaunch queue is paused at the park and what it still owes
//! rides the handoff layout to the successor ([`HostHandle::pause_restored`]),
//! and the successor's Commit carries on every restart record left in flight
//! ([`HostHandle::resume_after_handoff`]).
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
//!   ([`IdleHost::at_background`]) and the worker types only a line that
//!   ends nothing there — a NOTICE, or the RELEASE LINE a give-up, a void or
//!   a stop owes (2026-09-27: a break that never ends is the only point such
//!   a session has) — and ends nothing (owner, 2026-09-26: "The notice
//!   interrupts the agent's orchestration once, and the restart still never
//!   kills running work"). Only a whole `REASK_S` of that work running on
//!   earns a re-ask there, naming what runs. After `MAX_ASKS` notices the
//!   upgrade gives up.
//!   A tab whose poll loops could never end was otherwise told once and then
//!   waited on for days (2026-09-26). While the agent
//!   winds down (READY given, the restart imminent) the upgrade OWNS the
//!   session's turn ends ([`IdleHost::owns_turn_end`], read from the step's
//!   own result, never from the screen) and nothing is typed — and so while a
//!   person at the tab alone holds its act back, for the person's grace and one
//!   look past it, so the lapse is the upgrade's, never a `keep going` typed over
//!   a READY (2026-09-27); an agent that
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
//!   program leaves the agent — the agent not read to hold the tab
//!   ([`Acts::holds`]) — and the tab lives on, the host hands the worker the
//!   exit instead of just stopping it. An exit a person owns (a
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
//!   and one that keeps failing — never a silent give-up. AN EXIT THE
//!   HARNESS'S OWN RESTART MADE is read first, and is no one else's (S0 and
//!   S3 of the in-flight review, 2026-09-27): a restart — the upgrade's,
//!   Claude Code's or Codex's, or the restart in place's — whose step
//!   returned with its relaunch still to type, and whose agent then left
//!   ([`aterm_agent::harness::relaunch::restarted`]), is carried
//!   ([`aterm_agent::harness::relaunch::carry_restart`]) whoever is at the
//!   tab and whatever `[harness] relaunch` says — the relaunch line's own
//!   look still waits on a person and a hold — at least every
//!   [`relaunch::CARRY_EVERY`], until it lands or is said.
//! * THE RESTART IN PLACE (`[harness] relaunch`). Where the loop meets a
//!   point nothing typed can answer — Claude Code's critical-memory banner —
//!   it asks its worker to restart the agent there ([`IdleHost::restart`]):
//!   ended at that idle point and relaunched on its conversation
//!   ([`aterm_agent::harness::relaunch::restart_here`]), the loop running on
//!   over the new process and carrying it on at its next idle point.
//! * THE API'S REACH (`[harness] probe_api`; the outage of 2026-09-27). Where
//!   the loop waits at an API error the network caused, it asks its worker
//!   what the host MEASURES of the agent's route ([`IdleHost::reach`]): a
//!   Claude Code on the default route is measured by the instance's one
//!   probe ([`crate::harness_netprobe`], shared by every session, running
//!   only while one asks), any other route is not ([`Acts::route`], read
//!   once per agent process), and the policy continues an unreachable wall
//!   as soon as the API is measured back.
//!
//! **The owner sees the upgrade** (gap audit 2026-09-24): the host thread
//! keeps the window's view of its tabs' upgrades
//! ([`aterm_agent::harness::upgrade_drive::View`], [`Hooks::upgrade_view`]) —
//! each tab's `upgrade=`, the waiting record, a row and the tab's
//! `owner=upgrade` attention for a STALLED one — looked at again after a
//! worker acts ([`note_upgrade_act`]), at an activation notice or the
//! owner's word (`aterm harness upgrade <sid> --now|--defer|--skip`, whose
//! marker the activation wake watches), when the roster changes, at the
//! instant the view itself names (an upgrade turning overdue, a word running
//! out), and when the screen of a tab whose row reads its agent's live status
//! moves (and once more [`STATUS_AFTER_SCREEN`] after): never on a timer of
//! its own.
//!
//! **The watch** (design record 2026-09-28, "No upgrade stuck forever",
//! rollout step 7): every clock the upgrade has ran inside a step, and a step
//! ran only at a point the session's loop offered — tab #1 offered none for
//! three days, and nothing kept its time. Each upgrade record now carries its
//! own deadline (`upgrade_drive::watch_at`); the view names it as an instant,
//! the host wakes at it and hands the tab to [`Acts::watch`] on a thread of
//! its own at Background QoS ([`watch_upgrades`]), as it does at a worker's
//! start and at an activation notice. The watch reads through a DRY-RUN visit
//! and writes only under the sweep lock's `try_lock`: its own fields, the
//! fresh wait word, a retarget, a limit's clock hold, a re-arm. It has no
//! hands — it never types, presses, pastes, signals, kills, terminates or
//! ends anything, past any budget (`tools/grep_guard.sh` H1) — and a worker
//! whose upgrade reads due but asks for no point is parked again (the
//! re-park, [`watch_tab`]), so the loop's next point takes the step.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_agent::harness::netwatch::Route;
use aterm_agent::harness::relaunch::{
    self, ExitLook, ExitRecord, Foreground, OnExit, Outcome, Relaunches, Restart, Say, Snapshot,
};
use aterm_agent::harness::upgrade_drive::{self, After, Behind, Due};
use aterm_agent::harness::upgrade_wake::{ActivationWake, WakeTrigger};
use aterm_agent::supervise::policy::turn_end::Reach;
use aterm_agent::supervise::{
    Ctl, CtlReply, Endpoint, Guard, HostStep, IdleHost, Interrupter, RelayCtl, Session,
    SuperviseOpts, SupervisorConfig,
};

use crate::harness_netprobe::NetProbe;

use aterm_phase::Program;

use crate::session_store::{SessionState, Store};

/// `cfg` with every key the engine does not read at its default: what the
/// host runs under and compares, so an edit that changes nothing the engine
/// reads restarts nothing. Those are the three `[harness]` keys the HOST
/// reads and the loop does not: `upgrade`, `relaunch` and `probe_api` — read
/// from the same parse before they are masked ([`Switches`], live in every
/// worker), so the table has one parser.
pub(crate) fn effective(cfg: &SupervisorConfig) -> SupervisorConfig {
    let d = SupervisorConfig::default();
    SupervisorConfig {
        upgrade: d.upgrade,
        relaunch: d.relaunch,
        probe_api: d.probe_api,
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
    probe_api: AtomicBool,
    /// A seamless update's park holds the terminal
    /// ([`HostHandle::pause_restored`] until [`HostHandle::resume_restored`]):
    /// no relaunch on exit is typed meanwhile, and the one a worker's back-off
    /// still owes is the handoff layout's to carry ([`Kept::owed`]).
    parked: AtomicBool,
}

impl Switches {
    fn park(&self, parked: bool) {
        self.parked.store(parked, Ordering::SeqCst);
    }

    fn parked(&self) -> bool {
        self.parked.load(Ordering::SeqCst)
    }

    fn set(&self, cfg: &SupervisorConfig) {
        self.upgrade
            .store(cfg.enabled && cfg.upgrade, Ordering::SeqCst);
        self.relaunch
            .store(cfg.enabled && cfg.relaunch, Ordering::SeqCst);
        self.probe_api
            .store(cfg.enabled && cfg.probe_api, Ordering::SeqCst);
    }

    fn probe_api(&self) -> bool {
        self.probe_api.load(Ordering::SeqCst)
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

/// A restored tab's relaunch ([`HostHandle::relaunch_restored`]) is usually
/// tried at most this many times — a shell that is still starting says `NotYet` —
/// pausing [`RESTORED_FIRST_PAUSE`] first, doubling up to
/// [`RESTORED_MAX_PAUSE`]. An already typed relaunch still waiting for its
/// conversation is watched until the in-flight record's stale horizon.
const RESTORED_TRIES: u32 = 8;
const RESTORED_FIRST_PAUSE: Duration = Duration::from_secs(1);
const RESTORED_MAX_PAUSE: Duration = Duration::from_secs(30);
/// Whether a restored tab's relaunch step that came to `outcome` (its word
/// `step`) is tried again ([`HostHandle::relaunch_restored`]): only a step
/// that was not possible YET — the shell still starting, the relaunch still
/// coming up, another actor on the lock. A relaunch that was typed and did
/// not take (`failed:`: the agent ended as it started, never picked its
/// conversation up, or its prompt refused the line) is not: another try
/// types the same line into the tab again (day six, D30: three times, the
/// row 14 minutes late), and the row says what to type instead.
fn restored_again(step: &str, outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::NotYet(_) | Outcome::Busy) && !step.starts_with("failed:")
}

/// The usual retry budget is eight steps. `wait:resume` means the line was
/// already typed and its process may still register its conversation: a
/// shorter per-step lock hold must not make that process appear lost before
/// the in-flight record itself becomes stale.
fn restored_retry(step: &str, outcome: &Outcome, tried: u32, waiting: Duration) -> bool {
    restored_again(step, outcome)
        && (tried < RESTORED_TRIES
            || (step == "wait:resume" && waiting < Duration::from_secs(relaunch::STALE_S)))
}

/// One agent a cold restore hands the host ([`HostHandle::relaunch_restored`]):
/// its new tab's sid, the snapshot naming that tab, its new shell and what the
/// layout carried of the agent, and where the tab is in the person's words
/// (`in tab 2`, `App::upgrade_place`) for the row that says it did not come
/// back.
#[derive(Clone)]
pub(crate) struct RestoredAgent {
    pub(crate) sid: String,
    pub(crate) snap: Snapshot,
    pub(crate) place: String,
}

/// One cold-restore relaunch's next eligible step. A slow tab must not make
/// every other restored tab wait through its own growing retry pauses.
struct RestoredAttempt {
    agent: RestoredAgent,
    order: usize,
    due: Instant,
    pause: Duration,
    tried: u32,
    resume_wait_since: Option<Instant>,
    /// The worker is running this attempt's step right now. It stays in the
    /// queue meanwhile, so a handoff that parks mid-step carries it
    /// ([`HostHandle::pause_restored`]): the step's outcome is not known yet,
    /// and the successor's step is idempotent against whatever this one did
    /// (the in-flight record, the conversation's live owner).
    acting: bool,
}

/// How long after the first cold-restore relaunch of this process the
/// automatic update holds off while agents are still queued
/// ([`HostHandle::restored_pending`]): the in-flight record's own stale
/// horizon — the longest a typed relaunch is waited on — plus two minutes for
/// the queue's own retries. A bound, not a condition: past it the automatic
/// update lands and the successor carries what is left
/// ([`HostHandle::pending_restored_for`]).
pub(crate) const RESTORED_HOLD: Duration = Duration::from_secs(relaunch::STALE_S + 120);

/// THE RESTORED AGENTS' QUEUE (round four of the 2026-09 update robustness
/// work, plan item 7), shared by its one worker thread
/// ([`HostHandle::relaunch_restored`]) and a seamless update: the queue
/// used to be the worker's local, so a handoff landing while it ran dropped
/// every agent still in it — after the reopened layout's row had said they
/// would resume. Taken alone, never under [`Shared::state`].
#[derive(Default)]
struct RestoredQueue {
    /// Every agent not relaunched, said missed or dropped yet — the one being
    /// stepped included ([`RestoredAttempt::acting`]).
    attempts: VecDeque<RestoredAttempt>,
    /// The next attempt's place in the layout's order.
    next_order: usize,
    /// The ones that did not come back, said in one row once the queue is
    /// done or paused.
    missed: Vec<(usize, String, Outcome)>,
    /// A handoff parked the readers: the worker takes no new step until a
    /// rollback resumes it ([`HostHandle::resume_restored`]).
    paused: bool,
    /// What the pause froze for the handoff layout, so the park's capture and
    /// the Commit's re-capture read the same set whatever the step in flight
    /// ends in (`commit_layout_topology` compares them).
    carried: Option<Vec<(String, Snapshot)>>,
    /// The first relaunch this process queued: [`RESTORED_HOLD`] runs from
    /// it, once per process — a later queue never extends it.
    since: Option<Instant>,
    /// The worker thread runs.
    running: bool,
}

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

/// The source of the stamp a worker's step leaves on its worker
/// ([`WorkerIdle::stepped`]): each is unique, so no worker's stamp is ever
/// another's step's.
static STEP_STAMPS: AtomicU64 = AtomicU64::new(0);

/// A worker took an upgrade's or a relaunch's step: the owner's view is owed
/// a look ([`UPGRADE_ACTS`]), and the host is woken to take it.
fn note_upgrade_act() {
    UPGRADE_ACTS.fetch_add(1, Ordering::SeqCst);
    ring();
}

/// The window learned the owner's view is stale — a press refused because
/// the round it was offered for has since stopped (day five, D22): the view
/// is looked at again at the host's next wake, as after a worker's step.
pub(crate) fn look_at_upgrades_soon() {
    note_upgrade_act();
}

/// Wake the host: something it decides from moved (the roster, a program, a
/// claim, a worker, the config). Cheap, never blocks on anything but the
/// bell's own leaf lock, so it is safe under the store's or a timeline's lock.
pub(crate) fn ring() {
    FULL_FOLLOW_EPOCH.fetch_add(1, Ordering::SeqCst);
    #[cfg(test)]
    RINGS_HERE.with(|n| n.set(n.get() + 1));
    ring_bell();
}

#[cfg(test)]
thread_local! {
    /// The rings ([`ring`]) this thread made: a test reads its own thread's,
    /// which no other test's ring moves (the bell is process-wide).
    static RINGS_HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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

/// [`HostHandle::relaunching`] over the host's per-session [`Kept`] map:
/// with `relaunches` (the one switch for every session), the sessions whose kept
/// snapshot the relaunch would plan ([`relaunch::resumes_on_exit`]). The
/// map's lock is released before any session's own is taken (both leaves).
fn relaunching_of(
    relaunches: bool,
    kept: &Mutex<HashMap<String, Arc<Mutex<Kept>>>>,
) -> std::collections::HashSet<String> {
    if !relaunches {
        return std::collections::HashSet::new();
    }
    let kept: Vec<(String, Arc<Mutex<Kept>>)> = kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(sid, k)| (sid.clone(), Arc::clone(k)))
        .collect();
    kept.into_iter()
        .filter(|(_, kept)| {
            kept.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .snapshot
                .as_ref()
                .is_some_and(relaunch::resumes_on_exit)
        })
        .map(|(sid, _)| sid)
        .collect()
}

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
    /// stopped it): the roster names it no longer, and it is not read to
    /// hold the tab ([`Acts::holds`] answers anything but `Some(true)`) — a
    /// name the roster could not read while the agent still holds its tab is
    /// no exit ([`keep_holding`]), but a hold that cannot be read keeps
    /// nothing, so where it never can (a ConPTY has no foreground group) the
    /// roster alone decides, as it did before the hold; and a keep ends at
    /// [`HOLDS_KEEP_MAX`], or when another group is named in its stead. Cleared
    /// if the agent is back. The worker then handles the exit
    /// ([`on_agent_left`]).
    pub(crate) left: Arc<AtomicBool>,
    /// Set while the worker takes an upgrade step: the agent it ends and
    /// relaunches is ITS act, so while the step runs the host neither stops
    /// it nor hands it the exit. A step that ended the agent and whose
    /// relaunch waited leaves the tab at its shell once it is over: that exit
    /// is handed, and carried as the harness's own restart's whatever
    /// `[harness] relaunch` says ([`Acts::restarted`], [`on_agent_left`]).
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
    /// The foreground group the host last saw the roster name the agent in
    /// ([`Held::stamp`]; `0`: none yet): the group [`Hooks::still_wanted`]
    /// asks about after a failed run the roster cannot name (the fact
    /// [`Acts::holds`] reads, through the worker's own seam).
    group: Arc<AtomicI32>,
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
    /// THE RELAUNCH ON EXIT THIS SESSION IS OWED while its back-off runs
    /// ([`relaunch_until`]): the agent's snapshot, for an exit the relaunch
    /// is decided on already — a crash (its record survived) or a stall's
    /// remedy — and `None` once the relaunch lands, is left or can never
    /// land. What a seamless update carries across its handoff for the tab
    /// ([`HostHandle::pending_restored_for`]; round six, F13): the back-off
    /// lived in the worker alone, the Commit's stop read as the agent being
    /// back, and neither the successor nor a failed Commit's resumed host
    /// ever relaunched the conversation.
    owed: Option<Snapshot>,
    /// [`Self::owed`] and the worker that owed it was stopped by a park's
    /// Commit ([`HostHandle::suspend`]): a Commit that fails leaves it to
    /// [`HostHandle::relaunch_stranded`].
    stranded: bool,
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
/// Whether a session is still its worker's ([`Hooks::still_wanted`]):
/// `(sid, group)`.
type StillFn = dyn Fn(&str, i32) -> bool + Send + Sync;
/// Whether a tab's agent still holds it ([`Acts::holds`]): `(sid, group)`.
type HoldsFn = dyn Fn(&str, i32) -> Option<bool> + Send + Sync;
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
/// Whether the harness's own restart ended the agent that left
/// ([`Acts::restarted`]): `(sid, codex, pid)`.
type RestartedFn = dyn Fn(&str, bool, Option<u32>) -> bool + Send + Sync;
/// One attempt at carrying that restart on ([`Acts::carry`]): `(sid, grace,
/// codex, pid)`.
type CarryFn = dyn Fn(&str, u32, bool, Option<u32>) -> String + Send + Sync;
/// The relaunch of an agent whose tab a crash of aterm took, in the tab the
/// next launch reopened ([`relaunch::after_host_ended`]): `(sid, snapshot,
/// upgrade)` to the step's word.
type RestoredFn = dyn Fn(&str, &Snapshot, bool) -> String + Send + Sync;
/// One step of a restart of tab `sid` that an outgoing instance left in
/// flight at a seamless update's Commit ([`HostHandle::resume_after_handoff`]):
/// `(sid, human_grace_s)` to the step's word, or `None` once the tab has no
/// restart in flight ([`upgrade_drive::in_flight`]) — and
/// [`CARRY_SUPERVISED`], without a step, while one is but the tab's agent is
/// still supervisable: its worker's to carry, looked at again later.
type CarryInFlightFn = dyn Fn(&str, u32) -> Option<String> + Send + Sync;

/// The word [`Acts::carry_in_flight`] answers for a tab whose restart is in
/// flight while its agent still reads supervisable: nothing was stepped, and
/// the tab is looked at again (round six, F17). Until then that tab was
/// dropped at its first look, for good: the adopted-claim grace
/// ([`ADOPTED_CLAIM_GRACE`]) holds its worker off at exactly that moment, the
/// SIGTERMed agent then exits, the tab is at its shell and no worker ever
/// starts — the conversation the harness itself ended was never relaunched.
const CARRY_SUPERVISED: &str = "wait:supervised";
/// The owner's view of the upgrades ([`Hooks::upgrade_view`]): `true` looks
/// again and answers what the next look waits for ([`ViewNext`]), `false`
/// stands it down.
type ViewFn = dyn Fn(bool) -> ViewNext + Send + Sync;

/// What a look at the owner's view says of the next one
/// ([`Hooks::upgrade_view`], [`look_at_upgrades`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ViewNext {
    /// The unix second a row next changes by time alone
    /// ([`upgrade_drive::View::refresh`]), if one does.
    pub(crate) at: Option<u64>,
    /// The tabs whose row follows its agent's live status
    /// ([`upgrade_drive::View::follows`]): looked at again whenever one of
    /// their screens moves.
    pub(crate) follows: Vec<String>,
    /// The tabs a record of which is past its watch
    /// ([`upgrade_drive::View::watch_due`]): handed to the watch
    /// ([`Acts::watch`], [`watch_upgrades`]).
    pub(crate) watch: Vec<String>,
}
type RestartFn = dyn Fn(&str, u32, &Restart) -> String + Send + Sync;
/// THE WATCH of one tab's upgrade records off any point
/// ([`upgrade_drive::watch`]): `(sid, human_grace_s, who and why)`.
type WatchFn = dyn Fn(&str, u32, &upgrade_drive::Watcher) -> upgrade_drive::Watched + Send + Sync;
/// An agent's route to its API, read now from its pid, argv and directory
/// ([`Acts::route`]).
type RouteFn = dyn Fn(u32, &[String], &str) -> Route + Send + Sync;
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
    /// Whether session `sid` is still its worker's after a failed run: the
    /// roster names its agent, or the agent still holds its tab in `group`,
    /// the group the host last saw it named in (the fact [`Acts::holds`]
    /// reads for the host thread, asked here through this seam) — a failure
    /// during a name the roster could not read restarts the loop on its
    /// back-off, as any other, instead of ending the worker and what it
    /// remembers of the session's upgrade.
    pub(crate) still_wanted: Arc<StillFn>,
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
    /// floor it reclaims the build caches where they work, idle ones first
    /// and then the least recently used until free space is back
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
    /// Whether tab `sid`'s agent still HOLDS it, which tells an agent that
    /// left from a name the roster could not read ([`keep_holding`]): the
    /// tab's foreground group, read from its terminal now, is `group` — the
    /// group the roster last named the agent in — that group's leader still
    /// exists (a zombie until it is reaped: [`holds`]), and no program read
    /// for it since is one that could not host the agent. `None`: it cannot
    /// be read (no group named, no terminal to ask), which decides nothing
    /// and is no reason to keep a worker. Asked by the host thread alone: the
    /// store's lock to clone the tab's handle, its timeline's lock, then —
    /// neither held — two system calls; no process spawned. A worker after a
    /// failed run asks the SAME fact through its own seam
    /// ([`Hooks::still_wanted`], live the free [`still_wanted`] over the free
    /// [`holds`] this hook calls too), never through this hook: the tests'
    /// fakes of the two seams read one flag, and nothing else ties them, so
    /// a change to one is a change to both.
    pub(crate) holds: Arc<HoldsFn>,
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
    /// Whether the harness's OWN restart ended the agent that left — the
    /// upgrade's, or the restart in place's, left in flight by its step
    /// ([`relaunch::restarted`]: a Codex's by its tab, a Claude Code's by the
    /// snapshot's pid) — read as the exit is seen, before whose exit it was.
    pub(crate) restarted: Arc<RestartedFn>,
    /// One attempt at carrying that restart on
    /// ([`relaunch::carry_restart`]).
    pub(crate) carry: Arc<CarryFn>,
    /// The relaunch of a restored tab's agent after aterm itself ended
    /// ([`HostHandle::relaunch_restored`]).
    pub(crate) relaunch_restored: Arc<RestoredFn>,
    /// One step of a restart the outgoing instance left in flight at a
    /// seamless update's Commit, while one is
    /// ([`HostHandle::resume_after_handoff`]; a seamless update is unix's).
    #[cfg_attr(not(any(unix, test)), allow(dead_code))]
    pub(crate) carry_in_flight: Arc<CarryInFlightFn>,
    pub(crate) restart: Arc<RestartFn>,
    /// Whether the conversation the session's agent holds has a task — a
    /// prompt of a person's or an orchestrator's; the harness's own turns
    /// are none ([`WorkerIdle::taskless`]).
    pub(crate) tasked: Arc<TaskedFn>,
    /// The live upgrade's clocks held through the loop's limit episode
    /// ([`upgrade_drive::hold_clock`], [`WorkerIdle::limited`]).
    pub(crate) hold: Arc<HoldFn>,
    /// The route a Claude Code agent's API requests take, read-only from
    /// its exec environment and every settings source Claude Code reads for
    /// it ([`crate::harness_netprobe::route_of_agent`]): only the default
    /// route is measured ([`WorkerIdle::reach`]).
    pub(crate) route: Arc<RouteFn>,
    /// THE WATCH (design record 2026-09-28, "No upgrade stuck forever",
    /// §3.2 C4): one look at a tab's upgrade records OFF ANY POINT
    /// ([`upgrade_drive::watch`]) — a dry-run read, and under the sweep
    /// lock's `try_lock` only the watch's own fields and the three
    /// hands-free changes (a retarget, a limit's clock hold, a re-arm).
    /// Nothing is typed, signalled or ended. Called on a watch thread of its
    /// own at Background QoS ([`watch_upgrades`]), never on the host thread.
    pub(crate) watch: Arc<WatchFn>,
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
fn failing_badge(why: &str, pause: Duration) -> String {
    let why: String = why.split_whitespace().collect::<Vec<_>>().join(" ");
    // The whole badge within the server's keyed-attention cap (200 bytes).
    let mut end = why.len().min(80);
    while !why.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "supervisor keeps failing; restarting in {} min; last: {}",
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
        if job.stop.load(Ordering::SeqCst)
            || !(hooks.still_wanted)(&job.sid, job.group.load(Ordering::SeqCst))
        {
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
            (hooks.badge)(&job.sid, Some(&failing_badge(&why, pause)));
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
    // What a relaunch on exit needs, read while the agent runs — a Codex's
    // too (its thread, its home, how it runs).
    refresh_snapshot(&job.sid, &job.kept, hooks);
    // The conversation is named a moment after the agent starts — Claude
    // Code's record, a Codex's thread lock or the thread its daemon begins
    // for it (measured: a Codex attached-to reads none yet) — so the first
    // idle point reads it again.
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

/// Whether the relaunch on exit is written for `agent` ([`on_agent_left`]):
/// Claude Code's, and — its Codex lane, the harness's Codex parity
/// (2026-09-27) — Codex's.
fn relaunches(agent: Program) -> bool {
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
    /// An ask is in flight ([`ask_note`]): taken under the lock before the
    /// act, so a second asker — the worker's attach or idle point racing the
    /// host's wake — skips it instead of noting the session twice; put back
    /// (or cleared) under the lock once the act has said.
    flying: bool,
    /// An asker skipped it in flight: one put back rings the host, which
    /// then wakes for its next ask.
    skipped: bool,
    /// A notice owed it again in flight ([`owe_note`]): the ask in flight
    /// may have read the builds before that notice, so what it says is not
    /// kept — the note stays owed as the notice left it, and the host is
    /// rung to ask it.
    reowed: bool,
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
/// when one is owed already — to be asked at once; re-owed
/// ([`Owed::reowed`]) while an ask has it in flight.
fn owe_note(note: &Mutex<Option<Owed>>, since: u64) {
    let mut owed = note.lock().unwrap_or_else(PoisonError::into_inner);
    let since = owed.map_or(since, |o| o.since.min(since));
    let (flying, skipped) = owed.map_or((false, false), |o| (o.flying, o.skipped));
    *owed = Some(Owed {
        since,
        unread: 0,
        asked: None,
        by: Some(Instant::now()),
        flying,
        skipped,
        reowed: flying,
    });
}

/// An owed note's ask lands ([`Owed::flying`] cleared): whether the host is
/// to be rung for it — an asker skipped it in flight, or a notice re-owed
/// it — and whether it was re-owed.
fn land(owed: &mut Option<Owed>) -> (bool, bool) {
    owed.as_mut().map_or((false, false), |o| {
        o.flying = false;
        let reowed = std::mem::take(&mut o.reowed);
        (std::mem::take(&mut o.skipped) || reowed, reowed)
    })
}

/// An ask in flight ([`ask_note`]) that has not landed: an act that panics
/// lands it as it unwinds — the worker's body is restarted on this same
/// note, whose later asks would otherwise all skip it — left owed as it
/// stood, and the host rung if it had skipped it.
struct InFlight<'a>(Option<&'a Mutex<Option<Owed>>>);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let Some(note) = self.0.take() else {
            return;
        };
        let (rings, _) = land(&mut note.lock().unwrap_or_else(PoisonError::into_inner));
        if rings {
            ring();
        }
    }
}

/// Ask the session's owed note once ([`Acts::behind`], with no lock held
/// over the ask) and keep what it decides: nothing more owed once it noted
/// the session behind — the owner's view looked at again — or found nothing
/// to note; asked again after [`relaunch::BUSY`] while another actor holds
/// the state's lock, and on [`NOTE_AGAIN`] while the session cannot be read.
/// What it said; `None` when no note is owed, or another asker has it in
/// flight ([`Owed::flying`]).
fn ask_note(sid: &str, note: &Mutex<Option<Owed>>, hooks: &Hooks) -> Option<Behind> {
    let lock = || note.lock().unwrap_or_else(PoisonError::into_inner);
    let since = {
        let mut owed = lock();
        let o = owed.as_mut()?;
        if o.flying {
            o.skipped = true;
            return None;
        }
        o.flying = true;
        o.since
    };
    let mut flight = InFlight(Some(note));
    let said = (hooks.acts.behind)(sid, since);
    flight.0 = None;
    let now = Instant::now();
    let mut owed = lock();
    let (rings, reowed) = land(&mut owed);
    match (said, owed.as_mut()) {
        // Re-owed in flight: owed as the notice left it.
        _ if reowed => {}
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
    } else if rings && (reowed || matches!(said, Behind::Busy | Behind::Unread(_))) {
        ring();
    }
    Some(said)
}

/// The host thread's ask of a worker's owed note at a wake ([`Owed`]):
/// asked once its instant has come, or — while it could not read — once the
/// host wakes [`NOTE_GAP`] or more after the last ask. The instant the host
/// is to wake for it, if any: none while another asker has it in flight
/// (that one rings the host as it puts it back).
fn host_note(
    sid: &str,
    note: &Mutex<Option<Owed>>,
    now: Instant,
    hooks: &Hooks,
) -> Option<Instant> {
    let asks = {
        let mut owed = note.lock().unwrap_or_else(PoisonError::into_inner);
        let o = owed.as_mut()?;
        let by = o.by?;
        if o.flying {
            o.skipped = true;
            return None;
        }
        by <= now
            || (o.unread > 0
                && o.asked
                    .is_some_and(|at| now >= at + (hooks.pause)(NOTE_GAP)))
    };
    if asks {
        ask_note(sid, note, hooks);
    }
    host_wake(note)
}

/// The instant the host is to wake for a worker's owed note ([`host_note`]):
/// none while another asker has it in flight — marked skipped
/// ([`Owed::skipped`]), so that asker rings the host as it puts it back.
fn host_wake(note: &Mutex<Option<Owed>>) -> Option<Instant> {
    let mut owed = note.lock().unwrap_or_else(PoisonError::into_inner);
    let o = owed.as_mut()?;
    if o.flying {
        o.skipped = true;
        return None;
    }
    o.by
}

/// One worker's upgrade, between its steps.
#[derive(Default)]
struct UpgradeRun {
    /// The SETTLE AND DRAIN waits since the upgrade's last act
    /// ([`upgrade_drive::bounded_wait`]): what bounds how long the upgrade owns
    /// the session's turn ends after a settle or a drain
    /// ([`upgrade_drive::owns_turn_ends`]) — never after a person's hold
    /// ([`Self::attended`]). Every act starts it over, at an idle point or at a
    /// break ([`Self::acted`]); a turn of the agent's never does (68c98fd18),
    /// and no other wait moves it (2026-09-28, s-5c03a: every wait counted, and
    /// the notices typed at breaks started nothing over, so the READY's first
    /// look read six waits of a person's and the notice's and owned nothing —
    /// the loop's `keep going` voided the READY one second later).
    waits: u32,
    /// The ATTENDED LOOKS in a row at this point (`held-back:attended`,
    /// `wait:attended`: [`upgrade_drive::attended`]): what bounds how long a
    /// person's hold owns the session's turn ends
    /// ([`upgrade_drive::attended_owned`]) and how soon it is looked at again
    /// ([`upgrade_drive::after_attended`]). Its own count (the review of
    /// 2026-09-28: fed [`Self::waits`], which carries every wait since the last
    /// act — the notice's `awaiting-ready` turn ends among them — a READY a
    /// person held owned the point for about 19 minutes of their presence after
    /// 40 such waits, and for none of it after 97, and the loop's `keep going`
    /// took the grace's lapse over the READY again). Started over by any other
    /// step, and at a new point ([`Self::new_point`]): the READY comes with a
    /// turn of the agent's, so a person who held the re-ask before it leaves
    /// the READY's point to the upgrade all the same.
    attended: u32,
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
    /// The NOTICES ITS OWN FENCE REFUSED since the upgrade's last act or the
    /// owner's last word ([`upgrade_drive::refused`]): what bounds how long a
    /// refused notice owns the session's turn ends
    /// ([`upgrade_drive::OWNED_REFUSED_LOOKS`]) — its own count, never the
    /// settle's (the review of 2026-09-28: fed [`Self::waits`], a round whose
    /// notice gate had settled three looks owned nothing at its first refusal,
    /// and after the ninety minutes of refusals the band tells the owner of,
    /// an `Upgrade now` pressed there owned nothing either — the loop's `keep
    /// going` took the point, as on 0.98). A turn of the agent's never starts
    /// it over (68c98fd18); an act does ([`Self::acted`]), and so does a word
    /// ([`Self::worded`]).
    refused: u32,
    /// The host's activation notices and the owner's words this worker's
    /// counts were last started over for ([`Self::worded`],
    /// [`WorkerIdle::activations`]).
    activations: u64,
    /// The step words [`upgrade_drive::after`] was never taught
    /// ([`upgrade_drive::unknown_step`]) that this worker has warned of: each
    /// is said ONCE (design record 2026-09-28, §3.2 C8), not at every look.
    /// Bounded by [`UNKNOWN_WORDS_WARNED`].
    warned: Vec<String>,
}

/// How many distinct unknown step words one worker warns of: past it, a word
/// is looked at again all the same, and not said.
const UNKNOWN_WORDS_WARNED: usize = 8;

impl UpgradeRun {
    /// Whether the step that said `step` leaves the upgrade owning the
    /// session's turn ends ([`upgrade_drive::owns_turn_ends`]), read with the
    /// count its word is bounded by: a person's hold with its attended looks at
    /// this point, a refused notice with its refused looks ([`Self::refused`]),
    /// every other word with its settle and drain waits. Asked before
    /// [`Self::after`] counts the step.
    fn owns(&self, step: &str, grace_s: u32) -> bool {
        let looks = if upgrade_drive::attended(step) {
            self.attended
        } else if upgrade_drive::refused(step) {
            self.refused
        } else {
            self.waits
        };
        upgrade_drive::owns_turn_ends(step, looks, grace_s)
    }

    /// Whether `step` is the SAME person's hold as the step before it at this
    /// point (`wait:attended` again, no turn between): the journal carries the
    /// first look, and every one after it would be a row every twenty seconds
    /// for as long as the person stays ([`HostStep::repeat`]). Asked before
    /// [`Self::after`] counts the step.
    fn repeats(&self, step: &str) -> bool {
        upgrade_drive::attended(step) && self.attended > 0 && self.last == step
    }

    /// [`upgrade_drive::after`] for one step's word, the counts kept: the
    /// pause climbs only while the step waits on the same word — and a
    /// person's hold is looked at again at the first rung while it owns the
    /// point ([`upgrade_drive::after_attended`], `grace_s` the person's grace).
    fn after(&mut self, step: &str, grace_s: u32) -> After {
        if self.last != step {
            self.same = 0;
        }
        let attended = upgrade_drive::attended(step);
        self.attended = if attended {
            self.attended.saturating_add(1)
        } else {
            0
        };
        let after = if attended {
            upgrade_drive::after_attended(self.attended, self.same, grace_s)
        } else if upgrade_drive::refused(step) {
            // A notice its fence refused: looked at again at the first rung
            // while the point is still the upgrade's, so it is tried again
            // there (`refused` is still the count `owns` read).
            upgrade_drive::after_refused(self.refused, self.same)
        } else {
            upgrade_drive::after(step, self.same)
        };
        if upgrade_drive::unknown_step(step)
            && self.warned.len() < UNKNOWN_WORDS_WARNED
            && !self.warned.iter().any(|w| w == step)
        {
            self.warned.push(step.to_string());
            aterm_log::warn!(
                "harness: the live upgrade's step said `{step}`, a word the host was never \
                 taught; it is looked at again later rather than taken as the last word"
            );
        }
        // Only a SETTLE OR A DRAIN counts toward the bounded hold on turn ends
        // ([`upgrade_drive::bounded_wait`]), and a refused notice toward its
        // own ([`Self::refused`]); any other wait leaves both as they stand. A
        // step that acted and then looks again later (a re-arm, a stop resting
        // until its next round) starts every count over, as an act does: a new
        // round that inherited the rest's two hours of looks would own no turn
        // end, its twenty-second settle would never complete, and its first
        // notice would never be typed (the no-stall review of 2026-09-27, B3).
        if matches!(after, After::Later(_)) && step.starts_with("wait:") {
            if upgrade_drive::refused(step) {
                self.refused = self.refused.saturating_add(1);
            } else if upgrade_drive::bounded_wait(step) {
                self.waits = self.waits.saturating_add(1);
            }
            self.same = self.same.saturating_add(1);
            step.clone_into(&mut self.last);
        } else {
            self.waits = 0;
            self.refused = 0;
            self.same = 0;
            self.last.clear();
        }
        after
    }

    /// AN ACT OF THE UPGRADE'S AT A BREAK of the agent's own background work
    /// ([`WorkerIdle::at_background`]: a notice, a release, a give-up, a
    /// re-arm): every count starts over, as at an idle point — the point it
    /// asks the agent for (its READY, the end of its work) is the upgrade's to
    /// settle. A wait said at a break counts nothing: the break's looks are
    /// paced by the loop ([`BACKGROUND_LOOK`]), not by this ladder. Until
    /// 2026-09-28 nothing typed at a break went through these counts (s-5c03a:
    /// three notices at breaks, and the READY after them owned nothing).
    fn acted(&mut self) {
        self.waits = 0;
        self.refused = 0;
        self.same = 0;
        self.last.clear();
        self.attended = 0;
    }

    /// A WORD SINCE THE LAST STEP: the owner's (`aterm harness upgrade <sid>
    /// --now|--defer|--skip`, a band or menu press), or a newer build
    /// installed — both reach the worker as the host's activation notice
    /// ([`WorkerIdle::activations`]). The owner's word arms a new round of the
    /// upgrade (`upgrade_drive::St::new_round`), and a newer build moves its
    /// target: the counts that bound how long the upgrade owns the session's
    /// turn ends start over, and so does the ladder (the review of
    /// 2026-09-28: after the ninety minutes of refusals the band offers
    /// `Upgrade now` for, the refused notice the press asked for owned nothing
    /// — its looks were spent before the word — and the loop's `keep going`
    /// took the point). A person's looks at this point stand: they are the
    /// person's, not the round's. Never the agent's doing, so no self-waking
    /// agent renews anything by it (68c98fd18).
    fn worded(&mut self) {
        self.waits = 0;
        self.refused = 0;
        self.same = 0;
        self.last.clear();
    }

    /// A turn ran since the last look: the ladder starts over at the new
    /// point, and so do a person's looks ([`Self::attended`]); the waits that
    /// bound the upgrade's hold on the turn ends after a settle or a drain are
    /// kept (a self-waking agent must not renew those).
    fn new_point(&mut self) {
        self.same = 0;
        self.last.clear();
        self.attended = 0;
    }

    /// Nothing to step: every count starts over.
    fn reset(&mut self) {
        self.waits = 0;
        self.refused = 0;
        self.same = 0;
        self.last.clear();
        self.attended = 0;
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

/// WHAT THE WORKER'S LOOP LAST OFFERED ITS HOST, AND WHAT IT WITHHELD
/// (design record 2026-09-28, "No upgrade stuck forever", §3.2 C5): the unix
/// second of the last point offered — an idle point ([`IdleHost::at_idle`])
/// or a settled break ([`IdleHost::at_background`]) — and the loop guard that
/// withheld the latest point the host asked for ([`IdleHost::withheld`]),
/// with its second. Atomics, so the watch that copies them onto the
/// upgrade's record (rollout step 7) reads them off its own thread without a
/// lock the loop could hold. Reporting only: nothing here decides a step.
#[derive(Debug, Default)]
pub(crate) struct Points {
    /// [`Guard::code`] of the last guard said; `0`: none since the worker began.
    guard: AtomicU8,
    guard_at: AtomicU64,
    point_at: AtomicU64,
}

impl Points {
    /// The loop withheld a point by `guard` at `now`.
    fn withheld(&self, guard: Guard, now: u64) {
        self.guard_at.store(now, Ordering::SeqCst);
        self.guard.store(guard.code(), Ordering::SeqCst);
    }

    /// The loop offered a point at `now`.
    fn offered(&self, now: u64) {
        self.point_at.store(now, Ordering::SeqCst);
    }

    /// The last guard said and its second (`None`: none), and the last point
    /// offered (`0`: none).
    pub(crate) fn last(&self) -> (Option<(Guard, u64)>, u64) {
        let guard = Guard::from_code(self.guard.load(Ordering::SeqCst))
            .map(|g| (g, self.guard_at.load(Ordering::SeqCst)));
        (guard, self.point_at.load(Ordering::SeqCst))
    }
}

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
    /// The stamp of the last step this worker took that may end its agent
    /// ([`Self::act`]; `0`: none, or the one its predecessor's keep began
    /// at): a kept agent's exit is inferred only while it has not moved
    /// ([`keep_holding`]).
    stepped: Arc<AtomicU64>,
    stalled: Arc<AtomicBool>,
    switches: Arc<Switches>,
    kept: Arc<Mutex<Kept>>,
    hooks: Hooks,
    run: Mutex<UpgradeRun>,
    /// The host's count of activation notices and the owner's words
    /// ([`HostHandle::note_activation`]): one moved since the last step starts
    /// the upgrade's counts over ([`UpgradeRun::worded`]).
    activations: Arc<AtomicU64>,
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
    /// The instance's one API reach probe ([`NetProbe`]).
    net: Arc<NetProbe>,
    /// The agent's route as last read, by its pid, kernel start and working
    /// directory: read once per agent process ([`Acts::route`]), and again
    /// when the directory it was read with changes — a snapshot taken before
    /// the agent's cwd was known reads its route custom, and the relaunch's
    /// `follow` filling the cwd in later must be read, not the cached custom.
    route: Mutex<Option<(u32, String, String, Route)>>,
    /// What the loop last offered and withheld ([`Points`]).
    points: Arc<Points>,
}

impl std::fmt::Debug for WorkerIdle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (guard, point_at) = self.points.last();
        f.debug_struct("WorkerIdle")
            .field("sid", &self.sid)
            .field("park", &self.park)
            .field("owns", &self.owns)
            .field("guard", &guard.map(|(g, at)| (g.word(), at)))
            .field("point_at", &point_at)
            .finish_non_exhaustive()
    }
}

impl IdleHost for WorkerIdle {
    fn wants(&self) -> bool {
        self.park.load(Ordering::SeqCst)
    }

    /// A supervisor's information for a person (Codex's save-then-wait hold
    /// among them): a record in the window's Messages, no row, no badge.
    fn inform(&self, text: &str) {
        crate::message_inbox::queue_message(crate::message_reporters::supervisor_note(
            &self.sid, text,
        ));
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
        self.points.offered(crate::upgrade_host::now_s());
        self.park.store(false, Ordering::SeqCst);
        let step = self.idle_step();
        // A conversation not named yet (a Codex daemon client's thread has
        // no rollout before its first message) is read again at the next
        // idle point: known while the agent runs, its relaunch never depends
        // on its daemon still holding the thread at the exit (a daemon the
        // upgrade restarts meanwhile drops a dead client's thread).
        if relaunches(self.agent)
            && with_kept_of(&self.kept, |k| {
                k.snapshot.as_ref().is_some_and(|s| s.session.is_none())
            })
        {
            self.park.store(true, Ordering::SeqCst);
        }
        step
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
    /// releases them ([`upgrade_drive::released_at_break`]). So does the
    /// RELEASE LINE a give-up, a void or a stop owes, typed here as at an
    /// idle point (2026-09-27: a break that never ended is the only point
    /// such a session has): the agent is told to go on. Every turn typed here
    /// is said to the loop ([`upgrade_drive::typed`]), which journals it and
    /// awaits its answer as the harness's own — never the worker's work. The
    /// restart and a carry-on stay the next idle point's (the park is left
    /// set for it). A break that typed nothing is looked at again after
    /// [`BACKGROUND_LOOK`].
    fn at_background(&self) -> Option<String> {
        self.points.offered(crate::upgrade_host::now_s());
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
        self.act();
        let step = (hooks.acts.notice)(&self.sid, self.grace);
        self.acting.store(false, Ordering::SeqCst);
        // An ACT at a break starts the counts over, as at an idle point: the
        // READY it asks for is the upgrade's point to settle.
        if !step.starts_with("wait:") && !step.starts_with("busy:") {
            self.run
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .acted();
        }
        // What the owner sees moves with the step, typed or waiting.
        note_upgrade_act();
        if upgrade_drive::released_at_break(&step) {
            self.owns.store(false, Ordering::SeqCst);
        }
        if !upgrade_drive::typed(&step) {
            return None;
        }
        // Typed: the answer and the restart are the idle point's — a later
        // break has nothing to add before the pause. A notice's answer is the
        // upgrade's; a release's is the agent going on.
        if step.starts_with("announced:") {
            self.owns.store(true, Ordering::SeqCst);
        }
        Some(format!("upgrade step={step}"))
    }

    /// The loop withheld the point this worker asked for ([`Points`]): kept,
    /// with its second, for the watch to record. Nothing else.
    fn withheld(&self, guard: Guard) {
        self.points.withheld(guard, crate::upgrade_host::now_s());
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
        self.act();
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

    /// An agent the stall's remedy ends is relaunched here ([`on_agent_left`],
    /// the exit read as the remedy's, U1) — the agents the relaunch on exit
    /// is written for, under `[harness] relaunch` — so the loop may take the
    /// remedy itself once the stall has stood past its bound.
    fn relaunches_after_stall(&self) -> bool {
        relaunches(self.agent) && self.switches.relaunch()
    }

    /// Whether an end of the agent now would be followed by this host's
    /// relaunch on its conversation ([`IdleHost::relaunches_on_exit`]): it
    /// can restart it ([`Self::can_restart`]), and the kept snapshot is one
    /// the relaunch would plan ([`relaunch::resumes_on_exit`]). One leaf
    /// lock, no I/O: the shell's dialect was read with the snapshot.
    fn relaunches_on_exit(&self) -> bool {
        self.can_restart()
            && self
                .kept
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .snapshot
                .as_ref()
                .is_some_and(relaunch::resumes_on_exit)
    }

    /// The line a person runs to resume this session's agent on its OWN
    /// conversation — `claude --resume <id>` with its launch flags
    /// ([`aterm_agent::harness::resume::command`]) — from the relaunch
    /// snapshot the host keeps of it (its argv and the conversation Claude
    /// Code's own record names, FOLLOWED through an in-app `/clear`), for the
    /// loop's frozen mail and memory escalation where the host does not
    /// relaunch it itself. `None` for an agent the relaunch is not written
    /// for, or before a snapshot was read. Never `claude --continue`
    /// (robustness backlog item 2, 2026-09-26): it resumes the directory's
    /// newest conversation — a sibling tab's where two share it.
    fn resume_command(&self) -> Option<String> {
        if self.agent != Program::Claude {
            return None;
        }
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let snap = kept.snapshot.as_ref()?;
        aterm_agent::harness::resume::command(&snap.argv, snap.session.as_deref()?)
    }

    /// The loop holds for a stall the server published, or it lifted: kept
    /// for [`on_agent_left`] — an exit while it is held was the stall's
    /// remedy's (U1).
    fn stalled(&self, held: bool) {
        self.stalled.store(held, Ordering::SeqCst);
    }

    /// WHAT THE HOST MEASURES OF THE AGENT'S ROUTE TO ITS API (the loop
    /// asks only while it waits at a wall the network answers): a Claude
    /// Code on the default route asks the instance's one probe
    /// ([`NetProbe::ask`], its lease renewed); any other route — a base URL,
    /// a cloud provider, a unix socket, a proxy, a settings source that
    /// could not be read — any other agent, an agent not snapshotted yet,
    /// and `[harness] probe_api = false` (read live) are measured by
    /// nothing: [`Reach::Unknown`], the time ladder — and so is an agent
    /// whose working directory is not known yet (its project settings are
    /// unseen). The route is read once per agent process (its pid and kernel
    /// start) and directory — again when the directory is filled in —
    /// outside every lock.
    fn reach(&self) -> Reach {
        if self.agent != Program::Claude || !self.switches.probe_api() {
            return Reach::Unknown;
        }
        let agent = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .as_ref()
            .map(|s| (s.pid, s.start.clone(), s.argv.clone(), s.cwd.clone()));
        let Some((pid, start, argv, cwd)) = agent else {
            return Reach::Unknown;
        };
        let cached = self
            .route
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|(p, s, c, _)| *p == pid && *s == start && *c == cwd)
            .map(|(_, _, _, route)| route.clone());
        let route = match cached {
            Some(route) => route,
            None => {
                let route = (self.hooks.acts.route)(pid, &argv, &cwd);
                if let Route::Custom(why) = &route {
                    aterm_log::info!(
                        "harness @{}: the API's reach is not measured for pid {pid}: {why}",
                        self.sid
                    );
                }
                *self.route.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((pid, start, cwd, route.clone()));
                route
            }
        };
        match route {
            Route::Default => self.net.ask(&self.sid),
            Route::Custom(_) => Reach::Unknown,
        }
    }

    /// A TURN RAN since the loop's last point ([`IdleHost::turn_ran`]): the
    /// pause the upgrade climbed was of a point that is gone, so its ladder
    /// starts over ([`UpgradeRun::new_point`]) and the new point is looked at
    /// as it comes, its first pause the first rung again (D3 of the live E2E
    /// of 2026-09-26: the settle's pause climbed across the Stage-1 turn, and
    /// the turn end after it was looked at +0.3 s, +61 s and +366 s). Its
    /// waits in a row still count: they bound how long it owns the session's
    /// turn ends after a settle or a drain ([`upgrade_drive::owns_turn_ends`]),
    /// which a self-waking agent must not renew. A person's looks start over
    /// (the review of 2026-09-28): theirs are counted at one point, and the
    /// READY answer is a turn of its own. A step that did not wait left no
    /// pause to start over.
    fn turn_ran(&self) {
        let mut run = self.run.lock().unwrap_or_else(PoisonError::into_inner);
        let climbed = run.same > 0;
        run.new_point();
        if !climbed {
            return;
        }
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
    /// THE HOST'S STEP at an idle point ([`IdleHost::at_idle`], which parks
    /// again after it while the conversation is unnamed): the relaunch
    /// record read again, then the upgrade's step or a relaunched agent's
    /// owed carry-on.
    fn idle_step(&self) -> Option<HostStep> {
        if !upgrades(self.agent) {
            return None;
        }
        let hooks = &self.hooks;
        let relaunched = relaunches(self.agent);
        if relaunched {
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
        // The owner's word, or a newer build, since the last step: the
        // upgrade's round is new, and so are its counts.
        let activations = self.activations.load(Ordering::SeqCst);
        if run.activations != activations {
            run.activations = activations;
            run.worded();
        }
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
                    self.act();
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
                        repeat: false,
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
        self.owns
            .store(run.owns(&step, self.grace), Ordering::SeqCst);
        let repeat = run.repeats(&step);
        // The host decides again what the session is now (its agent may have
        // been ended and relaunched by the step), and what the owner sees.
        note_upgrade_act();
        if relaunched {
            refresh_snapshot(&self.sid, &self.kept, hooks);
        }
        match run.after(&step, self.grace) {
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
            repeat,
        })
    }

    /// A step that may end the agent begins ([`WorkerJob::acting`]), its
    /// stamp left first ([`Self::stepped`]): a pass that finds the step over
    /// has seen the stamp move.
    fn act(&self) {
        self.stepped.store(
            STEP_STAMPS.fetch_add(1, Ordering::SeqCst) + 1,
            Ordering::SeqCst,
        );
        self.acting.store(true, Ordering::SeqCst);
    }

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
        Some(mut snap) => {
            let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
            keep_session(kept.snapshot.as_ref(), &mut snap);
            publish_snapshot(sid, Some(&snap));
            kept.snapshot = Some(snap);
        }
        None => {
            follow_snapshot(sid, kept, hooks);
        }
    }
}

/// A read `snap` that cannot name the conversation (a Codex's daemon
/// restarted since, holding its thread no more) keeps what the earlier read
/// `before` of the SAME process named — unless it could not tell the thread
/// apart (`CodexRun::ambiguous`: two begun since it started, the review of
/// 2026-09-27): the earlier name may be another client's, and a crash would
/// resume that one in this tab.
fn keep_session(before: Option<&Snapshot>, snap: &mut Snapshot) {
    if snap.session.is_none()
        && !snap.codex.as_ref().is_some_and(|run| run.ambiguous)
        && let Some(before) = before
        && (before.pid, before.start.as_str()) == (snap.pid, snap.start.as_str())
    {
        snap.session.clone_from(&before.session);
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

/// [`publish_snapshot`], for a test of what reads it.
#[cfg(test)]
pub(crate) fn publish_snapshot_for_test(sid: &str, snap: Option<&Snapshot>) {
    publish_snapshot(sid, snap);
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
    with_kept_of(&job.kept, f)
}

/// [`with_kept`] over the [`Kept`] itself.
fn with_kept_of<R>(kept: &Mutex<Kept>, f: impl FnOnce(&mut Kept) -> R) -> R {
    let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut kept)
}

/// RELAUNCH ON EXIT: the session's agent left a tab that lives on. Whose
/// exit it was decides ([`relaunch::on_exit`]): the harness's OWN restart's
/// first — the upgrade's, or the restart in place's, left in flight by its
/// step ([`Acts::restarted`]; S0 and S3 of the in-flight review, 2026-09-27)
/// — which is carried whoever is at the tab and whatever the owner's switch
/// says ([`OnExit::Restarted`]); a person's (a keystroke within the grace)
/// or a holder's (a halt, a lease, a named driver's turn) is theirs, and the
/// tab is left to them; one the owner limited — `[harness] relaunch =
/// false` — is said once on the session's attention; any other (a Claude
/// Code's, a Codex's) is relaunched on its conversation after the session's
/// back-off, cut short when the agent is back or the worker is stopped for
/// good, and asked about again before each try — trying again on the growing
/// pause until it lands, until the exit proves to be the launch's own end
/// (journaled, nothing said), or until it can never land (said).
fn on_agent_left(job: &WorkerJob, hooks: &Hooks) {
    let sid = job.sid.as_str();
    let grace = job.opts.policy.human_grace_s;
    // Its loop held for a stall when it left: the stall's remedy ended it
    // (U1). Taken: the next agent's stall is its own.
    let stalled = job.stalled.swap(false, Ordering::SeqCst);
    let snap = with_kept(job, |k| k.snapshot.clone());
    // THE HARNESS'S OWN RESTART ENDED IT, read before whose exit it was: the
    // step that signalled it returned with its relaunch still to type (a
    // person at the returned prompt, a hold, a shell slow to take the
    // terminal back), and no step watches the tab once its agent is gone.
    // Asked as a person's exit, a holder's or a limited one, the agent the
    // upgrade ended was never brought back — the owner who pressed Upgrade
    // now had typed within the grace. A Codex's restart is found by its tab
    // (its record is filed per tab), a Claude Code's by the snapshot's pid.
    let codex = job.agent == Program::Codex;
    let pid = snap.as_ref().map(|s| s.pid);
    let restarted = (hooks.acts.restarted)(sid, codex, pid);
    let decide = || {
        let allowed = relaunches(job.agent) && job.switches.relaunch();
        relaunch::on_exit(
            allowed,
            (hooks.acts.status)(sid).as_deref(),
            grace,
            stalled,
            restarted,
        )
    };
    let decision = decide();
    let pause = match with_kept(job, |k| k.relaunches.exited(decision, Instant::now())) {
        Ok(pause) => pause,
        Err(said) => {
            forget_snapshot(job);
            left_alone(job, hooks, decision, said);
            return;
        }
    };
    if restarted {
        aterm_log::info!(
            "harness @{sid}: the agent exited; the harness's own restart ended it, and its \
             relaunch is carried"
        );
        relaunch_until(job, hooks, pause, &decide, true, || {
            (hooks.acts.carry)(sid, grace, codex, pid)
        });
        return;
    }
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
        "harness @{sid}: the agent exited; read at the exit: {}",
        left.word()
    );
    // An exit the relaunch is decided on already — a crash, a stall's remedy
    // — is OWED through the back-off: a handoff carries it (round six, F13).
    if stalled || matches!(left, relaunch::ExitRecord::Survived(_)) {
        with_kept(job, |k| {
            k.owed = Some(snap.clone());
            k.stranded = false;
        });
    }
    // On the build the upgrade would move it to only while `[harness]
    // upgrade` allows (read live, as the relaunch switch is).
    relaunch_until(job, hooks, pause, &decide, false, || {
        (hooks.acts.relaunch)(sid, grace, &snap, &left, job.switches.upgrade(), stalled)
    });
}

/// THE RELAUNCH'S ATTEMPTS after an agent left ([`on_agent_left`]): each
/// after its pause — cut short when the agent is back or the worker is
/// stopped for good — asked about again ([`relaunch::on_exit`], `decide`),
/// then `attempt`ed, until it lands, is left, or can never land. `restarted`:
/// the harness's own restart is carried ([`OnExit::Restarted`]), never paused
/// longer than [`relaunch::CARRY_EVERY`] — the back-off's ten minutes
/// outlasted its record, which expired between two attempts — and said in
/// the restart's words.
fn relaunch_until(
    job: &WorkerJob,
    hooks: &Hooks,
    mut pause: Duration,
    decide: &dyn Fn() -> OnExit,
    restarted: bool,
    mut attempt: impl FnMut() -> String,
) {
    let sid = job.sid.as_str();
    loop {
        let wait = if restarted {
            pause.min(relaunch::CARRY_EVERY)
        } else {
            pause
        };
        if wait_for(Instant::now() + (hooks.pause)(wait), || {
            !job.left.load(Ordering::SeqCst)
        }) {
            // A PARK'S COMMIT STOPPED THE WORKER, not the agent coming back
            // (round six, F13): the stop clears `left` as the agent's return
            // does. What the back-off owes stays owed — the handoff layout
            // carried it to the successor, and a Commit that fails hands it
            // to this host again ([`HostHandle::relaunch_stranded`]).
            if job.stop.load(Ordering::SeqCst) && job.switches.parked() {
                with_kept(job, |k| k.stranded = k.owed.is_some());
                return;
            }
            settled(job);
            if (hooks.still_wanted)(sid, job.group.load(Ordering::SeqCst)) {
                let said = with_kept(job, |k| k.relaunches.running());
                tell(hooks, sid, said, "", restarted);
            }
            return;
        }
        // Nothing is typed while a handoff has the terminal parked: looked
        // at again shortly, and a Commit's stop ends the wait above.
        if job.switches.parked() {
            pause = RESTORED_FIRST_PAUSE;
            continue;
        }
        let decision = decide();
        if let Some(said) = with_kept(job, |k| k.relaunches.decide(decision)) {
            settled(job);
            forget_snapshot(job);
            left_alone(job, hooks, decision, said);
            return;
        }
        let step = attempt();
        note_upgrade_act();
        let outcome = relaunch::outcome(&step);
        let said = with_kept(job, |k| k.relaunches.attempted(&outcome, Instant::now()));
        tell(hooks, sid, said, &step, restarted);
        if !matches!(outcome, Outcome::NotYet(_) | Outcome::Busy) {
            settled(job);
        }
        match outcome {
            Outcome::NotYet(_) => pause = with_kept(job, |k| k.relaunches.pause()),
            Outcome::Busy => pause = relaunch::BUSY,
            Outcome::Relaunched => {
                // What a relaunch of the NEW agent needs, read while it runs
                // (if it is already gone, the one kept still names its
                // conversation and its shell).
                if relaunches(job.agent) {
                    refresh_snapshot(&job.sid, &job.kept, hooks);
                }
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

/// The relaunch this session's back-off owed is no longer owed ([`Kept::owed`]):
/// it landed, was left, can never land, or the agent is back.
fn settled(job: &WorkerJob) {
    with_kept(job, |k| {
        k.owed = None;
        k.stranded = false;
    });
}

/// An exit the host leaves alone ([`relaunch::on_exit`] said `decision`):
/// journaled, and said on the session's attention when the relaunch was
/// LIMITED by the owner's `[harness]`.
fn left_alone(job: &WorkerJob, hooks: &Hooks, decision: OnExit, said: Say) {
    let sid = job.sid.as_str();
    let why = match decision {
        OnExit::PersonAsked => "a person typed into the session just before: the exit is theirs",
        OnExit::Held => {
            "the session is held (a halt, a lease or a driver's turn): the exit is theirs"
        }
        OnExit::Limited => "[harness] relaunch = false",
        OnExit::Relaunch | OnExit::Restarted => "",
    };
    aterm_log::info!(
        "harness @{sid}: {} exited; not relaunched: {why}",
        job.agent.name()
    );
    tell(hooks, sid, said, why, false);
}

/// Say a relaunch's word on the session's attention (`why`: the step, or the
/// limit, that said it). A step word (`refused:shell-gone`, `wait:resume`) is
/// the log's; the attention says the state and what a person can do — for a
/// relaunch that keeps failing, the cause when it is one only a person clears
/// ([`failing_text`]) — and, for a restart of the harness's own
/// (`restarted`), that aterm ended the agent, and has not brought it back yet,
/// or cannot.
fn tell(hooks: &Hooks, sid: &str, said: Say, why: &str, restarted: bool) {
    let why: String = why.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = why.len().min(80);
    while !why.is_char_boundary(end) {
        end -= 1;
    }
    let why = &why[..end];
    let (text, step) = match said {
        Say::Nothing => return,
        Say::Clear => {
            (hooks.badge)(sid, None);
            return;
        }
        Say::Cannot if restarted => (
            "aterm ended the agent to upgrade or restart it and cannot bring it back; resume it \
             by hand"
                .to_string(),
            why,
        ),
        Say::Cannot => (
            "the agent exited and cannot be relaunched; resume it by hand".to_string(),
            why,
        ),
        Say::Failing => (failing_text(why, restarted), why),
        Say::Limited => (format!("the agent exited and is not relaunched: {why}"), ""),
    };
    if step.is_empty() {
        aterm_log::warn!("harness @{sid}: {text}");
    } else {
        aterm_log::warn!("harness @{sid}: {text} ({step})");
    }
    (hooks.badge)(sid, Some(&text));
}

/// The badge of a relaunch that keeps failing after `step`: with the cause
/// when it is one only a person clears — a hold or a hand on the tab
/// (`wait:held`), a foreground job keeping the shell from its prompt
/// (`wait:shell-prompt`), the conversation open in another tab. Any other
/// step the retry gets past on its own, and it stays in the log. A restart of
/// the harness's own (`restarted`) says aterm ended the agent and has not
/// brought it back yet.
fn failing_text(step: &str, restarted: bool) -> String {
    let head = if restarted {
        "aterm ended the agent to upgrade or restart it and has not brought it back yet"
    } else {
        "the agent exited and its relaunch keeps failing"
    };
    let cause = match step {
        "wait:held" => "the session is held",
        "wait:shell-prompt" => "the shell prompt is not back",
        "wait:conversation-in-other-tab" => "the conversation is open in another tab",
        _ => return format!("{head} (still trying)"),
    };
    format!("{head} (still trying: {cause})")
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
    /// [`WorkerIdle::stepped`].
    stepped: Arc<AtomicU64>,
    /// The host's activation notices this worker was told of.
    activations: u64,
    /// [`WorkerJob::note`]: asked at the host's wakes.
    note: Note,
    /// [`WorkerJob::group`]: kept to the roster's by [`keep_holding`].
    group: Arc<AtomicI32>,
    /// [`WorkerIdle::points`]: what the loop last offered and withheld, for
    /// the watch to copy onto the upgrade's record ([`watch_upgrades`]).
    points: Arc<Points>,
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

/// THE ADOPTED-CLAIM GRACE (the round-four plan, item 9): how long this
/// host holds off an agent session a seamless update handed it, when the
/// outgoing process could not vouch that no other supervisor held it
/// (`SessionRecord::claim_grace`), before its worker claims it.
///
/// Why: an external supervisor's claim (aterm-agent's `watch` loop, a script
/// on `meta set supervisor … ttl=`) was held by the OLD instance. The loop
/// renews at its first request a renewal step
/// ([`aterm_agent::supervise::CLAIM_RENEW`]) after its last renewal, and it
/// makes a request at least once a step, so its next renewal reaches the new
/// instance within two steps of the last one the old instance saw. This host claimed at Commit, before that renewal, and the external
/// loop — refused `ERR busy supervisor=aterm-harness@…` — fell back to
/// watching: every update took each externally supervised session away from
/// its supervisor. Two steps and a margin let the renewal land first; the
/// host then parks behind it ([`CLAIM_HELD`]) exactly as it does in the old
/// instance. A session whose claim the producer DID vouch for (nobody's but
/// its own host's) is claimed at Commit as before.
///
/// Bounded and per session: it can only delay this host's own supervision,
/// by this much, after an update; it gates nothing a person or another
/// supervisor does. (The round-four plan's first sketch said one step and a
/// margin; one step misses a loop whose last renewal fell just short of its
/// step before the Commit.)
pub(crate) const ADOPTED_CLAIM_GRACE: Duration =
    Duration::from_secs(2 * aterm_agent::supervise::CLAIM_RENEW.as_secs() + 5);

/// The sessions [`HostHandle::defer_adopted_claims`] named, and when each may
/// be claimed ([`ADOPTED_CLAIM_GRACE`]).
#[derive(Default)]
struct ClaimGrace {
    /// Named, with their grace, and not started yet: a successor names them
    /// before its Commit, while its host is suspended, and the grace runs
    /// from the first look of the host thread once it is not — the Commit's
    /// resume — which is when the external supervisors' requests start
    /// reaching this instance.
    pending: Vec<(String, Duration)>,
    /// Each session's instant before which no worker starts for it.
    until: HashMap<String, Instant>,
}

impl ClaimGrace {
    /// Start the pending sessions' grace at `now`.
    fn start(&mut self, now: Instant) {
        for (sid, grace) in self.pending.drain(..) {
            self.until.insert(sid, now + grace);
        }
    }

    /// Forget every grace that has run out at `now`; the earliest instant a
    /// session still held off may be claimed, for the host's next wake.
    fn prune(&mut self, now: Instant) -> Option<Instant> {
        self.until.retain(|_, at| *at > now);
        self.until.values().min().copied()
    }

    /// Whether `sid` is held off at `now`.
    fn holds(&self, sid: &str, now: Instant) -> bool {
        self.until.get(sid).is_some_and(|at| *at > now)
    }
}

#[derive(Default)]
struct State {
    cfg: SupervisorConfig,
    /// Bumped by every [`HostHandle::set_config`] that changed the policy.
    cfg_gen: u64,
    suspended: bool,
    shutting_down: bool,
    /// The adopted sessions held off for their claim grace
    /// ([`ADOPTED_CLAIM_GRACE`]).
    claim_grace: ClaimGrace,
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
    /// The instance's one API reach probe, shared by every worker's
    /// [`WorkerIdle::reach`]; stopped at the host's shutdown.
    net: Arc<NetProbe>,
    /// The cold restore's relaunch queue ([`RestoredQueue`]; taken alone),
    /// and the condition its worker waits on — a retry's pause, or a
    /// handoff's pause until a rollback resumes it.
    restored: Mutex<RestoredQueue>,
    restored_wake: Condvar,
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
                net: NetProbe::new(),
                restored: Mutex::default(),
                restored_wake: Condvar::new(),
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

    /// Whether the host RELAUNCHES a frozen Claude Code it supervises once
    /// the stall's remedy (`signal term|kill`) has ended it (U1): it
    /// supervises ([`Self::supervising`]) under `[harness] relaunch`, read
    /// live. One answer for every session: whether it would relaunch ONE
    /// session's agent is [`Self::relaunching`] and the session's own claim
    /// and hands (`input_stall::host_relaunches`) — only then does the
    /// stall's remedy say so instead of naming a command (resume-hint
    /// review, 2026-09-26).
    pub(crate) fn relaunches(&self) -> bool {
        self.supervising() && self.shared.switches.relaunch()
    }

    /// The sessions whose agent this host WOULD relaunch on its conversation
    /// if it ended now: it relaunches at all ([`Self::relaunches`]), and the
    /// snapshot it keeps of the session's agent is one the relaunch would
    /// plan ([`relaunch::resumes_on_exit`]: a conversation, an argv whose
    /// every flag the rewrite knows, a launch resumable in place, a shell
    /// the line is written for). Empty otherwise — and for every session
    /// the host keeps no snapshot of (no worker read one, or another
    /// supervisor holds it).
    ///
    /// WHY (resume-hint review, 2026-09-26): a frozen Claude Code's remedy
    /// said "aterm relaunches it on its conversation" — and named no command
    /// — on [`Self::relaunches`] alone, one switch for every session, while
    /// the relaunch itself refused launches it cannot carry
    /// (`argv:unknown-flag`, `argv:not-resumable`), waited for ever on a
    /// shell it has no line for, and had nothing to relaunch without a
    /// snapshot. The window reads this set BEFORE it takes the store's lock
    /// (two leaf locks per session, no I/O), and asks per session.
    pub(crate) fn relaunching(&self) -> std::collections::HashSet<String> {
        relaunching_of(self.relaunches(), &self.shared.kept)
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
    /// ([`relaunch::after_host_ended`]) on one background worker — never this
    /// caller's, the event loop — while `[harness] relaunch` allows it: each
    /// tab gets a first attempt before another tab's retry pause, and a
    /// step that is not yet possible is tried again on a growing pause,
    /// [`RESTORED_TRIES`] times normally, with no pause after the last.
    /// An already typed relaunch waiting for its conversation can be checked
    /// until its in-flight stale horizon; a relaunch, a launch that ends there, one that
    /// cannot be made and one typed that did not take ([`restored_again`])
    /// are journaled, never retried. A headless instance relaunches nothing.
    ///
    /// The reopened layout's row told the person each agent the relaunch
    /// brings back resumes its conversation ([`Self::relaunches_restored`],
    /// ruling 293). One of those that did not come back — a shell gone, a tab
    /// never ready, the old agent still running, an agent that started and
    /// did not pick its conversation up — is said ONCE, in one row
    /// for them all once every tab is tried
    /// ([`crate::message_reporters::restored_agents_not_resumed`]); one that
    /// row already counted as lost (no conversation, a one-shot run) is not
    /// said twice.
    ///
    /// THE QUEUE OUTLIVES A SEAMLESS UPDATE (round four of the 2026-09 update
    /// robustness work, plan item 7). It is the host's ([`RestoredQueue`]),
    /// not the worker's: the automatic update holds off while it is not empty
    /// ([`Self::restored_pending`], bounded by [`RESTORED_HOLD`]); a park
    /// pauses it and freezes what it holds ([`Self::pause_restored`]), the
    /// handoff layout carries that on each agent's leaf
    /// ([`Self::pending_restored_for`]) and the successor queues them here in
    /// its own process at its Commit; a rollback resumes it
    /// ([`Self::resume_restored`]). A second call while the worker runs adds
    /// to its queue.
    pub(crate) fn relaunch_restored(&self, restored: Vec<RestoredAgent>) {
        if restored.is_empty() || !self.relaunches_restored() {
            return;
        }
        let now = Instant::now();
        let start = {
            let mut queue = self
                .shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            queue.since.get_or_insert(now);
            let first = queue.next_order;
            queue.next_order = first + restored.len();
            queue
                .attempts
                .extend(
                    restored
                        .into_iter()
                        .enumerate()
                        .map(|(at, agent)| RestoredAttempt {
                            agent,
                            order: first + at,
                            due: now,
                            pause: RESTORED_FIRST_PAUSE,
                            tried: 0,
                            resume_wait_since: None,
                            acting: false,
                        }),
                );
            !std::mem::replace(&mut queue.running, true)
        };
        self.shared.restored_wake.notify_all();
        if !start {
            return;
        }
        let act = Arc::clone(&self.hooks.acts.relaunch_restored);
        let badge = Arc::clone(&self.hooks.badge);
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}restored"))
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                relaunch_restored_queue(&shared, act.as_ref(), badge.as_ref());
            });
        if let Err(e) = spawned {
            aterm_log::warn!("harness: the restored tabs' agents could not be relaunched: {e}");
            // Nothing will step the queue: empty it, so the automatic update
            // it would hold is not held for nothing.
            let mut queue = self
                .shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            queue.attempts.clear();
            queue.running = false;
        }
    }

    /// THE AUTOMATIC UPDATE WAITS FOR THE RESTORED AGENTS (round four, plan
    /// item 7): `true` while the cold restore's queue still holds an agent
    /// ([`Self::relaunch_restored`]) and `now` is inside [`RESTORED_HOLD`] of
    /// the first one this process queued. Read by the ladder's facts
    /// (`ActivityFacts::harness_restored_pending`), which refuse the automatic
    /// park in every phase while it holds — the queue is seconds of work
    /// that a park would otherwise interrupt, right after the reopened
    /// layout's row said the agents come back. Bounded, once per process and
    /// nobody's to extend: it cannot pin a build (the ladder's law), and past
    /// it the successor carries what is left. An explicit update never reads
    /// it.
    pub(crate) fn restored_pending(&self, now: Instant) -> bool {
        let queue = self
            .shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        !queue.attempts.is_empty() && queue.since.is_some_and(|since| now < since + RESTORED_HOLD)
    }

    /// A HANDOFF PARKED THE READERS (round four, plan item 7): the restored
    /// agents' worker takes no new step — the one in flight finishes — and the
    /// queue as it stands, the step in flight included, is frozen for the
    /// handoff layout ([`Self::pending_restored_for`]). Idempotent; a rollback
    /// undoes it ([`Self::resume_restored`]), a Commit ends the process.
    ///
    /// A RELAUNCH ON EXIT A WORKER'S BACK-OFF STILL OWES is frozen with it
    /// (round six, F13; [`Kept::owed`]): the workers type no relaunch from
    /// here ([`Switches::parked`]), so the successor relaunches it once, from
    /// the layout.
    #[cfg(any(unix, test))]
    pub(crate) fn pause_restored(&self) {
        self.shared.switches.park(true);
        let owed = self.owed_relaunches();
        let mut queue = self
            .shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if queue.paused {
            return;
        }
        queue.paused = true;
        let mut carried: Vec<(String, Snapshot)> = queue
            .attempts
            .iter()
            .map(|attempt| (attempt.agent.sid.clone(), attempt.agent.snap.clone()))
            .collect();
        for (sid, snap) in owed {
            if !carried.iter().any(|(tab, _)| *tab == sid) {
                carried.push((sid, snap));
            }
        }
        queue.carried = Some(carried);
    }

    /// Every session whose relaunch on exit is owed now ([`Kept::owed`]), with
    /// the snapshot it is owed on. Each [`Kept`] is read under its own lock
    /// alone, after the map's is let go.
    fn owed_relaunches(&self) -> Vec<(String, Snapshot)> {
        let kept: Vec<(String, Arc<Mutex<Kept>>)> = self
            .shared
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(sid, kept)| (sid.clone(), Arc::clone(kept)))
            .collect();
        let mut owed: Vec<(String, Snapshot)> = kept
            .into_iter()
            .filter_map(|(sid, kept)| {
                let snap = kept
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .owed
                    .clone()?;
                Some((sid, snap))
            })
            .collect();
        owed.sort_by(|a, b| a.0.cmp(&b.0));
        owed
    }

    /// A COMMIT THAT FAILED after a park's stop: every relaunch on exit a
    /// stopped worker's back-off still owed ([`Kept::stranded`]) is this
    /// process's again, and no worker will take it (its tab is at its shell)
    /// — queued as the restored agents are ([`Self::relaunch_restored`]),
    /// `place` naming each tab for the row that says one did not come back
    /// (round six, F13). Called after [`Self::resume_restored`].
    #[cfg(any(unix, test))]
    pub(crate) fn relaunch_stranded(&self, place: &dyn Fn(&str) -> String) {
        let kept: Vec<(String, Arc<Mutex<Kept>>)> = self
            .shared
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(sid, kept)| (sid.clone(), Arc::clone(kept)))
            .collect();
        let mut stranded: Vec<RestoredAgent> = kept
            .into_iter()
            .filter_map(|(sid, kept)| {
                let snap = {
                    let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
                    if !std::mem::take(&mut kept.stranded) {
                        return None;
                    }
                    kept.owed.take()?
                };
                let place = place(&sid);
                aterm_log::info!(
                    "harness @{sid}: the update did not go through; the relaunch its agent's \
                     exit was owed is taken up again"
                );
                Some(RestoredAgent { sid, snap, place })
            })
            .collect();
        stranded.sort_by(|a, b| a.sid.cmp(&b.sid));
        self.relaunch_restored(stranded);
    }

    /// The handoff failed and this process keeps its sessions: the restored
    /// agents' worker steps again from where it paused, and so does a
    /// worker's relaunch on exit ([`Switches::parked`]).
    #[cfg(any(unix, test))]
    pub(crate) fn resume_restored(&self) {
        self.shared.switches.park(false);
        ring();
        {
            let mut queue = self
                .shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            queue.paused = false;
            queue.carried = None;
        }
        self.shared.restored_wake.notify_all();
    }

    /// The restored agent the queue still owes tab `sid`, as the handoff
    /// layout carries it on the tab's leaf (`App::capture_handoff_layout`):
    /// what [`Self::pause_restored`] froze while a handoff is parked, the live
    /// queue otherwise — and, there, a relaunch on exit a worker's back-off
    /// owes the tab ([`Kept::owed`], round six F13). `None` when the tab owes
    /// nothing.
    ///
    /// A RELAUNCH OWED AFTER THE PARK is carried too (round six F13, review
    /// two): a crash the worker reads only after [`Self::pause_restored`] froze
    /// the queue — its exit's detection and [`relaunch::EXIT_SETTLE`] run
    /// first — becomes owed with the readers parked, so the owed relaunches
    /// are read live whether or not a park froze the queue. A parked worker
    /// types none of them, so the layout naming one relaunches it once. The
    /// residual, stated: a crash the worker reads only after the layout was
    /// captured is not on it, and the Commit's stop leaves it stranded in a
    /// process about to exit.
    pub(crate) fn pending_restored_for(&self, sid: &str) -> Option<Snapshot> {
        {
            let queue = self
                .shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let frozen = match &queue.carried {
                Some(carried) => carried
                    .iter()
                    .find(|(tab, _)| tab == sid)
                    .map(|(_, snap)| snap.clone()),
                None => queue
                    .attempts
                    .iter()
                    .find(|a| a.agent.sid == sid)
                    .map(|attempt| attempt.agent.snap.clone()),
            };
            if frozen.is_some() {
                return frozen;
            }
        }
        self.owed_relaunches()
            .into_iter()
            .find(|(tab, _)| tab == sid)
            .map(|(_, snap)| snap)
    }

    /// Every tab the restored queue still owes an agent, in the layout's
    /// order (the tests' view of [`Self::pending_restored_for`]).
    #[cfg(test)]
    fn pending_restored(&self) -> Vec<String> {
        let queue = self
            .shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut owed: Vec<(usize, String)> = queue
            .attempts
            .iter()
            .map(|attempt| (attempt.order, attempt.agent.sid.clone()))
            .collect();
        owed.sort();
        owed.into_iter().map(|(_, sid)| sid).collect()
    }

    /// Whether the agents a cold restore hands this host are relaunched
    /// ([`Self::relaunch_restored`]): not in a headless instance, and while
    /// `[harness]` is on with `relaunch` allowed. The reopened layout's row
    /// reads it to say an agent resumes rather than was lost (ruling 293).
    pub(crate) fn relaunches_restored(&self) -> bool {
        !self.shared.headless && self.shared.switches.relaunch()
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

    /// THE SUCCESSOR'S COMMIT (round four of the 2026-09 update robustness
    /// work, plan item 7): supervise again ([`Self::resume`]), and carry on
    /// every restart the outgoing instance left IN FLIGHT at its Commit — an
    /// agent it had signalled, or whose relaunch line it had typed, whose
    /// record says so ([`upgrade_drive::in_flight`]) while nothing here would
    /// ever look at it again: the workers start for agent tabs only, and the
    /// tab of a signalled agent is back at its shell. `sessions` is every
    /// live session this process adopted, `(sid, place)`.
    ///
    /// One bounded background thread ([`carry_in_flight_after_handoff`]):
    /// every tab's first step before any retry, a step that is not possible
    /// yet tried again on the restored queue's growing pause, none past the
    /// in-flight record's stale horizon ([`carry_bound`]) — one still waiting
    /// then said — and none while this host is suspended again (a Commit then
    /// ends the process and the next successor carries it on from the
    /// record; a failed one resumes the carry). The upgrade's one lock
    /// serializes each step against the outgoing instance's own act and
    /// against this host's workers, and a tab whose agent is supervised again
    /// is its worker's — looked at again, never dropped
    /// ([`CARRY_SUPERVISED`]): a worker the adopted-claim grace holds off
    /// sees no exit. Helps from the first update INTO this build: the record
    /// is on disk whoever wrote it.
    #[cfg(any(unix, test))]
    pub(crate) fn resume_after_handoff(&self, sessions: Vec<(String, String)>) {
        self.resume();
        let grace = {
            let state = self.shared.lock();
            if sessions.is_empty() || !Shared::active(&state, self.shared.headless) {
                return;
            }
            state.cfg.human_grace_s
        };
        let carry = Arc::clone(&self.hooks.acts.carry_in_flight);
        let pause = Arc::clone(&self.hooks.pause);
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}carried"))
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                carry_in_flight_after_handoff(
                    &shared,
                    carry.as_ref(),
                    pause.as_ref(),
                    &sessions,
                    grace,
                );
            });
        if let Err(e) = spawned {
            aterm_log::warn!(
                "harness: the restarts the previous aterm left in flight could not be carried \
                 on: {e}"
            );
        }
    }

    /// HOLD OFF `sids` — sessions a seamless update handed this instance
    /// whose claim the outgoing process could not vouch for
    /// (`SessionRecord::claim_grace`) — for `grace` ([`ADOPTED_CLAIM_GRACE`]
    /// in production) before a worker claims any of them, so an external
    /// supervisor that held one in the old instance claims it again first
    /// (the round-four plan, item 9). The grace runs from the host thread's
    /// first look while the host is not suspended: on a successor, the
    /// Commit's [`Self::resume`]. Every other session is supervised as
    /// before.
    pub(crate) fn defer_adopted_claims(&self, sids: Vec<String>, grace: Duration) {
        if sids.is_empty() {
            return;
        }
        self.shared
            .lock()
            .claim_grace
            .pending
            .extend(sids.into_iter().map(|sid| (sid, grace)));
        ring();
    }

    /// The sessions this host is holding off for their claim grace right now
    /// ([`Self::defer_adopted_claims`]) — supervised by nobody here yet, so
    /// the menu bar raises their agent's own boxes as it would for a session
    /// this host does not supervise.
    pub(crate) fn held_off(&self) -> Vec<String> {
        let now = Instant::now();
        let state = self.shared.lock();
        let grace = &state.claim_grace;
        grace
            .until
            .keys()
            .filter(|sid| grace.holds(sid, now))
            .cloned()
            .collect()
    }

    /// Stop every worker and the host thread, bounded: past
    /// [`HOST_JOIN_TIMEOUT`] the thread is left to the process exit.
    pub(crate) fn shutdown_and_join(&self) {
        self.shared.lock().shutting_down = true;
        self.shared.net.stop();
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

/// THE RESTORED AGENTS' ONE WORKER ([`HostHandle::relaunch_restored`]), over
/// the host's queue ([`RestoredQueue`]) while `[harness] relaunch` allows it:
/// the earliest due attempt first — every first attempt before any retry —
/// each step run with the queue's lock released, its attempt kept in the
/// queue while it runs. A handoff's pause stops it at the top of the loop
/// (what it missed so far is said then: a Commit ends this process and its
/// row with it) until a rollback resumes it.
fn relaunch_restored_queue(shared: &Shared, act: &RestoredFn, badge: &BadgeFn) {
    let switches = &shared.switches;
    let mut queue = shared
        .restored
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    loop {
        if queue.paused {
            let missed = std::mem::take(&mut queue.missed);
            if !missed.is_empty() {
                drop(queue);
                say_restored_missed(missed);
                queue = shared
                    .restored
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            queue = shared
                .restored_wake
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        if queue.attempts.is_empty() || !switches.relaunch() {
            // Done — or switched off, which drops the rest as it always has
            // (and so releases the update's hold on them).
            queue.attempts.clear();
            queue.running = false;
            let missed = std::mem::take(&mut queue.missed);
            drop(queue);
            say_restored_missed(missed);
            return;
        }
        // The earliest due tab runs first. Initial attempts all precede any
        // retry, even when the first step took a while. The one worker still
        // takes only one upgrade lock at a time; no new actor can race a
        // relaunch.
        let (at, due) = queue
            .attempts
            .iter()
            .enumerate()
            .min_by_key(|(_, attempt)| attempt.due)
            .map(|(at, attempt)| (at, attempt.due))
            .expect("the queue was not empty");
        let now = Instant::now();
        if due > now {
            queue = shared
                .restored_wake
                .wait_timeout(queue, due - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            continue;
        }
        let attempt = &mut queue.attempts[at];
        attempt.acting = true;
        let (order, agent) = (attempt.order, attempt.agent.clone());
        drop(queue);
        let step = act(&agent.sid, &agent.snap, switches.upgrade());
        let last = relaunch::outcome(&step);
        queue = shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(at) = queue.attempts.iter().position(|a| a.order == order) else {
            continue;
        };
        let mut attempt = queue.attempts.remove(at).expect("the attempt was found");
        attempt.acting = false;
        attempt.tried += 1;
        let now = Instant::now();
        if step == "wait:resume" {
            attempt.resume_wait_since.get_or_insert(now);
        }
        let waiting = attempt
            .resume_wait_since
            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        if restored_retry(&step, &last, attempt.tried, waiting) {
            attempt.due = now + attempt.pause;
            attempt.pause = (attempt.pause * 2).min(RESTORED_MAX_PAUSE);
            queue.attempts.push_back(attempt);
            continue;
        }
        let RestoredAgent { sid, snap, place } = attempt.agent;
        // The relaunch is over, landed or not: a word a relaunch on exit
        // raised on the tab before a handoff carried it here ("still
        // trying", round six F13) is no longer true. Said without the queue.
        drop(queue);
        badge(&sid, None);
        queue = shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if last != Outcome::Relaunched
            && crate::restore::agent_resumes(snap.session.as_deref(), &snap.argv)
        {
            aterm_log::warn!(
                "harness @{sid}: the agent aterm hosted {place} was not relaunched after aterm \
                 ended: {last:?}"
            );
            queue.missed.push((attempt.order, place, last));
        }
    }
}

/// The one row for the restored agents that did not come back
/// ([`crate::message_reporters::restored_agents_not_resumed`]), in the
/// layout's order: attempts finish in due order. Nothing when none missed.
fn say_restored_missed(mut missed: Vec<(usize, String, Outcome)>) {
    if missed.is_empty() {
        return;
    }
    missed.sort_by_key(|(order, _, _)| *order);
    let missed = missed
        .into_iter()
        .map(|(_, place, outcome)| (place, outcome))
        .collect::<Vec<_>>();
    let log = crate::logging::log_dir().map(|dir| dir.join("aterm.log"));
    crate::message_inbox::queue_message(crate::message_reporters::restored_agents_not_resumed(
        &missed,
        log.as_deref(),
    ));
}

/// [`HostHandle::resume_after_handoff`]'s thread: each of `sessions` stepped
/// while it has a restart in flight ([`Acts::carry_in_flight`]), the earliest
/// due first, a step that is not possible yet ([`restored_again`]) — or a tab
/// whose agent still reads supervisable ([`CARRY_SUPERVISED`]) — tried again
/// on a growing pause ([`RESTORED_FIRST_PAUSE`] doubling to
/// [`RESTORED_MAX_PAUSE`], waited through the host's pause seam), until
/// [`carry_bound`] has passed or the host is shutting down. A restart that
/// cannot be made (`Cannot`: the tab's shell gone, no state to act on) is
/// said in the restored agents' one row ([`say_restored_missed`]).
///
/// LANDS, OR IS SAID (round six, F51): a tab whose last step still waited
/// when the bound ran out — a person at the returned prompt, a hold — is said
/// in that row too, with what it waited on. The bound is the record's own
/// expiry ([`relaunch::STALE_S`]) plus two of the longest pauses, so the step
/// that finds the record stale (`failed:stale-exit`, said) comes before it;
/// until then the loop ended at [`relaunch::STALE_S`] from the successor's
/// start, a few seconds after the record's expiry the OLD process stamped, so
/// that step usually never ran and the tab was dropped with nothing said, its
/// row promising a relaunch nothing would ever type. Only a tab whose last
/// answer was its worker's ([`CARRY_SUPERVISED`]) is left to that worker.
///
/// A SUSPENDED HOST STEPS NOTHING AND DROPS NOTHING: a park of this
/// successor's own waits, and a Commit that follows it ends the process (the
/// next successor carries the record on), while one that fails resumes it
/// and the carry goes on. Until then a park ended the loop, and a failed
/// Commit's [`HostHandle::resume`] never started it again.
#[cfg(any(unix, test))]
fn carry_in_flight_after_handoff(
    shared: &Shared,
    carry: &CarryInFlightFn,
    pause: &PauseFn,
    sessions: &[(String, String)],
    grace: u32,
) {
    // `(order, sid, place, due, pause, last)`: `last` is the last step's
    // outcome while it waited, `None` before a step or while the tab was its
    // worker's.
    type Owed<'a> = (usize, &'a str, &'a str, Instant, Duration, Option<Outcome>);
    let mut started = Instant::now();
    let bound = carry_bound(pause);
    let mut owed: Vec<Owed<'_>> = sessions
        .iter()
        .enumerate()
        .map(|(order, (sid, place))| {
            (
                order,
                sid.as_str(),
                place.as_str(),
                started,
                RESTORED_FIRST_PAUSE,
                None,
            )
        })
        .collect();
    let mut missed = Vec::new();
    while !owed.is_empty() {
        let suspended = {
            let state = shared.lock();
            if state.shutting_down {
                return;
            }
            state.suspended
        };
        if suspended {
            // The time a park held the host is no time the carry had: its
            // bound runs again from the resume.
            std::thread::sleep(pause(RESTORED_FIRST_PAUSE));
            started = Instant::now();
            continue;
        }
        if started.elapsed() >= bound {
            break;
        }
        let at = owed
            .iter()
            .enumerate()
            .min_by_key(|(_, (order, _, _, due, _, _))| (*due, *order))
            .map(|(at, _)| at)
            .expect("owed was not empty");
        let now = Instant::now();
        if owed[at].3 > now {
            // Never longer than one first pause, so a park is seen promptly.
            std::thread::sleep((owed[at].3 - now).min(pause(RESTORED_FIRST_PAUSE)));
            continue;
        }
        let (order, sid, place, _, wait, _) = owed[at];
        let Some(step) = carry(sid, grace) else {
            owed.remove(at);
            continue;
        };
        let last = relaunch::outcome(&step);
        let supervised = step == CARRY_SUPERVISED;
        if supervised || restored_again(&step, &last) {
            owed[at].3 = Instant::now() + pause(wait);
            owed[at].4 = (wait * 2).min(RESTORED_MAX_PAUSE);
            owed[at].5 = (!supervised).then_some(last);
            continue;
        }
        owed.remove(at);
        if matches!(last, Outcome::Cannot(_)) {
            aterm_log::warn!(
                "harness @{sid}: the restart the previous aterm left in flight {place} could \
                 not be carried on: {last:?}"
            );
            missed.push((order, place.to_string(), last));
        }
    }
    for (order, sid, place, _, _, last) in owed {
        let Some(last) = last else {
            continue;
        };
        aterm_log::warn!(
            "harness @{sid}: the restart the previous aterm left in flight {place} was not \
             carried on in time: {last:?}"
        );
        missed.push((order, place.to_string(), last));
    }
    say_restored_missed(missed);
}

/// ONE ANSWER OF [`Acts::carry_in_flight`] for a tab: `None` once nothing is
/// in flight there (`in_flight`), [`CARRY_SUPERVISED`] — no step — while its
/// agent reads supervisable (`supervisable`): the worker carries its own
/// restart on at its loop's idle point, and the upgrade's step never acts on
/// a live agent anywhere else; otherwise the step's word (`step`). The tab
/// its worker owns is looked at again, never dropped (round six, F17): the
/// adopted-claim grace ([`ADOPTED_CLAIM_GRACE`]) can hold that worker off
/// exactly while the SIGTERMed agent exits, and none starts after it.
fn carry_in_flight_now(
    in_flight: impl FnOnce() -> bool,
    supervisable: impl FnOnce() -> bool,
    step: impl FnOnce() -> String,
) -> Option<String> {
    if !in_flight() {
        return None;
    }
    if supervisable() {
        return Some(CARRY_SUPERVISED.to_string());
    }
    Some(step())
}

/// How long the successor carries a restart left in flight
/// ([`carry_in_flight_after_handoff`]): the record's own expiry
/// ([`relaunch::STALE_S`]) and two of the longest pauses between steps, so a
/// step that reads the record stale comes first — through the host's pause
/// seam, as every wait of the carry is.
#[cfg(any(unix, test))]
fn carry_bound(pause: &PauseFn) -> Duration {
    pause(Duration::from_secs(relaunch::STALE_S) + 2 * RESTORED_MAX_PAUSE)
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
    /// Every supervised agent as the roster last named it, kept while a pass
    /// that cannot name it finds it still holding its tab ([`keep_holding`]).
    holding: HashMap<String, Held>,
    /// THE WATCHES the host fires off any point ([`watch_upgrades`]).
    watches: Watches,
}

impl HostState {
    /// Named for what it is, not `new`: it takes `shared`'s lock, and the
    /// lock census resolves a call by its name — every `AtomicBool::new` in
    /// `start_workers`, under that same lock, read as re-entering it.
    fn from_shared(shared: &Shared) -> Self {
        Self {
            workers: Workers::default(),
            followed: HashMap::new(),
            full_epoch_seen: FULL_FOLLOW_EPOCH.load(Ordering::SeqCst),
            phase_epoch_seen: PHASE_FOLLOW_EPOCH.load(Ordering::SeqCst),
            held: HashMap::new(),
            forgiven_gen: shared.lock().cfg_gen,
            view: ViewLook::default(),
            disk_at: Instant::now() + DISK_FIRST_LOOK,
            holding: HashMap::new(),
            watches: Watches::default(),
        }
    }
}

/// A supervised agent as the roster last named it ([`HostState::holding`]).
struct Held {
    agent: Program,
    /// The stamp the roster last named it with. Its group
    /// ([`FollowStamp::group`]; `0`: none) is what [`Acts::holds`] asks
    /// about when a pass cannot name it; while it is kept, the whole stamp
    /// stands in the pass for the one the roster did not give, so its follow
    /// comes at a full follow as every session's does, not at every bell
    /// ([`follow_entries`] follows a session with no stamp at each pass).
    stamp: FollowStamp,
    /// While a pass cannot name it and it still holds its tab: when the host
    /// looks again, and the step that look was set by ([`hold_look`]).
    /// `None` while the roster names it.
    look: Option<(Instant, Duration)>,
    /// While kept: when the keep began, and its worker's step stamp then
    /// ([`WorkerIdle::stepped`]; `0` with no worker). The keep lasts
    /// [`HOLDS_KEEP_MAX`] at most; and another group named in its stead is
    /// the kept agent's exit only while its worker took no step since (a step
    /// that ended the agent relaunches it in a new group: that one is the
    /// step's, [`keep_holding`]) — its own worker's: another tab's step, or
    /// the window's look at the upgrades, relaunches nothing here. `None`
    /// while the roster names it, and while its worker's own step runs.
    kept: Option<(Instant, u64)>,
}

/// How long after a followed tab's screen moved the owner's view is looked
/// at ONCE MORE ([`look_at_upgrades`]). Claude Code writes the status its
/// screen shows after drawing it — from an effect that runs once the frame
/// is committed, through a read-then-write of its session file chained behind
/// the file's earlier writes (read in the 2.1.272 bundle) — so the look a
/// screen's move prompts can read the status that move replaces. The write
/// takes milliseconds; one more look this long after the move reads what it
/// wrote. A write later still is read at the tab's next move or step.
const STATUS_AFTER_SCREEN: Duration = Duration::from_secs(2);

/// What the host last looked at the owner's view of the upgrades over
/// ([`look_at_upgrades`]): the workers' acts and the activation notices seen,
/// the supervised tabs, and when the view said it changes next. `None`: it
/// is not shown (never looked at, or stood down).
#[derive(Default)]
struct ViewLook {
    shown: Option<(u64, u64, Vec<String>)>,
    next: Option<Instant>,
    /// The tabs whose row follows its agent's live status
    /// ([`ViewNext::follows`]), each with its screen's publication `rev` at
    /// that look: a move of one is a look.
    follows: Vec<(String, u64)>,
    /// When the look after a followed screen's move is owed
    /// ([`STATUS_AFTER_SCREEN`]), once.
    settle: Option<Instant>,
    /// The tabs the last look found past their watch ([`ViewNext::watch`]),
    /// replaced by every look: [`watch_upgrades`] fires each.
    watch: Vec<String>,
}

fn host_loop(shared: &Shared, hooks: &Hooks) {
    let mut host = HostState::from_shared(shared);
    let mut seen = bell_now();
    let mut timed_out = true;
    while let ControlFlow::Continue(until) = host_pass(shared, &mut host, timed_out, hooks) {
        (seen, timed_out) = bell_wait(seen, until);
    }
}

/// ONE PASS of the host over what the sessions are now (`timed_out`: no
/// bell woke it, a look it set came due). `Break` at shutdown, every worker
/// stopped and joined; otherwise the instant a look it owes itself comes
/// due with no bell to ring for it — the earliest of a worker's look, the
/// keep's ([`keep_holding`]), the owner's view's, the disk watch's and the
/// end of an adopted session's claim grace ([`ADOPTED_CLAIM_GRACE`]).
fn host_pass(
    shared: &Shared,
    host: &mut HostState,
    timed_out: bool,
    hooks: &Hooks,
) -> ControlFlow<(), Option<Instant>> {
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
    reap(host);
    if shutting_down {
        shutdown_workers(std::mem::take(&mut host.workers));
        return ControlFlow::Break(());
    }
    // Every agent the roster names is one the ONE predicate supervises
    // ([`agent_of`]).
    let (mut wanted, stamps): (HashMap<String, Program>, HashMap<String, FollowStamp>) = if active {
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
    // A supervised agent the roster could not name, still holding its tab,
    // is still wanted, at the stamp it was last named with ([`keep_holding`]).
    let mut stamps = stamps;
    let hold_look = if active {
        keep_holding(
            &mut host.holding,
            &host.workers,
            &mut wanted,
            &mut stamps,
            hooks,
        )
    } else {
        host.holding.clear();
        None
    };
    host.held.retain(|sid, _| wanted.contains_key(sid));
    age_and_forgive(shared, host, cfg_gen, &wanted);
    forget_closed(shared, &host.workers, &wanted, hooks);
    follow_workers(shared, host, &wanted, &stamps, follow_all, hooks);
    let next_look = visit_workers(
        shared,
        &mut host.workers,
        &mut host.watches.told,
        &wanted,
        active,
        cfg_gen,
        hooks,
    );
    let grace_at = start_workers(shared, host, &wanted, &cfg, cfg_gen, hooks);
    let view_at = look_at_upgrades(
        &mut host.view,
        &wanted,
        &stamps,
        active && shared.switches.upgrade(),
        (
            UPGRADE_ACTS.load(Ordering::SeqCst),
            shared.activations.load(Ordering::SeqCst),
        ),
        hooks,
    );
    let watch_at = watch_upgrades(
        &mut host.watches,
        &host.view.watch,
        &host.workers,
        &wanted,
        active && shared.switches.upgrade(),
        cfg.human_grace_s,
        hooks,
    );
    let disk_at = look_at_disk(&mut host.disk_at, Instant::now(), active, &wanted, hooks);
    ControlFlow::Continue(
        [next_look, hold_look, view_at, watch_at, disk_at, grace_at]
            .into_iter()
            .flatten()
            .min(),
    )
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
/// supervised tabs, the instant the view last named (an upgrade turning
/// overdue, a word running out), or — for a tab whose row reads its agent's
/// live status ([`ViewNext::follows`]) — that tab's screen moving (its
/// publication's `rev` in `stamps`, the push a box answered or a turn begun
/// comes with), and once more [`STATUS_AFTER_SCREEN`] after that move
/// (the review of the upgrade's leftovers, 2026-09-28: a look taken while a
/// box was up stood through the whole turn that followed). Nothing else
/// looks: no timer. Stood down — its rows gone from the window, its marks
/// lowered — while the policy or `[harness] upgrade` is off. The instant to
/// look again, if one is owed.
fn look_at_upgrades(
    view: &mut ViewLook,
    wanted: &HashMap<String, Program>,
    stamps: &HashMap<String, FollowStamp>,
    active: bool,
    (acts, activations): (u64, u64),
    hooks: &Hooks,
) -> Option<Instant> {
    if !active {
        if view.shown.take().is_some() {
            let _ = (hooks.upgrade_view)(false);
        }
        view.next = None;
        view.follows.clear();
        view.settle = None;
        view.watch.clear();
        return None;
    }
    let mut tabs: Vec<String> = wanted.keys().cloned().collect();
    tabs.sort();
    let now = Instant::now();
    let seen = Some((acts, activations, tabs));
    let rev = |tab: &str| stamps.get(tab).map(|s| s.rev);
    let moved = view
        .follows
        .iter()
        .any(|(tab, seen)| rev(tab) != Some(*seen));
    if view.shown != seen
        || view.next.is_some_and(|at| at <= now)
        || view.settle.is_some_and(|at| at <= now)
        || moved
    {
        let next = (hooks.upgrade_view)(true);
        let now_s = crate::upgrade_host::now_s();
        view.next = next
            .at
            .map(|at| now + Duration::from_secs(at.saturating_sub(now_s)));
        view.follows = next
            .follows
            .into_iter()
            .filter_map(|tab| rev(&tab).map(|r| (tab, r)))
            .collect();
        view.settle =
            (moved && !view.follows.is_empty()).then(|| now + (hooks.pause)(STATUS_AFTER_SCREEN));
        view.watch = next.watch;
        view.shown = seen;
    }
    [view.next, view.settle].into_iter().flatten().min()
}

/// How soon after a watch of a tab was fired another is fired for it while
/// the owner's view still lists it due ([`watch_upgrades`]): a watch that
/// wrote moves the record's own [`upgrade_drive::WATCH_GAP`] on, and the tab
/// leaves the list at the look its write prompts; one that could not write
/// (a real visit held the sweep lock, the record moved under the read, the
/// state could not be written) is fired again this long after, never in a
/// loop — and a TOLD one that could not write is told again this long after
/// ([`told_again`]). Scaled by the host's pause seam, as every pause it names.
const WATCH_AGAIN: Duration = Duration::from_secs(120);

/// THE WATCHES THE HOST FIRES ([`watch_upgrades`]).
#[derive(Default)]
struct Watches {
    /// The tabs owed a TOLD watch — a worker started (a hot swap, an
    /// attach), an activation notice came — taken at the next pass.
    told: BTreeSet<String>,
    /// Per tab: its last watch ([`Fired`]).
    fired: HashMap<String, Fired>,
}

/// One tab's last watch ([`Watches::fired`]): whether its thread still runs,
/// when it was fired, and whether it was a TOLD watch that could not write
/// ([`told_again`]) — owed another, told, [`WATCH_AGAIN`] after this one.
struct Fired {
    running: Arc<AtomicBool>,
    at: Instant,
    again: Arc<AtomicBool>,
}

/// Whether a watch must be TOLD AGAIN (the watch's review, 2026-09-28): a
/// told watch — a worker's start, an activation notice — whose write never
/// happened because the sweep lock was busy (another tab's watch, a real
/// visit elsewhere: an activation notice fires one told watch per tab at
/// once) or refused. Only a due tab was fired again, so the retarget the
/// told watch exists for waited for the record's own deadline, up to a
/// day. A told watch that found nothing owed (`no-record`, `not-due`), or
/// whose record a visit moved under the read (`moved`: that visit read the
/// same build), is owed nothing more.
fn told_again(told: bool, watched: &upgrade_drive::Watched) -> bool {
    told && !matches!(
        watched.skipped.as_deref(),
        None | Some("no-record" | "not-due" | "moved" | "no-home")
    )
}

/// THE WATCH'S TIMER (design record 2026-09-28, "No upgrade stuck forever",
/// §3.2 C3): every tab the owner's view last found past its record's watch
/// (`due`, [`ViewNext::watch`] — the host woke for it at the instant the
/// view named, [`upgrade_drive::View::watch_due`]) and every tab told since
/// the last pass (a worker's start, an activation notice) gets ONE watch, on
/// a thread of its own ([`spawn_watch`]), unless one of its runs already. A
/// tab still listed due is fired again only [`WATCH_AGAIN`] after the last,
/// and the host wakes for that; one told while its watch runs is owed the
/// next pass after it ends. Only a supervised agent the upgrade moves, with
/// a worker, and only while the policy and `[harness] upgrade` are on
/// (`active`). The watch has no hands: it reads, and writes the record
/// (step/notice/resume are the loop's points' alone; `tools/grep_guard.sh`
/// H1). The instant to fire again, if one.
fn watch_upgrades(
    watches: &mut Watches,
    due: &[String],
    workers: &Workers,
    wanted: &HashMap<String, Program>,
    active: bool,
    grace: u32,
    hooks: &Hooks,
) -> Option<Instant> {
    if !active {
        watches.told.clear();
        return None;
    }
    watches.fired.retain(|sid, _| wanted.contains_key(sid));
    let now = Instant::now();
    let mut next: Option<Instant> = None;
    // A told watch that could not write is told again, once WATCH_AGAIN has
    // passed since it was fired — never in a loop.
    for (sid, fired) in &watches.fired {
        if fired.running.load(Ordering::SeqCst) || !fired.again.load(Ordering::SeqCst) {
            continue;
        }
        let again = fired.at + (hooks.pause)(WATCH_AGAIN);
        if now < again {
            next = Some(next.map_or(again, |n| n.min(again)));
        } else {
            fired.again.store(false, Ordering::SeqCst);
            watches.told.insert(sid.clone());
        }
    }
    let mut owed: Vec<(String, bool)> = std::mem::take(&mut watches.told)
        .into_iter()
        .map(|sid| (sid, true))
        .collect();
    for sid in due {
        if !owed.iter().any(|(s, _)| s == sid) {
            owed.push((sid.clone(), false));
        }
    }
    for (sid, told) in owed {
        let Some(w) = workers.0.get(&sid) else {
            continue;
        };
        if !wanted.get(&sid).is_some_and(|agent| upgrades(*agent)) {
            continue;
        }
        if let Some(fired) = watches.fired.get(&sid) {
            if fired.running.load(Ordering::SeqCst) {
                if told {
                    watches.told.insert(sid);
                }
                continue;
            }
            let again = fired.at + (hooks.pause)(WATCH_AGAIN);
            if !told && now < again {
                next = Some(next.map_or(again, |n| n.min(again)));
                continue;
            }
        }
        if let Some((running, again)) = spawn_watch(&sid, told, w, grace, hooks) {
            watches.fired.insert(
                sid,
                Fired {
                    running,
                    at: now,
                    again,
                },
            );
        }
    }
    next
}

/// The aterm build a watch names as its looker (`looked_by`).
fn build_word() -> String {
    format!(
        "{}+g{}",
        crate::build_info::VERSION,
        crate::build_info::GIT_COMMIT
    )
}

/// Start one watch of tab `sid` on a thread of its own, at BACKGROUND QoS as
/// the relaunch threads run (the host thread sets none, and the owner's
/// typing is never starved): its whole body is [`watch_tab`]. The flag it
/// clears as it ends and the one it sets first when it must be told again
/// ([`told_again`]), or `None` when no thread could start (said; the tab is
/// watched again at a later pass).
fn spawn_watch(
    sid: &str,
    told: bool,
    w: &Worker,
    grace: u32,
    hooks: &Hooks,
) -> Option<(Arc<AtomicBool>, Arc<AtomicBool>)> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    let again = Arc::new(AtomicBool::new(false));
    let owed = Arc::clone(&again);
    let (points, park, look_at) = (
        Arc::clone(&w.points),
        Arc::clone(&w.park),
        Arc::clone(&w.look_at),
    );
    let acts = hooks.acts.clone();
    let tab = sid.to_string();
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}watch-{sid}"))
        .spawn(move || {
            crate::qos::set_self(crate::qos::Role::Background);
            let watched = watch_tab(&tab, told, grace, &points, &park, &look_at, &acts);
            owed.store(told_again(told, &watched), Ordering::SeqCst);
            flag.store(false, Ordering::SeqCst);
            ring_bell();
        });
    match spawned {
        Ok(_) => Some((running, again)),
        Err(e) => {
            aterm_log::warn!("harness @{sid}: the upgrade's watch could not start: {e}");
            None
        }
    }
}

/// ONE WATCH of tab `sid`, the watch thread's whole body (design record
/// 2026-09-28, §3.2 C4). It hands the tab to [`Acts::watch`] with what the
/// worker's loop last offered and withheld ([`Points`]) and who looks
/// ([`build_word`]); a watch that wrote has the owner's view looked at
/// again ([`note_upgrade_act`]), which reads the record's next watch.
///
/// THE RE-PARK, at a watch its record's deadline brought (a told one's
/// worker already asked for a point: the attach does, and the activation
/// notice): an upgrade [`Acts::due`] reads due (`Due::Yes`) while the
/// worker asks for no point ([`WorkerJob::park`] clear) and no look is owed
/// ([`WorkerJob::look_at`] empty) is a worker that stopped asking — a false
/// `Due::No` at its last look (the agent was not the terminal's foreground
/// group), a word [`upgrade_drive::after`] read as the last, a look that
/// broke — and nothing but an activation notice would have asked again. The
/// park is set, and said: the loop's next point takes the step, under every
/// gate it has.
///
/// NO HANDS: nothing here types, signals or ends anything, past any budget —
/// `tools/grep_guard.sh` H1 fences this function and everything of this
/// file it reaches.
fn watch_tab(
    sid: &str,
    told: bool,
    grace: u32,
    points: &Points,
    park: &AtomicBool,
    look_at: &Mutex<Option<Instant>>,
    acts: &Acts,
) -> upgrade_drive::Watched {
    let (guard, point_at) = points.last();
    let how = upgrade_drive::Watcher {
        by: build_word(),
        // Only a guard said at or after the last point offered withheld the
        // LATEST point: one said before it is history, and naming it put
        // `guard=wall` beside a later point (the watch's review, 2026-09-28).
        guard: guard
            .filter(|(_, at)| *at >= point_at)
            .map(|(g, _)| g.word().to_string())
            .unwrap_or_default(),
        point_at,
        told,
    };
    let watched = (acts.watch)(sid, grace, &how);
    if watched.wrote() {
        aterm_log::info!("harness @{sid}: {}", watched.line());
        note_upgrade_act();
    } else if !matches!(
        watched.skipped.as_deref(),
        Some("no-record" | "not-due") | None
    ) {
        aterm_log::info!("harness @{sid}: {}", watched.line());
    }
    // A told watch parks nothing of its own: the worker's attach and the
    // activation notice that told it already asked for a point.
    let idle = !told
        && !park.load(Ordering::SeqCst)
        && look_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none();
    if idle && matches!((acts.due)(sid), Due::Yes) {
        park.store(true, Ordering::SeqCst);
        aterm_log::info!(
            "harness @{sid}: the watch found an upgrade due that no point was asked for \
             (the worker's last look read it otherwise); the next point takes its step"
        );
    }
    watched
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

/// A NAME THE ROSTER COULD NOT READ IS NO EXIT (2026-09-28). The roster
/// has no word for "could not read". A session drops out of it when its
/// publication names no supervised agent ([`agent_of`]): no agent program
/// by name AND no reader that identifies one. A resolution of the name that
/// failed replaces a good one (`SessionTimeline::set_program(None)`) but
/// leaves the reader alone, and [`agent_of`] falls back to the reader — so
/// a failed name ALONE drops nothing. What drops a live agent is a failed
/// name while no reader identifies it (none read yet, or one the screen no
/// longer reads), or a runtime-named agent (`node`) whose reader dropped;
/// either lasts until a later read names it again (the status sweep
/// retries a quarter second to five seconds apart, and the resolver's one
/// thread may be behind). A supervised agent the roster named once and
/// names no longer is therefore asked about a FACT ([`Acts::holds`], about
/// the group it was last named in): one that still holds its tab — that
/// group still the tab's foreground, its leader still there, nothing read
/// for it since that could not host it — is kept wanted, as the agent it
/// was named and at the stamp it was named with, so the pass services it
/// as any other: its worker runs on, and one that ended meanwhile (handed
/// over to a changed policy, or failed and found unwanted) is started
/// again. One that holds its tab no longer, or that cannot be read, is
/// forgotten and left to [`visit_workers`] (a read that fails decides
/// nothing, and keeps nothing), as is one whose exit its worker was already
/// handed, and one kept for [`HOLDS_KEEP_MAX`] already — the hold is no
/// exact oracle ([`holds`]), and a real misread has healed long before; one
/// its worker's own step is acting through is asked once the step is over,
/// as a keep begun afresh.
///
/// While one is kept the host looks again ([`hold_look`]). The look is the
/// BACKSTOP for an exit during the misread, not its only signal: a
/// runtime-named agent's exit rings as its group moves (its name `node`
/// goes, `SessionTimeline::note_foreground_group`), and a name-failed one's
/// rings as the shell's name resolves (`set_program`, from none to the
/// shell's). The move itself rings nothing for an agent with neither a name
/// nor a reader, and when the shell's name fails to resolve too nothing
/// rings at all: a look that never came would strand that exit. Neither
/// the rings nor the look stands in for the other.
///
/// Also records, for every agent the roster names, the stamp it names it
/// with — the group its worker's too ([`Hooks::still_wanted`]) — and hands a
/// worker its kept agent's exit when the roster names ANOTHER group in the
/// kept one's stead (an agent started in the tab before a look saw the kept
/// one go), no worker's step taken since the keep began: the worker's loop,
/// and what it remembers of the session's upgrade (an announcement's turn
/// end), were the kept agent's and are never carried on to another process.
/// A step that ended the agent relaunches it in a new group, and that one is
/// the step's to carry on: the worker's own stamp ([`WorkerIdle::stepped`],
/// left as each step begins) moved since the keep began. Only its own: the
/// process-wide [`UPGRADE_ACTS`] it once read moved with every tab's step and
/// with the window's look ([`look_at_upgrades_soon`]), and a keep through any
/// of them handed no exit (the review of 2026-09-28). The instant to look
/// again, if one is kept.
fn keep_holding(
    holding: &mut HashMap<String, Held>,
    workers: &Workers,
    wanted: &mut HashMap<String, Program>,
    stamps: &mut HashMap<String, FollowStamp>,
    hooks: &Hooks,
) -> Option<Instant> {
    for (sid, stamp) in stamps.iter() {
        let worker = workers.0.get(sid);
        if let Some(w) = worker
            && let Some(was) = holding.get(sid)
            && was
                .kept
                .is_some_and(|(_, then)| then == w.stepped.load(Ordering::SeqCst))
            && was.stamp.group != stamp.group
            && !w.acting.load(Ordering::SeqCst)
            && !w.stop.load(Ordering::SeqCst)
        {
            aterm_log::info!(
                "harness @{sid}: the agent kept in group {} is gone, group {} named in its stead \
                 before a look saw it go: its exit is handed",
                was.stamp.group,
                stamp.group
            );
            w.leave();
        }
        holding.insert(
            sid.clone(),
            Held {
                agent: wanted[sid],
                stamp: *stamp,
                look: None,
                kept: None,
            },
        );
        if let Some(w) = worker {
            w.group.store(stamp.group, Ordering::SeqCst);
        }
    }
    let now = Instant::now();
    let mut next: Option<Instant> = None;
    holding.retain(|sid, held| {
        if stamps.contains_key(sid) {
            return true;
        }
        let worker = workers.0.get(sid);
        if worker.is_some_and(|w| w.acting.load(Ordering::SeqCst)) {
            held.look = None;
            held.kept = None;
            return true;
        }
        if worker.is_some_and(|w| w.left.load(Ordering::SeqCst))
            || (hooks.acts.holds)(sid, held.stamp.group) != Some(true)
        {
            return false;
        }
        let stepped = worker.map_or(0, |w| w.stepped.load(Ordering::SeqCst));
        let (since, _) = *held.kept.get_or_insert((now, stepped));
        if now.saturating_duration_since(since) >= HOLDS_KEEP_MAX {
            aterm_log::info!(
                "harness @{sid}: the roster has not named its agent for {}s, still read to hold \
                 the tab (group {}): no misread lasts this long; kept no longer",
                HOLDS_KEEP_MAX.as_secs(),
                held.stamp.group
            );
            return false;
        }
        if held.look.is_none() {
            aterm_log::info!(
                "harness @{sid}: the roster could not name its agent, which still holds the tab \
                 (group {}): no exit; looked at again",
                held.stamp.group
            );
        }
        let (at, step) = hold_look(held.look, now);
        held.look = Some((at, step));
        wanted.insert(sid.clone(), held.agent);
        stamps.insert(sid.clone(), held.stamp);
        next = Some(next.map_or(at, |n| n.min(at)));
        true
    });
    next
}

/// When the host looks again at an agent [`keep_holding`] kept, `look` the
/// look it had set (`None`: kept just now): [`HOLDS_LOOK`] after it was
/// first kept — as long as the look at an exit already leaves open
/// ([`relaunch::EXIT_SETTLE`]), so an exit during a short misread that no
/// bell rang for (neither the agent's name nor the shell's read:
/// [`keep_holding`]) is seen within the window the reading of what it left
/// is built for — then each look that finds it still kept twice as far on,
/// to [`HOLDS_LOOK_MAX`]: a misread that lasts (a runtime whose name reads
/// as its interpreter's while its reader is gone) costs a pass every few
/// seconds, not four a second, until [`HOLDS_KEEP_MAX`] ends the keep. A
/// pass before the look came due (another bell) keeps it where it was.
/// `(when, its step)`.
fn hold_look(look: Option<(Instant, Duration)>, now: Instant) -> (Instant, Duration) {
    match look {
        None => (now + HOLDS_LOOK, HOLDS_LOOK),
        Some((at, step)) if at <= now => {
            let step = step.saturating_mul(2).min(HOLDS_LOOK_MAX);
            (now + step, step)
        }
        Some(look) => look,
    }
}

/// The first look at an agent [`keep_holding`] kept ([`hold_look`]).
const HOLDS_LOOK: Duration = relaunch::EXIT_SETTLE;

/// The longest step between the looks at an agent still kept ([`hold_look`]).
/// An exit that no bell rang for during a misread this long is seen this
/// late at most — past [`relaunch::EXIT_SETTLE`], the window within which
/// another Claude Code's start could remove a crash's record (read then as
/// graceful, and not relaunched), which that constant's doc names — the
/// price of a host that does not wake four times a second through a long
/// misread. Only an exit that rang nothing at all is late by it: the agent's
/// or the shell's name ringing is seen at once ([`keep_holding`]).
const HOLDS_LOOK_MAX: Duration = Duration::from_secs(2);

/// The longest the host keeps an agent the roster does not name
/// ([`keep_holding`]). A misread heals as the status sweep names the agent
/// again, its retries a quarter second to five seconds apart, so a keep
/// this long is no misread's: it is a hold that reads true of something
/// that may not be the agent ([`holds`]: a runtime in its group that
/// outlived it, a leader not yet reaped, a recycled pid behind a foreground
/// group that is gone), which must not hold supervision — and the worker's
/// claim on the session — for as long as it runs. The keep then ends and
/// the exit is handed as before the keep existed: for an agent that really
/// was still running through so long a misread, the behaviour before
/// 2026-09-28, not a new one.
const HOLDS_KEEP_MAX: Duration = Duration::from_secs(30);

/// Each worker, against what the session is now: one whose agent is no
/// longer wanted — neither named by the roster nor still holding its tab
/// ([`keep_holding`]) — is stopped, or handed the exit when the agent LEFT a
/// tab that lives on, unless its own upgrade step is running (an exit that
/// step leaves behind is handed once it is over, and carried as the
/// harness's own restart's, [`on_agent_left`]); one whose agent is back has
/// its exit over, one under an older policy hands over, and one told of a new
/// activation, or whose look is due, is asked for its loop's next idle point.
/// The earliest look still to come.
fn visit_workers(
    shared: &Shared,
    workers: &mut Workers,
    told: &mut BTreeSet<String>,
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
            // The agent a worker's own upgrade step ends is that step's while
            // it runs: it comes back relaunched, or — its relaunch waited —
            // is handed at the next pass and carried as the harness's own.
            // Otherwise an agent that left a tab that lives on (it holds the
            // tab no longer: [`keep_holding`] kept every one that does) is the
            // worker's to handle; a tab that closed (or a policy that stopped)
            // just stops it. Each is asked once: a stop rings the bell, and one
            // asked again at each pass woke the host at once through the
            // worker's whole wind-down. A worker handed its exit is still
            // stopped when its tab closes (the stop clears `left`: its
            // relaunch's pause ends).
            if !w.acting.load(Ordering::SeqCst) {
                if active && (hooks.acts.open)(sid) {
                    if !w.stop.load(Ordering::SeqCst) {
                        w.leave();
                    }
                } else if w.left.load(Ordering::SeqCst) || !w.stop.load(Ordering::SeqCst) {
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
        } else if w.cfg_gen != cfg_gen && !w.handover.load(Ordering::SeqCst) {
            // Once, as a stop above.
            w.hand_over();
        }
        // A build installed mid-session (N3): the session is behind from the
        // notice, not from its next idle point — a busy turn's length later.
        if w.activations != activations {
            w.activations = activations;
            w.park.store(true, Ordering::SeqCst);
            if shared.switches.upgrade() && upgrades(wanted[sid]) {
                owe_note(&w.note, crate::upgrade_host::now_s());
                // And a watch told (design record 2026-09-28, §3.2 C3): a
                // record behind the old build is retargeted onto the new one
                // without waiting for a point.
                told.insert(sid.clone());
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
/// claim not yet released, and not held off for its adopted-claim grace
/// ([`ADOPTED_CLAIM_GRACE`]) — under the one hold of the state lock that
/// re-reads the policy and publishes the stop handles: a suspend() either
/// came first (nothing starts) or finds every worker started here. The
/// instant the next grace runs out, for the host's timed wake (no poll).
fn start_workers(
    shared: &Shared,
    host: &mut HostState,
    wanted: &HashMap<String, Program>,
    cfg: &SupervisorConfig,
    cfg_gen: u64,
    hooks: &Hooks,
) -> Option<Instant> {
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
    // THE ADOPTED-CLAIM GRACE (round four, item 9): a session an update
    // handed over is not claimed while an external supervisor that held it
    // may still be on its way to claiming it again.
    let now = Instant::now();
    if !state.suspended {
        state.claim_grace.start(now);
    }
    let grace_at = state.claim_grace.prune(now);
    starts.retain(|sid| !state.claim_grace.holds(sid, now));
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
        // A worker started for a kept agent takes the stamp the keep began
        // at: its predecessor's step before the keep is no step since.
        let stepped = Arc::new(AtomicU64::new(
            host.holding
                .get(sid.as_str())
                .and_then(|h| h.kept)
                .map_or(0, |(_, then)| then),
        ));
        let stalled: Arc<AtomicBool> = Arc::default();
        let note: Note = Arc::default();
        let group = Arc::new(AtomicI32::new(
            host.holding.get(sid.as_str()).map_or(0, |h| h.stamp.group),
        ));
        let points: Arc<Points> = Arc::default();
        let idle = Arc::new(WorkerIdle {
            sid: sid.clone(),
            agent: wanted[sid],
            grace: cfg.human_grace_s,
            park: Arc::clone(&park),
            look_at: Arc::clone(&look_at),
            acting: Arc::clone(&acting),
            stepped: Arc::clone(&stepped),
            stalled: Arc::clone(&stalled),
            switches: Arc::clone(&shared.switches),
            kept: Arc::clone(&kept),
            hooks: hooks.clone(),
            run: Mutex::new(UpgradeRun {
                activations,
                ..UpgradeRun::default()
            }),
            activations: Arc::clone(&shared.activations),
            owns: AtomicBool::new(false),
            background_at: Mutex::default(),
            tasked: Mutex::default(),
            note: Arc::clone(&note),
            clock_hold: Mutex::default(),
            net: Arc::clone(&shared.net),
            route: Mutex::default(),
            points: Arc::clone(&points),
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
            group: Arc::clone(&group),
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
                    stepped,
                    activations,
                    note,
                    group,
                    points,
                },
            );
            // A worker's start — a hot swap's, an attach's — is a watch told
            // (design record 2026-09-28, §3.2 C3): a record an older build
            // left is read, and retargeted, without waiting for a point.
            if upgrades(wanted[sid]) {
                host.watches.told.insert(sid.clone());
            }
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
    grace_at
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

/// Whether `sid`'s agent still HOLDS its tab ([`Acts::holds`]): the tab's
/// foreground group, read from its PTY master now, is `group`, that group's
/// leader EXISTS (`kill(pid, 0)`: a job-control job's group id is its
/// leader's pid), and no name read for the group since is one that could
/// not host a supervised agent — an `exec` in place into a shell is a
/// reading, and a reading decides. `None` when it cannot be read: no group
/// named yet, no such tab, no foreground group to read (a ConPTY has none).
/// The store's lock is held only to clone the tab's handle: an OS probe
/// never holds the registry (`cmd_who`'s rule); a tab closed meanwhile reads
/// no group.
///
/// The leader's conjunct is load-bearing: a terminal goes on naming a group
/// that is gone as its foreground until its session leader (the shell)
/// takes it back — measured on macOS 2026-09-28, the dead group's id read
/// back from `tcgetpgrp` with every process of it reaped. It is NOT an
/// exact liveness oracle: an agent that exited but is not yet reaped by its
/// parent is a zombie, and `kill(pid, 0)` finds a zombie, so it reads
/// `Some(true)` until the shell reaps it and takes the terminal back (a
/// fraction of a second; the keep's next look then ends it). Neither is a
/// recycled leader pid behind a stale foreground group told apart, nor a
/// runtime (`node`, `bun`) that outlives the agent it hosted in the same
/// group: the keep's ceiling ([`HOLDS_KEEP_MAX`]) is what bounds those.
/// `the_real_hold_reads_the_tabs_foreground_and_its_leader` and
/// `the_hold_reads_the_groups_leader_and_a_zombie_still_reads_held` bind
/// each case on a real terminal.
fn holds(store: &Store, sid: &str, group: i32) -> Option<bool> {
    let leader = u32::try_from(group).ok().filter(|g| *g > 0)?;
    let h = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .cloned()?;
    if h.state == SessionState::Exited {
        return Some(false);
    }
    let read_other = {
        let tl = h
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let agent = tl.agent();
        agent.program_pgid == group
            && agent_of(agent).is_none()
            && agent
                .program
                .as_deref()
                .is_some_and(|p| !aterm_phase::may_host_agent(p))
    };
    if read_other {
        return Some(false);
    }
    let fg = crate::quit_safety::foreground_pgrp(h.master);
    (fg > 0).then(|| fg == group && aterm_uds::process::pid_alive(leader))
}

/// Whether `sid` is still open (not closed, not exited).
fn open(store: &Store, sid: &str) -> bool {
    let guard = store.read().unwrap_or_else(PoisonError::into_inner);
    guard
        .by_sid(&aterm_session::SessionId::new(sid))
        .is_some_and(|h| h.state != SessionState::Exited)
}

/// Whether session `sid` is still its worker's after a failed run
/// ([`Hooks::still_wanted`]): its publication names a supervised agent
/// ([`still_supervisable`]), or its agent still holds its tab in `group`
/// ([`holds`]).
fn still_wanted(store: &Store, sid: &str, group: i32) -> bool {
    still_supervisable(store, sid) || holds(store, sid, group) == Some(true)
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
    let holds_store = store.clone();
    let carry_store = store.clone();
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
    let (o6, o7, o8, o9, o10, o11, o12, o13, o14) = (
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
    );
    let (o15, o16, o17, o18) = (
        Arc::clone(&opts),
        Arc::clone(&opts),
        Arc::clone(&opts),
        opts,
    );
    Acts {
        open: Arc::new(move |sid| open(&store, sid)),
        holds: Arc::new(move |sid, group| holds(&holds_store, sid, group)),
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
        exit_look: Arc::new(move |sid, snap| {
            // A Codex's exit is its shell's word, over the control socket.
            if snap.codex.is_some() {
                return o13(sid, 0).map(|o| relaunch::look_at_codex_exit(&o, snap));
            }
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
        restarted: Arc::new(move |sid, codex, pid| {
            o15(sid, 0).is_some_and(|o| relaunch::restarted(&o, codex, pid))
        }),
        carry: Arc::new(move |sid, grace, codex, pid| {
            let Some(o) = o16(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::carry_restart(&o, codex, pid);
            aterm_log::info!("harness @{sid}: {}", r.line_as("carry-restart"));
            r.step
        }),
        relaunch_restored: Arc::new(move |sid, snap, upgrade| {
            let Some(o) = o14(sid, 0) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::after_host_ended(&o, snap, upgrade);
            aterm_log::info!("harness @{sid}: {}", r.line_as("relaunch-after-host-ended"));
            r.step
        }),
        carry_in_flight: Arc::new(move |sid, grace| {
            let o = o17(sid, grace)?;
            carry_in_flight_now(
                || !upgrade_drive::in_flight(&o).is_empty(),
                || still_supervisable(&carry_store, sid),
                || {
                    let r = upgrade_drive::step(&o);
                    aterm_log::info!("harness @{sid}: {}", r.line_as("carry-on-after-update"));
                    r.step
                },
            )
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
        route: Arc::new(crate::harness_netprobe::route_of_agent),
        watch: Arc::new(move |sid, grace, how| watch_off_any_point(o18(sid, grace), sid, how)),
    }
}

/// THE PRODUCT'S [`Acts::watch`], as [`live_acts`] wires it: the tab handed
/// to [`upgrade_drive::watch`] and nothing else. A named function, not a
/// closure, so `tools/grep_guard.sh` H1 can fence it (the watch's review,
/// 2026-09-28: the fence stopped at `(acts.watch)(…)` in [`watch_tab`], and
/// the tests drive their own `Acts`, so an edit here that called
/// `upgrade_drive::step` — a real visit, typing at the next read — or a
/// relaunch would have passed both). H1 holds it to an ALLOW-LIST: the one
/// call it may make is `upgrade_drive::watch`. No home or state directory:
/// nothing is watched (said as `no-home`).
fn watch_off_any_point(
    opts: Option<upgrade_drive::Opts>,
    sid: &str,
    how: &upgrade_drive::Watcher,
) -> upgrade_drive::Watched {
    let Some(o) = opts else {
        return upgrade_drive::Watched {
            skipped: Some("no-home".to_string()),
            ..upgrade_drive::Watched::default()
        };
    };
    upgrade_drive::watch(&o, sid, how)
}

/// THE DISK WATCH over the store ([`Hooks::disk`]): each look takes the
/// supervised sessions' working directories (what their shells last reported,
/// OSC 7) and, off the host thread and one look at a time, hands the build
/// directories found there to `aterm_agent::harness::cli::disk_tick` — which,
/// below `[disk] auto_free_gib` of free space on the home volume, reclaims
/// the ones ON THAT VOLUME (one on another volume would free nothing there):
/// in each build directory a build tool laid, the `incremental/` of every
/// cargo profile idle past `target_stale_days` (one day unless the file
/// says otherwise), then — least recently used first, one profile at a time,
/// the home volume's free space measured again before each — recent ones
/// until it is back at the floor plus a GiB, or until the bytes they
/// released cover what it was short (a snapshot can keep the figure down);
/// never a profile a build holds or one written into within the last ten
/// minutes. Each is deleted under
/// cargo's own locks — never the directory or anything else in it —
/// journalling each removal's intent before it and its outcome after (and
/// where the pass stopped) in `<harness state>/disk.jsonl`. Above the floor
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
/// Whether each holds idle caches to reclaim is `disk_tick`'s witness to find;
/// `target.noindex` is safe to name, since the reclaim never removes the
/// directory a checkout's `target -> target.noindex` link needs.
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
    // The reclaim: a profile's `incremental/`, under cargo's locks, on the
    // volume measured and nowhere else — the idle ones, then recent ones
    // oldest first while `statvfs` says the home volume is still short.
    let judge = aterm_agent::harness::disk::Judge {
        now,
        threshold_days: config.target_stale_days,
        device: aterm_agent::harness::disk::target::dev_of(&home),
    };
    let tick = aterm_agent::harness::cli::disk_tick(
        &look,
        config,
        &mut aterm_agent::harness::disk::remover(judge),
        &mut || atpkg::freespace::available_bytes(&home),
    );
    if let aterm_agent::harness::cli::DiskTick::Reclaimed(done) = tick
        && !done.removed.is_empty()
    {
        aterm_log::info!(
            "harness disk watch: below {} GiB free, reclaimed incremental caches in {} row{} ({}); \
             each row is in {}",
            config.auto_free_gib,
            done.removed.len(),
            if done.removed.len() == 1 { "" } else { "s" },
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
            return ViewNext::default();
        }
        let Some((home, state)) = place.clone() else {
            return ViewNext::default();
        };
        let at = view.refresh(
            &upgrade_drive::Opts {
                home,
                state,
                sock: Some(sock.clone()),
                only_sid: None,
                dry_run: false,
                human_grace_s: 0,
                hand_back: true,
                background: false,
                // Where each tab's loop keeps its ledger — and the tab's goal
                // record beside it, a Codex goal the move paused
                // (`goal_hold`): what the row reads of it.
                aterm_state: aterm_agent::operator::default_state_root().ok(),
            },
            &mut hand,
        );
        ViewNext {
            at,
            follows: view.follows().to_vec(),
            watch: view.watch_due().to_vec(),
        }
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
            still_wanted: Arc::new(move |sid, group| still_wanted(&s2, sid, group)),
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
///
/// Then, in the SAME thread — both passes rewrite the one file, and two writers
/// racing on it could each back up and replace the other's result — add the
/// Claude Code defaults the owner runs with where they are UNSET
/// ([`aterm_primer::CLAUDE_DEFAULTS`]: the top-effort mode, `xhigh`, the survey
/// off),
/// leaving any value a person set by hand exactly as it is.
pub(crate) fn sweep_legacy_hooks() {
    let Some(home) = aterm_primer::home_dir() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}hook-sweep"))
        .spawn(move || {
            match aterm_primer::remove_aterm_hooks_in(&home) {
                Ok(aterm_primer::HookRemoval::Nothing) => {}
                Ok(removal) => {
                    aterm_log::info!("harness: legacy Claude hooks removed: {removal:?}");
                }
                Err(e) => aterm_log::warn!("harness: legacy Claude hooks left in place: {e}"),
            }
            match aterm_primer::add_claude_defaults_in(&home) {
                Ok(aterm_primer::DefaultsWrite::Nothing) => {}
                Ok(added) => aterm_log::info!("harness: Claude Code defaults added: {added:?}"),
                Err(e) => aterm_log::warn!("harness: Claude Code defaults not added: {e}"),
            }
        });
}

#[cfg(test)]
impl Acts {
    /// Acts over no real session: every tab closed (so no agent holds one),
    /// no upgrade due, no snapshot, nobody typing, and a step or relaunch
    /// that answers nothing it could act on.
    pub(crate) fn inert() -> Self {
        Acts {
            open: Arc::new(|_| false),
            holds: Arc::new(|_, _| None),
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
            restarted: Arc::new(|_, _, _| false),
            carry: Arc::new(|_, _, _, _| "refused:inert".to_string()),
            relaunch_restored: Arc::new(|_, _, _| "refused:inert".to_string()),
            carry_in_flight: Arc::new(|_, _| None),
            restart: Arc::new(|_, _, _| "refused:inert".to_string()),
            tasked: Arc::new(|_, _, _| None),
            hold: Arc::new(|_, _| true),
            route: Arc::new(|_, _, _| Route::Custom("inert".to_string())),
            watch: Arc::new(|_, _, _| upgrade_drive::Watched::default()),
        }
    }
}

#[cfg(test)]
impl HostHandle {
    /// A host over no real session — nothing supervised, every act inert —
    /// whose restored relaunch is `act`: another module's test of what a
    /// handoff carries from the restored queue drives it (round four, plan
    /// item 7).
    pub(crate) fn for_restored_test(act: Arc<RestoredFn>) -> Self {
        let mut acts = Acts::inert();
        acts.relaunch_restored = act;
        let hooks = Hooks {
            roster: Arc::new(Vec::new),
            still_wanted: Arc::new(|_, _| false),
            body: Arc::new(|_| BodyEnd::Stopped),
            badge: Arc::new(|_, _| {}),
            claim_epoch: Arc::new(|| 0),
            acts,
            backoff: Arc::new(|_| Duration::from_millis(10)),
            pause: Arc::new(|pause| pause / 1000),
            upgrade_view: Arc::new(|_| ViewNext::default()),
            disk: Arc::new(|_| {}),
        };
        Self::start(SupervisorConfig::default(), false, false, hooks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The doctor reports the defaults the window WRITES: `atpkg`'s mirror of the
    /// table (its values JSON text, where aterm-primer's are typed; it takes only
    /// the top-effort key's spelling from aterm-primer) must say exactly what
    /// `aterm_primer::CLAUDE_DEFAULTS` adds — same keys, same order, same values.
    #[test]
    fn the_doctors_claude_defaults_are_the_ones_the_window_writes() {
        let written: Vec<(&str, String)> = aterm_primer::CLAUDE_DEFAULTS
            .iter()
            .map(|(k, v)| {
                let text = match v {
                    aterm_primer::ClaudeDefault::Bool(b) => b.to_string(),
                    aterm_primer::ClaudeDefault::Str(s) => format!("\"{s}\""),
                    aterm_primer::ClaudeDefault::Number(n) => (*n).to_string(),
                };
                (*k, text)
            })
            .collect();
        let reported: Vec<(&str, String)> = atpkg::doctor::CLAUDE_DEFAULTS
            .iter()
            .map(|(k, v)| (*k, (*v).to_string()))
            .collect();
        assert_eq!(reported, written);
    }

    /// The doctor holds back the effort defaults by the window's OWN rule: atpkg's
    /// mirror names the same effort defaults as `aterm_primer::CLAUDE_EFFORT_DEFAULTS`,
    /// and reads the same hand-set efforts out of every shape below — a per-model
    /// and a top-level effort, a hand-set `xhigh` (which holds nothing back), a
    /// duplicate key, model-name order, a model setting with no effort, and text
    /// that is not a settings object. A drift either way would have the doctor
    /// promise a key the window never adds, or stay quiet about one it does.
    #[test]
    fn the_doctors_effort_rule_is_the_one_the_window_applies() {
        assert_eq!(
            atpkg::doctor::CLAUDE_EFFORT_DEFAULTS,
            aterm_primer::CLAUDE_EFFORT_DEFAULTS
        );
        let top_effort_beside_a_null_effort = format!(
            r#"{{"effortLevel": null, "{}": true}}"#,
            aterm_primer::CLAUDE_TOP_EFFORT_KEY
        );
        for text in [
            "{}",
            r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "medium"}}}"#,
            r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "xhigh"}}}"#,
            r#"{"effortLevel": "medium"}"#,
            r#"{"effortLevel": "xhigh"}"#,
            r#"{"effortLevel": "medium", "effortLevel": "xhigh"}"#,
            r#"{"effortLevel": "xhigh", "effortLevel": "low"}"#,
            r#"{"modelSettings": {"z": {"effortLevel": "low"}, "a": {"effortLevel": "high"}}}"#,
            r#"{"modelSettings": {"m": {"effortLevel": "low"}, "m": {"maxEffortLevel": "high"}}}"#,
            r#"{"modelSettings": {"m": {"maxEffortLevel": "high"}, "n": "low"}}"#,
            r#"{"modelSettings": "medium", "effortLevel": 3}"#,
            top_effort_beside_a_null_effort.as_str(),
            r#"["effortLevel"]"#,
            "not json",
            "",
        ] {
            assert_eq!(
                atpkg::doctor::claude_efforts_set_by_hand(text),
                aterm_primer::claude_efforts_set_by_hand(text),
                "{text}"
            );
        }
    }
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
            still_wanted: Arc::new(move |sid, _| {
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
            upgrade_view: Arc::new(|_| ViewNext::default()),
            disk: Arc::new(|_| {}),
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

    /// Waits for something that must happen. The minute is a hang detector,
    /// not a latency budget: these waits pass in well under a second alone.
    /// (The "not within … s: the successor runs" failures of
    /// `the_real_host_conforms_to_the_worker_lifecycle_model` under parallel
    /// runs were no slow host: its projection took a held worker for the
    /// successor a release had started, so the wait counted one worker short
    /// and never ended — round 32, fixed in `Probe::project`.)
    fn until(what: &str, pred: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !pred() {
            assert!(Instant::now() < deadline, "not within 60 s: {what}");
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
            codex: None,
            dialect: None,
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
            host.relaunch_restored(
                ["s-a", "s-b"]
                    .map(|sid| RestoredAgent {
                        sid: sid.to_string(),
                        snap: snap(sid),
                        place: "in tab 1".to_string(),
                    })
                    .into(),
            );
            (host, seen)
        };
        let (host, seen) = run(on(), false);
        assert!(
            host.relaunches_restored(),
            "the reopened row reads the same gate"
        );
        until("both relaunched", || seen.lock().unwrap().len() == 2);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(*seen.lock().unwrap(), ["s-a", "s-b"], "once each");

        let mut limited = on();
        limited.set("relaunch", "false").unwrap();
        let (host, seen) = run(limited, false);
        assert!(!host.relaunches_restored());
        std::thread::sleep(Duration::from_millis(100));
        assert!(seen.lock().unwrap().is_empty(), "relaunch = false");
        let (host, seen) = run(on(), true);
        assert!(!host.relaunches_restored());
        std::thread::sleep(Duration::from_millis(100));
        assert!(seen.lock().unwrap().is_empty(), "headless");
        let (host, _) = run(off(), false);
        assert!(!host.relaunches_restored(), "[harness] off");
    }

    /// A tab whose relaunched agent still has not registered its conversation
    /// cannot consume its retry pause before another tab's first relaunch.
    /// Its own in-flight step is still retried later; neither is typed twice
    /// after it succeeds.
    #[test]
    fn a_waiting_restored_tab_does_not_delay_another_tabs_first_step() {
        let world = Arc::new(World::default());
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let first = Arc::new(AtomicUsize::new(0));
        let mut h = hooks(&world, parking_body(&world));
        let (calls, tries) = (Arc::clone(&seen), Arc::clone(&first));
        h.acts.relaunch_restored = Arc::new(move |sid, _, _| {
            calls.lock().unwrap().push(sid.to_string());
            if sid == "s-a" && tries.fetch_add(1, Ordering::SeqCst) == 0 {
                std::thread::sleep(Duration::from_millis(50));
                "wait:resume"
            } else {
                "adopted"
            }
            .to_string()
        });
        let host = HostHandle::start(on(), false, false, h);
        let snap = |sid: &str| Snapshot {
            tab: sid.to_string(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".to_string(),
            shell: 4343,
            program: std::path::PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".to_string()),
            cwd: "/".to_string(),
            version: None,
            codex: None,
            dialect: None,
        };
        host.relaunch_restored(
            ["s-a", "s-b"]
                .map(|sid| RestoredAgent {
                    sid: sid.to_string(),
                    snap: snap(sid),
                    place: "in tab 1".to_string(),
                })
                .into(),
        );
        until("both first steps", || seen.lock().unwrap().len() >= 2);
        assert_eq!(seen.lock().unwrap()[..2], ["s-a", "s-b"]);
        until("the waiting tab retried", || {
            seen.lock().unwrap().len() == 3
        });
        assert_eq!(*seen.lock().unwrap(), ["s-a", "s-b", "s-a"]);

        // Tier-1: map the real worker's call order onto the model's actions.
        // The former nested loop enables RetryA immediately after FirstA;
        // this worker chose FirstB instead.
        let model = aterm_spec::derive::harness_restored_first_attempt_model();
        let mut state = model.init_state();
        let mut first_a = false;
        for sid in seen.lock().unwrap().iter() {
            let action = match sid.as_str() {
                "s-a" if !first_a => {
                    first_a = true;
                    "FirstA"
                }
                "s-a" => "RetryA",
                "s-b" => "FirstB",
                _ => panic!("unexpected restored tab {sid}"),
            };
            assert!(model.fire(action, &mut state), "{action}: {state:?}");
            if action == "FirstA" {
                assert!(!model.action_enabled("RetryA", &state));
                let buggy = aterm_spec::interp::with_buggy(&model, 1);
                assert!(buggy.action_enabled("RetryA", &state));
            }
        }
    }

    /// Shorter host steps must not exhaust the old in-flight window after
    /// eight fast `wait:resume` reads. Other waits retain the bounded budget,
    /// and a typed failure never retries.
    #[test]
    fn a_restored_in_flight_relaunch_stays_eligible_until_stale() {
        let waiting = Outcome::NotYet("wait:resume".to_string());
        assert!(restored_retry(
            "wait:resume",
            &waiting,
            RESTORED_TRIES,
            Duration::from_secs(relaunch::STALE_S - 1)
        ));
        assert!(!restored_retry(
            "wait:resume",
            &waiting,
            RESTORED_TRIES,
            Duration::from_secs(relaunch::STALE_S)
        ));
        assert!(!restored_retry(
            "wait:shell-prompt",
            &Outcome::NotYet("wait:shell-prompt".to_string()),
            RESTORED_TRIES,
            Duration::ZERO
        ));
        assert!(!restored_retry(
            "failed:no-resume",
            &Outcome::NotYet("failed:no-resume".to_string()),
            1,
            Duration::ZERO
        ));
    }

    /// RULING 293: an agent the reopened row said resumes and that did not
    /// come back is said ONCE, in one row, once every restored tab is tried:
    /// here tab 2's shell was gone (`refused:shell-gone`). NEGATIVE CONTROLS
    /// in the same pass: tab 1's relaunch landed (`adopted`) and tab 3's agent
    /// had no conversation (the reopened row already counted it lost), so
    /// neither is in the row.
    #[test]
    fn a_restored_agent_that_did_not_come_back_is_said_once() {
        let _lane = crate::message_inbox::lane_test_guard();
        let _ = crate::message_inbox::take_queued();
        let snap = |tab: &str, session: Option<&str>| Snapshot {
            tab: tab.to_string(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".to_string(),
            shell: 4343,
            program: std::path::PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: session.map(str::to_string),
            cwd: "/".to_string(),
            version: None,
            codex: None,
            dialect: None,
        };
        let world = Arc::new(World::default());
        let tried: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut h = hooks(&world, parking_body(&world));
        let t = Arc::clone(&tried);
        h.acts.relaunch_restored = Arc::new(move |sid, _, _| {
            t.lock().unwrap().push(sid.to_string());
            if sid == "s-a" {
                "adopted"
            } else {
                "refused:shell-gone"
            }
            .to_string()
        });
        let host = HostHandle::start(on(), false, false, h);
        let conversation = Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c");
        host.relaunch_restored(
            [
                ("s-a", conversation, 1),
                ("s-b", conversation, 2),
                ("s-c", None, 3),
            ]
            .map(|(sid, session, tab)| RestoredAgent {
                sid: sid.to_string(),
                snap: snap(sid, session),
                place: format!("in tab {tab}"),
            })
            .into(),
        );
        until("all three tried", || tried.lock().unwrap().len() == 3);
        let ours = || {
            crate::message_inbox::take_queued()
                .into_iter()
                .filter(|m| {
                    m.message.key.as_deref() == Some(crate::message_reporters::KEY_RESTORED_AGENTS)
                })
                .collect::<Vec<_>>()
        };
        let mut rows = restored_rows(Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(100));
        rows.extend(ours());
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0].message;
        assert_eq!(row.title, "Couldn't resume Claude in tab 2");
        assert!(
            row.detail
                .iter()
                .all(|l| !l.contains("tab 1") && !l.contains("tab 3")),
            "{:?}",
            row.detail
        );
    }

    /// DAY SIX, D30: a restored tab's relaunch that was TYPED AND DID NOT
    /// TAKE (`failed:no-resume`: the agent started and ended without its
    /// conversation) is tried once — another try types the same line into the
    /// tab again — and said at once, its reason true (D31) and the remedy in
    /// the plain sentence (D32). NEGATIVE CONTROLS: a step not possible yet
    /// (`wait:shell-prompt`) is tried again, up to [`RESTORED_TRIES`]; so is
    /// another actor on the lock; a landed relaunch is not.
    #[test]
    fn a_restored_relaunch_typed_that_did_not_take_is_tried_once_and_said_at_once() {
        assert!(!restored_again(
            "failed:no-resume",
            &relaunch::outcome("failed:no-resume")
        ));
        assert!(!restored_again(
            "failed:relaunch:ERR-busy",
            &relaunch::outcome("failed:relaunch:ERR-busy")
        ));
        for again in ["wait:shell-prompt", "wait:resume", "busy:another-sweep"] {
            assert!(restored_again(again, &relaunch::outcome(again)), "{again}");
        }
        assert!(!restored_again("adopted", &relaunch::outcome("adopted")));

        let _lane = crate::message_inbox::lane_test_guard();
        let _ = crate::message_inbox::take_queued();
        let world = Arc::new(World::default());
        let tried = Arc::new(AtomicUsize::new(0));
        let mut h = hooks(&world, parking_body(&world));
        let t = Arc::clone(&tried);
        h.acts.relaunch_restored = Arc::new(move |_, _, _| {
            t.fetch_add(1, Ordering::SeqCst);
            "failed:no-resume".to_string()
        });
        let host = HostHandle::start(on(), false, false, h);
        host.relaunch_restored(vec![RestoredAgent {
            sid: "s-a".to_string(),
            snap: Snapshot {
                tab: "s-a".to_string(),
                pid: 4242,
                start: "Sat Sep 27 01:02:03 2026".to_string(),
                shell: 4343,
                program: std::path::PathBuf::from("/opt/claude/bin/claude"),
                argv: vec!["/opt/claude/bin/claude".to_string()],
                session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".to_string()),
                cwd: "/".to_string(),
                version: None,
                codex: None,
                dialect: None,
            },
            place: "in tab 1".to_string(),
        }]);
        let rows = restored_rows(Duration::from_secs(60));
        // SAID WITHOUT A PAUSE — BY ORDER, not by a clock whose bound was the
        // pause itself (1 s, which a loaded machine crossed on the pass). The
        // queue's worker says its row only once its queue is empty
        // (`relaunch_restored_queue`), and a retry is queued, its pause owed,
        // before the queue can empty: a row seen with one try on the counter
        // and nothing owed was said right after that one try, with no retry
        // and so no pause. D30's defect retries the typed line a
        // RESTORED_FIRST_PAUSE later and says the row after the last of
        // RESTORED_TRIES tries — past this minute's hang detector.
        assert_eq!(
            (rows.len(), tried.load(Ordering::SeqCst)),
            (1, 1),
            "said once, after one try with no retry: {rows:?}"
        );
        assert!(
            host.pending_restored().is_empty(),
            "nothing is owed a retry"
        );
        // A real negative check: nothing types the line again afterwards.
        std::thread::sleep(RESTORED_FIRST_PAUSE + Duration::from_millis(200));
        assert_eq!(tried.load(Ordering::SeqCst), 1, "never typed again");
        let row = &rows[0].message;
        assert_eq!(row.title, "Couldn't resume Claude in tab 1");
        assert_eq!(
            row.detail[0],
            "Claude started in the tab but did not pick its conversation up after aterm \
             stopped: type claude --resume there to pick it up again"
        );
    }

    fn restored(sid: &str, place: &str) -> RestoredAgent {
        RestoredAgent {
            sid: sid.to_string(),
            snap: Snapshot {
                tab: sid.to_string(),
                pid: 4242,
                start: "Sat Sep 27 01:02:03 2026".to_string(),
                shell: 4343,
                program: std::path::PathBuf::from("/opt/claude/bin/claude"),
                argv: vec!["/opt/claude/bin/claude".to_string()],
                session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".to_string()),
                cwd: "/".to_string(),
                version: None,
                codex: None,
                dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
            },
            place: place.to_string(),
        }
    }

    /// ROUND FOUR, PLAN ITEM 7 (b): A PARK STOPS THE RESTORED QUEUE AND HANDS
    /// IT ON. Both restored tabs' shells are still starting (`NotYet`); the
    /// handoff parks. From the pause no step STARTS — the retry each was owed
    /// a second later never runs — and the queue still names both agents,
    /// which is what the handoff layout carries on their leaves
    /// ([`HostHandle::pending_restored_for`]). A rollback lets it run again.
    ///
    /// RED before the change: the queue was the worker's local `VecDeque`, so
    /// there was nothing to pause and nothing to read — the park never
    /// stopped a step, and the handoff leaf always said `agent: None`
    /// (`App::view_restore_descriptor_carrying`), so a Commit dropped both.
    #[test]
    fn a_park_during_restored_relaunch_hands_the_queue_on() {
        let world = Arc::new(World::default());
        let tried: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut h = hooks(&world, parking_body(&world));
        let t = Arc::clone(&tried);
        h.acts.relaunch_restored = Arc::new(move |sid, _, _| {
            t.lock().unwrap().push(sid.to_string());
            "wait:shell-prompt".to_string()
        });
        let host = HostHandle::start(on(), false, false, h);
        host.relaunch_restored(vec![
            restored("s-a", "in tab 1"),
            restored("s-b", "in tab 2"),
        ]);
        until("both first steps", || tried.lock().unwrap().len() >= 2);
        host.pause_restored();
        let at_pause = tried.lock().unwrap().len();
        // Each retry was due RESTORED_FIRST_PAUSE after its first step.
        std::thread::sleep(RESTORED_FIRST_PAUSE + Duration::from_millis(400));
        assert_eq!(
            tried.lock().unwrap().len(),
            at_pause,
            "no step starts while the terminal is parked"
        );
        assert_eq!(host.pending_restored(), ["s-a", "s-b"]);
        assert!(
            host.pending_restored_for("s-a")
                .is_some_and(|snap| snap.tab == "s-a" && snap.pid == 4242),
            "the handoff layout carries what the tab is owed"
        );
        assert!(host.pending_restored_for("s-c").is_none(), "owed nothing");
        // The rollback: the queue is this process's again.
        host.resume_restored();
        until("a retry after the rollback", || {
            tried.lock().unwrap().len() > at_pause
        });
    }

    /// ROUND FOUR, PLAN ITEM 7 (a): THE AUTOMATIC UPDATE'S HOLD on a restored
    /// queue lasts while an agent is queued and never past
    /// [`RESTORED_HOLD`] from the FIRST relaunch this process queued — a later
    /// queue does not extend it, so it cannot pin a build (the ladder's law).
    /// NEGATIVE CONTROLS: nothing queued holds nothing, and a queue that is
    /// done releases it at once.
    #[test]
    fn the_restored_hold_lasts_while_agents_are_queued_and_never_past_its_bound() {
        let world = Arc::new(World::default());
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<&'static str>();
        let gate_rx = Mutex::new(gate_rx);
        let mut h = hooks(&world, parking_body(&world));
        h.acts.relaunch_restored = Arc::new(move |_, _, _| {
            gate_rx
                .lock()
                .unwrap()
                .recv()
                .unwrap_or("adopted")
                .to_string()
        });
        let host = HostHandle::start(on(), false, false, h);
        let before = Instant::now();
        assert!(!host.restored_pending(before), "nothing queued");
        host.relaunch_restored(vec![restored("s-a", "in tab 1")]);
        let first = Instant::now();
        assert!(host.restored_pending(first), "an agent is queued");
        assert!(
            !host.restored_pending(first + RESTORED_HOLD),
            "never past its bound"
        );
        gate_tx.send("adopted").unwrap();
        until("the queue is done", || {
            !host.restored_pending(Instant::now())
        });
        // A LATER queue in the same process is held only inside the FIRST
        // queue's bound.
        std::thread::sleep(Duration::from_millis(20));
        host.relaunch_restored(vec![restored("s-b", "in tab 2")]);
        assert!(host.restored_pending(Instant::now()));
        assert!(
            !host.restored_pending(before + RESTORED_HOLD + Duration::from_millis(10)),
            "once per process: the bound runs from the first relaunch"
        );
        gate_tx.send("adopted").unwrap();
        until("the second queue is done", || {
            !host.restored_pending(Instant::now())
        });
    }

    /// ROUND FOUR, PLAN ITEM 7 (d): THE SUCCESSOR CARRIES ON A RESTART LEFT
    /// IN FLIGHT. The outgoing instance's harness had SIGTERMed a tab's agent
    /// (its record `exiting`, the real `upgrade_drive` record read through the
    /// real [`upgrade_drive::in_flight`]) and the Commit came before its
    /// relaunch line: the tab is back at its shell, no agent in the roster, so
    /// no worker would ever look at it. The successor's Commit steps it until
    /// it is no longer in flight — the tab with no record is never stepped —
    /// and a restart that cannot be carried on is said in the one row.
    ///
    /// RED before the change: the successor's Commit called
    /// [`HostHandle::resume`], which starts workers for agent tabs only — the
    /// NEGATIVE CONTROL below runs exactly that and the record is never
    /// stepped.
    #[test]
    fn resume_after_handoff_carries_an_exiting_record_in_a_shell_tab() {
        let _lane = crate::message_inbox::lane_test_guard();
        let _ = crate::message_inbox::take_queued();
        let root = std::env::temp_dir().join(format!(
            "aterm-harness-carry-{}-{}",
            std::process::id(),
            crate::upgrade_host::now_s()
        ));
        let state = root.join("state");
        std::fs::create_dir_all(state.join("upgrade")).unwrap();
        let session = "7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f";
        let record = state.join("upgrade").join(format!("{session}.json"));
        std::fs::write(
            &record,
            format!(
                "{{\"phase\":\"exiting\",\"at\":{},\"tab\":\"s-shell\",\"pid\":999999,\
                 \"agent\":\"claude\"}}",
                crate::upgrade_host::now_s()
            ),
        )
        .unwrap();
        let opts_for = {
            let (home, state) = (root.join("home"), state.clone());
            move |sid: &str| upgrade_drive::Opts {
                home: home.clone(),
                state: state.clone(),
                sock: None,
                only_sid: Some(sid.to_string()),
                dry_run: false,
                human_grace_s: 0,
                hand_back: true,
                background: false,
                aterm_state: None,
            }
        };
        assert_eq!(
            upgrade_drive::in_flight(&opts_for("s-shell")),
            [session],
            "the record reads in flight"
        );
        assert!(upgrade_drive::in_flight(&opts_for("s-other")).is_empty());
        let stepped: Arc<Mutex<Vec<String>>> = Arc::default();
        let carry: Arc<CarryInFlightFn> = {
            let (stepped, record) = (Arc::clone(&stepped), record.clone());
            Arc::new(move |sid, _| {
                if sid == "s-gone" {
                    stepped.lock().unwrap().push(sid.to_string());
                    return Some("refused:shell-gone".to_string());
                }
                if upgrade_drive::in_flight(&opts_for(sid)).is_empty() {
                    return None;
                }
                let mut seen = stepped.lock().unwrap();
                seen.push(sid.to_string());
                // The first step finds the shell's prompt not back yet; the
                // second relaunches, and the record is done.
                if seen.len() == 1 {
                    return Some("wait:shell-prompt".to_string());
                }
                std::fs::write(&record, "{\"phase\":\"done\",\"tab\":\"s-shell\"}").unwrap();
                Some("adopted".to_string())
            })
        };
        let sessions = || {
            vec![
                ("s-shell".to_string(), "in tab 1".to_string()),
                ("s-other".to_string(), "in tab 2".to_string()),
                ("s-gone".to_string(), "in tab 3".to_string()),
            ]
        };

        // NEGATIVE CONTROL: the Commit as it was — `resume` alone.
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        h.acts.carry_in_flight = Arc::clone(&carry);
        let old = HostHandle::start(on(), false, true, h);
        old.resume();
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            stepped.lock().unwrap().is_empty(),
            "the old Commit never looks at a shell tab's record"
        );

        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        h.acts.carry_in_flight = carry;
        let host = HostHandle::start(on(), false, true, h);
        host.resume_after_handoff(sessions());
        until("the record carried on and the gone tab tried", || {
            let seen = stepped.lock().unwrap();
            seen.iter().filter(|sid| *sid == "s-shell").count() == 2
                && seen.iter().any(|sid| sid == "s-gone")
        });
        std::thread::sleep(Duration::from_millis(100));
        let seen = stepped.lock().unwrap().clone();
        assert_eq!(
            seen.iter().filter(|sid| *sid == "s-shell").count(),
            2,
            "stepped until no longer in flight: {seen:?}"
        );
        assert!(
            !seen.iter().any(|sid| sid == "s-other"),
            "a tab with no record is never stepped"
        );
        assert_eq!(
            seen.iter().filter(|sid| *sid == "s-gone").count(),
            1,
            "a restart that cannot be made is never retried"
        );
        assert!(
            upgrade_drive::in_flight(&upgrade_drive::Opts {
                home: root.join("home"),
                state: state.clone(),
                sock: None,
                only_sid: Some("s-shell".to_string()),
                dry_run: false,
                human_grace_s: 0,
                hand_back: true,
                background: false,
                aterm_state: None,
            })
            .is_empty()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut rows = Vec::new();
        while rows.is_empty() && Instant::now() < deadline {
            rows.extend(crate::message_inbox::take_queued().into_iter().filter(|m| {
                m.message.key.as_deref() == Some(crate::message_reporters::KEY_RESTORED_AGENTS)
            }));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].message.title, "Couldn't resume Claude in tab 3");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The restored agents' one row, waited for (a hang detector, never a
    /// latency budget).
    fn restored_rows(within: Duration) -> Vec<crate::message_inbox::InboxMessage> {
        let deadline = Instant::now() + within;
        let mut rows = Vec::new();
        while rows.is_empty() && Instant::now() < deadline {
            rows.extend(crate::message_inbox::take_queued().into_iter().filter(|m| {
                m.message.key.as_deref() == Some(crate::message_reporters::KEY_RESTORED_AGENTS)
            }));
            std::thread::sleep(Duration::from_millis(10));
        }
        rows
    }

    /// A RESTART IN FLIGHT WHOSE AGENT STILL READS SUPERVISED IS LOOKED AT
    /// AGAIN (round six, F17). At the successor's Commit the SIGTERMed agent
    /// is still shutting down, so its tab still reads supervisable; the
    /// adopted-claim grace holds its worker off, the agent exits, the tab is
    /// at its shell and no worker ever starts. The carry's answer for that
    /// tab ([`carry_in_flight_now`], the production hook's decision) is
    /// "later", and the carry steps it once the agent is gone.
    ///
    /// FAILS WITHOUT THE FIX: the hook answered `None` while the tab read
    /// supervisable, and the carry dropped the tab at that first look — the
    /// relaunch never ran.
    #[test]
    fn a_restart_in_flight_behind_the_claim_grace_is_carried_once_the_agent_is_gone() {
        let supervisable = Arc::new(AtomicBool::new(true));
        let stepped: Arc<Mutex<Vec<String>>> = Arc::default();
        let carry: Arc<CarryInFlightFn> = {
            let (supervisable, stepped) = (Arc::clone(&supervisable), Arc::clone(&stepped));
            Arc::new(move |sid, _| {
                carry_in_flight_now(
                    || stepped.lock().unwrap().is_empty(),
                    || supervisable.load(Ordering::SeqCst),
                    || {
                        stepped.lock().unwrap().push(sid.to_string());
                        "adopted".to_string()
                    },
                )
            })
        };
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        h.acts.carry_in_flight = carry;
        let host = HostHandle::start(on(), false, true, h);
        host.defer_adopted_claims(vec!["s-exiting".into()], Duration::from_millis(200));
        host.resume_after_handoff(vec![("s-exiting".into(), "in tab 1".into())]);
        // The SIGTERMed agent exits inside the grace: the tab is at its shell.
        std::thread::sleep(Duration::from_millis(50));
        supervisable.store(false, Ordering::SeqCst);
        until("the restart carried on once its agent was gone", || {
            !stepped.lock().unwrap().is_empty()
        });
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(*stepped.lock().unwrap(), ["s-exiting"], "stepped once");
        // NEGATIVE CONTROL: nothing in flight is never stepped nor waited on.
        assert_eq!(
            carry_in_flight_now(|| false, || true, || unreachable!()),
            None
        );
        assert_eq!(
            carry_in_flight_now(|| true, || true, || unreachable!()).as_deref(),
            Some(CARRY_SUPERVISED)
        );
        host.shutdown_and_join();
    }

    /// A RESTART THE SUCCESSOR COULD NOT CARRY IN TIME IS SAID (round six,
    /// F51): its every step waited (a person at the returned prompt), the
    /// carry's bound ran out, and the tab is named in the restored agents'
    /// row. A park of the successor's own mid-carry steps nothing and drops
    /// nothing: resumed (a Commit that failed), the carry goes on.
    ///
    /// FAILS WITHOUT THE FIX: the loop ended at its wall-clock bound with the
    /// tab still owed and nothing said; and a suspend ended the loop for
    /// good, so the resumed host never stepped the tab again.
    #[test]
    fn a_restart_the_successor_cannot_carry_in_time_is_said() {
        let _lane = crate::message_inbox::lane_test_guard();
        let _ = crate::message_inbox::take_queued();
        let steps = Arc::new(AtomicU64::new(0));
        let carry: Arc<CarryInFlightFn> = {
            let steps = Arc::clone(&steps);
            Arc::new(move |_, _| {
                steps.fetch_add(1, Ordering::SeqCst);
                Some("wait:shell-prompt".to_string())
            })
        };
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        h.acts.carry_in_flight = carry;
        let host = HostHandle::start(on(), false, true, h);
        host.resume_after_handoff(vec![("s-shell".into(), "in tab 1".into())]);
        until("the first step", || steps.load(Ordering::SeqCst) > 0);
        // A park of the successor's own: nothing is stepped while it holds.
        host.suspend();
        std::thread::sleep(Duration::from_millis(20));
        let parked = steps.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(steps.load(Ordering::SeqCst), parked, "no step while parked");
        // The Commit failed: resumed, the carry goes on.
        host.resume();
        until("stepped again after the resume", || {
            steps.load(Ordering::SeqCst) > parked
        });
        let rows = restored_rows(Duration::from_secs(60));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].message.title, "Couldn't resume Claude in tab 1");
        host.shutdown_and_join();
    }

    /// TIER-1 for `HarnessRestoredCarry` (aterm-spec, round four plan item
    /// 7): the real host's restored queue, a park that lands MID-STEP, a
    /// successor's host relaunching what the park froze, and its Commit's
    /// sweep of a restart left in flight — each observed state projected onto
    /// the model's (`queued` from the live queue, `carried` from what the
    /// handoff leaf would say, `resolved`/`restart` from the successor's
    /// acts) and each observed transition fired in the model. The model's
    /// `StepStart` disabled while parked is checked against the real worker
    /// taking no step for longer than its retry pause. NEGATIVE CONTROL: what
    /// the code before the change carried (the handoff leaf's `agent: None`,
    /// the Commit's bare `resume`) projects onto the dead `ParkDropsQueue` and
    /// `CommitWithoutSweep`, and each breaks its invariant.
    #[test]
    fn the_real_restored_carry_conforms_to_its_model() {
        let model = aterm_spec::derive::harness_restored_carry_model();
        let project = |state: &mut aterm_spec::interp::State, host: &HostHandle| {
            state.insert("queued", i64::from(!host.pending_restored().is_empty()));
        };
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<String>();
        let (word_tx, word_rx) = std::sync::mpsc::channel::<&'static str>();
        let (entered_tx, word_rx) = (Mutex::new(entered_tx), Mutex::new(word_rx));
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        h.acts.relaunch_restored = Arc::new(move |sid, _, _| {
            entered_tx.lock().unwrap().send(sid.to_string()).unwrap();
            word_rx
                .lock()
                .unwrap()
                .recv()
                .unwrap_or("wait:shell-prompt")
                .to_string()
        });
        let parent = HostHandle::start(on(), false, false, h);
        let mut state = model.init_state();

        parent.relaunch_restored(vec![restored("s-a", "in tab 1")]);
        // The first step must start: a hang detector, a minute.
        let entered = entered_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert_eq!(entered, "s-a");
        assert!(model.fire("StepStart", &mut state), "{state:?}");
        let mut seen = state.clone();
        project(&mut seen, &parent);
        assert_eq!(seen, state, "queued while its step runs");

        // THE PARK LANDS MID-STEP: what it freezes includes the step running.
        parent.pause_restored();
        assert!(model.fire("Park", &mut state), "{state:?}");
        let carried = parent.pending_restored_for("s-a");
        assert_eq!(i64::from(carried.is_some()), state["carried"]);

        // The step ends not possible yet: still queued, and no new step
        // starts while parked — past the retry pause it was owed.
        word_tx.send("wait:shell-prompt").unwrap();
        assert!(model.fire("StepRetries", &mut state), "{state:?}");
        assert!(
            entered_rx
                .recv_timeout(RESTORED_FIRST_PAUSE + Duration::from_millis(400))
                .is_err(),
            "no step starts while parked"
        );
        assert!(!model.action_enabled("StepStart", &state));
        let mut seen = state.clone();
        project(&mut seen, &parent);
        assert_eq!(seen, state);
        assert_eq!(
            parent.pending_restored_for("s-a"),
            carried,
            "the frozen carry does not move with the step"
        );

        // THE COMMIT: the successor queues what the leaf carried, and sweeps
        // the restart left in flight in a shell tab.
        assert!(model.fire("Commit", &mut state), "{state:?}");
        let relaunched: Arc<Mutex<Vec<Snapshot>>> = Arc::default();
        let swept = Arc::new(AtomicUsize::new(0));
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        let r = Arc::clone(&relaunched);
        h.acts.relaunch_restored = Arc::new(move |_, snap, _| {
            r.lock().unwrap().push(snap.clone());
            "adopted".to_string()
        });
        let w = Arc::clone(&swept);
        h.acts.carry_in_flight = Arc::new(move |sid, _| {
            (sid == "s-shell" && w.fetch_add(1, Ordering::SeqCst) == 0)
                .then(|| "adopted".to_string())
        });
        let successor = HostHandle::start(on(), false, true, h);
        successor.resume_after_handoff(vec![("s-shell".to_string(), "in tab 2".to_string())]);
        let carried = carried.expect("the park carried the agent");
        successor.relaunch_restored(vec![RestoredAgent {
            sid: "s-a".to_string(),
            snap: carried.clone(),
            place: "in tab 1".to_string(),
        }]);
        until("the successor relaunched it", || {
            !relaunched.lock().unwrap().is_empty()
        });
        assert_eq!(relaunched.lock().unwrap()[0], carried);
        assert!(model.fire("SuccRelaunches", &mut state), "{state:?}");
        until("the sweep carried the restart on", || {
            swept.load(Ordering::SeqCst) >= 1
        });
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            swept.load(Ordering::SeqCst),
            1,
            "a carry-on that relaunched is not stepped again"
        );
        assert!(model.fire("SweepCarriesOn", &mut state), "{state:?}");
        for invariant in ["NoSilentLoss", "NoStepWhileParked", "NoStrandedRestart"] {
            assert!(model.check_invariant(invariant, &state), "{invariant}");
        }
        assert_eq!((state["resolved"], state["restart"]), (1, 0));

        // NEGATIVE CONTROL: the old code carried nothing and swept nothing.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut old = buggy.init_state();
        for action in [
            "StepStart",
            "ParkDropsQueue",
            "StepRetries",
            "CommitWithoutSweep",
        ] {
            assert!(buggy.fire(action, &mut old), "{action}: {old:?}");
        }
        assert_eq!(old["carried"], 0, "the handoff leaf said agent: None");
        assert!(!buggy.check_invariant("NoSilentLoss", &old));
        assert!(!buggy.check_invariant("NoStrandedRestart", &old));
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
                ViewNext {
                    at: *n.lock().unwrap(),
                    ..ViewNext::default()
                }
            }),
            ..hooks(&world, parking_body(&world))
        };
        let taken = || std::mem::take(&mut *looks.lock().unwrap());
        let mut view = ViewLook::default();
        let none: HashMap<String, FollowStamp> = HashMap::new();
        let one: HashMap<String, Program> = [("s-a".to_string(), Program::Claude)].into();
        let two: HashMap<String, Program> = [
            ("s-a".to_string(), Program::Claude),
            ("s-b".to_string(), Program::Claude),
        ]
        .into();
        assert_eq!(
            look_at_upgrades(&mut view, &one, &none, true, (0, 0), &hooks),
            None
        );
        assert_eq!(taken(), [true], "the first look");
        assert_eq!(
            look_at_upgrades(&mut view, &one, &none, true, (0, 0), &hooks),
            None
        );
        assert!(taken().is_empty(), "nothing moved: no look");
        let _ = look_at_upgrades(&mut view, &one, &none, true, (1, 0), &hooks);
        assert_eq!(taken(), [true], "a worker acted");
        let _ = look_at_upgrades(&mut view, &one, &none, true, (1, 1), &hooks);
        assert_eq!(taken(), [true], "an activation, or the owner's word");
        let _ = look_at_upgrades(&mut view, &two, &none, true, (1, 1), &hooks);
        assert_eq!(taken(), [true], "the roster changed");
        // The view names an instant: the host looks again then.
        *next.lock().unwrap() = Some(crate::upgrade_host::now_s());
        let _ = look_at_upgrades(&mut view, &one, &none, true, (1, 1), &hooks);
        assert_eq!(taken(), [true]);
        *next.lock().unwrap() = None;
        assert!(
            look_at_upgrades(&mut view, &one, &none, true, (1, 1), &hooks).is_none(),
            "looked at again at its instant, and named none after"
        );
        assert_eq!(taken(), [true]);
        // Off: stood down once; back on: looked at afresh.
        let _ = look_at_upgrades(&mut view, &one, &none, false, (1, 1), &hooks);
        let _ = look_at_upgrades(&mut view, &one, &none, false, (2, 2), &hooks);
        assert_eq!(taken(), [false], "stood down once");
        let _ = look_at_upgrades(&mut view, &one, &none, true, (2, 2), &hooks);
        assert_eq!(taken(), [true], "back on");
    }

    /// THE OWNER'S VIEW FOLLOWS A LIVE STATUS BY ITS TAB'S SCREEN (the review
    /// of the upgrade's leftovers, 2026-09-28). A row the view read off
    /// Claude's live status ([`ViewNext::follows`]) moves whenever that status
    /// does — a box answered, a turn that runs on — and no worker's step, no
    /// activation, no change of tabs and no instant comes with that: the
    /// host kept the look it took while a box was up for the whole turn after
    /// it. A move of that tab's screen (its publication's `rev`, which a box
    /// answered moves) is now a look, and so, once, is [`STATUS_AFTER_SCREEN`]
    /// after it. NEGATIVE CONTROLS: another tab's screen moving is no look;
    /// the look after the move is taken once; and a tab the view no longer
    /// follows is no look however its screen moves.
    #[test]
    fn the_owners_view_follows_the_screen_of_a_tab_whose_row_reads_a_live_status() {
        let world = Arc::new(World::default());
        let looks: Arc<Mutex<Vec<bool>>> = Arc::default();
        let follows: Arc<Mutex<Vec<String>>> = Arc::default();
        let (l, f) = (Arc::clone(&looks), Arc::clone(&follows));
        let hooks = Hooks {
            upgrade_view: Arc::new(move |look| {
                l.lock().unwrap().push(look);
                ViewNext {
                    at: None,
                    follows: f.lock().unwrap().clone(),
                    ..ViewNext::default()
                }
            }),
            ..hooks(&world, parking_body(&world))
        };
        let taken = || std::mem::take(&mut *looks.lock().unwrap());
        let wanted: HashMap<String, Program> = [
            ("s-a".to_string(), Program::Claude),
            ("s-b".to_string(), Program::Claude),
        ]
        .into();
        let revs = |a: u64, b: u64| -> HashMap<String, FollowStamp> {
            [("s-a", a), ("s-b", b)]
                .into_iter()
                .map(|(sid, rev)| {
                    (
                        sid.to_string(),
                        FollowStamp {
                            rev,
                            ..FollowStamp::default()
                        },
                    )
                })
                .collect()
        };
        let mut view = ViewLook::default();
        let mut look = |a: u64, b: u64| {
            look_at_upgrades(&mut view, &wanted, &revs(a, b), true, (0, 0), &hooks)
        };

        *follows.lock().unwrap() = vec!["s-a".to_string()];
        assert_eq!(look(1, 1), None, "no instant named");
        assert_eq!(
            taken(),
            [true],
            "the first look: s-a's row reads its status"
        );
        let _ = look(1, 1);
        assert!(taken().is_empty(), "nothing moved: no look");
        let _ = look(1, 2);
        assert!(taken().is_empty(), "another tab's screen moved: no look");
        let settle = look(2, 2);
        assert_eq!(taken(), [true], "the followed tab's screen moved: a look");
        assert!(settle.is_some(), "and one more once the status is written");
        std::thread::sleep((hooks.pause)(STATUS_AFTER_SCREEN) + Duration::from_millis(20));
        assert_eq!(
            look(2, 2),
            None,
            "the look after the move names nothing more"
        );
        assert_eq!(taken(), [true], "the look after the move");
        let _ = look(2, 2);
        assert!(taken().is_empty(), "taken once");
        // A step recorded a word: the view no longer follows s-a.
        *follows.lock().unwrap() = Vec::new();
        assert_eq!(look(3, 2), None, "nothing more to follow");
        assert_eq!(taken(), [true], "the move while it was followed");
        let _ = look(4, 2);
        assert!(taken().is_empty(), "no longer followed: no look");
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

    /// The relaunch on exit is written for Claude Code and — the harness's
    /// Codex parity, 2026-09-27 — for Codex: a Codex that leaves its tab
    /// unasked is relaunched as a Claude Code is, what its relaunch needs
    /// read while it runs, and nothing is said on its attention (until then
    /// it was badged "relaunch not built for this agent yet"). The UPGRADE is
    /// built for Codex too, so a Codex with one due is parked for its idle
    /// point — and one with none, its run naming its thread already, is not.
    /// NEGATIVE CONTROL: an agent neither is written for is not parked for
    /// and nothing of it is read.
    #[test]
    fn a_codex_is_relaunched_on_exit_and_upgraded_too() {
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
            group: Arc::default(),
        };
        let codex = job(Program::Codex);
        attach(&codex, &hooks);
        assert!(
            codex.park.load(Ordering::SeqCst),
            "due: parked for its step"
        );
        assert!(with_kept(&codex, |k| k.snapshot.is_some()), "its run read");
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
        assert!(with_kept(&other, |k| k.snapshot.is_none()), "nothing read");
        on_agent_left(&codex, &hooks);
        assert_eq!(*a.relaunched.lock().unwrap(), ["s-k"]);
        assert!(
            a.badges.lock().unwrap().iter().all(Option::is_none),
            "nothing said: {:?}",
            a.badges.lock().unwrap()
        );
        let claude = job(Program::Claude);
        attach(&claude, &hooks);
        assert!(claude.park.load(Ordering::SeqCst), "due: parked for");
        on_agent_left(&claude, &hooks);
        assert_eq!(*a.relaunched.lock().unwrap(), ["s-k", "s-k"]);
    }

    /// S0 AND S3 OF THE IN-FLIGHT REVIEW (2026-09-27): AN AGENT THE HARNESS'S
    /// OWN RESTART ENDED is that restart's to carry, read before whose exit
    /// it was. The owner who pressed Upgrade now had typed within the grace,
    /// so the exit read as theirs; a hold, `[harness] relaunch = false` or a
    /// Codex (`relaunch not built for this agent yet`) left it too — and the
    /// agent the upgrade ended was never brought back. Now it is carried — a
    /// Claude Code by its snapshot's pid, a Codex by its tab — never by the
    /// relaunch on exit's own attempt; tried again at least every
    /// `CARRY_EVERY` while it misses (the back-off's ten minutes outlasted
    /// its record); and said in the restart's own words once it keeps
    /// missing and once it can never land. NEGATIVE CONTROL: the same exits
    /// with no restart of the harness's own are left to the person, and a
    /// Codex's, nobody at the tab, is the relaunch on exit's (its Codex
    /// parity, 2026-09-27): relaunched, never carried.
    #[test]
    fn an_agent_the_harness_own_restart_ended_is_carried_whoever_is_at_the_tab() {
        let a = Arc::new(Acting::default());
        let hooks = a.hooks();
        let job = |agent: Program, relaunch: bool| {
            let mut cfg = on();
            if !relaunch {
                cfg.set("relaunch", "false").unwrap();
            }
            let switches = Arc::new(Switches::default());
            switches.set(&cfg);
            WorkerJob {
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
                switches,
                kept: Arc::default(),
                note: Arc::default(),
                group: Arc::default(),
            }
        };
        let with_snapshot = |job: WorkerJob| {
            with_kept(&job, |k| k.snapshot = Some(snap("s-k")));
            job
        };
        // The owner typed just before the SIGTERM, and a hold stands.
        *a.human_ms.lock().unwrap() = Some(800);
        a.held.store(true, Ordering::SeqCst);
        a.restart_in_flight.store(true, Ordering::SeqCst);
        on_agent_left(&with_snapshot(job(Program::Claude, false)), &hooks);
        assert_eq!(*a.carries.lock().unwrap(), [(false, Some(4242))]);
        assert!(
            a.relaunched.lock().unwrap().is_empty(),
            "never the relaunch on exit's attempt"
        );
        assert!(a.badges.lock().unwrap().is_empty(), "landed: nothing said");
        on_agent_left(&job(Program::Codex, true), &hooks);
        assert_eq!(a.carries.lock().unwrap()[1], (true, None), "by its tab");
        assert_eq!(a.restarted_asked.lock().unwrap()[1], (true, None));
        assert!(
            a.badges.lock().unwrap().is_empty(),
            "never `not built`: {:?}",
            a.badges.lock().unwrap()
        );
        // It keeps missing (a person at the prompt), then can never land.
        a.pauses.lock().unwrap().clear();
        *a.carry_steps.lock().unwrap() =
            VecDeque::from(["wait:held", "wait:held", "wait:held", "refused:stale-exit"]);
        on_agent_left(&with_snapshot(job(Program::Claude, true)), &hooks);
        assert_eq!(a.carries.lock().unwrap().len(), 6, "every attempt carried");
        let pauses = a.pauses.lock().unwrap().clone();
        assert!(
            pauses.len() == 4 && pauses.iter().all(|p| *p <= relaunch::CARRY_EVERY),
            "never a pause its record outlives: {pauses:?}"
        );
        let badges = a.badges.lock().unwrap().clone();
        // In aterm's words, the state and what a person can do: the step
        // word (`wait:held`, `refused:stale-exit`) is the log's.
        assert!(
            matches!(&badges[..], [Some(failing), Some(cannot)]
                if failing.starts_with("aterm ended the agent")
                    && failing.contains("has not brought it back yet (still trying: the session \
                                         is held)")
                    && cannot.contains("cannot bring it back; resume it by hand")
                    && !failing.contains("wait:")
                    && !cannot.contains("refused:")
                    && !cannot.contains("the harness")),
            "{badges:?}"
        );
        // NEGATIVE CONTROLS: no restart of the harness's own.
        a.restart_in_flight.store(false, Ordering::SeqCst);
        a.badges.lock().unwrap().clear();
        on_agent_left(&with_snapshot(job(Program::Claude, true)), &hooks);
        assert_eq!(a.carries.lock().unwrap().len(), 6, "nothing carried");
        assert!(
            a.relaunched.lock().unwrap().is_empty(),
            "left to the person"
        );
        assert!(a.badges.lock().unwrap().is_empty());
        // A Codex's exit with no restart of the harness's own is the relaunch
        // on exit's (the harness's Codex parity): relaunched on its thread,
        // never carried, nothing said.
        *a.human_ms.lock().unwrap() = None;
        a.held.store(false, Ordering::SeqCst);
        on_agent_left(&with_snapshot(job(Program::Codex, true)), &hooks);
        assert_eq!(a.carries.lock().unwrap().len(), 6, "nothing carried");
        assert_eq!(*a.relaunched.lock().unwrap(), ["s-k"], "relaunched on exit");
        assert!(
            a.badges.lock().unwrap().iter().all(Option::is_none),
            "{:?}",
            a.badges.lock().unwrap()
        );
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
            raised.iter().all(
                |t| t.starts_with("supervisor keeps failing; restarting in ")
                    && t.contains("boom in the loop")
                    && t.len() <= 200
            ),
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

    /// THE ADOPTED-CLAIM GRACE (the round-four plan, item 9). A successor's
    /// host names, before its Commit, the adopted sessions whose claim the
    /// outgoing process could not vouch for; from the Commit's resume it
    /// claims every other agent session at once, and those only once their
    /// grace has run out — so an external supervisor that held one in the old
    /// instance renews into the new one first, and this host then parks
    /// behind it as it would have in the old one. The menu bar, meanwhile,
    /// does not count them as the host's (`held_off`).
    ///
    /// FAILS WITHOUT THE FIX: the host claims every agent session at resume —
    /// `s-adopted` is live at once. (Measured: with the `retain` in
    /// `start_workers` removed, the first assertion that it is not yet live
    /// fails.)
    #[test]
    fn the_successor_harness_waits_a_renewal_before_claiming_an_adopted_session() {
        // The production grace outlasts two renewal steps of an external
        // loop, the most one can be from its next renewal at the Commit.
        assert!(
            ADOPTED_CLAIM_GRACE > 2 * aterm_agent::supervise::CLAIM_RENEW,
            "{ADOPTED_CLAIM_GRACE:?}"
        );
        let grace = Duration::from_millis(1500);
        let world = Arc::new(World::default());
        world.set(&[
            ("s-adopted", Program::Claude),
            ("s-vouched", Program::Claude),
        ]);
        let host = HostHandle::start(on(), false, true, hooks(&world, parking_body(&world)));
        // Named before the Commit, while the host is suspended: nothing runs,
        // and the grace has not started.
        host.defer_adopted_claims(vec!["s-adopted".to_string()], grace);
        assert!(!host.is_running());
        let resumed = Instant::now();
        host.resume();
        until("the vouched session is claimed at resume", || {
            host.live().iter().any(|sid| sid == "s-vouched")
        });
        // Still inside the grace (read, THEN the clock, so the reading is
        // known to predate the grace's end): not claimed, and not the host's
        // for the menu bar.
        let live = host.live();
        let held_off = host.held_off();
        if Instant::now() < resumed + grace {
            assert_eq!(live, ["s-vouched"], "held off through its grace");
            assert_eq!(held_off, ["s-adopted"]);
        }
        until("claimed once its grace has run out", || {
            host.live() == ["s-adopted", "s-vouched"]
        });
        assert!(
            resumed.elapsed() >= grace,
            "not before the grace: {:?}",
            resumed.elapsed()
        );
        assert!(host.held_off().is_empty(), "the grace is over");
        host.shutdown_and_join();

        // A host already running (no Commit to wait for) starts the grace at
        // once; a session it does not name is untouched.
        let world = Arc::new(World::default());
        world.set(&[("s-late", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, hooks(&world, parking_body(&world)));
        until("supervised", || host.live() == ["s-late"]);
        let named = Instant::now();
        host.defer_adopted_claims(vec!["s-next".to_string()], grace);
        world.set(&[("s-late", Program::Claude), ("s-next", Program::Claude)]);
        until("claimed after its grace", || {
            host.live() == ["s-late", "s-next"]
        });
        assert!(named.elapsed() >= grace, "{:?}", named.elapsed());
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

    /// A WORKER WINDING DOWN IS STOPPED ONCE ([`visit_workers`]): one handed
    /// over to a changed policy, and one whose tab closed, is asked to stop
    /// at the pass that finds it so — its connection cut once — and not
    /// again at each pass until it is reaped. Every stop rings the bell, so
    /// a stop repeated at every pass woke the host at once, and it spun
    /// through the worker's whole wind-down, re-reading the roster at each
    /// pass: 114,274 to 115,011 passes in the ~500 ms a reload waited on an
    /// upgrade step (the review of 2026-09-28); here, before the fix, about
    /// 91,000 cuts over one hand-over's 300 ms and 195,000 over one closed
    /// tab's. The cuts are this test's alone; the passes count every other
    /// test's bell too (up to 273 in a window under the module's parallel
    /// run), so their bound is loose.
    #[test]
    fn a_worker_winding_down_is_stopped_once_not_at_every_pass() {
        const WIND_DOWN: Duration = Duration::from_millis(300);
        const LOOK: Duration = Duration::from_millis(200);
        let world = Arc::new(World::default());
        let (cuts, winding, passes) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let mut hooks = hooks(&world, parking_body(&world));
        let (w, n) = (Arc::clone(&world), Arc::clone(&passes));
        hooks.roster = Arc::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
            w.roster
                .lock()
                .unwrap()
                .iter()
                .map(|(sid, program)| (sid.clone(), *program, FollowStamp::default()))
                .collect()
        });
        let (w, c, d) = (Arc::clone(&world), Arc::clone(&cuts), Arc::clone(&winding));
        // parking_body, with an interrupter that counts its cuts, and a stop
        // that takes the body WIND_DOWN to end (a step finishing, the claim
        // given back).
        hooks.body = Arc::new(move |job: &WorkerJob| {
            let c = Arc::clone(&c);
            let me = std::thread::current();
            *job.interrupt.lock().unwrap() = Some(Box::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
                me.unpark();
            }));
            w.runs.fetch_add(1, Ordering::SeqCst);
            while !job.stop.load(Ordering::SeqCst) {
                std::thread::park();
            }
            d.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(WIND_DOWN);
            BodyEnd::Stopped
        });
        let host = HostHandle::start(on(), false, false, hooks);
        world.set(&[("s-w", Program::Claude)]);
        until("attached", || world.runs.load(Ordering::SeqCst) == 1);
        // How many passes one wind-down drew, `n` bodies having seen a stop.
        let winding_passes = |n: usize| {
            until("winding down", || winding.load(Ordering::SeqCst) >= n);
            let before = passes.load(Ordering::SeqCst);
            std::thread::sleep(LOOK);
            passes.load(Ordering::SeqCst) - before
        };
        // A changed policy: handed over, and a successor once it is reaped.
        let mut policy = on();
        policy.set("dismiss_surveys", "false").unwrap();
        host.set_config(policy);
        let handing = winding_passes(1);
        until("the successor runs", || {
            world.runs.load(Ordering::SeqCst) >= 2
        });
        assert_eq!(cuts.load(Ordering::SeqCst), 1, "handed over once");
        assert!(handing < 10_000, "{handing} passes over one hand-over");
        // The tab closes: the successor is stopped.
        world.set(&[]);
        let closing = winding_passes(2);
        until("reaped", || host.live().is_empty());
        assert_eq!(cuts.load(Ordering::SeqCst), 2, "stopped once");
        assert!(closing < 10_000, "{closing} passes over one stop");
        assert_eq!(world.runs.load(Ordering::SeqCst), 2, "no third run");
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
        /// The harness's own restart ended the agent that left
        /// ([`Acts::restarted`]), and what each look asked it with.
        restart_in_flight: AtomicBool,
        restarted_asked: Mutex<Vec<(bool, Option<u32>)>>,
        /// What carrying it answers, in turn; `adopted` once spent — and
        /// what each attempt asked it with.
        carry_steps: Mutex<VecDeque<&'static str>>,
        carries: Mutex<Vec<(bool, Option<u32>)>>,
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
        /// The foreground group the roster names the agent in.
        group: AtomicI32,
        /// Whether the agent still holds its tab ([`Acts::holds`]; `None`:
        /// it cannot be read), and the groups the host asked about.
        holds: Mutex<Option<bool>>,
        holds_asked: Mutex<Vec<i32>>,
        /// The Claude Codes (pids) the harness's own restart ended, its
        /// relaunch still in flight ([`Acts::restarted`] of that very
        /// process, as `relaunch::restarted` reads it); a carry that lands
        /// removes it.
        restarted_pids: Mutex<Vec<u32>>,
        /// Whether the upgrade owns the session's turn ends, as the worker's
        /// loop last read it ([`IdleHost::owns_turn_end`]).
        owns_now: Mutex<Option<bool>>,
        /// The loop's next run fails once.
        fail_once: AtomicBool,
        /// The groups a worker asked [`Hooks::still_wanted`] about.
        still_asked: Mutex<Vec<i32>>,
        /// Every watch the host fired ([`Acts::watch`]): the tab, and whether
        /// it was told (a worker's start, an activation) or due.
        watches: Mutex<Vec<(String, bool)>>,
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
            codex: None,
            dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
        }
    }

    /// THE HOST'S WORD OF WHICH SESSIONS IT WOULD RELAUNCH
    /// ([`HostHandle::relaunching`], resume-hint review 2026-09-26): the
    /// sessions whose kept snapshot its relaunch would plan, and none with
    /// `[harness] relaunch` off. NEGATIVE CONTROLS, each a session the
    /// window's remedy used to promise a relaunch for on the one switch for
    /// every session alone: an argv with a flag the rewrite does not know (the
    /// relaunch refuses `argv:unknown-flag`), a `--worktree` launch
    /// (`argv:not-resumable`), a shell with no line (no dialect: the plan
    /// waits for ever), and a session with no snapshot kept at all.
    #[test]
    fn the_host_names_only_the_sessions_its_relaunch_would_plan() {
        let kept: Mutex<HashMap<String, Arc<Mutex<Kept>>>> = Mutex::default();
        let keep = |sid: &str, snapshot: Option<Snapshot>| {
            kept.lock().unwrap().insert(
                sid.to_string(),
                Arc::new(Mutex::new(Kept {
                    snapshot,
                    ..Kept::default()
                })),
            );
        };
        let with_argv = |sid: &str, argv: &[&str]| Snapshot {
            argv: argv.iter().map(|a| (*a).to_string()).collect(),
            ..snap(sid)
        };
        keep("s-plans", Some(snap("s-plans")));
        keep(
            "s-unknown-flag",
            Some(with_argv(
                "s-unknown-flag",
                &["/opt/claude", "--permission-prompt-tool", "mcp__x"],
            )),
        );
        keep(
            "s-worktree",
            Some(with_argv("s-worktree", &["/opt/claude", "--worktree"])),
        );
        keep(
            "s-nushell",
            Some(Snapshot {
                dialect: None,
                ..snap("s-nushell")
            }),
        );
        keep("s-no-snapshot", None);
        assert_eq!(
            relaunching_of(true, &kept),
            std::iter::once("s-plans".to_string()).collect(),
        );
        assert!(relaunching_of(false, &kept).is_empty(), "relaunch = false");
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
            let (a16, a18, a19, a20) = (w(self), w(self), w(self), w(self));
            Hooks {
                roster: Arc::new(move || {
                    let stamp = FollowStamp {
                        group: w1.group.load(Ordering::SeqCst),
                        ..FollowStamp::default()
                    };
                    w1.roster
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(sid, program)| (sid.clone(), *program, stamp))
                        .collect()
                }),
                // The live predicate's: the roster names it, or its agent
                // still holds its tab.
                still_wanted: Arc::new(move |sid, group| {
                    w2.still_asked.lock().unwrap().push(group);
                    w2.roster.lock().unwrap().iter().any(|(s, _)| s == sid)
                        || *w2.holds.lock().unwrap() == Some(true)
                }),
                body: Arc::new(move |job: &WorkerJob| {
                    w3.runs.fetch_add(1, Ordering::SeqCst);
                    loop {
                        if job.stop.load(Ordering::SeqCst) {
                            return BodyEnd::Stopped;
                        }
                        if w3.fail_once.swap(false, Ordering::SeqCst) {
                            return BodyEnd::Failed("a failed run".to_string());
                        }
                        if let Some(host) = job.opts.idle_host.as_ref() {
                            host.stalled(w3.stall_held.load(Ordering::SeqCst));
                            *w3.taskless_seen.lock().unwrap() = Some(host.taskless());
                            *w3.owns_now.lock().unwrap() = Some(host.owns_turn_end());
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
                    holds: Arc::new(move |_, group| {
                        a16.holds_asked.lock().unwrap().push(group);
                        *a16.holds.lock().unwrap()
                    }),
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
                            codex: false,
                            crashed: None,
                        })
                    }),
                    relaunch_restored: Arc::new(|_, _, _| "refused:inert".to_string()),
                    carry_in_flight: Arc::new(|_, _| None),
                    relaunch: Arc::new(move |sid, _, snap, left, upgrade, stalled| {
                        assert_eq!(snap.tab, sid, "the snapshot of the session that left");
                        a5.exit_records.lock().unwrap().push(left.word());
                        // `after_exit`'s decision on what the exit left: a
                        // record read at the exit, else the record now.
                        let survived = match left {
                            ExitRecord::Survived(_) | ExitRecord::Crashed => true,
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
                    restarted: Arc::new(move |_, codex, pid| {
                        a18.restarted_asked.lock().unwrap().push((codex, pid));
                        a18.restart_in_flight.load(Ordering::SeqCst)
                            || pid.is_some_and(|pid| {
                                a18.restarted_pids.lock().unwrap().contains(&pid)
                            })
                    }),
                    carry: Arc::new(move |_, _, codex, pid| {
                        let step = a19
                            .carry_steps
                            .lock()
                            .unwrap()
                            .pop_front()
                            .unwrap_or("adopted");
                        // Landed BEFORE it is counted: whoever sees the carry
                        // sees its record gone.
                        if step == "adopted" {
                            a19.owed.store(true, Ordering::SeqCst);
                            a19.restarted_pids
                                .lock()
                                .unwrap()
                                .retain(|p| Some(*p) != pid);
                        }
                        a19.carries.lock().unwrap().push((codex, pid));
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
                    route: Arc::new(|_, _, _| Route::Custom("inert".to_string())),
                    watch: Arc::new(move |sid, _, how| {
                        a20.watches
                            .lock()
                            .unwrap()
                            .push((sid.to_string(), how.told));
                        upgrade_drive::Watched::default()
                    }),
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
                upgrade_view: Arc::new(|_| ViewNext::default()),
                disk: Arc::new(|_| {}),
            }
        }
    }

    /// T1-h, THE HOST WAKES FOR THE WATCH (design record 2026-09-28, "No
    /// upgrade stuck forever", §3.2 C3; rollout step 7): a session whose loop
    /// offers NO POINT — a turn that never ends, the case that kept tab #1's
    /// record unread for three days — still has its record watched, at the
    /// instant the owner's view names ([`ViewNext::at`], the record's
    /// `watch_at`) and not before: the host wakes for it, finds the tab listed
    /// due ([`ViewNext::watch`]) and fires [`Acts::watch`] for it, off the host
    /// thread. The worker's start is a watch told. And the watch has no hands:
    /// nothing the loop's points take — the step, the notice at a break, the
    /// carry-on — is ever taken from it. The look's PLUMBING, below: a view
    /// that names no instant leaves the host no instant to wake at; the view
    /// that names it does. That is no negative control of the watch's arm —
    /// `look_at_upgrades` only takes the view's instant, whatever named it
    /// (the watch's review, 2026-09-28) — which is aterm-agent's
    /// `the_view_names_a_records_watch_and_lists_it_due_once_past`, over a
    /// real `View` and real records: without `next_change`'s watch arm, an
    /// overdue record names no instant and nothing lists its tab due.
    #[test]
    fn the_host_fires_the_watch_at_its_instant_with_no_point_offered() {
        let a = Arc::new(Acting::default());
        // The loop offers no idle point, and no break either.
        a.busy.store(true, Ordering::SeqCst);
        a.set(&[("s-w", Program::Claude)]);
        let at = crate::upgrade_host::now_s() + 1;
        let view = move |named: bool| {
            move |_look: bool| {
                let now = crate::upgrade_host::now_s();
                ViewNext {
                    at: (named && now < at).then_some(at),
                    watch: if now >= at {
                        vec!["s-w".to_string()]
                    } else {
                        Vec::new()
                    },
                    ..ViewNext::default()
                }
            }
        };
        let hooks = Hooks {
            upgrade_view: Arc::new(view(true)),
            ..a.hooks()
        };
        let host = HostHandle::start(on(), false, false, hooks);
        until("the watch the start was told", || {
            a.watches
                .lock()
                .unwrap()
                .contains(&("s-w".to_string(), true))
        });
        until("the watch the instant brought", || {
            a.watches
                .lock()
                .unwrap()
                .contains(&("s-w".to_string(), false))
        });
        assert!(crate::upgrade_host::now_s() >= at, "not before the instant");
        assert_eq!(a.parks.load(Ordering::SeqCst), 0, "no point was offered");
        assert_eq!(
            a.stepped.load(Ordering::SeqCst),
            0,
            "no step from the watch"
        );
        assert_eq!(a.noticed.load(Ordering::SeqCst), 0, "no notice from it");
        assert_eq!(a.carried.load(Ordering::SeqCst), 0, "no carry-on from it");
        host.shutdown_and_join();

        // The look's plumbing, both ways (the watch arm's own negative
        // control is aterm-agent's, over a real View: see the doc above).
        let one: HashMap<String, Program> = [("s-w".to_string(), Program::Claude)].into();
        let none: HashMap<String, FollowStamp> = HashMap::new();
        let at = crate::upgrade_host::now_s() + 60;
        for (named, wakes) in [(false, false), (true, true)] {
            let hooks = Hooks {
                upgrade_view: Arc::new(move |_| ViewNext {
                    at: named.then_some(at),
                    ..ViewNext::default()
                }),
                ..a.hooks()
            };
            let mut look = ViewLook::default();
            let next = look_at_upgrades(&mut look, &one, &none, true, (0, 0), &hooks);
            assert_eq!(next.is_some(), wakes, "named={named}: {next:?}");
            assert!(look.watch.is_empty(), "nothing due before the instant");
        }
    }

    /// THE RE-PARK (design record 2026-09-28, §3.2 C4): a watch its record's
    /// deadline brought, finding the upgrade due ([`Acts::due`] `Yes`) while
    /// the worker asks for no point and owes itself no look, parks the worker
    /// again — a false `Due::No` at its last look, or a word read as the last,
    /// had left nothing to ask — and hands the watch who looks and what the
    /// loop last offered and withheld. NEGATIVE CONTROLS: a told watch (the
    /// attach and the activation already asked), an upgrade not due, a look
    /// already owed, and a park already set each leave the park as it was.
    #[test]
    fn a_due_watch_parks_a_worker_that_stopped_asking() {
        let seen: Arc<Mutex<Vec<upgrade_drive::Watcher>>> = Arc::default();
        let s = Arc::clone(&seen);
        let acts = |due: Due| Acts {
            due: Arc::new(move |_| due),
            watch: Arc::new({
                let s = Arc::clone(&s);
                move |_, _, how| {
                    s.lock().unwrap().push(how.clone());
                    upgrade_drive::Watched::default()
                }
            }),
            ..Acts::inert()
        };
        let points = Points::default();
        points.withheld(Guard::Wall, 7);
        points.offered(5);
        let run = |told: bool, due: Due, parked: bool, look: Option<Instant>| {
            let park = AtomicBool::new(parked);
            let look_at = Mutex::new(look);
            let _ = watch_tab("s-p", told, 0, &points, &park, &look_at, &acts(due));
            park.load(Ordering::SeqCst)
        };
        assert!(run(false, Due::Yes, false, None), "parked again");
        let how = seen.lock().unwrap().last().cloned().expect("the watch ran");
        assert_eq!(
            (how.guard.as_str(), how.point_at, how.told),
            ("wall", 5, false)
        );
        assert_eq!(how.by, build_word());
        // NEGATIVE CONTROLS.
        assert!(
            !run(true, Due::Yes, false, None),
            "a told watch parks nothing"
        );
        assert!(!run(false, Due::No, false, None), "not due");
        assert!(
            !run(
                false,
                Due::Yes,
                false,
                Some(Instant::now() + Duration::from_secs(60))
            ),
            "a look already owed"
        );
        assert!(run(false, Due::No, true, None), "a park already set stays");
        // A point offered AFTER the guard was said: the guard withheld no
        // later point, and the watch names none (the watch's review,
        // 2026-09-28: `guard=wall` stood beside a later point). Before the
        // fix this read `("wall", 9)`.
        points.offered(9);
        assert!(run(false, Due::Yes, false, None));
        let how = seen.lock().unwrap().last().cloned().expect("the watch ran");
        assert_eq!((how.guard.as_str(), how.point_at), ("", 9));
    }

    /// A TOLD WATCH THAT COULD NOT WRITE IS TOLD AGAIN (the watch's review,
    /// 2026-09-28): an activation notice fires one told watch per tab at
    /// once, each `try_lock`ing the one machine-wide sweep lock, and a told
    /// watch that read `busy` was dropped — only a due tab was fired again,
    /// so the retarget it exists for waited on the record's own deadline.
    /// It is told again [`WATCH_AGAIN`] after, through the real host's timer,
    /// and once it has written it is owed nothing more. NEGATIVE CONTROLS:
    /// [`told_again`] owes nothing to a watch that wrote, found nothing owed
    /// (`no-record`, `not-due`) or found its record moved by a visit, nor to
    /// a DUE watch (the view fires that one again itself), and — `no-home`
    /// — to one that no pause will change; with [`told_again`] owing nothing,
    /// as the code before the fix did, the first assertion fails (measured).
    #[test]
    fn a_told_watch_that_found_the_lock_busy_is_told_again() {
        let skipped = |why: &str| upgrade_drive::Watched {
            skipped: Some(why.to_string()),
            ..upgrade_drive::Watched::default()
        };
        assert!(told_again(true, &skipped("busy")));
        assert!(told_again(true, &skipped("state-unwritable")));
        for why in ["no-record", "not-due", "moved"] {
            assert!(!told_again(true, &skipped(why)), "{why}");
        }
        assert!(!told_again(true, &upgrade_drive::Watched::default()));
        assert!(!told_again(true, &skipped("no-home")));
        assert!(!told_again(false, &skipped("busy")), "a due one");

        let a = Arc::new(Acting::default());
        a.busy.store(true, Ordering::SeqCst);
        a.set(&[("s-w", Program::Claude)]);
        let told = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let t = Arc::clone(&told);
        let mut hooks = a.hooks();
        hooks.acts = Acts {
            watch: Arc::new(move |_, _, how| {
                if !how.told {
                    return upgrade_drive::Watched::default();
                }
                if t.fetch_add(1, Ordering::SeqCst) == 0 {
                    upgrade_drive::Watched {
                        skipped: Some("busy".to_string()),
                        ..upgrade_drive::Watched::default()
                    }
                } else {
                    upgrade_drive::Watched {
                        wrote: vec!["retargeted:2.1.283->2.1.284".to_string()],
                        ..upgrade_drive::Watched::default()
                    }
                }
            }),
            ..hooks.acts.clone()
        };
        let host = HostHandle::start(on(), false, false, hooks);
        until("the told watch, told again", || {
            told.load(Ordering::SeqCst) >= 2
        });
        // Written, it is owed nothing more: many WATCH_AGAINs pass (scaled
        // by the test's pause) with no third.
        std::thread::sleep(quick_pause(WATCH_AGAIN) * 8);
        assert_eq!(told.load(Ordering::SeqCst), 2, "never in a loop");
        host.shutdown_and_join();
    }

    /// WHAT THE LOOP OFFERED AND WITHHELD, KEPT BY THE WORKER (design record
    /// 2026-09-28, "No upgrade stuck forever", §3.2 C5): the guard the loop
    /// says withheld a point ([`IdleHost::withheld`]) is kept with its
    /// second, and every point the loop offers — an idle point, a settled
    /// break — is stamped as it comes, whatever the step then finds
    /// ([`Points`]), for the watch to copy onto the upgrade's record.
    /// Reporting only: the guard types nothing and steps nothing. NEGATIVE
    /// CONTROLS: a withheld point stamps no point, and before anything is
    /// said the worker holds nothing; every guard's atomic code reads back
    /// as itself, and no other number reads as a guard.
    #[test]
    fn the_worker_keeps_the_guard_its_loop_withheld_and_the_points_it_offered() {
        for g in Guard::ALL {
            assert_eq!(Guard::from_code(g.code()), Some(g), "{g:?}");
        }
        assert_eq!(Guard::from_code(0), None);
        let past = u8::try_from(Guard::ALL.len() + 1).unwrap();
        assert_eq!(Guard::from_code(past), None);
        let world = Arc::new(World::default());
        let stepped: Arc<Mutex<Vec<&str>>> = Arc::default();
        let (s1, s2) = (Arc::clone(&stepped), Arc::clone(&stepped));
        let mut hooks = hooks(&world, parking_body(&world));
        hooks.acts = Acts {
            due: Arc::new(|_| Due::No),
            step: Arc::new(move |_, _| {
                s1.lock().unwrap().push("step");
                "announced:1".to_string()
            }),
            notice: Arc::new(move |_, _| {
                s2.lock().unwrap().push("notice");
                "announced:1".to_string()
            }),
            ..Acts::inert()
        };
        let switches = Arc::new(Switches::default());
        switches.set(&on());
        let idle = WorkerIdle {
            sid: "s-pts".to_string(),
            agent: Program::Claude,
            grace: 0,
            park: Arc::default(),
            look_at: Arc::default(),
            acting: Arc::default(),
            stepped: Arc::default(),
            stalled: Arc::default(),
            switches: Arc::clone(&switches),
            kept: Arc::default(),
            hooks,
            run: Mutex::default(),
            owns: AtomicBool::new(false),
            background_at: Mutex::default(),
            tasked: Mutex::default(),
            note: Arc::default(),
            clock_hold: Mutex::default(),
            net: NetProbe::new(),
            route: Mutex::default(),
            points: Arc::default(),
            activations: Arc::default(),
        };
        assert_eq!(idle.points.last(), (None, 0), "nothing said yet");
        let before = crate::upgrade_host::now_s();
        idle.withheld(Guard::Wall);
        let (guard, point) = idle.points.last();
        assert_eq!(guard.map(|(g, _)| g), Some(Guard::Wall));
        assert!(guard.is_some_and(|(_, at)| at >= before));
        assert_eq!(point, 0, "a withheld point is no point");
        // A settled break: stamped, though nothing is due and nothing typed.
        assert_eq!(idle.at_background(), None);
        let (guard, point) = idle.points.last();
        assert!(point >= before, "the break is stamped");
        assert_eq!(
            guard.map(|(g, _)| g),
            Some(Guard::Wall),
            "the last guard stands; the two seconds say which came last"
        );
        idle.withheld(Guard::Settle);
        assert_eq!(idle.points.last().0.map(|(g, _)| g), Some(Guard::Settle));
        // An idle point: stamped the same way.
        let after_break = idle.points.last().1;
        assert!(idle.at_idle().is_none(), "nothing due");
        assert!(idle.points.last().1 >= after_break);
        assert!(stepped.lock().unwrap().is_empty(), "no step, no notice");
        assert!(
            format!("{idle:?}").contains("guard: Some((\"settle\""),
            "{idle:?}"
        );
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
            stepped: Arc::default(),
            stalled: Arc::default(),
            switches: Arc::clone(&switches),
            kept: Arc::default(),
            hooks: hooks.clone(),
            run: Mutex::default(),
            activations: Arc::default(),
            owns: AtomicBool::new(false),
            background_at: Mutex::default(),
            tasked: Mutex::default(),
            note: Arc::default(),
            clock_hold: Mutex::default(),
            net: NetProbe::new(),
            route: Mutex::default(),
            points: Arc::default(),
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

    /// THE API'S REACH, AS THE WORKER ANSWERS ITS LOOP: a Claude Code on the
    /// default route asks the instance's probe (here one whose resolver
    /// fails at once: Down, the run's start kept across asks), its route read
    /// ONCE per agent process; a custom route, another agent, an agent not
    /// snapshotted yet and `[harness] probe_api = false` ask nothing and read
    /// Unknown. NEGATIVE CONTROL: a new agent process (another pid), and the
    /// same one with its directory filled in, read their route again.
    #[test]
    fn a_worker_measures_the_default_route_only_and_reads_it_once_per_agent() {
        use crate::harness_netprobe::Probe;
        struct Nx(AtomicU64);
        impl Probe for Nx {
            fn resolve(&self, _: &str, _: u16) -> std::io::Result<Vec<std::net::SocketAddr>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            }
            fn connect(
                &self,
                _: std::net::SocketAddr,
                _: Duration,
            ) -> std::io::Result<std::net::TcpStream> {
                unreachable!()
            }
            fn handshake(
                &self,
                _: std::net::TcpStream,
                _: &str,
                _: aterm_http::Deadline,
            ) -> std::io::Result<()> {
                unreachable!()
            }
        }
        let routes = Arc::new(Mutex::new(Vec::<u32>::new()));
        let custom = Arc::new(AtomicBool::new(false));
        let (r1, c1) = (Arc::clone(&routes), Arc::clone(&custom));
        let mut hooks = hooks(
            &Arc::new(World::default()),
            Arc::new(|_: &WorkerJob| BodyEnd::Stopped),
        );
        hooks.acts = Acts {
            route: Arc::new(move |pid, _, _| {
                r1.lock().unwrap().push(pid);
                if c1.load(Ordering::SeqCst) {
                    Route::Custom("ANTHROPIC_BASE_URL in the agent's environment".to_string())
                } else {
                    Route::Default
                }
            }),
            ..Acts::inert()
        };
        let probe = Arc::new(Nx(AtomicU64::new(0)));
        let net = NetProbe::with(Arc::clone(&probe) as Arc<dyn Probe>, Duration::from_secs(2));
        let switches = Arc::new(Switches::default());
        switches.set(&on());
        let snapshot = |pid: u32| Snapshot {
            tab: "s-net".to_string(),
            pid,
            start: "Sun Sep 27 18:00:00 2026".to_string(),
            shell: 1,
            program: std::path::PathBuf::from("/usr/local/bin/claude"),
            argv: vec!["claude".to_string()],
            session: None,
            cwd: "/tmp".to_string(),
            version: None,
            codex: None,
            dialect: None,
        };
        let host = |agent: Program| WorkerIdle {
            sid: "s-net".to_string(),
            agent,
            grace: 0,
            park: Arc::default(),
            look_at: Arc::default(),
            acting: Arc::default(),
            stepped: Arc::default(),
            stalled: Arc::default(),
            switches: Arc::clone(&switches),
            kept: Arc::default(),
            hooks: hooks.clone(),
            run: Mutex::default(),
            activations: Arc::default(),
            owns: AtomicBool::new(false),
            background_at: Mutex::default(),
            clock_hold: Mutex::default(),
            tasked: Mutex::default(),
            note: Arc::default(),
            net: Arc::clone(&net),
            route: Mutex::default(),
            points: Arc::default(),
        };
        let idle = host(Program::Claude);
        assert_eq!(idle.reach(), Reach::Unknown, "no agent snapshotted yet");
        assert!(routes.lock().unwrap().is_empty());
        idle.kept.lock().unwrap().snapshot = Some(snapshot(4242));
        let asked = Instant::now();
        let mut seen = idle.reach();
        while !matches!(seen, Reach::Down { .. }) && asked.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
            seen = idle.reach();
        }
        assert!(matches!(seen, Reach::Down { .. }), "{seen:?}");
        assert_eq!(idle.reach(), seen, "the run's start stands");
        assert_eq!(*routes.lock().unwrap(), [4242], "read once per agent");
        // A new agent process: its route read again — custom, never measured.
        custom.store(true, Ordering::SeqCst);
        idle.kept.lock().unwrap().snapshot = Some(snapshot(4343));
        let probes = probe.0.load(Ordering::SeqCst);
        assert_eq!(idle.reach(), Reach::Unknown);
        assert_eq!(*routes.lock().unwrap(), [4242, 4343]);
        // The same process with its directory filled in later (a snapshot
        // taken before its cwd was read, `follow` completing it): read again,
        // once — a route read blind must not stand for the process's life.
        let mut placed = snapshot(4343);
        placed.cwd = "/private/tmp".to_string();
        idle.kept.lock().unwrap().snapshot = Some(placed);
        assert_eq!(idle.reach(), Reach::Unknown);
        assert_eq!(idle.reach(), Reach::Unknown);
        assert_eq!(*routes.lock().unwrap(), [4242, 4343, 4343]);
        // `probe_api = false`, another agent: nothing read, nothing asked.
        let mut off = on();
        off.probe_api = false;
        switches.set(&off);
        custom.store(false, Ordering::SeqCst);
        idle.kept.lock().unwrap().snapshot = Some(snapshot(4444));
        assert_eq!(idle.reach(), Reach::Unknown);
        switches.set(&on());
        let codex = host(Program::Codex);
        codex.kept.lock().unwrap().snapshot = Some(snapshot(4545));
        assert_eq!(codex.reach(), Reach::Unknown);
        assert_eq!(*routes.lock().unwrap(), [4242, 4343, 4343]);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(probe.0.load(Ordering::SeqCst), probes, "no probe for them");
        net.stop();
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

    /// A gated [`Acts::behind`] for [`an_owed_note_is_asked_by_one_asker_at_a_time`]:
    /// each act says it is inside, then waits for its word (`None`: it
    /// panics). The count of acts, the word's sender, the inside receiver.
    fn gated_behind(
        hooks: &mut Hooks,
    ) -> (
        Arc<AtomicUsize>,
        std::sync::mpsc::Sender<Option<Behind>>,
        std::sync::mpsc::Receiver<u64>,
    ) {
        let (inside_tx, inside_rx) = std::sync::mpsc::channel();
        let (word_tx, word_rx) = std::sync::mpsc::channel::<Option<Behind>>();
        let (acts, word_rx) = (Arc::new(AtomicUsize::new(0)), Mutex::new(word_rx));
        let counted = Arc::clone(&acts);
        let inside_tx = Mutex::new(inside_tx);
        hooks.acts.behind = Arc::new(move |_, since| {
            counted.fetch_add(1, Ordering::SeqCst);
            inside_tx.lock().unwrap().send(since).unwrap();
            let word = word_rx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv_timeout(Duration::from_secs(60))
                .expect("a word for the act within 60 s");
            word.unwrap_or_else(|| panic!("the act fails (a test's)"))
        });
        (acts, word_tx, inside_rx)
    }

    /// AN OWED NOTE IS ASKED BY ONE ASKER AT A TIME (round 33, rulings 360
    /// and 362): an ask holds it in flight over its act, and a second asker —
    /// the host's wake racing the worker's attach or idle point — skips it
    /// rather than noting the session twice; a busy or unread put-back after
    /// a skip rings the host (its wake named none meanwhile), counted on the
    /// asker's own thread. The skip is taken at the host's first read, at
    /// its last (the host's wake after its own ask, an idle point's flight
    /// under way), and by a second ask. A notice that owes the note again
    /// in flight keeps it owed whatever the flight says, and an act that
    /// panics leaves it owed, not in flight for good. Each act is gated on a
    /// channel, so every interleaving is forced, none slept for. NEGATIVE
    /// CONTROL: a flight nobody skipped puts back without a ring (an idle
    /// point's own ask wakes no host).
    #[test]
    fn an_owed_note_is_asked_by_one_asker_at_a_time() {
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        let (acts, word, inside) = gated_behind(&mut h);
        let hooks = Arc::new(h);
        let note: Note = Arc::default();
        let wait_inside = || {
            inside
                .recv_timeout(Duration::from_secs(60))
                .expect("the act within 60 s")
        };
        // One ask on its own thread: what it said, and the rings it made.
        let fly = || {
            let (hooks, note) = (Arc::clone(&hooks), Arc::clone(&note));
            std::thread::spawn(move || {
                let rung = RINGS_HERE.with(std::cell::Cell::get);
                let said = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ask_note("s-1", &note, &hooks)
                }));
                (said.ok(), RINGS_HERE.with(std::cell::Cell::get) - rung)
            })
        };
        let owed = || *note.lock().unwrap();

        // The host's wake and a second ask skip the flight: one act.
        owe_note(&note, 100);
        let asker = fly();
        assert_eq!(wait_inside(), 100);
        assert_eq!(ask_note("s-1", &note, &hooks), None, "a second ask skips");
        let now = Instant::now();
        assert_eq!(host_note("s-1", &note, now, &hooks), None, "the wake skips");
        word.send(Some(Behind::Busy)).unwrap();
        let (said, rings) = asker.join().unwrap();
        assert_eq!(said, Some(Some(Behind::Busy)));
        assert_eq!(acts.load(Ordering::SeqCst), 1, "asked once");
        assert!(rings >= 1, "the put-back after a skip rings the host");
        let o = owed().expect("busy: still owed");
        assert!(!o.flying && !o.skipped && o.by.is_some_and(|by| by > now));

        // NEGATIVE CONTROL: nobody skipped it — no ring.
        let asker = fly();
        wait_inside();
        word.send(Some(Behind::Busy)).unwrap();
        assert_eq!(asker.join().unwrap(), (Some(Some(Behind::Busy)), 0));

        // The host's LAST read (its wake after its own ask) meets a flight.
        let asker = fly();
        wait_inside();
        assert_eq!(host_wake(&note), None, "no wake named in flight");
        word.send(Some(Behind::Unread(upgrade_drive::UNREAD_RECORD)))
            .unwrap();
        let (said, rings) = asker.join().unwrap();
        assert!(matches!(said, Some(Some(Behind::Unread(_)))));
        assert!(
            rings >= 1,
            "the put-back after the last read rings the host"
        );
        assert!(host_wake(&note).is_some(), "the host wakes for it again");

        // A notice owes it again in flight: the flight's word is not kept.
        let asker = fly();
        wait_inside();
        owe_note(&note, 50);
        let at = Instant::now();
        assert_eq!(host_note("s-1", &note, at, &hooks), None);
        word.send(Some(Behind::Nothing)).unwrap();
        let (said, rings) = asker.join().unwrap();
        assert_eq!(said, Some(Some(Behind::Nothing)));
        assert!(rings >= 1, "the host is rung for the notice's note");
        let o = owed().expect("re-owed: still owed");
        assert!(o.since == 50 && !o.flying && o.by.is_some_and(|by| by <= at));
        word.send(Some(Behind::Noted)).unwrap();
        assert_eq!(host_note("s-1", &note, Instant::now(), &hooks), None);
        assert_eq!(wait_inside(), 50, "asked with the notice's second");
        assert!(owed().is_none(), "noted: nothing more owed");

        // An act that panics lands its flight, and rings for the skip.
        owe_note(&note, 7);
        let before = acts.load(Ordering::SeqCst);
        let asker = fly();
        wait_inside();
        assert_eq!(host_note("s-1", &note, Instant::now(), &hooks), None);
        word.send(None).unwrap();
        let (said, rings) = asker.join().unwrap();
        assert_eq!(said, None, "the act panicked");
        assert!(rings >= 1, "the unwound flight rings for the skip");
        assert!(owed().is_some_and(|o| !o.flying && o.since == 7));
        word.send(Some(Behind::Noted)).unwrap();
        assert_eq!(ask_note("s-1", &note, &hooks), Some(Behind::Noted));
        assert_eq!(acts.load(Ordering::SeqCst), before + 2, "asked again");
        assert_eq!(wait_inside(), 7, "with the owed second");
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
    /// minted or found nothing to mint is not asked again. The session's
    /// turn runs throughout (no idle point), so every ask counted is the
    /// host's: an idle point the notice parks asks the same owed note, and
    /// its ask raced the host's — both asked, and a count waited for at 3
    /// read 4 (3 of 240 runs, 2026-09-28). NEGATIVE CONTROLS: nothing is
    /// noted before the notice (nothing due at attach), and under
    /// `[harness] upgrade = false` the notice notes nothing.
    #[test]
    fn a_build_installed_mid_session_is_noted_behind_at_its_notice() {
        for agent in [Program::Claude, Program::Codex] {
            let a = Arc::new(Acting::default());
            a.busy.store(true, Ordering::SeqCst);
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
            host.shutdown_and_join();
            // The switch turned off under a running host, the loop at its idle
            // points (a fresh world: a park the busy turn kept would be taken
            // first, and the notice's own seen only by a race): the notice
            // parks the loop and notes nothing.
            let a = Arc::new(Acting::default());
            a.set(&[("s-m", agent)]);
            let host = HostHandle::start(on(), false, false, a.hooks());
            until("attached", || a.runs.load(Ordering::SeqCst) == 1);
            let mut no_upgrade = on();
            no_upgrade.set("upgrade", "false").unwrap();
            host.set_config(no_upgrade);
            host.note_activation();
            until("parked", || a.parks.load(Ordering::SeqCst) >= 1);
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(a.behinds.load(Ordering::SeqCst), 0, "{agent:?}: off");
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
            // The work done: the idle point takes the step the park kept. Its
            // word is a wait, so the next look comes a (quick) pause later
            // and steps again: the count passes 1 within ~20 ms, and a wait
            // for exactly 1 that looked late never saw it (a hang, 2026-09-28).
            a.at_break.store(false, Ordering::SeqCst);
            until("the idle point's step", || {
                a.stepped.load(Ordering::SeqCst) >= 1
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

    /// THE RELEASE TYPED AT A BREAK IS THE HARNESS'S OWN TURN (2026-09-27: a
    /// break that never ends is where a give-up's release is typed, for want
    /// of an idle point): the worker says it took it, so the loop journals it
    /// (`HOST seq=<n> background upgrade step=released:<why>`) and awaits its
    /// answer as the harness's, never as the worker's own turn — and the
    /// upgrade gives the session's turn ends back at once, the agent told to
    /// go on. NEGATIVE CONTROL: a release still owed there (a person's draft
    /// holds it) typed nothing and says nothing.
    #[test]
    fn a_release_typed_at_a_break_is_said_and_gives_the_turn_ends_back() {
        let said = |notice: &'static str| {
            let a = Arc::new(Acting::default());
            a.due.store(true, Ordering::SeqCst);
            *a.steps.lock().unwrap() = ["announced:4"].into();
            *a.notices.lock().unwrap() = [notice].into();
            *a.owns_seen.lock().unwrap() = Some(Vec::new());
            // The shells never end: breaks, and no idle point, from the
            // notice's step on.
            let flip = Arc::clone(&a);
            *a.during_step.lock().unwrap() = Some(Box::new(move || {
                flip.at_break.store(true, Ordering::SeqCst);
            }));
            a.set(&[("s-rb", Program::Claude)]);
            let host = HostHandle::start(on(), false, false, a.hooks());
            until("the break's step", || a.noticed.load(Ordering::SeqCst) == 1);
            std::thread::sleep(Duration::from_millis(50));
            host.shutdown_and_join();
            let owns = a.owns_seen.lock().unwrap().clone().unwrap_or_default();
            let broke = a.break_said.lock().unwrap().clone();
            (broke, owns)
        };
        let (broke, owns) = said("released:gave-up");
        assert_eq!(broke, ["upgrade step=released:gave-up"]);
        assert_eq!(owns, [true, false], "the notice's claim, then given back");
        let (broke, owns) = said("wait:release:draft");
        assert!(broke.is_empty(), "{broke:?}");
        assert_eq!(owns, [true]);
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
    /// the ownership bound counts the settles and drains it bounds
    /// ([`upgrade_drive::bounded_wait`]), across words. NEGATIVE
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
        .map(|s| run.after(s, 120))
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
        assert_eq!(
            run.waits, 3,
            "the ownership bound counts the settles it bounds, not the daemon's waits"
        );
        assert_eq!(run.after("announced:1", 120), After::NextIdle);
        assert_eq!((run.waits, run.same), (0, 0));
        assert_eq!(run.after("wait:settling", 120), After::Later(LATER[0]));
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
            run.after(step, 120);
        }
        for _ in 0..20 {
            run.after("wait:failed", 120);
        }
        // The rest's looks own nothing and bound nothing
        // (`upgrade_drive::bounded_wait`): they climb the ladder only.
        assert_eq!(
            (run.waits, run.same),
            (0, 20),
            "the rest looked many times, and spent none of the settle's looks"
        );
        run.after("rearmed:unanswered", 120);
        assert_eq!(
            (run.waits, run.same),
            (0, 0),
            "the re-arm starts the counts over"
        );
        run.after("wait:settling", 120);
        assert!(upgrade_drive::owns_turn_ends(
            "wait:settling",
            run.waits,
            120
        ));
        for _ in 0..upgrade_drive::OWNED_SETTLE_LOOKS {
            run.after("wait:settling", 120);
        }
        assert!(!upgrade_drive::owns_turn_ends(
            "wait:settling",
            run.waits,
            120
        ));
    }

    /// The REAL worker for one session under the default grace, its steps'
    /// words scripted in order, and the pauses it asked for, as the attended
    /// tests drive it.
    fn attended_worker(
        world: &Arc<World>,
        steps: &[&str],
    ) -> (WorkerIdle, Arc<Mutex<Vec<Duration>>>) {
        let grace = SupervisorConfig::default().human_grace_s;
        let words: Arc<Mutex<VecDeque<String>>> =
            Arc::new(Mutex::new(steps.iter().map(|s| (*s).to_string()).collect()));
        let paused: Arc<Mutex<Vec<Duration>>> = Arc::default();
        let (s1, p1) = (Arc::clone(&words), Arc::clone(&paused));
        let mut hooks = hooks(world, parking_body(world));
        hooks.acts = Acts {
            due: Arc::new(|_| Due::Yes),
            step: Arc::new(move |_, g| {
                assert_eq!(g, grace, "the worker's grace reaches its step");
                s1.lock().unwrap().pop_front().expect("a scripted step")
            }),
            ..Acts::inert()
        };
        hooks.pause = Arc::new(move |pause| {
            p1.lock().unwrap().push(pause);
            pause / 1000
        });
        let switches = Arc::new(Switches::default());
        switches.set(&on());
        let idle = WorkerIdle {
            sid: "s-att".to_string(),
            agent: Program::Claude,
            grace,
            park: Arc::default(),
            look_at: Arc::default(),
            acting: Arc::default(),
            stepped: Arc::default(),
            stalled: Arc::default(),
            switches,
            kept: Arc::default(),
            hooks,
            run: Mutex::default(),
            activations: Arc::default(),
            owns: AtomicBool::new(false),
            background_at: Mutex::default(),
            tasked: Mutex::default(),
            note: Arc::default(),
            clock_hold: Mutex::default(),
            net: NetProbe::new(),
            route: Mutex::default(),
            points: Arc::default(),
        };
        (idle, paused)
    }

    /// A PERSON AT THE TAB: THE WORKER OWNS THE POINT FOR THEIR GRACE (the
    /// 2026-09-27 20:27 point, s-d3346's journal: `HOST … upgrade
    /// step=held-back:attended` at 20:27:27, `wait:attended` at 20:27:49 and
    /// 20:28:10, the next look due at 20:29:10 — and `CONTINUED … keep going`
    /// at 20:28:42, the grace's lapse, over the agent's READY). The REAL
    /// worker, its steps scripted as that upgrade answered them, under the
    /// default 120 s grace: every attended step leaves it owning the session's
    /// turn ends — what its loop reads as `upgrading`, and so types nothing at
    /// the lapse — and asks to be looked at again at the first rung every
    /// time, so the look that finds the grace lapsed comes within 20 s of it
    /// and takes the restart (`terminated`, which moves the point). The same
    /// hold said again at the point is marked a repeat, journaled once
    /// ([`HostStep::repeat`]).
    ///
    /// **TIER-1 FOR `UpgradeAttendedTurnEnd`** (aterm-spec
    /// `upgrade_attended_turn_end_model`): the worker's steps before the READY
    /// are the model's `HostWaitsBefore` (`wait:awaiting-ready`) or
    /// `HostHoldsBefore` (a re-ask a person holds), the READY's turn
    /// (`turn_ran`) its `ReadyArrives`, each attended step at the READY's point
    /// its `HostLooks` — `Bound` the real backstop's looks — and the worker's
    /// ownership after each look must be the model's `owns`. NEGATIVE
    /// CONTROLS, each the real worker against a model dial it must disagree
    /// with at the READY's first look: the host before the fix (`Buggy`: an
    /// attended wait owning nothing); the backstop fed every wait in a row
    /// (`Every`, the review of 2026-09-28: after the notice's 97 answers
    /// without READY the READY owned nothing, and the count the host fed then
    /// is shown to read so); and a count the READY's turn leaves standing
    /// (`Stretch`: 97 looks at a re-ask a person held). Past the backstop the
    /// real worker owns nothing, as the model says, and goes back on the
    /// ladder; an answer without READY owns nothing.
    #[test]
    fn a_person_at_the_tab_leaves_the_point_to_the_upgrade_for_their_grace() {
        use upgrade_drive::LATER;
        let world = Arc::new(World::default());
        let grace = SupervisorConfig::default().human_grace_s;

        // The live sequence, then the look past the lapse takes the restart.
        let mut steps = vec!["held-back:attended"];
        steps.extend(["wait:attended"; 7]);
        steps.push("terminated");
        let (idle, paused) = attended_worker(&world, &steps);
        for (n, word) in steps.iter().enumerate() {
            let step = idle.at_idle().expect("a step");
            assert_eq!(step.line, format!("upgrade step={word}"));
            if *word == "terminated" {
                assert!(step.moved, "the restart moves the point");
                assert!(!step.repeat, "an act is journaled");
                assert!(!idle.owns_turn_end(), "a moved point owns nothing after it");
                continue;
            }
            assert!(!step.moved, "{n}: {word} leaves the point standing");
            assert_eq!(
                step.repeat,
                n >= 2,
                "{n}: {word}: the hold's first look and its first wait are journaled"
            );
            assert!(
                idle.owns_turn_end(),
                "{n}: {word}: the point is the upgrade's while the person's grace runs"
            );
        }
        assert_eq!(
            *paused.lock().unwrap(),
            vec![LATER[0]; 8],
            "every attended look asks for the first rung again"
        );

        // The real backstop, in the model's looks: every attended step at the
        // point is a look, `held-back:attended` among them.
        let look = LATER[0].as_secs();
        let last_owned = (u64::from(grace) + upgrade_drive::ATTENDED_OWNED_S) / look;
        let bound = i64::try_from(last_owned).unwrap();
        let before = usize::try_from(bound + 1).unwrap();
        let model = aterm_spec::interp::with_consts(
            &aterm_spec::derive::upgrade_attended_turn_end_model(),
            &[("Bound", bound), ("Looks", 4 * bound)],
        );
        let at_ready = usize::try_from(bound + 2).unwrap();

        // Walk the real worker and the model beside it: `history` before the
        // READY (each step its word, its model action — none for an act the
        // model starts after — and whether a turn ran before it, as the loop
        // continues the agent between turn ends), the READY's turn, then
        // `at_ready` attended looks, the first said `first`. `against`: a dial
        // the worker must disagree with at the READY's first look. Returns the
        // waits in a row the worker counted by the READY.
        let walk = |history: &[(&'static str, &'static str, bool)],
                    first: &'static str,
                    against: (&str, i64)| {
            let mut script: Vec<&str> = history.iter().map(|(word, _, _)| *word).collect();
            script.push(first);
            script.extend(std::iter::repeat_n("wait:attended", at_ready - 1));
            let (idle, paused) = attended_worker(&world, &script);
            let wrong = aterm_spec::interp::with_consts(&model, &[against]);
            let (mut st, mut w) = (model.init_state(), wrong.init_state());
            for (n, (word, action, turn)) in history.iter().enumerate() {
                if *turn {
                    idle.turn_ran();
                }
                let step = idle.at_idle().expect("a step");
                assert_eq!(step.line, format!("upgrade step={word}"));
                if !action.is_empty() {
                    assert!(model.fire(action, &mut st), "{n}: {action} at {st:?}");
                    assert!(wrong.fire(action, &mut w), "{n}: {action}");
                }
            }
            // The READY: a turn of the agent's.
            idle.turn_ran();
            assert!(model.fire("ReadyArrives", &mut st), "{st:?}");
            assert!(wrong.fire("ReadyArrives", &mut w));
            let waits = idle.run.lock().unwrap().waits;
            let pauses_before = paused.lock().unwrap().len();
            for n in 0..at_ready {
                let step = idle.at_idle().expect("a step");
                let word = if n == 0 { first } else { "wait:attended" };
                assert_eq!(step.line, format!("upgrade step={word}"));
                // The first look at the READY's point is journaled, and so is
                // the first `wait:attended` after a `held-back:attended`.
                let journaled = n == 0 || (n == 1 && first != "wait:attended");
                assert_eq!(step.repeat, !journaled, "{n}");
                assert!(model.fire("HostLooks", &mut st), "{n}: {st:?}");
                assert!(wrong.fire("HostLooks", &mut w), "{n}");
                assert_eq!(
                    i64::from(idle.owns_turn_end()),
                    st["owns"],
                    "look {n} at the READY: the real worker's ownership against the model at \
                     {st:?}"
                );
                if n == 0 {
                    assert_eq!(st["owns"], 1, "the READY's first look is the upgrade's");
                    assert_ne!(
                        i64::from(idle.owns_turn_end()),
                        w["owns"],
                        "NEGATIVE CONTROL {against:?}: that host owned nothing here"
                    );
                }
            }
            assert!(!idle.owns_turn_end(), "past the backstop: the loop's again");
            // Back on the ladder once the next look owns nothing.
            let pauses = paused.lock().unwrap()[pauses_before..].to_vec();
            let owned = usize::try_from(last_owned).unwrap();
            assert_eq!(pauses[..owned], vec![LATER[0]; owned][..]);
            assert_eq!(pauses[owned..], [LATER[3], LATER[3]], "{pauses:?}");
            waits
        };

        // The model beside the real worker, look for look, to past the
        // backstop, from a fresh run.
        let _ = walk(&[], "held-back:attended", ("Buggy", 1));

        // The review of 2026-09-28: the notice, then its answers without
        // READY at turn ends of their own — more of them than the backstop's
        // looks — then the READY a person holds (`wait:attended`: the
        // notice's hold said `held-back:attended` once already).
        let mut noticed = vec![("announced:1", "", false)];
        noticed.extend(std::iter::repeat_n(
            ("wait:awaiting-ready", "HostWaitsBefore", true),
            before,
        ));
        let fed = walk(&noticed, "wait:attended", ("Every", 1));
        assert_eq!(
            fed, 0,
            "the notice's answers without READY count toward no bound \
             (`upgrade_drive::bounded_wait`)"
        );
        let every = u32::try_from(before).unwrap();
        assert!(
            !upgrade_drive::owns_turn_ends("wait:attended", every, grace),
            "NEGATIVE CONTROL: the count the host fed before this fix (every wait in a row, \
             {every}) owns nothing"
        );

        // A person who held the re-ask at its point past the backstop, and
        // then the agent's READY.
        let held = vec![("wait:attended", "HostHoldsBefore", false); before];
        let _ = walk(&held, "wait:attended", ("Stretch", 1));

        // NEGATIVE CONTROL: an answer without READY owns nothing.
        let (idle, _) = attended_worker(&world, &["wait:awaiting-ready"]);
        idle.at_idle().expect("a step");
        assert!(!idle.owns_turn_end());
    }

    /// A READY AFTER NOTICES TYPED AT BREAKS OWNS ITS SETTLE (2026-09-28, tab
    /// s-5c03a): a person at the tab at its idle points from 14:48 (one
    /// `held-back:attended`, then `wait:attended` looks), three notices typed at
    /// BREAKS of the agent's own work (16:54, 17:26, 17:56), then the READY at
    /// 18:21:02 — whose first look waits the restart's settle
    /// (`wait:settling`). On 0.98 that look owned nothing: every wait counted
    /// toward the settle's bound, and nothing typed at a break started the
    /// count over, so the loop typed `keep going` over the READY one second
    /// later and voided it. The REAL worker, its idle steps and its breaks'
    /// notices scripted as that upgrade answered them: the READY's settle is
    /// the upgrade's for `OWNED_SETTLE_LOOKS` looks, and the loop's after.
    ///
    /// **TIER-1 FOR `UpgradeSettleTurnEnd`** (aterm-spec
    /// `upgrade_settle_turn_end_model`): each idle step before the READY is the
    /// model's `OtherWait`, `SettleBefore` or `NoticeAtIdle`, each break's notice
    /// its `NoticeAtBreak`, the READY's turn (`turn_ran`) its `ReadyArrives`,
    /// each look at the READY's point its `HostSettles`, and the worker's
    /// ownership after each look must be the model's `owns`. NEGATIVE
    /// CONTROLS, each the real worker against a model dial it must disagree
    /// with at the READY's first look: the incident's builds (`Buggy`), a
    /// notice at a break that leaves the count (`Carry`, over settles before
    /// it), and every wait counted (`Every`, over the notice's answers without
    /// READY).
    #[test]
    fn a_ready_after_notices_at_breaks_owns_its_settle() {
        let world = Arc::new(World::default());
        let model = aterm_spec::interp::with_consts(
            &aterm_spec::derive::upgrade_settle_turn_end_model(),
            &[("Steps", 12)],
        );
        let bound = usize::try_from(upgrade_drive::OWNED_SETTLE_LOOKS).unwrap();
        // `before`: each idle step before the READY (its word, its model
        // action, whether a turn ran before it); `breaks`: the notices typed at
        // breaks after them; then the READY's turn and its settle's looks.
        let walk =
            |before: &[(&'static str, &'static str, bool)], breaks: usize, against: (&str, i64)| {
                let mut script: Vec<&str> = before.iter().map(|(word, _, _)| *word).collect();
                script.extend(std::iter::repeat_n("wait:settling", bound + 1));
                let (mut idle, _) = attended_worker(&world, &script);
                let notices: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(
                    (1..=breaks).map(|n| format!("announced:{n}")).collect(),
                ));
                let typed = Arc::clone(&notices);
                idle.hooks.acts.notice = Arc::new(move |_, _| {
                    typed
                        .lock()
                        .unwrap()
                        .pop_front()
                        .expect("a scripted notice")
                });
                let wrong = aterm_spec::interp::with_consts(&model, &[against]);
                let (mut st, mut w) = (model.init_state(), wrong.init_state());
                for (n, (word, action, turn)) in before.iter().enumerate() {
                    if *turn {
                        idle.turn_ran();
                    }
                    let step = idle.at_idle().expect("a step");
                    assert_eq!(step.line, format!("upgrade step={word}"));
                    assert!(model.fire(action, &mut st), "{n}: {action} at {st:?}");
                    assert!(wrong.fire(action, &mut w), "{n}: {action}");
                }
                for n in 1..=breaks {
                    // The break's own pause since the last look.
                    *idle.background_at.lock().unwrap() = None;
                    assert_eq!(
                        idle.at_background(),
                        Some(format!("upgrade step=announced:{n}")),
                        "the notice at break {n}"
                    );
                    assert!(idle.owns_turn_end(), "a notice owns the turn ends");
                    assert!(model.fire("NoticeAtBreak", &mut st), "{st:?}");
                    assert!(wrong.fire("NoticeAtBreak", &mut w));
                }
                // The READY: a turn of the agent's.
                idle.turn_ran();
                assert!(model.fire("ReadyArrives", &mut st), "{st:?}");
                assert!(wrong.fire("ReadyArrives", &mut w));
                let mut owned = Vec::new();
                for n in 0..=bound {
                    let step = idle.at_idle().expect("a settle look");
                    assert_eq!(step.line, "upgrade step=wait:settling");
                    assert!(model.fire("HostSettles", &mut st), "{n}: {st:?}");
                    assert!(wrong.fire("HostSettles", &mut w), "{n}");
                    assert_eq!(
                        i64::from(idle.owns_turn_end()),
                        st["owns"],
                        "look {n} at the READY: the real worker against the model at {st:?}"
                    );
                    if n == 0 {
                        assert_eq!(st["owns"], 1, "the READY's first look is the upgrade's");
                        assert_ne!(
                            i64::from(idle.owns_turn_end()),
                            w["owns"],
                            "NEGATIVE CONTROL {against:?}: that host owned nothing here"
                        );
                    }
                    owned.push(idle.owns_turn_end());
                }
                owned
            };
        let mut lapses = vec![true; bound];
        lapses.push(false);

        // The incident: a person's looks at the idle points, then the notices
        // at breaks, then the READY.
        let mut person = vec![("held-back:attended", "OtherWait", false)];
        person.extend(std::iter::repeat_n(
            ("wait:attended", "OtherWait", false),
            5,
        ));
        assert_eq!(walk(&person, 3, ("Buggy", 1)), lapses, "the incident");

        // Settles before the first notice, which is typed at a break.
        let settled = vec![("wait:settling", "SettleBefore", false); bound + 1];
        assert_eq!(walk(&settled, 1, ("Carry", 1)), lapses, "settles before");

        // A notice at an idle point, then its answers without READY at turn
        // ends of their own.
        let mut answered = vec![("announced:1", "NoticeAtIdle", false)];
        answered.extend(std::iter::repeat_n(
            ("wait:awaiting-ready", "OtherWait", true),
            bound + 1,
        ));
        assert_eq!(walk(&answered, 0, ("Every", 1)), lapses, "answers before");
    }

    /// A NOTICE ITS OWN FENCE REFUSED OWNS ITS POINT, BOUNDED (2026-09-28,
    /// s-d3346): a person at the tab at 14:50:37 (`held-back:attended`), the
    /// owner's `--now` at 14:50:50, and at the idle point the notice was due
    /// its own fence refused it (`wait:announce-refused:changed`) at 14:50:58,
    /// 14:51:19 and 14:52:21 — each look owning nothing and climbing the
    /// ladder, until the loop typed `keep going` into the point at 14:52:45
    /// and the upgrade had no look for six hours. The REAL worker, its steps
    /// scripted as that upgrade answered them: each refused look owns the
    /// session's turn ends while its count since the upgrade's last act is
    /// within `OWNED_REFUSED_LOOKS`, and asks for the first rung, so the
    /// notice is tried again at the point it is due; past the bound the point
    /// is the loop's, on the ladder, and a turn of the agent's renews nothing.
    ///
    /// **TIER-1 FOR `UpgradeRefusedTurnEnd`** (aterm-spec
    /// `upgrade_refused_turn_end_model`): each refused look is the model's
    /// `Refused`, the loop's continuation past the bound its `LoopContinues`,
    /// the next turn end its `PointComes`, and the worker's ownership after
    /// each look must be the model's `owns`. NEGATIVE CONTROLS: the builds
    /// before (`Buggy`: a refused notice owns nothing) disagree with the real
    /// worker at the first refused look; a count per point (`Renew`) at the
    /// first look after the turn.
    #[test]
    fn a_refused_notice_owns_its_point_for_its_looks() {
        use upgrade_drive::LATER;
        let world = Arc::new(World::default());
        let model = aterm_spec::derive::upgrade_refused_turn_end_model();
        let bound = usize::try_from(upgrade_drive::OWNED_REFUSED_LOOKS).unwrap();
        let refused = "wait:announce-refused:changed";
        let mut steps = vec!["held-back:attended"];
        steps.extend(std::iter::repeat_n(refused, bound + 2));
        let (idle, paused) = attended_worker(&world, &steps);
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let renew = aterm_spec::interp::with_consts(&model, &[("Renew", 1)]);
        let (mut st, mut b, mut r) = (model.init_state(), buggy.init_state(), renew.init_state());
        // The person at the tab before the owner's word: its own count.
        assert_eq!(
            idle.at_idle().expect("a step").line,
            "upgrade step=held-back:attended"
        );
        let pauses_before = paused.lock().unwrap().len();
        for n in 0..=bound {
            let step = idle.at_idle().expect("a refused look");
            assert_eq!(step.line, format!("upgrade step={refused}"));
            assert!(!step.moved, "{n}: nothing typed");
            for (m, s) in [(&model, &mut st), (&buggy, &mut b), (&renew, &mut r)] {
                assert!(m.fire("Refused", s), "{n}: {s:?}");
            }
            assert_eq!(
                i64::from(idle.owns_turn_end()),
                st["owns"],
                "refused look {n}: the real worker against the model at {st:?}"
            );
            if n == 0 {
                assert_eq!(st["owns"], 1, "the refused notice owns its point");
                assert_ne!(
                    i64::from(idle.owns_turn_end()),
                    b["owns"],
                    "NEGATIVE CONTROL Buggy: the builds before owned nothing here"
                );
            }
        }
        assert!(!idle.owns_turn_end(), "past the bound: the loop's");
        let pauses = paused.lock().unwrap()[pauses_before..].to_vec();
        assert_eq!(pauses[..bound], vec![LATER[0]; bound][..], "{pauses:?}");
        assert_eq!(
            pauses[bound], LATER[bound],
            "back on the ladder: {pauses:?}"
        );
        // The loop continues; the agent's turn ends at a new point.
        for (m, s) in [(&model, &mut st), (&renew, &mut r)] {
            assert!(m.fire("LoopContinues", s), "{s:?}");
            assert!(m.fire("PointComes", s), "{s:?}");
            assert!(m.fire("Refused", s), "{s:?}");
        }
        idle.turn_ran();
        let step = idle.at_idle().expect("the next point's look");
        assert_eq!(step.line, format!("upgrade step={refused}"));
        assert_eq!(
            i64::from(idle.owns_turn_end()),
            st["owns"],
            "a turn renews nothing: {st:?}"
        );
        assert_eq!(st["owns"], 0);
        assert_ne!(
            i64::from(idle.owns_turn_end()),
            r["owns"],
            "NEGATIVE CONTROL Renew: a count per point owned it again"
        );
    }

    /// A REFUSED NOTICE'S LOOKS ARE A COUNT OF THEIR OWN, AND THE OWNER'S WORD
    /// STARTS IT OVER (the review of 2026-09-28). Two paths the incident's
    /// test could not see, since its person's look (`held-back:attended`)
    /// started every count over before the refusals: a round whose notice
    /// gate SETTLED for more looks than a refused notice owns (a busy agent's
    /// `wait:settling`, `wait:not-idle`), then refused — its first refused look
    /// owned nothing when both shared one count; and the OWNER'S WORD after
    /// the refusals past the bound (the `Upgrade now` the band offers once
    /// they have stood ninety minutes) — the next refused look owned nothing,
    /// its looks spent before the word, and the loop's `keep going` took the
    /// point. The REAL worker, its steps scripted, the word reaching it as the
    /// host's activation notice does ([`WorkerIdle::activations`]).
    ///
    /// **TIER-1 FOR `UpgradeRefusedTurnEnd`** (aterm-spec
    /// `upgrade_refused_turn_end_model`): each settle look is the model's
    /// `SettleBefore`, each refused look its `Refused`, the word its
    /// `OwnerWord`, and the worker's ownership after each refused look must be
    /// the model's `owns`. NEGATIVE CONTROLS, each the real worker against a
    /// model dial it must disagree with: one shared count (`Shared`) at the
    /// first refused look after the settles; a word that leaves the count
    /// standing (`Stale`) at the first refused look after the word.
    #[test]
    fn a_refused_notices_count_is_its_own_and_the_owners_word_starts_it_over() {
        let world = Arc::new(World::default());
        let model = aterm_spec::derive::upgrade_refused_turn_end_model();
        let bound = usize::try_from(upgrade_drive::OWNED_REFUSED_LOOKS).unwrap();
        let settles = usize::try_from(upgrade_drive::OWNED_SETTLE_LOOKS).unwrap() + 1;
        let refused = "wait:announce-refused:changed";

        // Settles, then a refusal: the refusal's own count.
        let mut steps = vec!["wait:settling"; settles];
        steps.push(refused);
        let (idle, _) = attended_worker(&world, &steps);
        let shared = aterm_spec::interp::with_consts(&model, &[("Shared", 1)]);
        let (mut st, mut w) = (model.init_state(), shared.init_state());
        for n in 0..settles {
            assert_eq!(
                idle.at_idle().expect("a settle look").line,
                "upgrade step=wait:settling"
            );
            assert!(model.fire("SettleBefore", &mut st), "{n}: {st:?}");
            assert!(shared.fire("SettleBefore", &mut w), "{n}");
        }
        assert_eq!(
            idle.at_idle().expect("the refused look").line,
            format!("upgrade step={refused}")
        );
        assert!(model.fire("Refused", &mut st), "{st:?}");
        assert!(shared.fire("Refused", &mut w));
        assert_eq!(
            i64::from(idle.owns_turn_end()),
            st["owns"],
            "after the settles: the real worker against the model at {st:?}"
        );
        assert_eq!(st["owns"], 1, "the refused notice owns its point");
        assert_ne!(
            i64::from(idle.owns_turn_end()),
            w["owns"],
            "NEGATIVE CONTROL Shared: one count, spent by the settles, owned nothing"
        );

        // Refusals past the bound, then the owner's word: the next refused
        // look owns the point again.
        let steps = vec![refused; bound + 2];
        let (idle, _) = attended_worker(&world, &steps);
        let stale = aterm_spec::interp::with_consts(&model, &[("Stale", 1)]);
        let (mut st, mut w) = (model.init_state(), stale.init_state());
        for n in 0..=bound {
            assert_eq!(
                idle.at_idle().expect("a refused look").line,
                format!("upgrade step={refused}")
            );
            assert!(model.fire("Refused", &mut st), "{n}: {st:?}");
            assert!(stale.fire("Refused", &mut w), "{n}");
            assert_eq!(i64::from(idle.owns_turn_end()), st["owns"], "{n}: {st:?}");
        }
        assert!(!idle.owns_turn_end(), "past the bound: the loop's");
        // The owner's word (or a newer build): the host's activation notice.
        idle.activations.fetch_add(1, Ordering::SeqCst);
        assert!(model.fire("OwnerWord", &mut st), "{st:?}");
        assert!(stale.fire("OwnerWord", &mut w));
        assert_eq!(
            idle.at_idle().expect("the look after the word").line,
            format!("upgrade step={refused}")
        );
        assert!(model.fire("Refused", &mut st), "{st:?}");
        assert!(stale.fire("Refused", &mut w));
        assert_eq!(
            i64::from(idle.owns_turn_end()),
            st["owns"],
            "after the word: the real worker against the model at {st:?}"
        );
        assert_eq!(st["owns"], 1, "the word's refused notice owns its point");
        assert_ne!(
            i64::from(idle.owns_turn_end()),
            w["owns"],
            "NEGATIVE CONTROL Stale: the word left the count spent, and owned nothing"
        );
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
        assert!(upgrade_drive::owns_turn_ends("announced", 0, 120));
        assert!(upgrade_drive::owns_turn_ends("announced:2", 0, 120));
        assert!(upgrade_drive::owns_turn_ends("wait:settling", 0, 120));
        assert!(!upgrade_drive::owns_turn_ends(
            "wait:settling",
            upgrade_drive::OWNED_SETTLE_LOOKS,
            120
        ));
        assert!(upgrade_drive::owns_turn_ends(
            "wait:background",
            upgrade_drive::OWNED_BACKGROUND_LOOKS - 1,
            120
        ));
        assert!(!upgrade_drive::owns_turn_ends(
            "wait:background",
            upgrade_drive::OWNED_BACKGROUND_LOOKS,
            120
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
            assert!(!upgrade_drive::owns_turn_ends(last, 0, 120), "{last}");
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
            "wait:queued",
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
        // The second word is a wait: a third step (`current`) follows a quick
        // pause later, so the count is waited past 2, never for exactly 2.
        until("two steps", || a.stepped.load(Ordering::SeqCst) >= 2);
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
        // Codex: the restart IN PLACE is Claude Code's alone (its memory
        // banner and its model buckets are Claude Code's); a Codex that
        // exits is relaunched on exit instead.
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

    /// A NAME THE ROSTER COULD NOT READ IS NO EXIT (2026-09-28): a pass
    /// that does not name the agent while it still holds its tab — asked
    /// about the group the roster last named it in — keeps the worker: the
    /// SAME loop is seen running on after the misread (one run, still owning
    /// the turn ends of the session it announced to — a stopped loop writes
    /// nothing, and a new one owns nothing), and once the roster names the
    /// agent again the host asks about it no more. An exit DURING the
    /// misread — here one nothing rings for, the case where neither the
    /// agent's name nor the shell's could be read ([`keep_holding`]: in the
    /// live timeline the shell's name resolving usually rings first) — is
    /// handed at a later pass (the look [`keep_holding`] sets, bound in the
    /// host's own pass by `a_pass_that_keeps_an_agent_owes_the_host_a_look`)
    /// and relaunched.
    /// NEGATIVE CONTROL: a hold that cannot be read decides nothing, so the
    /// pass hands the exit as before.
    #[test]
    fn a_name_the_roster_cannot_read_is_no_exit_while_the_agent_holds_its_tab() {
        let a = Arc::new(Acting::default());
        a.group.store(4242, Ordering::SeqCst);
        *a.steps.lock().unwrap() = ["announced:1"].into();
        a.due.store(true, Ordering::SeqCst);
        a.open.lock().unwrap().insert("s-n".to_string());
        // Announced: the agent's turn runs (no idle point), the upgrade owns
        // its turn end.
        let busy = Arc::clone(&a);
        *a.during_step.lock().unwrap() = Some(Box::new(move || {
            busy.busy.store(true, Ordering::SeqCst);
        }));
        a.set(&[("s-n", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("announced, the turn end owned", || {
            a.stepped.load(Ordering::SeqCst) == 1 && *a.owns_now.lock().unwrap() == Some(true)
        });
        let asked = || a.holds_asked.lock().unwrap().len();
        // The loop is seen running on from here, and it is the one that
        // announced.
        let running_on = |what: &str| {
            *a.owns_now.lock().unwrap() = None;
            until(what, || a.owns_now.lock().unwrap().is_some());
            assert_eq!(
                *a.owns_now.lock().unwrap(),
                Some(true),
                "{what}: its upgrade's memory"
            );
            assert_eq!(a.runs.load(Ordering::SeqCst), 1, "{what}: the same loop");
        };
        // The misread: the agent still holds its tab.
        *a.holds.lock().unwrap() = Some(true);
        a.set(&[]);
        until("decided at the pass and again, or handed", || {
            asked() >= 2 || a.exit_looks.load(Ordering::SeqCst) > 0
        });
        assert_eq!(a.exit_looks.load(Ordering::SeqCst), 0, "no exit handed");
        assert!(
            a.holds_asked.lock().unwrap().iter().all(|g| *g == 4242),
            "the group the roster named it in: {:?}",
            a.holds_asked.lock().unwrap()
        );
        assert_eq!(host.live(), ["s-n"], "kept");
        running_on("kept, running on");
        assert!(a.relaunched.lock().unwrap().is_empty());
        assert!(a.badges.lock().unwrap().is_empty());
        // Named again: nothing was an exit, the same loop runs on, and the
        // host asks about the agent no more (a pass already under way may
        // have asked once more).
        a.set(&[("s-n", Program::Claude)]);
        until("asked about no more", || {
            let before = asked();
            std::thread::sleep(HOLDS_LOOK * 2);
            asked() == before
        });
        running_on("named again, running on");
        assert_eq!(a.exit_looks.load(Ordering::SeqCst), 0);
        assert!(a.relaunched.lock().unwrap().is_empty());
        // An exit during a misread: no bell of its own rings for it.
        a.set(&[]);
        let before = asked();
        until("kept again", || asked() > before);
        *a.holds.lock().unwrap() = Some(false);
        until("handed at a later pass, and relaunched", || {
            a.relaunched.lock().unwrap().len() == 1
        });
        until("the relaunching worker reaped", || host.live().is_empty());
        // NEGATIVE CONTROL: a hold nobody can read keeps nothing.
        a.set(&[("s-n", Program::Claude)]);
        until("attached again", || host.live() == ["s-n"]);
        *a.holds.lock().unwrap() = None;
        a.set(&[]);
        until("handed at the pass", || {
            a.relaunched.lock().unwrap().len() == 2
        });
        host.shutdown_and_join();
    }

    /// A worker the tests hand [`keep_holding`], its stop flags and
    /// interrupter unwired: `group` shared as a live one's is.
    fn idle_worker() -> Worker {
        Worker {
            join: None,
            done: Arc::default(),
            stop: Arc::default(),
            handover: Arc::default(),
            interrupt: Arc::default(),
            cfg_gen: 0,
            epoch: 0,
            park: Arc::default(),
            look_at: Arc::default(),
            left: Arc::default(),
            acting: Arc::default(),
            stepped: Arc::default(),
            activations: 0,
            note: Arc::default(),
            group: Arc::default(),
            points: Arc::default(),
        }
    }

    /// [`keep_holding`] over `holding` and `workers` for a pass whose roster
    /// names `named` (sid `s-k`, a Codex, at those stamps): `(the look owed,
    /// wanted, the stamps the pass follows by)`.
    fn keep_pass(
        holding: &mut HashMap<String, Held>,
        workers: &Workers,
        named: Option<FollowStamp>,
        hooks: &Hooks,
    ) -> (
        Option<Instant>,
        HashMap<String, Program>,
        HashMap<String, FollowStamp>,
    ) {
        let mut wanted: HashMap<String, Program> = HashMap::new();
        let mut stamps: HashMap<String, FollowStamp> = HashMap::new();
        if let Some(stamp) = named {
            wanted.insert("s-k".to_string(), Program::Codex);
            stamps.insert("s-k".to_string(), stamp);
        }
        let at = keep_holding(holding, workers, &mut wanted, &mut stamps, hooks);
        (at, wanted, stamps)
    }

    /// A stamp in foreground group `group`.
    fn in_group(group: i32) -> FollowStamp {
        FollowStamp {
            group,
            ..FollowStamp::default()
        }
    }

    /// THE KEEP ([`keep_holding`], the leave model's `Visit`): an agent the
    /// roster names has its group recorded — its worker's too — and nothing
    /// kept; one it does not name whose agent still holds its tab — asked
    /// about that group — is kept wanted as the agent it was named, at the
    /// stamp it was named with, with a look [`HOLDS_LOOK`] on that a pass
    /// before it came due leaves where it was; and one whose worker ENDED
    /// meanwhile (handed over, or failed and gone) is kept wanted too, so the
    /// pass starts it again. NEGATIVE CONTROLS: an agent that holds its tab
    /// no longer, and a hold that cannot be read, are forgotten with no look;
    /// one whose worker's own step is acting through it is not asked (and
    /// not forgotten), and one whose worker was handed its exit is forgotten
    /// unasked. THE CEILING: one kept [`HOLDS_KEEP_MAX`] already is forgotten
    /// though it still reads as holding its tab — and one kept a second less
    /// is not.
    #[test]
    fn a_kept_agent_is_looked_at_again_and_nothing_else_is_kept() {
        let a = Arc::new(Acting::default());
        let hooks = a.hooks();
        let mut workers = Workers::default();
        workers.0.insert("s-k".to_string(), idle_worker());
        let mut holding: HashMap<String, Held> = HashMap::new();
        let name = |holding: &mut HashMap<String, Held>, workers: &Workers| {
            assert_eq!(
                keep_pass(holding, workers, Some(in_group(9)), &hooks).0,
                None
            );
        };
        name(&mut holding, &workers);
        assert_eq!(
            (
                holding["s-k"].stamp,
                holding["s-k"].look,
                holding["s-k"].kept
            ),
            (in_group(9), None, None),
            "the stamp it was named with"
        );
        assert_eq!(
            workers.0["s-k"].group.load(Ordering::SeqCst),
            9,
            "its worker's"
        );
        assert!(
            a.holds_asked.lock().unwrap().is_empty(),
            "a named agent is not asked"
        );
        let keep = |holding: &mut HashMap<String, Held>, workers: &Workers| {
            let (at, wanted, _) = keep_pass(holding, workers, None, &hooks);
            (at, wanted)
        };
        // Not named, still holding its tab: kept, and looked at again.
        *a.holds.lock().unwrap() = Some(true);
        let before = Instant::now();
        let (at, wanted) = keep(&mut holding, &workers);
        let after = Instant::now();
        let at = at.expect("a look");
        assert!(
            at >= before + HOLDS_LOOK && at <= after + HOLDS_LOOK,
            "{at:?}"
        );
        assert_eq!(wanted, [("s-k".to_string(), Program::Codex)].into());
        assert_eq!(*a.holds_asked.lock().unwrap(), [9]);
        // A pass before the look came due leaves it where it was (one a
        // loaded machine ran later is [`hold_look`]'s, bound on its own).
        let again = keep(&mut holding, &workers).0;
        if Instant::now() < at {
            assert_eq!(again, Some(at));
        }
        // Its worker ended meanwhile: still kept wanted, to be started again.
        let gone = Workers::default();
        let (again, wanted) = keep(&mut holding, &gone);
        assert!(again.is_some());
        assert_eq!(wanted, [("s-k".to_string(), Program::Codex)].into());
        // THE CEILING: kept a second short of it, kept; kept for it, not —
        // though the hold still reads true.
        for (kept_for, still) in [
            (HOLDS_KEEP_MAX - Duration::from_secs(1), true),
            (HOLDS_KEEP_MAX, false),
        ] {
            name(&mut holding, &workers);
            keep(&mut holding, &workers);
            let since = Instant::now().checked_sub(kept_for).expect("uptime");
            holding.get_mut("s-k").expect("kept").kept = Some((since, 0));
            let (at, wanted) = keep(&mut holding, &workers);
            assert_eq!(
                (at.is_some(), wanted.len(), holding.len()),
                if still { (true, 1, 1) } else { (false, 0, 0) },
                "kept for {kept_for:?}"
            );
        }
        // Holding it no longer, or unread: forgotten, no look.
        for hold in [Some(false), None] {
            name(&mut holding, &workers);
            *a.holds.lock().unwrap() = hold;
            let (at, wanted) = keep(&mut holding, &workers);
            assert_eq!((at, wanted.len()), (None, 0), "{hold:?}");
            assert!(holding.is_empty(), "{hold:?}");
        }
        // Acting: not asked, and kept for the step's end; handed its exit:
        // forgotten, unasked.
        *a.holds.lock().unwrap() = Some(true);
        let asked = a.holds_asked.lock().unwrap().len();
        name(&mut holding, &workers);
        workers.0["s-k"].acting.store(true, Ordering::SeqCst);
        let (at, wanted) = keep(&mut holding, &workers);
        assert_eq!((at, wanted.len(), holding.len()), (None, 0, 1), "acting");
        workers.0["s-k"].acting.store(false, Ordering::SeqCst);
        workers.0["s-k"].left.store(true, Ordering::SeqCst);
        let (at, wanted) = keep(&mut holding, &workers);
        assert_eq!((at, wanted.len(), holding.len()), (None, 0, 0), "left");
        assert_eq!(a.holds_asked.lock().unwrap().len(), asked, "not asked");
    }

    /// A KEPT AGENT IS FOLLOWED AS ANY OTHER, at the full follows: the pass
    /// that keeps it follows it by the stamp it was last named with
    /// ([`Held::stamp`]), so a bell of any other tab — a single phase wake,
    /// no full follow — reads nothing of it, where a session with no stamp
    /// is followed at every pass ([`follow_entries`]' fallback): Claude's
    /// record of it read and parsed at the machine's bell rate through a
    /// misread. NEGATIVE CONTROL: a full follow follows it.
    #[test]
    fn a_kept_agent_is_followed_at_full_follows_not_at_every_bell() {
        let a = Arc::new(Acting::default());
        let mut hooks = a.hooks();
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        hooks.acts.follow = Arc::new(move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            Foreground::Agent
        });
        let mut workers = Workers::default();
        workers.0.insert("s-k".to_string(), idle_worker());
        let following = vec![(
            "s-k".to_string(),
            Arc::new(Mutex::new(Kept {
                snapshot: Some(snap("s-k")),
                ..Kept::default()
            })),
        )];
        let mut holding = HashMap::new();
        let mut followed = HashMap::new();
        let stamp = FollowStamp {
            group: 9,
            reader: Some(Program::Codex),
            rev: 4,
        };
        // Named, and followed once at that stamp.
        let (_, _, stamps) = keep_pass(&mut holding, &workers, Some(stamp), &hooks);
        follow_entries(&following, &stamps, &mut followed, false, &hooks);
        assert_eq!(reads.load(Ordering::SeqCst), 1, "followed at its stamp");
        // The misread: kept, at the stamp it was named with.
        *a.holds.lock().unwrap() = Some(true);
        let (at, _, stamps) = keep_pass(&mut holding, &workers, None, &hooks);
        assert!(at.is_some(), "kept");
        assert_eq!(
            stamps.get("s-k"),
            Some(&stamp),
            "the stamp it was named with"
        );
        follow_entries(&following, &stamps, &mut followed, false, &hooks);
        assert_eq!(reads.load(Ordering::SeqCst), 1, "no read at another's bell");
        // NEGATIVE CONTROL: a full follow follows it.
        follow_entries(&following, &stamps, &mut followed, true, &hooks);
        assert_eq!(reads.load(Ordering::SeqCst), 2, "a full follow");
    }

    /// ANOTHER GROUP NAMED IN A KEPT AGENT'S STEAD IS ITS EXIT: the kept
    /// agent ended during the misread and nothing rang (neither its name nor
    /// the shell's could be read), and a new agent was started and named in
    /// the tab before a look saw the first go — the worker is handed the
    /// first one's exit ([`Worker::leave`]), so its loop and what it
    /// remembers of the upgrade (an announcement's turn end) end with the
    /// agent they were about, never carried on to another process.
    /// Another tab's step, or the window's look at the upgrades
    /// ([`look_at_upgrades_soon`]), taken during the keep relaunches nothing
    /// here: the exit is still handed (the process-wide [`UPGRADE_ACTS`] the
    /// keep once read let either cancel it — the review of 2026-09-28).
    /// NEGATIVE CONTROLS: named again in the SAME group (the misread healed)
    /// carries on; the worker's own step taken since the keep began (the
    /// step's relaunch, in a new group, is the step's), a step running at the
    /// pass, and a keep a running step had already let go of, carry on too.
    #[test]
    fn another_group_named_in_a_kept_agents_stead_is_its_exit() {
        let a = Arc::new(Acting::default());
        let hooks = a.hooks();
        *a.holds.lock().unwrap() = Some(true);
        // (named again in, a step elsewhere since the keep began, its own
        // worker's step since, acting at the pass, acting at a pass of the
        // keep) -> handed.
        for (group, elsewhere, stepped, acting_now, acted, handed) in [
            (10, false, false, false, false, true),
            (10, true, false, false, false, true),
            (9, false, false, false, false, false),
            (10, false, true, false, false, false),
            (10, false, false, true, false, false),
            (10, false, false, false, true, false),
        ] {
            let case = format!(
                "group {group} elsewhere {elsewhere} stepped {stepped} acting {acting_now} \
                 acted {acted}"
            );
            let mut workers = Workers::default();
            workers.0.insert("s-k".to_string(), idle_worker());
            let w = |workers: &Workers| {
                let w = &workers.0["s-k"];
                (w.left.load(Ordering::SeqCst), w.stop.load(Ordering::SeqCst))
            };
            let mut holding = HashMap::new();
            keep_pass(&mut holding, &workers, Some(in_group(9)), &hooks);
            let (at, _, _) = keep_pass(&mut holding, &workers, None, &hooks);
            assert!(at.is_some(), "{case}: kept");
            if acted {
                workers.0["s-k"].acting.store(true, Ordering::SeqCst);
                keep_pass(&mut holding, &workers, None, &hooks);
                workers.0["s-k"].acting.store(false, Ordering::SeqCst);
            }
            if elsewhere {
                look_at_upgrades_soon();
            }
            if stepped {
                // What [`WorkerIdle::act`] leaves as a step begins.
                let stamp = STEP_STAMPS.fetch_add(1, Ordering::SeqCst) + 1;
                workers.0["s-k"].stepped.store(stamp, Ordering::SeqCst);
            }
            workers.0["s-k"].acting.store(acting_now, Ordering::SeqCst);
            keep_pass(&mut holding, &workers, Some(in_group(group)), &hooks);
            assert_eq!(w(&workers), (handed, handed), "{case}");
            assert_eq!(
                (holding["s-k"].stamp.group, holding["s-k"].kept),
                (group, None),
                "{case}: named now"
            );
            assert_eq!(workers.0["s-k"].group.load(Ordering::SeqCst), group);
        }
    }

    /// THE KEEP'S LOOK GROWS ([`hold_look`]): first [`HOLDS_LOOK`] on, then
    /// each look that finds the agent still kept doubles the step, to
    /// [`HOLDS_LOOK_MAX`] — a long misread costs a pass every two seconds,
    /// not four a second. A pass before the look came due keeps it.
    #[test]
    fn the_keeps_look_doubles_to_its_cap() {
        let now = Instant::now();
        assert_eq!(hold_look(None, now), (now + HOLDS_LOOK, HOLDS_LOOK));
        let early = (now + Duration::from_millis(100), HOLDS_LOOK);
        assert_eq!(hold_look(Some(early), now), early, "not due yet");
        let mut look = (now, HOLDS_LOOK);
        let mut steps = Vec::new();
        for _ in 0..5 {
            look = hold_look(Some((now, look.1)), now);
            assert_eq!(look.0, now + look.1);
            steps.push(look.1.as_millis());
        }
        assert_eq!(steps, [500, 1000, 2000, 2000, 2000]);
        assert_eq!(HOLDS_LOOK, Duration::from_millis(250));
    }

    /// THE KEEP'S LOOK, IN THE HOST'S OWN PASS ([`host_pass`], the leave
    /// model's `Visit` keeping `due`; the host of `VisitNoLook` is what this
    /// catches): a pass that keeps an agent the roster cannot name owes the
    /// host a look [`HOLDS_LOOK`] on — the instant the host's wait ends by
    /// itself, the backstop for an exit during the misread that no bell rang
    /// for ([`keep_holding`]: neither the agent's name nor the shell's read)
    /// — and a look
    /// that finds it still kept owes the next, further on. This host's passes
    /// are this test's alone (its thread never runs), so no bell of another
    /// test's can stand in for the look. Through the misread the worker is
    /// the tab's: a FAILED RUN restarts its loop on its back-off
    /// ([`Hooks::still_wanted`] asked about the group it was named in), and a
    /// changed policy hands it over to a new worker, which starts once the
    /// old one ended. NEGATIVE CONTROL: an agent that holds its tab no longer
    /// is handed its exit, and the host owes itself no look.
    #[test]
    fn a_pass_that_keeps_an_agent_owes_the_host_a_look() {
        let a = Arc::new(Acting::default());
        a.group.store(11, Ordering::SeqCst);
        a.open.lock().unwrap().insert("s-p".to_string());
        a.set(&[("s-p", Program::Claude)]);
        let handle = HostHandle::start(on(), false, true, a.hooks());
        handle.shared.lock().suspended = false;
        let mut host = HostState::from_shared(&handle.shared);
        let pass = |host: &mut HostState| match host_pass(&handle.shared, host, true, &handle.hooks)
        {
            ControlFlow::Continue(until) => until,
            ControlFlow::Break(()) => panic!("shut down"),
        };
        let owes_none =
            |until: Option<Instant>| until.is_none_or(|at| at > Instant::now() + HOLDS_LOOK_MAX);
        let until_first = pass(&mut host);
        assert!(owes_none(until_first), "named: {until_first:?}");
        until("its loop runs", || a.runs.load(Ordering::SeqCst) == 1);
        // The misread: the agent still holds its tab.
        *a.holds.lock().unwrap() = Some(true);
        a.set(&[]);
        let before = Instant::now();
        let first = pass(&mut host).expect("a look owed");
        assert!(
            first >= before + HOLDS_LOOK && first <= Instant::now() + HOLDS_LOOK,
            "{first:?}"
        );
        let again = pass(&mut host);
        if Instant::now() < first {
            assert_eq!(again, Some(first), "not due yet: where it was");
        }
        assert_eq!(handle.live(), ["s-p"]);
        // A step its worker took before the keep began is no step since, for
        // the worker that takes the session over during it (below).
        let before_keep = STEP_STAMPS.fetch_add(1, Ordering::SeqCst) + 1;
        host.holding.get_mut("s-p").expect("kept").kept = Some((Instant::now(), before_keep));
        // A failed run: the loop restarts on its back-off, the same worker.
        a.fail_once.store(true, Ordering::SeqCst);
        until("the loop run again", || a.runs.load(Ordering::SeqCst) == 2);
        assert_eq!(
            *a.still_asked.lock().unwrap(),
            [11],
            "the group it was named in"
        );
        assert_eq!(handle.faults_of("s-p"), 1, "a restart of its own loop");
        // A changed policy: handed over, and a new worker once it ended.
        {
            let mut state = handle.shared.lock();
            state.cfg_gen += 1;
        }
        assert!(pass(&mut host).is_some(), "still kept");
        until("the old worker ended", || {
            host.workers.0["s-p"].done.load(Ordering::SeqCst)
        });
        assert!(pass(&mut host).is_some(), "kept, and started again");
        until("the new worker's loop runs", || {
            a.runs.load(Ordering::SeqCst) == 3
        });
        assert_eq!(handle.live(), ["s-p"]);
        assert_eq!(host.workers.0["s-p"].group.load(Ordering::SeqCst), 11);
        assert_eq!(
            host.workers.0["s-p"].stepped.load(Ordering::SeqCst),
            before_keep,
            "the keep's stamp, taken over"
        );
        // At its look, still kept: looked at again, further on.
        let look = pass(&mut host).expect("a look owed");
        std::thread::sleep(look.saturating_duration_since(Instant::now()));
        let at = Instant::now();
        let next = pass(&mut host).expect("looked at again");
        assert!(next >= at + HOLDS_LOOK * 2, "{next:?} {at:?}");
        assert_eq!(a.exit_looks.load(Ordering::SeqCst), 0, "no exit handed");
        // NEGATIVE CONTROL: it holds its tab no longer — handed, no look.
        *a.holds.lock().unwrap() = Some(false);
        let handed = pass(&mut host);
        assert!(owes_none(handed), "{handed:?}");
        until("handed and relaunched", || {
            a.relaunched.lock().unwrap().len() == 1
        });
        handle.shared.lock().shutting_down = true;
        assert_eq!(
            host_pass(&handle.shared, &mut host, true, &handle.hooks),
            ControlFlow::Break(())
        );
        handle.shutdown_and_join();
    }

    /// How [`pty_job`]'s agent is parented, which decides how its end reads.
    #[cfg(unix)]
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum JobEnd {
        /// Its parent exits at once, so the agent is reaped (by init) the
        /// moment it ends.
        Reaped,
        /// As `Reaped`, with a second process in the agent's group that
        /// outlives it (a runtime's child, a pipeline's other end).
        Member,
        /// Its parent is the session leader, which never reaps it: once it
        /// ends it is a ZOMBIE.
        Zombie,
    }

    /// A pseudo-terminal whose SESSION LEADER stays — the stand-in for the
    /// tab's shell, a `sleep` that never takes the terminal back — and whose
    /// FOREGROUND group is a job of that session: a `sleep` leading its own
    /// group, made the terminal's foreground (`tcsetpgrp`) as a shell makes a
    /// job's — the stand-in for the agent holding the tab. `(master, the
    /// session leader, the agent's pid)`: the agent's group id is its pid.
    /// The caller ends both ([`end_pty_job`]).
    #[cfg(unix)]
    fn pty_job(end: JobEnd) -> (i32, std::process::Child, i32) {
        use std::ffi::CString;
        use std::os::fd::{FromRawFd as _, OwnedFd};
        use std::os::unix::process::CommandExt as _;
        // `openpty` from parallel threads fails now and then (aterm-session's
        // input_backlog_pty.rs, 2026-09-25): one at a time, and retried.
        static OPENPTY: Mutex<()> = Mutex::new(());
        let (mut master, mut slave) = (-1i32, -1i32);
        {
            let _one = OPENPTY.lock().unwrap_or_else(PoisonError::into_inner);
            for _ in 0..20 {
                // SAFETY: `openpty` fills the two out-params; the optional
                // pointers are null.
                let rc = unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                if rc == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert!(master >= 0 && slave >= 0, "openpty");
        // SAFETY: `slave` is a fresh fd this function alone owns.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let stdio = || std::process::Stdio::from(slave.try_clone().expect("dup slave"));
        // The agent's image and argv, made before the fork: nothing is
        // allocated in the child.
        let (image, secs) = (
            CString::new("/bin/sleep").expect("path"),
            CString::new("30").expect("arg"),
        );
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg("30").stdin(stdio()).stdout(stdio()).stderr(stdio());
        // SAFETY: runs in the forked child before exec, after stdio is in
        // place, and calls only async-signal-safe functions (`setsid`,
        // `ioctl`, `fork`, `setpgid`, `signal`, `tcsetpgrp`, `getpid`,
        // `execv`, `_exit`, `waitpid`) over memory made before the fork.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let argv = [image.as_ptr(), secs.as_ptr(), std::ptr::null()];
                // The agent's parent: this leader for a zombie, else a
                // go-between that exits at once.
                let between = if end == JobEnd::Zombie {
                    0
                } else {
                    libc::fork()
                };
                if between == 0 {
                    if libc::fork() == 0 {
                        // The agent: its own group, the terminal's foreground
                        // — and for `Member` a second process of that group,
                        // the fork's child, which runs the same image.
                        libc::setpgid(0, 0);
                        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                        libc::tcsetpgrp(0, libc::getpid());
                        if end == JobEnd::Member {
                            libc::fork();
                        }
                        libc::execv(argv[0], argv.as_ptr());
                        libc::_exit(127);
                    }
                    if end != JobEnd::Zombie {
                        libc::_exit(0);
                    }
                } else if between > 0 {
                    libc::waitpid(between, std::ptr::null_mut(), 0);
                }
                Ok(())
            });
        }
        let leader = cmd.spawn().expect("spawn the session leader");
        drop(slave);
        let shell = libc::pid_t::try_from(leader.id()).expect("pid");
        let agent = std::cell::Cell::new(0);
        until("the agent's group leads the pty's foreground", || {
            agent.set(crate::quit_safety::foreground_pgrp(master));
            agent.get() > 0 && agent.get() != shell
        });
        (master, leader, agent.get())
    }

    /// End what [`pty_job`] made: the agent's group, the session leader,
    /// the master.
    #[cfg(unix)]
    fn end_pty_job(master: i32, mut leader: std::process::Child, agent: i32) {
        // SAFETY: signals the test's own processes; closes the master fd the
        // test opened and alone owns.
        unsafe {
            libc::killpg(agent, libc::SIGKILL);
        }
        let _ = leader.kill();
        let _ = leader.wait();
        // SAFETY: as above.
        unsafe { libc::close(master) };
    }

    /// Kill `pid` alone, and wait until it is gone — reaped, not a zombie.
    #[cfg(unix)]
    fn kill_until_reaped(pid: i32) {
        // SAFETY: signals the test's own process.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        until("reaped", || {
            !aterm_uds::process::pid_alive(u32::try_from(pid).expect("pid"))
        });
    }

    /// Whether `pid` is a zombie: ended, not yet reaped by its parent — as
    /// `ps` reads its state (`Z`). macOS answers no `proc_pidinfo` for a
    /// zombie, so the process table is asked through the one reader both
    /// platforms ship.
    #[cfg(unix)]
    fn is_zombie(pid: i32) -> bool {
        std::process::Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .trim_start()
                    .starts_with('Z')
            })
    }

    /// A tab on `master` registered in a fresh store, its agent named in
    /// `group` as `program`: `(store, sid, name)` — `name` names it again.
    #[cfg(unix)]
    fn tab_on(master: i32, local: u64, group: i32) -> (Store, String, impl Fn(Option<&str>)) {
        let store = crate::session_store::new_store();
        let handle = crate::session_store::test_handle_on(local, master);
        let sid = handle.sid.as_str().to_string();
        let ctx = Arc::clone(&handle.ctx);
        let name = move |program: Option<&str>| {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(group);
            tl.set_program(group, program.map(str::to_string));
        };
        name(Some("claude"));
        store.write().unwrap().register(handle);
        (store, sid, name)
    }

    /// THE HOLD, READ FOR REAL ([`holds`], the production [`Acts::holds`]),
    /// over a registered tab on a real pseudo-terminal whose shell stays: its
    /// agent — a job of the shell's, leading the tab's foreground group,
    /// named in that group — holds the tab (`Some(true)`, the one answer
    /// that keeps it), a name the resolver could not read (`None`) and a
    /// runtime that may host an agent (`node`) included — and a worker's
    /// failed run during that misread finds its session still its own
    /// ([`still_wanted`], the live [`Hooks::still_wanted`]). NEGATIVE
    /// CONTROLS: another group than the one it was named in, a name read
    /// since that cannot host an agent (an `exec` into a shell), the agent's
    /// end, and a tab that ended hold nothing; no group named, and no such
    /// tab, cannot be read. The agent's end is read by the leader's conjunct
    /// ALONE, and the test says so: its shell does not take the terminal
    /// back, and the terminal goes on naming the dead group as its
    /// foreground — without that conjunct the end reads `Some(true)`.
    #[cfg(unix)]
    #[test]
    fn the_real_hold_reads_the_tabs_foreground_and_its_leader() {
        let (master, leader, group) = pty_job(JobEnd::Reaped);
        let (store, sid, name) = tab_on(master, 7101, group);
        assert_eq!(holds(&store, &sid, group), Some(true), "it holds its tab");
        assert_eq!(holds(&store, &sid, group + 1), Some(false), "another group");
        assert!(still_wanted(&store, &sid, group), "named");
        name(None);
        assert_eq!(holds(&store, &sid, group), Some(true), "a misread");
        assert!(
            still_wanted(&store, &sid, group),
            "a failed run during a misread"
        );
        assert!(
            !still_wanted(&store, &sid, group + 1),
            "not in the group it was named in"
        );
        name(Some("node"));
        assert_eq!(holds(&store, &sid, group), Some(true), "a runtime");
        name(Some("zsh"));
        assert_eq!(
            holds(&store, &sid, group),
            Some(false),
            "an exec into a shell"
        );
        assert!(!still_wanted(&store, &sid, group), "a shell's");
        name(Some("claude"));
        assert_eq!(holds(&store, &sid, 0), None, "no group named");
        assert_eq!(
            holds(&store, "s-00000000000000000000", group),
            None,
            "no such tab"
        );
        kill_until_reaped(group);
        assert_eq!(
            crate::quit_safety::foreground_pgrp(master),
            group,
            "the dead group still reads as the foreground: only the leader's conjunct reads its end"
        );
        assert_eq!(holds(&store, &sid, group), Some(false), "its end");
        store.write().unwrap().set_state(7101, SessionState::Exited);
        assert_eq!(holds(&store, &sid, group), Some(false), "the tab ended");
        end_pty_job(master, leader, group);
    }

    /// THE HOLD IS THE GROUP LEADER'S, AND NO EXACT LIVENESS ORACLE
    /// ([`holds`]), on a real pseudo-terminal whose shell stays: the agent —
    /// its group's leader — ended while a process of its group lives on (a
    /// runtime's child, a pipeline's other end) holds the tab no longer,
    /// though the group is still the terminal's foreground. A ZOMBIE leader
    /// — ended, not yet reaped by its parent — still reads as holding it
    /// (`kill(pid, 0)` finds a zombie): pinned here as the known imprecision
    /// [`holds`]'s doc states, which the next look ends once the shell reaps
    /// it and takes the terminal back, and which [`HOLDS_KEEP_MAX`] bounds
    /// if it never does. Nothing may rely on the hold as the agent's
    /// liveness.
    #[cfg(unix)]
    #[test]
    fn the_hold_reads_the_groups_leader_and_a_zombie_still_reads_held() {
        let (master, leader, group) = pty_job(JobEnd::Member);
        let (store, sid, _name) = tab_on(master, 7102, group);
        assert_eq!(holds(&store, &sid, group), Some(true), "it holds its tab");
        kill_until_reaped(group);
        // SAFETY: `killpg(.., 0)` only asks whether the group has a member.
        assert_eq!(unsafe { libc::killpg(group, 0) }, 0, "a member lives on");
        assert_eq!(
            crate::quit_safety::foreground_pgrp(master),
            group,
            "its group is still the foreground"
        );
        assert_eq!(holds(&store, &sid, group), Some(false), "its leader ended");
        end_pty_job(master, leader, group);

        let (master, leader, group) = pty_job(JobEnd::Zombie);
        let (store, sid, _name) = tab_on(master, 7103, group);
        assert_eq!(holds(&store, &sid, group), Some(true), "it holds its tab");
        // SAFETY: signals the test's own process.
        unsafe { libc::kill(group, libc::SIGKILL) };
        // Its parent never reaps it: a zombie, still found by `kill(pid, 0)`.
        until("the agent is a zombie", || is_zombie(group));
        assert_eq!(
            holds(&store, &sid, group),
            Some(true),
            "a zombie leader reads as holding the tab"
        );
        end_pty_job(master, leader, group);
    }

    /// THE UPGRADE'S OWN EXIT, THROUGH THE HOST (the gate failure of
    /// 32a51a716): the step ends the agent — the tab back at its shell, the
    /// agent holding it no longer — and the relaunch it makes waits; once the
    /// step is over the host hands the worker that exit, and under `[harness]
    /// relaunch = false` it is the harness's own restart's
    /// ([`Acts::restarted`], asked about the pid that left): carried, nothing
    /// said, and the new process carried on where its loop parks. NEGATIVE
    /// CONTROLS, each the agent's own exit — left for the limit and said: no
    /// restart in flight, and a restart in flight in the tab for ANOTHER
    /// process (a person's `claude` at the prompt an earlier relaunch waited
    /// on).
    #[test]
    fn the_upgrades_own_restart_is_carried_on_whatever_relaunch_says() {
        let mut limited = on();
        limited.set("relaunch", "false").unwrap();
        let run = |restarted: Option<u32>| {
            let a = Arc::new(Acting::default());
            *a.steps.lock().unwrap() = ["wait:shell-prompt"].into();
            a.due.store(true, Ordering::SeqCst);
            a.open.lock().unwrap().insert("s-u".to_string());
            let end = Arc::clone(&a);
            *a.during_step.lock().unwrap() = Some(Box::new(move || {
                // The SIGTERM: its record removed, its group gone from the tab.
                end.restarted_pids.lock().unwrap().extend(restarted);
                end.record_gone.store(true, Ordering::SeqCst);
                *end.holds.lock().unwrap() = Some(false);
                end.set(&[]);
            }));
            a.set(&[("s-u", Program::Claude)]);
            let host = HostHandle::start(limited.clone(), false, false, a.hooks());
            (a, host)
        };
        let said = |a: &Acting| {
            a.badges
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .any(|b| b.contains("not relaunched"))
        };
        let (a, host) = run(Some(snap("s-u").pid));
        until("carried", || a.carries.lock().unwrap().len() == 1);
        assert_eq!(*a.carries.lock().unwrap(), [(false, Some(snap("s-u").pid))]);
        assert_eq!(a.stepped.load(Ordering::SeqCst), 1);
        assert!(a.restarted_pids.lock().unwrap().is_empty(), "landed");
        assert!(
            a.relaunched.lock().unwrap().is_empty(),
            "never the relaunch on exit's"
        );
        assert!(!said(&a), "{:?}", a.badges.lock().unwrap());
        until("the relaunching worker reaped", || host.live().is_empty());
        a.set(&[("s-u", Program::Claude)]); // the relaunched agent
        until("attached again, and carried on", || {
            host.live() == ["s-u"] && a.carried.load(Ordering::SeqCst) == 1
        });
        assert!(!said(&a), "{:?}", a.badges.lock().unwrap());
        host.shutdown_and_join();
        // NEGATIVE CONTROLS: the agent's own exit.
        for restarted in [None, Some(snap("s-u").pid + 1)] {
            let (a, host) = run(restarted);
            until("said once", || said(&a));
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                a.relaunched.lock().unwrap().is_empty() && a.carries.lock().unwrap().is_empty(),
                "left: {restarted:?}"
            );
            host.shutdown_and_join();
        }
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

    /// A CODEX DAEMON CLIENT'S NAME is kept over a read that names none only
    /// while that read is no contradiction: a daemon restarted since (it
    /// holds the thread no more) keeps it; a read that could not tell the
    /// thread apart (`ambiguous`, its own thread begun beside the one named)
    /// drops it — the earlier name may be another client's. NEGATIVE
    /// CONTROLS: another process's name is never kept, and a read that names
    /// one wins.
    #[test]
    fn a_kept_codex_thread_name_goes_with_an_ambiguous_read() {
        let run = |ambiguous| aterm_agent::harness::relaunch::CodexRun {
            home: "/u/.codex".into(),
            embedded: false,
            started_ms: 1,
            ambiguous,
        };
        let read = |session: Option<&str>, ambiguous| Snapshot {
            session: session.map(str::to_string),
            codex: Some(run(ambiguous)),
            ..snap("s-c")
        };
        let before = read(Some("T-other"), false);
        let mut now = read(None, false);
        keep_session(Some(&before), &mut now);
        assert_eq!(now.session.as_deref(), Some("T-other"), "no contradiction");
        let mut now = read(None, true);
        keep_session(Some(&before), &mut now);
        assert_eq!(now.session, None, "an ambiguous read drops the name");
        // NEGATIVE CONTROLS.
        let mut other = Snapshot {
            pid: before.pid + 1,
            ..read(None, false)
        };
        keep_session(Some(&before), &mut other);
        assert_eq!(other.session, None, "another process's name");
        let mut named = read(Some("T-mine"), false);
        keep_session(Some(&before), &mut named);
        assert_eq!(named.session.as_deref(), Some("T-mine"));
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

    /// A relaunch that keeps failing names on its badge a cause only a person
    /// clears; any other step stays in the log.
    #[test]
    fn a_failing_relaunch_badge_names_the_cause_only_a_person_clears() {
        for (step, cause) in [
            ("wait:held", "the session is held"),
            ("wait:shell-prompt", "the shell prompt is not back"),
            (
                "wait:conversation-in-other-tab",
                "the conversation is open in another tab",
            ),
        ] {
            assert_eq!(
                failing_text(step, false),
                format!("the agent exited and its relaunch keeps failing (still trying: {cause})")
            );
        }
        // NEGATIVE CONTROL: a step the retry gets past alone stays in the log.
        assert_eq!(
            failing_text("wait:resume", false),
            "the agent exited and its relaunch keeps failing (still trying)"
        );
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
                badges[0].as_deref().is_some_and(
                    |b| b.contains("keeps failing (still trying)") && !b.contains("wait:")
                ),
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
                .is_some_and(|b| b.contains("cannot be relaunched; resume it by hand")
                    && !b.contains("refused:")),
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
        // Past it, not at it: the next try is a pause on, and a look that
        // came after it fails the count below by name instead of hanging.
        until("one miss", || a.relaunched.lock().unwrap().len() >= 7);
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

    /// A RELAUNCH ON EXIT DUE AT A SEAMLESS UPDATE IS CARRIED, OR TAKEN UP
    /// AGAIN (round six, F13). A crashed agent's relaunch keeps waiting on
    /// its back-off; a park comes: no relaunch is typed while it holds, and
    /// the handoff layout names the agent the tab is owed
    /// ([`HostHandle::pending_restored_for`]) for the successor to relaunch.
    /// The Commit's stop ends the worker; the Commit fails, and the owed
    /// relaunch is this host's again ([`HostHandle::relaunch_stranded`]).
    ///
    /// FAILS WITHOUT THE FIX: the back-off lived in the worker alone — the
    /// layout named nothing, the stop read as the agent being back, and
    /// nothing ever relaunched the conversation.
    #[test]
    fn a_relaunch_due_at_the_commit_is_carried_or_taken_up_again() {
        let a = Arc::new(Acting::default());
        *a.relaunches.lock().unwrap() = ["wait:shell-prompt"; 64].into();
        a.open.lock().unwrap().insert("s-a".to_string());
        a.set(&[("s-a", Program::Claude)]);
        let taken_up: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut h = a.hooks();
        h.acts.relaunch_restored = {
            let taken_up = Arc::clone(&taken_up);
            Arc::new(move |sid, snap, _| {
                assert_eq!(snap.tab, sid, "the snapshot the tab was owed");
                taken_up.lock().unwrap().push(sid.to_string());
                "adopted".to_string()
            })
        };
        let host = HostHandle::start(on(), false, false, h);
        until("attached", || host.live() == ["s-a"]);
        assert_eq!(host.pending_restored_for("s-a"), None, "owed nothing yet");
        // The agent crashes: its record survives, and the relaunch waits.
        a.set(&[]);
        until("a first try", || !a.relaunched.lock().unwrap().is_empty());
        assert!(
            host.pending_restored_for("s-a").is_some(),
            "owed on its back-off"
        );
        // The park: nothing typed while it holds, and the layout names it.
        host.pause_restored();
        std::thread::sleep(Duration::from_millis(50));
        let parked = a.relaunched.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            a.relaunched.lock().unwrap().len(),
            parked,
            "no try while parked"
        );
        let carried = host
            .pending_restored_for("s-a")
            .expect("the layout carries it");
        assert_eq!(carried.tab, "s-a");
        // The Commit's stop, then a failed Commit.
        host.suspend();
        until("the worker stopped", || host.live().is_empty());
        host.resume();
        host.resume_restored();
        host.relaunch_stranded(&|_| "in tab 1".to_string());
        until("taken up again", || !taken_up.lock().unwrap().is_empty());
        assert_eq!(*taken_up.lock().unwrap(), ["s-a"]);
        assert_eq!(host.pending_restored_for("s-a"), None, "owed no more");
        // Once: a second rollback finds nothing stranded.
        host.relaunch_stranded(&|_| "in tab 1".to_string());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(taken_up.lock().unwrap().len(), 1);
        host.shutdown_and_join();
    }

    /// A RELAUNCH OWED ONLY AFTER THE PARK IS CARRIED TOO (round six F13,
    /// review two): the agent crashes once the park has frozen the restored
    /// queue — the worker reads the exit after its detection and settle — and
    /// the handoff layout still names the tab, while nothing is typed.
    ///
    /// FAILS WITHOUT THE FIX: a frozen queue answered from its snapshot alone,
    /// so the layout named nothing and the Commit dropped the relaunch.
    #[test]
    fn a_relaunch_owed_after_the_park_is_carried() {
        let a = Arc::new(Acting::default());
        *a.relaunches.lock().unwrap() = ["wait:shell-prompt"; 64].into();
        a.open.lock().unwrap().insert("s-a".to_string());
        a.set(&[("s-a", Program::Claude)]);
        let host = HostHandle::start(on(), false, false, a.hooks());
        until("attached", || host.live() == ["s-a"]);
        // The park first: nothing is owed when it freezes the queue.
        host.pause_restored();
        assert_eq!(host.pending_restored_for("s-a"), None, "owed nothing yet");
        // Then the crash.
        a.set(&[]);
        until("owed after the park", || {
            host.pending_restored_for("s-a").is_some()
        });
        assert_eq!(
            host.pending_restored_for("s-a").map(|snap| snap.tab),
            Some("s-a".to_string())
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            a.relaunched.lock().unwrap().is_empty(),
            "no try while parked"
        );
        host.resume_restored();
        until("a try once resumed", || {
            !a.relaunched.lock().unwrap().is_empty()
        });
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
                still_wanted: Arc::new(move |sid, _| {
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
                upgrade_view: Arc::new(|_| ViewNext::default()),
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
        /// restart pause (still the session's supervisor) — and never one
        /// whose run ended held (the model's `Hold`: it ends); `old` a body
        /// asked to stop that still runs. Until a new worker's body
        /// registers, the newest flag seen is its predecessor's: after a
        /// release that is the held run's, never stopped, so read as `cur`
        /// it passed for the worker the release started before that body
        /// ran ("reload restarts" waits for that body).
        ///
        /// ONE snapshot, so a stop landing mid-read never tears it: the
        /// newest run's end is read under its lock, taken first as the body
        /// takes it (newest, then workers), and each stop flag is read ONCE —
        /// the newest's read twice projected a worker both current and asked
        /// to stop, `OneSupervisor` broken by a state the host never had
        /// (round 32: 1 run in 200 alone).
        fn project(
            &self,
            faults: usize,
            live: bool,
        ) -> std::collections::BTreeMap<&'static str, i64> {
            let wanted = i64::from(!self.roster.lock().unwrap().is_empty());
            let newest_run = self.newest.lock().unwrap();
            let running = self.running.lock().unwrap();
            let workers = self.workers.lock().unwrap();
            let held_end = newest_run.2;
            let newest = workers.last();
            let stopped = newest.is_some_and(|stop| stop.load(Ordering::SeqCst));
            let in_body = newest.is_some_and(|n| running.iter().any(|r| Arc::ptr_eq(r, n)));
            // Asked to stop: a body still running, or the newest worker on its
            // way out of its restart pause.
            let asked = |s: &Arc<AtomicBool>| {
                if newest.is_some_and(|n| Arc::ptr_eq(s, n)) {
                    stopped
                } else {
                    s.load(Ordering::SeqCst)
                }
            };
            let old = running.iter().filter(|s| asked(s)).count() as i64
                + i64::from(live && stopped && !in_body);
            // A worker whose run ended held is on its way out, never current:
            // read as current, it passed for the successor a release starts
            // before that one's body had run, and the next step's count of
            // the probe's workers missed it (round 32).
            let cur = i64::from(live && newest.is_some() && !stopped && !held_end);
            drop(workers);
            drop(running);
            drop(newest_run);
            let faults = faults as i64;
            // Held is the HOST's answer too: the run ended held and the host
            // reaped the worker without starting another (`!live`). Without
            // it this state is the probe's own flag, and a host that
            // restarted a held session at once passed (round 32 review).
            let held = i64::from(held_end && wanted == 1 && cur == 0 && !live);
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
            // A hang detector, as [`until`]'s: a 5 s deadline failed "fail
            // before a flap" under a loaded parallel run (2026-09-28).
            let deadline = Instant::now() + Duration::from_secs(60);
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
                // The released worker reads as `cur` from its start, before
                // its body ran ([`Probe::project`]): counted before that body
                // registers, `before` is one short, the successor makes it two
                // more, and the wait below never passes (a hang, 2026-09-28).
                // Its body is waited for first — not taken into the projection,
                // where a `held` read before the host reacts would let a host
                // that ignores the hold pass the "held" step.
                until("the released worker's body runs", || {
                    let held = probe.newest.lock().unwrap().2;
                    !held && probe.running.lock().unwrap().len() == 1
                });
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

    // -----------------------------------------------------------------------
    // Tier-1: the real host against `HarnessLeave`.
    // -----------------------------------------------------------------------

    /// What the `Acting` world shows of `HarnessLeave`'s variables: `alive`,
    /// `ours` and `stray` are what the driver did to it, `named` the roster,
    /// `handed` and `decided` what the worker made of an exit (relaunched:
    /// carried on; the limit said). `due` — the host's own bell and look — is
    /// not observable, and is taken from the model.
    fn leave_observed(
        a: &Acting,
        (alive, ours, stray): (i64, i64, i64),
        due: i64,
    ) -> aterm_spec::interp::State {
        let named = i64::from(!a.roster.lock().unwrap().is_empty());
        let relaunched =
            !a.relaunched.lock().unwrap().is_empty() || !a.carries.lock().unwrap().is_empty();
        let said = a
            .badges
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|b| b.contains("not relaunched"));
        let decided = if relaunched {
            1
        } else if said {
            2
        } else {
            0
        };
        [
            ("alive", alive),
            ("named", named),
            ("ours", ours),
            ("stray", stray),
            ("due", due),
            ("handed", i64::from(decided > 0)),
            ("decided", decided),
        ]
        .into_iter()
        .collect()
    }

    /// Drive the real host through every path of `HarnessLeave`'s
    /// environment — `Misread`, `Reread`, `Exit`, `Terminate`, `Stray`, to
    /// three actions or the agent's end — each path over a fresh `Acting`
    /// world under `[harness] relaunch = false`, the host taking its own
    /// actions (`Visit`, `Decide`) after each: what it did — the worker kept,
    /// the exit carried on (relaunched) or said — must be what the model
    /// does after the same actions and its own, `NoExitWhileAlive`,
    /// `TheUpgradesOwnIsCarried` and `OnlyTheUpgradesOwnIsCarried` holding
    /// on every state observed on the way and every invariant on the state
    /// it settles in. A misread is held until the host has decided it twice
    /// (the pass, and a later one), so "kept" is a decision that was made,
    /// never one that did not come; a misread agent's `Exit` rings nothing
    /// here, the model's conservative case (in the live timeline the shell's
    /// name resolving, or a runtime's name leaving, usually rings: the look
    /// is the backstop, [`keep_holding`]); and a `Stray` is a restart in
    /// flight in the tab for another process — [`Acts::restarted`] is asked
    /// about the leaving agent's pid (`relaunch::restarted`'s own filter —
    /// another process, another tab, the other agent's lane — is bound in
    /// aterm-agent by
    /// `the_host_finds_and_carries_the_restart_that_ended_the_agent_that_left`).
    /// NEGATIVE CONTROLS: from every misread the host of 32a51a716
    /// (`VisitUnconfirmed`) hands an exit the model's `NoExitWhileAlive`
    /// rejects — and the real host kept the worker; from every exit after a
    /// stray, the tab-scoped read (`DecideByTab`) carries on an exit
    /// `OnlyTheUpgradesOwnIsCarried` rejects — and the real host said it.
    ///
    /// WHAT THIS BIND DOES AND DOES NOT SHOW. `alive`, `ours`, `stray` and
    /// `named` are what the driver DID to the fake world, and `due` is taken
    /// from the model: only `handed` and `decided` are OBSERVED of the real
    /// host. The negative controls fire the `Buggy` actions on MODEL state
    /// alone — they show the model tells those hosts apart, not that this
    /// host would be caught if it became one of them. The hold here is the
    /// fake's flag, not the shipping [`holds`] or [`still_wanted`] (bound on
    /// a real terminal by `the_real_hold_reads_the_tabs_foreground_and_its_leader`
    /// and `the_hold_reads_the_groups_leader_and_a_zombie_still_reads_held`).
    /// And the look (`VisitNoLook`) is NOT caught here: another test's bell
    /// wakes this threaded host too, and a mutant with no look passes this
    /// bind in a parallel run — `a_pass_that_keeps_an_agent_owes_the_host_a_look`,
    /// whose host's passes are its own, is what catches it.
    #[test]
    fn the_real_host_conforms_to_the_leave_model() {
        use aterm_spec::interp::State;
        const ENV: [&str; 5] = ["Misread", "Reread", "Exit", "Terminate", "Stray"];
        let model = aterm_spec::derive::harness_leave_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        // The host's own actions, taken until nothing more moves.
        let settle = |s: &mut State| loop {
            let before = s.clone();
            for action in ["Visit", "Decide"] {
                let mut next = s.clone();
                if model.fire(action, &mut next) {
                    *s = next;
                }
            }
            if *s == before {
                break;
            }
        };
        let mut paths: Vec<Vec<&str>> = Vec::new();
        let mut init = model.init_state();
        settle(&mut init);
        let mut open = vec![(Vec::new(), init)];
        while let Some((path, state)) = open.pop() {
            let next: Vec<&str> = ENV
                .into_iter()
                .filter(|action| model.action_enabled(action, &state))
                .collect();
            if path.len() == 3 || next.is_empty() {
                paths.push(path);
                continue;
            }
            for action in next {
                let mut after = state.clone();
                assert!(model.fire(action, &mut after));
                settle(&mut after);
                let mut longer = path.clone();
                longer.push(action);
                open.push((longer, after));
            }
        }
        assert_eq!(paths.len(), 16, "{paths:?}");
        let mut limited = on();
        limited.set("relaunch", "false").unwrap();
        for path in &paths {
            let a = Arc::new(Acting::default());
            a.group.store(7, Ordering::SeqCst);
            *a.holds.lock().unwrap() = Some(true);
            a.open.lock().unwrap().insert("s-l".to_string());
            a.set(&[("s-l", Program::Claude)]);
            let host = HostHandle::start(limited.clone(), false, false, a.hooks());
            until("attached", || host.live() == ["s-l"]);
            let mut expect = model.init_state();
            settle(&mut expect);
            let (mut alive, mut ours, mut stray) = (1, 0, 0);
            let agent = snap("s-l").pid;
            for &action in path {
                let before = expect.clone();
                assert!(model.fire(action, &mut expect), "{path:?} {action}");
                settle(&mut expect);
                let asked = a.holds_asked.lock().unwrap().len();
                match action {
                    "Misread" => a.set(&[]),
                    "Reread" => a.set(&[("s-l", Program::Claude)]),
                    "Exit" => {
                        alive = 0;
                        *a.holds.lock().unwrap() = Some(false);
                        // A named agent's exit moves its name, which rings;
                        // a misread one's rings nothing.
                        if before["named"] == 1 {
                            a.set(&[]);
                        }
                    }
                    "Terminate" => {
                        (alive, ours) = (0, 1);
                        *a.steps.lock().unwrap() = ["wait:shell-prompt"].into();
                        let end = Arc::clone(&a);
                        *a.during_step.lock().unwrap() = Some(Box::new(move || {
                            end.restarted_pids.lock().unwrap().push(agent);
                            end.record_gone.store(true, Ordering::SeqCst);
                            *end.holds.lock().unwrap() = Some(false);
                            end.set(&[]);
                        }));
                        a.due.store(true, Ordering::SeqCst);
                        host.note_activation();
                    }
                    "Stray" => {
                        stray = 1;
                        a.restarted_pids.lock().unwrap().push(agent + 1);
                    }
                    _ => unreachable!("{action}"),
                }
                let kept = expect["alive"] == 1 && expect["named"] == 0;
                let deadline = Instant::now() + Duration::from_secs(5);
                let seen = loop {
                    let seen = leave_observed(&a, (alive, ours, stray), expect["due"]);
                    for inv in [
                        "NoExitWhileAlive",
                        "TheUpgradesOwnIsCarried",
                        "OnlyTheUpgradesOwnIsCarried",
                    ] {
                        assert!(
                            model.check_invariant(inv, &seen),
                            "{path:?} {action}: {inv} broken by {seen:?}"
                        );
                    }
                    if seen == expect && (!kept || a.holds_asked.lock().unwrap().len() >= asked + 2)
                    {
                        break seen;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "{path:?} {action}: observed {seen:?}, model {expect:?}"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                };
                for inv in &model.invariants {
                    assert!(
                        model.check_invariant(inv.name, &seen),
                        "{path:?} {action}: {} broken by {seen:?}",
                        inv.name
                    );
                }
                if action == "Misread" {
                    let mut old = before.clone();
                    assert!(buggy.fire("Misread", &mut old));
                    assert!(buggy.fire("VisitUnconfirmed", &mut old), "{old:?}");
                    assert!(!model.check_invariant("NoExitWhileAlive", &old), "{old:?}");
                    assert_eq!((seen["handed"], host.live()), (0, vec!["s-l".to_string()]));
                }
                if action == "Exit" && before["stray"] == 1 {
                    let mut old = before.clone();
                    for step in ["Exit", "Visit", "DecideByTab"] {
                        assert!(buggy.fire(step, &mut old), "{step}: {old:?}");
                    }
                    assert!(
                        !model.check_invariant("OnlyTheUpgradesOwnIsCarried", &old),
                        "{old:?}"
                    );
                    assert_eq!(seen["decided"], 2, "said: {seen:?}");
                }
            }
            host.shutdown_and_join();
        }
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
                still_wanted: Arc::new(|_, _| true),
                body: Arc::new(|_| BodyEnd::Stopped),
                badge: Arc::new(|_, _| {}),
                claim_epoch: Arc::new(|| 0),
                acts,
                backoff: Arc::new(quick_backoff),
                pause: Arc::new(quick_pause),
                upgrade_view: Arc::new(|_| ViewNext::default()),
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
                group: Arc::default(),
            };
            attach(&job, &hooks);
            WorkerIdle {
                sid: job.sid,
                agent: job.agent,
                grace: 120,
                park: job.park,
                look_at: job.look_at,
                acting: job.acting,
                stepped: Arc::default(),
                stalled: job.stalled,
                switches: job.switches,
                kept: job.kept,
                hooks,
                run: Mutex::default(),
                activations: Arc::default(),
                owns: AtomicBool::new(false),
                background_at: Mutex::default(),
                tasked: Mutex::default(),
                note: job.note,
                clock_hold: Mutex::default(),
                net: NetProbe::new(),
                route: Mutex::default(),
                points: Arc::default(),
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
