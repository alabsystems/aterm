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
//! program is read ([`agent_of`]). The relaunch on exit and the upgrade
//! below are both agents': Codex's by its own lane of the same step, at the
//! same idle points.
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
//! as a crash of aterm (the process keeps running). The HOST thread's own
//! loop is restarted the same way, in place and for ever, with what it held:
//! its workers run on through its back-off, held to the policy as it is
//! ([`restart_under_policy`]), each one's agent's exit watched meanwhile, and
//! its first pass back hands any exit it finds ([`host_restarted`], round
//! seventeen); news a pass must act on (the policy changed, a Commit failed:
//! [`State::news`]) cuts the back-off short. A panic in that restart is
//! caught as the run's is, and loses nothing the workers held
//! ([`restart_host`], round eighteen).
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
//! with the explicit restart's cause where its act ended the agent (round
//! eighteen, [`RestoredAgent::asked`]), and the successor's Commit carries on
//! every restart record left in flight ([`HostHandle::resume_after_handoff`])
//! and watches every agent the outgoing host supervised where nothing was
//! owed ([`HostHandle::watch_handed`], round eighteen): an exit the outgoing
//! instance read during its Commit, and will never act on, is the
//! successor's.
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
//!   "graceful" (D2 of the 2026-09-26 live test). An exit no pass sees as it
//!   comes — the worker ended leaving it to the host while a Commit holds it,
//!   or the host thread is down after a panic — is read the same way, at the
//!   kernel's word of the agent's end, by the exit watch, and that read is
//!   the one the worker handed it later takes ([`Kept::exit_watch`], round
//!   seventeen). Any other is relaunched on its
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
//! * THE ACCOUNT'S WALL (the accounts design, §5 and §12 slice 2). Where the
//!   loop opens a limit episode on a Claude Code ([`IdleHost::limited`]), its
//!   worker hands the session's own usage wall to the accounts recorder
//!   ([`Acts::account_wall`]): recorded for the account the agent's own
//!   environment names, where the owner listed one and neither its
//!   environment nor its settings give it another way to sign in
//!   ([`WorkerIdle::settled`]) — with no roster nothing of the agent is read,
//!   but under `[harness] usage_notes`, where the machine's own row stands
//!   for it — and a session that is over is forgotten
//!   ([`Acts::account_gone`]). It records and nothing more: nothing is typed,
//!   no session is touched, nothing is shown. It is under no switch
//!   (`[harness] accounts = "off"` leaves it recording), it waits for the
//!   store's lock a bounded time, and a row that lags the screen is asked for
//!   again at the session's next decisions ([`WorkerIdle::account_wall_again`])
//!   — which the worker asks the loop to make soon, a few times, by advising a
//!   wait a moment away ([`WorkerIdle::account_lags`]; never with no roster,
//!   never under `off`).
//! * THE ACCOUNT'S MOVE (the accounts design, §6; `[harness] accounts`).
//!   Where the owner listed accounts, the loop asks its worker at a turn end
//!   what the roster advises ([`IdleHost::account`]: the roster and the state
//!   read WITHOUT the store's lock, `accounts::advise`), and where its policy
//!   decides on the move ([`Restart::Account`]) the worker makes it
//!   ([`WorkerIdle::account_move`]): the move committed in one transaction
//!   under the store's lock (one session moves alone; the others follow once
//!   it has served a turn), then the restart in place with the roster's
//!   login on its line, the new process's own environment checked
//!   afterwards — and a relaunch that was not made taken back exactly. Only
//!   a final answer is `refused:` or `failed:`; whatever can pass by itself
//!   is a `wait:`. A relaunch left in
//!   flight, and one whose new process could not be read, stand: where the
//!   host's carry lands one — or a later read finds it — it is a made move,
//!   and its loop learns of it: written for the loop that takes the agent,
//!   or told to the one still running ([`account_land`], [`Landing`]). A tab a
//!   reopened layout brought back is relaunched under the login and the
//!   Claude home its leaf names (read off the agent's process while it ran),
//!   and — its leaf naming neither — under the machine's proven active
//!   account ([`Acts::account_cold`], [`RestoredAgent::unread`]).
//!   Nothing of it is done where nothing of the accounts acts
//!   ([`Switches::accounts_live`], [`WorkerIdle::accounts_act`], read live:
//!   `accounts = "off"` with `logins = "off"`, or `off` over a roster with no
//!   row a renewal restores); under `accounts = "off"` the one move left is
//!   the login lifecycle's follow onto a renewed login. What answers the move
//!   is told to the store ([`IdleHost::account_served`],
//!   [`IdleHost::account_answered`]),
//!   and news of the accounts from any process — the owner's `accounts use`,
//!   a login found signed in, an account proven or struck — wakes a session
//!   waiting at its limit ([`IdleHost::account_stamp`], the store's wake
//!   marker on the activation wake's kqueue). The roster is asked under the
//!   mode the owner wrote: under `early` a session on an account to LEAVE —
//!   a limit another session recorded on it, or a `/usage` sample that shows
//!   it low — is advised the move AHEAD of its own limit, at a plain turn
//!   end; under `wall` it is not. With no roster nothing of
//!   an agent is read, nothing is made on disk and no decision changes.
//!   NEVER RUN AGAINST TWO REAL ACCOUNTS: stand-ins, a temp roster and store.
//! * THE ACCOUNT'S `/usage` PROBE (the accounts design, §5 "The probe";
//!   `docs/SPEC-claude-accounts-probe-2026-10-01.md`; `early` only). Where
//!   the roster says the account's read is due, the advice handed to the
//!   policy says so WITH what the roster advises past it
//!   ([`AccountAdvice::Probe`], `accounts::advise_past_probe`: a due probe
//!   never masks a move, and never refuses one at its commit). The host's
//!   two parts of the probe itself are THE CLAIM ([`IdleHost::account_probe`],
//!   [`WorkerIdle::probe_claim`]: one probe per account per machine, under
//!   the store's lock) and THE SAMPLE ([`IdleHost::account_probed`],
//!   [`WorkerIdle::probe_record`]: recorded only for the process the claim
//!   was made for). Typing `/usage`, reading the panel and closing it are
//!   the loop's (`Session::usage_probe`); the host types nothing.
//! * THE ACCOUNT'S LIMIT RESET (the accounts design, §7;
//!   `docs/SPEC-claude-accounts-resets-2026-10-02.md`; `[harness]
//!   limit_reset`, which ships OFF and under which, off, nothing here is
//!   read, made or locked). The keys are the loop's; the host's parts are
//!   THE CLAIM ([`IdleHost::reset_take`], [`WorkerIdle::reset_claim`]: the
//!   default login and no other, this process's own wall, a reset that
//!   qualifies, and ONE attempt per wall on the machine, under the store's
//!   lock), THE OUTCOME ([`IdleHost::reset_told`], [`WorkerIdle::reset_tell`]:
//!   the dialog's one answer line read from the TRANSCRIPT since the claim,
//!   recorded only for the process the claim was made for) and THE HOLD
//!   ([`account_resetting`]: nobody is advised off an account whose reset is
//!   in flight). It needs no roster: where none lists an account the
//!   machine's own login is the one row ([`account_roster_or_own`]). NEVER
//!   RUN AGAINST A REAL WALL: no wall with a reset on offer has occurred, and
//!   every text of the reset is the 2.1.287 bundle's.
//! * THE ACCOUNTS AS A WHOLE, SAID TO THE WINDOW ([`HostNote::Accounts`],
//!   [`AccountsSaid`]: the digest's two `harness.accounts` lines — the
//!   accounts that are signed out, or the roster that is refused). One look
//!   ([`accounts_said`]), told once per change ([`accounts_tell`]) at the
//!   host's start, at news of the accounts and at a session's advice — and
//!   at the activation thread's every wake ([`activation_loop`]: its ten
//!   minute fallback at the least), where a line that stands is said again,
//!   so the digest's own rules restate it and end its dismissal. A line of
//!   the digest, never a row or a mark on a tab. With no roster it says
//!   there is nothing, and under `off` it says so with no file read; a
//!   headless instance that supervises nothing says nothing at all
//!   ([`Switches::mute`]).
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
//! the host wakes at it — by the wall clock too, so an instant slept through
//! is past at the wake ([`bell_wait`]; a record no look has read past its
//! wake grace reads `unwatched`, design record C10) — and hands the tab to
//! [`Acts::watch`] on a thread of its own at Background QoS
//! ([`watch_upgrades`]), as it does at a worker's start and at an activation
//! notice. The watch reads through a DRY-RUN visit
//! and writes only under the sweep lock's `try_lock`: its own fields, the
//! fresh wait word, a retarget, a limit's clock hold, a re-arm. It has no
//! hands — it never types, presses, pastes, signals, kills, terminates or
//! ends anything, past any budget (the aterm-census guard `watch-hands`) — and a worker
//! whose upgrade reads due but asks for no point is parked again (the
//! re-park, [`watch_tab`]), so the loop's next point takes the step.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_agent::harness::accounts;
use aterm_agent::harness::netwatch::Route;
use aterm_agent::harness::relaunch::{
    self, CodexRun, ExitLook, ExitRecord, Foreground, OnExit, Outcome, Relaunches, Restart, Say,
    Snapshot,
};
use aterm_agent::harness::restart_stamp;
use aterm_agent::harness::resume;
use aterm_agent::harness::self_claimed;
use aterm_agent::harness::tap;
use aterm_agent::harness::upgrade;
use aterm_agent::harness::upgrade_drive::{self, After, Behind, Due};
use aterm_agent::harness::upgrade_wake::{ActivationWake, WakeTrigger};
use aterm_agent::supervise::memory::AgentMemory;
use aterm_agent::supervise::policy::turn_end::{
    AccountAdvice, AccountAt, AccountBad, AccountPoint, Reach,
};
use aterm_agent::supervise::policy::{
    ResetAsk, ResetEnd, ResetGo, ResetNo, ResetOutcome, ResetWord,
};
use aterm_agent::supervise::{
    Autopilot, Ctl, CtlReply, Endpoint, Guard, HostStep, IdleHost, Interrupter, ProbeMiss,
    RelayCtl, Session, SuperviseOpts, SupervisorConfig, UsageSeen,
};

use crate::harness_netprobe::NetProbe;

use aterm_phase::Program;

use crate::session_store::{SessionState, Store};

/// `cfg` with every key the engine does not read at its default: what the
/// host runs under and compares, so an edit that changes nothing the engine
/// reads restarts nothing. Those are the three `[harness]` keys the HOST
/// reads and the loop does not: `upgrade`, `relaunch`, `probe_api` and the
/// per-agent admission list — read
/// from the same parse before they are masked ([`Switches`], live in every
/// worker), so the table has one parser. And `accounts` IN PART: the loop's
/// policy reads only whether it is `off` (`policy::turn_end`'s
/// `account_rotates`), the host which of `early` and `wall` it is
/// ([`Switches::accounts`]) — so every word but `off` is the default here,
/// and an edit between `early` and `wall` restarts nothing.
pub(crate) fn effective(cfg: &SupervisorConfig) -> SupervisorConfig {
    let d = SupervisorConfig::default();
    SupervisorConfig {
        upgrade: d.upgrade,
        relaunch: d.relaunch,
        probe_api: d.probe_api,
        off: d.off,
        accounts: match cfg.accounts {
            accounts::Mode::Off => accounts::Mode::Off,
            accounts::Mode::Early | accounts::Mode::Wall => d.accounts,
        },
        ..cfg.clone()
    }
}

/// `[harness] upgrade_ask` as the window last parsed it ([`Switches::set`]):
/// the live upgrade's notice-and-READY flow (`true`), or — the default since
/// 2026-09-29 — nothing typed before a move at a genuinely idle point. Read
/// live by every worker's step, as the switches are.
static UPGRADE_ASK: AtomicBool = AtomicBool::new(false);

/// `[harness] usage_tap` as the window last parsed it ([`Switches::set`]),
/// under the master switch: every worker's relaunch line carries the usage
/// tap where its gate holds (`upgrade_drive::Opts::usage_tap`). Read live:
/// an edit lands at the next line typed. Independent of `[harness]
/// accounts`: the tap is a recorder, and moves no session.
static USAGE_TAP: AtomicBool = AtomicBool::new(false);

/// The log's word for a usage tap's reading, or silence, that is recorded
/// for nobody ([`WorkerIdle::tap_label`]).
const TAP_NOT_OWN: &str = "reading not this machine's own login's";

/// The agents switched off by their own table (`[harness.<agent>] enabled =
/// false`, `SupervisorConfig::off`) as the window last parsed it
/// ([`Switches::set`]): every worker's upgrade step and the carry after an
/// update leave those agents' lanes and restarts in flight alone
/// (`upgrade_drive::Opts::agents_off`). Read live, as the switches are.
static AGENTS_OFF: Mutex<Vec<Program>> = Mutex::new(Vec::new());

/// [`UPGRADE_ASK`], read now: the flow an upgrade row is read under
/// (`upgrade_drive::Row::quiet`) wherever the window reads one outside the
/// host — the row a pressed word answers with.
pub(crate) fn upgrade_ask() -> bool {
    UPGRADE_ASK.load(Ordering::SeqCst)
}

/// Whether `cfg` supervises this instance's agent sessions: `[harness]
/// enabled`, and — in a headless instance — not `headless = false`. The
/// host's own gate ([`Shared::active`]) and the launch's word when a window
/// has no control socket to run a host on.
pub(crate) fn would_supervise(cfg: &SupervisorConfig, headless: bool) -> bool {
    cfg.enabled && (!headless || cfg.headless)
}

/// The host's own `[harness]` switches — the master switch and each act's —
/// as the window last parsed them, read live by every worker: switching one
/// off lands at the worker's next act, with no restart.
#[derive(Default)]
pub(crate) struct Switches {
    /// Agent admission is live, separately from the loop policy: narrowing
    /// one agent neither restarts another nor forgives its fault history.
    agents: AtomicU8,
    upgrade: AtomicBool,
    relaunch: AtomicBool,
    /// The policy supervises this instance at all ([`would_supervise`]):
    /// the explicit restart's relaunches the restored queue carries are
    /// taken under it whatever `relaunch` says ([`restored_allowed`]).
    supervises: AtomicBool,
    probe_api: AtomicBool,
    /// `[harness] upgrade_ask` for this host's workers: the flow their
    /// upgrade's steps are read under where the host decides by it — which
    /// of a step's words own the session's turn ends
    /// ([`upgrade_drive::owns_turn_ends_as`]). [`UPGRADE_ASK`] carries the
    /// same word to what reads it outside the host.
    ask: AtomicBool,
    /// A seamless update's park holds the terminal
    /// ([`HostHandle::pause_restored`] until [`HostHandle::resume_restored`]):
    /// no relaunch on exit is typed meanwhile, and the one a worker's back-off
    /// still owes is the handoff layout's to carry ([`Kept::owed`]).
    parked: AtomicBool,
    /// `[harness] accounts` as the window last parsed it, as a rank
    /// ([`ACCOUNTS_OFF`], [`ACCOUNTS_WALL`], [`ACCOUNTS_EARLY`]): the mode a
    /// worker's advice about its session's account is asked under
    /// ([`WorkerIdle::account`]) and the host's own look is made under
    /// ([`accounts_look`]). Off until [`Self::set`], wherever the policy
    /// supervises nothing in this instance ([`would_supervise`]): `[harness]
    /// enabled = false`, or a headless instance under `headless = false` —
    /// and wherever Claude Code, whose logins they are ([`accounts_for`]), is
    /// switched off by its own table (`[harness.claude] enabled = false`).
    accounts: AtomicU8,
    /// A HOST THAT SAYS NOTHING OF THE ACCOUNTS AS A WHOLE, not even that
    /// there is nothing ([`accounts_look`], [`HostHandle::accounts_switched`]):
    /// a headless instance whose policy is on and leaves its sessions to the
    /// window (`headless = false`). It adopts no line of the per-user log, so
    /// its `Nothing` would end the window's line or forget what the owner
    /// filed of it while the cause stands. Under `enabled = false` the
    /// owner's switch ends both lines everywhere, and it is said.
    mute: AtomicBool,
    /// `[harness] limit_reset` as the window last parsed it, and `false`
    /// with `[harness] enabled = false`: whether a worker's host answers for
    /// a limit reset at all ([`WorkerIdle::reset_claim`],
    /// [`WorkerIdle::reset_tell`]) and holds a move off an account whose
    /// reset is in flight ([`account_resetting`]). Off until [`Self::set`].
    limit_reset: AtomicBool,
    /// `[harness] usage_tap` under the master switch, for this host's
    /// workers: whether a silent tap is said to the window
    /// ([`WorkerIdle::usage_tap`]). [`USAGE_TAP`] carries the same word to
    /// the line builder, which reads it outside the host.
    usage_tap: AtomicBool,
    /// `[harness] usage_notes` as the window last parsed it, and `false` where
    /// Claude Code is not supervised here, as for the other account switches:
    /// with no account listed, whether a worker's host records the one
    /// account's usage under the machine's own row ([`WorkerIdle::usage_seen`])
    /// and the host's look reads it ([`accounts_look`]). Off until
    /// [`Self::set`].
    usage_notes: AtomicBool,
    /// `[harness] logins` as the window last parsed it, and `false` wherever
    /// the policy supervises nothing here or Claude Code is switched off, as
    /// for [`Self::accounts`]: whether a session on a renewable
    /// login (a roster row with an email) is waited at when it ends, told of
    /// its renewal and relaunched under it ([`WorkerIdle::login_label`],
    /// [`Self::accounts_live`]). Apart from [`Self::accounts`]: the
    /// rotation's off keeps renewal working. Off until [`Self::set`].
    logins: AtomicBool,
    /// NEWS OF THE ACCOUNTS, counted ([`HostHandle::note_accounts`]: the
    /// store's wake marker was replaced — by the owner's `accounts use`, a
    /// check, a session of any window proving or striking an account, a
    /// limit reset's outcome): the
    /// stamp every worker's loop reads at each step of a wait
    /// ([`IdleHost::account_stamp`]). Not a switch; kept here because this
    /// is what every worker of the host shares, live.
    accounts_news: AtomicU64,
    /// WHAT THE WINDOW WAS LAST TOLD OF THE ACCOUNTS AS A WHOLE
    /// ([`accounts_tell`]): the hash of that [`AccountsSaid`], 0 for none
    /// yet — or none the window is known to hold
    /// ([`HostHandle::accounts_again`]). Not a switch either: every teller of
    /// the host — its start, the news, each worker's advice — tells against
    /// this one word.
    accounts_said: AtomicU64,
    /// `[harness] autopilot` as the window last parsed it — THE DEFAULT
    /// AUTOPILOT of a session with no word of its own ([`OffSessions`]) — as
    /// [`autopilot_rank`] spells it: `0` (the [`Default`]) is `cruise`. Read
    /// live where the host's state lock is not held: the restored queue and
    /// the in-flight carry, which keep an Off session's hands off too.
    autopilot: AtomicU8,
}

/// [`Switches::autopilot`]'s encoding of a mode: `cruise` 0 (an atomic's
/// default, the table's), `land` 1, `off` 2.
const fn autopilot_rank(mode: Autopilot) -> u8 {
    match mode {
        Autopilot::Cruise => 0,
        Autopilot::Land => 1,
        Autopilot::Off => 2,
    }
}

/// [`Switches::accounts`]: `[harness] accounts = "off"`, or no policy.
const ACCOUNTS_OFF: u8 = 0;
/// [`Switches::accounts`]: `"wall"`.
const ACCOUNTS_WALL: u8 = 1;
/// [`Switches::accounts`]: `"early"`.
const ACCOUNTS_EARLY: u8 = 2;

impl Switches {
    fn park(&self, parked: bool) {
        self.parked.store(parked, Ordering::SeqCst);
    }

    fn parked(&self) -> bool {
        self.parked.load(Ordering::SeqCst)
    }

    /// `cfg`'s switches in this instance, a headless one where `headless`.
    fn set(&self, cfg: &SupervisorConfig, headless: bool) {
        self.agents.store(admitted_mask(cfg), Ordering::SeqCst);
        self.upgrade
            .store(cfg.enabled && cfg.upgrade, Ordering::SeqCst);
        self.relaunch
            .store(cfg.enabled && cfg.relaunch, Ordering::SeqCst);
        self.supervises
            .store(would_supervise(cfg, headless), Ordering::SeqCst);
        self.probe_api
            .store(cfg.enabled && cfg.probe_api, Ordering::SeqCst);
        self.ask.store(cfg.upgrade_ask, Ordering::SeqCst);
        UPGRADE_ASK.store(cfg.upgrade_ask, Ordering::SeqCst);
        *AGENTS_OFF.lock().unwrap_or_else(PoisonError::into_inner) = cfg.off.clone();
        // The accounts are Claude Code's logins ([`accounts_for`]): with its
        // sessions unsupervised — the policy off here, or its own table
        // switching it off — nothing of them is read, moved or renewed.
        let accounts_on = would_supervise(cfg, headless) && cfg.on_for(Program::Claude);
        let accounts = match cfg.accounts {
            _ if !accounts_on => ACCOUNTS_OFF,
            accounts::Mode::Off => ACCOUNTS_OFF,
            accounts::Mode::Wall => ACCOUNTS_WALL,
            accounts::Mode::Early => ACCOUNTS_EARLY,
        };
        self.accounts.store(accounts, Ordering::SeqCst);
        self.mute.store(
            cfg.enabled && !would_supervise(cfg, headless),
            Ordering::SeqCst,
        );
        self.limit_reset
            .store(cfg.enabled && cfg.limit_reset, Ordering::SeqCst);
        self.usage_tap
            .store(cfg.enabled && cfg.usage_tap, Ordering::SeqCst);
        USAGE_TAP.store(cfg.enabled && cfg.usage_tap, Ordering::SeqCst);
        self.usage_notes
            .store(accounts_on && cfg.usage_notes, Ordering::SeqCst);
        self.autopilot
            .store(autopilot_rank(cfg.autopilot), Ordering::SeqCst);
        self.logins
            .store(accounts_on && cfg.logins, Ordering::SeqCst);
    }

    /// `[harness] usage_notes`, read live ([`Self::usage_notes`]).
    fn notes(&self) -> bool {
        self.usage_notes.load(Ordering::SeqCst)
    }

    /// `[harness] autopilot`, the default Autopilot, read live
    /// ([`Self::autopilot`]).
    fn autopilot(&self) -> Autopilot {
        match self.autopilot.load(Ordering::SeqCst) {
            1 => Autopilot::Land,
            2 => Autopilot::Off,
            _ => Autopilot::Cruise,
        }
    }

    /// `[harness] logins`, read live ([`Self::logins`]).
    fn logins(&self) -> bool {
        self.logins.load(Ordering::SeqCst)
    }

    /// Whether anything of the accounts acts here: the rotation
    /// ([`Self::accounts`] not off) or the login lifecycle ([`Self::logins`]).
    /// The gate of everything both share — the advice, the move's commit,
    /// the tells of what answered it.
    fn accounts_live(&self) -> bool {
        self.accounts() != accounts::Mode::Off || self.logins()
    }

    /// `[harness] limit_reset`, read live ([`Self::limit_reset`]). Named
    /// apart from [`Acts::limit_reset`], which is the reset instant a
    /// transcript names.
    fn reset_on(&self) -> bool {
        self.limit_reset.load(Ordering::SeqCst)
    }

    /// `[harness] accounts`, read live ([`Self::accounts`]).
    fn accounts(&self) -> accounts::Mode {
        match self.accounts.load(Ordering::SeqCst) {
            ACCOUNTS_EARLY => accounts::Mode::Early,
            ACCOUNTS_WALL => accounts::Mode::Wall,
            _ => accounts::Mode::Off,
        }
    }

    /// [`Self::mute`].
    fn mute(&self) -> bool {
        self.mute.load(Ordering::SeqCst)
    }

    fn probe_api(&self) -> bool {
        self.probe_api.load(Ordering::SeqCst)
    }

    /// [`Self::ask`]: the ask-and-drain flow (`true`), or the quiet one.
    fn ask(&self) -> bool {
        self.ask.load(Ordering::SeqCst)
    }

    fn upgrade(&self) -> bool {
        self.upgrade.load(Ordering::SeqCst)
    }

    fn relaunch(&self) -> bool {
        self.relaunch.load(Ordering::SeqCst)
    }

    /// [`Self::supervises`].
    fn supervises(&self) -> bool {
        self.supervises.load(Ordering::SeqCst)
    }

    fn on_for(&self, agent: Program) -> bool {
        self.admission().admits(agent)
    }

    fn admission(&self) -> Admission {
        Admission(self.agents.load(Ordering::SeqCst))
    }
}

/// Which agents the policy's switches admit, as one copyable mask
/// ([`Switches::on_for`], [`HostHandle::admission`]): every agent under the
/// master switch but those their own tables switch off
/// ([`SupervisorConfig::on_for`], agent by agent). It starts from every bit
/// and clears the switched-off agents', so a new [`Program`] is admitted
/// with no list of agents to extend here ([`agent_bit`] is its place).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Admission(u8);

impl Admission {
    pub(crate) fn of(cfg: &SupervisorConfig) -> Self {
        let off = cfg
            .off
            .iter()
            .fold(0, |mask, agent| mask | agent_bit(*agent));
        Self(if cfg.enabled { !off } else { 0 })
    }

    pub(crate) fn admits(self, agent: Program) -> bool {
        self.0 & agent_bit(agent) != 0
    }
}

/// The policy admission snapshot shared by the live switches and their UI
/// projection (`autopilot::HarnessPosture::Restricted`): [`Admission::of`]'s
/// mask. No session word or supervisor claim can lift this gate.
pub(crate) fn admitted_mask(cfg: &SupervisorConfig) -> u8 {
    Admission::of(cfg).0
}

/// `agent`'s bit of an [`Admission`] mask: one per [`Program`], by its
/// place in the enum, so no two agents can share one. The mask lives in
/// memory alone (the switches, the Autopilot posture), never on disk.
pub(crate) fn agent_bit(agent: Program) -> u8 {
    const _: () = assert!(
        Program::ALL.len() <= u8::BITS as usize,
        "Admission's u8 is full"
    );
    1 << agent as u8
}

/// Restarts per session per [`RESTART_WINDOW`] on the short pauses; past
/// it the pauses grow to an hour and the window is told (a record, and a
/// line of the digest after an hour) while it waits
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
    /// THE SNAPSHOT IS THE LAYOUT'S: no process was read for it here. What
    /// its leaf carried of the agent's environment — the Claude home and the
    /// login the host read off its process while it ran
    /// (`restore::AgentRestore::home`, `::login`) — rides the snapshot and is
    /// re-applied as a live process's is; a leaf naming neither (an older
    /// build's, or an agent that had neither variable) says nothing of its
    /// login, and its relaunch carries the machine's proven active account
    /// where the owner listed two or more ([`Self::unread`],
    /// [`Acts::account_cold`]; the accounts design, §6). `false`: read from a
    /// live process (a relaunch on exit a failed Commit left stranded), whose
    /// own login it re-applies as ever.
    pub(crate) cold: bool,
    /// THE EXPLICIT RESTART ENDED IT (round eighteen; ruling 20's (iv)): the
    /// act's cause, as the worker that owed its relaunch read it
    /// ([`OwedRelaunch::asked`]) — carried by the handoff layout's leaf
    /// (`restore::AgentRestore::cause`) and by a failed Commit's rollback,
    /// each the very tab, its sid kept. Never by a cold restore: a durable
    /// layout carries no cause (`App::pending_restored_agent`), and a
    /// reopened tab's relaunch is aterm's own end.
    /// Its relaunch is the act's ([`Acts::relaunch`] with the cause, as the
    /// live worker makes it), carried through a hand its line waits on and
    /// asked the claim at every look, as the live worker's is
    /// ([`restored_carried`]): never a counted try while a hand stands.
    /// `None`: an exit of its own, or aterm's end.
    pub(crate) asked: Option<restart_stamp::Cause>,
}

impl RestoredAgent {
    /// NOTHING SAYS WHICH LOGIN THIS AGENT RAN UNDER: its snapshot is the
    /// layout's ([`Self::cold`]) and names neither a Claude home nor a login
    /// of its own. Only then does the roster's proven active account ride
    /// its relaunch ([`Acts::account_cold`]): a login the leaf names is the
    /// one its process had, re-applied as every live relaunch re-applies it
    /// (the accounts design: "a login the process already had is
    /// re-applied"), and a home it names is not the default one, whose
    /// accounts the roster places — and carrying the roster's login there
    /// would sign it in by another keychain item than its home's.
    fn unread(&self) -> bool {
        self.cold && self.snap.home.is_none() && self.snap.login.is_none()
    }
}

/// One cold-restore relaunch's next eligible step. A slow tab must not make
/// every other restored tab wait through its own growing retry pauses.
struct RestoredAttempt {
    agent: RestoredAgent,
    /// Revocation survives removal from the queue while a landing read runs.
    revoked: Arc<AtomicBool>,
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
    /// AN AGENT RAN IN THIS TAB while the relaunch was queued: the host's
    /// roster named one there ([`note_agents_in_restored_tabs`]). A FACT,
    /// never unset (round twelve) — see [`restored_overtaken`].
    agent_seen: bool,
    /// AN ASKED EXIT'S LOOK FOUND A HAND ON THE TAB and said so (round
    /// eighteen; [`restored_carried`]): said once, however many looks.
    carried: bool,
    /// ITS STEP LANDED AN AGENT THAT HOLDS ITS TAB AND THAT NO READ HAS
    /// REACHED YET (round eighteen, the review of its first cut;
    /// [`restored_landing_look`]): the attempt stays queued, the holder of
    /// that agent's exit, as a worker stays its landing's ([`read_landed`])
    /// — looked at again every [`LANDED_LOOK`], uncounted, until a read
    /// reaches the agent (watched until a pass names it: over), a pass names
    /// it (its own worker's: over), or it is gone before any read — its own
    /// relaunch's exit again, stepped again on the attempt's back-off and
    /// within its budget ([`restored_landing_ends`]). Its line typed, the
    /// agent in its tab its own: never overtaken meanwhile
    /// ([`Self::typed`]), so a park carries it and a quit keeps it.
    landed: bool,
}

impl RestoredAttempt {
    /// Whether this relaunch has typed its line: a step of it said the
    /// relaunched agent is still to take its conversation up
    /// (`wait:resume`). An agent in its tab then may be its own.
    ///
    /// This process's fact alone (the review of round twelve): a handoff
    /// carries the snapshot, and the successor's attempt starts untyped — an
    /// agent its roster names in the tab, the one this line started,
    /// overtakes it there, and its queue takes it off with no row. What was
    /// typed is the conversation's record in flight, which the successor's
    /// Commit sweeps on every tab it adopted
    /// ([`HostHandle::resume_after_handoff`]): carried on by that agent's
    /// worker, or stepped and said there.
    ///
    /// AND ONE WHOSE STEP LANDED AN AGENT NO READ HAS REACHED
    /// ([`Self::landed`], round eighteen): typed, and the agent its own.
    fn typed(&self) -> bool {
        self.resume_wait_since.is_some() || self.landed
    }
}

/// Whether a restored relaunch of tab `sid` is OVER because an agent ran in
/// its tab (round eleven): it typed nothing ([`RestoredAttempt::typed`]), and
/// an agent ran there since it was queued — `seen`, the host's roster named
/// one ([`RestoredAttempt::agent_seen`]), or `runs` ([`Hooks::still_wanted`],
/// no group of its own to ask about) says the tab's publication names one
/// now: a person started one, or anyone did. Owed no more: never carried to
/// a successor ([`HostHandle::pending_restored_for`]) nor stepped
/// ([`relaunch_restored_queue`]), where the relaunch's shell-prompt guard
/// alone stood in front of typing its line into that agent's tab the moment
/// it gave the shell its prompt back (a ctrl-z, its own exit): a second agent
/// on the tab, or a conversation the person left brought back.
///
/// WHETHER ONE RAN, NOT WHETHER ONE RUNS (round twelve): `runs` alone is
/// read at one instant, and a ctrl-z or an exit clears the publication — an
/// agent started and stopped again between the park's capture and the
/// Commit's re-capture, or between two of the queue's steps, read as none,
/// and the relaunch was typed into the tab beside it. A step that typed its
/// line is never overtaken: the agent it started is its own (`seen` during
/// that step included). Asked with the queue's lock let go: the publication
/// is the store's.
fn restored_overtaken(
    typed: bool,
    seen: bool,
    runs: &dyn Fn(&str, i32) -> bool,
    sid: &str,
) -> bool {
    !typed && (seen || runs(sid, 0))
}

/// AN AGENT RUNS IN A TAB THE RESTORED QUEUE OWES (round twelve): every
/// queued relaunch of a tab the host's roster names an agent in — one it
/// supervises (`wanted`), or one its own switch leaves unsupervised
/// (`switched_off`: no worker is its, but it ran there) — is marked
/// ([`RestoredAttempt::agent_seen`]), for good — read at every host pass,
/// so an agent a person starts and stops again stays a fact after its
/// publication is cleared ([`restored_overtaken`]). The queue's lock alone,
/// nothing else held.
fn note_agents_in_restored_tabs(
    shared: &Shared,
    wanted: &HashMap<String, Program>,
    switched_off: &[String],
) {
    if wanted.is_empty() && switched_off.is_empty() {
        return;
    }
    let mut queue = shared
        .restored
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    for attempt in &mut queue.attempts {
        let sid = &attempt.agent.sid;
        if wanted.contains_key(sid) || switched_off.contains(sid) {
            attempt.agent_seen = true;
        }
    }
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
    /// rollback resumes it ([`HostHandle::resume_restored`]). The handoff
    /// layout reads the queue LIVE meanwhile ([`HostHandle::pending_restored_for`]).
    paused: bool,
    /// The first relaunch this process queued: [`RESTORED_HOLD`] runs from
    /// it, once per process — a later queue never extends it.
    since: Option<Instant>,
    /// The worker thread runs.
    running: bool,
    /// THE HOST SHUTS DOWN ([`HostHandle::shutdown_and_join`]; round
    /// thirteen): nothing is queued any more, and the worker steps nothing
    /// more and ends — read under this lock with what it gates, where the
    /// state lock's `shutting_down` was read before it and acted on after
    /// (a shutdown in between started the queue, and a queue already running
    /// went on stepping relaunches into tabs about to close).
    ///
    /// IT STOPS THE STEPS, NOT THE DEBTS (restore audit round two,
    /// 2026-10-02): what the queue still owes stays in [`Self::attempts`],
    /// read ([`HostHandle::pending_restored_for`]) by the quit layout of a
    /// restart, a logout or a shutdown, which is filled after this shutdown
    /// and is the one record left of it once that quit retires the crash
    /// journal (`App::fill_desk_agents`). The worker emptied the queue here,
    /// and such a restart inside the relaunch's window reopened its tab bare.
    /// A relaunch a stopped worker hands it after the close is refused and
    /// stays owed on its session, read there by the same layout (round
    /// sixteen, [`take_up_after_rollback`]).
    closed: bool,
    /// The times the worker found the queue paused and waited: the fact a
    /// test reads for "no step while parked" — each a look at the park that
    /// stepped nothing — where a sleep can pass before the worker looked.
    #[cfg(test)]
    parked_looks: usize,
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

/// How many rings ([`ring`]) this thread has made: a test outside this
/// module reads its own thread's count, as the host's own tests read
/// `RINGS_HERE`.
#[cfg(test)]
pub(crate) fn rings_here() -> u64 {
    RINGS_HERE.with(std::cell::Cell::get)
}

/// A changed agent phase wakes the host, but only the tab whose publication
/// stamp moved needs its Claude session file followed. Two phase wakes that
/// coalesce are treated as a full follow: the same tab could have moved away
/// and back between host reads.
pub(crate) fn ring_phase() {
    PHASE_FOLLOW_EPOCH.fetch_add(1, Ordering::SeqCst);
    #[cfg(test)]
    PHASE_RINGS_HERE.with(|n| n.set(n.get() + 1));
    ring_bell();
}

#[cfg(test)]
thread_local! {
    /// The phase wakes ([`ring_phase`]) this thread made, as [`RINGS_HERE`]
    /// counts [`ring`]'s: [`PHASE_FOLLOW_EPOCH`] is process-wide, and every
    /// other test's publication moves it.
    static PHASE_RINGS_HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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

/// What the control thread reaches of a session's worker when a proven
/// self-claim moves ([`self_claimed`], [`self_claim_released`]): its loop's
/// connection cut ([`WorkerJob::interrupt`]) and whether it watches beside a
/// proven self-claim now ([`WorkerJob::beside`]). Registered as the worker
/// starts, let go as it is reaped.
struct ClaimSide {
    interrupt: Arc<Mutex<Option<Interrupter>>>,
    beside: Arc<Mutex<Option<String>>>,
}

static CLAIM_SIDES: Mutex<Option<HashMap<String, ClaimSide>>> = Mutex::new(None);

/// Cut tab `sid`'s worker's loop where `cut` says so of its beside state:
/// the loop ends at once, and its worker decides again whose the session is
/// ([`worker_run`]: beside a proven self-claim, or its own loop).
fn cut_claim_side(sid: &str, cut: impl Fn(Option<&str>) -> bool) -> bool {
    let sides = CLAIM_SIDES.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(side) = sides.as_ref().and_then(|m| m.get(sid)) else {
        return false;
    };
    let beside = side
        .beside
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if !cut(beside.as_deref()) {
        return false;
    }
    if let Some(cut) = side
        .interrupt
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        cut();
    }
    true
}

/// A PROVEN SELF-CLAIM landed on tab `sid` ([`crate::self_claim`]),
/// replacing `replaced` when it replaced a claim: the host's own loop's
/// connection is CUT AT ONCE (its `HostCtl::interrupter`) — what it had in
/// flight is refused by the server's `if-supervisor=` fence, and nothing more
/// goes out — and its worker watches beside the self-claim from then on
/// ([`worker_run`]). The bell is rung for the host.
pub(crate) fn self_claimed(sid: &str, replaced: Option<&str>) {
    if cut_claim_side(sid, |beside| beside.is_none()) {
        aterm_log::info!(
            "harness @{sid}: a proven self-claim {} the session; its loop's connection is cut",
            if replaced.is_some() {
                "replaced this host's claim on"
            } else {
                "took"
            }
        );
    }
    CLAIM_EPOCH.fetch_add(1, Ordering::SeqCst);
    ring();
}

/// The proven self-claim on tab `sid` was cleared. Its agent still runs
/// (`alive`: codex-alab went passive and gave the claim back): the worker
/// watching beside it is cut, and takes the session back with its own loop
/// — aterm's lane and the menu rows return. Its agent gone: nothing is cut;
/// the exit is the watch-only worker's to read as it is handed
/// ([`on_agent_left`]).
pub(crate) fn self_claim_released(sid: &str, alive: bool) {
    if alive && cut_claim_side(sid, |beside| beside.is_some()) {
        aterm_log::info!(
            "harness @{sid}: the proven self-claim was given back while its agent runs; this \
             host supervises the session again"
        );
    }
}

/// THE PROVEN SELF-CLAIM a session's `status` names, when it is one this
/// host watches beside: its holder (`supervisor=`, percent-decoded) where
/// `supervisor_proven=1` — never this host's own, nor a claim whose holder is
/// no self-claim's spelling.
fn proven_self_claim(status: &str) -> Option<String> {
    let proven = status
        .split_whitespace()
        .any(|t| t == "supervisor_proven=1");
    if !proven {
        return None;
    }
    let holder = status
        .split_whitespace()
        .find_map(|t| t.strip_prefix("supervisor="))
        .map(aterm_control::wire::pct_decode)?;
    crate::self_claim::holder_pid(&holder).map(|_| holder)
}

fn bell_now() -> u64 {
    *BELL.rung.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Park on the bell until `done` is set or `deadline` passes; whether it was
/// set. Whoever sets a flag this waits on rings the bell after the store, so
/// no set is missed between the read and the wait ([`wait_for`]).
fn wait_done(done: &AtomicBool, deadline: Instant) -> bool {
    wait_for(deadline, || done.load(Ordering::SeqCst))
}

/// [`wait_done`] for any condition over state whose setters ring the bell.
/// The condition is read with the bell's lock NOT held — it may take a lock
/// of its own (the state's, a session's), under which another thread rings
/// (round seventeen: [`HostHandle::suspend`] rings under the state's lock,
/// which the host's restart reads at its back-off) — after the bell's count
/// is read: a set rung after that read moves the count, and the wait is not
/// begun.
fn wait_for(deadline: Instant, done: impl Fn() -> bool) -> bool {
    loop {
        let seen = bell_now();
        if done() {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let rung = BELL.rung.lock().unwrap_or_else(PoisonError::into_inner);
        if *rung == seen {
            drop(
                BELL.cv
                    .wait_timeout(rung, left)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
    }
}

/// Park until the bell has moved past `seen`, or `until` passes (the next
/// look a worker's upgrade asked for), or — `wall`, unix seconds — the WALL
/// CLOCK reaches the owner's view's next instant ([`ViewLook::next_s`]);
/// returns where the bell stands, and whether a deadline passed.
///
/// THE WALL CLOCK, CHECKED ACROSS A SLEEP (design record 2026-09-28, C10; the
/// owner's ruling of 2026-09-29, "with a grace after wake"). `until` is an
/// [`Instant`], which on macOS is `CLOCK_UPTIME_RAW` and does not advance
/// while the Mac sleeps: a watch the view named for 3 AM, armed at 11 PM and
/// slept through, came due only once the Mac had been AWAKE the four hours
/// left — so every record due overnight went unwatched for hours of the
/// morning, which the owner's view now says (`unwatched`) past a wake grace
/// of [`upgrade_drive::WAKE_GRACE_S`]. While the view names an instant, the
/// wait is cut into slices of [`upgrade_drive::WAKE_CATCH_UP_S`] and the
/// wall clock read at each: the host is past its view's instant within one
/// slice of a wake, well inside the grace. A slice that ends with the wall
/// short of it waits again: no pass, no look — one condition-variable wake,
/// and only while a record is owed a watch.
fn bell_wait(seen: u64, until: Option<Instant>, wall: Option<u64>) -> (u64, bool) {
    let mut rung = BELL.rung.lock().unwrap_or_else(PoisonError::into_inner);
    let mut timed_out = false;
    let slice = wall.map(|_| Duration::from_secs(upgrade_drive::WAKE_CATCH_UP_S));
    while *rung == seen {
        if wall.is_some_and(|at| at <= crate::upgrade_host::now_s()) {
            timed_out = true;
            break;
        }
        let left = until.map(|at| at.saturating_duration_since(Instant::now()));
        if left.is_some_and(|left| left.is_zero()) {
            timed_out = true;
            break;
        }
        rung = match [left, slice].into_iter().flatten().min() {
            None => BELL.cv.wait(rung).unwrap_or_else(PoisonError::into_inner),
            Some(left) => {
                BELL.cv
                    .wait_timeout(rung, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
        };
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

/// The holder name this process claims sessions under: the one spelling
/// `aterm harness upgrade --status` tells this host's claim from another
/// supervisor's by ([`upgrade_drive::hand_name`]).
pub(crate) fn holder() -> String {
    upgrade_drive::hand_name()
}

/// ANOTHER SUPERVISOR'S LIVE CLAIM, from one `status` reply: the holder its
/// `supervisor=` names (percent-decoded), when it is no window host's
/// ([`upgrade_drive::is_host_holder`], as `aterm harness upgrade --status`
/// tells them). `None` for no claim (`-`), this host's own ([`holder`]), a
/// predecessor's (a seamless update leaves the outgoing host's claim behind,
/// and one carried anyway names a process that is gone and lapses by
/// itself), or a reply that names none (a server before the field: nobody's,
/// as an unreadable status is to [`relaunch::on_exit`]).
///
/// A PROVEN SELF-CLAIM WHOSE PROCESS IS GONE is nobody's (the self-claim
/// contract, §4): the agent that supervised itself is the one that exited,
/// and the exit is this host's watch-only worker's to decide — never left to
/// a dead holder ([`on_agent_left`]).
fn claimed_by_another(status: &str) -> Option<String> {
    let named = status
        .split_whitespace()
        .find_map(|t| t.strip_prefix("supervisor="))
        .filter(|h| *h != "-")
        .map(aterm_control::wire::pct_decode)?;
    if proven_self_claim(status).is_some()
        && crate::self_claim::holder_pid(&named)
            .is_some_and(|pid| !aterm_uds::process::pid_alive(pid))
    {
        return None;
    }
    (!upgrade_drive::is_host_holder(&named)).then_some(named)
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
    admission: Option<(Arc<Switches>, Program)>,
    /// THE LOOP'S OWN HOLDER, fencing every write it types
    /// (`if-supervisor=<holder>`, [`fenced_write`]): a write that reaches the
    /// server after another claim replaced the loop's — a proven self-claim —
    /// is refused there, never typed. `None`: unfenced.
    fence: Option<String>,
    /// WATCH-ONLY beside this proven self-claim's holder ([`WorkerJob::beside`]):
    /// every write that types into the session (`key`, `send`, `paste`,
    /// `turn`, `feed-bin`) is answered `ERR busy supervisor=<holder>` here and
    /// never reaches the wire. `None`: writes go out.
    watch_only: Option<String>,
}

/// Whether `args` types into the session: what a watch-only transport
/// refuses ([`HostCtl::watch_only`]).
fn is_typing(args: &[&str]) -> bool {
    args.iter()
        .find(|a| !a.starts_with('@'))
        .is_some_and(|verb| matches!(*verb, "key" | "send" | "paste" | "turn" | "feed-bin"))
}

/// `args` with `if-supervisor=<holder>` put right after the verb of a write
/// the server fences on its claim (`key`, `send`, `paste`, `turn`: the verbs
/// that type into the session), so the option LEADS as the server reads
/// options; every other request is returned as it is.
fn fenced_write<'a>(args: &[&'a str], fence: &'a str) -> Option<Vec<&'a str>> {
    let at = args.iter().position(|a| !a.starts_with('@'))?;
    if !matches!(args[at], "key" | "send" | "paste" | "turn") {
        return None;
    }
    let mut out = Vec::with_capacity(args.len() + 1);
    out.extend_from_slice(&args[..=at]);
    out.push(fence);
    out.extend_from_slice(&args[at + 1..]);
    Some(out)
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
            admission: None,
            fence: None,
            watch_only: None,
        }
    }
}

impl Ctl for HostCtl {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        if self
            .admission
            .as_ref()
            .is_some_and(|(switches, agent)| !switches.on_for(*agent))
        {
            self.cut.store(true, Ordering::SeqCst);
        }
        if self.cut.load(Ordering::SeqCst) {
            if !cleanup_request(args) {
                return Err(STOPPED.to_string());
            }
            if !self.redialed {
                self.inner = RelayCtl::new(self.endpoint.clone(), self.token.clone());
                self.redialed = true;
            }
        }
        if let Some(holder) = &self.watch_only
            && is_typing(args)
        {
            return Ok(CtlReply {
                code: 1,
                stdout: String::new(),
                stderr: format!(
                    "aterm-ctl: ERR busy supervisor={} (watch-only beside a proven self-claim)\n",
                    aterm_control::wire::pct_encode(holder)
                ),
            });
        }
        let fence = self.fence.as_ref().map(|h| format!("if-supervisor={h}"));
        match fence.as_deref().and_then(|f| fenced_write(args, f)) {
            Some(fenced) => self.inner.call(&fenced),
            None => self.inner.call(args),
        }
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
    /// Policy admission revoked for this worker for good. A stop also means
    /// an exit or Commit, so it cannot carry this distinct off/on fence.
    pub(crate) revoked: Arc<AtomicBool>,
    /// The live connection's cut, published by the body so a stop ends a
    /// parked wait at once.
    pub(crate) interrupt: Arc<Mutex<Option<Interrupter>>>,
    /// Set before `stop` when a new worker takes the session straight after
    /// (a changed policy): the loop leaves its badges for that one to adopt,
    /// and the worker leaves its agent's exit to the host as it ends
    /// ([`stopped`], [`Kept::unwatched`]).
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
    /// THE HOST'S WORD ON THE WORKER'S AGENT ([`ExitWord`]). [`Word::Left`]
    /// when the host hands the worker its agent's exit, before it stops it:
    /// the session's agent LEFT a tab that lives on (not when the tab closed
    /// or the policy stopped it) — the roster names it no longer, and it is
    /// not read to hold the tab ([`Acts::holds`] answers anything but
    /// `Some(true)`). A name the roster could not read while the agent still
    /// holds its tab is no exit ([`keep_holding`]), but a hold that cannot be
    /// read keeps nothing, so where it never can (a ConPTY has no foreground
    /// group) the roster alone decides, as it did before the hold; and a keep
    /// ends at [`HOLDS_KEEP_MAX`], or when another group is named in its
    /// stead. [`Word::Watching`] again if the agent is back; [`Word::Stopped`]
    /// once a stop reached the worker, for good. The worker handles the exit
    /// ([`on_agent_left`]).
    pub(crate) left: Arc<ExitWord>,
    /// A PARK'S COMMIT STOPPED THE WORKER WITH ITS AGENT'S EXIT HANDED TO IT
    /// ([`Self::left`]; round twelve): set by [`HostHandle::suspend`] — or by
    /// [`Worker::leave`] after it — before that stop's word, and never
    /// cleared. The stop's word ends a relaunch's wait, and `left` was also
    /// the one word the worker read to handle the exit at its end
    /// ([`stopped`]) and to finish reading what the exit left
    /// ([`on_agent_left`]'s settle): a worker the Commit's stop reached
    /// between its agent's exit and that read handled nothing, or read the
    /// crash as unread, and the relaunch it owed was lost — no worker comes
    /// back for a tab at its shell. With this set it reads the exit through
    /// (read-only), and what it owes is the Commit's ([`Committed`]).
    pub(crate) left_at_commit: Arc<AtomicBool>,
    /// Set while the worker takes an upgrade step: the agent it ends and
    /// relaunches is ITS act, so while the step runs the host neither stops
    /// it nor hands it the exit. A step that ended the agent and whose
    /// relaunch waited leaves the tab at its shell once it is over: that exit
    /// is handed, and carried as the harness's own restart's whatever
    /// `[harness] relaunch` says ([`Acts::restarted`], [`on_agent_left`]).
    pub(crate) acting: Arc<AtomicBool>,
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
    /// The host, for the one act a stopped worker hands it: a relaunch the
    /// Commit's stop left owed, decided after that Commit's rollback
    /// ([`take_up_after_rollback`]). Weak: a worker never keeps its host.
    host: std::sync::Weak<Shared>,
    /// THE ADOPTED-CLAIM GRACE THIS WORKER WATCHES THROUGH (round eight of
    /// the update audit, #501; [`ADOPTED_CLAIM_GRACE`]): until this instant
    /// the worker only WATCHES. It attaches as every worker does — it reads
    /// what a relaunch of its agent needs ([`attach`]) — and the host hands
    /// it its agent's exit as it hands any worker one ([`Worker::leave`]),
    /// but it runs no loop: it claims nothing and types nothing. An exit
    /// handed meanwhile is decided by this worker as any worker decides one
    /// ([`on_agent_left`]: a person's own `/exit` is theirs, a crash is
    /// relaunched on its conversation, and another supervisor's live claim at
    /// the exit makes it that supervisor's), save that the claim comes first
    /// even of an exit the harness's own restart ended ([`Acts::restarted`])
    /// and is asked again before every attempt at its relaunch
    /// ([`claimed_by_another`]). Past it the same worker runs its loop, which
    /// claims the session — or yields to the supervisor that claimed it first
    /// ([`BodyEnd::Held`]). A worker that reads its agent's exit alone
    /// ([`exit_main`]) started inside the grace carries it too (the review of
    /// #501): it runs no loop at all, so for it the instant turns on that
    /// claim check alone. `None` for every other worker, and once the grace
    /// is over.
    watch_until: Option<Instant>,
    /// THE PROVEN SELF-CLAIM THIS WORKER WATCHES BESIDE (the self-claim
    /// contract, §4), its holder, or `None`: set by [`worker_run`] from the
    /// session's `status` (`supervisor_proven=1`) whenever its loop ends — in
    /// both orders: its loop's claim met the proven one (`take_claim` behind
    /// it), or the proven one replaced its loop's and the control thread cut
    /// that loop ([`self_claimed`]). While set the worker is WATCH-ONLY: its
    /// loop claims nothing and types, presses and upgrades nothing
    /// ([`Session::set_beside`], a transport that refuses every write, a
    /// [`WorkerIdle`] that asks for no step), yet keeps the stall's restart
    /// and the memory ceiling's measure, and it is its agent's exit's one
    /// decider ([`on_agent_left`]'s crash rule, never the Codex lane's).
    /// Shared with its [`WorkerIdle`] and [`CLAIM_SIDES`].
    pub(crate) beside: Arc<Mutex<Option<String>>>,
}

impl WorkerJob {
    /// The proven self-claim this worker watches beside, if any.
    fn beside(&self) -> Option<String> {
        self.beside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn admitted(&self) -> bool {
        !self.revoked.load(Ordering::SeqCst) && self.switches.on_for(self.agent)
    }
}

/// What the host last said of a worker's agent ([`ExitWord`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Word {
    /// No exit handed: the agent runs in the tab as the host last read it —
    /// since the worker's start, or BACK after an exit (relaunched, or
    /// started by someone).
    Watching = 0,
    /// The agent's exit is the worker's to read ([`Worker::leave`]; a worker
    /// that reads its exit alone starts so, [`exit_main`]).
    Left = 1,
    /// A stop reached the worker ([`request_stop`]): for good — its tab
    /// closed, the policy, the shutdown, a hand-over — or a park's Commit's,
    /// which [`HostHandle::suspend`] records on the session first
    /// ([`Kept::commit`]). Never undone.
    Stopped = 2,
}

/// THE HOST'S WORD ON A WORKER'S AGENT, ONE WORD (the joint audit of
/// 2026-10-01): its exit handed, the agent back, or a stop. It was one flag,
/// `left`, which the agent's return and a stop both cleared, and "no flag
/// says which": a relaunch whose own agent came back — the roster named it
/// while the line's act waited on it — and was gone again before the worker
/// read the flag was read as a stop for good. The relaunch it owed was
/// dropped with nothing said and no worker left for a tab at its shell, a
/// park's re-capture no longer carried it (its Commit refused "so this
/// process relaunches it", which nothing did), and a Commit's stop that came
/// while the agent was named, reading the cleared flag, marked nothing. A
/// stop is now a fact of its own, set by the stop and never undone, and the
/// word on the agent moves only between the words that are not a stop.
#[derive(Debug, Default)]
pub(crate) struct ExitWord(AtomicU8);

impl ExitWord {
    /// A word with the agent's exit handed from the start ([`exit_main`]).
    fn handed() -> Self {
        Self(AtomicU8::new(Word::Left as u8))
    }

    fn read(&self) -> Word {
        match self.0.load(Ordering::SeqCst) {
            0 => Word::Watching,
            1 => Word::Left,
            _ => Word::Stopped,
        }
    }

    fn is(&self, word: Word) -> bool {
        self.read() == word
    }

    /// `from` moved to `to`, unless the word is `from` no longer (a stop
    /// came first: it stands).
    fn moved(&self, from: Word, to: Word) -> bool {
        self.0
            .compare_exchange(from as u8, to as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// The agent's exit handed: by the host to a worker whose agent ran, or
    /// by the worker to itself again when the agent the host read back is
    /// gone again ([`relaunch_until`]). None to a stopped worker.
    fn hand(&self) -> bool {
        self.moved(Word::Watching, Word::Left)
    }

    /// The agent is back: its exit is over.
    fn back(&self) -> bool {
        self.moved(Word::Left, Word::Watching)
    }

    fn stop(&self) {
        self.0.store(Word::Stopped as u8, Ordering::SeqCst);
    }
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
    /// that agent's exit is handled, or once nothing will handle it (an Off
    /// session's, [`unwatched_exits`]) — never carried onto the next agent.
    snapshot: Option<Snapshot>,
    /// THE RELAUNCH ON EXIT THIS SESSION IS OWED while its back-off runs
    /// ([`relaunch_until`]): the agent's snapshot, for an exit the relaunch
    /// is decided on already — a crash (a Claude Code's record survived it,
    /// a Codex's shell said so: [`relaunch::ExitRecord::crashed`]) or a
    /// stall's remedy — and `None` once the relaunch lands, is left or can never
    /// land. What a seamless update carries across its handoff for the tab
    /// ([`HostHandle::pending_restored_for`]; round six, F13): the back-off
    /// lived in the worker alone, the Commit's stop read as the agent being
    /// back, and neither the successor nor a failed Commit's resumed host
    /// ever relaunched the conversation. With the explicit restart's cause
    /// where its act ended the agent (round eighteen, [`OwedRelaunch`]).
    owed: Option<OwedRelaunch>,
    /// What a park's Commit made of the session's worker and of what it owes
    /// ([`Committed`]). Reset when any worker starts for the session
    /// ([`start_worker`]): one for an agent that runs in the tab, which owes
    /// nothing of the last one's exit — or one that reads an exit no worker
    /// was handed ([`exit_main`], [`Kept::unwatched`]), whose exit is that
    /// worker's now, an ordinary exit of this host, and nothing of the Commit
    /// that left it unwatched is left to act on.
    commit: Committed,
    /// A PARK'S COMMIT STOPPED THE SESSION'S WORKER WITH NO EXIT OF ITS AGENT
    /// HANDED TO IT, AND THAT WORKER HAS ENDED (round thirteen; the review of
    /// round twelve): what it knew of the agent it supervised ([`Unwatched`]:
    /// the agent, and the group the roster last named it in — not whether its
    /// loop held for a stall, which is no worker's to carry: an exit the
    /// explicit restart made is read by its stamp, [`Acts::asked`]).
    /// Set by the worker as it ends ([`stopped`]), under this
    /// lock, which [`Worker::leave`] hands an exit under — so one of the two
    /// sees the other. Nobody watches that agent's exit now: an agent that
    /// exits while the Commit holds the host (a suspended host hands no exit),
    /// or just before the Commit's stop and before the host's pass handed it,
    /// was never read, and after a failed Commit no worker came back for a tab
    /// the roster names no agent in — the F13 loss, on the same late-host
    /// premise. The host hands it once it supervises again
    /// ([`start_workers`]: a worker that reads the exit alone,
    /// [`exit_main`]); a worker started for an agent in the tab, a tab
    /// closed, or a harness switched off ends it.
    ///
    /// A WORKER HANDED OVER ENDS SO TOO (round fourteen; the review of round
    /// thirteen): a changed policy stops it ([`Worker::hand_over`]) for a
    /// successor the host starts only at its next pass that finds the agent
    /// in the tab, and never while a Commit holds it. Its `stop` is already
    /// set when a Commit's [`HostHandle::suspend`] comes, so the Commit does
    /// not mark the session; an agent that exited between the hand-over and
    /// that successor — a pass at most, or the Commit's whole hold — was
    /// handed to nobody (a stopped worker is handed no exit), and once the
    /// Commit failed nothing read it.
    ///
    /// AN AGENT THE HOST READS BACK IN THE TAB OF A WORKER HANDED ITS EXIT IS
    /// MARKED SO TOO (the joint audit of 2026-10-01; [`Worker::back`]): that
    /// worker is stopped — it only finishes deciding the exit it was handed
    /// — and the agent's own worker starts at the host's next pass that finds
    /// it in the tab, after this one is reaped. A relaunch that LANDED (its
    /// act's word, `adopted`, after the roster named the agent it started),
    /// or one whose agent is back, ended its worker with nothing watching the
    /// new agent until then, and an exit in between was read by nobody. The
    /// worker takes the mark back if it reads that agent gone again before
    /// it ends — in its back-off, or at the landing that started it, read
    /// by nothing (round two) — the exit is still its own relaunch's
    /// ([`relaunch_until`], [`gone_again`]). That mark names the agent as the
    /// roster read it back, and the worker's own end keeps it ([`stopped`]).
    ///
    /// Whoever is handed that exit later reads what it left AS IT WAS SEEN
    /// (round seventeen): the worker that ends leaving it here arms the exit
    /// watch ([`Self::exit_watch`]).
    unwatched: Option<Unwatched>,
    /// THE EXIT WATCH (round seventeen; [`watch_exit`]): the kernel's word of
    /// the session's agent's end, for the time no pass of the host would see
    /// it — the worker ended leaving its exit to the host ([`Self::unwatched`]:
    /// a park's Commit holding the host, a hand-over's gap), the host thread
    /// down after a panic ([`host_restarted`]), or a relaunch landed an agent
    /// no pass has named yet, whose exit the watch hands itself
    /// ([`ExitWatched::hands`], [`runs_again`]) — a worker's landing, the
    /// cold restore queue's ([`restored_landing_look`]), or the agent a seamless
    /// update's outgoing host supervised, watched from the successor's
    /// Commit ([`HostHandle::watch_handed`]; round eighteen). At the end it reads
    /// what the exit left within its settle ([`relaunch::exit_record`]) and
    /// keeps it, for the worker the host hands that exit to once it reads its
    /// roster again ([`read_at_exit`]): read only then — a Commit's whole
    /// hold, or the panic's back-off, later — a crash's record that any
    /// Claude Code started meanwhile had removed read as a graceful exit, and
    /// it was never relaunched (D2 of the 2026-09-26 live test). Ended by the
    /// worker that takes its read, and by a pass that names an agent in the
    /// tab again — a watch armed before it read its roster — or finds the
    /// harness switched off ([`end_exit_watches`]).
    exit_watch: Option<ExitWatched>,
    /// What the host keeps of the session's ACCOUNT ([`AccountKept`]).
    account: AccountKept,
}

/// A RELAUNCH ON EXIT A SESSION IS OWED ([`Kept::owed`]): the agent's
/// snapshot, and — where the explicit restart's act ended it — the act's
/// cause as the worker deciding its exit holds it (round eighteen; ruling
/// 20's (iv)): read at the exit's first read, or at the end of a program its
/// act put off ([`on_agent_left`]), and kept for the relaunch however long
/// a hand it is carried through stands, whatever becomes of the stamp. What
/// a handoff carries to the successor's queue ([`RestoredAgent::asked`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwedRelaunch {
    pub(crate) snap: Snapshot,
    pub(crate) asked: Option<restart_stamp::Cause>,
}

/// AN EXIT NO WORKER WAS HANDED ([`Kept::unwatched`]): what the worker that
/// ended handed nothing knew of its agent, for the worker that reads the exit
/// alone ([`exit_main`]). Whether its exit was the explicit restart's is not
/// here: that is the restart's stamp, on disk before its signal, which
/// outlives the worker and the instance ([`Acts::asked`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unwatched {
    /// The agent it supervised.
    agent: Program,
    /// The group the roster last named it in.
    group: i32,
}

/// WHAT THE EXIT WATCH KEEPS ([`Kept::exit_watch`]): the process it watches,
/// as the session's snapshot named it when it was armed, and what that
/// process's exit left once it is read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ExitWatched {
    /// The process: its pid and kernel start ([`Snapshot::pid`],
    /// [`Snapshot::start`]).
    pid: u32,
    start: String,
    /// Which arming this is ([`EXIT_WATCH_ARMINGS`]): a watch whose arming
    /// was ended, or replaced, keeps nothing and stops at its next look.
    arming: u64,
    /// THE EXIT IT HANDS ITSELF, once read (round seventeen, the review of
    /// its first cut): the agent a relaunch LANDED ([`runs_again`]), alive
    /// and named by no pass yet, its worker ended. No worker is handed that
    /// exit and no mark leaves it to the host — read as the host's before the
    /// roster caught up, it would be the exit of an agent alive in its tab —
    /// so the kernel's word of the very process's end, read here, is what
    /// hands it: the host's next pass that finds no agent named in the tab
    /// starts a worker that reads it alone ([`unwatched_exits`]). `None`: a
    /// worker, or a mark, hands this watch's exit.
    hands: Option<Unwatched>,
    /// What the exit left, read as it was seen; `None` until it is.
    read: Option<ExitRecord>,
}

impl ExitWatched {
    /// Whether it watches the very process `snap` names.
    fn names(&self, snap: &Snapshot) -> bool {
        self.pid == snap.pid && self.start == snap.start
    }
}

/// WHAT THE HOST KEEPS OF A SESSION'S ACCOUNT, across its workers (the
/// accounts design, §6): what it last advised, why its last ask for a move
/// was not made, the move it made that nothing has answered yet, and one
/// whose relaunch it has not seen land. Leaf state under [`Kept`]'s lock;
/// with no roster none of it is ever set — but a limit reset's outcome the
/// lock kept out, under `[harness] limit_reset` — and what reads it finds
/// nothing.
#[derive(Default)]
struct AccountKept {
    /// The move [`IdleHost::account`] last advised, and at which point: the
    /// one move [`IdleHost::restart`] makes. Anything else asked of it — an
    /// advice the loop kept from before the roster changed, a point of
    /// another kind — is answered `wait:account-changed`, and the loop asks
    /// for advice again.
    advised: Option<Advised>,
    /// WHY THE LAST MOVE ASKED WAS NOT MADE, for the report
    /// (`accounts::SessionRow::held`): `jobs`, `person` or `line`
    /// ([`held_word`]); `None` once a move is made, or where the refusal
    /// was none of those.
    held: Option<&'static str>,
    /// Why the roster last advised NOTHING (`accounts::StayWhy`'s word), so
    /// the log says it when it changes and not at every turn end.
    stay: Option<&'static str>,
    /// The label of the last ask that was NOT MADE with nothing signalled: a
    /// tab whose own jobs refuse the relaunch is asked again on the loop's
    /// back-off for as long as they run, and the rotation it commits and
    /// takes back each time is written to the ledger — and wakes the other
    /// sessions — the FIRST time only ([`WorkerIdle::account_unmade`]).
    /// Whatever the step: a reason that changes is the session's row's to
    /// say ([`Self::held`]).
    unmade: Option<String>,
    /// THE MOVE THIS SESSION'S HOST MADE THAT NOTHING HAS SERVED YET
    /// ([`Moved`]): kept until the loop says the account served a turn
    /// ([`IdleHost::account_served`]) or another move is made; a bad answer
    /// told of it and struck on record marks it answered
    /// ([`Moved::answered`]) and keeps it.
    moved: Option<Moved>,
    /// THE LOST LOGIN OF THE PROCESS IN THE TAB, AS THIS HOST FIRST MET IT
    /// ([`LostSeen`]; [`WorkerIdle::lost_seen`]): what the roster is told of
    /// when the login was lost (`accounts::Point::LostLogin`'s `seen`).
    lost: Option<LostSeen>,
    /// THE LOGIN THE WAY BACK BROUGHT THE SESSION UP UNDER, raw
    /// ([`Snapshot::login`]), set where [`account_way_back`] carried the
    /// login before the move — a login nobody saw lost: the session left it
    /// by a move, and the store does not record it signed out. The relaunch
    /// resumes the moved process's conversation, whose last row may be that
    /// process's `Please run /login`: the next process's first ask at a lost
    /// login, under this very login, is met as long ago
    /// ([`WorkerIdle::lost_seen`]), so the login's standing `ok` brings it
    /// back under itself. Taken at the next ask of any point, and dropped
    /// where a lost login of the session's own is told.
    came_back: Option<Option<String>>,
    /// AN ACCOUNT MOVE WHOSE RELAUNCH NOBODY HAS SEEN LAND ([`Flying`]): its
    /// signal was sent and its line is the host's carry's to finish, or its
    /// new process could not be read when it was adopted. Kept until a
    /// process other than the one before the move is READ in the tab
    /// ([`account_land`]).
    flying: Option<Flying>,
    /// THE AGENT'S SETTINGS SIDE OF ITS LOGIN as last read, by its pid, its
    /// kernel start and its directory ([`WorkerIdle::settled`]: read once
    /// per agent process, as its route is).
    settled: Option<(u32, String, String, bool)>,
    /// HOW MANY MORE TIMES THE RECORDER IS ASKED in the limit episode that
    /// stands ([`WorkerIdle::account_wall_again`]): [`WALL_AGAIN`] where its
    /// first answer was one that asking again can change
    /// ([`wall_asked_again`]), none once it answered anything else or the
    /// episode closed.
    wall_again: u8,
    /// HOW MANY TIMES THE LOOP WAS TOLD TO ASK AGAIN SOON in the limit
    /// episode that stands ([`WorkerIdle::account_lags`]: the short wait a
    /// host whose evidence lags the screen answers): [`WALL_AGAIN`] at most,
    /// and none again until the episode has closed.
    lagged: u8,
    /// THE `/usage` PROBE THIS SESSION CLAIMED AND HAS NOT BEEN TOLD THE
    /// READ OF ([`Probing`]; [`WorkerIdle::probe_claim`]): taken when the
    /// loop says what the probe read ([`WorkerIdle::probe_record`]), whatever
    /// it read. A read told with none standing is recorded for nobody.
    probing: Option<Probing>,
    /// THE OUTCOME OF THIS SESSION'S LIMIT RESET THAT THE STORE'S LOCK KEPT
    /// OUT ([`ResetOwed`]; [`WorkerIdle::reset_tell`]): written at the
    /// session's next transaction on the accounts state
    /// ([`WorkerIdle::reset_flush`]). Never set with `[harness] limit_reset`
    /// off.
    reset_owed: Option<ResetOwed>,
}

/// [`AccountKept::probing`]: the claim of one `/usage` probe, and THE
/// PROCESS IT WAS MADE FOR. A sample is recorded only where the process in
/// the tab when the read is told is this one, under this label: a relaunch
/// between the claim and the read (an upgrade, an account move, a crash)
/// leaves a process whose panel is another's — perhaps another account's.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Probing {
    /// The roster's label the claim was made for.
    label: String,
    /// The process it was made for: its pid and kernel start.
    pid: u32,
    start: String,
}

/// WHY A PROBE WAS NOT CLAIMED ([`WorkerIdle::probe_claimed`]), each by its
/// own word for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoProbe {
    /// `[harness] accounts`, read live, is not `early`: `wall` and `off`
    /// never probe. Nothing was read.
    Mode,
    /// No roster lists an account, the agent is no Claude Code, or nothing
    /// of its process could be read. With no roster nothing was read, made
    /// or locked.
    Unread,
    /// The process is not one the roster's judgments are about
    /// ([`AccountLook::judged`]: a listed, stored login in the default
    /// home): the figures its panel shows are no listed account's.
    NotJudged,
    /// Asked afresh under the lock, the roster no longer says a probe is
    /// due: a sample was recorded since, a wall stands on the account, the
    /// owner pinned, the account is the active one still unanswered, or
    /// there is an account to follow.
    NotDue,
    /// A claim stands in the way (`accounts::probe_held`): this account's
    /// within `accounts::PROBE_MIN_S`, or ANY account's within
    /// `accounts::PROBE_APART_S` — the vendor answers a usage read from the
    /// home's one snapshot for a minute, whichever login fetched it (READ
    /// in the 2.1.287 bundle; never observed with two logins).
    Claimed,
    /// The store's lock stayed held past its bounded wait.
    LockHeld,
    /// The state could not be read or written as this build's.
    State,
}

impl NoProbe {
    fn as_str(self) -> &'static str {
        match self {
            NoProbe::Mode => "not-early",
            NoProbe::Unread => "unread",
            NoProbe::NotJudged => "not-a-stored-login",
            NoProbe::NotDue => "not-due",
            NoProbe::Claimed => "claimed",
            NoProbe::LockHeld => "lock-held",
            NoProbe::State => "state",
        }
    }
}

/// WHY A LIMIT RESET WAS NOT CLAIMED ([`WorkerIdle::reset_claimed`]), each by
/// its own word for the log. What the loop is answered is [`Self::word`]:
/// its own coarser word, or to ask again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoReset {
    /// `[harness] limit_reset`, read live, is off (or the whole policy).
    /// Nothing was read.
    Off,
    /// The agent is no Claude Code, the host has no seat, or nothing of the
    /// agent's process could be read.
    Unread,
    /// The roster's file does not admit: a refused roster resets nothing.
    Roster,
    /// The process names a login directory: it is not on the default login.
    LoginDir,
    /// Its Claude home is one of its own.
    OwnHome,
    /// It does not authenticate by its stored login: a credential or a route
    /// in its environment, or settings that take the stored login away.
    NotStored,
    /// No row of the roster is the default login's (every row names a
    /// directory), or the row is no longer the one with none.
    NotDefaultRow,
    /// The machine's active account is another row.
    ActiveOther,
    /// Another account ran within the vendor's profile window of this
    /// process's start (`accounts::RESET_PROFILE_S`).
    OtherLogin,
    /// `[harness] accounts = "off"` and the roster lists another login:
    /// nothing records when that login ran (the sessions' rows are the
    /// rotation's to write), so no other login can be said to be off the
    /// record.
    Unrecorded,
    /// No quota rejection stands last in the transcript yet
    /// ([`Standing::Unwritten`]): asked again.
    Unwritten,
    /// The one that stands is not this process's ([`Standing::NotOwn`]).
    NotOwnWall,
    /// It names no reset, one that is not believed
    /// (`accounts::believed_reset`), or one that has come.
    NoUntil,
    /// Its window is not one a grant clears ([`aterm_phase::reset_limit_name`]).
    Window,
    /// The wall is neither a long one nor one whose grant could end within a
    /// day ([`reset_qualifies`]).
    NotQualifying,
    /// The account's walls were ended after this wall's row was written —
    /// by a reset, or by the owner's `accounts use`.
    Cleared,
    /// Another claim is in flight (`accounts::ResetClaimed::Held`).
    Held,
    /// This wall has had its one attempt (`accounts::ResetClaimed::Attempted`).
    Attempted,
    /// The claim is this session's, and too near its lapse for an Enter
    /// written now to be answered inside it ([`RESET_ENTER_MARGIN_S`]).
    Late,
    /// The claim is this session's, and the dialog it was made for has
    /// already answered: its command's output stands in the transcript
    /// since the claim.
    Answered,
    /// The store's lock stayed held past its bounded wait: asked again.
    LockHeld,
    /// The state could not be read or written as this build's.
    State,
}

impl NoReset {
    fn as_str(self) -> &'static str {
        match self {
            NoReset::Off => "off",
            NoReset::Unread => "unread",
            NoReset::Roster => "roster-refused",
            NoReset::LoginDir => "login-dir",
            NoReset::OwnHome => "own-home",
            NoReset::NotStored => "not-a-stored-login",
            NoReset::NotDefaultRow => "not-the-default-row",
            NoReset::ActiveOther => "active-other",
            NoReset::OtherLogin => "other-login",
            NoReset::Unrecorded => "accounts-off",
            NoReset::Unwritten => "no-wall",
            NoReset::NotOwnWall => "not-own-wall",
            NoReset::NoUntil => "no-reset",
            NoReset::Window => "window",
            NoReset::NotQualifying => "not-qualifying",
            NoReset::Cleared => "cleared",
            NoReset::Held => "held",
            NoReset::Attempted => "attempted",
            NoReset::Late => "late",
            NoReset::Answered => "answered",
            NoReset::LockHeld => WALL_LOCK_HELD,
            NoReset::State => "state",
        }
    }

    /// What the loop is answered (`IdleHost::reset_take`): to ask again for
    /// the two refusals a moment can change, else no, by the loop's own
    /// word. A wall whose limits were cleared since, and a claim too near its
    /// lapse, are answered as the wall's attempt being over.
    fn word(self) -> ResetWord {
        ResetWord::No(match self {
            NoReset::Unwritten | NoReset::LockHeld => return ResetWord::Again,
            NoReset::Off => ResetNo::Off,
            NoReset::Unread => ResetNo::Unread,
            NoReset::Roster | NoReset::State => ResetNo::State,
            NoReset::LoginDir | NoReset::OwnHome | NoReset::NotStored | NoReset::NotDefaultRow => {
                ResetNo::NotDefaultLogin
            }
            NoReset::ActiveOther | NoReset::OtherLogin | NoReset::Unrecorded => ResetNo::OtherLogin,
            NoReset::NotOwnWall => ResetNo::NotOwnWall,
            NoReset::NoUntil => ResetNo::NoReset,
            NoReset::Window => ResetNo::Window,
            NoReset::NotQualifying => ResetNo::NotQualifying,
            NoReset::Held => ResetNo::Held,
            NoReset::Cleared | NoReset::Attempted | NoReset::Late | NoReset::Answered => {
                ResetNo::Attempted
            }
        })
    }
}

/// How long a wall must still stand for a reset to be worth taking whatever
/// its grant's deadline ([`reset_qualifies`]): more than a day.
const RESET_LONG_WALL_S: i64 = 24 * 3600;

/// How long before its lapse this session's own claim stops answering for a
/// key ([`NoReset::Late`]): the claim holds every move off the account only
/// while it is in flight (`accounts::RESET_CLAIM_S`), and an Enter written in
/// its last seconds would be answered by the vendor — whose claim is bounded
/// at 35 s (READ in the 2.1.287 bundle) — after the hold had lapsed, on an
/// account a session may just have been moved off. Two minutes: the vendor's
/// bound, the loop's wait for the answer and the transcript's lag, twice over.
const RESET_ENTER_MARGIN_S: i64 = 120;

/// How long after its claim a dialog's answer line is read as that claim's
/// ([`WorkerIdle::reset_tell`]): the claim's life and the margin its Enter is
/// kept inside. A step told of later than this — taken up by a loop that
/// started long after, held for a person past every bound — is told without
/// a line: what the transcript holds by then may be a later dialog's.
const RESET_LINE_WINDOW_S: i64 = accounts::RESET_CLAIM_S + RESET_ENTER_MARGIN_S;

/// WHETHER A WALL QUALIFIES FOR A RESET on the host's clock `now`, and
/// whether it is a LONG one: `Some(true)` where it still stands more than
/// [`RESET_LONG_WALL_S`] (`until` is its own row's reset); `Some(false)`
/// where it does not and the grant's last day (`day_start`, its first second
/// as the loop placed it) could hold a deadline within
/// `accounts::OFFER_EXPIRING_S` — a pre-filter: the deadline itself is judged
/// on the confirmation's day and hour; `None` where neither holds, or the
/// day did not place. PURE.
fn reset_qualifies(until: i64, day_start: Option<i64>, now: i64) -> Option<bool> {
    const DAY_S: i64 = 24 * 3600;
    if until.saturating_sub(now) > RESET_LONG_WALL_S {
        return Some(true);
    }
    day_start
        .is_some_and(|start| {
            start < now.saturating_add(accounts::OFFER_EXPIRING_S)
                && start.saturating_add(DAY_S) > now
        })
        .then_some(false)
}

/// HOW A CLAIMED LIMIT RESET ENDED, as the host read it: `accounts::ResetDone`
/// owning its reason, so that an outcome the lock kept out can be held
/// ([`ResetOwed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum ResetRead {
    /// The vendor's own line said the limits were reset, with the count it
    /// named.
    Used { left: Option<u32> },
    /// It said the limit had lifted by itself.
    Lifted,
    /// The reset was kept, and the word why.
    Kept(String),
    /// The attempt failed or its end was not read, and the word why.
    Failed(String),
}

impl ResetRead {
    fn done(&self) -> accounts::ResetDone<'_> {
        match self {
            ResetRead::Used { left } => accounts::ResetDone::Used { left: *left },
            ResetRead::Lifted => accounts::ResetDone::Lifted,
            ResetRead::Kept(why) => accounts::ResetDone::Kept(why),
            ResetRead::Failed(why) => accounts::ResetDone::Failed(why),
        }
    }
}

/// WHAT THE END OF A LIMIT RESET'S STEP IS READ AS ([`WorkerIdle::reset_tell`]):
/// what goes on record and what the loop is answered, from the ONE answer
/// line the dialog wrote to the transcript since the claim (`line`, classed
/// by `aterm_phase::reset_line` — never from the screen) and what the loop
/// says it did (`end`). `at` is the second the answer is made at: the
/// clearing's instant for a reset spent or a limit lifted. `None`: no line
/// yet and the loop will ask again. PURE.
///
/// * `Limits reset · …` alone is a reset SPENT; the two lifted lines are a
///   limit LIFTED with nothing spent; `Reset kept · …` is KEPT, with the
///   loop's word for why where it has one; the login line is a FAILURE for
///   this login.
/// * ANY OTHER LINE is a failure by its head (a line with no words in it,
///   `no-words`) — except where the loop kept
///   the reset and wrote no Enter: then the line is not this attempt's
///   answer (the wait row's line, after a row that opened nothing), and the
///   loop's word is recorded as KEPT.
/// * NO LINE at the loop's last ask: KEPT where no Enter was written, else a
///   failure, `unread` — a reset whose Enter went out and whose answer nobody
///   read is never recorded as spent.
fn reset_read(line: Option<&str>, end: &ResetEnd, at: i64) -> Option<(ResetRead, ResetOutcome)> {
    use aterm_phase::ResetLine;
    let kept = |otherwise: &str| {
        ResetRead::Kept(
            end.kept
                .map_or_else(|| otherwise.to_owned(), |why| why.to_string()),
        )
    };
    Some(
        match line.map(|text| (text, aterm_phase::reset_line(text))) {
            None if !end.last => return None,
            None if end.entered => (ResetRead::Failed("unread".to_owned()), ResetOutcome::Unread),
            None => (kept("no-line"), ResetOutcome::Kept),
            Some((_, ResetLine::Spent { left, asks })) => (
                ResetRead::Used { left },
                ResetOutcome::Spent {
                    asks,
                    left,
                    cleared_at: at,
                },
            ),
            Some((_, ResetLine::Lifted { asks })) => (
                ResetRead::Lifted,
                ResetOutcome::Lifted {
                    asks,
                    cleared_at: at,
                },
            ),
            Some((_, ResetLine::Kept)) => (kept("kept"), ResetOutcome::Kept),
            Some((_, ResetLine::Login)) => (
                ResetRead::Failed("login".to_owned()),
                ResetOutcome::Failed { login: true },
            ),
            Some((_, ResetLine::Other)) if !end.entered && end.kept.is_some() => {
                (kept("kept"), ResetOutcome::Kept)
            }
            Some((text, ResetLine::Other)) => {
                // The ledger keeps the line's head and no more
                // (`accounts::reset_outcome` cuts and bounds the reason); a
                // line with no words in it has this one.
                let said = if text.trim().is_empty() {
                    "no-words"
                } else {
                    text
                };
                (
                    ResetRead::Failed(said.to_owned()),
                    ResetOutcome::Failed { login: false },
                )
            }
        },
    )
}

/// THE OFFER A LIMIT RESET'S END PUTS ON RECORD (`accounts::note_offer`: the
/// count, and the deadline where one was read): only what was READ AS AN
/// OFFER — the count the vendor's own `reset` line names, with no deadline
/// (the line names the week's day, not the grant's end); for a reset spent
/// whose line names no count, one fewer than the confirmation showed; for a
/// reset kept, or a limit that lifted by itself, the count and the placed
/// deadline of a confirmation read exactly (`ResetEnd::offer`). AFTER A
/// FAILURE, NOTHING: the vendor's line may say the grant is gone or was used
/// (`This reset isn't available any more`, `That reset was already used`),
/// and an Enter whose answer nobody read may have spent it — the
/// confirmation's count from before is no offer then. The wall dialog's row
/// is never one: it names a day. PURE.
fn reset_offer(read: &ResetRead, end: &ResetEnd) -> Option<(u32, Option<i64>)> {
    match read {
        ResetRead::Used { left: Some(left) } => Some((*left, None)),
        ResetRead::Used { left: None } => end.offer.map(|(left, _)| (left.saturating_sub(1), None)),
        ResetRead::Lifted | ResetRead::Kept(_) => end.offer,
        ResetRead::Failed(_) => None,
    }
}

/// [`AccountKept::reset_owed`]: ONE LIMIT RESET'S OUTCOME AS THE LOOP WAS
/// ANSWERED IT ([`WorkerIdle::reset_tell`]), to be written — at once, or,
/// where the store's lock stayed held, at the session's next transaction on
/// the accounts state ([`WorkerIdle::reset_flush`]). It is written with the
/// second it was answered at, so the clearing the state stamps is the one
/// the loop was told (`ResetOutcome::Spent`'s `cleared_at`: the policy types
/// one continuation per clearing).
#[derive(Clone, Debug, PartialEq, Eq)]
struct ResetOwed {
    /// The roster's label the claim was made for.
    label: String,
    /// The wall's reset: with the label, what the claim is keyed on.
    until: i64,
    /// How the attempt ended.
    read: ResetRead,
    /// The offer recorded with it ([`reset_offer`]).
    offer: Option<(u32, Option<i64>)>,
    /// The unix second the loop was answered at.
    at: i64,
}

impl ResetOwed {
    /// Record it on `state` for session `sid` (`accounts::reset_outcome`,
    /// then the offer): the ledger's one row of the attempt — or nothing,
    /// where the claim is no longer this session's open one, and then no
    /// offer either.
    fn write(&self, state: &mut accounts::State, sid: &str, now: i64) -> Vec<accounts::Event> {
        let events = accounts::reset_outcome(
            state,
            &self.label,
            self.until,
            sid,
            &self.read.done(),
            self.at,
        );
        if let (false, Some((left, use_by))) = (events.is_empty(), self.offer) {
            accounts::note_offer(state, &self.label, left, use_by, self.at, now);
        }
        events
    }
}

/// What became of the reset outcome a session holds ([`WorkerIdle::reset_flush`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flushed {
    /// None was held.
    Nothing,
    /// On record, with its ledger row.
    Written,
    /// The claim in the state is not this session's open one: nothing
    /// recorded, and the outcome dropped.
    NotMine,
    /// The store's lock stayed held: still held here, for the next
    /// transaction.
    Held,
    /// The state could not be read or written as this build's: dropped.
    Refused,
}

/// [`AccountKept::advised`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct Advised {
    /// The label advised.
    to: String,
    /// The point it was advised at.
    point: AccountPoint,
}

/// [`AccountKept::moved`]: an account move made, verified, and not yet
/// served.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Moved {
    /// The label moved to.
    to: String,
    /// THE LOGIN BEFORE THE MOVE, raw ([`Snapshot::login`]): what a
    /// relaunch on exit of the moved process carries while the move has not
    /// served ([`account_way_back`]). The process the move replaced ran
    /// under it — or, where that process was itself one a move brought up
    /// that NOTHING HAS ANSWERED ([`Self::answered`]: no turn served, no
    /// bad answer struck), it is THAT move's login before: a login nobody has
    /// heard a word from is no way back, so this is the login before the
    /// first of the moves nobody answered. A move answered at all ends the
    /// chain: what came after it — a wall the account met, then the turns
    /// served once the wall ended (the first of which the loop tells, and
    /// the record is gone) — is that login's, and the move from it goes
    /// back to it. (A login its move found lost is struck signed out where
    /// that is told and recorded, and the way back carries no login so
    /// recorded while the other it could is not.)
    login: Option<String>,
    /// The process the move brought up (its pid and kernel start): the only
    /// one this record speaks of.
    pid: u32,
    start: String,
    /// A BAD ANSWER OF THIS MOVE WAS TOLD AND ITS STRIKE RECORDED
    /// ([`IdleHost::account_answered`]: a lost login, or a usage limit of the
    /// process's own), while its process held the tab: the move is answered,
    /// and a move made from that process carries no way back of this one's
    /// ([`Self::login`]). The record is kept for the exit of the process
    /// ([`account_way_back`]), until the loop says the account served.
    answered: bool,
    /// THE LOOP THAT ASKED FOR IT WAS TOLD TO WAIT, AND IS STILL RUNNING: the
    /// move was read as landed only afterwards, at one of that loop's asks
    /// ([`account_land`], [`Landing::Asked`]), so nothing has told it the
    /// move was made. It is told through the move itself: advised the same
    /// label once more, at the limit its session left
    /// ([`WorkerIdle::account_untold`]), its ask is answered `adopted` with
    /// nothing relaunched ([`WorkerIdle::account_move`]) — and then it
    /// counts the move, leaves the limit it stood at and tells what answers
    /// the account, as for any move it was told of at once. Dropped untold
    /// where the session has gone on by itself.
    untold: bool,
}

/// [`AccountKept::flying`]: an account move the rotation was committed for
/// and whose relaunch has not been seen to land.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Flying {
    /// The label moved to.
    to: String,
    /// Where the move was asked: what its ledger row says ([`Acts::account_made`]).
    at: AccountAt,
    /// The login before the move, raw ([`Moved::login`]'s, once it lands).
    login: Option<String>,
    /// The process before the move (its pid and kernel start): a read of
    /// that one says nothing of where the move landed.
    pid: u32,
    start: String,
}

/// [`AccountKept::lost`]: when this host first met ONE PROCESS'S lost
/// login.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LostSeen {
    /// The process (its pid and kernel start): the only one this record
    /// speaks of.
    pid: u32,
    start: String,
    /// The seat's clock at the first ask about it (Unix seconds).
    at: i64,
}

/// How many times more the recorder is asked in one limit episode
/// ([`AccountKept::wall_again`]): a transcript's limit row that lags the
/// screen, and a store's lock that stays held, are asked again at the
/// session's next decisions of that episode — these many, no more. And how
/// many times the loop is told to MAKE such a decision soon
/// ([`AccountKept::lagged`]).
const WALL_AGAIN: u8 = 3;

/// How far off the instant is that a host whose evidence lags the screen
/// answers ([`account_lagging`]): a moment. The loop decides no nearer than
/// its own floor (a minute, `IdleHost::account`), so a row a second late is
/// read a minute on — against a wait that would else run to the limit's
/// reset, hours or days off.
const WALL_AGAIN_AFTER: Duration = Duration::from_secs(30);

/// THE ADVICE OF A SESSION WHOSE EVIDENCE LAGS ITS SCREEN
/// ([`WorkerIdle::account_lags`]; `IdleHost::account`: "it answers a wait
/// for an instant a moment away … and the point decided then asks this once
/// more"): nothing advised becomes a wait until `again`, and a wait that
/// ends later is cut to it — the loop's limit is waited out as it would be,
/// and decided again then. A move or a resume decides the point itself and
/// is handed over as it is; a wait that ends sooner is not lengthened. PURE.
fn account_lagging(advised: Option<AccountAdvice>, again: Instant) -> Option<AccountAdvice> {
    match advised {
        None => Some(AccountAdvice::Wait { until: again }),
        Some(AccountAdvice::Wait { until }) => Some(AccountAdvice::Wait {
            until: until.min(again),
        }),
        other => other,
    }
}

/// How far off the instant is that a host answers while a limit reset is in
/// flight on the session's account ([`account_resetting`]): a moment, as
/// [`WALL_AGAIN_AFTER`]. The claim lapses by itself within
/// `accounts::RESET_CLAIM_S`, which bounds how often this is answered.
const RESET_AGAIN_AFTER: Duration = WALL_AGAIN_AFTER;

/// THE ADVICE OF A SESSION WHOSE ACCOUNT'S LIMIT RESET IS IN FLIGHT
/// (`accounts::reset_in_flight`; [`account_lagging`]'s sibling): NOBODY IS
/// MOVED OFF THAT ACCOUNT — the reset may be about to end its limits for
/// every session on it, and a rotation made meanwhile would take the machine
/// off an account just refilled. A move (alone or behind a due probe), a due
/// probe and nothing advised all become a wait until `again`, a moment away;
/// a wait that ends later is cut to it; a wait that ends sooner and a resume
/// are handed over as they are. The point decided at `again` asks once more:
/// by then the outcome is on record (a reset used or a limit lifted is
/// advised as a resume), or the claim has lapsed and the roster's own advice
/// stands again. At a plain turn end the policy acts on a move alone, so the
/// wait there is the move withheld and nothing else. PURE.
fn account_resetting(advised: Option<AccountAdvice>, again: Instant) -> Option<AccountAdvice> {
    match advised {
        Some(AccountAdvice::Wait { until }) => Some(AccountAdvice::Wait {
            until: until.min(again),
        }),
        Some(resume @ AccountAdvice::Resume { .. }) => Some(resume),
        // A move, a due probe with or without one behind it, nothing — and
        // any advice a later build adds: held, never acted on.
        _ => Some(AccountAdvice::Wait { until: again }),
    }
}

/// The recorder's word where the accounts store's lock stayed held past its
/// bounded wait (`accounts::Recorded::lock_held`): nothing was read, and the
/// recorder is asked again ([`wall_asked_again`]).
const WALL_LOCK_HELD: &str = "lock-held";

/// WHETHER THE RECORDER'S WORD IS ONE THAT ASKING AGAIN CAN CHANGE
/// ([`AccountKept::wall_again`]): `no-wall` — the screen shows a usage limit
/// and the transcript's row is not written yet — and [`WALL_LOCK_HELD`].
/// Every other word is final for the episode: recorded, on record, no row of
/// the roster's, no stored login, another process's row, no environment, a
/// state this build may not write. PURE.
fn wall_asked_again(word: &str) -> bool {
    matches!(word, "no-wall" | WALL_LOCK_HELD)
}

/// A PARK'S COMMIT STOPPED THE SESSION'S WORKER — a FACT on the session,
/// under its lock (round eleven, the review of 173b163e2). Recorded by
/// [`HostHandle::suspend`] before its stop reaches the worker, and read by
/// the worker WHENEVER it decides, however late it is scheduled: it was read
/// as [`Switches::parked`], which a failed Commit's rollback lifts
/// ([`HostHandle::resume_restored`]) — a worker the Commit stopped that
/// looked only after the rollback read a stop for good, settled what it
/// owed, and the crashed agent's relaunch was lost with no worker coming
/// back for a tab at its shell (the F13 loss). The decision and the rollback
/// meet here, in either order ([`HostHandle::relaunch_stranded`]). Marked and
/// rolled back only where a seamless update runs (unix): elsewhere `Stopped`
/// and `TakenUp` are never made. A worker the Commit stopped that was handed
/// no exit decides nothing here: it ends leaving its agent's exit to the
/// host ([`Kept::unwatched`]), which a host supervising again hands — the
/// mark is then the host's to reset (round thirteen).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(any(unix, test)), allow(dead_code))]
enum Committed {
    /// No park's Commit stopped the session's worker: a stop is for good (its
    /// tab closed, the policy, the shutdown), or its agent is back.
    #[default]
    No,
    /// A Commit stopped the worker, and neither it has decided nor a
    /// rollback come since.
    Stopped,
    /// The worker the Commit stopped decided, its agent not back: what
    /// [`Kept::owed`] holds stays owed — the successor's from the handoff
    /// layout, or this host's again when the Commit fails. Also what a
    /// worker told `TakenUp` leaves when the restored queue refused it,
    /// closed by a quit: the next launch's, from the quit layout
    /// ([`take_up_after_rollback`]).
    Stranded,
    /// The Commit failed and its rollback came before the worker decided:
    /// the worker takes what it still owes up itself as it decides, on the
    /// restored queue, its tab named by these words (`in tab 2`).
    TakenUp(String),
}

/// How the worker whose wait a stop ended reads it, its agent not back
/// ([`Kept::stopped`]).
enum Stop {
    /// A stop for good, or nothing owed: what was owed is settled.
    ForGood,
    /// A Commit's, not rolled back: the relaunch stays owed.
    ByCommit,
    /// A Commit's that has rolled back: this relaunch is taken up now.
    RolledBack(Box<RestoredAgent>),
}

impl Kept {
    /// What the stop that ended session `sid`'s worker's wait was, its agent
    /// not back ([`Committed`]), moved on as the worker decides.
    fn stopped(&mut self, sid: &str) -> Stop {
        match (std::mem::take(&mut self.commit), self.owed.take()) {
            (Committed::Stopped, Some(owed)) => {
                self.owed = Some(owed);
                self.commit = Committed::Stranded;
                Stop::ByCommit
            }
            (Committed::TakenUp(place), Some(owed)) => Stop::RolledBack(Box::new(RestoredAgent {
                sid: sid.to_string(),
                snap: owed.snap,
                place,
                cold: false,
                asked: owed.asked,
            })),
            _ => Stop::ForGood,
        }
    }
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
    /// It ended watching beside a proven self-claim ([`WorkerJob::beside`]):
    /// stopped, never a `host.held` entry — the session is supervised by its
    /// agent, and the next pass that finds the agent starts a worker again.
    SelfClaimed,
}

/// A tab's publication as read under its timeline lock with the roster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FollowStamp {
    group: i32,
    reader: Option<Program>,
    rev: u64,
    /// The reads of its screen that moved under a verdict that stood while
    /// its agent was not busy (`AgentPublication::screen_moves`): an in-app
    /// `/clear` moves no verdict, only this (measured on Claude Code 2.1.300:
    /// `agent=idle agent_rev=1` before and after it, the record rewritten
    /// within 0.6 s of the Enter), and its record is followed for it
    /// ([`follow_entries`]).
    screen: u64,
}

impl FollowStamp {
    /// Whether `self` moved from `before` in what is not its screen alone: the
    /// group, the reader or the verdict ([`follow_entries`] follows such a
    /// move at once, a screen's alone at most once per settle).
    fn moved_beyond_screen(self, before: Self) -> bool {
        Self {
            screen: before.screen,
            ..self
        } != before
    }
}

type RosterFn = dyn Fn() -> Vec<(String, Program, FollowStamp)> + Send + Sync;
/// Every live session's own Autopilot word ([`Hooks::autopilot`]), by sid: a
/// session with none is not in the map.
type WordsFn = dyn Fn() -> HashMap<String, Autopilot> + Send + Sync;
type WantedFn = dyn Fn(&str) -> bool + Send + Sync;
/// [`Acts::offered`]: for tab `sid`, `None` where the explicit restart would
/// not be carried, else `Some(promised)`.
type OfferedFn = dyn Fn(&str) -> Option<bool> + Send + Sync;
/// Whether a session is still its worker's ([`Hooks::still_wanted`]):
/// `(sid, group)`.
type StillFn = dyn Fn(&str, i32) -> bool + Send + Sync;
/// Whether a tab's agent still holds it ([`Acts::holds`]): `(sid, group)`.
type HoldsFn = dyn Fn(&str, i32) -> Option<bool> + Send + Sync;
/// What the upgrade reads of a session ([`upgrade_drive::due`]).
type DueFn = dyn Fn(&str) -> Due + Send + Sync;
type BodyFn = dyn Fn(&WorkerJob) -> BodyEnd + Send + Sync;
type BadgeFn = dyn Fn(&str, Option<&str>) + Send + Sync;
/// The loop's next try, told to the window ([`IdleHost::waiting`]): `(sid,
/// plan)` — the unix second and its local clock — or `None` once no wait
/// stands.
type WaitingFn = dyn Fn(&str, Option<crate::presence::RetryPlan>) + Send + Sync;
/// What the loop tells the window of the harness's glass ([`Hooks::fact`]):
/// `(sid, fact)` — an act it took, or the task's face (`None`: unknown).
type FactFn = dyn Fn(&str, crate::HarnessFact) + Send + Sync;

/// A Codex's wall reset as the worker placed it, told to the window
/// ([`IdleHost::place_reset`]): `(sid, placement)`, or `None` once the loop
/// that placed it ended.
type PlacedFn = dyn Fn(&str, Option<crate::presence::PlacedReset>) + Send + Sync;
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
/// The kernel's word that the process a snapshot names has ended
/// ([`Acts::exited`]): waited for at most the duration given.
type ExitedFn = dyn Fn(&Snapshot, Duration) -> bool + Send + Sync;
/// THE EXPLICIT RESTART'S STAMP naming the agent that left ([`Acts::asked`]):
/// `(sid, snapshot)` to its cause.
type AskedFn = dyn Fn(&str, &Snapshot) -> Option<restart_stamp::Cause> + Send + Sync;
/// One relaunch attempt on an agent's exit ([`relaunch::after_exit`]):
/// `(sid, grace, snapshot, what the exit left, upgrade, the act's cause)`.
/// Tab `sid`'s copy of its proven self-claim's record ([`Acts::self_copy`]).
type SelfCopyFn = dyn Fn(&str) -> Option<self_claimed::Copy> + Send + Sync;
/// The crash rule's relaunch ([`Acts::self_relaunch`]): `(sid, grace, snap,
/// resume, asked)`, the step word.
type SelfRelaunchFn = dyn Fn(&str, u32, &Snapshot, &self_claimed::Resume, Option<restart_stamp::Cause>) -> String
    + Send
    + Sync;
type RelaunchFn = dyn Fn(&str, u32, &Snapshot, &ExitRecord, bool, Option<restart_stamp::Cause>) -> String
    + Send
    + Sync;
/// Whether the harness's own restart ended the agent that left
/// ([`Acts::restarted`]): `(sid, agent, pid)`.
type RestartedFn =
    dyn Fn(&str, aterm_agent::harness::upgrade::Agent, Option<u32>) -> bool + Send + Sync;
/// One attempt at carrying that restart on ([`Acts::carry`]): `(sid, grace,
/// agent, pid)`.
type CarryFn =
    dyn Fn(&str, u32, aterm_agent::harness::upgrade::Agent, Option<u32>) -> String + Send + Sync;
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
/// ([`ADOPTED_CLAIM_GRACE`]) held its worker off at exactly that moment, the
/// SIGTERMed agent then exited, the tab was at its shell and no worker ever
/// started — the conversation the harness itself ended was never relaunched.
/// (Since round eight, #501, that worker WATCHES the grace and carries the
/// restart on as its agent's exit; the look again still carries one whose
/// watcher stood down for another supervisor's claim.)
const CARRY_SUPERVISED: &str = "wait:supervised";
/// The owner's view of the upgrades ([`Hooks::upgrade_view`]): `true` looks
/// again and answers what the next look waits for ([`ViewNext`]), `false`
/// stands it down. Beside it, the tabs whose worker the host parks behind
/// another supervisor's claim ([`ViewLook::claimed`]), which the look reads
/// as held by that claim (`upgrade_drive::View::set_claimed`), and then the
/// tabs whose Autopilot is Off ([`ViewLook::off`]), which it reads as held
/// by the Off (`upgrade_drive::View::set_off`), and last the tabs whose agent
/// its own switch holds off ([`ViewLook::agent_off`]), which it reads as held
/// by that switch (`upgrade_drive::View::set_agent_off`).
type ViewFn = dyn Fn(bool, &[String], &[String], &[String]) -> ViewNext + Send + Sync;

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
/// An agent's memory, measured now from its tab and pid ([`Acts::memory`]).
type MemoryFn = dyn Fn(&str, u32) -> Option<AgentMemory> + Send + Sync;
/// Whether the conversation a tab's agent holds has a task
/// ([`upgrade_drive::Tasked::of_snapshot`], the tab's supervisor's own turns
/// none), read on from where the worker's last read of it ended, in the
/// agent's own records: `(sid, its snapshot, memo)`.
type TaskedFn = dyn Fn(&str, &Snapshot, &mut upgrade_drive::Tasked) -> Option<bool> + Send + Sync;
/// The reset the standing usage limit of the conversation a snapshot names
/// names, read in its agent's own records
/// ([`upgrade_drive::named_limit_reset`]; [`Acts::limit_reset`]).
type LimitResetFn = dyn Fn(&Snapshot) -> Option<u64> + Send + Sync;
/// The tail of the transcript the Claude Code a snapshot names writes now
/// ([`upgrade_drive::snapshot_transcript_tail`]; [`Acts::transcript_tail`]).
type TranscriptTailFn = dyn Fn(&Snapshot) -> Option<String> + Send + Sync;
/// The accounts recorder at a session's usage wall ([`Acts::account_wall`]):
/// `(sid, its agent, whether that agent's SETTINGS leave it its stored
/// login)` — its route the default one, no settings source and no profile
/// store giving it another ([`WorkerIdle::settled`]); asked for only where
/// the answer needs it — answering the recorder's word and, where the wall is
/// an account's, that account's roster label.
type AccountWallFn = dyn Fn(&str, &WalledAgent<'_>, &dyn Fn() -> bool) -> (&'static str, Option<String>)
    + Send
    + Sync;
/// What a session's screen says of its account's usage, recorded
/// ([`Acts::account_usage`]): `(sid, the agent, its settings side, what the
/// screen says, accounts listed and not off)`.
type AccountUsageFn = dyn Fn(&str, &WalledAgent<'_>, &dyn Fn() -> bool, &UsageSeen, bool) -> &'static str
    + Send
    + Sync;
/// A session the accounts recorder keeps nothing of any more
/// ([`Acts::account_gone`]).
type AccountGoneFn = dyn Fn(&str) + Send + Sync;
/// Where the accounts live for this host, and its clock ([`Acts::account_seat`]).
type AccountSeatFn = dyn Fn() -> Option<AccountSeat> + Send + Sync;
/// A process's exec environment, `KEY=value` entries ([`Acts::account_env`]).
type AccountEnvFn = dyn Fn(u32) -> Option<Vec<String>> + Send + Sync;
/// Whether an agent's settings files and the vendor's profile store leave it
/// its stored login, read now from its `(pid, argv, directory)`
/// ([`Acts::account_settings`]).
type AccountSettingsFn = dyn Fn(u32, &[String], &str) -> bool + Send + Sync;
/// [`Acts::tap_born`].
type TapBornFn = dyn Fn(u32) -> Option<i64> + Send + Sync;
/// The label of the account move whose restart signalled the agent `pid` of
/// tab `sid` ([`Acts::account_restart`]): `(sid, pid)`.
type AccountRestartFn = dyn Fn(&str, u32) -> Option<String> + Send + Sync;
/// An account move MADE, written where the session's loop reads it
/// ([`Acts::account_made`]): `(sid, the label moved to, where it was asked)`.
type AccountMadeFn = dyn Fn(&str, &str, AccountAt) + Send + Sync;
/// The login a relaunch with no process to read carries
/// ([`Acts::account_cold`]), for the snapshot the layout carried.
type AccountColdFn = dyn Fn(&Snapshot) -> Option<String> + Send + Sync;
/// The reset a Codex's wall words name, placed in the zone that Codex
/// printed them in: `(pid, $CODEX_HOME, words, now)`
/// ([`upgrade_drive::codex_wall_reset`]; [`Acts::codex_reset`]).
type CodexResetFn = dyn Fn(u32, &Path, &str, u64) -> Option<u64> + Send + Sync;
type BackoffFn = dyn Fn(usize) -> Duration + Send + Sync;
type PauseFn = dyn Fn(Duration) -> Duration + Send + Sync;
/// One look of the disk watch over the supervised sessions ([`Hooks::disk`]).
type DiskFn = dyn Fn(&[String]) + Send + Sync;
/// What a host thread tells the window ([`Hooks::note`], [`HostNote`]).
type NoteFn = dyn Fn(HostNote) + Send + Sync;

/// WHAT THE HOST'S THREADS TELL THE WINDOW ABOUT THEMSELVES, in facts and not in
/// alerts (owner rules, 2026-09-28: an alert persists only where the owner can
/// act, and says why and what; a fact that mends itself is a record, and a line
/// of the exceptions digest only once it has outlasted its horizon). The window
/// ([`crate::App::note_harness_host`]) writes the record, holds the claim for the
/// digest and withdraws it when the fact ends; a host thread never marks a tab or
/// posts a row. Every "begins" has an "ends", and a worker's exit ends its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostNote {
    /// The tab's supervisor has failed past its restart budget and is being
    /// started again after `pause`, on a growing pause; `why` is the last
    /// failure. Told at every failure, so the window can tell a supervisor that
    /// stopped failing (its run outlived the pause) from one that did not.
    SupervisorFailing {
        sid: String,
        why: String,
        pause: Duration,
    },
    /// That supervisor runs steadily again, or its worker ended.
    SupervisorSteady { sid: String },
    /// The disk watch left the home volume below the automatic floor
    /// [`DISK_SHORT_LOOKS`] passes running — whether or not either pass freed
    /// anything ([`disk_pass_outcome`]); `hogs` are the largest build
    /// directories it can NOT reclaim, each a line naming its size, what it
    /// is and what frees it or keeps it, fitted to a detail line
    /// (`aterm_agent::harness::disk::Hog::line_within`), biggest first.
    DiskLow {
        free_gib: u64,
        floor_gib: u64,
        hogs: Vec<String>,
    },
    /// A `[disk]` value aterm cannot read turns every removal off, and
    /// [`DISK_SHORT_LOOKS`] looks running found the home volume below the
    /// automatic floor still read, `floor_gib` (aterm.toml's `auto_free_gib`,
    /// or its default where the file writes none; [`disk_look_without_pass`]):
    /// no pass and no search ran, and none will until the owner fixes the
    /// value.
    /// Its line is not [`Self::DiskLow`]'s: it ends when space is back
    /// ([`Self::DiskPlenty`]) or when the settings read again
    /// ([`Self::DiskSettingsRead`]).
    DiskLowCleanupOff { free_gib: u64, floor_gib: u64 },
    /// The volume is at or above the floor (before a pass, or measured after
    /// it): said at EVERY such look (the window's end is idempotent),
    /// so a low-disk line of either form an earlier process raised ends too.
    DiskPlenty,
    /// aterm.toml does not say what the owner meant by a `[disk]` removal
    /// knob, so every removal is off until it is fixed
    /// (`aterm_agent::harness::cli::DiskReading::off_by_fault`); `notes` are
    /// the reader's notes on the values that turned removal off, or the
    /// file's one note where none of its values can be read
    /// ([`disk_settings_note`]); none quotes a value. Said at every look while
    /// it holds.
    DiskSettingsOff { notes: Vec<String> },
    /// No `[disk]` value turns removal off: said at EVERY look that reads
    /// the settings so (the window's end is idempotent), so a `disk.settings`
    /// line, and a [`Self::DiskLowCleanupOff`] line, that an earlier process
    /// raised end too.
    DiskSettingsRead,
    /// The live upgrade cannot run at all in this tab (`step` names why: its
    /// state folder, `dir`, cannot hold the lock).
    UpgradeCannotRun {
        sid: String,
        step: String,
        dir: Option<PathBuf>,
    },
    /// The upgrade runs in this tab (said at a run's first fine step and when
    /// it recovers), or its worker ended.
    UpgradeRuns { sid: String },
    /// A rating survey the loop cannot close is open in the tab.
    SurveyStuck {
        sid: String,
        kind: aterm_agent::supervise::StuckSurvey,
        why: String,
    },
    /// No survey is open in the tab (the loop's first look saw none, or the card
    /// left), or its worker ended.
    SurveyEnded { sid: String },
    /// The tab's resume loop found a point only a person can answer
    /// ([`IdleHost::exception`]): `Some` while it stands, `None` when it is over
    /// (the worker works, the condition left the screen, the loop or its worker
    /// ended). Told once per change; the window keeps one per tab and gives
    /// `Family::Resume` its whole answer at every change, and asks the owner
    /// on the glass for a `Now` one no escalation of the loop's carries.
    ResumeException {
        sid: String,
        standing: Option<aterm_agent::supervise::policy::exception::Exception>,
    },
    /// The stall canary's second rung ([`IdleHost::stall_unread`]): the tab's
    /// screen no reader names has stood for `Some(minutes)` while Claude's own
    /// title says idle (it read busy on background work, a wait this build does
    /// not classify, or it is a dialog this build does not read), or `None`
    /// when it is over.
    ResumeUnread { sid: String, minutes: Option<u64> },
    /// What the tab's relaunch has to say about itself ([`RelaunchSay`]): a fact
    /// in words, never a mark on the tab except the one case a person alone can
    /// mend and can be told the command for.
    Relaunch { sid: String, said: RelaunchSay },
    /// What the Claude Code accounts as a whole say ([`AccountsSaid`]): the
    /// machine's fact and no tab's. Told once per change ([`accounts_tell`]);
    /// the window gives `Family::Accounts` its two lines from it.
    Accounts(AccountsSaid),
    /// The tab's supervisor informs a person ([`IdleHost::inform`]): a
    /// record the window names by the tab's place.
    Inform { sid: String, text: String },
    /// The tab's supervisor warns a person ([`IdleHost::warn`], the memory
    /// ceiling's): a warning row the window names by the tab's place.
    Warn { sid: String, text: String },
    /// What a layout's leaf carries of the agent the host publishes for the
    /// tab moved — its conversation (an in-app `/clear`), its process, or
    /// none any more ([`tell_published`]): a fact for the window's crash
    /// journal, which captures again. Nothing is said.
    AgentPublished { sid: String },
    /// The usage tap of the tab's Claude Code went silent (`silent`: its
    /// process served a turn and wrote no reading at two decided points;
    /// [`WorkerIdle::usage_tap`]) or is heard again, or the worker ended
    /// (`silent` false). The window keeps one line for the machine while any
    /// tab stands silent, raised at most once a day.
    UsageTap { sid: String, silent: bool },
}

/// WHAT THE HOST SAYS OF THE ACCOUNTS AS A WHOLE ([`HostNote::Accounts`];
/// the look is [`accounts_said`]): which of the digest's two lines stands —
/// the roster that is refused, the accounts that are signed out — and never
/// both, since a refused roster names no account. Only labels cross: the
/// refusal's reason names the file, and is `aterm harness accounts`' to say.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AccountsSaid {
    /// No roster, none listed, `[harness] accounts = "off"` with `logins`
    /// off (or over a roster with no email to renew by): every line ends.
    Nothing,
    /// The roster does not admit: its line stands, the signed-out line ends.
    Refused,
    /// The roster admits (or, with none and `[harness] usage_notes` on, the
    /// machine's own row stands for it): its line ends, and each of these
    /// lines stands while what it names is there (labels, in roster order):
    Listed {
        /// The signed-out line: accounts whose login is known signed out.
        signed_out: Vec<String>,
        /// The credits line: accounts usage credits carry
        /// (`accounts::credits_noted`).
        credits: Vec<String>,
        /// The week's line: accounts whose week is nearly used with no
        /// credits, each with the vendor's word for why its credits are off
        /// (`accounts::windows_full`).
        window_full: Vec<(String, String)>,
        /// The due line: renewable logins due or ended
        /// (`accounts::renew_due`), the machine's batch to renew.
        due: Vec<String>,
        /// The warning that no renewable login is left working within a day
        /// (`accounts::none_working_within`).
        none_soon: bool,
        /// The line of renewals that answered another account
        /// (`accounts::wrong_accounts`).
        wrong: Vec<String>,
    },
}

impl AccountsSaid {
    /// Whether a line of the digest stands for this: the one a look that
    /// reads the same again still has something to say for.
    fn stands(&self) -> bool {
        match self {
            Self::Nothing => false,
            Self::Refused => true,
            Self::Listed {
                signed_out,
                credits,
                window_full,
                due,
                none_soon,
                wrong,
            } => {
                !signed_out.is_empty()
                    || !credits.is_empty()
                    || !window_full.is_empty()
                    || !due.is_empty()
                    || *none_soon
                    || !wrong.is_empty()
            }
        }
    }

    /// What a roster that admits (or the machine's own row standing for
    /// none) says, from the state and the logins read whole at `now`
    /// ([`accounts_said`], [`WorkerIdle::account_advice`]): who is signed
    /// out, whose usage credits carry their sessions, whose week is nearly
    /// used with no credits, and the login lifecycle's lines.
    fn listed(
        roster: &accounts::Roster,
        state: &accounts::State,
        logins: &accounts::Logins,
        now: i64,
    ) -> AccountsSaid {
        let enrolled = accounts::enrolled(roster);
        AccountsSaid::Listed {
            signed_out: accounts::signed_out(roster, state, &enrolled, now),
            credits: accounts::credits_noted(roster, state, now),
            window_full: accounts::windows_full(roster, state, now),
            due: accounts::renew_due(roster, logins, state, &enrolled, now),
            none_soon: accounts::none_working_within(
                roster,
                logins,
                state,
                &enrolled,
                now,
                accounts::NONE_SOON_S,
            ),
            wrong: accounts::wrong_accounts(roster, logins),
        }
    }
}

/// WHAT A TAB'S RELAUNCH SAYS ([`HostNote::Relaunch`]), by the owner's rule
/// (2026-09-28: an alert persists only where the owner can act, and says why and
/// what; a fact that mends itself is a record):
///
/// * [`Self::Failing`]: the relaunch keeps missing and is still tried. A record,
///   never a mark. Only when the cause is one a person clears (a hold, a job in
///   front of the prompt, the conversation open elsewhere) and it lasts an hour
///   is it a line of the digest.
/// * [`Self::Limited`]: the owner's own `[harness] relaunch = false`. A record.
/// * [`Self::Cannot`]: no relaunch can be made and only a person can bring the
///   agent back. With the command to run it is the tab's mark (the ask names why
///   and the one command); without one, a line of the digest.
/// * [`Self::Ended`]: an agent runs in the tab again (or nothing is owed any
///   more): every line above ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RelaunchSay {
    Failing {
        /// The cause a person clears, when it is one.
        blocked_by: Option<&'static str>,
        /// The relaunch is aterm's own restart's, carried.
        restarted: bool,
    },
    Limited {
        why: String,
    },
    Cannot {
        /// The refusal in a person's words.
        reason: String,
        restarted: bool,
        /// `claude --resume <id>` (with its carried flags) when one is known.
        command: Option<String>,
    },
    Ended,
}

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
    /// THE SESSIONS' OWN AUTOPILOT WORDS (`meta set autopilot`, the chip,
    /// Settings ▸ Harness; `docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §2.1), every live session's — an agent's or not, so a session whose
    /// agent just exited is still known Off ([`OffSessions`]). Read once a
    /// pass, beside the roster, and by the restored queue and the in-flight
    /// carry before each step. Live, [`words_of`]: the store's snapshot, then
    /// each session's metadata, a leaf lock — never under the host's.
    pub(crate) autopilot: Arc<WordsFn>,
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
    /// Where the loop's NEXT TRY goes ([`IdleHost::waiting`], the tab's
    /// retry plan): the window, which shows it on the tab beside an API wall
    /// (`Wake::HarnessWait`). A fact only — nothing acts on it.
    pub(crate) waiting: Arc<WaitingFn>,
    /// WHAT THE HARNESS JUST DID, AND THE TASK'S FACE
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.2.3, §9.4.4): the
    /// loop's acts ([`IdleHost::act_taken`]) and its task's face
    /// ([`IdleHost::task_face`]), and the host's own "unknown" after each run
    /// of the body and at a worker's end — the window, which shows them on the
    /// Autopilot chip (`Wake::HarnessFact`). Facts only: nothing acts on them.
    pub(crate) fact: Arc<FactFn>,

    /// Where a Codex's wall reset goes once the worker placed it in that
    /// Codex's own zone ([`IdleHost::place_reset`]): the window, which counts
    /// the tab's wall down to it (`Wake::HarnessReset`). A fact only —
    /// nothing acts on it.
    pub(crate) placed: Arc<PlacedFn>,
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
    /// What the workers and the disk watch tell the window about themselves
    /// ([`HostNote`]): handed to the app loop as `Wake::HostNote`.
    pub(crate) note: Arc<NoteFn>,
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
    /// THE KERNEL'S WORD THAT THE AGENT ENDED, waited for at most the time
    /// given: the very process the snapshot names (its pid AND its kernel
    /// start) is gone — or ended and not reaped yet, which is an end
    /// (`upgrade_wake::wait_exit`'s `NOTE_EXIT`; a zombie answers `ESRCH`).
    /// The exit watch's ([`watch_exit`]), for an exit no pass of the host
    /// would see soon.
    pub(crate) exited: Arc<ExitedFn>,
    /// THE EXPLICIT RESTART'S STAMP naming the agent that left
    /// ([`restart_stamp::lookup`]: `aterm ctl restart` writes it BEFORE it
    /// signals, under the harness state directory, so every reader of the
    /// exit — this instance's, or a successor's after a Commit — finds it):
    /// the act's cause, or `None`. A stamp whose process is gone and is not
    /// this exit's, or one past `STALE_S`, is swept as it is found. Read at
    /// every exit ([`on_agent_left`]).
    pub(crate) asked: Arc<AskedFn>,
    /// Whether the explicit restart would be carried for tab `sid`'s agent
    /// now — THE ONE PREDICATE a stall's words choose by
    /// ([`crate::restart_act::offered`], over the tab's live context and its
    /// published stall's leader) — and, where it would (`Some`), whether its
    /// relaunch on the conversation is promised: the loop's frozen mail and
    /// warning name the act where it says so ([`WorkerIdle::restart_offered`]),
    /// as the server's attention entry, the menu row, the band and the
    /// refusals do.
    pub(crate) offered: Arc<OfferedFn>,
    pub(crate) relaunch: Arc<RelaunchFn>,
    /// ATERM'S COPY of a proven self-claimed codex-alab's session record for
    /// tab `sid` ([`crate::self_claim::copy_for`]: read only while its
    /// identity was live), whatever identity it names: the exit's reader
    /// matches it to the snapshot ([`self_claimed_exit`]).
    pub(crate) self_copy: Arc<SelfCopyFn>,
    /// THE CRASH RULE'S RELAUNCH of such an agent, on the line its copy
    /// validated ([`upgrade_drive::self_claimed_after_exit`]).
    pub(crate) self_relaunch: Arc<SelfRelaunchFn>,
    /// Whether the harness's OWN restart ended the agent that left — the
    /// upgrade's, or the restart in place's, left in flight by its step
    /// ([`relaunch::restarted`]: both lanes' by the snapshot's pid, `None`
    /// none) — read as the exit is seen, before whose exit it was.
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
    /// are none ([`WorkerIdle::taskless`]) — read in the agent's own Claude
    /// records: where its snapshot read its record (its `CLAUDE_CONFIG_DIR`
    /// resolved as Claude Code resolves it), as every relaunch reads them.
    pub(crate) tasked: Arc<TaskedFn>,
    /// The usage limit's reset the conversation's transcript names, past or
    /// to come ([`WorkerIdle::limit_until`]): asked by the loop where its
    /// limit episode read none of its own (design record C9), and where a
    /// notice names a clock time but no day — read, as every reader of the
    /// agent's limit and wall here, in its own records
    /// (`upgrade_drive::Records::of_snapshot`): read in the user's home, an
    /// agent under another Claude home never had its reset named.
    pub(crate) limit_reset: Arc<LimitResetFn>,
    /// THE TRANSCRIPT THAT CONFIRMS A WALL ([`WorkerIdle::transcript_tail`],
    /// H6-HARNESS-1): the tail of the conversation the snapshot's process
    /// writes now, found from that process as the footer finds it (its own
    /// `CLAUDE_CONFIG_DIR` or `HOME`). It reads; it judges nothing.
    pub(crate) transcript_tail: Arc<TranscriptTailFn>,
    /// THE SESSION'S OWN USAGE WALL, RECORDED FOR ITS ACCOUNT (the accounts
    /// design, §5 "What counts as an observation" and §12 slice 2;
    /// [`WorkerIdle::limited`]): the Claude Code the worker keeps, at the
    /// open of its loop's limit episode, handed to
    /// `accounts::record_wall` with the facts of its process
    /// ([`account_wall_live`]). It records; nothing is typed, no session is
    /// touched and nothing is shown. The word is the log's.
    pub(crate) account_wall: Arc<AccountWallFn>,
    /// WHAT THE SESSION'S SCREEN SAYS OF ITS ACCOUNT'S USAGE, RECORDED FOR THE
    /// ACCOUNT (accounts v2 S5, [`WorkerIdle::usage_seen`]): usage credits
    /// carrying it and the vendor's warning that a window fills, handed to
    /// `accounts::record_usage` with the facts of its process and the instant
    /// they were true ([`account_usage_live`]). It records; nothing is typed,
    /// no session is touched. The word is the log's (`usage` where something
    /// was recorded).
    pub(crate) account_usage: Arc<AccountUsageFn>,
    /// THE SESSION IS OVER for the accounts recorder (`accounts::record_gone`):
    /// its Claude Code left and nothing brings it back — an exit left alone,
    /// a launch that ended, one that cannot come back ([`on_agent_left`]) —
    /// or its tab closed ([`forget_closed`]). Never told of an agent the
    /// harness itself relaunches: that one comes back under the same session.
    pub(crate) account_gone: Arc<AccountGoneFn>,
    /// WHERE THE ACCOUNTS LIVE, AND THE TIME ([`AccountSeat`]: the roster's
    /// file, the harness state root the store is under, the home Claude's
    /// transcripts are under, now) — every PLACE the account's move reads,
    /// so a test hands it a temp roster and store and a fixed second. `None`
    /// where this host has no home, no state root or no clock: then nothing
    /// is advised and nothing is read.
    pub(crate) account_seat: Arc<AccountSeatFn>,
    /// THE AGENT'S OWN EXEC ENVIRONMENT, by its pid (`KERN_PROCARGS2`): which
    /// login it runs under, and whether it authenticates by a stored login at
    /// all (`accounts::ProcessLogin::of_env`). `None`: unreadable. Asked only
    /// where a roster lists an account.
    pub(crate) account_env: Arc<AccountEnvFn>,
    /// WHETHER THE AGENT'S SETTINGS LEAVE IT ITS STORED LOGIN: no settings
    /// source Claude Code reads for it — the user's, the project's, the
    /// managed ones, a `--settings` — gives it a credential, a profile or an
    /// `apiKeyHelper`, every source there could be read, and the vendor's
    /// profile store holds no config
    /// ([`crate::harness_netprobe::stored_login_of_agent`]: the route's own
    /// gatherer, handed to `accounts::ProcessLogin::with_settings` and
    /// `with_profile_store`). `false` makes it no stored login, whatever its
    /// environment says. Asked only where a roster lists an account, once
    /// per agent process ([`WorkerIdle::settled`]).
    pub(crate) account_settings: Arc<AccountSettingsFn>,
    /// WHEN THE KERNEL STARTED PROCESS `pid`, Unix seconds
    /// (`aterm_uds::process::read_process_birth`): a usage tap's file older
    /// than its process is a reused pid's, never its own
    /// ([`WorkerIdle::usage_tap`]). `None`: no such record.
    pub(crate) tap_born: Arc<TapBornFn>,
    /// THE ACCOUNT MOVE WHOSE RESTART SIGNALLED THE AGENT, by its label
    /// (`relaunch::account_restart`: the restart's own record, in flight or
    /// just stopped): what tells a move's relaunch LEFT IN FLIGHT — carried
    /// by the host, under the target's login — from one never begun, whether
    /// or not the signalled process has gone yet ([`WorkerIdle::account_move`]).
    /// `None`: nothing of an account move's signalled that process.
    pub(crate) account_restart: Arc<AccountRestartFn>,
    /// AN ACCOUNT MOVE THE HOST LANDED ITSELF is written to the session's
    /// approval ledger as the loop writes one it made (`account-rotate@v1`,
    /// `typed`: [`account_made_at`]) — the loop that takes the relaunched
    /// agent reads it there, counts it and answers it. Only for a move the
    /// host's carry landed BETWEEN TWO LOOPS ([`account_land`],
    /// [`Landing::Carried`]): the loop that asked was told to wait and has
    /// ended, and no loop is running that appends to that file.
    pub(crate) account_made: Arc<AccountMadeFn>,
    /// THE LOGIN A RELAUNCH WITH NO PROCESS TO READ CARRIES (the accounts
    /// design, §6: a cold restore carries `active`; [`account_cold_at`]): the
    /// machine's proven active account's, where two or more are enrolled and
    /// the leaf's Claude home is the default one. `None`: nothing is added.
    /// It reads the roster, the state WITHOUT its lock, the names of the
    /// default home's conversations and the reopened shell's exec
    /// environment; with no roster, the roster's absence alone.
    pub(crate) account_cold: Arc<AccountColdFn>,
    /// The reset a Codex's usage wall names, placed as its upgrade places it
    /// ([`WorkerIdle::place_reset`]): asked by the loop wherever it reads
    /// such a wall's words (the review of 2026-09-30).
    pub(crate) codex_reset: Arc<CodexResetFn>,
    /// The live upgrade's clocks held through the loop's limit episode
    /// ([`upgrade_drive::hold_clock`], [`WorkerIdle::limited`]).
    pub(crate) hold: Arc<HoldFn>,
    /// The route a Claude Code agent's API requests take, read-only from
    /// its exec environment and every settings source Claude Code reads for
    /// it ([`crate::harness_netprobe::route_of_agent`]): only the default
    /// route is measured ([`WorkerIdle::reach`]).
    pub(crate) route: Arc<RouteFn>,
    /// THE MEMORY CEILING's measure of a tab's agent by its pid
    /// ([`WorkerIdle::memory`]): its process group's physical footprint, the
    /// machine's memory (its size, what is available, what is in use), its
    /// pressure level and its swap rate ([`agent_memory`]: a handful of
    /// syscalls, no process spawned), and the largest footprint of the other
    /// tabs' agents measured lately ([`MemoryBoard`]) — so only the dominant
    /// agent is restarted when the Mac is in trouble.
    pub(crate) memory: Arc<MemoryFn>,
    /// THE WATCH (design record 2026-09-28, "No upgrade stuck forever",
    /// §3.2 C4): one look at a tab's upgrade records OFF ANY POINT
    /// ([`upgrade_drive::watch`]) — a dry-run read, and under the sweep
    /// lock's `try_lock` only the watch's own fields and the three
    /// hands-free changes (a retarget, a limit's clock hold, a re-arm).
    /// Nothing is typed, signalled or ended. Called on a watch thread of its
    /// own at Background QoS ([`watch_upgrades`]), never on the host thread.
    pub(crate) watch: Arc<WatchFn>,
}

/// What a worker hands the accounts recorder of its Claude Code at a usage
/// wall ([`Acts::account_wall`]), from the snapshot it keeps of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WalledAgent<'a> {
    /// The agent's snapshot: its pid, whose exec environment names the login
    /// it runs under; its kernel start ([`Snapshot::start`]), before which a
    /// wall is the process's before it, and nobody's; and where its records
    /// are, which the wall is read from (`upgrade_drive::named_standing_wall`).
    pub(crate) snap: &'a Snapshot,
    /// The conversation it holds: the transcript the wall stands in.
    pub(crate) session: &'a str,
    /// `[harness] usage_notes`, read live: with no account listed, the
    /// machine's own row (`accounts::Roster::own`) stands for the roster, so
    /// what the wall says of the one account's usage credits is kept
    /// (accounts v2 S5) — never a move: one row advises none.
    pub(crate) own: bool,
}

/// Stop one worker: its flag, then its connection's cut. A stop is never an
/// agent's exit: its word says so first ([`Word::Stopped`]), so a worker
/// waiting out a relaunch's pause ends it.
fn request_stop(stop: &AtomicBool, left: &ExitWord, interrupt: &Mutex<Option<Interrupter>>) {
    left.stop();
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
/// ever ([`restart_backoff`]; told to the window while it waits once past
/// [`RESTART_BUDGET`] an hour), and ends stopped — handling its agent's exit
/// first when the host says the agent LEFT ([`on_agent_left`]) — or held.
fn worker_main(job: WorkerJob, hooks: &Hooks) -> WorkerExit {
    let sid = job.sid.clone();
    let exit = worker_run(job, hooks);
    // Whatever this worker told the window about itself ends with it: a claim
    // for the digest must not outlive the tab's supervisor.
    end_notes(hooks, &sid);
    exit
}

/// The facts a worker told the window about itself end with it ([`HostNote`]):
/// each is idempotent at the window, which acts only on what it holds.
fn end_notes(hooks: &Hooks, sid: &str) {
    (hooks.note)(HostNote::SupervisorSteady {
        sid: sid.to_string(),
    });
    (hooks.note)(HostNote::UpgradeRuns {
        sid: sid.to_string(),
    });
    (hooks.note)(HostNote::SurveyEnded {
        sid: sid.to_string(),
    });
    (hooks.note)(HostNote::ResumeException {
        sid: sid.to_string(),
        standing: None,
    });
    (hooks.note)(HostNote::ResumeUnread {
        sid: sid.to_string(),
        minutes: None,
    });
}

/// [`worker_main`]'s life proper.
fn worker_run(mut job: WorkerJob, hooks: &Hooks) -> WorkerExit {
    // A worker that starts over a relaunch's word (the agent runs in the tab
    // again, the body clears the mark) ends every line that word made.
    if job.clear_badge {
        (hooks.note)(HostNote::Relaunch {
            sid: job.sid.clone(),
            said: RelaunchSay::Ended,
        });
    }
    attach(&job, hooks);
    // THE ADOPTED-CLAIM GRACE, WATCHED (round eight of the update audit,
    // #501): until it is over this worker only watches its agent
    // ([`WorkerJob::watch_until`]). Held off instead, no worker read the
    // agent or was handed its exit, and one that crashed inside the grace was
    // left at a bare shell with nothing said. A stop — its exit handed, its
    // tab closed, a Commit, a hand-over — ends the watch as it ends a loop.
    if let Some(until) = job.watch_until {
        if wait_done(&job.stop, until) {
            return stopped(&job, hooks);
        }
        job.watch_until = None;
    }
    // Consecutive failures past the budget: each pauses longer.
    let mut over = 0usize;
    // The window was told this supervisor keeps failing.
    let mut noted = false;
    loop {
        if job.stop.load(Ordering::SeqCst) || !job.admitted() {
            return stopped_beside(&job, hooks);
        }
        let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (hooks.body)(&job)))
            .unwrap_or_else(|payload| BodyEnd::Failed(panic_text(payload.as_ref())));
        job.clear_badge = false;
        *job.interrupt.lock().unwrap_or_else(PoisonError::into_inner) = None;
        // The loop that told a next try is gone — ended, held or failed
        // alike: the tab does not keep a time nobody will act on through the
        // restart pause ([`WorkerIdle`]'s drop clears it only when this
        // worker thread ends, and a failing body is started again on the
        // same [`WorkerJob`], its idle host with it). A plan a fresh run
        // tells is told over this; so is a Codex's reset it places again.
        (hooks.waiting)(&job.sid, None);
        // So does the task's face that loop told: "unknown" until the next
        // run's loop tells its own at its start — a body that panicked never
        // reached its loop's end, and a wake may have been lost.
        (hooks.fact)(&job.sid, crate::HarnessFact::Task(None));
        (hooks.placed)(&job.sid, None);
        if job.stop.load(Ordering::SeqCst) {
            return stopped_beside(&job, hooks);
        }
        // THE PROVEN SELF-CLAIM, IN BOTH ORDERS (the self-claim contract,
        // §4): whose the session is, read again as every run ends — its
        // loop's claim met a proven self-claim (`take_claim` behind it), or
        // one replaced its loop's claim and the control thread cut the loop
        // ([`self_claimed`]); or the self-claim went while its agent runs
        // ([`self_claim_released`]). A move is no failure: the next run is
        // the one the session's claim calls for — watch-only beside the
        // self-claim, or this host's own loop again — at once.
        let beside = (hooks.acts.status)(&job.sid)
            .as_deref()
            .and_then(proven_self_claim);
        if beside != job.beside() {
            match &beside {
                Some(holder) => aterm_log::info!(
                    "harness @{}: {holder} supervises itself (a proven self-claim): watching \
                     only — aterm's stall and memory remedies stand, and its exit is read here",
                    job.sid
                ),
                None => aterm_log::info!(
                    "harness @{}: the proven self-claim is gone; supervising again",
                    job.sid
                ),
            }
            *job.beside.lock().unwrap_or_else(PoisonError::into_inner) = beside;
            continue;
        }
        let why = match end {
            BodyEnd::Stopped if job.stop.load(Ordering::SeqCst) => {
                return stopped_beside(&job, hooks);
            }
            BodyEnd::Held(other) => {
                aterm_log::info!("harness @{}: not supervised: {CLAIM_HELD}{other}", job.sid);
                return WorkerExit::Held;
            }
            BodyEnd::Stopped => "the loop ended unasked".to_string(),
            BodyEnd::Failed(why) => why,
        };
        if job.stop.load(Ordering::SeqCst)
            || !(hooks.still_wanted)(&job.sid, job.group.load(Ordering::SeqCst))
        {
            return stopped_beside(&job, hooks);
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
            // A record, and a line of the digest once it has gone on for an hour
            // ([`Horizon::SUPERVISOR_RESTARTS`]); never a mark on the tab.
            (hooks.note)(HostNote::SupervisorFailing {
                sid: job.sid.clone(),
                why: why.clone(),
                pause,
            });
            noted = true;
        } else {
            aterm_log::warn!(
                "harness @{}: restarting the supervisor ({failed} of {RESTART_BUDGET} this hour) \
                 in {} ms: {why}",
                job.sid,
                pause.as_millis(),
            );
            // Back within the budget: the failure is over.
            if std::mem::take(&mut noted) {
                (hooks.note)(HostNote::SupervisorSteady {
                    sid: job.sid.clone(),
                });
            }
        }
        if wait_done(&job.stop, Instant::now() + pause) {
            return stopped_beside(&job, hooks);
        }
    }
}

/// [`stopped`], said as the watch-only worker's end where it watched beside
/// a proven self-claim ([`WorkerExit::SelfClaimed`]: never a `host.held`
/// entry).
fn stopped_beside(job: &WorkerJob, hooks: &Hooks) -> WorkerExit {
    let exit = stopped(job, hooks);
    if exit == WorkerExit::Stopped && job.beside().is_some() {
        WorkerExit::SelfClaimed
    } else {
        exit
    }
}

/// A stopped worker's end: its agent's exit handled first when that is why
/// it was stopped — or when a Commit's stop came with that exit handed and
/// took its word ([`WorkerJob::left_at_commit`]). One a Commit's stop ended
/// with no exit handed, or one handed over to a successor not started yet
/// ([`WorkerJob::handover`], round fourteen), leaves its agent's exit to the
/// host ([`Kept::unwatched`]): read under the session's lock, as
/// [`Worker::leave`] hands one. The host's next pass that finds the agent in
/// the tab starts its successor, which ends the mark; one that finds it gone
/// hands the exit. Whether the explicit restart ended it is not this
/// worker's to carry: the worker that reads the exit alone looks up the
/// restart's stamp ([`Acts::asked`]).
///
/// AN AGENT THE HOST READ BACK BEFORE THIS READ WAS MARKED SO BY THE HOST
/// ITSELF ([`Worker::back`]), and that mark stands (round fifteen, ported
/// onto the joint audit's): it names the agent in the tab now, as the roster
/// read it back, not this worker's agent as it last knew it.
///
/// A WORKER THAT ENDS LEAVING ITS AGENT'S EXIT TO THE HOST ARMS THE EXIT
/// WATCH (round seventeen; [`Kept::exit_watch`]): the host hands that exit
/// once a pass of it reads the roster again — after a Commit's whole hold —
/// and a crash's record read only then could have been removed by any
/// Claude Code started meanwhile, and read as a graceful exit.
fn stopped(job: &WorkerJob, hooks: &Hooks) -> WorkerExit {
    let handed = with_kept(job, |k| {
        if job.revoked.load(Ordering::SeqCst) {
            return false;
        }
        let handed = exit_handed(job);
        if !handed
            && k.unwatched.is_none()
            && (job.handover.load(Ordering::SeqCst)
                || matches!(k.commit, Committed::Stopped | Committed::TakenUp(_)))
        {
            k.unwatched = Some(Unwatched {
                agent: job.agent,
                group: job.group.load(Ordering::SeqCst),
            });
        }
        handed
    });
    if handed {
        on_agent_left(job, hooks);
    }
    if job.admitted() && with_kept(job, |k| k.unwatched.is_some()) {
        watch_exit_guarded(&job.sid, &job.kept, hooks, None, Some(&job.revoked));
    }
    WorkerExit::Stopped
}

/// A WORKER THAT READS ITS AGENT'S EXIT ALONE (round thirteen): started by
/// the host for an exit no worker was handed ([`Kept::unwatched`]), with its
/// exit handed from the start — `left` and `stop` set, as
/// [`Worker::leave`] sets them — so it runs no loop: it reads the exit as
/// any worker does as it ends ([`stopped`], [`on_agent_left`]), relaunching
/// it on the session's back-off, and a Commit's stop, a tab closed or the
/// agent back reach it as they reach that one — and whether the explicit
/// restart ended it (U1) is read as any worker reads it, by the restart's
/// stamp ([`Acts::asked`]).
fn exit_main(job: WorkerJob, hooks: &Hooks) -> WorkerExit {
    let exit = stopped(&job, hooks);
    end_notes(hooks, &job.sid);
    exit
}

/// Whether the agent's exit is the worker's to read: handed to it
/// ([`WorkerJob::left`]), or handed when a Commit's stop came
/// ([`WorkerJob::left_at_commit`], set before that stop's word — so the
/// word is read first).
fn exit_handed(job: &WorkerJob) -> bool {
    job.left.is(Word::Left) || job.left_at_commit.load(Ordering::SeqCst)
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
    aterm_agent::harness::lanes::lanes(agent).upgrade
}

/// Whether the relaunch on exit is written for `agent` ([`on_agent_left`]):
/// Claude Code's, and — its Codex lane, the harness's Codex parity
/// (2026-09-27) — Codex's.
fn relaunches(agent: Program) -> bool {
    aterm_agent::harness::lanes::lanes(agent).relaunch
}

/// Whether the host reads `agent`'s conversation through its TRANSCRIPT —
/// whether anyone asked it anything ([`WorkerIdle::taskless`]), the limit's
/// reset it names ([`WorkerIdle::limit_until`]), the tail that confirms a
/// wall ([`WorkerIdle::transcript_tail`]): Claude Code's, whose own records
/// name the conversation and the file it writes. A Codex's rollout is not
/// read here, nor agy's or Muse Code's records, and a program no reader
/// identifies has none.
fn reads_transcript(agent: Program) -> bool {
    match agent {
        Program::Claude => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Generic => false,
    }
}

/// Whether the accounts are written for `agent` — the recorder of its walls,
/// the roster's moves, renewals and probe, the limit reset and the usage tap
/// ([`WorkerIdle::account_ctx`], [`account_gone`]): Claude Code's logins
/// alone. Another agent has no account here.
fn accounts_for(agent: Program) -> bool {
    match agent {
        Program::Claude => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Generic => false,
    }
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
    /// This run has told the window the upgrade runs, once: the first step that
    /// is not `Broken` restates it, so a `cannot-run` line adopted from an
    /// earlier process ends at the first fine look ([`Self::upgrade_runs`]).
    runs_told: bool,
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
    /// The flow the last step was taken under: QUIET (`[harness] upgrade_ask
    /// = false`, the host's [`Switches::ask`] read at the step), where a wait
    /// on the agent's own work owns no turn end and so bounds none
    /// ([`upgrade_drive::owns_turn_ends_as`], [`upgrade_drive::bounded_wait_as`];
    /// round eight, #25). `false`: the ask-and-drain flow.
    quiet: bool,
}

/// How many distinct unknown step words one worker warns of: past it, a word
/// is looked at again all the same, and not said.
const UNKNOWN_WORDS_WARNED: usize = 8;

impl UpgradeRun {
    /// Whether the step that said `step` leaves the upgrade owning the
    /// session's turn ends ([`upgrade_drive::owns_turn_ends`]), read with the
    /// count its word is bounded by: a person's hold with its attended looks at
    /// this point, a refused notice with its refused looks ([`Self::refused`]),
    /// every other word with its settle and drain waits — under the flow the
    /// host's upgrade runs in ([`Self::quiet`],
    /// [`upgrade_drive::owns_turn_ends_as`]). Asked before [`Self::after`]
    /// counts the step.
    fn owns(&self, step: &str, grace_s: u32) -> bool {
        let looks = if upgrade_drive::attended(step) {
            self.attended
        } else if upgrade_drive::refused(step) {
            self.refused
        } else {
            self.waits
        };
        upgrade_drive::owns_turn_ends_as(step, looks, grace_s, self.quiet)
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
            } else if upgrade_drive::bounded_wait_as(step, self.quiet) {
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

/// How long the live [`Acts::limit_reset`] keeps a conversation's read of
/// its transcript's limit reset before reading it again: a reset matters to
/// the hour ([`upgrade_drive::LIMIT_GRACE_S`] past it), and the loop asks at
/// every read a limit with no reset of its own stands on.
const LIMIT_READ_AGAIN: Duration = Duration::from_secs(60);

/// The live [`Acts::limit_reset`]'s read of `session`, kept: `read` runs only
/// where no read of it is under [`LIMIT_READ_AGAIN`] old (the loop asks at
/// each read a limit with no reset of its own stands on), and reads that old
/// are let go.
fn limit_reset_kept(
    kept: &Mutex<HashMap<String, (Instant, Option<u64>)>>,
    session: &str,
    read: impl FnOnce() -> Option<u64>,
) -> Option<u64> {
    let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
    kept.retain(|_, (at, _)| at.elapsed() < LIMIT_READ_AGAIN);
    if let Some((_, reset)) = kept.get(session) {
        return *reset;
    }
    let reset = read();
    kept.insert(session.to_string(), (Instant::now(), reset));
    reset
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
    stop: Arc<AtomicBool>,
    grace: u32,
    park: Arc<AtomicBool>,
    look_at: Arc<Mutex<Option<Instant>>>,
    acting: Arc<AtomicBool>,
    /// The stamp of the last step this worker took that may end its agent,
    /// moved as the step begins ([`Self::act`]) and again as it ends
    /// ([`Self::acted`]; `0`: none, or the one its predecessor's keep began
    /// at): a kept agent's exit is inferred only while it has not moved
    /// ([`keep_holding`]), and an agent a pass's roster missed is judged
    /// gone only if it has not moved since that read ([`StepsSeen`]).
    stepped: Arc<AtomicU64>,
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
    /// The agent's memory as last measured, by its pid and when
    /// ([`WorkerIdle::memory`]): answered again until
    /// [`aterm_agent::supervise::memory::SAMPLE_EVERY`] has passed, or the
    /// agent is another process.
    memory: Mutex<Option<(u32, Instant, Option<AgentMemory>)>>,
    /// What the loop last offered and withheld ([`Points`]).
    points: Arc<Points>,
    /// What this worker keeps of its session's usage tap ([`TapKept`]).
    tap: Mutex<TapKept>,
    /// The host started this worker as the session's Autopilot left Off
    /// ([`HostState::was_off`], [`IdleHost::lifted_off`]).
    lifted_off: bool,
    /// [`WorkerJob::beside`]: while set the loop is asked for no step — no
    /// upgrade, no notice, no restart in place.
    beside: Arc<Mutex<Option<String>>>,
}

impl WorkerIdle {
    /// Whether the worker watches beside a proven self-claim
    /// ([`WorkerJob::beside`]): nothing of the host's is typed or ended.
    fn beside(&self) -> bool {
        self.beside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }
}

/// WHAT A WORKER KEEPS OF ITS SESSION'S USAGE TAP ([`WorkerIdle::usage_tap`]):
/// the freshness rule over its tab's readings, the silence rule over its
/// tapped process, the process a turn was last seen to run in, and what it
/// last took to the store.
#[derive(Debug, Default)]
pub(crate) struct TapKept {
    fresh: tap::Fresh,
    silence: tap::Silence,
    /// The process a turn was last seen to run in ([`IdleHost::turn_ran`]).
    served: Option<u32>,
    /// The windows this worker last recorded, and when: a fresh reading of
    /// the same before the heartbeat is not taken to the store's lock.
    noted: Option<(Option<tap::Window>, Option<tap::Window>, i64)>,
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
    /// A point asked for, or a look the worker scheduled come due: the loop
    /// wakes at [`Self::wants_at`] and must not wait on the host thread's
    /// own wake to turn that look into [`WorkerJob::park`].
    fn wants(&self) -> bool {
        // Beside a proven self-claim the host takes no step (§4).
        if self.beside() {
            return false;
        }
        self.park.load(Ordering::SeqCst)
            || self
                .look_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some_and(|at| at <= Instant::now())
    }

    fn wants_at(&self) -> Option<Instant> {
        *self.look_at.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A supervisor's information for a person (Codex's save-then-wait hold
    /// among them): a record in the window's Messages, no row, no badge.
    fn inform(&self, text: &str) {
        (self.hooks.note)(HostNote::Inform {
            sid: self.sid.clone(),
            text: text.to_string(),
        });
    }

    /// A WARNING ([`IdleHost::warn`]: the memory ceiling's, once an
    /// episode and again when what it says comes next changes): a warning
    /// ROW on the Messages band
    /// ([`crate::message_reporters::supervisor_warning`]), nothing asked.
    fn warn(&self, text: &str) {
        (self.hooks.note)(HostNote::Warn {
            sid: self.sid.clone(),
            text: text.to_string(),
        });
    }

    /// WHAT THE SESSION'S SCREEN SAYS OF ITS ACCOUNT'S USAGE
    /// ([`IdleHost::usage_seen`]; accounts v2 S5): usage credits carrying it,
    /// the vendor's warning that a window fills — recorded for the account its
    /// Claude Code runs on ([`Acts::account_usage`]) where a record is kept at
    /// all: accounts listed and `[harness] accounts` not `off`, or none listed
    /// and `[harness] usage_notes` on, under the machine's own row. Another
    /// agent has no account here, a Claude Code with no snapshot yet has no
    /// process to read, and a screen that says nothing has nothing to record:
    /// nothing is asked. A record that changed tells the
    /// window what the accounts say now ([`accounts_look`]: the digest's
    /// credits line and its week's). One line of the log: the recorder's word
    /// and what the screen said. Nothing is typed, and the loop decides
    /// nothing by it.
    fn usage_seen(&self, seen: &UsageSeen) {
        let listed = self.switches.accounts() != accounts::Mode::Off;
        let own = self.switches.notes();
        // Claude Code's notices only: a Codex account's usage is its rollouts'.
        let claude = match self.agent {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Generic => false,
        };
        if !claude || !(listed || own) || !seen.says() {
            return;
        }
        let agent = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .clone()
            .and_then(|s| Some((s.session.clone()?, s)));
        let Some((session, snap)) = agent else {
            return;
        };
        let settled = || self.settled(snap.pid, &snap.start, &snap.argv, &snap.cwd);
        let walled = WalledAgent {
            snap: &snap,
            session: &session,
            own,
        };
        let word = (self.hooks.acts.account_usage)(&self.sid, &walled, &settled, seen, listed);
        let credits = if seen.credits.is_some() {
            "in-use"
        } else {
            "-"
        };
        let warning = seen.warning.as_ref().map_or("-".to_string(), |w| {
            format!("{} {}%", w.window.name(), w.pct)
        });
        aterm_log::info!(
            "harness @{}: account usage {word}: credits {credits} warning {warning}",
            self.sid
        );
        if word == "usage" {
            accounts_look(&self.switches, &self.hooks, false);
        }
    }

    fn owns_turn_end(&self) -> bool {
        self.on() && self.owns.load(Ordering::SeqCst) && self.switches.upgrade()
    }

    /// A rating survey the loop cannot close, or its end: told to the window as a
    /// fact ([`HostNote::SurveyStuck`]) — a record now and a line of the digest
    /// after an hour ("press 0 in that tab"), never a mark on the tab.
    fn survey_stuck(&self, stuck: Option<(aterm_agent::supervise::StuckSurvey, &str)>) {
        (self.hooks.note)(match stuck {
            Some((kind, why)) => HostNote::SurveyStuck {
                sid: self.sid.clone(),
                kind,
                why: why.to_string(),
            },
            None => HostNote::SurveyEnded {
                sid: self.sid.clone(),
            },
        });
    }

    /// A point only a person can answer, or its end: told to the window as a fact
    /// ([`HostNote::ResumeException`]), which files a `Later` one in the digest
    /// (`harness.resume.<kind>`) and ends it with the fact. A `Now` one is never
    /// an entry: one the supervisor escalated (the old lost login's `/login`) is
    /// that escalation's, and one it did not is the owner's standing ask on the
    /// glass (`harness.resume-ask.<tab>`) while it stands.
    fn exception(&self, standing: Option<&aterm_agent::supervise::policy::exception::Exception>) {
        (self.hooks.note)(HostNote::ResumeException {
            sid: self.sid.clone(),
            standing: standing.cloned(),
        });
    }

    /// The stall canary's second rung (90 minutes of background work, 30 of a
    /// dialog), or its end ([`HostNote::ResumeUnread`]): a record and an error
    /// line at the window, nothing on the tab.
    fn stall_unread(&self, minutes: Option<u64>) {
        (self.hooks.note)(HostNote::ResumeUnread {
            sid: self.sid.clone(),
            minutes,
        });
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
        if !self.on() || self.beside() {
            return None;
        }
        self.points.offered(crate::upgrade_host::now_s());
        // The look this point answers is spent, and the request cleared, under
        // the look's lock: the host thread turns a due look into `park` under
        // it too, so it either did so first (and the request is cleared with
        // the look it stood for) or finds no look due. Clearing `park` before
        // the look let a conversion in between park the loop again for a look
        // already taken: a second step at once, an unread ladder's rung
        // skipped (the review of 4c0625764).
        let mut look = self.look_at.lock().unwrap_or_else(PoisonError::into_inner);
        if look.is_some_and(|at| at <= Instant::now()) {
            *look = None;
        }
        self.park.store(false, Ordering::SeqCst);
        drop(look);
        // A limit reset's outcome the store's lock kept out is written at the
        // session's next idle point at the latest ([`Self::reset_flush`]):
        // one lock read where none is held.
        if with_kept_of(&self.kept, |k| k.account.reset_owed.is_some())
            && let Some(seat) = (self.hooks.acts.account_seat)()
        {
            self.reset_flush(&seat);
        }
        let step = self.idle_step();
        if !self.on() {
            return None;
        }
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
        if !self.on() || self.beside() || !upgrades(self.agent) || !self.switches.upgrade() {
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
        if !self.on() {
            return None;
        }
        self.act();
        let step = (hooks.acts.notice)(&self.sid, self.grace);
        self.acted();
        if !self.on() {
            return None;
        }
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
    ///
    /// A MOVE TO ANOTHER ACCOUNT ([`Restart::Account`]) is that restart with
    /// the roster's login on its line, inside the move's own transaction
    /// ([`Self::account_move`]): one line of the log, the label and the step.
    /// Its relaunch types nothing: the next idle point is decided as with no
    /// move, or the wall's resume typed after a move made at one.
    fn restart(&self, why: &Restart) -> Option<String> {
        if !self.can_restart() || self.beside() {
            return None;
        }
        if let Restart::Account { to, at } = why {
            let step = self.account_move(why, to, *at);
            aterm_log::info!("harness @{}: account move to {to}: {step}", self.sid);
            return self.on().then_some(step);
        }
        let hooks = &self.hooks;
        self.act();
        let step = (hooks.acts.restart)(&self.sid, self.grace, why);
        self.acted();
        if !self.on() {
            return None;
        }
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
        self.on()
            && aterm_agent::harness::lanes::lanes(self.agent).restart_in_place
            && self.switches.relaunch()
    }

    /// An agent the stall's remedy ends is relaunched here ([`on_agent_left`],
    /// the exit read as the remedy's, U1) — the agents the relaunch on exit
    /// is written for, under `[harness] relaunch` — so the loop may take the
    /// remedy itself once the stall has stood past its bound.
    fn relaunches_after_stall(&self) -> bool {
        self.on() && relaunches(self.agent) && self.switches.relaunch()
    }

    /// The agent's process as the kept snapshot names it: the leader the
    /// stall's remedy restarts (`restart pid=<n> if-stalled`). One leaf lock.
    fn agent_pid(&self) -> Option<u32> {
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .as_ref()
            .map(|snap| snap.pid)
    }

    /// Whether the explicit restart would be carried for the agent now, and
    /// whether its relaunch on the conversation is promised
    /// ([`IdleHost::restart_offered`]): the one predicate every stall word
    /// chooses by ([`Acts::offered`], [`crate::restart_act::offered`]), which
    /// `[harness] relaunch = false` does not limit (ruling 1, 2026-10-08).
    /// The store's read lock, then leaf locks; no I/O.
    fn restart_offered(&self) -> Option<bool> {
        (self.hooks.acts.offered)(&self.sid)
    }

    /// The line a person runs to resume this session's agent on its OWN
    /// conversation — `claude --resume <id>` with its launch flags, a
    /// `--model` its record marks failed left off, or `codex resume <thread>`
    /// with its kept flags ([`resume_told`]) — from the relaunch snapshot the
    /// host keeps of it (its argv and the conversation the agent's own
    /// records name, FOLLOWED through an in-app `/clear`), for the loop's
    /// frozen mail and memory escalation where the host does not relaunch it
    /// itself. `None` for an agent the relaunch is not written for, or before
    /// a snapshot was read. Never `claude --continue` (robustness backlog
    /// item 2, 2026-09-26): it resumes the directory's newest conversation —
    /// a sibling tab's where two share it.
    fn resume_command(&self) -> Option<String> {
        if !relaunches(self.agent) {
            return None;
        }
        // Copied out of the (leaf) lock: the line reads the model record.
        let snap = {
            let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
            kept.snapshot.clone()?
        };
        resume_told(&snap)
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
        let probed = match self.agent {
            Program::Claude => self.switches.probe_api(),
            Program::Codex | Program::Agy | Program::Muse | Program::Generic => false,
        };
        if !probed {
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
        match self.route_of(pid, &start, &argv, &cwd) {
            Route::Default => self.net.ask(&self.sid),
            Route::Custom(_) => Reach::Unknown,
        }
    }

    /// WHAT THE HOST MEASURES OF THE AGENT'S MEMORY ([`IdleHost::memory`];
    /// the frozen Mac of 2026-09-28): the agent the relaunch snapshot names
    /// ([`Kept::snapshot`]: the tab's foreground job), measured by
    /// [`Acts::memory`] on the loop's own thread — never the window's — at
    /// most once per [`aterm_agent::supervise::memory::SAMPLE_EVERY`] for
    /// the same process, the last reading answered in between; a new agent
    /// process is measured at once. Any agent the host supervises (the
    /// restart itself is Claude Code's alone, [`Self::can_restart`]); none
    /// before a snapshot names it.
    fn memory(&self) -> Option<AgentMemory> {
        let pid = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .as_ref()
            .map(|s| s.pid)?;
        let now = Instant::now();
        if let Some((p, at, m)) = *self.memory.lock().unwrap_or_else(PoisonError::into_inner)
            && p == pid
            && now.saturating_duration_since(at) < aterm_agent::supervise::memory::SAMPLE_EVERY
        {
            return m;
        }
        // Measured outside every lock.
        let m = (self.hooks.acts.memory)(&self.sid, pid);
        *self.memory.lock().unwrap_or_else(PoisonError::into_inner) = Some((pid, now, m));
        m
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
        if let Some(pid) = with_kept_of(&self.kept, |k| k.snapshot.as_ref().map(|s| s.pid)) {
            self.tap
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .served = Some(pid);
        }
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

    /// THE USAGE TAP AT A DECIDED IDLE TURN END ([`IdleHost::usage_tap`];
    /// `aterm_agent::harness::tap`, the accounts design v2 §3.1, §3.2,
    /// ruling R-7): [`Self::tap_step`], said in one line of the log where
    /// it did something. It types nothing and moves no session.
    fn usage_tap(&self) {
        if let Some(word) = self.tap_step() {
            aterm_log::info!("harness @{}: usage tap: {word}", self.sid);
        }
    }

    /// STARTED AS OFF LIFTED ([`HostState::was_off`]): the person drove the
    /// session while its worker was stopped, so the acts its ledger shows
    /// carry no task for this one (save for a turn running as it attaches).
    /// Its first point is backed off and acted on as any first point is.
    fn lifted_off(&self) -> bool {
        self.lifted_off
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
        if !reads_transcript(self.agent) {
            return false;
        }
        let held = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .clone()
            .filter(|s| s.session.is_some());
        let Some(snap) = held else {
            return false;
        };
        let mut memo = self.tasked.lock().unwrap_or_else(PoisonError::into_inner);
        (self.hooks.acts.tasked)(&self.sid, &snap, &mut memo) == Some(false)
    }

    /// THE USAGE LIMIT'S RESET THE CONVERSATION'S TRANSCRIPT NAMES
    /// ([`IdleHost::limit_until`]; the owner's ruling of 2026-09-29, design
    /// record C9): the conversation a Claude Code holds — the kept record, as
    /// [`Self::taskless`] reads it, in its agent's own records — through
    /// [`Acts::limit_reset`]. Asked by
    /// the loop where its limit episode read no reset of its own, and where a
    /// notice names a clock time but no day; past this reset and its grace
    /// the notice left on the screen is the idle point it is. A Codex, and a
    /// conversation not registered yet, name none.
    fn limit_until(&self) -> Option<u64> {
        if !reads_transcript(self.agent) {
            return None;
        }
        let snap = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .clone()
            .filter(|s| s.session.is_some())?;
        (self.hooks.acts.limit_reset)(&snap)
    }

    /// THE TRANSCRIPT THAT CONFIRMS A WALL ([`IdleHost::transcript_tail`],
    /// H6-HARNESS-1): the Claude Code in the tab NOW — the snapshot read again
    /// first ([`refresh_snapshot`]: the process in the tab) — through
    /// [`Acts::transcript_tail`], which finds the conversation that process
    /// writes now from its own record (`/clear` moves it) under its own
    /// Claude Code directory. The kept snapshot is the one the host last
    /// read, at its own points: after a relaunch in the tab it can still name
    /// the previous process, whose transcript's end says nothing of this
    /// wall. A read that fails leaves no transcript (a wall read under a
    /// tool's call is then not acted on), and so does a Codex.
    fn transcript_tail(&self) -> Option<String> {
        if !reads_transcript(self.agent) || !refresh_snapshot(&self.sid, &self.kept, &self.hooks) {
            return None;
        }
        let snap = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .clone()?;
        (self.hooks.acts.transcript_tail)(&snap)
    }

    /// A CODEX'S WALL, PLACED WHERE ITS UPGRADE PLACES IT
    /// ([`IdleHost::place_reset`]; the review of 2026-09-30): the words of
    /// the wall the Codex the worker keeps (its snapshot's pid and
    /// `$CODEX_HOME`) printed, placed in that Codex's own zone through
    /// [`Acts::codex_reset`] — the loop runs in this process, whose zone a
    /// `TZ` the tab's shell exported never reaches. `Some(None)`: its zone
    /// cannot be read, and no reset is guessed, as its upgrade guesses none.
    /// A Claude Code (its words name their zone), and a Codex not read yet,
    /// are the loop's own to read.
    fn place_reset(&self, text: &str, now: i64) -> Option<Option<i64>> {
        match self.agent {
            Program::Codex => {}
            Program::Claude | Program::Agy | Program::Muse | Program::Generic => return None,
        }
        let (pid, home) = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .as_ref()
            .and_then(|s| Some((s.pid, s.codex.as_ref()?.home.clone())))?;
        let now = u64::try_from(now).ok()?;
        let at = (self.hooks.acts.codex_reset)(pid, &home, text, now)
            .and_then(|at| i64::try_from(at).ok());
        // The window counts the tab's wall down to this placement, not to its
        // own reading of the words in its own zone.
        (self.hooks.placed)(
            &self.sid,
            Some(crate::presence::PlacedReset {
                words: text.to_string(),
                at_unix: at,
            }),
        );
        Some(at)
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
    ///
    /// The open is also where a Claude Code's own wall is recorded for its
    /// account ([`Self::account_wall`]) — under no switch of the upgrade's
    /// (the recorder is not the upgrade's) and none of the accounts' (where
    /// nothing of the accounts acts — [`Switches::accounts_live`],
    /// [`Self::accounts_act`]: `accounts = "off"` with `logins = "off"`, or
    /// `off` over a roster with no row a renewal restores — no move is made,
    /// and under `accounts = "off"` the one move left is the login
    /// lifecycle's follow onto a renewed login; the recorder records all the
    /// same), and LAST: the recorder waits on the accounts store's lock — a
    /// bounded wait — and the upgrade's hold is never stamped late behind
    /// it. Where its word is one that asking again can change
    /// ([`wall_asked_again`]: the transcript's row not written yet, the lock
    /// held), it is asked again at the session's next decisions of this
    /// episode ([`Self::account_wall_again`]); the close ends that, and
    /// gives the next episode its own few short waits
    /// ([`AccountKept::lagged`]: the open does not — the loop decides a
    /// limited point BEFORE it opens the episode on it, so the first of
    /// them is already counted).
    fn limited(&self, open: bool) {
        if upgrades(self.agent) {
            if open {
                self.owns.store(false, Ordering::SeqCst);
            }
            let now = aterm_time::unix_secs().unwrap_or(0);
            *self
                .clock_hold
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(now);
            self.hold_clock();
        }
        let again = if open && wall_asked_again(self.account_wall()) {
            WALL_AGAIN
        } else {
            0
        };
        with_kept_of(&self.kept, |k| {
            k.account.wall_again = again;
            if !open {
                k.account.lagged = 0;
            }
        });
    }

    /// THE LOOP'S NEXT TRY ([`IdleHost::waiting`]): told to the window, which
    /// shows it on this tab beside an API wall; `None` when no wait stands.
    ///
    /// The plan's clock time is read HERE, on the worker's thread and at the
    /// try's own instant ([`crate::presence::RetryPlan::at`]: the C library's
    /// zone rules at that instant), so a daylight-saving or timezone change
    /// since it started moves nothing.
    fn waiting(&self, plan: Option<aterm_agent::supervise::WaitPlan>) {
        (self.hooks.waiting)(
            &self.sid,
            plan.map(|p| crate::presence::RetryPlan::at(p.at_unix)),
        );
    }

    /// AN ACT OF THE HARNESS'S ([`IdleHost::act_taken`]): told to the window,
    /// which may light the session's Autopilot chip for a moment.
    fn act_taken(&self, act: aterm_agent::harness::acts::Act) {
        (self.hooks.fact)(&self.sid, crate::HarnessFact::Act(act));
    }

    /// THE TASK'S FACE ([`IdleHost::task_face`]): told to the window, which
    /// keeps it on the session's timeline for the chip and `status task=`.
    fn task_face(&self, face: aterm_agent::harness::acts::TaskFace) {
        (self.hooks.fact)(&self.sid, crate::HarnessFact::Task(Some(face)));
    }

    /// WHAT THE ROSTER ADVISES ABOUT THIS SESSION'S ACCOUNT at `point`
    /// ([`IdleHost::account`]; the accounts design, §6), in this order:
    ///
    /// 1. `[harness] accounts` (read live) is not `off`, the agent is a
    ///    Claude Code, and THE OWNER LISTED AN ACCOUNT — else `None`, with
    ///    nothing of the agent read and nothing made on disk.
    /// 2. What the agent's own process says of its login
    ///    ([`Self::account_look`]: its exec environment, and its route for a
    ///    login that could be a row's), and — at a usage limit — THIS
    ///    PROCESS'S OWN standing wall ([`own_wall`]). A limit only on the
    ///    screen, or a row older than the process (the account it left's,
    ///    still standing after a move), is no wall of the roster's: it is
    ///    asked about as a plain turn end ([`account_point`]) — followed
    ///    from, recorded for nobody, and rotated from only as any plain
    ///    turn end is (`early`, on what is ON RECORD of the account).
    /// 3. The state, READ WITHOUT THE STORE'S LOCK (it is written by a
    ///    rename: a reader sees the old file or the new one). A state this
    ///    build cannot read as its own advises nothing.
    /// 4. `accounts::advise`, under the mode the owner wrote (`early`: the
    ///    move ahead of a limit too, and the word that the account's
    ///    `/usage` read is due), as the policy takes it
    ///    ([`account_advice_of`]). WHERE THE ROSTER ANSWERS THAT A PROBE IS
    ///    DUE it is asked once more past the probe
    ///    (`accounts::advise_past_probe`), and both are handed on
    ///    ([`AccountAdvice::Probe`]'s `otherwise`): the roster answers
    ///    `Probe` AHEAD of its leave check, and a probe nobody makes must
    ///    not hide the move behind it.
    ///
    /// The session's row is written for the report where it is missing or
    /// stale ([`Self::account_note`]: the one write here, under the lock,
    /// short). What is advised is kept ([`AccountKept::advised`]) — a move
    /// behind a due probe as any other, since it is the move the policy
    /// decides on: it is the one move [`IdleHost::restart`] will make.
    ///
    /// FIRST, whatever the mode: the recorder is asked again where the limit
    /// episode's open left it owed ([`Self::account_wall_again`]) — the
    /// loop asks here once a decision, so this is the session's next
    /// decision of that episode. And a move whose relaunch nobody has seen
    /// land is looked at again ([`account_land`]): landed, with this loop
    /// told to wait when it asked and still standing at the limit its
    /// session left, it is ADVISED ONCE MORE — the move the loop asks for
    /// then is answered `adopted` with nothing relaunched
    /// ([`Moved::untold`], [`Self::account_untold`]), which is how a loop
    /// that is running learns that its session has moved.
    ///
    /// LAST: A LIMIT WHOSE EVIDENCE LAGS THE SCREEN IS ASKED ABOUT AGAIN
    /// SOON ([`Self::account_lags`]). The loop's next decision at a limit
    /// nothing is advised for is that limit's reset, so a row that reaches
    /// the transcript a second after the notice would be read hours late;
    /// where the row is not written yet, or the recorder could not take the
    /// store's lock, the answer is a wait a moment away
    /// ([`account_lagging`]) — a few times an episode, then the limit is
    /// waited out as ever.
    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let owed = self.account_wall_again(point);
        let advice = self.account_advice(point, owed);
        // Read as the policy reads it ([`AccountAdvice::past_probe`]): the
        // move behind a due probe is the move it asks for.
        let advised = match advice.as_ref().and_then(AccountAdvice::past_probe) {
            Some(AccountAdvice::Move { to }) => Some(Advised {
                to: to.clone(),
                point,
            }),
            _ => None,
        };
        with_kept_of(&self.kept, |k| k.account.advised = advised);
        advice
    }

    /// THE ACCOUNT THIS SESSION MOVED TO SERVED A TURN
    /// ([`IdleHost::account_served`]): proven, where it is the active account
    /// and had not answered yet (`accounts::prove`: a `proven` row, the wake
    /// marker replaced — every session waiting on it decides again and
    /// follows). The account is the one the process's own environment names
    /// NOW, never a label remembered from the move — and only a process the
    /// roster's judgments are about ([`AccountLook::judged`]): the loop's
    /// policy remembers a move from its session's ledger, and the agent in
    /// the tab by then may be one started by hand under a key or in a home of
    /// its own, whose turn proves no account.
    ///
    /// NOT WHILE A MOVE'S LANDING IS UNREAD ([`AccountKept::flying`]): the
    /// loop was told to wait, so the move it says was served is its older
    /// one, never the process the rotation brought up — whose start-up may
    /// be all the tell read. Nothing is proven and every record is kept:
    /// where the move landed is read at the next ask ([`account_land`]), and
    /// a service told after that is the landed process's.
    ///
    /// Where nothing of the accounts acts ([`Switches::accounts_live`],
    /// [`Self::accounts_act`], read live: `accounts = "off"` with `logins =
    /// "off"`, or `off` over a roster with no row a renewal restores) NOTHING
    /// is read and nothing is recorded: the tell is this feature's, and the
    /// switches stop all of it but the recorder.
    fn account_served(&self) {
        if !self.switches.accounts_live() {
            return;
        }
        let Some((seat, roster, look)) = self.account_ctx() else {
            return;
        };
        let Some(label) = look.judged(&roster) else {
            return;
        };
        if with_kept_of(&self.kept, |k| k.account.flying.is_some()) {
            aterm_log::info!(
                "harness @{}: account {label} served: a move's landing is unread; nothing proven",
                self.sid
            );
            return;
        }
        with_kept_of(&self.kept, |k| k.account.moved = None);
        let done = self.account_transact(&seat, |state, now| {
            let mut events: Vec<accounts::Event> =
                accounts::prove(state, label, now).into_iter().collect();
            events.extend(accounts::note_session(state, &self.sid, label, None, now));
            Ok(events)
        });
        let word = match &done {
            Ok(events) if events.iter().any(|e| e.kind == accounts::Kind::Proven) => "proven",
            Ok(_) => "served",
            Err(_) => "served: not recorded",
        };
        aterm_log::info!("harness @{}: account {label} {word}", self.sid);
    }

    /// THE ACCOUNT THIS SESSION MOVED TO ANSWERED WITH A LOST LOGIN OR A
    /// USAGE LIMIT ([`IdleHost::account_answered`]): struck (`accounts::
    /// strike`: `signed-out`, or `limited` by the wall — and, where it is the
    /// active account still unproven, `active` put back to the account
    /// before it; the wake marker replaced), so the advice asked next is the
    /// next account or the way back. A USAGE LIMIT is the loop's reading of
    /// the SCREEN, which a resumed conversation may paint with the account
    /// it left's limit (never observed): it strikes only on THIS PROCESS'S
    /// OWN standing wall ([`own_wall`]), and with none it strikes nothing.
    /// Nothing is struck on the word of a process the roster's judgments are
    /// not about ([`AccountLook::judged`]): its lost login is not an
    /// account's. Nor is a LOST LOGIN struck for a process on a login a
    /// renewal superseded ([`accounts::Me::left`]): it is the account's, but
    /// the login that ended is not the row's renewed one ([`renewable_own`]),
    /// and the move onto that one is advised at the session's next point.
    /// And nothing at all where nothing of the accounts acts, as
    /// [`Self::account_served`].
    ///
    /// A LOST LOGIN TOLD IS MET HERE, for the process that told it
    /// ([`Self::lost_seen`]): the policy tells it at points it does not ask
    /// the host about (a draft in the composer, a point the upgrade holds),
    /// and an owner's sign-in after the tell and before the first ask is
    /// one made since the login was lost. A strike the store's lock keeps
    /// out is met all the same.
    ///
    /// THE MOVE IS ANSWERED ONCE ITS STRIKE IS ON RECORD: where the process
    /// in the tab is the one this host's move brought up, that record is
    /// marked so ([`Moved::answered`]), and a move made from that process
    /// goes back to the login it ran under — the loop tells nothing more of
    /// this move but its service, and the turns it serves once the wall has
    /// ended are that login's. Where nothing was recorded (a process the
    /// roster does not judge, a limit that is not this process's own, a
    /// lock held past its bound) it is not marked: the way back of a move
    /// made from that process is the login before the chain, as for a move
    /// nobody answered — never the login a strike nobody recorded left
    /// looking `ok`.
    fn account_answered(&self, bad: AccountBad) {
        if !self.switches.accounts_live() {
            return;
        }
        let Some((seat, roster, look)) = self.account_ctx() else {
            return;
        };
        let Some(label) = look.judged(&roster) else {
            return;
        };
        if matches!(bad, AccountBad::SignedOut) {
            // Its own lost login, told: not one the way back brought it up
            // under unseen ([`AccountKept::came_back`]).
            with_kept_of(&self.kept, |k| k.account.came_back = None);
            let lost = AccountPoint::Wall {
                kind: aterm_phase::WallKind::Auth,
            };
            self.lost_seen(&look.snap, lost, seat.now);
            // A process still on a login a renewal superseded is judged as
            // that row's account ([`accounts::Me::left`]): what ended is the
            // login it holds, and a strike would sign the RENEWED one out.
            if look.who.me(&roster).account.is_none() {
                aterm_log::info!(
                    "harness @{}: account {label}: a superseded login's end is no news of the \
                     renewed one's; nothing struck",
                    self.sid
                );
                return;
            }
        }
        let why = match bad {
            AccountBad::SignedOut => accounts::Strike::SignedOut,
            AccountBad::Wall => match own_wall(&seat, &look.snap) {
                Some(wall) => accounts::Strike::Wall {
                    window: wall.window,
                    until: wall.until,
                },
                None => {
                    aterm_log::info!(
                        "harness @{}: account {label}: the limit on the screen is not this \
                         process's own; nothing struck",
                        self.sid
                    );
                    return;
                }
            },
        };
        let done = self.account_transact(&seat, |state, now| {
            Ok(accounts::strike(state, label, &self.sid, &why, now))
        });
        if done.is_ok() {
            with_kept_of(&self.kept, |k| {
                if let (Some(moved), Some(tab)) = (k.account.moved.as_mut(), k.snapshot.as_ref())
                    && (moved.pid, moved.start.as_str()) == (tab.pid, tab.start.as_str())
                {
                    moved.answered = true;
                }
            });
        }
        let word = match (&why, &done) {
            (_, Err(_)) => "struck: not recorded",
            (accounts::Strike::SignedOut, _) => "struck: signed-out",
            _ => "struck: wall",
        };
        aterm_log::info!("harness @{}: account {label} {word}", self.sid);
    }

    /// NEWS OF THE ACCOUNTS since the host started ([`IdleHost::account_stamp`];
    /// [`Switches::accounts_news`]): one atomic read. It moves when the
    /// accounts store's wake marker is replaced, by any process
    /// ([`spawn_activation_wake`]) — never with no roster and `[harness]
    /// limit_reset` off, where nothing writes that marker (on, a limit
    /// reset's outcome does).
    fn account_stamp(&self) -> u64 {
        self.switches.accounts_news.load(Ordering::SeqCst)
    }

    /// THE RENEWABLE LOGIN THE SESSION RUNS ON ([`IdleHost::login_label`]):
    /// its label ([`Self::renewable_look`]).
    fn login_label(&self) -> Option<String> {
        self.renewable_look().map(|(_, _, label)| label)
    }

    /// WHETHER THE SESSION'S PROCESS HOLDS A LOGIN A RENEWAL REPLACED
    /// ([`IdleHost::login_superseded`]): the process is a renewable row's
    /// account ([`Self::renewable_look`]) on a login that is not that row's
    /// own any more ([`renewable_own`]: none) — the row bound to the renewed
    /// login, the process still on the one before it.
    fn login_superseded(&self) -> bool {
        self.renewable_look()
            .is_some_and(|(roster, look, _)| renewable_own(&roster, &look).is_none())
    }

    /// CLAUDE CODE'S `Your login expires in N days` ([`IdleHost::login_notice`]):
    /// recorded for the login the process holds (`accounts::note_login_notice`,
    /// one bounded transaction), and only where that is its renewable row's
    /// OWN login — a superseded login's notice is no news of the renewed
    /// one's. Under `[harness] logins` alone.
    fn login_notice(&self, days: u32) {
        if !self.switches.logins() {
            return;
        }
        let Some((seat, roster, look)) = self.account_ctx() else {
            return;
        };
        let Some(label) = renewable_own(&roster, &look) else {
            return;
        };
        let done = login_transact_of(&self.sid, &self.hooks, &seat, |_, logins, now| {
            accounts::note_login_notice(logins, &label, days, now);
            Ok(Vec::new())
        });
        let word = if done.is_ok() {
            "recorded"
        } else {
            "not recorded"
        };
        aterm_log::info!(
            "harness @{}: account {label}: Claude Code says its login ends in {days} day(s); {word}",
            self.sid
        );
    }

    /// THE CLAIM OF THE ACCOUNT'S ONE `/usage` PROBE
    /// ([`IdleHost::account_probe`]): [`Self::probe_claim`] — `early` read
    /// live, a listed stored login in the default home, and one bounded
    /// transaction in which the roster must still say `Probe` and the claim
    /// must take. `Ok` is the only answer on which the loop types; a refusal
    /// is its one word ([`NoProbe::as_str`]), which the loop puts on its own
    /// line about the probe it did not run (`SKIPPED … gate=claim
    /// host=<word>`).
    fn account_probe(&self) -> Result<(), &'static str> {
        self.probe_claim().map_err(NoProbe::as_str)
    }

    /// WHAT THE CLAIMED PROBE READ ([`IdleHost::account_probed`]):
    /// [`Self::probe_record`] — the sample recorded only for the process the
    /// claim was made for; a read that was no sample is the loop's one word
    /// ([`ProbeMiss::word`]), logged, and the claim let go.
    fn account_probed(&self, read: Result<&accounts::Sample, ProbeMiss>) {
        self.probe_record(read.map_err(ProbeMiss::word));
    }

    /// MAY THIS SESSION TAKE ITS ACCOUNT'S LIMIT RESET
    /// ([`IdleHost::reset_take`]): [`Self::reset_claim`] — `[harness]
    /// limit_reset` read live, the default login and no other, this
    /// process's own wall with a reset a grant can clear, one that
    /// qualifies, and one bounded transaction in which the claim must take.
    /// [`ResetWord::Take`] is the only answer on which the loop writes a key
    /// of a reset; the host writes none, here or anywhere.
    fn reset_take(&self, ask: &ResetAsk) -> ResetWord {
        self.reset_claim(ask)
    }

    /// HOW THE CLAIMED RESET ENDED ([`IdleHost::reset_told`]):
    /// [`Self::reset_tell`] — the dialog's one answer line read from the
    /// transcript since the claim, recorded only for the process the claim
    /// was made for.
    fn reset_told(&self, end: &ResetEnd) -> ResetOutcome {
        self.reset_tell(end)
    }
}

/// A worker that ends takes its session's next try with it, and the reset it
/// placed for a Codex's wall: the tab does not keep a time nobody will act on,
/// and counts a wall down in its own zone again. (A loop that ends and is
/// STARTED AGAIN on the same worker takes them too: [`worker_main`] tells
/// `None` as each run of the body ends, since this drop runs only when the
/// worker thread does.)
impl Drop for WorkerIdle {
    fn drop(&mut self) {
        (self.hooks.waiting)(&self.sid, None);
        // …and the task's face it told: nobody tells it any more.
        (self.hooks.fact)(&self.sid, crate::HarnessFact::Task(None));
        (self.hooks.placed)(&self.sid, None);
        // A worker's end ends its own silent tap.
        let silent = self
            .tap
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .silence
            .silent();
        if silent {
            self.tap_heard();
        }
    }
}

impl WorkerIdle {
    /// THE SESSION ON A RENEWABLE LOGIN ([`IdleHost::login_label`],
    /// [`IdleHost::login_superseded`]): under `[harness] logins` (read live),
    /// the roster through this machine's bindings, the look at the agent's
    /// process, and the label of the roster row with an email that its Claude
    /// Code's own login is — the row's renewed login, or one a renewal
    /// superseded — where the process runs on a stored login in the default
    /// home ([`AccountLook::judged`]). With no roster nothing of the agent is
    /// read ([`Self::account_ctx`]).
    fn renewable_look(&self) -> Option<(accounts::Roster, AccountLook, String)> {
        if !self.switches.logins() || !accounts_for(self.agent) {
            return None;
        }
        // Asked at every reading: a roster with no row to renew reads nothing
        // of the agent.
        let seat = (self.hooks.acts.account_seat)()?;
        let roster = account_roster(&seat).filter(renewable_listed)?;
        let look = self.account_look(&roster)?;
        let label = roster
            .row(look.judged(&roster)?)
            .filter(|row| row.email.is_some())?
            .label
            .clone();
        Some((roster, look, label))
    }

    /// [`IdleHost::usage_tap`]'s work, and its word for the log (`None`:
    /// nothing to say). ONLY A TAPPED PROCESS IS READ — a Claude Code whose
    /// own argv carries aterm's tap (`tap::carried_by`) — and only ITS OWN
    /// FILE: this tab's, its pid's, written since the kernel started it
    /// (`tap::Reading::read`; [`Acts::tap_born`]).
    ///
    /// 1. THE SILENCE RULE (`tap::Silence`): a process up 30 s that served a
    ///    turn, with no reading fresher than 30 s at two points 30 s apart,
    ///    is marked silent once — a `no-tap` row in the accounts ledger and,
    ///    while `[harness] usage_tap` is on, the window's one line
    ///    ([`HostNote::UsageTap`]). The line's part ends when the process
    ///    is heard again, or another holds the tab: one relaunched in place
    ///    (which brings a moved app's tap back), or one that carries no tap
    ///    (`tap::Silence::untapped`).
    /// 2. THE FRESHNESS RULE (`tap::Fresh`, §3.2): an idle process's repeat
    ///    is no reading.
    /// 3. ATTRIBUTION: the reading's login variables are the snapshot's, the
    ///    snapshot is a stored login in the default home, and the reading is
    ///    recorded under the label the agent's own process is judged on,
    ///    where no roster lists a second account ([`Self::tap_label`]) —
    ///    whichever login the one account is bound to, as the line's tap
    ///    follows it (`tap::Armed::lone_login`): the default login, a
    ///    renewal's, or the one it superseded.
    /// 4. THE RECORD (`accounts::note_reading`, in one bounded transaction):
    ///    the max rule within one reset; the state and the ledger are
    ///    written only on a change, or at the heartbeat — and a reading this
    ///    worker already took is not taken to the store's lock again before
    ///    it. One it could not take (its process's environment unread, the
    ///    store's lock held, a write refused) is tried again at the next
    ///    point ([`Self::tap_untake`]).
    fn tap_step(&self) -> Option<&'static str> {
        if !accounts_for(self.agent) {
            return None;
        }
        let snap = with_kept_of(&self.kept, |k| k.snapshot.clone())?;
        if !tap::carried_by(&snap.argv) {
            // A silent tap's process gave way to one that carries none.
            let heard = self
                .tap
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .silence
                .untapped(snap.pid);
            if heard == tap::Heard::Again {
                self.tap_heard();
            }
            return None;
        }
        let seat = (self.hooks.acts.account_seat)()?;
        let born = (self.hooks.acts.tap_born)(snap.pid)?;
        let dir = tap::rl_dir_in(&seat.state);
        let read = tap::Reading::read(&dir, &self.sid, snap.pid, born);
        let age = read
            .as_ref()
            .map(|(_, mtime)| seat.now.saturating_sub(*mtime));
        // `before`: the freshness rule as it stood before a fresh reading
        // (`None`: none came), put back where it cannot be taken.
        let (heard, before) = {
            let mut kept = self.tap.lock().unwrap_or_else(PoisonError::into_inner);
            let served = kept.served == Some(snap.pid);
            let heard = kept.silence.step(
                seat.now,
                snap.pid,
                seat.now.saturating_sub(born),
                served,
                age,
            );
            let before = kept.fresh.clone();
            let fresh = read.as_ref().is_some_and(|(r, _)| kept.fresh.admit(r));
            (heard, fresh.then_some(before))
        };
        match heard {
            tap::Heard::Silent => {
                self.tap_silent(&seat, snap.pid);
                return Some("silent");
            }
            tap::Heard::Again => self.tap_heard(),
            tap::Heard::Same => {}
        }
        let (reading, _) = read.filter(|_| before.is_some())?;
        let own = snap.stored_login
            && snap.home.is_none()
            && reading.login == snap.login
            && reading.home == snap.home;
        if !own {
            return Some(TAP_NOT_OWN);
        }
        let label = match self.tap_label(&seat, snap.pid) {
            Ok(Some(label)) => label,
            Ok(None) => {
                self.tap_untake(before);
                return None;
            }
            Err(word) => return Some(word),
        };
        let windows = (reading.five_hour, reading.seven_day);
        let taken = self
            .tap
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .noted;
        if taken.is_some_and(|(five, seven, at)| {
            (five, seven) == windows && seat.now.saturating_sub(at) < accounts::READING_HEARTBEAT_S
        }) {
            return None;
        }
        let noted = self.account_transact(&seat, |state, now| {
            Ok(accounts::note_reading(
                state,
                &label,
                reading.sample(now),
                now,
            ))
        });
        match noted {
            Ok(events) => {
                self.tap
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .noted = Some((windows.0, windows.1, seat.now));
                Some(if events.is_empty() {
                    "reading: nothing new"
                } else {
                    "reading recorded"
                })
            }
            Err(why) => {
                self.tap_untake(before);
                Some(if why == accounts::LOCK_HELD {
                    "reading dropped: lock-held"
                } else {
                    "reading dropped: not recorded"
                })
            }
        }
    }

    /// A FRESH READING THIS WORKER COULD NOT TAKE ([`Self::tap_step`]): the
    /// freshness rule as it stood before it (`before`) is put back, so the
    /// same reading — which an idle process re-reports until its next
    /// request — is tried again at the next point.
    fn tap_untake(&self, before: Option<tap::Fresh>) {
        if let Some(before) = before {
            self.tap
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .fresh = before;
        }
    }

    /// A silent tap's mark ends ([`tap::Heard::Again`]; the worker's end):
    /// the window's line loses this tab's part.
    fn tap_heard(&self) {
        (self.hooks.note)(HostNote::UsageTap {
            sid: self.sid.clone(),
            silent: false,
        });
    }

    /// WHOSE A USAGE TAP'S RECORD IS — a reading ([`Self::tap_step`]), a
    /// silence ([`Self::tap_silent`]) of the agent process `pid`: THE LABEL
    /// THAT PROCESS IS JUDGED ON ([`Self::account_look`]: its exec
    /// environment, its route, its settings and the vendor's profile store),
    /// WHERE NO ROSTER LISTS A SECOND ACCOUNT ([`account_roster_or_own`]: the
    /// machine's own row where none lists any). One account can move no
    /// session — there is no candidate to rotate to, and nothing is probed —
    /// so its figures are recorded where the roster lists it alone: R-7's
    /// own enrolment is one row. Under a roster of several, a reading could
    /// move a session under `[harness] accounts`, and is nobody's. `Err` with the
    /// log's word where the record is nobody's; `Ok(None)` where the process
    /// could not be looked at just now.
    fn tap_label(&self, seat: &AccountSeat, pid: u32) -> Result<Option<String>, &'static str> {
        let roster = account_roster_or_own(seat, true)
            .filter(|roster| !roster.several())
            .ok_or(TAP_NOT_OWN)?;
        let Some(look) = self.account_look(&roster) else {
            return Ok(None);
        };
        look.judged(&roster)
            .filter(|_| look.snap.pid == pid)
            .map(|label| Some(label.to_owned()))
            .ok_or(TAP_NOT_OWN)
    }

    /// A TAPPED PROCESS WENT SILENT ([`Self::tap_step`]): the accounts
    /// ledger's `no-tap` row, under the label its readings are recorded on
    /// ([`Self::tap_label`]: none where they are nobody's), and — while
    /// `[harness] usage_tap` is on — the window's line
    /// ([`HostNote::UsageTap`]).
    fn tap_silent(&self, seat: &AccountSeat, pid: u32) {
        if let Ok(Some(label)) = self.tap_label(seat, pid) {
            let _ = self.account_transact(seat, |_, now| {
                Ok(vec![
                    accounts::Event::new(now, accounts::Kind::NoTap, &label)
                        .because("a served turn left no reading"),
                ])
            });
        }
        if self.switches.usage_tap.load(Ordering::SeqCst) {
            (self.hooks.note)(HostNote::UsageTap {
                sid: self.sid.clone(),
                silent: true,
            });
        }
    }

    /// THE HOST'S STEP at an idle point ([`IdleHost::at_idle`], which parks
    /// again after it while the conversation is unnamed): the relaunch
    /// record read again, then the upgrade's step or a relaunched agent's
    /// owed carry-on.
    fn idle_step(&self) -> Option<HostStep> {
        if !self.on() || !upgrades(self.agent) {
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
            if !self.on() {
                return None;
            }
            // Types into the agent, never ends it: its exit, meanwhile, is one.
            ("carry-on", (hooks.acts.resume)(&self.sid, self.grace))
        } else {
            let due = due();
            if !self.on() {
                return None;
            }
            match due {
                Due::Yes => {
                    if !self.on() {
                        return None;
                    }
                    self.act();
                    let step = (hooks.acts.step)(&self.sid, self.grace);
                    self.acted();
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
        if !self.on() {
            return None;
        }
        run.unread = 0;
        run.quiet = !self.switches.ask();
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
                self.upgrade_runs(&mut run);
                self.park.store(true, Ordering::SeqCst);
            }
            After::Later(pause) => {
                self.upgrade_runs(&mut run);
                *self.look_at.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(Instant::now() + (hooks.pause)(pause));
                ring();
            }
            After::Finished => {
                self.upgrade_runs(&mut run);
            }
            After::Broken => {
                if !std::mem::replace(&mut run.broken, true) {
                    aterm_log::warn!(
                        "harness @{}: the live upgrade cannot run: {step} (its state directory \
                         cannot hold the lock); it is tried again at the next activation notice",
                        self.sid
                    );
                    // The owner's line, from the host that observed it: no row of a
                    // tab exists to classify (`Need::upgrade_cannot_run`).
                    (self.hooks.note)(HostNote::UpgradeCannotRun {
                        sid: self.sid.clone(),
                        step: step.clone(),
                        dir: harness_state().map(|state| state.join("upgrade")),
                    });
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

    /// The upgrade runs in this tab (a step that is not `Broken`): a fact told
    /// as `UpgradeCannotRun` ends, and the first fine step of a run says so even
    /// when this run never told it (the line may be one an earlier process
    /// raised and this window adopted).
    fn upgrade_runs(&self, run: &mut UpgradeRun) {
        let was_broken = std::mem::replace(&mut run.broken, false);
        if !std::mem::replace(&mut run.runs_told, true) || was_broken {
            (self.hooks.note)(HostNote::UpgradeRuns {
                sid: self.sid.clone(),
            });
        }
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

    /// ...and ends: its stamp moved again BEFORE `acting` clears, then the
    /// host rung. A pass whose roster read fell inside the step — and missed
    /// the agent the step ended and relaunched — finds the stamp moved since
    /// that read even once the step is over ([`StepsSeen::stepping`]), and
    /// leaves the absence to the pass this ring brings, which reads the
    /// roster after the step.
    fn acted(&self) {
        self.stepped.store(
            STEP_STAMPS.fetch_add(1, Ordering::SeqCst) + 1,
            Ordering::SeqCst,
        );
        self.acting.store(false, Ordering::SeqCst);
        ring();
    }

    fn on(&self) -> bool {
        !self.stop.load(Ordering::SeqCst) && self.switches.on_for(self.agent)
    }

    /// THE AGENT'S ROUTE ([`Acts::route`]), read once per agent process (its
    /// pid and kernel start) and directory — again when the directory is
    /// filled in — outside every lock, and kept: what [`IdleHost::reach`]
    /// measures by, and what the accounts recorder is told
    /// ([`Self::account_wall`]).
    fn route_of(&self, pid: u32, start: &str, argv: &[String], cwd: &str) -> Route {
        let cached = self
            .route
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|(p, s, c, _)| *p == pid && s == start && c == cwd)
            .map(|(_, _, _, route)| route.clone());
        if let Some(route) = cached {
            return route;
        }
        let route = (self.hooks.acts.route)(pid, argv, cwd);
        if let Route::Custom(why) = &route {
            aterm_log::info!(
                "harness @{}: the API's reach is not measured for pid {pid}: {why}",
                self.sid
            );
        }
        *self.route.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((pid, start.to_string(), cwd.to_string(), route.clone()));
        route
    }

    /// THE SESSION'S OWN USAGE WALL, FOR ITS ACCOUNT (the accounts design, §5
    /// "What counts as an observation", §12 slice 2), as the loop's limit
    /// episode opens ([`IdleHost::limited`]): the Claude Code the worker
    /// keeps — its process and the conversation it holds — is handed to the
    /// recorder ([`Acts::account_wall`]), with its route for the asking
    /// ([`Self::route_of`], the read [`IdleHost::reach`] keeps: a route that
    /// is not the default one is no stored login's). Whatever `[harness]
    /// upgrade` says. Another agent has no account here; no snapshot, or a
    /// conversation Claude has not registered yet, has no transcript to read
    /// the wall from, and nothing is asked (`unasked`). One line of the log:
    /// the recorder's word and the account's roster label, nothing else. The
    /// word is answered, for the asking again ([`Self::limited`]).
    fn account_wall(&self) -> &'static str {
        if !accounts_for(self.agent) {
            return "unasked";
        }
        let agent = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .clone()
            .and_then(|s| Some((s.session.clone()?, s)));
        let Some((session, snap)) = agent else {
            return "unasked";
        };
        let settled = || self.settled(snap.pid, &snap.start, &snap.argv, &snap.cwd);
        let walled = WalledAgent {
            snap: &snap,
            session: &session,
            own: self.switches.notes(),
        };
        let (word, label) = (self.hooks.acts.account_wall)(&self.sid, &walled, &settled);
        match label {
            Some(label) => {
                aterm_log::info!("harness @{}: account wall {word} {label}", self.sid);
            }
            None => aterm_log::info!("harness @{}: account wall {word}", self.sid),
        }
        word
    }

    /// THE RECORDER, ASKED AGAIN at the session's next decision of the limit
    /// episode that stands ([`IdleHost::account`] is asked once a decision):
    /// only at a usage limit, only while the episode's open left it owed
    /// ([`AccountKept::wall_again`], [`WALL_AGAIN`] times at most), and no
    /// more once it answers a word that is final ([`wall_asked_again`]). A
    /// limit row that reached the transcript a moment after the screen
    /// showed the notice is recorded here, for the account and for the
    /// rotation this very decision is advised on; a lock that was held is
    /// tried again. With no roster and `[harness] usage_notes` off, the
    /// recorder's first word is `not-listed` and nothing is asked again;
    /// under `usage_notes` the machine's own row stands for the roster, and a
    /// `no-wall` or `lock-held` is asked again as for a listed account.
    ///
    /// Whether the recorder, asked here, STILL COULD NOT SAY (the row not
    /// written yet, the lock held): the evidence lags, and the decision
    /// this is asked in is worth making again soon ([`Self::account_lags`]).
    fn account_wall_again(&self, point: AccountPoint) -> bool {
        if !usage_limit(point) {
            return false;
        }
        let owed = with_kept_of(&self.kept, |k| {
            let owed = k.account.wall_again > 0;
            k.account.wall_again = k.account.wall_again.saturating_sub(1);
            owed
        });
        if !owed {
            return false;
        }
        let again = wall_asked_again(self.account_wall());
        if !again {
            with_kept_of(&self.kept, |k| k.account.wall_again = 0);
        }
        again
    }

    /// THE MOVE THIS LOOP IS STILL TO BE TOLD OF ([`Moved::untold`]), by its
    /// label — and only WHERE THE LOOP STANDS AT THE LIMIT ITS SESSION LEFT
    /// (`left`: a usage limit on its screen that the process in the tab did
    /// not write), with that process the one the move brought up (`snap`).
    /// That is the loop the telling is for: it waits out a notice that is no
    /// longer its session's, and holds back for it the wall's resume it owes
    /// the relaunched agent. ANYWHERE ELSE — a plain turn end, a limit this
    /// process wrote, another process in the tab — the session has gone on
    /// by itself: the mark is dropped, and nothing is told. (An `adopted`
    /// there would be followed by nothing typed — the relaunch is behind it —
    /// and the point's own act would be lost.)
    fn account_untold(&self, snap: &Snapshot, left: bool) -> Option<String> {
        with_kept_of(&self.kept, |k| {
            let moved = k.account.moved.as_mut().filter(|m| m.untold)?;
            let here = (moved.pid, moved.start.as_str()) == (snap.pid, snap.start.as_str());
            if here && left {
                return Some(moved.to.clone());
            }
            moved.untold = false;
            None
        })
    }

    /// WHETHER THE LOOP IS TOLD TO ASK AGAIN SOON ([`account_lagging`]): the
    /// evidence of the limit on the screen LAGS it — `lags`: the transcript
    /// holds no limit row yet, or the recorder could not take the store's
    /// lock just now — and fewer than [`WALL_AGAIN`] such answers were given
    /// in this limit episode ([`AccountKept::lagged`], counted here). A row
    /// that never comes (a notice the resumed conversation repainted, a
    /// transcript nobody can read) costs that many decisions a minute apart,
    /// and then the limit is waited out as it was before accounts existed.
    fn account_lags(&self, lags: bool) -> bool {
        lags && with_kept_of(&self.kept, |k| {
            let more = k.account.lagged < WALL_AGAIN;
            if more {
                k.account.lagged += 1;
            }
            more
        })
    }

    /// THE AGENT'S SETTINGS SIDE OF ITS LOGIN: its route is the default one
    /// ([`Self::route_of`], the read [`IdleHost::reach`] keeps) AND its
    /// settings files and the vendor's profile store leave it its stored
    /// login ([`Acts::account_settings`]: a credential in a settings `env`
    /// block, an `apiKeyHelper`, a profile, a source nobody could read).
    /// `false` makes it no stored login, whatever its environment says.
    /// Read once per agent process (its pid and kernel start) and directory,
    /// outside every lock, and kept ([`AccountKept::settled`]). Asked only
    /// for a process a row of the roster names — the machine's own row under
    /// `[harness] usage_notes` where none is listed — so with no roster and
    /// no `usage_notes` no settings file is read for this.
    fn settled(&self, pid: u32, start: &str, argv: &[String], cwd: &str) -> bool {
        if self.route_of(pid, start, argv, cwd) != Route::Default {
            return false;
        }
        let kept = with_kept_of(&self.kept, |k| {
            k.account
                .settled
                .as_ref()
                .filter(|(p, s, c, _)| *p == pid && s == start && c == cwd)
                .map(|(_, _, _, stored)| *stored)
        });
        if let Some(stored) = kept {
            return stored;
        }
        let stored = (self.hooks.acts.account_settings)(pid, argv, cwd);
        if !stored {
            aterm_log::info!(
                "harness @{}: pid {pid} is no stored login by its settings: no account is its",
                self.sid
            );
        }
        with_kept_of(&self.kept, |k| {
            k.account.settled = Some((pid, start.to_string(), cwd.to_string(), stored));
        });
        stored
    }

    /// A RENEWABLE LOGIN'S OWN END ([`accounts::Point::LoginEnded`]), at the
    /// advice it is asked for: recorded for the row whose OWN login the
    /// process holds ([`renewable_own`]) — signed out, the end kept for the
    /// forecast (`accounts::note_login_end`) — in one bounded transaction
    /// whose rows wake nobody. Asked again at the same wall it writes
    /// nothing more.
    fn login_ended(&self, seat: &AccountSeat, roster: &accounts::Roster, look: &AccountLook) {
        let Some(label) = renewable_own(roster, look) else {
            return;
        };
        let done = login_transact_of(&self.sid, &self.hooks, seat, |state, logins, now| {
            Ok(accounts::note_login_end(state, logins, &label, now))
        });
        match done {
            Ok(events) if events.iter().any(|e| e.kind == accounts::Kind::LoginEnded) => {
                aterm_log::info!("harness @{}: account {label}: its login ended", self.sid);
            }
            Ok(_) => {}
            Err(_) => {
                aterm_log::info!(
                    "harness @{}: account {label}: its login's end not recorded: refused",
                    self.sid
                );
            }
        }
    }

    /// A TURN SERVED UNDER A RENEWABLE LOGIN: what makes it `ok`
    /// (`accounts::note_served_login`), for the row whose OWN login the
    /// process holds, at most once an hour ([`ACCOUNT_ROW_STALE_S`], the
    /// session row's own cadence), the logins read without the lock first.
    fn login_served(&self, seat: &AccountSeat, roster: &accounts::Roster, look: &AccountLook) {
        let Some(label) = renewable_own(roster, look) else {
            return;
        };
        let fresh = accounts::Store::at(&seat.state)
            .load_logins(seat.now)
            .ok()
            .and_then(|(logins, _)| logins.get(&label).and_then(|b| b.served_at))
            .is_some_and(|at| (0..ACCOUNT_ROW_STALE_S).contains(&seat.now.saturating_sub(at)));
        if fresh {
            return;
        }
        let done = login_transact_of(&self.sid, &self.hooks, seat, |_, logins, now| {
            accounts::note_served_login(logins, &label, now);
            Ok(Vec::new())
        });
        if done.is_err() {
            aterm_log::info!(
                "harness @{}: account {label}: served, not recorded",
                self.sid
            );
        }
    }

    /// THE ACCOUNTS AS THIS HOST HAS THEM, AND THIS SESSION'S AGENT AMONG
    /// THEM: the seat ([`Acts::account_seat`]), its roster WHERE IT LISTS AN
    /// ACCOUNT ([`account_roster`]) and what the agent's own process says of
    /// its login ([`Self::account_look`]). `None` — and nothing of the agent
    /// read — with no seat or no roster.
    fn account_ctx(&self) -> Option<(AccountSeat, accounts::Roster, AccountLook)> {
        if !accounts_for(self.agent) {
            return None;
        }
        let seat = (self.hooks.acts.account_seat)()?;
        let roster = account_roster(&seat)?;
        if !self.accounts_act(&roster) {
            return None;
        }
        let look = self.account_look(&roster)?;
        Some((seat, roster, look))
    }

    /// WHETHER ANYTHING OF THE ACCOUNTS ACTS FOR THIS SESSION under
    /// `roster`, the switches read live: the rotation on (`[harness]
    /// accounts` not off), or the login lifecycle on (`logins`) with a row
    /// it renews ([`renewable_listed`]). With neither nothing of the agent is
    /// read: `accounts = "off"` over a roster with no email is as inert as it
    /// was before renewal existed.
    fn accounts_act(&self, roster: &accounts::Roster) -> bool {
        self.switches.accounts() != accounts::Mode::Off
            || (self.switches.logins() && renewable_listed(roster))
    }

    /// WHEN THIS HOST FIRST MET THE LOST LOGIN OF THE PROCESS `snap` IS
    /// ([`AccountKept::lost`]): at a lost login, the instant kept for that
    /// process — or, where none is, `now`, kept: this is the first ask about
    /// it, or its tell ([`IdleHost::account_answered`]), whichever came
    /// first. The roster reads only a word about the login written AFTER it
    /// as the owner's sign-in since (`accounts::Point::LostLogin`), so the
    /// login's `ok` from its enrolment, or from before the move, is never
    /// taken for one — whether or not the strike of this lost login is on
    /// record yet. Every ask of that process's lost login reads the one
    /// instant, the commit's re-advice included; another process's is met
    /// anew. At any other point nothing is kept, and `now` is returned
    /// unread.
    ///
    /// A PROCESS THE WAY BACK BROUGHT UP UNDER A LOGIN NOBODY SAW LOST
    /// ([`AccountKept::came_back`]) meets its first lost login as long ago
    /// (`i64::MIN`): the row is the moved process's, repainted, and the
    /// login's standing `ok` is no word the lost login has overruled — the
    /// roster moves the session back under its own login, whose resume then
    /// tests it (a bound of one relaunch, should that login be gone too).
    /// The mark is taken at any ask, so it never outlives that first one.
    fn lost_seen(&self, snap: &Snapshot, point: AccountPoint, now: i64) -> i64 {
        let lost_login = matches!(
            point,
            AccountPoint::Wall {
                kind: aterm_phase::WallKind::Auth
            }
        );
        with_kept_of(&self.kept, |k| {
            let came_back = k.account.came_back.take();
            if !lost_login {
                return now;
            }
            match &k.account.lost {
                Some(lost)
                    if (lost.pid, lost.start.as_str()) == (snap.pid, snap.start.as_str()) =>
                {
                    lost.at
                }
                _ => {
                    let at = if came_back.as_ref() == Some(&snap.login) {
                        i64::MIN
                    } else {
                        now
                    };
                    k.account.lost = Some(LostSeen {
                        pid: snap.pid,
                        start: snap.start.clone(),
                        at,
                    });
                    at
                }
            }
        })
    }

    /// WHAT THE AGENT'S OWN PROCESS SAYS OF ITS LOGIN ([`AccountLook`]; the
    /// recorder's read, [`account_wall_of`]): the Claude Code the worker
    /// keeps a snapshot of, its exec environment read through
    /// [`Acts::account_env`] (one read; an environment that reads EMPTY was
    /// not read, and is never the default login) and — only for a login that
    /// could be a row's stored login — its settings side ([`Self::settled`]:
    /// a route that is not the default one, a credential or an
    /// `apiKeyHelper` in a settings file and the vendor's profile store each
    /// make it no stored login). `None`: no snapshot yet, or an environment
    /// that cannot be read. Called only with a roster in hand — the machine's
    /// own row where none lists an account, for a limit reset and the usage
    /// tap ([`account_roster_or_own`]).
    fn account_look(&self, roster: &accounts::Roster) -> Option<AccountLook> {
        let snap = with_kept_of(&self.kept, |k| k.snapshot.clone())?;
        let who = exec_login(&self.hooks, snap.pid)?;
        let listed = accounts::judged_label(roster, &who).is_some();
        let who = if listed {
            accounts::ProcessLogin {
                stored_login: self.settled(snap.pid, &snap.start, &snap.argv, &snap.cwd),
                ..who
            }
        } else {
            who
        };
        let label = who.me(roster).label().map(str::to_owned);
        Some(AccountLook { who, label, snap })
    }

    /// [`IdleHost::account`]'s judgment, before it is kept. `owed`: the
    /// recorder, asked again at this decision, still could not say
    /// ([`Self::account_wall_again`]).
    ///
    /// THE WINDOW IS TOLD WHAT THE ACCOUNTS AS A WHOLE SAY, from the files
    /// this judgment reads anyway ([`accounts_tell`], once per change):
    /// nothing with the rotation and the login lifecycle off (no file is
    /// read; [`Switches::accounts_live`]), what a roster that lists no account
    /// says ([`roster_read`]), and with the state in hand what the accounts
    /// say — where that state was read whole ([`accounts_said`]'s rule).
    /// UNDER `[harness] usage_notes` the machine's own row stands for a
    /// roster that lists none, and its lines are never told `Nothing` (the
    /// review of S5: the credits line ended at every turn end): with no
    /// account listed this tells the look ([`accounts_said`]), and with both
    /// switches off nothing — the switch's own look says it
    /// ([`HostHandle::set_config`]).
    fn account_advice(&self, point: AccountPoint, owed: bool) -> Option<AccountAdvice> {
        let mode = self.switches.accounts();
        let notes = self.switches.notes();
        if !self.switches.accounts_live() {
            if !notes {
                accounts_tell(&self.switches, &self.hooks, AccountsSaid::Nothing, false);
            }
            return None;
        }
        // A move left in flight, or not read when it was adopted: where it
        // landed is settled before anything is advised on it.
        account_land(&self.sid, &self.kept, &self.hooks, Landing::Asked);
        // [`Self::account_ctx`], with the roster's word for the window where
        // it lists no account.
        if !accounts_for(self.agent) {
            return None;
        }
        let seat = (self.hooks.acts.account_seat)()?;
        let roster = match roster_read(&seat) {
            Ok(roster) => roster,
            Err(said) => {
                // No account listed: with `[harness] limit_reset` on (read
                // live) the machine's own row is advised by what a limit
                // reset left on record, and by nothing else.
                let none = said == AccountsSaid::Nothing;
                let own = none && self.switches.reset_on();
                let said = if none && notes {
                    accounts_said(
                        mode != accounts::Mode::Off,
                        self.switches.logins(),
                        notes,
                        &seat,
                    )
                } else {
                    Some(said)
                };
                if let Some(said) = said {
                    accounts_tell(&self.switches, &self.hooks, said, false);
                }
                return own.then(|| self.reset_advice(&seat, point)).flatten();
            }
        };
        if !self.accounts_act(&roster) {
            accounts_tell(&self.switches, &self.hooks, AccountsSaid::Nothing, false);
            return None;
        }
        let look = self.account_look(&roster)?;
        let seen = self.lost_seen(&look.snap, point, seat.now);
        let (at, wall, unwritten) = account_point_read(&seat, &look, point, seen);
        // …and one that landed with this loop told to wait is told now, as
        // the move it asked for ([`Moved::untold`]): the same label, once
        // more.
        if let Some(to) = self.account_untold(&look.snap, usage_limit(point) && wall.is_none()) {
            aterm_log::info!(
                "harness @{}: account advice: its move to {to} has landed; told as made",
                self.sid
            );
            return Some(AccountAdvice::Move { to });
        }
        let store = accounts::Store::at(&seat.state);
        // A RENEWABLE LOGIN'S OWN END, seen in a session on that very login
        // (the row's renewed one: a superseded login's end is no news of the
        // renewed one's), is recorded before anything is advised on it — the
        // login signed out, its end kept for the forecast.
        if at == accounts::Point::LoginEnded {
            self.login_ended(&seat, &roster, &look);
        }
        // WITHOUT THE LOCK: the state is replaced by a rename, so this reads
        // the file as it was or as it is. What is decided on it is advice;
        // the move itself is decided again under the lock.
        let (state, unread) = store.load(seat.now).ok()?;
        let enrolled = accounts::enrolled(&roster);
        if unread.is_none()
            && let Ok((logins, None)) = store.load_logins(seat.now)
        {
            accounts_tell(
                &self.switches,
                &self.hooks,
                AccountsSaid::listed(&roster, &state, &logins, seat.now),
                false,
            );
        }
        if at == accounts::Point::TurnEnd {
            self.login_served(&seat, &roster, &look);
        }
        let me = asked_at(look.who.me(&roster), point, self.switches.logins());
        let advice = accounts::advise(&roster, &state, &enrolled, &me, &at, mode, seat.now);
        // WHAT A DUE PROBE STANDS IN FRONT OF (`early` alone answers one): the
        // roster says `Probe` before it asks whether the account is one to
        // leave, so it is asked once more past the probe, on the same state
        // and second. The policy decides on that; whether anybody makes the
        // probe is the loop's ([`Self::probe_claim`]).
        let past = matches!(advice, accounts::Advice::Probe).then(|| {
            accounts::advise_past_probe(&roster, &state, &enrolled, &me, &at, mode, seat.now)
        });
        self.account_note(&seat, &roster, &look, &state);
        // THE LOOP'S point, not the roster's: a session at a limit an earlier
        // process wrote is asked about as a plain turn end ([`account_point`])
        // and still WAITS at that limit, so an account ahead that has not
        // answered yet is a wait until its canary's hold ends there too —
        // else it would sleep to the old limit's reset before taking the
        // canary's place.
        let at_wall = point.at_wall();
        let opening = accounts::next_opening(&roster, &state, &enrolled, seat.now);
        let advised = account_advice_of(
            &advice,
            past.as_ref(),
            &state,
            opening,
            at_wall,
            seat.now,
            Instant::now(),
        );
        // A LIMIT RESET IN FLIGHT ON THIS SESSION'S ACCOUNT (`[harness]
        // limit_reset`, read live; `accounts::reset_in_flight` on the state
        // just read): nobody is moved off it meanwhile, and the point is
        // asked about again a moment on ([`account_resetting`]).
        if self.switches.reset_on()
            && let Some(label) = look.judged(&roster)
            && accounts::reset_in_flight(&state, label, seat.now).is_some()
        {
            aterm_log::info!(
                "harness @{}: account advice: a limit reset is in flight on {label}; nobody moves \
                 off it, asked again shortly",
                self.sid
            );
            return account_resetting(advised, Instant::now() + RESET_AGAIN_AFTER);
        }
        // One line of the log: a move and a wait as they are advised; why
        // NOTHING is advised only when the reason changes ([`AccountKept::stay`]:
        // a session asks at every turn end, and says `nothing-due` once). It
        // is said of what the point is DECIDED on — the roster's word past a
        // due probe and its mapping — and a due probe with nothing behind it
        // is `probe: due`.
        let due = past.is_some();
        let (word, decided) = (
            past.as_ref().unwrap_or(&advice),
            advised.as_ref().and_then(AccountAdvice::past_probe),
        );
        let stay = match (word, decided) {
            (accounts::Advice::Stay(_), None) if due => Some("probe: due"),
            (accounts::Advice::Stay(why), None) => Some(why.as_str()),
            _ => None,
        };
        let said = with_kept_of(&self.kept, |k| std::mem::replace(&mut k.account.stay, stay));
        match (word, decided) {
            (accounts::Advice::Move { to, .. }, _) => {
                aterm_log::info!("harness @{}: account advice: move to {to}", self.sid);
            }
            (accounts::Advice::Resume { .. }, Some(_)) => aterm_log::info!(
                "harness @{}: account advice: resume (the owner cleared this account)",
                self.sid
            ),
            (accounts::Advice::Stay(why), Some(_)) => aterm_log::info!(
                "harness @{}: account advice: wait ({})",
                self.sid,
                why.as_str()
            ),
            (_, Some(_)) => aterm_log::info!("harness @{}: account advice: wait", self.sid),
            _ => {
                if let Some(why) = stay.filter(|why| said != Some(*why)) {
                    aterm_log::info!("harness @{}: account advice: stay ({why})", self.sid);
                }
            }
        }
        // A LIMIT WHOSE EVIDENCE LAGS ITS SCREEN ([`evidence_lags`]), where
        // nothing is advised that decides the point: asked about again a
        // moment on ([`account_lagging`]).
        let lags = evidence_lags(point, look.judged(&roster).is_some(), unwritten, owed);
        if matches!(advised, None | Some(AccountAdvice::Wait { .. })) && self.account_lags(lags) {
            aterm_log::info!(
                "harness @{}: account advice: the limit's record is not there yet; asked again \
                 shortly",
                self.sid
            );
            return account_lagging(advised, Instant::now() + WALL_AGAIN_AFTER);
        }
        advised
    }

    /// THE SESSION'S ROW, FOR THE REPORT (`aterm harness accounts`: `on:
    /// @s-… (held: jobs)`): written — `accounts::note_session`, under the
    /// store's lock, for the read-edit-write alone — only where the row the
    /// lockless read found is MISSING OR STALE: another account, another
    /// `held` word than the host's last refusal ([`AccountKept::held`]), or
    /// older than [`ACCOUNT_ROW_STALE_S`]. So a session whose every turn end
    /// asks for advice writes its row once an hour, not once a turn. Only
    /// for a session the roster's judgments are about ([`AccountLook::judged`]:
    /// a listed, stored login in the default home), as the recorder's rule is.
    fn account_note(
        &self,
        seat: &AccountSeat,
        roster: &accounts::Roster,
        look: &AccountLook,
        state: &accounts::State,
    ) {
        let Some(label) = look.judged(roster) else {
            return;
        };
        let held = with_kept_of(&self.kept, |k| k.account.held);
        let fresh = state.sessions.get(&self.sid).is_some_and(|row| {
            row.account == label
                && row.held.as_deref() == held
                && seat.now.saturating_sub(row.at) < ACCOUNT_ROW_STALE_S
        });
        if fresh {
            return;
        }
        if self
            .account_transact(seat, |state, now| {
                Ok(accounts::note_session(state, &self.sid, label, held, now))
            })
            .is_err()
        {
            // The reason names files under the owner's state directory.
            aterm_log::info!("harness @{}: account session not noted: refused", self.sid);
        }
    }

    /// ONE SHORT TRANSACTION on the accounts state, for this session
    /// ([`account_transact_of`]) — after the outcome of a limit reset this
    /// session still holds, if any ([`Self::reset_flush`]).
    fn account_transact(
        &self,
        seat: &AccountSeat,
        edit: impl FnOnce(&mut accounts::State, i64) -> Result<Vec<accounts::Event>, String>,
    ) -> Result<Vec<accounts::Event>, String> {
        // A limit reset's outcome the lock kept out goes first, in a
        // transaction of its own: `edit` may refuse, and a refusal writes
        // nothing.
        self.reset_flush(seat);
        account_transact_of(&self.sid, &self.hooks, seat, edit)
    }

    /// THE CLAIM OF THIS SESSION'S ACCOUNT'S ONE `/usage` PROBE (what the
    /// loop asks before it types anything — [`IdleHost::account_probe`]; the
    /// probe's spec, P22, P23 and P27): `Ok`, and then the loop types
    /// `/usage`, only where EVERY ONE of these holds, in this order — else
    /// the refusal by its name ([`NoProbe`]), which is logged here and handed
    /// to the loop for its own line (`SKIPPED … gate=claim host=<word>`) —
    ///
    /// 1. `[harness] accounts`, READ LIVE, is `early`: `wall` and `off`
    ///    never probe, and the loop cannot tell `early` from `wall`
    ///    ([`effective`]). Nothing is read otherwise.
    /// 2. The owner listed an account and the agent is a Claude Code whose
    ///    process is read ([`Self::account_ctx`]) — with no roster nothing
    ///    of the agent is read, nothing is made on disk and nothing is
    ///    locked — and that process is one the roster's judgments are about
    ///    ([`AccountLook::judged`]: a listed, STORED login in the DEFAULT
    ///    home). A panel read under a key, a route or a home of its own
    ///    shows no listed account's windows.
    /// 3. ONE BOUNDED TRANSACTION under the store's lock ([`NoProbe`] names
    ///    each refusal): no claim stands in the way (`accounts::probe_held`:
    ///    this account's within `accounts::PROBE_MIN_S`, ANY account's
    ///    within `accounts::PROBE_APART_S`); `accounts::advise`, asked
    ///    AFRESH on the state under the lock at a plain turn end, STILL says
    ///    `Probe` — the advice the loop holds was read without the lock, a
    ///    decision ago, and the roster never says it for a pinned machine, a
    ///    walled account or the active account that has not answered yet;
    ///    and `accounts::claim_probe` takes, which writes the ledger's
    ///    `probe` row. A refusal writes nothing. Nothing waits inside it,
    ///    and a lock that stays held is a refusal ([`account_transact_of`]).
    ///
    /// The claim is kept WITH THE PROCESS IT WAS MADE FOR ([`Probing`]: its
    /// label, pid and kernel start), for the read the loop tells next
    /// ([`Self::probe_record`]). A refusal drops whatever claim was kept.
    /// The host types nothing, here or anywhere in the probe.
    fn probe_claim(&self) -> Result<(), NoProbe> {
        let claimed = self.probe_claimed();
        match &claimed {
            Ok(probing) => aterm_log::info!(
                "harness @{}: account probe claimed for {}",
                self.sid,
                probing.label
            ),
            Err(why) => aterm_log::info!(
                "harness @{}: account probe not claimed: {}",
                self.sid,
                why.as_str()
            ),
        }
        let (probing, taken) = match claimed {
            Ok(probing) => (Some(probing), Ok(())),
            Err(why) => (None, Err(why)),
        };
        with_kept_of(&self.kept, |k| k.account.probing = probing);
        taken
    }

    /// [`Self::probe_claim`]'s decision: the claim taken, with the process it
    /// is for, or why not ([`NoProbe`]).
    fn probe_claimed(&self) -> Result<Probing, NoProbe> {
        /// The transaction's two refusals of its own: nothing is written.
        const CLAIMED: &str = "probe-claimed";
        const NOT_DUE: &str = "probe-not-due";
        if self.switches.accounts() != accounts::Mode::Early {
            return Err(NoProbe::Mode);
        }
        let (seat, roster, look) = self.account_ctx().ok_or(NoProbe::Unread)?;
        let label = look.judged(&roster).ok_or(NoProbe::NotJudged)?.to_owned();
        let enrolled = accounts::enrolled(&roster);
        let me = look.who.me(&roster);
        let claimed = self.account_transact(&seat, |state, now| {
            // The claim's own test first, for its own word: the roster's
            // advice folds a standing claim into "nothing due".
            if accounts::probe_held(state, &label, now) {
                return Err(CLAIMED.to_string());
            }
            let advice = accounts::advise(
                &roster,
                state,
                &enrolled,
                &me,
                &accounts::Point::TurnEnd,
                accounts::Mode::Early,
                now,
            );
            if advice != accounts::Advice::Probe {
                return Err(NOT_DUE.to_string());
            }
            accounts::claim_probe(state, &label, &self.sid, now)
                .map(|event| vec![event])
                .ok_or_else(|| CLAIMED.to_string())
        });
        match claimed {
            Ok(_) => Ok(Probing {
                label,
                pid: look.snap.pid,
                start: look.snap.start,
            }),
            Err(why) if why == CLAIMED => Err(NoProbe::Claimed),
            Err(why) if why == NOT_DUE => Err(NoProbe::NotDue),
            Err(why) if why == accounts::LOCK_HELD => Err(NoProbe::LockHeld),
            Err(_) => Err(NoProbe::State),
        }
    }

    /// WHAT THE CLAIMED PROBE READ (what the loop tells exactly once after a
    /// claim it was given — [`IdleHost::account_probed`]; the probe's spec,
    /// P22): a settled panel's sample, its resets placed by the loop (which
    /// has the zone seams), or the one word for why there is none. The
    /// claim is TAKEN here, whatever was read; a read told with no claim
    /// standing is recorded for nobody.
    ///
    /// A SAMPLE IS RECORDED ONLY FOR THE PROCESS THE CLAIM WAS MADE FOR. The
    /// agent is read again ([`Self::account_ctx`]): the label the roster's
    /// judgments are about, the pid and the kernel start must be those
    /// claimed ([`Probing`]). A relaunch between the claim and the read — an
    /// upgrade, an account move, an exit — leaves another process in the
    /// tab, perhaps under another login, and the panel that was read is not
    /// known to be its account's: the sample is dropped. So is one told
    /// under `[harness] accounts = "off"` (read live: the switch stops all
    /// of the feature but the recorder), and one that cannot be read as
    /// anybody's.
    ///
    /// Recorded in ONE BOUNDED TRANSACTION (`accounts::note_sample`, stamped
    /// with the transaction's own second; the ledger's `sample` row): the
    /// library drops a sample with neither figure, or no newer than the one
    /// held. A lock that stays held DROPS the sample, and the log says so —
    /// the next probe is a few minutes off, and a figure kept to be written
    /// later would be written stale. No sample moves the wake marker
    /// (`accounts::Kind::Sample`): a session asks at every turn end.
    fn probe_record(&self, read: Result<&accounts::Sample, &'static str>) {
        let Some(probing) = with_kept_of(&self.kept, |k| k.account.probing.take()) else {
            aterm_log::info!(
                "harness @{}: account probe: a read with no claim standing; dropped",
                self.sid
            );
            return;
        };
        let word = match read {
            Err(miss) => miss,
            Ok(sample) => self.probe_sampled(&probing, sample),
        };
        aterm_log::info!(
            "harness @{}: account probe of {}: {word}",
            self.sid,
            probing.label
        );
    }

    /// [`Self::probe_record`] for a settled panel's `sample`: recorded for
    /// the process `probing` names, or dropped — and the one word of which.
    fn probe_sampled(&self, probing: &Probing, sample: &accounts::Sample) -> &'static str {
        if self.switches.accounts() == accounts::Mode::Off {
            return "sample dropped: accounts-off";
        }
        let Some((seat, roster, look)) = self.account_ctx() else {
            return "sample dropped: unread";
        };
        let same = look.judged(&roster) == Some(probing.label.as_str())
            && (look.snap.pid, look.snap.start.as_str()) == (probing.pid, probing.start.as_str());
        if !same {
            return "sample dropped: not the process it was claimed for";
        }
        let noted = self.account_transact(&seat, |state, now| {
            let sample = accounts::Sample {
                at: now,
                ..sample.clone()
            };
            Ok(accounts::note_sample(state, &probing.label, sample, now))
        });
        match noted {
            Ok(events) if events.is_empty() => "sample dropped: no figure, or none newer",
            Ok(_) => "sampled",
            Err(why) if why == accounts::LOCK_HELD => "sample dropped: lock-held",
            Err(_) => "sample dropped: not recorded",
        }
    }

    /// [`Self::account_ctx`] FOR A LIMIT RESET, asked only with `[harness]
    /// limit_reset` on: the seat, the roster a reset is judged against
    /// ([`account_roster_or_own`]: the machine's own row where no account is
    /// listed, NOTHING where the file is refused) and what the agent's own
    /// process says of its login ([`Self::account_look`]).
    fn reset_ctx(&self) -> Result<(AccountSeat, accounts::Roster, AccountLook), NoReset> {
        if !accounts_for(self.agent) {
            return Err(NoReset::Unread);
        }
        let seat = (self.hooks.acts.account_seat)().ok_or(NoReset::Unread)?;
        let roster = account_roster_or_own(&seat, true).ok_or(NoReset::Roster)?;
        let look = self.account_look(&roster).ok_or(NoReset::Unread)?;
        Ok((seat, roster, look))
    }

    /// THE CLAIM OF THIS SESSION'S ACCOUNT'S LIMIT RESET (what the loop asks
    /// immediately before the first key on the usage-limit dialog's grant
    /// row, and once more before the confirmation's Enter —
    /// [`IdleHost::reset_take`];
    /// `docs/SPEC-claude-accounts-resets-2026-10-02.md` R7 to R10):
    /// [`Self::reset_claimed`]'s decision, said in one line of the log — the
    /// roster's label where one was read, and the refusal's word
    /// ([`NoReset::as_str`]); never a path, a login's name or the wall's
    /// text — and answered as the loop takes it ([`NoReset::word`]). The host
    /// types nothing and writes no key.
    ///
    /// NOTHING OF IT HAS BEEN SEEN: no wall with a reset on offer has
    /// occurred, and the texts the loop reads are the 2.1.287 bundle's.
    fn reset_claim(&self, ask: &ResetAsk) -> ResetWord {
        let mut label = None;
        let claimed = self.reset_claimed(ask, &mut label);
        let of = label.map_or_else(String::new, |label| format!(" for {label}"));
        match claimed {
            Ok(go) => {
                aterm_log::info!(
                    "harness @{}: limit reset claimed{of}: wall until {}, {}",
                    self.sid,
                    go.until,
                    if go.long {
                        "long"
                    } else {
                        "its grant ends soon"
                    }
                );
                ResetWord::Take(go)
            }
            Err(why) => {
                aterm_log::info!(
                    "harness @{}: limit reset not claimed{of}: {}",
                    self.sid,
                    why.as_str()
                );
                why.word()
            }
        }
    }

    /// [`Self::reset_claim`]'s decision: the go, or why not ([`NoReset`]).
    /// `said` is set to the roster's label once one is read, for the log.
    /// The go only where EVERY ONE of these holds, in this order:
    ///
    /// 1. `[harness] limit_reset`, READ LIVE, is on — nothing is read, made
    ///    or locked otherwise. The agent is a Claude Code whose process is
    ///    read, on a roster that admits ([`Self::reset_ctx`]).
    /// 2. THE DEFAULT LOGIN: the process names no login directory, its
    ///    Claude home is the default one, it authenticates by its stored
    ///    login ([`AccountLook::judged`]), and the roster's row with no
    ///    directory is its row. The vendor posts the claim to the HOME
    ///    PROFILE's organization (READ), which is this login's only where no
    ///    other login ran (step 5).
    /// 3. ITS OWN WALL ([`standing_wall`]): the quota rejection standing last
    ///    in its transcript is this process's; a row not written yet is
    ///    asked again. Its reset is believed (`accounts::believed_reset`) and
    ///    still ahead, and its window is one a grant clears
    ///    ([`aterm_phase::reset_limit_name`]).
    /// 4. IT QUALIFIES on this host's clock ([`reset_qualifies`]): the wall
    ///    stands more than a day, or the grant's last day could end within
    ///    one.
    /// 5. ONE BOUNDED TRANSACTION under the store's lock, refused whole —
    ///    nothing written — unless all of it holds: the wall's row was not
    ///    written before the account's limits were last cleared
    ///    (`accounts::note_wall`'s own rule: such a wall is one a reset, or
    ///    the owner's word, already ended); the wall is noted; a reset is
    ///    allowed on this login (`accounts::reset_allowed`: the row with no
    ///    directory, no other account active, none on record running within
    ///    the vendor's profile window of this process's start); and
    ///    `accounts::claim_reset` answers that the ONE attempt at this wall
    ///    is this session's and this process's — taken now, or taken before,
    ///    still more than [`RESET_ENTER_MARGIN_S`] from its lapse, and with
    ///    no answer of the dialog's in the transcript since. A lock that
    ///    stays held is asked again.
    ///
    /// Under `[harness] accounts = "off"` a roster that lists ANOTHER login
    /// takes no reset (`accounts-off`): the rows step 5 reads are the
    /// rotation's to write, and with it off nothing records when that login
    /// ran. An outcome of this session's the store's lock kept out is
    /// written FIRST, and while it cannot be, no claim is asked for: a wall
    /// whose attempt was answered is never taken again on a claim that only
    /// looks open.
    fn reset_claimed(&self, ask: &ResetAsk, said: &mut Option<String>) -> Result<ResetGo, NoReset> {
        if !self.switches.reset_on() {
            return Err(NoReset::Off);
        }
        let (seat, roster, look) = self.reset_ctx()?;
        if self.switches.accounts() == accounts::Mode::Off
            && roster.rows.iter().any(|row| row.dir.is_some())
        {
            return Err(NoReset::Unrecorded);
        }
        if look.who.login.is_some() {
            return Err(NoReset::LoginDir);
        }
        if !look.who.default_home {
            return Err(NoReset::OwnHome);
        }
        if look.label.is_none() {
            return Err(NoReset::NotDefaultRow);
        }
        let label = look.judged(&roster).ok_or(NoReset::NotStored)?.to_owned();
        *said = Some(label.clone());
        let wall = match standing_wall(&seat, &look.snap) {
            Standing::Own(wall) => wall,
            Standing::Unwritten => return Err(NoReset::Unwritten),
            Standing::NotOwn => return Err(NoReset::NotOwnWall),
        };
        let Some(started) = process_started(&look.snap.start) else {
            return Err(NoReset::NotOwnWall);
        };
        let until = accounts::believed_reset(wall.until, wall.at)
            .filter(|until| *until > seat.now)
            .ok_or(NoReset::NoUntil)?;
        let limits = aterm_phase::reset_limit_name(&wall.window)
            .map(|name| vec![name.to_owned()])
            .ok_or(NoReset::Window)?;
        let long = reset_qualifies(until, ask.day_start, seat.now).ok_or(NoReset::NotQualifying)?;
        if self.reset_flush(&seat) == Flushed::Held {
            return Err(NoReset::LockHeld);
        }
        // This session's claim for the wall, as the state stands (read
        // without the lock): has its dialog answered since it was taken?
        let answered = accounts::Store::at(&seat.state)
            .load(seat.now)
            .ok()
            .and_then(|(state, _)| state.reset)
            .filter(|claim| {
                claim.account == label
                    && claim.until == Some(until)
                    && claim.sid == self.sid
                    && claim.done.is_none()
            })
            .is_some_and(|claim| {
                let since = u64::try_from(claim.at).unwrap_or(0);
                upgrade_drive::named_reset_line(&seat.home, &look.snap, since).is_some()
            });
        let mut refused = None;
        let claimed = account_transact_of(&self.sid, &self.hooks, &seat, |state, now| {
            let mut no = |why: NoReset| {
                refused = Some(why);
                Err(why.as_str().to_string())
            };
            let cleared = state
                .accounts
                .get(&label)
                .and_then(|record| record.cleared_at)
                .is_some_and(|cleared| wall.at < cleared.min(now));
            if cleared {
                return no(NoReset::Cleared);
            }
            let events =
                accounts::note_wall(state, &label, &wall.window, Some(until), wall.at, now);
            if let Err(why) = accounts::reset_allowed(&roster, state, &label, started) {
                return no(match why {
                    "active-other" => NoReset::ActiveOther,
                    "other-login" => NoReset::OtherLogin,
                    _ => NoReset::NotDefaultRow,
                });
            }
            let (pid, start) = (look.snap.pid, look.snap.start.as_str());
            match accounts::claim_reset(state, &label, until, &self.sid, pid, start, now) {
                accounts::ResetClaimed::Taken => Ok(events),
                accounts::ResetClaimed::Mine if answered => no(NoReset::Answered),
                accounts::ResetClaimed::Mine => {
                    let room = accounts::reset_in_flight(state, &label, now)
                        .is_some_and(|lapses| lapses.saturating_sub(now) > RESET_ENTER_MARGIN_S);
                    if room { Ok(events) } else { no(NoReset::Late) }
                }
                accounts::ResetClaimed::Held { .. } => no(NoReset::Held),
                accounts::ResetClaimed::Attempted => no(NoReset::Attempted),
            }
        });
        match claimed {
            Ok(_) => Ok(ask.go(until, long, limits)),
            Err(why) if why == accounts::LOCK_HELD => Err(NoReset::LockHeld),
            Err(_) if refused.is_some() => Err(refused.unwrap_or(NoReset::State)),
            Err(_) => {
                self.reset_unclaim(&seat, &look, &label, until);
                Err(NoReset::State)
            }
        }
    }

    /// A CLAIM'S TRANSACTION THAT FAILED AFTER IT WAS SAVED (the state
    /// written, its ledger row or wake marker not): the claim stands on
    /// disk as this session's, and the loop was answered no. It is ended at
    /// once, as kept (`state`), so it holds nobody off the account for its
    /// ten minutes: the wall has had its attempt, and no key was written.
    /// Nothing where the state does not hold this session's open claim for
    /// the wall (the transaction failed before its save).
    fn reset_unclaim(&self, seat: &AccountSeat, look: &AccountLook, label: &str, until: i64) {
        let open = accounts::Store::at(&seat.state)
            .load(seat.now)
            .ok()
            .and_then(|(state, _)| state.reset)
            .is_some_and(|claim| {
                claim.account == label
                    && claim.until == Some(until)
                    && claim.sid == self.sid
                    && claim.done.is_none()
                    && (claim.pid, claim.start.as_str())
                        == (look.snap.pid, look.snap.start.as_str())
            });
        if !open {
            return;
        }
        let ended = account_transact_of(&self.sid, &self.hooks, seat, |state, now| {
            let done = accounts::ResetDone::Kept(NoReset::State.as_str());
            Ok(accounts::reset_outcome(
                state, label, until, &self.sid, &done, now,
            ))
        });
        aterm_log::info!(
            "harness @{}: limit reset's claim for {label} was saved and its transaction \
             failed: ended as kept ({})",
            self.sid,
            if ended.is_ok() {
                "recorded"
            } else {
                "on the state"
            }
        );
    }

    /// HOW THE CLAIMED RESET ENDED (what the loop tells when the step the row
    /// opened is over — [`IdleHost::reset_told`]; the reset's spec, R15 to
    /// R17 and R22). THE OUTCOME IS THE TRANSCRIPT'S LINE, never the
    /// screen's: the dialog writes ONE answer line when it ends, as the
    /// output of its own command (`upgrade_drive::named_reset_line`: the
    /// LAST such output), and only a line written at or after the claim's
    /// second, told within [`RESET_LINE_WINDOW_S`] of it, is this attempt's.
    ///
    /// In this order, each refusal a line of the log and
    /// [`ResetOutcome::Unread`] with NOTHING RECORDED — or, where a moment
    /// may change it (the process or the roster not read just now, the state
    /// unreadable) and the loop will ask again, [`ResetOutcome::Pending`]:
    ///
    /// 1. `[harness] limit_reset`, read live, is on — nothing is read
    ///    otherwise.
    /// 2. THE CLAIM IN THE STATE IS THIS SESSION'S, with no outcome yet, FOR
    ///    THE PROCESS IN THE TAB NOW: read without the lock (the state is
    ///    replaced by a rename), its account the label this process is
    ///    judged as, its pid and kernel start this process's. A relaunch
    ///    between the claim and the tell — an upgrade, an exit — leaves a
    ///    process whose transcript's line is not known to answer that claim;
    ///    the claim then lapses by its own clock, and the wall is not
    ///    attempted again.
    /// 3. The line is read and classed ([`reset_read`]); with none yet, and
    ///    the loop asking again, [`ResetOutcome::Pending`] and nothing more.
    ///
    /// Then ONE BOUNDED TRANSACTION ([`Self::reset_flush`]): the outcome on
    /// the claim, its one ledger row (`accounts::reset_outcome`: a reset
    /// used or a limit lifted ends the account's limits for every session,
    /// and each row wakes the waiting ones) and the offer that was read
    /// ([`reset_offer`]). THE LOOP IS ANSWERED AT ONCE EITHER WAY: an outcome
    /// the lock kept out is held ([`AccountKept::reset_owed`]) and written at
    /// the session's next transaction or idle point, with the second it was
    /// answered at. One line of the log: the label, the outcome's word and
    /// whether it is on record — never the line's text.
    fn reset_tell(&self, end: &ResetEnd) -> ResetOutcome {
        let unread = |why: &str| {
            aterm_log::info!("harness @{}: limit reset's end not read: {why}", self.sid);
            ResetOutcome::Unread
        };
        // What a moment may change — the process not read just now, the
        // roster mid-save, the state mid-replace — is asked again while the
        // loop will ask: an answer here is final.
        let not_yet = |why: NoReset| {
            if end.last {
                unread(why.as_str())
            } else {
                ResetOutcome::Pending
            }
        };
        if !self.switches.reset_on() {
            return unread(NoReset::Off.as_str());
        }
        let (seat, roster, look) = match self.reset_ctx() {
            Ok(ctx) => ctx,
            Err(why) => return not_yet(why),
        };
        let Some(label) = look.judged(&roster) else {
            return unread(NoReset::NotStored.as_str());
        };
        let Ok((state, _)) = accounts::Store::at(&seat.state).load(seat.now) else {
            return not_yet(NoReset::State);
        };
        let claim = state.reset.as_ref().filter(|claim| {
            claim.account == label
                && claim.sid == self.sid
                && claim.done.is_none()
                && (claim.pid, claim.start.as_str()) == (look.snap.pid, look.snap.start.as_str())
        });
        let Some((until, since)) = claim.and_then(|claim| Some((claim.until?, claim.at))) else {
            return unread("no open claim of this session's process");
        };
        // Past the claim's window the transcript's line may be a later
        // dialog's: none is read, and this is the last word.
        let late = seat.now.saturating_sub(since) > RESET_LINE_WINDOW_S;
        let line = (!late)
            .then(|| u64::try_from(since).unwrap_or(0))
            .and_then(|since| upgrade_drive::named_reset_line(&seat.home, &look.snap, since));
        let end = &ResetEnd {
            last: end.last || late,
            ..*end
        };
        let Some((read, outcome)) = reset_read(line.as_deref(), end, seat.now) else {
            return ResetOutcome::Pending;
        };
        let owed = ResetOwed {
            label: label.to_owned(),
            until,
            offer: reset_offer(&read, end),
            read,
            at: seat.now,
        };
        with_kept_of(&self.kept, |k| k.account.reset_owed = Some(owed));
        let (recorded, outcome) = match self.reset_flush(&seat) {
            Flushed::Written => ("recorded", outcome),
            Flushed::Held => ("held for the lock", outcome),
            Flushed::Refused => ("not recorded", outcome),
            // Another session's claim took the slot between the read and
            // the lock: this attempt has no record to end.
            Flushed::NotMine | Flushed::Nothing => {
                ("not this session's claim", ResetOutcome::Unread)
            }
        };
        aterm_log::info!(
            "harness @{}: limit reset of {label}: {}; {recorded}",
            self.sid,
            outcome.word()
        );
        outcome
    }

    /// WRITE THE LIMIT RESET'S OUTCOME THIS SESSION HOLDS
    /// ([`AccountKept::reset_owed`]), if any, in one bounded transaction
    /// ([`account_transact_of`]): [`ResetOwed::write`]. A lock that stays
    /// held leaves it held for the next call — [`Self::reset_tell`] itself,
    /// any transaction of this session's ([`Self::account_transact`]) and its
    /// next idle point ([`IdleHost::at_idle`]). A claim that is no longer
    /// this session's open one, and a state this build may not write, drop
    /// it: the claim lapses by its own clock. With none held this takes one
    /// lock of the host's own and reads nothing.
    fn reset_flush(&self, seat: &AccountSeat) -> Flushed {
        let Some(owed) = with_kept_of(&self.kept, |k| k.account.reset_owed.take()) else {
            return Flushed::Nothing;
        };
        let mut mine = false;
        let done = account_transact_of(&self.sid, &self.hooks, seat, |state, now| {
            let events = owed.write(state, &self.sid, now);
            mine = !events.is_empty();
            Ok(events)
        });
        match done {
            Ok(_) if mine => Flushed::Written,
            Ok(_) => Flushed::NotMine,
            Err(why) if why == accounts::LOCK_HELD => {
                // A newer outcome put there meanwhile stands: there is one
                // claim, and the older one's slot is gone with it.
                with_kept_of(&self.kept, |k| {
                    k.account.reset_owed.get_or_insert(owed);
                });
                Flushed::Held
            }
            Err(_) => Flushed::Refused,
        }
    }

    /// THE ADVICE OF A SESSION ON A MACHINE THAT LISTS NO ACCOUNT, WITH
    /// `[harness] limit_reset` ON ([`Self::account_advice`]'s arm for it; the
    /// reset's spec, R10, R16 and R18): the machine's own row
    /// (`accounts::Roster::own`) is advised BY WHAT A LIMIT RESET LEFT ON
    /// RECORD AND BY NOTHING ELSE — there is no account to move to.
    ///
    /// * A reset IN FLIGHT on the session's account: a wait a moment away
    ///   ([`account_resetting`]), so the point is decided again once the
    ///   outcome is on record instead of at the limit's own reset.
    /// * The account's limits ENDED after the limit row this session stands
    ///   at (a reset used, or a limit the vendor said had lifted): the
    ///   roster's resume (`accounts::Advice::Resume`), its continuation typed
    ///   once per clearing by the arm that exists.
    /// * Anything else is nothing, as with no roster.
    ///
    /// It WRITES NOTHING — no session row, no wall — and where no state file
    /// exists (no reset was ever claimed on this machine) it reads nothing of
    /// the agent either.
    fn reset_advice(&self, seat: &AccountSeat, point: AccountPoint) -> Option<AccountAdvice> {
        let store = accounts::Store::at(&seat.state);
        if !store.state_file().exists() {
            return None;
        }
        let roster = accounts::Roster::own();
        let look = self.account_look(&roster)?;
        let label = look.judged(&roster)?;
        let (state, _) = store.load(seat.now).ok()?;
        if accounts::reset_in_flight(&state, label, seat.now).is_some() {
            aterm_log::info!(
                "harness @{}: account advice: a limit reset is in flight on {label}; asked again \
                 shortly",
                self.sid
            );
            return account_resetting(None, Instant::now() + RESET_AGAIN_AFTER);
        }
        let seen = self.lost_seen(&look.snap, point, seat.now);
        let (at, _, _) = account_point_read(seat, &look, point, seen);
        let advice = accounts::advise(
            &roster,
            &state,
            &accounts::enrolled(&roster),
            &look.who.me(&roster),
            &at,
            self.switches.accounts(),
            seat.now,
        );
        if !matches!(advice, accounts::Advice::Resume { .. }) {
            return None;
        }
        aterm_log::info!(
            "harness @{}: account advice: resume (a limit reset ended this account's limits)",
            self.sid
        );
        let at_wall = point.at_wall();
        account_advice_of(
            &advice,
            None,
            &state,
            None,
            at_wall,
            seat.now,
            Instant::now(),
        )
    }

    /// THE MOVE ([`Restart::Account`]; the accounts design, §6 "The host
    /// commits in one transaction under the lock, immediately before it asks
    /// for the relaunch, and a lost compare-and-set advises once more"). One
    /// step word back, as every restart's: `adopted` (the loop's next idle
    /// point takes the relaunch's last step, nothing typed, and then decides
    /// that point as with no move — or types the wall's resume after a move
    /// made at one), `wait:…` (asked again), `refused:…` / `failed:…` (not
    /// asked again while the wall stands).
    ///
    /// ONLY A FINAL ANSWER IS `refused:` OR `failed:` — the loop latches the
    /// wall on either, so each is a word nothing but the owner can change:
    /// `refused:accounts-off` (`[harness] accounts = "off"`, read live, but
    /// for the login lifecycle's follow: `logins` on over a roster with a row
    /// a renewal restores), the relaunch's own refusals
    /// (`refused:account-not-listed`, the roster no longer lists the label;
    /// `refused:line`; a one-shot launch), and
    /// `failed:account-not-applied` — the new process's environment was READ
    /// and names another login. Everything that can pass by itself is a
    /// `wait:`, and is asked again: `wait:account-changed`,
    /// `wait:account-state` (the store could not be read or written just
    /// now — its lock stayed held, a file not this build's),
    /// `wait:account-unverified`, and `wait:restart-in-flight`
    /// (`relaunch::IN_FLIGHT_OTHER`: a restart of another kind was in flight
    /// and was carried; the move was not tried).
    ///
    /// IN THIS ORDER:
    ///
    /// 0. THE ADVICE THE LOOP ACTS ON IS THE ONE THIS HOST GAVE
    ///    ([`AccountKept::advised`]: the same label at the same kind of
    ///    point), and the roster still advises it on what the process and
    ///    the state say now — else `wait:account-changed`, nothing done.
    ///    WHERE THAT MOVE HAS LANDED ALREADY — this loop was told to wait
    ///    when it asked, and the relaunch was read as made since
    ///    ([`Moved::untold`]) — the answer is `adopted` and NOTHING ELSE IS
    ///    DONE: this ask is how the loop is told.
    /// 1. THE COMMIT, under `accounts.lock` and nothing else: the state read
    ///    again, this process's own wall noted (`accounts::note_wall`: what
    ///    the rotation is judged on is the state, not the point), the advice
    ///    asked ONCE MORE on it — PAST A DUE PROBE, here and at step 0
    ///    (`accounts::advise_past_probe`: a `/usage` read that is due
    ///    refuses no move). No longer a move to `to` — another session
    ///    rotated first, the candidate was struck, the owner pinned another
    ///    — is `wait:account-changed` with nothing written, and so is a lost
    ///    compare-and-set. Else: `accounts::begin_rotation` (this session
    ///    moves ALONE), `accounts::take_canary`, or nothing (a follow), and
    ///    the session's row. THE ROTATION'S ROW IS WRITTEN WITH IT — the state
    ///    never says a rotation the ledger does not — except where the
    ///    session's last ask for the same label was not made either: that
    ///    one's row is held back for the answer ([`Self::account_unmade`]).
    /// 2. THE RELAUNCH ([`Acts::restart`], `relaunch::restart_under`: the
    ///    roster's login on the line as required environment), as the
    ///    worker's own act, the lock long released.
    /// 3. THE ANSWER, under the lock again ([`Self::account_unmade`]):
    ///    * made, and THE NEW PROCESS'S OWN ENVIRONMENT NAMES THE TARGET
    ///      ([`Self::account_applied`], on a snapshot of another process than
    ///      the one before): the session is on record there, and the move is
    ///      kept until it is answered ([`AccountKept::moved`]);
    ///    * made, and its environment was READ and names another login — or
    ///      the tab, read, still holds the very process the move was to
    ///      replace, under the login it had: `failed:account-not-applied`,
    ///      the rotation taken back, nothing held against the account;
    ///    * made, and the new process COULD NOT BE READ (no snapshot of the
    ///      tab just now, an environment that reads empty):
    ///      `wait:account-unverified` — THE MOVE STANDS, nothing is taken
    ///      back, and where it landed is read at the session's next ask
    ///      ([`AccountKept::flying`], [`account_land`]): read as made, the
    ///      loop is advised the same label once more and that ask is
    ///      answered `adopted` ([`Landing::Asked`]);
    ///    * `wait:restart-in-flight`: a restart of another kind was carried
    ///      and the move not tried — THE EXACT UNDO, nothing held against
    ///      anyone, and the loop's next idle point is asked for (the carried
    ///      restart's carry-on may be owed);
    ///    * not made, nothing signalled (`wait:changed` for its jobs, a
    ///      person's hand, a line that cannot be typed): THE EXACT UNDO
    ///      (`accounts::Strike::NotMoved`), the reason kept for the report
    ///      ([`held_word`]);
    ///    * `failed:…`, the agent signalled by this move's own restart
    ///      ([`Acts::account_restart`]): the account is struck
    ///      (`accounts::Strike::Failed`) where this session was its canary —
    ///      a follower's failed relaunch says nothing against an account
    ///      others are served by;
    ///    * `wait:…`, the agent signalled by this move's own restart: the
    ///      relaunch is IN FLIGHT — whether or not the signalled process has
    ///      gone yet — its line typed or still to type, and the host's carry
    ///      finishes it ([`on_agent_left`]) under the target's login. The
    ///      rotation stands, and the move is kept as flying: when the carry
    ///      lands it is A MADE MOVE ([`account_land`]).
    ///
    /// THE LOCK ORDER. `accounts.lock` and the upgrade's sweep lock are
    /// NEVER HELD TOGETHER, by anyone: this takes the first for steps 1 and
    /// 3 and has released it before step 2 takes the second (inside
    /// `relaunch::restart_under`, held for the whole relaunch) and takes it
    /// again only after that has returned; and nothing that runs under the
    /// sweep lock opens the accounts store. So there is no order between
    /// them to get wrong. EVERY TAKE OF `accounts.lock` HERE IS BOUNDED
    /// (`accounts::LOCK_LADDER`, [`account_transact_of`]): this is the
    /// loop's thread, and a lock that stays held is `wait:account-state`. A
    /// HERD CONVERGES THROUGH THE SWEEP LOCK ONE
    /// RELAUNCH AT A TIME: ten tabs at one wall commit in turn under
    /// `accounts.lock` (microseconds each) — one rotates, nine are told to
    /// wait for it — and once it has served a turn the nine follow, each
    /// asking the sweep lock for its relaunch; the one that gets it
    /// relaunches, the rest answer `busy:another-sweep` and are asked again
    /// on the loop's own back-off. Nothing waits on a lock for another
    /// tab's relaunch.
    ///
    /// WHAT BRINGS BACK A SESSION WHOSE MOVE ENDED AT THE SHELL. While the
    /// relaunch is in flight, the host's carry of it, under the TARGET's
    /// login (the line is the record's). Once a moved process has come up
    /// and then left before anything served the move, the relaunch on exit,
    /// under THE LOGIN BEFORE THE MOVE — or, where the account has served a
    /// turn since, the login it had — and never under one the store records
    /// signed out while the other is not: with neither left, under the login
    /// it had, where the loop waits for the owner's sign-in
    /// ([`account_way_back`]). A relaunch that FAILED after its signal is, as
    /// every restart in place is, the tab's mark and its `claude --resume`
    /// line — which, typed at that prompt, runs under the shell's own login.
    fn account_move(&self, why: &Restart, to: &str, at: AccountAt) -> String {
        const CHANGED: &str = "wait:account-changed";
        const STATE: &str = "wait:account-state";
        let hooks = &self.hooks;
        let at_wall = at == AccountAt::Wall;
        let advised = with_kept_of(&self.kept, |k| k.account.advised.take())
            .filter(|a| a.to == to && at_wall == a.point.at_wall());
        let mode = self.switches.accounts();
        // Under `accounts = "off"` the one move made is the login
        // lifecycle's, over a roster with a row it renews.
        let renewable = || {
            self.switches.logins()
                && (self.hooks.acts.account_seat)()
                    .and_then(|seat| account_roster(&seat))
                    .is_some_and(|roster| renewable_listed(&roster))
        };
        if mode == accounts::Mode::Off && !renewable() {
            return "refused:accounts-off".to_string();
        }
        let Some(advised) = advised else {
            return CHANGED.to_string();
        };
        // THE MOVE HAS LANDED ALREADY, and this loop was told to wait when it
        // asked ([`Moved::untold`]): this is the ask it is told by. Nothing
        // is read, committed or relaunched — the move was — and the loop's
        // next idle point is asked for: the relaunch's last step still owed
        // is taken there, nothing typed, and the point then decided as with
        // no move — or the wall's resume typed after a move made at one.
        let landed = with_kept_of(&self.kept, |k| {
            k.account
                .moved
                .as_mut()
                .filter(|m| m.untold && m.to == to)
                .map(|m| m.untold = false)
                .is_some()
        });
        if landed {
            self.park.store(true, Ordering::SeqCst);
            return "adopted".to_string();
        }
        let Some((seat, roster, look)) = self.account_ctx() else {
            return CHANGED.to_string();
        };
        let Some(label) = look.label.clone() else {
            return CHANGED.to_string();
        };
        let seen = self.lost_seen(&look.snap, advised.point, seat.now);
        let (point, wall) = account_point(&seat, &look, advised.point, seen);
        let Ok((state, _)) = accounts::Store::at(&seat.state).load(seat.now) else {
            return STATE.to_string();
        };
        let enrolled = accounts::enrolled(&roster);
        // PAST THE PROBE, at both re-advices: a `/usage` read that is due is
        // no reason to refuse a move the loop was advised (the roster answers
        // `Probe` ahead of its leave check, and would answer it here for as
        // long as nobody made the probe). The session is asked about as the
        // advice asked about it ([`asked_at`]).
        let advise = |state: &accounts::State, now: i64| {
            let me = asked_at(look.who.me(&roster), advised.point, self.switches.logins());
            accounts::advise_past_probe(&roster, state, &enrolled, &me, &point, mode, now)
        };
        let accounts::Advice::Move { to: first, .. } = advise(&state, seat.now) else {
            return CHANGED.to_string();
        };
        if first != to {
            return CHANGED.to_string();
        }
        // 1. The commit. The rotation's row is written with it, unless the
        // session's last ask for this label was not made either: a tab whose
        // jobs refuse its relaunch commits and takes back every few minutes,
        // and the ledger hears that once ([`Self::account_unmade`]).
        let repeat = with_kept_of(&self.kept, |k| k.account.unmade.as_deref() == Some(to));
        let mut canary = false;
        let mut owed: Vec<accounts::Event> = Vec::new();
        let committed = self.account_transact(&seat, |state, now| {
            let mut events = Vec::new();
            if let Some(wall) = &wall {
                events.extend(accounts::note_wall(
                    state,
                    &label,
                    &wall.window,
                    wall.until,
                    wall.at,
                    now,
                ));
            }
            let changed = || Err(ACCOUNT_CHANGED.to_string());
            // NOBODY MOVES OFF AN ACCOUNT WHOSE LIMIT RESET IS IN FLIGHT
            // ([`account_resetting`]), whenever the move was advised: a claim
            // taken since is read here, under the lock.
            if self.switches.reset_on() && accounts::reset_in_flight(state, &label, now).is_some() {
                return changed();
            }
            let accounts::Advice::Move {
                to: again,
                rotates,
                takes_canary,
                ..
            } = advise(&*state, now)
            else {
                return changed();
            };
            if again != to {
                return changed();
            }
            let mut rotated = None;
            if rotates {
                let expect = state.active.clone();
                let mover = accounts::Mover {
                    sid: &self.sid,
                    account: &label,
                };
                let Ok(event) = accounts::begin_rotation(
                    state,
                    &roster,
                    &enrolled,
                    expect.as_deref(),
                    mover,
                    to,
                    now,
                ) else {
                    return changed();
                };
                rotated = Some(event);
            } else if takes_canary {
                let expect = state.canary.clone();
                let Some(event) = accounts::take_canary(state, expect.as_ref(), &self.sid, now)
                else {
                    return changed();
                };
                rotated = Some(event);
            }
            canary = rotated.is_some();
            if repeat {
                owed.extend(rotated);
            } else {
                events.extend(rotated);
            }
            events.extend(accounts::note_session(state, &self.sid, &label, None, now));
            Ok(events)
        });
        match committed {
            Ok(_) => {}
            Err(why) if why == ACCOUNT_CHANGED => return CHANGED.to_string(),
            Err(_) => return STATE.to_string(),
        }
        // The way back ([`Moved::login`]): the login the replaced process ran
        // under, unless that process is the move's that nothing has answered
        // yet ([`Moved::answered`]) — then the login that move would have
        // gone back to.
        let back = with_kept_of(&self.kept, |k| {
            k.account
                .moved
                .as_ref()
                .filter(|m| {
                    !m.answered
                        && (m.pid, m.start.as_str()) == (look.snap.pid, look.snap.start.as_str())
                })
                .map(|m| m.login.clone())
        })
        .unwrap_or_else(|| look.snap.login.clone());
        // 2. The relaunch, no lock of the store's held.
        let before = (look.snap.pid, look.snap.start.clone());
        let step = if self.can_restart() {
            self.act();
            let step = (hooks.acts.restart)(&self.sid, self.grace, why);
            self.acted();
            step
        } else {
            // The transaction below must still undo a rotation already
            // committed before this live gate refused the restart.
            "refused:supervisor-off".to_string()
        };
        note_upgrade_act();
        let fresh = refresh_snapshot(&self.sid, &self.kept, hooks);
        let after = with_kept_of(&self.kept, |k| k.snapshot.clone()).filter(|_| fresh);
        // The move, should its relaunch have to be looked for later.
        let flying = Flying {
            to: to.to_string(),
            at,
            login: back.clone(),
            pid: before.0,
            start: before.1.clone(),
        };
        // 3. The answer.
        let word = step.split_once(':').map_or(step.as_str(), |(head, _)| head);
        if matches!(word, "adopted" | "relaunched" | "continued" | "done") {
            if step == "adopted" {
                self.park.store(true, Ordering::SeqCst);
            }
            let applied = match after {
                None => Applied::Unread,
                // The process before the move still holds the tab: read, and
                // under the login it had.
                Some(s) if (s.pid, s.start.as_str()) == (before.0, before.1.as_str()) => {
                    Applied::No
                }
                Some(s) => match self.account_applied(&roster, to, &s) {
                    Applied::Yes => {
                        with_kept_of(&self.kept, |k| {
                            k.account.held = None;
                            k.account.unmade = None;
                            k.account.flying = None;
                            k.account.moved = Some(Moved {
                                to: to.to_string(),
                                login: back,
                                pid: s.pid,
                                start: s.start,
                                untold: false,
                                answered: false,
                            });
                        });
                        Applied::Yes
                    }
                    other => other,
                },
            };
            return match applied {
                Applied::Yes => {
                    let noted = self.account_transact(&seat, |state, now| {
                        owed.extend(accounts::note_session(state, &self.sid, to, None, now));
                        Ok(owed)
                    });
                    if noted.is_err() {
                        aterm_log::info!(
                            "harness @{}: account session not noted: refused",
                            self.sid
                        );
                    }
                    step
                }
                Applied::No => {
                    let why = accounts::Strike::NotMoved("account-not-applied".to_string());
                    self.account_unmade(
                        &seat,
                        (to, label.as_str()),
                        canary,
                        owed,
                        Some(&why),
                        None,
                    );
                    "failed:account-not-applied".to_string()
                }
                Applied::Unread => {
                    // THE MOVE STANDS: nothing says it was not applied, and
                    // taking the rotation back would leave a session on the
                    // target with the machine pointing elsewhere. Where it
                    // landed is read at the next ask ([`account_land`]).
                    with_kept_of(&self.kept, |k| k.account.flying = Some(flying));
                    self.account_unmade(&seat, (to, label.as_str()), canary, owed, None, None);
                    "wait:account-unverified".to_string()
                }
            };
        }
        // Not made. A restart of ANOTHER kind was in flight and was carried:
        // the move was not tried, and that restart's carry-on may be owed.
        if step == relaunch::IN_FLIGHT_OTHER {
            self.park.store(true, Ordering::SeqCst);
            let why = accounts::Strike::NotMoved(step.clone());
            self.account_unmade(&seat, (to, label.as_str()), canary, owed, Some(&why), None);
            return step;
        }
        // Whether THIS move's own restart signalled the agent: its record
        // says so, whether or not the signalled process has gone yet.
        let signalled = (hooks.acts.account_restart)(&self.sid, before.0).as_deref() == Some(to);
        let why = match (signalled, step.starts_with("failed:")) {
            (true, true) => Some(accounts::Strike::Failed(step.clone())),
            // In flight: the rotation stands, and the carry finishes it.
            (true, false) => {
                with_kept_of(&self.kept, |k| k.account.flying = Some(flying));
                None
            }
            (false, _) => Some(accounts::Strike::NotMoved(step.clone())),
        };
        let held = (!signalled).then(|| held_word(&step)).flatten();
        self.account_unmade(
            &seat,
            (to, label.as_str()),
            canary,
            owed,
            why.as_ref(),
            held,
        );
        step
    }

    /// WHETHER THE PROCESS AN ACCOUNT MOVE'S RELAUNCH BROUGHT UP RUNS UNDER
    /// THE TARGET ([`Applied`]), `new` being the snapshot just read of
    /// another process than the one before the move. `adopted` says the new
    /// process holds the conversation, never under which login. Its own
    /// environment names the target (`relaunch::runs_under`): yes. Else it
    /// is asked for again ([`Acts::account_env`]), because a snapshot's
    /// login reads "unset" both for a variable that is not set and for an
    /// environment the kernel handed back empty: READ, and naming another
    /// login or none of the roster's, it is not the move; not read, nobody
    /// knows.
    fn account_applied(&self, roster: &accounts::Roster, to: &str, new: &Snapshot) -> Applied {
        if relaunch::runs_under(new, roster, to) {
            return Applied::Yes;
        }
        match exec_login(&self.hooks, new.pid) {
            Some(who) => match who.me(roster).account {
                Some(label) if label == to => Applied::Yes,
                _ => Applied::No,
            },
            None => Applied::Unread,
        }
    }

    /// THE ANSWER OF A MOVE THAT WAS NOT MADE ([`Self::account_move`], step
    /// 3), one transaction: the strike `why` on the word of this session
    /// where it was the candidate's canary (`accounts::strike`:
    /// [`accounts::Strike::NotMoved`] takes the rotation back EXACTLY —
    /// `active`, its proof, the canary, the rotation's time — and holds
    /// nothing against the account; a failure strikes it), and the session's
    /// row on the account it still runs on, with why it could not move
    /// (`held`). `None`: nothing is undone (a relaunch in flight, a move
    /// nobody could verify).
    ///
    /// THE LEDGER HEARS A REFUSAL ONCE. `owed` is the rotation's own row,
    /// held back at the commit only where the session's last ask for the
    /// same label was not made either ([`AccountKept::unmade`]): it is
    /// written here with the strike's, unless this ask was not made again. A
    /// tab whose jobs refuse its
    /// relaunch is asked again every few minutes for as long as they run;
    /// each ask commits and takes back, and without this each would write a
    /// `rotate` and a `struck` row and wake every waiting session — which,
    /// where two such tabs wait at one wall, wake each other for ever.
    fn account_unmade(
        &self,
        seat: &AccountSeat,
        (to, label): (&str, &str),
        canary: bool,
        owed: Vec<accounts::Event>,
        why: Option<&accounts::Strike>,
        held: Option<&'static str>,
    ) {
        let unmade = matches!(why, Some(accounts::Strike::NotMoved(_))).then(|| to.to_string());
        let again = with_kept_of(&self.kept, |k| {
            k.account.held = held;
            let again = unmade.is_some() && k.account.unmade == unmade;
            k.account.unmade = unmade;
            again
        });
        let done = self.account_transact(seat, |state, now| {
            let mut events = owed;
            if let Some(why) = why.filter(|_| canary) {
                events.extend(accounts::strike(state, to, &self.sid, why, now));
            }
            events.extend(accounts::note_session(state, &self.sid, label, held, now));
            Ok(if again { Vec::new() } else { events })
        });
        if done.is_err() {
            aterm_log::info!(
                "harness @{}: account move to {to} not taken back: refused",
                self.sid
            );
        }
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
/// while it still describes the tab ([`follow_snapshot`]). Whether the record
/// kept IS this read (`false`: it failed, and what is kept is older).
fn refresh_snapshot(sid: &str, kept: &Mutex<Kept>, hooks: &Hooks) -> bool {
    refresh_snapshot_guarded(sid, kept, hooks, None)
}

fn refresh_snapshot_guarded(
    sid: &str,
    kept: &Mutex<Kept>,
    hooks: &Hooks,
    revoked: Option<&AtomicBool>,
) -> bool {
    match (hooks.acts.snapshot)(sid) {
        Some(mut snap) => {
            let moved = {
                let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
                // A restored queue may close while the unlocked read runs.
                // Its last publication must precede the shutdown's follow.
                if revoked.is_some_and(|revoked| revoked.load(Ordering::SeqCst)) {
                    return false;
                }
                keep_session(kept.snapshot.as_ref(), &mut snap);
                let moved = publish_snapshot(sid, Some(&snap));
                kept.snapshot = Some(snap);
                moved
            };
            tell_published(sid, moved, hooks);
            true
        }
        None => {
            follow_snapshot_guarded(sid, kept, hooks, revoked);
            false
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

/// Publish `sid`'s kept snapshot (`None`: it has none). Whether what a
/// layout's leaf carries of it ([`crate::restore::AgentRestore::of`]: the
/// process, its conversation, build, folder, home and login) moved — the
/// caller tells the window so, with no lock of its own held
/// ([`tell_published`]).
fn publish_snapshot(sid: &str, snap: Option<&Snapshot>) -> bool {
    let carried = |snap: &Snapshot| crate::restore::AgentRestore::of(snap.clone());
    let mut map = PUBLISHED.lock().unwrap_or_else(PoisonError::into_inner);
    let map = map.get_or_insert_with(HashMap::new);
    let before = match snap {
        Some(snap) => map.insert(sid.to_string(), snap.clone()),
        None => map.remove(sid),
    };
    before.as_ref().map(carried) != snap.map(carried)
}

/// THE WINDOW IS TOLD WHEN WHAT A RESTORE CARRIES OF A TAB'S AGENT MOVED
/// (LC-1 of ATERM-KI-1015-1; `moved`, [`publish_snapshot`]):
/// [`HostNote::AgentPublished`], one wake of its loop, so its crash journal
/// captures again ([`crate::crash_journal`] writes only on a wake that is not
/// a timer's). The conversation an in-app `/clear` moves the agent to is
/// followed on a desk that draws nothing more, and a journal that was not
/// woken kept naming the one the agent had left until something else moved.
fn tell_published(sid: &str, moved: bool, hooks: &Hooks) {
    if moved {
        (hooks.note)(HostNote::AgentPublished {
            sid: sid.to_string(),
        });
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
    follow_snapshot_guarded(sid, kept, hooks, None)
}

fn follow_snapshot_guarded(
    sid: &str,
    kept: &Mutex<Kept>,
    hooks: &Hooks,
    revoked: Option<&AtomicBool>,
) -> bool {
    if revoked.is_some_and(|revoked| revoked.load(Ordering::SeqCst)) {
        return false;
    }
    let lock = || kept.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(before) = lock().snapshot.clone() else {
        return false;
    };
    let mut snap = before.clone();
    let fg = (hooks.acts.follow)(sid, &mut snap);
    let moved = {
        let mut session = lock();
        if revoked.is_some_and(|revoked| revoked.load(Ordering::SeqCst))
            || session.snapshot.as_ref() != Some(&before)
        {
            return false;
        }
        match fg {
            Foreground::Agent => {
                // A conversation the record moved to (an in-app `/clear` or
                // `/resume`): a fact the log keeps, rare as the move.
                if snap.session != before.session {
                    aterm_log::info!(
                        "harness @{sid}: its agent's record names conversation {} now (was {})",
                        snap.session.as_deref().unwrap_or("-"),
                        before.session.as_deref().unwrap_or("-")
                    );
                }
                let moved = publish_snapshot(sid, Some(&snap));
                session.snapshot = Some(snap);
                moved
            }
            Foreground::Shell => false,
            Foreground::Other => {
                let moved = publish_snapshot(sid, None);
                session.snapshot = None;
                moved
            }
        }
    };
    tell_published(sid, moved, hooks);
    true
}

/// The agent's exit is handled: what was read of it goes with it.
fn forget_snapshot(job: &WorkerJob, hooks: &Hooks) {
    let moved = publish_snapshot(&job.sid, None);
    with_kept(job, |k| k.snapshot = None);
    tell_published(&job.sid, moved, hooks);
}

/// THE SESSION'S AGENT LEFT AND NOTHING BRINGS IT BACK, so the accounts
/// recorder forgets the session ([`Acts::account_gone`]): an exit the host
/// leaves alone ([`left_alone`]: a person's, a holder's, one the owner
/// limited), a launch that ended, one that cannot be brought back, one
/// nothing was read of. A Claude Code's alone: no other agent's wall is
/// recorded ([`WorkerIdle::limited`]). NEVER on the way to a relaunch
/// ([`relaunch_until`]'s `Relaunched`, an agent that is back, a relaunch a
/// handoff carries) — that agent is the same session, on the same account —
/// and never for an agent the relaunch found still running (`not-exited`).
/// An exit left to a person may be a suspend the host cannot tell from one
/// (ctrl-z): that agent, brought to the front again, is recorded again at
/// its next wall.
fn account_gone(job: &WorkerJob, hooks: &Hooks) {
    if accounts_for(job.agent) {
        // What the host kept of its account goes with it: the next agent in
        // this tab is advised, and refused, on its own.
        with_kept(job, |k| k.account = AccountKept::default());
        (hooks.acts.account_gone)(&job.sid);
    }
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

/// How long the exit watch waits on the kernel's word at a time before it
/// looks whether it is still armed ([`watch_exit`]): how long a watch nobody
/// needs any more outlives its arming, at most. The end itself is the
/// kernel's push, read at once.
const EXIT_WATCH_LOOK: Duration = Duration::from_secs(1);

/// Every exit watch armed so far ([`ExitWatched::arming`]): a watch armed
/// before a read of it has an arming below what it read.
static EXIT_WATCH_ARMINGS: AtomicU64 = AtomicU64::new(0);

/// [`end_exit_watches`] of every watch, whenever it was armed: the harness
/// switched off, the shutdown.
const EVERY_WATCH: u64 = u64::MAX;

/// THE EXIT WATCH, ARMED (round seventeen; [`Kept::exit_watch`]) on the
/// process session `sid`'s snapshot names, unless one watches it already: a
/// thread of its own waits for the kernel's word of that process's end
/// ([`Acts::exited`]), reads what the exit left within its settle
/// ([`relaunch::exit_record`]) as a worker handed the exit at once would, and
/// keeps it for the worker the host hands the exit to ([`read_at_exit`]),
/// ringing the host. `hands`: the exit it hands ITSELF once it has read it
/// ([`ExitWatched::hands`]; a landing's, [`runs_again`]). Disarmed — a pass
/// names an agent in the tab again, the harness is switched off
/// ([`end_exit_watches`]), a worker took its read — it keeps nothing and
/// ends at its next look. Nothing without a snapshot: no relaunch could be
/// made on that exit either.
fn watch_exit(sid: &str, kept: &Arc<Mutex<Kept>>, hooks: &Hooks, hands: Option<Unwatched>) {
    watch_exit_guarded(sid, kept, hooks, hands, None);
}

fn watch_exit_guarded(
    sid: &str,
    kept: &Arc<Mutex<Kept>>,
    hooks: &Hooks,
    hands: Option<Unwatched>,
    revoked: Option<&AtomicBool>,
) {
    let armed = with_kept_of(kept, |k| {
        if revoked.is_some_and(|revoked| revoked.load(Ordering::SeqCst)) {
            return None;
        }
        let snap = k.snapshot.clone()?;
        if let Some(w) = k.exit_watch.as_mut().filter(|w| w.names(&snap)) {
            w.hands = w.hands.or(hands);
            return None;
        }
        let arming = EXIT_WATCH_ARMINGS.fetch_add(1, Ordering::SeqCst);
        k.exit_watch = Some(ExitWatched {
            pid: snap.pid,
            start: snap.start.clone(),
            arming,
            hands,
            read: None,
        });
        Some((snap, arming))
    });
    let Some((snap, arming)) = armed else {
        return;
    };
    let (owner, watched, hooks) = (sid.to_string(), Arc::clone(kept), hooks.clone());
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}exit-{sid}"))
        .spawn(move || exit_watch_main(&owner, &watched, &hooks, &snap, arming));
    if let Err(e) = spawned {
        aterm_log::warn!("harness @{sid}: could not watch its agent's exit: {e}");
        with_kept_of(kept, |k| {
            if k.exit_watch.as_ref().is_some_and(|w| w.arming == arming) {
                k.exit_watch = None;
            }
        });
    }
}

/// [`watch_exit`]'s thread: the end of `snap`'s process, then what it left,
/// kept while the watch is still armed as `arming`. A look that panicked read
/// nothing, and says so: whoever is handed the exit reads it then. A watch
/// disarmed while it waited for the end reads nothing at that end: its last
/// wait (up to [`EXIT_WATCH_LOOK`]) can outlast the disarm, and a look then
/// was one more at an exit whose worker was reading it itself (round
/// seventeen, the amend: every landing arms a watch, which the pass naming
/// its agent ends at once).
fn exit_watch_main(sid: &str, kept: &Mutex<Kept>, hooks: &Hooks, snap: &Snapshot, arming: u64) {
    let armed = || {
        with_kept_of(kept, |k| {
            k.exit_watch.as_ref().is_some_and(|w| w.arming == arming)
        })
    };
    let seen = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        while !(hooks.acts.exited)(snap, EXIT_WATCH_LOOK) {
            if !armed() {
                return None;
            }
        }
        if !armed() {
            return None;
        }
        Some(relaunch::exit_record(
            || (hooks.acts.exit_look)(sid, snap),
            |step| !wait_for(Instant::now() + (hooks.pause)(step), || !armed()),
        ))
    }));
    let left = match seen {
        Ok(Some(left)) => left,
        Ok(None) => return,
        Err(_) => ExitRecord::Unread,
    };
    let kept_it = with_kept_of(kept, |k| match k.exit_watch.as_mut() {
        Some(w) if w.arming == arming => {
            w.read = Some(left.clone());
            true
        }
        _ => false,
    });
    if kept_it {
        aterm_log::info!(
            "harness @{sid}: its agent exited with no worker handed its exit; read at the exit: {}",
            left.word()
        );
        ring();
    }
}

/// WHAT `snap`'s EXIT LEFT, AS IT WAS SEEN, for the worker handed it: the
/// exit watch's read where one watched that very process
/// ([`Kept::exit_watch`]) — waited for while it is still reading, as long as
/// one read can take — taken, so the exit is read once; else read now,
/// within the settle ([`relaunch::exit_record`]), as the exit is handed. A
/// watch that read nothing (`unread`) is read past. Cut short, either, when
/// the exit is the worker's no more ([`exit_handed`]).
fn read_at_exit(job: &WorkerJob, hooks: &Hooks, snap: &Snapshot) -> ExitRecord {
    let reading = || {
        with_kept(job, |k| {
            k.exit_watch
                .as_ref()
                .is_some_and(|w| w.names(snap) && w.read.is_none())
        })
    };
    if reading() {
        let most = EXIT_WATCH_LOOK + relaunch::EXIT_GONE + relaunch::EXIT_SETTLE;
        wait_for(Instant::now() + most, || !reading() || !exit_handed(job));
    }
    let watched = with_kept(job, |k| {
        let read = k
            .exit_watch
            .as_ref()
            .filter(|w| w.names(snap))?
            .read
            .clone()?;
        k.exit_watch = None;
        Some(read)
    });
    match watched {
        Some(left) if left != ExitRecord::Unread => left,
        _ => relaunch::exit_record(
            || (hooks.acts.exit_look)(&job.sid, snap),
            |step| !wait_for(Instant::now() + (hooks.pause)(step), || !exit_handed(job)),
        ),
    }
}

/// THE HOST SEES ITS TABS AGAIN: the exit watch of every session `which`
/// names ends ([`Kept::exit_watch`]) — an agent the roster names in its tab
/// is its worker's to watch, and nothing kept of one before it there is
/// anyone's (the tab is not at its shell); with the harness switched off,
/// nobody's is. Only a watch armed before `armed_before` (an
/// [`EXIT_WATCH_ARMINGS`] the caller read; [`EVERY_WATCH`]: all): a pass
/// ends the watches armed before it read its roster, never one armed on news
/// that read has not seen (round seventeen, the review of its first cut: a
/// pass that read the roster before a Commit's stop ended the watch that
/// stop's worker armed as it ended, and no pass of the suspended host armed
/// another — the crash during the hold read only after it, as a graceful
/// exit). Each session's lock is taken alone, after the map's is let go.
fn end_exit_watches(shared: &Shared, armed_before: u64, which: impl Fn(&str) -> bool) {
    let kept: Vec<Arc<Mutex<Kept>>> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|(sid, _)| which(sid))
        .map(|(_, kept)| Arc::clone(kept))
        .collect();
    for kept in kept {
        with_kept_of(&kept, |k| {
            if k.exit_watch
                .as_ref()
                .is_some_and(|w| w.arming < armed_before)
            {
                k.exit_watch = None;
            }
        });
    }
}

/// RELAUNCH ON EXIT: the session's agent left a tab that lives on. Whose
/// exit it was decides: the harness's OWN restart's first — the upgrade's,
/// or the restart in place's, left in flight by its step
/// ([`Acts::restarted`]; S0 and S3 of the in-flight review, 2026-09-27) —
/// which is carried whoever is at the tab and whatever the owner's switch
/// says ([`OnExit::Restarted`]), save by a worker inside the session's
/// adopted-claim grace ([`WorkerJob::watch_until`]), for which another
/// supervisor's live claim comes first; one another supervisor's live claim
/// holds is that supervisor's ([`claimed_by_another`]); then
/// ([`relaunch::on_exit`]) a person's (a keystroke within the human grace)
/// or a holder's (a halt, a lease, a named driver's turn) is theirs, and the
/// tab is left to them; one the owner limited — `[harness] relaunch =
/// false` — is said once on the session's attention where a relaunch was
/// owed, and left as the launch's own end, saying nothing, where it is
/// that end (a graceful exit, a one-shot run); any other (a Claude
/// Code's, a Codex's) is relaunched on its conversation after the session's
/// back-off, cut short when the agent is back or the worker is stopped for
/// good, and asked about again before each try — trying again on the
/// growing pause until it lands, until the exit proves to be the launch's
/// own end (journaled, nothing said), or until it can never land (said).
/// An exit the explicit restart's standing stamp names whose program still
/// runs — stopped away from its tab — is nobody's yet: whose it was is
/// decided once it ends, as an exit's first read is, its relaunch owed
/// meanwhile (rulings 13, 15 and 17 of 2026-10-08; the model's `Read` needs
/// `alive == 0` and no other hand). One that ended, read while a hold or
/// any hand its relaunch line waits on holds the tab
/// ([`relaunch::hand_on_tab`]), is CARRIED through that hand (ruling 20 of
/// 2026-10-14): owed, nothing attempted while the hand stands
/// ([`relaunch::WAIT_HAND_LIFTS`], looked at again uncounted), relaunched as
/// the act's once it lifts — never the holder's, which left the agent a
/// person asked to restart at its shell. Another supervisor's claim still
/// takes it, asked at every look of the carry as at an exit's first read
/// (ruling 17 and the review of ruling 20; the model's `ReadByClaim`).
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "HarnessRestartStamp",
        action = "Read",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "HarnessRestartStamp",
        action = "ReadByClaim",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
fn on_agent_left(job: &WorkerJob, hooks: &Hooks) {
    if !job.admitted() {
        // No decision is made of this exit: a copy whose process it was is
        // not kept for a later one.
        crate::self_claim::discard_dead(job.sid.as_str());
        settled(job);
        return;
    }
    let sid = job.sid.as_str();
    let grace = job.opts.policy.human_grace_s;
    let snap = with_kept(job, |k| k.snapshot.clone());
    // THE EXPLICIT RESTART ENDED IT (design of 2026-10-08): its stamp, on
    // disk BEFORE its signal, names the process the kept snapshot names —
    // found by LOOKUP, whoever reads the exit ([`Acts::asked`]). The
    // stall's remedy (`restart if-stalled`) or a person's own `restart`:
    // relaunched on its conversation however it ended, a keystroke before
    // it not read as their `/exit`, and under `[harness] relaunch = false`
    // too (a person's explicit restart is not refused by it). No other
    // signal is the restart's: a bare `signal term`'s exit is the
    // program's own, read by the rules below.
    let asked = snap.as_ref().and_then(|snap| (hooks.acts.asked)(sid, snap));
    // A PROVEN SELF-CLAIMED AGENT'S EXIT (the self-claim contract, §4) is
    // decided by its own rule, here, by this one reader: never the Codex
    // lane's shell-word crash nor its relaunch ([`self_claimed_exit`]).
    // Only a Codex exit is a codex-alab's: a Claude's (or another program's)
    // never meets a copy, and a copy whose process ended unread is dropped
    // rather than kept for a later exit.
    let codex_exit = match job.agent {
        Program::Codex => true,
        Program::Claude | Program::Generic | Program::Agy | Program::Muse => false,
    };
    let copy = (hooks.acts.self_copy)(sid).filter(|_| codex_exit);
    match copy.filter(|copy| match snap.as_ref() {
        Some(snap) => snap.pid == copy.pid && snap.start == copy.start,
        // No snapshot to match (the tab's own program): only a copy whose
        // process has ended is this exit's.
        None => !restart_stamp::alive_as(copy.pid, &copy.start),
    }) {
        Some(copy) => {
            self_claimed_exit(job, hooks, snap, &copy, asked);
            return;
        }
        None => crate::self_claim::discard_dead(sid),
    }
    // THE HARNESS'S OWN RESTART ENDED IT, read before whose exit it was: the
    // step that signalled it returned with its relaunch still to type (a
    // person at the returned prompt, a hold, a shell slow to take the
    // terminal back), and no step watches the tab once its agent is gone.
    // Asked as a person's exit, a holder's or a limited one, the agent the
    // upgrade ended was never brought back — the owner who pressed Upgrade
    // now had typed within the grace. Both lanes' restart is the record that
    // ended the very process the snapshot names, by a restart that ends its
    // agent itself: never a relaunch on exit's own record, nor another
    // process's (a Codex a person started at the prompt that record waited
    // on, whose `/exit` is theirs). An agent no lane restarts (agy, Muse
    // Code) has no such record, and none is read for it as Claude Code's.
    let lane = aterm_agent::harness::upgrade::Agent::of(job.agent);
    let pid = snap.as_ref().map(|s| s.pid);
    let restarted = lane.is_some_and(|agent| (hooks.acts.restarted)(sid, agent, pid));
    // AN EXIT WHOSE PROGRAM STILL RUNS IS NOBODY'S YET (rulings 15 and 17,
    // review of 1a733ed60): the act's stamped program stopped from elsewhere
    // inside its grace — or put in the background — reads as an exit here
    // (its shell takes the terminal back), but its end is still to come: its
    // watch ends it once it leads again and no other hand holds the tab
    // (rulings 8 and 11), it finishes the shutdown its `SIGTERM` started, or
    // someone else ends it. Decided while it ran — `Held` under a hand that
    // came and went while it was stopped — the exit was left and its
    // snapshot forgotten, and the act's `KILL` after `fg`, which had waited
    // that hand out, was nobody's to relaunch. So while the program runs and
    // its stamp stands every decision below PUTS IT OFF (`Relaunch`, its
    // attempt [`relaunch::WAIT_ACT_ENDS`]: nothing read, nothing typed), and
    // the first decision that finds it ENDED is made as an exit's first read
    // is (ruling 17): the stamp looked up again — an end after it went stale
    // is the program's own, read by the other rules, never relaunched as the
    // act's on the cause the first read found — and another supervisor's
    // claim asked. Not the harness's own restart's exit (`restarted`),
    // carried by its rules. `cause`: the stamp's cause the exit is decided
    // and relaunched by — the first read's, until that look again.
    let act_snap = snap.clone().filter(|_| asked.is_some() && !restarted);
    let cause = std::cell::Cell::new(asked);
    let put_off = std::cell::Cell::new(false);
    // Set by the first put-off and kept until the end is decided: a decision
    // between that does not put it off (the stamp lapsed while the program
    // still ran, then an attempt answered `busy:another-sweep`) must not let
    // a later end ride on the first read's `cause`.
    let end_owed = std::cell::Cell::new(false);
    // AN ASKED EXIT IS CARRIED THROUGH A HAND THAT WAITS (ruling 20 of
    // 2026-10-14, THE HOLDER RULE AT THE READ): an end the act's stamp names,
    // decided while a hold or any hand its relaunch line waits on holds the
    // tab ([`relaunch::hand_on_tab`]: another driver's lease, a turn, a
    // `driving:` hand) — one that landed after the program ended and before
    // this read (a hearing program's own end, the KILL just after a hand
    // cleared), or the end of a program stopped meanwhile, read up to
    // `relaunch::BUSY` after `fg` — is the act's still
    // ([`relaunch::on_exit`]), and its relaunch waits for that hand: while
    // it stands the attempt answers [`relaunch::WAIT_HAND_LIFTS`] in its
    // place (owed, its snapshot kept, looked at again uncounted and unsaid),
    // as the harness's own restart's relaunch line waits on `held`. Before,
    // it was left to the holder, and nobody relaunches once a hold, a lease
    // or a turn lifts: the agent a person asked to restart stayed at its
    // shell, its stamp on disk. The hands are the line's own (save a
    // person's keystroke and unread input, short, which its counted waits
    // suit): under any other the line answers `wait:held`, a counted miss,
    // and a turn of minutes climbed the back-off. Another supervisor's claim
    // is not carried through (below, ruling 17): it is a supervisor of the
    // session, which relaunches what it holds, and the relaunch line does
    // not wait on a claim — so every look of a carry asks it again.
    let hand_waits = std::cell::Cell::new(false);
    // AN EXIT UNDER ANOTHER SUPERVISOR'S LIVE CLAIM is that supervisor's:
    // it holds the session as the exit is first read (whenever it claimed
    // it: a supervisor parked behind this host's loop can claim the tab at
    // its shell once the loop gave its claim back, as at a Commit's stop),
    // and this host never relaunches an agent a supervisor it parks behind
    // holds ([`BodyEnd::Held`]). Asked of every exit as it is read, whether or not
    // this host ever held the session (the review of round 28: asked only
    // inside the adopted-claim grace, an exit read alone after a Commit that
    // failed past the grace went into the tab of the supervisor that had
    // claimed it back) — save, past the grace, an exit the harness's own
    // restart ended (`restarted`): [`relaunch::on_exit`] asks that first and
    // carries it whoever is at the tab, so a claim taken between the restart
    // and this read (a supervisor parked behind this host's loop, claiming
    // the tab at its shell or while a Commit holds the host) does not leave
    // the agent the restart ended at a bare shell. A worker that watched the
    // grace ([`WorkerJob::watch_until`], #501) or read its exit alone inside
    // it ([`exit_main`]) asks the claim first even of that exit — no loop of
    // this host stepped the session inside the grace, so a restart in flight
    // is the outgoing instance's, and the successor's carry carries it
    // ([`HostHandle::resume_after_handoff`]) — and asks it again before every
    // attempt, as a hold is: the grace is there for that supervisor to claim
    // the session again first. AN EXIT CARRIED THROUGH A HAND (ruling 20)
    // asks it at every look of the carry and at the look that finds the
    // hand lifted, as an exit's first read does (the review of ruling 20,
    // 2026-10-14): carrying keeps the decision open for the whole hand, so a
    // claim that lands meanwhile — the first read found none — holds the
    // session as the end is decided; asked only at the first read, the
    // relaunch was typed into the tab that supervisor claimed once the hand
    // lifted. Any other relaunch, once decided, is this host's own: a
    // supervisor parked behind its loop that claims the tab at its shell —
    // the loop gave its claim back at the exit — does not end it.
    let watched = job.watch_until.is_some();
    let decide_at = |exit: bool| {
        put_off.set(false);
        // The look before carried the exit through a hand: this look decides
        // it again as its first read did.
        let carried = hand_waits.replace(false);
        let mut exit = exit;
        if let Some(snap) = act_snap.as_ref() {
            let runs = restart_stamp::alive_as(snap.pid, &snap.start);
            if runs && (hooks.acts.asked)(sid, snap).is_some() {
                put_off.set(true);
                end_owed.set(true);
                return OnExit::Relaunch;
            }
            // Its end, first seen after any put-off (a program still running
            // past its stamp is left as `not-exited` by its attempt, looked
            // up no more).
            if end_owed.get() && !runs {
                end_owed.set(false);
                cause.set((hooks.acts.asked)(sid, snap));
                // What is owed is relaunched on the cause its end is decided
                // by — and carried on it (round eighteen).
                with_kept(job, |k| {
                    if !job.revoked.load(Ordering::SeqCst)
                        && let Some(owed) = k.owed.as_mut()
                    {
                        owed.asked = cause.get();
                    }
                });
                exit = true;
            }
        }
        let status = (hooks.acts.status)(sid);
        if (watched || ((exit || carried) && !restarted))
            && let Some(other) = status.as_deref().and_then(claimed_by_another)
        {
            aterm_log::info!(
                "harness @{sid}: {other} claims the session; its agent's exit is that \
                 supervisor's"
            );
            return OnExit::Held;
        }
        let asked = cause.get();
        let allowed = relaunches(job.agent) && (job.switches.relaunch() || asked.is_some());
        let decision = relaunch::on_exit(
            allowed,
            status.as_deref(),
            grace,
            asked.is_some(),
            restarted,
        );
        hand_waits.set(
            decision == OnExit::Relaunch
                && asked.is_some()
                && status.as_deref().is_some_and(relaunch::hand_on_tab),
        );
        decision
    };
    let decide = || decide_at(false);
    let decision = decide_at(true);
    // A LIMIT IS SAID ONLY FOR AN EXIT IT KEPT FROM COMING BACK (the
    // real-Claude sweep of 2026-10-09: under `relaunch = false` a person's
    // `/exit` and a `kill` each posted a Warn record that the agent "is not
    // relaunched: [harness] relaunch = false"). What the exit left is read
    // first, as the relaunch reads it: an exit that is its launch's own end
    // — graceful, a one-shot run, or one nothing was read of while it ran —
    // is left as `relaunch = true` leaves it ([`left_as_its_end`]: the
    // decision a relaunch's, its attempt finding that end), and said nothing.
    if decision == OnExit::Limited
        && let Some(why) = launch_end_read(job, hooks, snap.as_ref())
    {
        let now = Instant::now();
        let said = with_kept(job, |k| {
            let _ = k.relaunches.exited(OnExit::Relaunch, now);
            k.relaunches.attempted(&Outcome::Left(why.to_string()), now)
        });
        left_as_its_end(job, hooks, why, said);
        return;
    }
    let pause = match with_kept(job, |k| k.relaunches.exited(decision, Instant::now())) {
        // An exit the act asked for is no crash: its first attempt is not
        // backed off.
        Ok(_) if asked.is_some() => Duration::ZERO,
        Ok(pause) => pause,
        Err(said) => {
            forget_snapshot(job, hooks);
            left_alone(job, hooks, decision, said);
            return;
        }
    };
    if let Some(agent) = lane.filter(|_| restarted) {
        aterm_log::info!(
            "harness @{sid}: the agent exited; the harness's own restart ended it, and its \
             relaunch is carried"
        );
        relaunch_until(job, hooks, pause, &decide, true, || {
            (hooks.acts.carry)(sid, grace, agent, pid)
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
        account_gone(job, hooks);
        return;
    };
    // WHAT THE EXIT LEFT of Claude's own record — a crash leaves it, a
    // graceful exit removes it — read NOW, within a short settle of the exit
    // and before the back-off, and kept for every attempt: any Claude Code
    // started meanwhile removes a dead agent's record, and a crash read
    // after the back-off was a graceful exit, silently never relaunched (D2
    // of the 2026-09-26 live test). Cut short when the agent is back or the
    // worker is stopped for good — never by a Commit's stop, which reads it
    // through: a crash read as unread there was never owed, and the rollback
    // had nothing to take up (round twelve; [`WorkerJob::left_at_commit`]).
    // An exit no pass of the host saw as it came — while a Commit held the
    // host, or its thread was down after a panic — was read by the exit
    // watch as it was seen, and that read is the one taken (round
    // seventeen; [`read_at_exit`]).
    // An account move nothing has served, whose process is the one that
    // left: its relaunch carries the login before the move, or the one it
    // had ([`account_way_back`]).
    let mut snap = snap;
    account_way_back(job, hooks, &mut snap);
    let left = read_at_exit(job, hooks, &snap);
    aterm_log::info!(
        "harness @{sid}: the agent exited; read at the exit: {}",
        left.word()
    );
    // An exit the relaunch is decided on already — a crash, a stall's remedy
    // — is OWED through the back-off: a handoff carries it (round six, F13).
    // A crash by the rule the lanes relaunch on ([`relaunch::ExitRecord::crashed`]):
    // a Codex's shell's word too, not a Claude Code's surviving record alone.
    if asked.is_some() || left.crashed() {
        with_kept(job, |k| {
            if !job.revoked.load(Ordering::SeqCst) {
                k.owed = Some(OwedRelaunch {
                    snap: snap.clone(),
                    asked: cause.get(),
                });
            }
        });
    }
    // AN ACT'S PROGRAM STOPPED AWAY FROM ITS TAB (ruling 13, review of
    // 1a733ed60): while a decision puts its exit off (`put_off` above), no
    // relaunch is attempted — the attempt answers
    // [`relaunch::WAIT_ACT_ENDS`], owed, looked at again uncounted — and its
    // snapshot is kept. Left — settled, its snapshot forgotten, as
    // `wait:not-exited` is — that end was usually read by no worker holding
    // anything of the agent (the watch's `SIGKILL` within a second of a `fg`
    // beats the host's naming it again and reading it), and was never
    // relaunched. Once the stamp no longer stands (stale: the watch let the
    // program go) the relaunch is asked, finds it running, and it is left as
    // before.
    let away_said = std::cell::Cell::new(false);
    let hand_said = std::cell::Cell::new(false);
    // On the build the upgrade would move it to only while `[harness]
    // upgrade` allows (read live, as the relaunch switch is).
    relaunch_until(job, hooks, pause, &decide, false, || {
        if put_off.get() {
            if !away_said.replace(true) {
                aterm_log::info!(
                    "harness @{sid}: the agent the explicit restart is ending still runs away \
                     from its tab (stopped, or in the background); whose exit it is is decided at \
                     its end while its stamp stands"
                );
            }
            return relaunch::WAIT_ACT_ENDS.to_string();
        }
        if hand_waits.get() {
            if !hand_said.replace(true) {
                aterm_log::info!(
                    "harness @{sid}: the agent the explicit restart ended is carried through \
                     another hand on its tab (a hold, a lease, a turn); its relaunch is typed once \
                     that hand lifts"
                );
            }
            return relaunch::WAIT_HAND_LIFTS.to_string();
        }
        hand_said.set(false);
        (hooks.acts.relaunch)(
            sid,
            grace,
            &snap,
            &left,
            job.switches.upgrade(),
            cause.get(),
        )
    });
}

/// THE CRASH RULE (the self-claim contract, §4): the exit of an agent a
/// proven self-claim supervised, decided by this host's watch-only worker
/// alone, from aterm's OWN COPY of the agent's session record (`copy`, read
/// while its identity was live) — never from the file now, never by the
/// Codex lane's shell word (the exit-137 branch), and never left to the dead
/// self-claim (`claimed_by_another` reads it as nobody's). First whose exit it
/// was ([`relaunch::on_exit`]: a person's keystroke, a hold, `[harness]
/// relaunch`, another live supervisor's claim); then the copy's verdict
/// ([`self_claimed::verdict`]): an orderly end is left alone; a crash — or
/// the end of aterm's own restart, its stamp `asked` — is relaunched on the
/// line the copy validated, through the Codex lane's relaunch
/// ([`Acts::self_relaunch`]: the shell at its marked prompt, the thread's
/// writer lock free, the witness ledger), after the session's back-off. A
/// line that did not validate relaunches nothing and says so once (one
/// Messages record); so does a tab whose agent was its spawn command (no
/// shell to relaunch at).
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "HarnessRestartStamp",
        action = "CopyRelaunches",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "HarnessRestartStamp",
        action = "CopyLeaves",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
fn self_claimed_exit(
    job: &WorkerJob,
    hooks: &Hooks,
    snap: Option<Snapshot>,
    copy: &self_claimed::Copy,
    asked: Option<restart_stamp::Cause>,
) {
    let sid = job.sid.as_str();
    let grace = job.opts.policy.human_grace_s;
    // This exit's copy is taken by this decision alone.
    crate::self_claim::take(sid, copy.pid, &copy.start);
    let decide = || {
        let status = (hooks.acts.status)(sid);
        if let Some(other) = status.as_deref().and_then(claimed_by_another) {
            aterm_log::info!(
                "harness @{sid}: {other} claims the session; its agent's exit is that \
                 supervisor's"
            );
            return OnExit::Held;
        }
        let allowed = relaunches(job.agent) && (job.switches.relaunch() || asked.is_some());
        relaunch::on_exit(allowed, status.as_deref(), grace, asked.is_some(), false)
    };
    let inform = |text: String| {
        (hooks.note)(HostNote::Inform {
            sid: sid.to_string(),
            text,
        });
    };
    let decision = decide();
    if let self_claimed::Verdict::Orderly(why) = self_claimed::verdict(copy, asked.is_some()) {
        let now = Instant::now();
        let said = with_kept(job, |k| {
            let _ = k.relaunches.exited(OnExit::Relaunch, now);
            k.relaunches.attempted(&Outcome::Left(why.to_string()), now)
        });
        left_as_its_end(
            job,
            hooks,
            &format!("the self-supervised codex-alab ended in order ({why})"),
            said,
        );
        return;
    }
    let pause = match with_kept(job, |k| k.relaunches.exited(decision, Instant::now())) {
        Ok(_) if asked.is_some() => Duration::ZERO,
        Ok(pause) => pause,
        Err(said) => {
            forget_snapshot(job, hooks);
            left_alone(job, hooks, decision, said);
            return;
        }
    };
    let Some(snap) = snap.filter(|snap| snap.shell != 0) else {
        settled(job);
        forget_snapshot(job, hooks);
        aterm_log::info!(
            "harness @{sid}: the self-supervised codex-alab crashed; not relaunched: it was the \
             tab's own program, with no shell to relaunch it at"
        );
        inform(format!(
            "codex-alab ({}) crashed and is not relaunched: it was this tab's own program, with \
             no shell to start it again at",
            copy.pid
        ));
        return;
    };
    let resume = match &copy.resume {
        Ok(resume) => resume.clone(),
        Err(why) => {
            settled(job);
            forget_snapshot(job, hooks);
            aterm_log::info!(
                "harness @{sid}: the self-supervised codex-alab crashed; not relaunched: its \
                 record's resume line did not validate against the live process ({why})"
            );
            inform(format!(
                "codex-alab ({}) crashed and is not relaunched: its session record's resume \
                 line did not match the process aterm saw run ({why})",
                copy.pid
            ));
            return;
        }
    };
    aterm_log::info!(
        "harness @{sid}: the self-supervised codex-alab crashed; relaunching it on its \
         conversation {}",
        resume.session
    );
    relaunch_until(job, hooks, pause, &decide, false, || {
        (hooks.acts.self_relaunch)(sid, grace, &snap, &resume, asked)
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
        if !job.admitted() {
            settled(job);
            return;
        }
        let wait = if restarted {
            pause.min(relaunch::CARRY_EVERY)
        } else {
            pause
        };
        if wait_for(Instant::now() + (hooks.pause)(wait), || {
            !job.left.is(Word::Left)
        }) {
            // The host read the agent back ([`Worker::back`]) or a stop came
            // ([`request_stop`]), each its own word; `stop` says nothing, the
            // exit's own [`Worker::leave`] set it (round ten — read as a
            // Commit's stop, an agent back during a park stayed owed, and the
            // successor or the rollback relaunched it in its tab). The tab
            // says whether its agent is back NOW.
            if (hooks.still_wanted)(sid, job.group.load(Ordering::SeqCst)) {
                runs_again(job, hooks, false);
                let said = with_kept(job, |k| k.relaunches.running());
                tell(job, hooks, said, "", restarted);
                // Back before any carry was needed: a move's own line, typed
                // by the step that signalled it, has landed.
                account_land_for(job, hooks);
                return;
            }
            // Back and gone again, or a stop ([`gone_again`]).
            match gone_again(job) {
                None => {
                    // An agent named again since the tab was asked is read
                    // back at the pass this rings, not a back-off later.
                    ring();
                    pause = with_kept(job, |k| k.relaunches.pause());
                    continue;
                }
                Some(Stop::ByCommit) => {}
                Some(Stop::RolledBack(agent)) => take_up_after_rollback(job, hooks, *agent),
                Some(Stop::ForGood) => settled(job),
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
        if !job.admitted() {
            settled(job);
            return;
        }
        if !job.left.is(Word::Left) {
            // Read a Commit stop through the existing wait/rollback path;
            // it carries debt even though it must start no new attempt.
            continue;
        }
        if let Some(said) = with_kept(job, |k| k.relaunches.decide(decision)) {
            settled(job);
            forget_snapshot(job, hooks);
            left_alone(job, hooks, decision, said);
            return;
        }
        let step = attempt();
        if !job.admitted() {
            settled(job);
            return;
        }
        // A CARRIED LOOK ATTEMPTED NOTHING (the review of ruling 20,
        // 2026-10-14): the host's own word for an asked exit it carries
        // through a hand moved no upgrade state, so the owner's view is owed
        // no look and the host no wake; a step that ran owes both. Rung, a
        // carry through an hour's hold woke the host's full follow every
        // `relaunch::BUSY`. (A put-off look, `WAIT_ACT_ENDS`, still rings:
        // bounded by its stamp's life.)
        if step != relaunch::WAIT_HAND_LIFTS {
            note_upgrade_act();
        }
        let outcome = relaunch::outcome(&step);
        let said = with_kept(job, |k| k.relaunches.attempted(&outcome, Instant::now()));
        tell(job, hooks, said, &step, restarted);
        match outcome {
            Outcome::NotYet(_) => pause = with_kept(job, |k| k.relaunches.pause()),
            Outcome::Busy => pause = relaunch::BUSY,
            Outcome::Relaunched => {
                // What a relaunch of the NEW agent needs, read while it runs
                // — a read that fails tried again while the agent holds its
                // tab and no pass has named it (round eighteen,
                // [`read_landed`]): its landing's watch needs the process.
                let fresh = relaunches(job.agent)
                    && read_landed(
                        sid,
                        &job.kept,
                        hooks,
                        job.group.load(Ordering::SeqCst),
                        &|| job.left.is(Word::Left),
                    );
                let read = !relaunches(job.agent) || fresh;
                // LANDED AND GONE AGAIN (round two of the joint audit of
                // 2026-10-01): the agent this line started — the host read
                // it back in the tab ([`Worker::back`]), or a Commit's stop
                // came — is gone before this worker could read it. Nothing
                // describes it to a worker the host would hand its exit: the
                // host's follow forgot the kept snapshot as soon as another
                // process held the tab ([`follow_snapshot`]), no read of the
                // new one succeeded, and its start removed the record the
                // last exit left — that reader said `nothing-recorded`, or
                // read a graceful exit, and relaunched nothing. The exit is
                // still this relaunch's, decided as one back and gone again
                // in the wait above: tried again on the record read at the
                // exit, or owed through the Commit. So is one no pass named
                // before it ended ([`Word::Left`], round three): read by no
                // pass and no read here, it was nobody's to watch, and
                // ending here left the tab at its shell with nothing said.
                // One alive and named meanwhile ends the pause, read back
                // ([`Worker::back`]).
                // NOT A RESTART'S CARRY (the review of 6c7df720f, 2026-10-02): no
                // carry types a relaunch line twice, so a try again held the
                // harness's hand on the tab for the dead process 90 s at a time
                // until its record went stale, then said it could never come back
                // (`refused:no-resume`, a relaunch that "never registered") — and
                // a restart's carry answered with an end is said Cannot by its
                // own rule ([`relaunch::Relaunches::attempted`]).
                // THE RESIDUAL, STATED: for a restart's carry this loop never
                // relaunches a relaunched agent that dies before any read
                // (since round eighteen a read that fails is made again while
                // the agent holds its tab and no pass names it,
                // [`read_landed`]: only one gone before any read stays). Where
                // the host read it back, [`runs_again`] leaves the host's mark,
                // and the worker handed that exit holds nothing of the dead agent
                // but, at most, the restarted agent's snapshot. A forgotten
                // snapshot says `nothing-recorded`. A Claude Code's carries
                // again, which closes the record and answers `landed` (the ledger
                // says `failed:exited-before-continuing`). A Codex's record is no
                // longer the restart's, so its lane decides on the shell's word
                // for the command that just ran — the dead agent's own exit: it
                // closes the record (`failed:exited-before-continuing`), ends a
                // clean exit (`ended:graceful-exit`) and relaunches a crash on
                // the restarted agent's conversation. With no mark, no worker is
                // handed the exit at all. So only a crashed Codex with its
                // snapshot kept is relaunched; for the rest it is accepted: no
                // read described the dead agent, so no relaunch could name its
                // conversation, and the tab is left at its shell, as before
                // 6c7df720f, never held for a dead process and said Cannot.
                if !restarted
                    && !read
                    && !(hooks.still_wanted)(sid, job.group.load(Ordering::SeqCst))
                {
                    match gone_again(job) {
                        None => {
                            ring();
                            pause = with_kept(job, |k| k.relaunches.pause());
                            continue;
                        }
                        Some(Stop::ByCommit) => return,
                        Some(Stop::RolledBack(agent)) => {
                            take_up_after_rollback(job, hooks, *agent);
                            return;
                        }
                        Some(Stop::ForGood) => {}
                    }
                }
                runs_again(job, hooks, fresh);
                // AN ACCOUNT MOVE THIS CARRY LANDED IS A MADE MOVE: its
                // record and its row are written now, before the loop that
                // takes the relaunched agent starts and reads them
                // ([`account_land`]). Nothing is kept flying with no roster.
                account_land_for(job, hooks);
                return;
            }
            Outcome::Left(why) => {
                left_as_its_end(job, hooks, &why, said);
                return;
            }
            Outcome::Cannot(_) => {
                settled(job);
                forget_snapshot(job, hooks);
                account_gone(job, hooks);
                return;
            }
        }
    }
}

/// How soon a landed agent no read reached is read again ([`read_landed`]),
/// through the host's pause seam.
const LANDED_LOOK: Duration = Duration::from_secs(1);

/// THE AGENT A RELAUNCH LANDED, READ (round eighteen; the residual round
/// seventeen stated: a landing whose new agent could not be read armed
/// nothing): what a relaunch of it needs, read into session `sid`'s
/// [`Kept`] ([`refresh_snapshot`]) — and, while that read fails, read again
/// every [`LANDED_LOOK`] for as long as `waiting` says the landing is still
/// the reader's (no pass has named the agent, no stop came) and the agent
/// still holds its tab ([`Hooks::still_wanted`], in `group`). `true` once it
/// is read. Its landing's watch needs the process ([`watch_exit`]): read
/// once, a read the control socket refused, or a roster that had not named
/// the tab's group yet, left the agent the landing started watched by
/// nobody until a pass named it — the whole back-off of a host thread down
/// after a panic — and its crash meanwhile was read by nobody and never
/// said. One no read ever reaches is an agent nothing describes: its exit,
/// as any such agent's, is `nothing-recorded` to whoever reads it. Once
/// `waiting` ends — a pass named it — it is read once more, the agent's own
/// worker to read it after; one gone before any read is the caller's
/// (a relaunch's exit still: [`relaunch_until`]).
fn read_landed(
    sid: &str,
    kept: &Mutex<Kept>,
    hooks: &Hooks,
    group: i32,
    waiting: &dyn Fn() -> bool,
) -> bool {
    loop {
        if refresh_snapshot(sid, kept, hooks) {
            return true;
        }
        if !waiting() {
            return refresh_snapshot(sid, kept, hooks);
        }
        if !(hooks.still_wanted)(sid, group) {
            return false;
        }
        wait_for(Instant::now() + (hooks.pause)(LANDED_LOOK), || !waiting());
    }
}

/// AN EXIT LEFT AS ITS LAUNCH'S OWN END, `why` ([`Outcome::Left`], `said`
/// the attempt's word): nothing owed, what was read of the agent forgotten,
/// journaled, and the accounts recorder told — save for `not-exited`
/// ([`relaunch::outcome`]: suspended or put in the background by someone,
/// its process still there; an explicit restart's program only once its
/// stamp no longer stands: [`relaunch::WAIT_ACT_ENDS`], [`on_agent_left`]).
/// Nothing is owed any more, so what a failing relaunch said ends — NOT
/// what a restart of aterm's own just said it cannot do (`Say::Cannot`,
/// [`relaunch::Relaunches::attempted`]): that word is the tab's last, and
/// ending it here left the tab silent.
fn left_as_its_end(job: &WorkerJob, hooks: &Hooks, why: &str, said: Say) {
    let sid = job.sid.as_str();
    settled(job);
    forget_snapshot(job, hooks);
    aterm_log::info!("harness @{sid}: the agent exited; not relaunched: {why}");
    if why != "not-exited" {
        account_gone(job, hooks);
    }
    if left_ends_relaunch_line(said) {
        (hooks.note)(HostNote::Relaunch {
            sid: sid.to_string(),
            said: RelaunchSay::Ended,
        });
    }
}

/// Whether the exit of the agent `snap` describes is its launch's own end,
/// read as the relaunch reads it ([`read_at_exit`]: as it was seen, then
/// [`relaunch::launch_ended`]); `nothing-recorded` where
/// nothing was read of the agent while it ran (a wrapper or a script, not a
/// launch the relaunch brings back). `None`: a relaunch on exit was owed.
fn launch_end_read(
    job: &WorkerJob,
    hooks: &Hooks,
    snap: Option<&Snapshot>,
) -> Option<&'static str> {
    let Some(snap) = snap else {
        return Some("nothing-recorded");
    };
    let left = read_at_exit(job, hooks, snap);
    relaunch::launch_ended(snap, &left)
}

/// THE AGENT THE HOST READ BACK IS GONE AGAIN before this worker on its
/// relaunch read it — or a stop came first ([`relaunch_until`]: in its
/// back-off, or at a landing whose agent ended before it was read).
///
/// NO STOP SINCE (the joint audit of 2026-10-01): that agent — this
/// relaunch's own, named while its line's act still waited on it, that ended
/// as it started, or one someone started — left no exit of its own: a worker
/// on its relaunch is handed no exit again ([`visit_workers`]). It hands the
/// exit to itself, takes the host's mark back ([`Kept::unwatched`]) and tries
/// again after its pause, on what was read at the exit (`None`). Read as a
/// stop for good — one cleared flag said both — the relaunch was dropped
/// with nothing said and no worker left for a tab at its shell. One no pass
/// read back ([`Word::Left`]: a landing whose agent ended before the roster
/// named it, round three) is the worker's still: nothing to take back.
///
/// A PARK'S COMMIT STOPPED THE WORKER, the agent not back (round six, F13) —
/// the fact the Commit recorded on the session before its stop reached this
/// worker ([`Committed`]), never whether a park holds NOW: a failed Commit's
/// rollback lifts the park, and a worker scheduled only after it read its
/// stop as one for good (round eleven). What the back-off owes stays owed —
/// the handoff layout carried it to the successor, and a Commit that fails
/// hands it to this host again ([`HostHandle::relaunch_stranded`]) — or, the
/// rollback over already, is taken up ([`Stop::RolledBack`]): the host's
/// mark taken back either way, so it is relaunched once. A stop for good
/// leaves the mark: what reads it then is the host's.
fn gone_again(job: &WorkerJob) -> Option<Stop> {
    with_kept(job, |k| {
        // Under the session's lock, which [`Worker::back`] reads the agent
        // back under: one of the two sees the other.
        if job.left.is(Word::Left) {
            return None;
        }
        if job.left.hand() {
            k.unwatched = None;
            return None;
        }
        let stop = k.stopped(&job.sid);
        if !matches!(stop, Stop::ForGood) {
            k.unwatched = None;
        }
        Some(stop)
    })
}

/// Whether an exit the relaunch left ([`Outcome::Left`]) ends the lines the
/// failing relaunch raised: not when the same step said a restart of aterm's own
/// cannot be brought back ([`Say::Cannot`]), which is the tab's last word.
fn left_ends_relaunch_line(said: Say) -> bool {
    said != Say::Cannot
}

/// The relaunch this session's back-off owed is no longer owed ([`Kept::owed`]):
/// it landed, was left, can never land, or the agent is back — and nothing of
/// it is a Commit's rollback's to take up ([`Committed`]).
fn settled(job: &WorkerJob) {
    with_kept(job, settle);
}

/// [`settled`], under the session's lock.
fn settle(k: &mut Kept) {
    k.owed = None;
    k.commit = Committed::No;
}

/// Revoke only debt that names an agent the policy denies. An orphaned
/// landing's watch uses its own identity, not a now-absent worker's.
fn revoke_kept(kept: &mut Kept, switches: &Switches) {
    if kept
        .owed
        .as_ref()
        .is_some_and(|owed| !switches.on_for(owed.snap.agent().program()))
    {
        settle(kept);
    }
    if kept
        .unwatched
        .is_some_and(|exit| !switches.on_for(exit.agent))
    {
        kept.unwatched = None;
    }
    let revoked_watch = kept.exit_watch.as_ref().is_some_and(|watch| {
        watch.hands.is_some_and(|exit| !switches.on_for(exit.agent))
            || kept
                .snapshot
                .as_ref()
                .is_some_and(|snap| watch.names(snap) && !switches.on_for(snap.agent().program()))
    });
    if revoked_watch {
        kept.exit_watch = None;
    }
}

/// THE AGENT RUNS IN THE TAB AGAIN — the relaunch landed, or the agent is
/// back — and this worker ends ([`settled`]), the agent's own worker to
/// start at the host's next pass that finds it in the tab. Until then its
/// exit is the host's ([`Kept::unwatched`]) where the host read it there
/// already ([`Worker::back`] marked it so, and nothing took the mark back),
/// or where a park's Commit stopped this worker (its mark on the session,
/// [`Committed`], recorded before its stop): the Commit leaves every exit of
/// an agent running at its stop to the host (round thirteen), and a host
/// supervising again after a failed Commit hands it. Not after a stop for
/// good. An agent the landing started and nothing read, gone already, is
/// not here: its exit is still the relaunch's ([`gone_again`], round two),
/// read back by a pass or not (round three). A restart's carry is the
/// exception: it is never tried again, so its landing comes here with that
/// agent gone, and nothing here relaunches it (only the Codex lane's exit
/// worker may, for a crash; [`relaunch_until`], THE RESIDUAL, STATED, says
/// when).
///
/// A RELAUNCH THAT LANDED (`landed`: the conversation registered, the act's
/// `adopted`, and the agent it started read alive — its snapshot is the
/// session's now) and that no pass has named yet is left to the roster,
/// whose naming starts its worker — no mark: an exit read for an agent the
/// roster has not caught up with yet would be the exit of an agent alive in
/// its tab — and WATCHED until a pass names it (round seventeen, the review
/// of its first cut): its exit watch hands that exit itself once it has the
/// kernel's word of the very process's end ([`ExitWatched::hands`]). That
/// agent ending between the landing and the pass that names it was the
/// residual stated here: read by nobody, never relaunched — the moment
/// before the next pass while the host runs, but the whole back-off while
/// its thread is down after a panic, through which this worker carries on
/// ([`host_restarted`]). A landing whose agent a first read did not reach
/// reads it again first, while it holds its tab and no pass names it
/// ([`read_landed`], round eighteen): it arms nothing only for an agent no
/// read ever reaches, or one a pass named first (its own worker's then).
/// The cold restore's queue arms the same watch at its landings, and keeps
/// its attempt for one no read reaches yet ([`restored_landing_look`],
/// [`restored_landing_ends`]).
fn runs_again(job: &WorkerJob, hooks: &Hooks, landed: bool) {
    let hands = with_kept(job, |k| {
        if job.revoked.load(Ordering::SeqCst) {
            return None;
        }
        let group = job.group.load(Ordering::SeqCst);
        if matches!(k.commit, Committed::Stopped | Committed::TakenUp(_)) {
            k.unwatched = Some(Unwatched {
                agent: job.agent,
                group,
            });
        }
        settle(k);
        landed.then_some(Unwatched {
            agent: job.agent,
            group,
        })
    });
    if hands.is_some() {
        watch_exit_guarded(&job.sid, &job.kept, hooks, hands, Some(&job.revoked));
        let pid = with_kept(job, |k| k.snapshot.as_ref().map(|s| s.pid));
        aterm_log::info!(
            "harness @{}: the relaunch landed (pid {}); its agent's exit is watched until a pass \
             names it",
            job.sid,
            pid.map_or_else(|| "-".to_string(), |p| p.to_string())
        );
    }
}

/// A WORKER THE COMMIT STOPPED DECIDES AFTER THE ROLLBACK
/// ([`Committed::TakenUp`]): what it owed is this host's again, as
/// [`HostHandle::relaunch_stranded`] would have queued it had the worker
/// decided first — on the restored queue, never by this stopped worker.
///
/// UNLESS ITS TAB CLOSED SINCE (round twelve): the rollback may be long
/// over when the worker decides, and a closed tab owes nothing — queued, it
/// held the automatic update through its `wait:tab-not-live` retries and
/// ended in a row saying an agent did not come back.
///
/// A HOST SHUTTING DOWN QUEUES NOTHING ([`queue_restored`]), AND THE
/// RELAUNCH STAYS OWED (round sixteen; the review of round fifteen): a quit
/// during a failed Commit closes the restored queue, and this worker can
/// decide after it. The queue refused it, [`Kept::stopped`] had taken it
/// off the session, and [`HostHandle::pending_restored_for`] — the quit
/// layout's reader of what is owed in a tab, for which the close keeps
/// every debt ([`RestoredQueue::closed`]) — carried nothing for the tab.
/// (That layout asks first for the agent the host last published there
/// ([`published_snapshot`]), which names the crashed agent still unless a
/// follow forgot it: another program held the tab.) It is left owed,
/// stranded as a worker deciding before the rollback leaves it
/// ([`Committed::Stranded`]), for the layout to carry to the next launch.
/// (The rollback's own [`HostHandle::relaunch_stranded`] runs on the
/// window's thread inside its event loop, and the quit's shutdown after
/// that loop: the close never refuses it.)
fn take_up_after_rollback(job: &WorkerJob, hooks: &Hooks, agent: RestoredAgent) {
    let Some(shared) = job.host.upgrade() else {
        return;
    };
    let sid = agent.sid.clone();
    if !(hooks.acts.open)(&sid) {
        aterm_log::info!(
            "harness @{sid}: the tab closed after aterm's update did not complete; its agent's \
             relaunch is over"
        );
        return;
    }
    let place = agent.place.clone();
    match queue_restored_guarded(&shared, hooks, vec![agent], Some(&job.revoked)) {
        Ok(()) => aterm_log::info!(
            "harness @{sid}: aterm's update did not complete; relaunching the agent that exited \
             {place}"
        ),
        Err(refused) => {
            with_kept(job, |k| {
                if job.revoked.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(agent) = refused.into_iter().next() {
                    k.owed = Some(OwedRelaunch {
                        snap: agent.snap,
                        asked: agent.asked,
                    });
                    k.commit = Committed::Stranded;
                }
            });
            aterm_log::info!(
                "harness @{sid}: aterm's update did not complete and aterm is shutting down; the \
                 relaunch of the agent that exited {place} stays owed, for the layout it saves"
            );
        }
    }
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
    tell(job, hooks, said, why, false);
    account_gone(job, hooks);
}

/// Say a relaunch's word (`why`: the step, or the limit, that said it). A step
/// word (`refused:shell-gone`, `wait:resume`) is the log's. What the OWNER is
/// told follows the rule that an alert persists only where they can act and it
/// says why and what ([`RelaunchSay`]): a relaunch that keeps failing and one
/// the owner limited are records; an agent that cannot be brought back is the
/// tab's mark only with the command that resumes it (`claude --resume <id>`),
/// else one line of the exceptions digest — save one its move's own row asks
/// the owner about (`told:<why>`), said there alone; and a restart of the
/// harness's own (`restarted`) says aterm ended the agent.
fn tell(job: &WorkerJob, hooks: &Hooks, said: Say, why: &str, restarted: bool) {
    let sid = job.sid.as_str();
    let why: String = why.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = why.len().min(80);
    while !why.is_char_boundary(end) {
        end -= 1;
    }
    let why = &why[..end];
    let told = |said: RelaunchSay| {
        (hooks.note)(HostNote::Relaunch {
            sid: sid.to_string(),
            said,
        });
    };
    match said {
        Say::Nothing => {}
        Say::Clear => {
            (hooks.badge)(sid, None);
            told(RelaunchSay::Ended);
        }
        // THE MOVE'S OWN ROW ASKS (`told:`, `relaunch::outcome`): an
        // upgrade's record that stopped after its agent's exit raises
        // `agent-down` with the one remedy (a fresh restart's: a plain
        // `claude`), and this host says nothing over it — said here too, it
        // was a second report in other words and a tab mark telling
        // `claude --resume` of a conversation Claude may not resume (the
        // review of the 2026-10-09 sweep fixes). Its own lines end.
        Say::Cannot if why.starts_with("told:") => {
            aterm_log::info!(
                "harness @{sid}: aterm ended the agent and cannot bring it back ({why}); its \
                 upgrade's own row asks the owner"
            );
            told(RelaunchSay::Ended);
        }
        Say::Cannot => {
            let command = resume_line(job);
            let reason = cannot_reason(why);
            let text = cannot_text(restarted, &reason, command.as_deref());
            aterm_log::warn!("harness @{sid}: {text} ({why})");
            // The tab's mark only where the ask is whole: why, and the one command.
            if command.is_some() {
                (hooks.badge)(sid, Some(&text));
            }
            told(RelaunchSay::Cannot {
                reason,
                restarted,
                command,
            });
        }
        Say::Failing => {
            aterm_log::warn!(
                "harness @{sid}: the agent exited and its relaunch keeps failing (still trying) \
                 ({why})"
            );
            told(RelaunchSay::Failing {
                blocked_by: blocked_cause(why),
                restarted,
            });
        }
        Say::Limited => {
            aterm_log::warn!("harness @{sid}: the agent exited and is not relaunched: {why}");
            told(RelaunchSay::Limited {
                why: why.to_string(),
            });
        }
    }
}

/// The cause a PERSON clears, of a relaunch that keeps failing after `step`: a
/// hold or a hand on the tab (`wait:held`), a foreground job keeping the shell
/// from its prompt (`wait:shell-prompt`), the conversation open in another tab.
/// Any other step the retry gets past on its own, and it stays in the log.
fn blocked_cause(step: &str) -> Option<&'static str> {
    match step {
        "wait:held" => Some("held"),
        "wait:shell-prompt" => Some("shell-prompt"),
        "wait:conversation-in-other-tab" => Some("other-tab"),
        _ => None,
    }
}

/// A refusal ([`relaunch::outcome`]'s `Cannot` word) in a person's words.
fn cannot_reason(step: &str) -> String {
    let step = step
        .strip_prefix("refused:")
        .or_else(|| step.strip_prefix("busy:"))
        .unwrap_or(step);
    match step {
        "shell-gone" => "its shell is gone".to_string(),
        "one-shot" => "it ran one prompt and ended".to_string(),
        "self" => "it is aterm's own agent".to_string(),
        other if other.starts_with("argv:") => {
            "it was started with options aterm cannot repeat".to_string()
        }
        other if other.starts_with("state") => "aterm's state folder cannot be used".to_string(),
        // The stale bound (tab #1, 2026-10-01: read as "aterm refused it"): no
        // try typed the line in time — what held each one is in the log and
        // the ledger (`relaunch::expired` names the wait).
        "stale-exit" => "no try could type at its prompt in time".to_string(),
        // The relaunched agent ended as it started, or never came up: the
        // owner's view says it in these words (`upgrade_status`'s
        // `stop_words`), never `aterm refused it` (the review of the
        // 2026-10-09 sweep fixes).
        "no-resume" => "the new agent never picked the conversation back up".to_string(),
        _ => "aterm refused it".to_string(),
    }
}

/// The tab mark for an agent that cannot be brought back: why, and the one
/// command, never cut. Within the server's keyed attention (200 bytes), what
/// gives way, in order: every carried flag that sets no permission mode
/// ([`resume::mode_of`]), the reason, and last the permission mode itself
/// (the bare line, [`resume::bare_of`]) — the last flag a line gives up
/// (round three of the joint audit of 2026-10-01: an orchestrator told a
/// bare line comes back in another permission mode).
fn cannot_text(restarted: bool, reason: &str, command: Option<&str>) -> String {
    let head = if restarted {
        "aterm ended the agent and cannot bring it back"
    } else {
        "the agent exited and aterm cannot bring it back"
    };
    let Some(command) = command else {
        return format!("{head} ({reason})");
    };
    let mode = aterm_agent::harness::resume::mode_of(command);
    let bare = aterm_agent::harness::resume::bare_of(command);
    let with = |command: &str| format!("{head} ({reason}); run {command} in this tab");
    let without = |command: &str| format!("{head}; run {command} in this tab");
    std::iter::once(with(command))
        .chain(mode.as_deref().map(with))
        .chain(mode.as_deref().map(without))
        .chain(bare.as_deref().map(with))
        .find(|text| text.len() <= 190)
        .unwrap_or_else(|| {
            bare.map_or_else(
                || format!("{head}; resume it in this tab"),
                |bare| without(&bare),
            )
        })
}

/// The command that resumes the tab's agent by hand, from the snapshot the host
/// kept of it ([`resume_told`]: `None` for a launch the rewrite refuses as not
/// resumable in place, and for a Codex whose thread was not named).
fn resume_line(job: &WorkerJob) -> Option<String> {
    resume_told(&with_kept(job, |k| {
        k.snapshot
            .clone()
            .or_else(|| k.owed.as_ref().map(|owed| owed.snap.clone()))
    })?)
}

/// THE LINE A PERSON IS TOLD TO RUN to resume the agent `snap` describes on
/// its own conversation: the mark of an agent aterm cannot bring back
/// ([`resume_line`]) and the loop's memory warning, frozen mail and turn-end
/// escalation ([`WorkerIdle::resume_command`]) alike, through the one rule
/// the footer's remedy goes through too
/// ([`upgrade_drive::resume_command_kept`]): a `--model` the conversation's
/// record marks failed is not printed for a person to start on again. A
/// Codex's is `codex resume <thread>` ([`codex_resume_line`]). `None` for a
/// launch the rewrite refuses as not resumable in place, and for a Codex
/// whose thread the read did not name, or could not tell from another's.
fn resume_told(snap: &Snapshot) -> Option<String> {
    match snap.agent() {
        upgrade::Agent::Claude => upgrade_drive::resume_command_kept(
            harness_state().as_deref(),
            &snap.argv,
            snap.session.as_deref()?,
        ),
        upgrade::Agent::Codex => codex_resume_line(snap, snap.codex.as_ref()?),
    }
}

/// `codex resume <its kept flags> <thread>` ([`resume::codex_command`]), behind
/// `CODEX_HOME=<value>` where the Codex was started with one
/// ([`CodexRun::home_env`], as its relaunch line carries it): the line a
/// person runs to resume the Codex `snap` describes on its own thread, which
/// every surface that prints it steps down and keeps whole as it does Claude
/// Code's ([`resume::shorter`], [`resume::never_cut`]). `None` where the read
/// named no thread, or could not tell it from another client's
/// ([`CodexRun::ambiguous`]: the line would resume that one), and where
/// [`resume::codex_command`] says none.
///
/// A RELATIVE `CODEX_HOME` is the TUI's directory's (Codex resolves it
/// against its own working directory as it starts), and this line names no
/// directory: a person runs it wherever they read it (the Messages row, a
/// new tab). So it carries the home the TUI resolved, [`CodexRun::home`];
/// an absolute value is carried verbatim, as the relaunch line carries it.
fn codex_resume_line(snap: &Snapshot, run: &CodexRun) -> Option<String> {
    if run.ambiguous {
        return None;
    }
    let home = run.home_env.as_deref().map(|value| {
        if value.is_empty() || std::path::Path::new(value).is_absolute() {
            value
        } else {
            run.home.to_str().unwrap_or(value)
        }
    });
    resume::codex_command(&snap.argv, snap.session.as_deref()?, home)
}

struct Worker {
    agent: Program,
    revoked: Arc<AtomicBool>,
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
    left: Arc<ExitWord>,
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
    /// [`WorkerJob::kept`]: where [`HostHandle::suspend`] records the
    /// Commit's stop ([`Committed`]).
    kept: Arc<Mutex<Kept>>,
    /// [`WorkerJob::left_at_commit`].
    left_at_commit: Arc<AtomicBool>,
}

impl Worker {
    fn stop(&self) {
        request_stop(&self.stop, &self.left, &self.interrupt);
    }

    /// Stop it because its agent LEFT a tab that lives on: the worker
    /// handles the exit ([`on_agent_left`]) as it ends.
    ///
    /// Handed after a Commit's stop (a pass that began before
    /// [`HostHandle::suspend`]), the exit is that Commit's to keep too
    /// ([`WorkerJob::left_at_commit`]): the session's lock orders this
    /// against `suspend`'s look at `left`, so one of the two sees the other.
    fn leave(&self) {
        hand_exit(
            &self.kept,
            &self.left_at_commit,
            &self.left,
            &self.stop,
            &self.interrupt,
        );
    }

    /// Its agent is BACK in the tab — relaunched, or started by someone —
    /// while the worker was handed its exit: the exit is over. That worker
    /// is stopped, and only finishes deciding the exit, so the agent the host
    /// reads here (`agent`, as the roster names it) is the host's to watch
    /// until its own worker starts ([`Kept::unwatched`]): marked under the
    /// session's lock, the one the worker takes the mark back under when it
    /// reads that agent gone again ([`relaunch_until`]). Whether the word
    /// moved (`false`: no exit handed, or a stop came first).
    fn back(&self, agent: Program) -> bool {
        let back = with_kept_of(&self.kept, |k| {
            let back = self.left.back();
            if back {
                // [`stopped`] keeps this mark.
                k.unwatched = Some(Unwatched {
                    agent,
                    group: self.group.load(Ordering::SeqCst),
                });
            }
            back
        });
        if back {
            ring();
        }
        back
    }

    /// Stop it for a worker that takes the session straight after: its
    /// badges stay for that one to adopt, never cleared and raised again
    /// (the reliability review of 2026-09-24: a policy edit dropped every
    /// idle-point escalation and re-notified every box). "Straight after" is
    /// the host's next pass that finds the agent in the tab — past a Commit's
    /// whole hold when one comes first — so it ends leaving its agent's exit
    /// to the host ([`stopped`], [`Kept::unwatched`]).
    fn hand_over(&self) {
        let handed = with_kept_of(&self.kept, |_| {
            if self.revoked.load(Ordering::SeqCst) {
                return false;
            }
            self.handover.store(true, Ordering::SeqCst);
            true
        });
        if handed {
            self.stop();
        }
    }
}

/// [`Worker::leave`]'s act, over the handles it reaches the worker through —
/// the ones a [`StopHandle`] holds too ([`StopHandle::leave`], a test's).
fn hand_exit(
    kept: &Mutex<Kept>,
    left_at_commit: &AtomicBool,
    left: &ExitWord,
    stop: &AtomicBool,
    interrupt: &Mutex<Option<Interrupter>>,
) {
    with_kept_of(kept, |k| {
        if matches!(k.commit, Committed::Stopped | Committed::TakenUp(_)) {
            left_at_commit.store(true, Ordering::SeqCst);
        }
        left.hand();
        stop.store(true, Ordering::SeqCst);
    });
    ring();
    if let Some(cut) = interrupt
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        cut();
    }
}

/// The host thread's workers; dropping it stops them — the thread's end
/// alone, past the shutdown's own bounded stop ([`shutdown_workers`]).
///
/// NOT A PANIC OF THE HOST'S LOOP (round seventeen, closing the residual
/// round fourteen stated here): the map is the host thread's own, outside
/// the run that panicked, and the restarted run takes it over
/// ([`host_restarted`]). Dropped there, it stopped every worker with no
/// hand-over and no Commit, each ending with no exit left to the host
/// ([`Kept::unwatched`]) — and no mark could be left instead: it would have
/// outlived a harness switched off meanwhile (the restart gave up then, and
/// no pass cleared it), and the next one switched on would hand an exit from
/// before. Now nothing is marked; the workers live on, the host thread runs
/// on — parked while the harness is off, never given up — and the restart
/// holds them to the policy as it is ([`restart_under_policy`]): a
/// switch-off stops them for good, as a pass that finds the harness off
/// does. NOR OF ITS RESTART (round eighteen): the restart is caught as the
/// run is ([`restart_host`]), and works on the host's own state in place
/// ([`host_restarted`]) — moved into a value of its own, a panic there
/// dropped the workers with it.
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
///
/// IT DELAYS THE CLAIM, NOT THE WATCH (round eight of the update audit,
/// #501): the session's worker starts at once and WATCHES the grace
/// ([`WorkerJob::watch_until`]) — it reads what a relaunch of the agent
/// needs and is handed the agent's exit, which it decides as every worker
/// does, unless another supervisor's live claim makes the exit that one's
/// ([`claimed_by_another`]). Held off instead, an agent that crashed inside
/// the grace was left at a bare shell with `[harness] relaunch` on.
pub(crate) const ADOPTED_CLAIM_GRACE: Duration =
    Duration::from_secs(2 * aterm_agent::supervise::CLAIM_RENEW.as_secs() + 5);

/// The sessions [`HostHandle::defer_adopted_claims`] named, and when each may
/// be claimed ([`ADOPTED_CLAIM_GRACE`]); its worker watches until then
/// ([`WorkerJob::watch_until`]).
#[derive(Default)]
struct ClaimGrace {
    /// Named, with their grace, and not started yet: a successor names them
    /// before its Commit, while its host is suspended, and the grace runs
    /// from the first look of the host thread once it is not — the Commit's
    /// resume — which is when the external supervisors' requests start
    /// reaching this instance.
    pending: Vec<(String, Duration)>,
    /// Each session's instant before which its worker only watches: it runs
    /// no loop, so it claims nothing.
    until: HashMap<String, Instant>,
}

impl ClaimGrace {
    /// Start the pending sessions' grace at `now`.
    fn start(&mut self, now: Instant) {
        for (sid, grace) in self.pending.drain(..) {
            self.until.insert(sid, now + grace);
        }
    }

    /// Forget every grace that has run out at `now`.
    fn prune(&mut self, now: Instant) {
        self.until.retain(|_, at| *at > now);
    }

    /// Whether `sid` is held off at `now`.
    fn holds(&self, sid: &str, now: Instant) -> bool {
        self.runs_until(sid, now).is_some()
    }

    /// When `sid`'s grace runs out, while it holds at `now`: until then its
    /// worker only watches ([`WorkerJob::watch_until`]).
    fn runs_until(&self, sid: &str, now: Instant) -> Option<Instant> {
        self.until.get(sid).copied().filter(|at| *at > now)
    }
}

#[derive(Default)]
struct State {
    cfg: SupervisorConfig,
    /// Bumped by every [`HostHandle::set_config`] that changed the policy.
    cfg_gen: u64,
    suspended: bool,
    /// Agent admission wakes a backing-off host without forgiving faults or
    /// handing unrelated workers over to another loop policy.
    admission_gen: u64,
    /// Bumped by every [`HostHandle::resume`]: with [`Self::cfg_gen`], the
    /// NEWS a host thread waiting out its restart's back-off acts on at once
    /// ([`Self::news`]).
    resumes: u64,
    shutting_down: bool,
    /// The adopted sessions held off for their claim grace
    /// ([`ADOPTED_CLAIM_GRACE`]).
    claim_grace: ClaimGrace,
    /// The sessions with a live worker, as the host thread last left them.
    live: Vec<String>,
    /// Their stop flags and interrupters, published under this lock with the
    /// starts that made them, so [`HostHandle::suspend`] stops every one.
    stops: Vec<StopHandle>,
    /// The sessions whose Autopilot the last pass read Off ([`OffSessions`]),
    /// among those it knew of (an agent named, a worker, a session kept): no
    /// relaunch of theirs is promised ([`HostHandle::relaunching`]).
    off: std::collections::HashSet<String>,
    /// SUPERVISION STOOD SWITCHED OFF since a pass last read the roster: the
    /// master switch (`[harness] enabled = false`, or a headless instance's
    /// `headless = false`) at the host's start or at a reload
    /// ([`HostHandle::start`], [`HostHandle::set_config`]) — never a
    /// Commit's hold. Every worker was stopped meanwhile and the person drove
    /// every session, as under each one's own Off: the first pass that reads
    /// the roster again takes it and marks every session it names
    /// ([`HostState::was_off`]), so each worker it starts carries no task
    /// from before ([`IdleHost::lifted_off`]).
    unsupervised: bool,
}

/// A worker's stop flag, its exit flag, its connection's interrupter slot,
/// what the host keeps of its session ([`Kept`]) and its exit kept through a
/// Commit's stop ([`WorkerJob::left_at_commit`]). Read by
/// [`HostHandle::suspend`] alone, which a build with no seamless update (not
/// unix) leaves out.
#[cfg_attr(not(any(unix, test)), allow(dead_code))]
struct StopHandle {
    agent: Program,
    revoked: Arc<AtomicBool>,
    handover: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    left: Arc<ExitWord>,
    interrupt: Arc<Mutex<Option<Interrupter>>>,
    kept: Arc<Mutex<Kept>>,
    left_at_commit: Arc<AtomicBool>,
}

#[cfg(test)]
impl StopHandle {
    /// Its worker LET GO, as [`Worker::leave`] lets one go: a test's hand on
    /// a worker the host thread keeps ([`HostHandle::leave_every_worker`]).
    fn leave(&self) {
        hand_exit(
            &self.kept,
            &self.left_at_commit,
            &self.left,
            &self.stop,
            &self.interrupt,
        );
    }
}

struct Shared {
    state: Mutex<State>,
    headless: bool,
    /// What the instance keeps of its desk ([`crate::desk::Desk`]): whether
    /// the agents a restore hands this host are relaunched at all.
    desk: crate::desk::Desk,
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
    /// Wakes the activation thread's parked wait: at shutdown, and for a
    /// look at the accounts ([`HostHandle::accounts_again`]). Set once, when
    /// that thread starts ([`spawn_activation_wake`]).
    wake: std::sync::OnceLock<WakeTrigger>,
    /// Set (then the bell rung) when the host thread's loop has returned.
    host_done: AtomicBool,
    /// The instance's one API reach probe, shared by every worker's
    /// [`WorkerIdle::reach`]; stopped at the host's shutdown.
    net: Arc<NetProbe>,
    /// The cold restore's relaunch queue ([`RestoredQueue`]; taken alone),
    /// and the condition its worker waits on — a retry's pause, or a
    /// handoff's pause until a rollback resumes it.
    restored: Mutex<RestoredQueue>,
    restored_wake: Condvar,
    /// A TEST'S PANIC INSIDE THE HOST'S RESTART ([`host_restarted`]): set,
    /// the next restart panics once, after it has reset what the restarted
    /// run starts afresh and held the workers to the policy — where a
    /// restart that moved the workers into a value of its own dropped them
    /// with it.
    #[cfg(test)]
    restart_panics: AtomicBool,
}

// THE HOST THREAD IS NEVER GIVEN UP ON (owner ruling of 2026-09-25: only
// an explicit setting takes the harness's power away). A host loop that
// ends in a panic starts again IN PLACE, after [`Hooks::backoff`] — the
// workers' growing ladder, a second up to an hour, counted over the panics
// in a row — with what it held ([`host_restarted`], round seventeen); a
// shutdown ends the wait at once (the bell), and so does news a pass must
// act on ([`State::news`]: the policy changed, a Commit failed), while a
// suspend waits it out as any pass of a suspended host would. Until the
// ruling a process-lifetime budget of three restarts, spent by a policy
// change or a resume, ended supervision for good after one repeating panic,
// with nothing but a log line.

/// The disk watch's period: a free-space figure has no event to hang on
/// (design §5.5, §4.2 row 18), so this is the one timer the host keeps.
const DISK_TICK: Duration = Duration::from_secs(6 * 3600);
/// The first look, a while after the host starts rather than in its first
/// breath.
const DISK_FIRST_LOOK: Duration = Duration::from_secs(120);

impl State {
    /// THE NEWS A HOST THREAD WAITING OUT ITS RESTART'S BACK-OFF ACTS ON AT
    /// ONCE (round seventeen; the review of its first cut): the policy's
    /// generation and the resumes ([`HostHandle::set_config`],
    /// [`HostHandle::resume`]). Its workers live on through the back-off
    /// ([`host_restarted`]), but only a pass starts a worker, and a change of
    /// either is one a pass must act on: a narrowed or switched policy's
    /// successors ([`restart_under_policy`]), a failed Commit's tabs — whose
    /// workers that Commit's stop ended — and a policy switched on again,
    /// which a back-off of up to an hour left unsupervised. (A host thread
    /// that ended at a suspend or a switch-off, as it did before, was started
    /// again by the resume or the switch-on at once.)
    fn news(&self) -> (u64, u64, u64) {
        (self.cfg_gen, self.resumes, self.admission_gen)
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the policy supervises anything in this instance now.
    fn active(state: &State, headless: bool) -> bool {
        would_supervise(&state.cfg, headless) && !state.suspended
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
        switches.set(&cfg, headless);
        let handle = Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    cfg: effective(&cfg),
                    suspended,
                    unsupervised: !would_supervise(&effective(&cfg), headless),
                    ..State::default()
                }),
                headless,
                desk: crate::desk::Desk::of(headless),
                faults: Mutex::default(),
                kept: Mutex::default(),
                switches,
                activations: Arc::default(),
                wake: std::sync::OnceLock::new(),
                host_done: AtomicBool::new(false),
                net: NetProbe::new(),
                restored: Mutex::default(),
                restored_wake: Condvar::new(),
                #[cfg(test)]
                restart_panics: AtomicBool::new(false),
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

    /// Whether the host is suspended for a seamless update's handoff
    /// ([`Self::suspend`]): `harness policy` reads `effective=off
    /// why=suspended` meanwhile.
    pub(crate) fn suspended(&self) -> bool {
        self.shared.lock().suspended
    }

    /// The live agent admission snapshot for owner-facing projections. Read
    /// before the session store/timeline locks, like `supervising`.
    pub(crate) fn admission(&self) -> Admission {
        self.shared.switches.admission()
    }

    /// The agent's live policy gate, independently of a claim that its old
    /// worker may still be returning after a stop.
    pub(crate) fn admits(&self, agent: Program) -> bool {
        self.shared.switches.on_for(agent)
    }

    /// Whether the host RELAUNCHES the agents it supervises on its own —
    /// an exit, its memory restart — (U1): it supervises
    /// ([`Self::supervising`]) under `[harness] relaunch`, read live. One
    /// answer for every session: whether it would relaunch ONE session's
    /// agent is [`Self::relaunching`] and the session's own claim and hands
    /// (`input_stall::host_hand_refusal`). A person's explicit `restart` is
    /// not limited by it (ruling 1, 2026-10-08): a stall's words choose by
    /// `restart_act::offered` instead.
    pub(crate) fn relaunches(&self) -> bool {
        self.supervising() && self.shared.switches.relaunch()
    }

    /// Whether this host TYPES the line that re-keys an agent's degraded shell
    /// (`integration_notice`, the review of 2026-09-29): it supervises the
    /// instance's agent sessions ([`Self::supervising`]) and moves them — the
    /// live upgrade's move (`[harness] upgrade`) or the relaunch on exit
    /// (`[harness] relaunch`), both of which type the relaunch line that takes
    /// a one-use key (`shell_rekey::typed`) — read live. Off, a tab whose
    /// shell lost its key is told a new tab is its remedy, never a move that
    /// will not come.
    pub(crate) fn rekeys_agents(&self) -> bool {
        self.supervising() && (self.shared.switches.upgrade() || self.shared.switches.relaunch())
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
    /// WHY (resume-hint review, 2026-09-26): a remedy said "aterm relaunches
    /// it on its conversation" — and named no command — on
    /// [`Self::relaunches`] alone, one switch for every session, while the
    /// relaunch itself refused launches it cannot carry
    /// (`argv:unknown-flag`, `argv:not-resumable`), waited for ever on a
    /// shell it has no line for, and had nothing to relaunch without a
    /// snapshot. The window reads this set BEFORE it takes the store's lock
    /// (two leaf locks per session, no I/O), and asks per session: the
    /// memory row's word (`status_item::SessionRow::restarts`).
    ///
    /// An Off session is not in it ([`OffSessions`], as the last pass read
    /// them): the host stopped its worker, and relaunches nothing there.
    pub(crate) fn relaunching(&self) -> std::collections::HashSet<String> {
        let mut relaunching = relaunching_of(self.relaunches(), &self.shared.kept);
        let off = self.shared.lock().off.clone();
        relaunching.retain(|sid| !off.contains(sid));
        relaunching
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
    pub(crate) fn live(&self) -> Vec<String> {
        self.shared.lock().live.clone()
    }

    /// The sessions the host's last pass read Off ([`State::off`]).
    #[cfg(test)]
    pub(crate) fn off_sessions(&self) -> std::collections::HashSet<String> {
        self.shared.lock().off.clone()
    }

    /// Let every worker the host last published go ([`StopHandle::leave`]),
    /// each with no lock of the host's held: what a host that implemented
    /// Off as a let-go would do to an Off session's worker.
    #[cfg(test)]
    pub(crate) fn leave_every_worker(&self) {
        let handles: Vec<StopHandle> = self
            .shared
            .lock()
            .stops
            .iter()
            .map(|h| StopHandle {
                agent: h.agent,
                revoked: Arc::clone(&h.revoked),
                handover: Arc::clone(&h.handover),
                stop: Arc::clone(&h.stop),
                left: Arc::clone(&h.left),
                interrupt: Arc::clone(&h.interrupt),
                kept: Arc::clone(&h.kept),
                left_at_commit: Arc::clone(&h.left_at_commit),
            })
            .collect();
        for handle in &handles {
            handle.leave();
        }
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
            // The host thread ends only at shutdown (checked above): a panic
            // of its loop starts the loop again in place, and a policy
            // switched off leaves it parked. One that has FINISHED ended in a
            // panic past that restart ([`Workers`]' drop stopped every worker
            // it ran) — the slot still held its handle and nothing started
            // another (the reliability review of 2026-09-24). Start it again:
            // no budget (see the host loop's own).
            if !running.is_finished() {
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
                // A panic restarts the host IN PLACE after the back-off — never
                // given up on (above) — with what it held ([`host_restarted`]):
                // its workers run on through the back-off, held to the policy
                // as it is ([`restart_under_policy`]). NEWS cuts the back-off
                // short ([`State::news`]: the policy changed, a Commit failed),
                // each once — read against what it was as the back-off began,
                // so a run that panics again waits its next one out and never
                // spins. Only the shutdown ends the thread.
                //
                // THE RESTART IS CAUGHT AS THE RUN IS (round eighteen; the
                // residual round seventeen stated): `host` — the workers in it
                // — stays this thread's own across both, never moved into a
                // value a panic would drop ([`Workers`]' drop stops every
                // worker). A panic in the restart is one more in a row, and
                // the restart is made again a pause later ([`RESTART_AGAIN`]),
                // whatever panicked: never a spin.
                let mut attempt = 0usize;
                let mut host = HostState::from_shared(&shared);
                'host: loop {
                    let began = Instant::now();
                    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        host_loop(&shared, &mut host, &hooks);
                    }));
                    // The host loop returns only at shutdown.
                    let Err(payload) = run else { break };
                    // In a row: a run that lasted an hour starts the count
                    // again, as a worker's failures age out of the same
                    // window ([`RESTART_WINDOW`]).
                    if began.elapsed() >= RESTART_WINDOW {
                        attempt = 0;
                    }
                    attempt += 1;
                    aterm_log::warn!(
                        "harness host stopped after an internal panic ({attempt} in a row): {}",
                        panic_text(payload.as_ref())
                    );
                    let seen = shared.lock().news();
                    loop {
                        let restart =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                restart_host(&shared, &mut host, &hooks, attempt, seen)
                            }));
                        match restart {
                            Ok(true) => break,
                            Ok(false) => break 'host,
                            Err(payload) => {
                                attempt += 1;
                                aterm_log::warn!(
                                    "harness host's restart stopped after an internal panic \
                                     ({attempt} in a row), its workers running on: {}",
                                    panic_text(payload.as_ref())
                                );
                                wait_for(Instant::now() + (hooks.pause)(RESTART_AGAIN), || {
                                    shared.lock().shutting_down
                                });
                            }
                        }
                    }
                    aterm_log::warn!("harness host started again after a panic");
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
    /// are journaled, never retried. A headless instance relaunches nothing
    /// (but under the lab's headless seam, [`crate::desk::Desk`]).
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
    /// pauses it ([`Self::pause_restored`]), the handoff layout carries what
    /// it still owes on each agent's leaf, read live
    /// ([`Self::pending_restored_for`]), and the successor queues them here in
    /// its own process at its Commit; a rollback resumes it
    /// ([`Self::resume_restored`]). A second call while the worker runs adds
    /// to its queue; a host shutting down queues nothing.
    pub(crate) fn relaunch_restored(&self, restored: Vec<RestoredAgent>) {
        // A cold restore's agents, and the rollback's: neither meets a closed
        // queue in a running window ([`take_up_after_rollback`] says why).
        let _ = queue_restored(&self.shared, &self.hooks, restored);
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
    /// it. An attempt already over — an agent ran in its tab, it typed
    /// nothing and no step of it is in flight ([`restored_overtaken`]) — holds
    /// nothing while it waits for its retry to take it off (round thirteen).
    pub(crate) fn restored_pending(&self, now: Instant) -> bool {
        let queue = self
            .shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue
            .attempts
            .iter()
            .any(|a| a.acting || a.typed() || !a.agent_seen)
            && queue.since.is_some_and(|since| now < since + RESTORED_HOLD)
    }

    /// A HANDOFF PARKED THE READERS (round four, plan item 7): the restored
    /// agents' worker takes no new step — the one in flight finishes — and the
    /// queue, the step in flight included, is the handoff layout's to carry
    /// ([`Self::pending_restored_for`]). Idempotent; a rollback undoes it
    /// ([`Self::resume_restored`]), a Commit ends the process.
    ///
    /// NOT FROZEN (round eleven): a copy frozen here kept naming a tab whose
    /// agent a person started during the park, for the successor to step a
    /// relaunch at — read live, an attempt whose tab an agent ran in since it
    /// was queued is carried no more ([`restored_overtaken`]: ran, round
    /// twelve, not runs — one started and stopped again before the Commit's
    /// re-capture counts), and a Commit whose two captures disagree on one
    /// (that agent seen between them, a step in flight landed) is refused as
    /// activity, as for an owed relaunch.
    ///
    /// A RELAUNCH ON EXIT A WORKER'S BACK-OFF STILL OWES is held with it
    /// (round six, F13; [`Kept::owed`]): the workers type no relaunch from
    /// here ([`Switches::parked`]), so the successor relaunches it once, from
    /// the layout. It is NOT frozen: the layout reads it live, so one owed no
    /// more — its agent is back, its try in flight landed — is not carried,
    /// and a Commit whose two captures disagree on it is refused as activity
    /// ([`Self::pending_restored_for`]). Round ten: a copy frozen here kept
    /// naming an agent started again in its tab during the park, for the
    /// successor to relaunch over it.
    #[cfg(any(unix, test))]
    pub(crate) fn pause_restored(&self) {
        self.shared.switches.park(true);
        let mut queue = self
            .shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.paused = true;
    }

    /// Every session whose relaunch on exit is owed now ([`Kept::owed`]), with
    /// the snapshot it is owed on. Each [`Kept`] is read under its own lock
    /// alone, after the map's is let go.
    fn owed_relaunches(&self) -> Vec<(String, OwedRelaunch)> {
        let kept: Vec<(String, Arc<Mutex<Kept>>)> = self
            .shared
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(sid, kept)| (sid.clone(), Arc::clone(kept)))
            .collect();
        let mut owed: Vec<(String, OwedRelaunch)> = kept
            .into_iter()
            .filter_map(|(sid, kept)| {
                let owed = kept
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .owed
                    .clone()?;
                Some((sid, owed))
            })
            .collect();
        owed.sort_by(|a, b| a.0.cmp(&b.0));
        owed
    }

    /// A COMMIT THAT FAILED after a park's stop: every relaunch on exit a
    /// worker the Commit stopped still owed ([`Committed`]) is this process's
    /// again, and no worker will take it (its tab is at its shell) — queued
    /// as the restored agents are ([`Self::relaunch_restored`]), `place`
    /// naming each tab for the row that says one did not come back (round
    /// six, F13). Called after [`Self::resume_restored`].
    ///
    /// WHENEVER THAT WORKER DECIDES (round eleven): one that decided already
    /// left it stranded, and it is queued here; one not scheduled since the
    /// Commit's stop has decided nothing — its agent may be back — and is
    /// told the rollback came ([`Committed::TakenUp`]): it queues what it
    /// still owes itself as it decides ([`take_up_after_rollback`]). Taken
    /// once either way.
    ///
    /// A TAB CLOSED SINCE OWES NOTHING (round twelve): a closed tab's worker
    /// is stopped for good, but a close at the instant of the Commit can
    /// reach the worker after the Commit's mark (the host thread stops it
    /// without the state lock `suspend` reads it under) — what it left
    /// stranded is dropped here, and one told is dropped as it decides.
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
                let stopped = with_kept_of(&kept, |k| {
                    matches!(k.commit, Committed::Stopped | Committed::Stranded)
                });
                if !stopped {
                    return None;
                }
                // The window's words for the tab, and whether it is open,
                // never read under the session's lock (each takes the
                // store's).
                let open = (self.hooks.acts.open)(&sid);
                let place = place(&sid);
                let owed = with_kept_of(&kept, |k| match std::mem::take(&mut k.commit) {
                    Committed::Stranded => k.owed.take().filter(|_| open),
                    Committed::Stopped => {
                        k.commit = Committed::TakenUp(place.clone());
                        None
                    }
                    // Decided in between, its agent back or nothing owed — or
                    // already told.
                    other => {
                        k.commit = other;
                        None
                    }
                })?;
                aterm_log::info!(
                    "harness @{sid}: aterm's update did not complete; relaunching the agent \
                     that exited {place}"
                );
                // Read from the live process that left: its own login is on it.
                Some(RestoredAgent {
                    sid,
                    snap: owed.snap,
                    place,
                    cold: false,
                    asked: owed.asked,
                })
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
        }
        self.shared.restored_wake.notify_all();
    }

    /// The restored agent the queue still owes tab `sid`, as the handoff
    /// layout carries it on the tab's leaf (`App::capture_handoff_layout`):
    /// read from the live queue, parked or not ([`Self::pause_restored`]),
    /// unless an agent ran in the tab since it was queued
    /// ([`restored_overtaken`], rounds eleven and twelve) — and, there, a
    /// relaunch on exit a worker's back-off owes the tab now ([`Kept::owed`],
    /// round six F13). `None` when the tab owes nothing.
    ///
    /// A RELAUNCH OWED AFTER THE PARK is carried too (round six F13, review
    /// two): a crash the worker reads only after [`Self::pause_restored`] —
    /// its exit's detection and [`relaunch::EXIT_SETTLE`] run first — becomes
    /// owed with the readers parked, so the owed relaunches are read live —
    /// and, read live, one owed at the park whose agent is back during it is
    /// carried no more (round ten). A parked worker types none of them, so the
    /// layout naming one relaunches it once.
    ///
    /// The two captures of one park can therefore differ (round seven,
    /// finding 111): a crash the worker reads after the pending layout was
    /// captured is named by the Commit's re-capture only, and an owed
    /// relaunch settled in between — or a restored one whose step in flight
    /// landed, or whose tab an agent ran in since — by the pending layout
    /// only. The Commit compares what the leaves carry apart from the topology
    /// (`app_update_handoff::commit_agent_carry`) and is refused for that
    /// reason, as activity: the rollback resumes the worker, which relaunches
    /// the agent here, and the next attempt carries what is still owed. A
    /// crash the worker reads after the Commit's re-capture — the instant
    /// before [`Self::suspend`], or during the Commit's hold — is on neither
    /// layout as owed, and the Commit's stop leaves it in a process about to
    /// exit: since round eighteen it is the successor's, which watches the
    /// agent the leaf names supervised ([`Self::watch_handed`]).
    ///
    /// EVERY RELAUNCH QUEUED FOR THE TAB IS READ (round thirteen; the review
    /// of round twelve): an overtaken one stays queued until its retry comes
    /// due — up to [`RESTORED_MAX_PAUSE`] on, or the length of a park — and a
    /// tab can be owed a later one meanwhile: an agent a person started there
    /// crashed, and a failed Commit's rollback queued its relaunch
    /// ([`Self::relaunch_stranded`]). Read first-found, the overtaken one hid
    /// it: both captures carried nothing, the Commit went ahead, and the
    /// crashed agent was never relaunched. The latest one not overtaken is
    /// carried: it names the tab's latest agent.
    ///
    /// WITH THE EXPLICIT RESTART'S CAUSE where its act ended the agent
    /// (round eighteen; ruling 20's (iv), [`OwedRelaunch::asked`]): the
    /// handoff layout's leaf carries it (`restore::AgentRestore::cause`), so
    /// the successor's queue relaunches it as the act's and carries it
    /// through a hand that still stands ([`RestoredAgent::asked`]).
    pub(crate) fn pending_restored_for(&self, sid: &str) -> Option<OwedRelaunch> {
        let mut queued: Vec<(usize, OwedRelaunch, bool, bool)> = {
            let queue = self
                .shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            queue
                .attempts
                .iter()
                .filter(|a| a.agent.sid == sid)
                .map(|attempt| {
                    (
                        attempt.order,
                        OwedRelaunch {
                            snap: attempt.agent.snap.clone(),
                            asked: attempt.agent.asked,
                        },
                        attempt.typed(),
                        attempt.agent_seen,
                    )
                })
                .collect()
        };
        queued.sort_by_key(|(order, ..)| std::cmp::Reverse(*order));
        // Whether an agent runs in the tab now is asked once, and only if an
        // attempt needs it (the store's lock, the queue's let go). The first
        // call's answer stands for every later one WHATEVER ITS ARGUMENTS:
        // each is [`restored_overtaken`]'s `(sid, 0)` — this tab, no group of
        // its own — so a cache keyed by nothing answers the one question
        // asked. A caller that asked another tab or group would need its own.
        let runs = std::cell::OnceCell::new();
        let runs_now =
            |tab: &str, group: i32| *runs.get_or_init(|| (self.hooks.still_wanted)(tab, group));
        if let Some((_, owed, ..)) = queued
            .into_iter()
            .find(|(_, _, typed, seen)| !restored_overtaken(*typed, *seen, &runs_now, sid))
        {
            return Some(owed);
        }
        self.owed_relaunches()
            .into_iter()
            .find(|(tab, _)| tab == sid)
            .map(|(_, owed)| owed)
    }

    /// The snapshot [`Self::pending_restored_for`] carries for tab `sid`
    /// (the tests' view of it, which most of them read).
    #[cfg(test)]
    pub(crate) fn pending_restored_snapshot_for(&self, sid: &str) -> Option<Snapshot> {
        self.pending_restored_for(sid).map(|owed| owed.snap)
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
    /// ([`Self::relaunch_restored`]): not in a headless instance (but under
    /// the lab's headless seam, [`crate::desk::Desk`]), and while
    /// `[harness]` is on with `relaunch` allowed. The reopened layout's row
    /// reads it to say an agent resumes rather than was lost (ruling 293).
    pub(crate) fn relaunches_restored(&self) -> bool {
        relaunches_restored(&self.shared)
    }

    /// Whether a restored agent of `agent` is relaunched: where this host
    /// relaunches restored agents ([`Self::relaunches_restored`]), and only
    /// while it admits that agent ([`Self::admits`]) — one switched off by
    /// its own table is relaunched by nobody ([`restored_allowed`]). The
    /// reopened layout's row asks it of each agent the layout carried, so
    /// one switched off is said lost, never resuming.
    pub(crate) fn relaunches_restored_of(&self, agent: upgrade::Agent) -> bool {
        relaunches_restored(&self.shared) && self.admits(agent.program())
    }

    /// WHO ANSWERS a wall of `kind` on a session this host supervises (the
    /// presence row and tab chip): the ONE table the policy
    /// acts on ([`aterm_agent::supervise::policy::outlook::answer`]) under the
    /// policy this host runs. `has_reset`: the notice names a reset.
    pub(crate) fn wall_answer(
        &self,
        kind: aterm_phase::WallKind,
        has_reset: bool,
    ) -> aterm_messages::presence::WallAnswer {
        use aterm_agent::supervise::policy::outlook::{Answer, answer};
        use aterm_messages::presence::WallAnswer;
        let cfg = self.shared.lock().cfg.clone();
        match answer(kind, has_reset, &cfg, cfg.enabled) {
            Answer::Nobody => WallAnswer::Nobody,
            Answer::Resumes => WallAnswer::Harness,
            Answer::ResumesAsks(action) => WallAnswer::HarnessAsks(action),
        }
    }

    /// A reloaded `[harness]` policy: workers restart under it, or all stop
    /// (within one wait) when it is off. The host's own switches
    /// (`upgrade`, `relaunch`) land at each worker's next act, restarting
    /// nothing.
    pub(crate) fn set_config(&self, cfg: SupervisorConfig) {
        let switches = &self.shared.switches;
        let was = (switches.accounts(), switches.notes());
        let agents = switches.agents.load(Ordering::SeqCst);
        switches.set(&cfg, self.shared.headless);
        self.accounts_switched(was);
        let cfg = effective(&cfg);
        let mut state = self.shared.lock();
        let admission_changed = agents != self.shared.switches.agents.load(Ordering::SeqCst);
        if state.cfg == cfg && !admission_changed {
            return;
        }
        // Switched off: every worker stops, and the sessions are the
        // person's until it is on again ([`State::unsupervised`]).
        state.unsupervised |= !would_supervise(&cfg, self.shared.headless);
        for worker in &state.stops {
            if !self.shared.switches.on_for(worker.agent) {
                // A policy revocation is permanent for this worker, including
                // a Commit's earlier pause. Re-enabling admits a new worker;
                // it does not revive that old worker's exit claim.
                with_kept_of(&worker.kept, |kept| {
                    worker.revoked.store(true, Ordering::SeqCst);
                    worker.handover.store(false, Ordering::SeqCst);
                    if kept.owed.is_none() {
                        kept.commit = Committed::No;
                    }
                    revoke_kept(kept, &self.shared.switches);
                });
                worker.left_at_commit.store(false, Ordering::SeqCst);
                request_stop(&worker.stop, &worker.left, &worker.interrupt);
            }
        }
        if state.cfg != cfg {
            state.cfg = cfg;
            state.cfg_gen += 1;
        }
        if admission_changed {
            state.admission_gen += 1;
        }
        drop(state);
        // Mark a removed attempt before sweeping its landing claim. A read
        // already in flight either armed before this mark and is swept, or
        // sees the mark under Kept and cannot arm after the sweep.
        self.shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attempts
            .retain(|attempt| {
                let allowed = restored_allowed(&self.shared, &attempt.agent);
                if !allowed {
                    attempt.revoked.store(true, Ordering::SeqCst);
                }
                allowed
            });
        // Landing claims can outlive their workers. They have no StopHandle,
        // but the same typed admission revokes their retained exit debt.
        let kept: Vec<_> = self
            .shared
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for kept in kept {
            with_kept_of(&kept, |kept| revoke_kept(kept, &self.shared.switches));
        }
        self.shared.restored_wake.notify_all();
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
    ///
    /// THE COMMIT'S STOP IS RECORDED ON EACH SESSION FIRST ([`Committed`],
    /// round eleven): under the session's lock, before the stop reaches its
    /// worker, so the worker reads it however late it decides — after a
    /// failed Commit's rollback too. Every worker no stop has reached is the
    /// Commit's — one its agent's exit was handed to, and one whose agent the
    /// host read back since, which has not decided that exit yet (the joint
    /// audit of 2026-10-01: read off a cleared `left`, that one was taken for
    /// stopped, and a relaunch it went on to owe was nobody's after a failed
    /// Commit). A worker already stopped for good (its tab closed, the
    /// policy) is not; nor is one a changed policy handed over
    /// ([`Worker::hand_over`]), whose successor no pass starts while the
    /// Commit holds the host — it leaves its agent's exit to the host as it
    /// ends ([`stopped`], [`Kept::unwatched`]; round fourteen).
    ///
    /// AN EXIT HANDED TO THE WORKER STAYS ITS TO READ (round twelve): the
    /// stop's word replaces the one that said the worker had its agent's
    /// exit to read — kept on [`WorkerJob::left_at_commit`] first, under the
    /// session's lock, which [`Worker::leave`] takes too.
    #[cfg(any(unix, test))]
    pub(crate) fn suspend(&self) {
        let mut state = self.shared.lock();
        state.suspended = true;
        for worker in &state.stops {
            with_kept_of(&worker.kept, |k| {
                let word = worker.left.read();
                if word != Word::Stopped {
                    k.commit = Committed::Stopped;
                    if word == Word::Left {
                        worker.left_at_commit.store(true, Ordering::SeqCst);
                    }
                }
            });
            request_stop(&worker.stop, &worker.left, &worker.interrupt);
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

    /// NEWS OF THE ACCOUNTS (the accounts store's wake marker was replaced:
    /// the owner's `aterm harness accounts use`, a check that found a login
    /// signed in, a session of any window proving or striking an account, a
    /// limit reset's outcome):
    /// counted, and every worker's loop reads the count at the next step of
    /// its wait ([`IdleHost::account_stamp`]) — a session waiting at its
    /// limit on the roster's advice is decided again within one step, where
    /// the advice is asked afresh. Nothing is parked, nothing rung: a loop
    /// that waits on no advice of the roster's reads the count and does
    /// nothing. Then the host looks at the accounts as a whole and tells the
    /// window where that changed ([`accounts_look`]): on the caller's thread,
    /// the activation wake's, since the look reads files.
    pub(crate) fn note_accounts(&self) {
        self.shared
            .switches
            .accounts_news
            .fetch_add(1, Ordering::SeqCst);
        accounts_look(&self.shared.switches, &self.hooks, false);
    }

    /// THE OWNER'S SWITCHES MOVED: `[harness] accounts` between `off` and a
    /// mode, or `[harness] usage_notes` ([`Self::set_config`]; `was` is the
    /// pair before it) — what the look reads ([`accounts_said`]). The digest's
    /// lines follow them now, not at the next turn end — which under
    /// `[harness] enabled = false` never comes. Both off, there is nothing to
    /// say, and it is said here: no file is read for it. Otherwise the look
    /// reads files and this is the window's thread, so the activation thread
    /// is woken to make it ([`Self::accounts_again`]): under `usage_notes` the
    /// machine's own row may still have its lines with the accounts off.
    /// Turned mute ([`Switches::mute`]), nothing is said.
    fn accounts_switched(&self, was: (accounts::Mode, bool)) {
        let switches = &self.shared.switches;
        let read = |(mode, notes): (accounts::Mode, bool)| (mode == accounts::Mode::Off, notes);
        let now = read((switches.accounts(), switches.notes()));
        if read(was) == now || switches.mute() {
            return;
        }
        if now == (true, false) {
            accounts_tell(switches, &self.hooks, AccountsSaid::Nothing, false);
        } else {
            self.accounts_again();
        }
    }

    /// THE ACCOUNTS ARE LOOKED AT AGAIN, off the caller's thread: what the
    /// window was told is forgotten ([`Switches::accounts_said`]), so the
    /// next look tells whatever it reads, and the activation thread is woken
    /// to make that look now and not at its fallback ([`activation_loop`]).
    fn accounts_again(&self) {
        self.shared
            .switches
            .accounts_said
            .store(0, Ordering::SeqCst);
        if let Some(wake) = self.shared.wake.get() {
            wake.pull();
        }
    }

    /// Supervise again (the successor's Commit activated it, or a Commit
    /// failed and this process keeps its sessions). What this host told the
    /// window of the accounts before then is said again
    /// ([`Self::accounts_again`]): a successor's app loop takes no host note
    /// until its Commit, so the window may hold none of it.
    #[cfg(any(unix, test))]
    pub(crate) fn resume(&self) {
        {
            let mut state = self.shared.lock();
            state.suspended = false;
            state.resumes += 1;
        }
        self.accounts_again();
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
    /// ([`CARRY_SUPERVISED`]): a worker watching its adopted-claim grace
    /// stands down from an exit another supervisor's claim makes that one's
    /// ([`WorkerJob::watch_until`]). Helps from the first update INTO this
    /// build: the record is on disk whoever wrote it.
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
        let words = Arc::clone(&self.hooks.autopilot);
        let pause = Arc::clone(&self.hooks.pause);
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}carried"))
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                carry_in_flight_after_handoff(
                    &shared,
                    carry.as_ref(),
                    words.as_ref(),
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

    /// THE AGENTS THE OUTGOING INSTANCE SUPERVISED, WATCHED FROM THIS
    /// SUCCESSOR'S COMMIT (round eighteen; `HostHandle::pending_restored_for`'s
    /// stated residual, a crash after the Commit's re-capture on neither
    /// layout): each `(sid, snapshot)` the agent the outgoing host last
    /// published for an adopted tab where it owed no relaunch
    /// (`restore::TerminalLeafRestore::supervised`). Each is watched as a
    /// landing's agent is ([`landing_watched`]): the first pass that names an
    /// agent in its tab ends the watch, and its own worker takes it up; one
    /// that ENDED — during the outgoing instance's Commit hold, read there by
    /// a process about to exit and acted on by nobody (parked, then stopped),
    /// or before this host's first pass — has its exit read at the kernel's
    /// word and handed by this host's next pass to a worker that reads it
    /// alone ([`unwatched_exits`], [`exit_main`]): relaunched, left or said
    /// by the rules any exit is, once — the outgoing instance typed nothing
    /// after its park, and its Commit compared the process it names
    /// (`app_update_handoff::commit_agent_carry`), so an exit it decided first
    /// refused that Commit. An agent the harness's own restart ended is not
    /// watched: this host's carry takes it on from its record
    /// ([`Self::resume_after_handoff`], [`Acts::restarted`]). A tab whose
    /// snapshot names another process already (its worker read one) is that
    /// worker's. Nothing while the harness is off.
    pub(crate) fn watch_handed(&self, agents: Vec<(String, Snapshot)>) {
        if agents.is_empty() || !would_supervise(&self.shared.lock().cfg, self.shared.headless) {
            return;
        }
        for (sid, snap) in agents {
            if !self.shared.switches.on_for(snap.agent().program()) {
                continue;
            }
            if (self.hooks.acts.restarted)(&sid, snap.agent(), Some(snap.pid)) {
                aterm_log::info!(
                    "harness @{sid}: the agent the previous aterm supervised here was ended by its \
                     own restart; its carry takes it on"
                );
                continue;
            }
            // Admission and claim creation share set_config's state -> Kept
            // order. A revocation either prevents this claim or sweeps it.
            let state = self.shared.lock();
            if !self.shared.switches.on_for(snap.agent().program()) {
                continue;
            }
            let kept = Arc::clone(
                self.shared
                    .kept
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .entry(sid.clone())
                    .or_default(),
            );
            let (ours, moved) = with_kept_of(&kept, |k| match &k.snapshot {
                None => {
                    k.snapshot = Some(snap.clone());
                    (true, publish_snapshot(&sid, Some(&snap)))
                }
                Some(read) => (read.pid == snap.pid && read.start == snap.start, false),
            });
            tell_published(&sid, moved, &self.hooks);
            if ours && let Some(pid) = landing_watched(&sid, &kept, &self.hooks, None) {
                aterm_log::info!(
                    "harness @{sid}: the agent the previous aterm supervised here (pid {pid}) is \
                     watched until a pass names it"
                );
            }
            drop(state);
        }
    }

    /// HOLD OFF `sids` — sessions a seamless update handed this instance
    /// whose claim the outgoing process could not vouch for
    /// (`SessionRecord::claim_grace`) — for `grace` ([`ADOPTED_CLAIM_GRACE`]
    /// in production) before a worker claims any of them, so an external
    /// supervisor that held one in the old instance claims it again first
    /// (the round-four plan, item 9). Their workers WATCH meanwhile (round
    /// eight, #501; [`WorkerJob::watch_until`]). The grace runs from the host
    /// thread's first look while the host is not suspended: on a successor,
    /// the Commit's [`Self::resume`]. Every other session is supervised as
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
    /// ([`Self::defer_adopted_claims`]) — supervised by nobody here yet (their
    /// worker only watches, [`WorkerJob::watch_until`]: no loop answers a
    /// box), so the menu bar raises their agent's own boxes as it would for a
    /// session this host does not supervise.
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
    ///
    /// THE RESTORED QUEUE CLOSES FIRST ([`RestoredQueue::closed`]; round
    /// fifteen, the review of round fourteen): whoever reads `shutting_down`
    /// — the host thread, which then stops every worker, one of which may
    /// take up a relaunch after a rollback ([`take_up_after_rollback`]) —
    /// finds it closed already. Set the other way round, a worker the
    /// shutdown stopped could queue a relaunch between the two, and start the
    /// restored worker to type into tabs about to close.
    pub(crate) fn shutdown_and_join(&self) {
        {
            let mut queue = self
                .shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            queue.closed = true;
            for attempt in &queue.attempts {
                attempt.revoked.store(true, Ordering::SeqCst);
            }
        }
        self.shared.restored_wake.notify_all();
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

/// [`HostHandle::relaunches_restored`].
fn relaunches_restored(shared: &Shared) -> bool {
    shared.desk.kept() && shared.switches.relaunch()
}

/// Whether the restored queue relaunches `agent` ([`queue_restored`],
/// [`relaunch_restored_queue`]): where it relaunches restored agents at all
/// ([`relaunches_restored`]) — or, the explicit restart's act having ended
/// it ([`RestoredAgent::asked`]), where the desk keeps its agents and the
/// harness supervises: a person's restart is not refused by `[harness]
/// relaunch = false`, as the live worker's relaunch of it is not
/// ([`on_agent_left`]'s `allowed`; the review of round eighteen's first
/// cut: one carried across a Commit was dropped there, with no row).
fn restored_allowed(shared: &Shared, agent: &RestoredAgent) -> bool {
    shared.switches.on_for(agent.snap.agent().program())
        && (relaunches_restored(shared)
            || (agent.asked.is_some() && shared.desk.kept() && shared.switches.supervises()))
}

/// A delayed read may apply only while its original queue entry is still
/// admitted. An off/on edit must not give a revoked attempt a new claim.
fn restored_current(shared: &Shared, order: usize, agent: &RestoredAgent) -> bool {
    restored_allowed(shared, agent)
        && shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attempts
            .iter()
            .any(|attempt| attempt.order == order && !attempt.revoked.load(Ordering::SeqCst))
}

/// AN OFF SESSION'S RESTORED AGENT IS NOT RELAUNCHED ([`OffSessions`]): its
/// tab's session is Off — its own word, carried with the tab, or the table's
/// default — so the harness keeps its hands off it, the relaunch after aterm
/// ended included. Asked before each step, on the queue's own thread
/// ([`relaunch_restored_queue`]) — a cold restore may seed the tab's word
/// after the queue took it — and not as it is queued ([`queue_restored`]):
/// its drop clears the word a relaunch raised on the tab ("still trying", a
/// predecessor's carried here) over the instance's own socket, which the
/// queue's thread may call and the window's thread that queues may not (the
/// review of 345c41647). Said in the log, never in the restored agents' row:
/// the person chose it.
fn restored_off(shared: &Shared, hooks: &Hooks, agent: &RestoredAgent) -> bool {
    let off = OffSessions::read(hooks, shared.switches.autopilot()).contains(&agent.sid);
    if off {
        aterm_log::info!(
            "harness @{}: autopilot is off {}; the relaunch after aterm ended is not made",
            agent.sid,
            agent.place
        );
    }
    off
}

/// [`HostHandle::relaunch_restored`]: `restored` queued on the host's
/// restored queue, its one worker started if it does not run yet — never
/// once the host shuts down: a stopped worker that decides late
/// ([`take_up_after_rollback`]) would start that worker during the
/// teardown, to type into tabs about to close (round twelve). Asked under
/// the queue's lock, in the hold that queues and starts
/// ([`RestoredQueue::closed`], round thirteen). REFUSED SO, the agents
/// come back (`Err`), for a caller that owes one to keep it owed (round
/// sixteen); `Ok` for every other end, nothing queued where the host
/// relaunches no restored agent.
fn queue_restored(
    shared: &Arc<Shared>,
    hooks: &Hooks,
    restored: Vec<RestoredAgent>,
) -> Result<(), Vec<RestoredAgent>> {
    queue_restored_guarded(shared, hooks, restored, None)
}

fn queue_restored_guarded(
    shared: &Arc<Shared>,
    hooks: &Hooks,
    mut restored: Vec<RestoredAgent>,
    revoked: Option<&AtomicBool>,
) -> Result<(), Vec<RestoredAgent>> {
    // An Off session's agent is queued too: its queue's thread drops it,
    // clearing its tab's word ([`restored_off`]).
    restored.retain(|agent| restored_allowed(shared, agent));
    if restored.is_empty() {
        return Ok(());
    }
    let now = Instant::now();
    let start = {
        let mut queue = shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if revoked.is_some_and(|revoked| revoked.load(Ordering::SeqCst)) {
            return Ok(());
        }
        if queue.closed {
            return Err(restored);
        }
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
                        revoked: Arc::default(),
                        order: first + at,
                        due: now,
                        pause: RESTORED_FIRST_PAUSE,
                        tried: 0,
                        resume_wait_since: None,
                        acting: false,
                        agent_seen: false,
                        carried: false,
                        landed: false,
                    }),
            );
        !std::mem::replace(&mut queue.running, true)
    };
    shared.restored_wake.notify_all();
    if !start {
        return Ok(());
    }
    let queue_hooks = hooks.clone();
    let queue_shared = Arc::clone(shared);
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}restored"))
        .spawn(move || {
            crate::qos::set_self(crate::qos::Role::Background);
            relaunch_restored_queue(&queue_shared, &queue_hooks);
        });
    if let Err(e) = spawned {
        aterm_log::warn!("harness: the restored tabs' agents could not be relaunched: {e}");
        // Nothing will step the queue: empty it, so the automatic update
        // it would hold is not held for nothing.
        let mut queue = shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.attempts.clear();
        queue.running = false;
    }
    Ok(())
}

/// THE RESTORED AGENTS' ONE WORKER ([`HostHandle::relaunch_restored`]), over
/// the host's queue ([`RestoredQueue`]) while `[harness] relaunch` allows it:
/// the earliest due attempt first — every first attempt before any retry —
/// each step run with the queue's lock released, its attempt kept in the
/// queue while it runs. A handoff's pause stops it at the top of the loop
/// (what it missed so far is said then: a Commit ends this process and its
/// row with it) until a rollback resumes it. An attempt whose tab an agent
/// ran in since it was queued is over, taken off without a step
/// ([`restored_overtaken`], `runs` asking the tab). The host's shutdown
/// ends it at its next look, parked or not, nothing more stepped and nothing
/// said, what it still owes kept ([`RestoredQueue::closed`]).
///
/// A COLD agent's step whose leaf named no login and no home of its own
/// ([`RestoredAgent::unread`]) carries the machine's proven active account
/// where the owner listed two or more (`cold`, [`Acts::account_cold`];
/// [`relaunch_cold`]) — asked at every step, off every lock, and never under
/// `[harness] accounts = "off"`. One whose leaf named them carries those,
/// on its snapshot, as a live relaunch does.
///
/// AN ASKED EXIT IS THE ACT'S HERE TOO (round eighteen; ruling 20's (iv)):
/// one the explicit restart ended ([`RestoredAgent::asked`]) is carried
/// through a hand its line waits on and asked the claim at every look
/// ([`restored_carried`]) — no step while a hand stands, nothing counted —
/// and relaunched as the act's ([`Acts::relaunch`] with its cause), as the
/// live worker relaunches it. A STEP THAT LANDS reads its agent and arms
/// its watch before the attempt is taken off ([`restored_landing_look`]);
/// one no read reaches yet keeps its attempt, the holder of its exit, until
/// a read, a pass naming it, or its end before any read — then stepped
/// again ([`restored_landing_ends`]; the review of round eighteen's first
/// cut).
fn relaunch_restored_queue(shared: &Arc<Shared>, hooks: &Hooks) {
    let (act, cold, badge, runs) = (
        hooks.acts.relaunch_restored.as_ref(),
        hooks.acts.account_cold.as_ref(),
        hooks.badge.as_ref(),
        hooks.still_wanted.as_ref(),
    );
    let switches = &shared.switches;
    let mut queue = shared
        .restored
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    loop {
        if queue.closed {
            queue.missed.clear();
            queue.running = false;
            return;
        }
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
            #[cfg(test)]
            {
                queue.parked_looks += 1;
            }
            queue = shared
                .restored_wake
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        // An explicit restart bypasses the relaunch limit, but never the
        // switch for its agent.
        queue
            .attempts
            .retain(|attempt| restored_allowed(shared, &attempt.agent));
        if queue.attempts.is_empty() {
            // Done.
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
        let revoked = Arc::clone(&attempt.revoked);
        let (typed, seen, carried) = (attempt.typed(), attempt.agent_seen, attempt.carried);
        let landed = attempt.landed;
        drop(queue);
        // ITS STEP LANDED AN AGENT NO READ HAS REACHED (round eighteen, the
        // review of its first cut): looked at again, no step, nothing
        // counted — read now, named by a pass, still unread, or gone.
        if landed {
            let look = restored_landing_look(shared, hooks, &agent.sid, seen, &revoked);
            queue = shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if queue.closed {
                continue;
            }
            let Some(at) = queue.attempts.iter().position(|a| a.order == order) else {
                continue;
            };
            let mut attempt = queue.attempts.remove(at).expect("the attempt was found");
            attempt.acting = false;
            match restored_landing_ends(attempt, look, hooks.pause.as_ref()) {
                LandingEnd::Keep(attempt) => queue.attempts.push_back(attempt),
                LandingEnd::Over(attempt, last, step) => {
                    drop(queue);
                    let missed = restored_over(badge, attempt, last, step);
                    queue = shared
                        .restored
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    queue.missed.extend(missed);
                }
            }
            continue;
        }
        // AUTOPILOT OFF in the tab since it was queued: over, no step, no
        // row — and the word a relaunch raised on the tab ("still trying",
        // a predecessor's carried here) is cleared, as a stop clears an Off
        // session's badges ([`cleanup_request`]): no worker of its own will
        // (the review of 345c41647). Asked FIRST, before whether an agent ran
        // there: that branch leaves the attention to the agent's worker,
        // which an Off session never gets.
        if restored_off(shared, hooks, &agent) {
            queue = shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if queue.closed {
                continue;
            }
            if let Some(at) = queue.attempts.iter().position(|a| a.order == order) {
                queue.attempts.remove(at);
            }
            drop(queue);
            badge(&agent.sid, None);
            queue = shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        // AN AGENT RAN IN THE TAB SINCE (rounds eleven and twelve): nothing
        // of this relaunch typed, it is over — the tab's agent is not typed
        // at, and no row says one did not come back. Asked where the step
        // would run, the attempt held as acting meanwhile. The tab's
        // attention is left alone: its owner is that agent's own worker now
        // (a word of this process's relaunch is cleared as that worker
        // starts), and a step that came later — up to a long pause on —
        // unset the escalation it had raised.
        if restored_overtaken(typed, seen, runs, &agent.sid) {
            aterm_log::info!(
                "harness @{}: an agent ran {} since; the relaunch after aterm ended is over",
                agent.sid,
                agent.place
            );
            queue = shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if queue.closed {
                continue;
            }
            if let Some(at) = queue.attempts.iter().position(|a| a.order == order) {
                queue.attempts.remove(at);
            }
            continue;
        }
        if let Some(look) = restored_carried(hooks, &agent, carried) {
            queue = shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if queue.closed {
                continue;
            }
            if let Some(at) = queue.attempts.iter().position(|a| a.order == order) {
                match look {
                    // Over, as a step that ends it is: a word a relaunch on
                    // exit raised on the tab is no longer true, and no row
                    // says it did not come back (it is that supervisor's).
                    Carried::Claimed => {
                        queue.attempts.remove(at);
                        drop(queue);
                        badge(&agent.sid, None);
                        queue = shared
                            .restored
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    // Looked at again uncounted, its pause its own.
                    Carried::Hand => {
                        let attempt = &mut queue.attempts[at];
                        attempt.acting = false;
                        attempt.carried = true;
                        attempt.due = Instant::now() + (hooks.pause)(relaunch::BUSY);
                    }
                }
            }
            continue;
        }
        let login = (agent.unread() && switches.accounts() != accounts::Mode::Off)
            .then(|| cold(&agent.snap))
            .flatten();
        let grace = shared.lock().cfg.human_grace_s;
        if !restored_current(shared, order, &agent) {
            queue = shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if queue.closed {
                continue;
            }
            queue.attempts.retain(|attempt| attempt.order != order);
            continue;
        }
        let step = relaunch_cold(&agent.snap, login, &|snap| match agent.asked {
            // The act's relaunch, as the live worker makes it once the hand
            // lifts: on its cause, the graceful-exit rule not applying.
            Some(cause) => (hooks.acts.relaunch)(
                &agent.sid,
                grace,
                snap,
                &ExitRecord::Unread,
                switches.upgrade(),
                Some(cause),
            ),
            None => act(&agent.sid, snap, switches.upgrade()),
        });
        let last = relaunch::outcome(&step);
        if !restored_current(shared, order, &agent) {
            queue = shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if queue.closed {
                continue;
            }
            queue.attempts.retain(|attempt| attempt.order != order);
            continue;
        }
        // THE AGENT IT LANDED IS READ AT ONCE, and watched until a pass names
        // it (round eighteen), armed before the attempt is taken off the
        // queue — or, no read reaching it, kept on the queue for it (the
        // review of round eighteen's first cut, [`RestoredAttempt::landed`]).
        let landing = (last == Outcome::Relaunched)
            .then(|| restored_landing_look(shared, hooks, &agent.sid, false, &revoked));
        queue = shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if queue.closed {
            continue;
        }
        let Some(at) = queue.attempts.iter().position(|a| a.order == order) else {
            continue;
        };
        let mut attempt = queue.attempts.remove(at).expect("the attempt was found");
        attempt.acting = false;
        attempt.tried += 1;
        if let Some(look) = landing {
            match restored_landing_ends(attempt, look, hooks.pause.as_ref()) {
                LandingEnd::Keep(attempt) => queue.attempts.push_back(attempt),
                LandingEnd::Over(attempt, last, step) => {
                    drop(queue);
                    let missed = restored_over(badge, attempt, last, step);
                    queue = shared
                        .restored
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    queue.missed.extend(missed);
                }
            }
            continue;
        }
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
        drop(queue);
        let missed = restored_over(badge, attempt, last, &step);
        queue = shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.missed.extend(missed);
    }
}

/// A RESTORED ATTEMPT IS OVER, `last` its last step's outcome (`step` its
/// word): the relaunch is over, landed or not, so a word a relaunch on exit
/// raised on the tab before a handoff carried it here ("still trying",
/// round six F13) is no longer true — said without the queue's lock — and
/// one that did not land is said missed where its agent resumes a
/// conversation ([`say_restored_missed`]): the row it returns, which its
/// caller adds under the queue's lock, taken again after this. Called
/// without that lock: the word is said outside it, and no guard goes in or
/// out, so every acquisition of the queue's lock is its caller's own.
fn restored_over(
    badge: &BadgeFn,
    attempt: RestoredAttempt,
    last: Outcome,
    step: &str,
) -> Option<(usize, String, Outcome)> {
    let RestoredAgent {
        sid, snap, place, ..
    } = attempt.agent;
    badge(&sid, None);
    if last != Outcome::Relaunched
        && crate::restore::agent_resumes(snap.session.as_deref(), &snap.argv)
    {
        aterm_log::warn!(
            "harness @{sid}: the agent aterm hosted {place} was not relaunched after aterm \
             ended (last step: {step})"
        );
        return Some((attempt.order, place, last));
    }
    None
}

/// What [`restored_landing_ends`] decided for a landed attempt.
enum LandingEnd {
    /// Kept on the queue, looked at again at its `due`.
    Keep(RestoredAttempt),
    /// Over: its last step's outcome and word ([`restored_over`]).
    Over(RestoredAttempt, Outcome, &'static str),
}

/// What a carried look at an asked exit's tab found ([`restored_carried`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carried {
    /// Another supervisor claims the session: the end is that supervisor's.
    Claimed,
    /// A hand the relaunch line waits on stands: looked at again, uncounted.
    Hand,
}

/// A RESTORED ASKED EXIT'S LOOK BEFORE ITS STEP (round eighteen; ruling
/// 20's (iv), NOT FIXED until now): `agent`'s relaunch is the explicit
/// restart's act's ([`RestoredAgent::asked`]) — carried across a seamless
/// update's Commit, or left stranded by a failed one, by a worker that
/// carried it through a hand ([`relaunch::WAIT_HAND_LIFTS`]) — and the tab is
/// looked at as the live worker looks at it at every look of its carry
/// ([`on_agent_left`]): ANOTHER SUPERVISOR'S CLAIM makes the end that
/// supervisor's ([`Carried::Claimed`]; ruling 17, the review of ruling 20 —
/// the relaunch line does not wait on a claim), and A HAND THE RELAUNCH
/// LINE WAITS ON ([`relaunch::hand_on_tab`]) is waited out with no step
/// ([`Carried::Hand`]), looked at again after [`relaunch::BUSY`], uncounted
/// and unsaid save one log line (`said`: it was). Stepped as aterm's own end
/// before, the line's own look answered `wait:held` — a COUNTED retry — and
/// after [`RESTORED_TRIES`] (about 91 s) the tab was said not to have come
/// back, the agent a person asked to restart left at its shell. `None`: no
/// carried look stands in the way (or no cause is carried): the step is
/// made.
fn restored_carried(hooks: &Hooks, agent: &RestoredAgent, said: bool) -> Option<Carried> {
    agent.asked?;
    let sid = agent.sid.as_str();
    let status = (hooks.acts.status)(sid);
    if let Some(other) = status.as_deref().and_then(claimed_by_another) {
        aterm_log::info!(
            "harness @{sid}: {other} claims the session; the end the explicit restart made {}, \
             carried across aterm's update, is that supervisor's",
            agent.place
        );
        return Some(Carried::Claimed);
    }
    if !status.as_deref().is_some_and(relaunch::hand_on_tab) {
        return None;
    }
    if !said {
        aterm_log::info!(
            "harness @{sid}: the agent the explicit restart ended {} is carried through another \
             hand on its tab (a hold, a lease, a turn) across aterm's update; its relaunch is \
             typed once that hand lifts",
            agent.place
        );
    }
    Some(Carried::Hand)
}

/// What a look at the agent a restored step landed found
/// ([`restored_landing_look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LandingLook {
    /// Read, its exit watched until a pass names it — or named by a pass
    /// already: its own worker's.
    Read,
    /// It holds its tab, and no read reaches it yet.
    Unread,
    /// Gone before any read reached it.
    Gone,
}

/// The step word a restored attempt whose landed agents kept exiting before
/// any read reached them ends on ([`restored_landing_ends`]).
const LANDED_GONE: &str = "exited-before-read";

/// THE AGENT A RESTORED STEP LANDED IN TAB `sid`, LOOKED AT (round
/// eighteen; the residual round seventeen stated: the cold restore's queue
/// landed agents with nothing watching them, and one that crashed before a
/// pass named it — the moment before the next pass, or a host panic's whole
/// back-off, through which the queue's own thread runs on — was read by
/// nobody). Read into the session's [`Kept`] ([`refresh_snapshot`]) and
/// watched until a pass names it ([`landing_watched`]), as a worker's
/// landing is: its exit read as it is seen and handed by the host's next
/// pass. `seen`: a pass named an agent in the tab already
/// ([`RestoredAttempt::agent_seen`]), its worker's to read — nothing is
/// armed. Else it holds its tab unread ([`Hooks::still_wanted`], no group of
/// its own to ask about: a read the control socket refused, a roster that
/// has not named the tab's group yet) or is gone before any read.
fn restored_landing_look(
    shared: &Shared,
    hooks: &Hooks,
    sid: &str,
    seen: bool,
    revoked: &AtomicBool,
) -> LandingLook {
    if seen || revoked.load(Ordering::SeqCst) {
        return LandingLook::Read;
    }
    let kept = Arc::clone(
        shared
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(sid.to_string())
            .or_default(),
    );
    if refresh_snapshot_guarded(sid, &kept, hooks, Some(revoked)) {
        if let Some(pid) = landing_watched(sid, &kept, hooks, Some(revoked)) {
            aterm_log::info!(
                "harness @{sid}: the relaunch after aterm ended landed (pid {pid}); its agent's \
                 exit is watched until a pass names it"
            );
        }
        return LandingLook::Read;
    }
    if revoked.load(Ordering::SeqCst) {
        LandingLook::Read
    } else if (hooks.still_wanted)(sid, 0) {
        LandingLook::Unread
    } else {
        LandingLook::Gone
    }
}

/// THE RESTORED ATTEMPT WHOSE STEP LANDED, after `look` at its agent
/// ([`restored_landing_look`]; round eighteen, the review of its first
/// cut). Read — or a pass named an agent in the tab since
/// ([`RestoredAttempt::agent_seen`]): its own worker's — it is over,
/// landed. HOLDING ITS TAB UNREAD, it is KEPT ([`RestoredAttempt::landed`]),
/// looked at again after [`LANDED_LOOK`] (through `pause`, the host's
/// seam), uncounted. GONE BEFORE ANY READ after a look found it holding its
/// tab, the exit is its own relaunch's again, as a worker's landing's is
/// ([`relaunch_until`]'s LANDED AND GONE AGAIN): stepped again on the
/// attempt's back-off while its budget lasts ([`RESTORED_TRIES`]) — a fresh
/// step, its line not typed, so an agent someone starts in the tab
/// meanwhile overtakes it ([`restored_overtaken`]) — and past the budget
/// over, said missed ([`LANDED_GONE`]). The first cut ended the attempt at
/// every landing, and the thread that read the agent again gave up once it
/// no longer held its tab: a crash before any read was held by nobody — no
/// watch, no mark, no worker, no attempt — never relaunched and never said.
///
/// THE RESIDUAL, STATED: the look right after the step that neither reads
/// the agent nor finds it holding its tab leaves it to the roster, as the
/// first cut did — the step's `adopted` found it registered and holding the
/// tab through the tab's own job (`relaunch::await_held`), whatever the
/// publication names yet, so that look cannot tell an agent gone in the
/// moment since from one no publication names yet, and a step made again
/// on the second would be typed beside a live agent. One gone there is
/// read by nobody. Decided without the queue's lock: its caller keeps the
/// attempt or ends it ([`LandingEnd`]).
fn restored_landing_ends(
    mut attempt: RestoredAttempt,
    look: LandingLook,
    pause: &PauseFn,
) -> LandingEnd {
    let now = Instant::now();
    match look {
        _ if attempt.agent_seen => LandingEnd::Over(attempt, Outcome::Relaunched, "landed"),
        LandingLook::Read => LandingEnd::Over(attempt, Outcome::Relaunched, "landed"),
        LandingLook::Unread => {
            if !attempt.landed {
                aterm_log::info!(
                    "harness @{}: the relaunch after aterm ended landed {}, but no read reaches \
                     its agent yet; it is read again while it holds its tab",
                    attempt.agent.sid,
                    attempt.agent.place
                );
            }
            attempt.landed = true;
            attempt.due = now + pause(LANDED_LOOK);
            LandingEnd::Keep(attempt)
        }
        LandingLook::Gone if !attempt.landed => {
            aterm_log::info!(
                "harness @{}: the relaunch after aterm ended landed {}; no read reaches its agent \
                 and nothing names it in its tab yet: it is left to the roster",
                attempt.agent.sid,
                attempt.agent.place
            );
            LandingEnd::Over(attempt, Outcome::Relaunched, "landed")
        }
        LandingLook::Gone if attempt.tried < RESTORED_TRIES => {
            aterm_log::info!(
                "harness @{}: the agent the relaunch after aterm ended landed {} exited before \
                 any read reached it; its relaunch is made again",
                attempt.agent.sid,
                attempt.agent.place
            );
            attempt.landed = false;
            attempt.resume_wait_since = None;
            attempt.due = now + attempt.pause;
            attempt.pause = (attempt.pause * 2).min(RESTORED_MAX_PAUSE);
            LandingEnd::Keep(attempt)
        }
        LandingLook::Gone => LandingEnd::Over(
            attempt,
            Outcome::Cannot(LANDED_GONE.to_string()),
            LANDED_GONE,
        ),
    }
}

/// A LANDING'S AGENT, READ into session `sid`'s [`Kept`], is watched until a
/// pass names it: its exit watch hands that exit itself once read
/// ([`ExitWatched::hands`]) — the agent the snapshot names, a Codex or a
/// Claude Code, in the group its job leads. Its pid, where one was watched.
fn landing_watched(
    sid: &str,
    kept: &Arc<Mutex<Kept>>,
    hooks: &Hooks,
    revoked: Option<&AtomicBool>,
) -> Option<u32> {
    let landed = with_kept_of(kept, |k| {
        k.snapshot.as_ref().map(|snap| {
            (
                Unwatched {
                    agent: snap.agent().program(),
                    group: i32::try_from(snap.pid).unwrap_or(0),
                },
                snap.pid,
            )
        })
    });
    let (hands, pid) = landed?;
    watch_exit_guarded(sid, kept, hooks, Some(hands), revoked);
    Some(pid)
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
///
/// AN OFF SESSION IS NOT CARRIED ([`OffSessions`], `words` its reader): the
/// harness keeps its hands off it, a restart left in flight there included —
/// dropped before its step, said in the log alone.
#[cfg(any(unix, test))]
fn carry_in_flight_after_handoff(
    shared: &Shared,
    carry: &CarryInFlightFn,
    words: &WordsFn,
    pause: &PauseFn,
    sessions: &[(String, String)],
    grace: u32,
) {
    // `(order, sid, place, due, pause, last)`: `last` is the last step's word
    // and outcome while it waited, `None` before a step or while the tab was
    // its worker's.
    type Owed<'a> = (
        usize,
        &'a str,
        &'a str,
        Instant,
        Duration,
        Option<(String, Outcome)>,
    );
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
        let off = OffSessions {
            words: words(),
            default: shared.switches.autopilot(),
        };
        if off.contains(sid) {
            aterm_log::info!(
                "harness @{sid}: autopilot is off {place}; the restart left in flight there is \
                 not carried"
            );
            owed.remove(at);
            continue;
        }
        let Some(step) = carry(sid, grace) else {
            owed.remove(at);
            continue;
        };
        let last = relaunch::outcome(&step);
        let supervised = step == CARRY_SUPERVISED;
        if supervised || restored_again(&step, &last) {
            owed[at].3 = Instant::now() + pause(wait);
            owed[at].4 = (wait * 2).min(RESTORED_MAX_PAUSE);
            owed[at].5 = (!supervised).then_some((step, last));
            continue;
        }
        owed.remove(at);
        if matches!(last, Outcome::Cannot(_)) {
            aterm_log::warn!(
                "harness @{sid}: the restart the previous aterm left in flight {place} could \
                 not be carried on (last step: {step})"
            );
            missed.push((order, place.to_string(), last));
        }
    }
    for (order, sid, place, _, _, last) in owed {
        let Some((step, last)) = last else {
            continue;
        };
        aterm_log::warn!(
            "harness @{sid}: the restart the previous aterm left in flight {place} was not \
             carried on in time (last step: {step})"
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
/// adopted-claim grace ([`ADOPTED_CLAIM_GRACE`]) held that worker off exactly
/// while the SIGTERMed agent exited, and none started after it — and the
/// worker that now watches the grace stands down from that exit when another
/// supervisor's claim makes it that one's.
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

/// One worker's thread, running `main` ([`worker_main`], or [`exit_main`]
/// for an exit no worker was handed).
fn spawn_worker(
    sid: &str,
    job: WorkerJob,
    done: Arc<AtomicBool>,
    hooks: &Hooks,
    main: fn(WorkerJob, &Hooks) -> WorkerExit,
) -> Option<JoinHandle<WorkerExit>> {
    let hooks = hooks.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}{sid}"))
        .spawn(move || {
            let exit = main(job, &hooks);
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
    /// THE LOOK EACH TAB'S RECORD IS STILL OWED after its publication moved
    /// (LC-1, [`follow_entries`]): when the host follows it once more.
    settle: HashMap<String, Instant>,
    /// The earliest instant the last pass named for anything but a
    /// [`Self::settle`] — a worker's look, the keep's, the owner's view's,
    /// the watches', the disk watch's: a timer that ends the host's wait
    /// before it is a settle's alone ([`settle_alone`]).
    timer_at: Option<Instant>,
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
    /// that cannot name it finds it still holding its tab ([`keep_holding`])
    /// — and through a Commit's hold, which reads no roster ([`host_pass`]).
    holding: HashMap<String, Held>,
    /// THE WATCHES the host fires off any point ([`watch_upgrades`]).
    watches: Watches,
    /// THE AGENT SESSIONS A PASS READ OFF ([`OffSessions`]) since a worker
    /// last started for them: the next worker one gets starts as its Off
    /// lifted ([`start_worker`], [`WorkerIdle::lifted_off`]) — the person
    /// drove it meanwhile, so the acts its ledger shows carry no task for
    /// that worker's loop. Marked at every pass that reads the roster, so a
    /// stop still being reaped as the word turns back keeps its mark; a
    /// session the roster names no more is forgotten. Every session the
    /// roster names is marked as supervision comes back after the master
    /// switch stood off ([`State::unsupervised`]).
    was_off: std::collections::HashSet<String>,
}

impl HostState {
    /// Named for what it is, not `new`: it takes `shared`'s lock, and the
    /// lock census resolves a call by its name — every `AtomicBool::new` in
    /// `start_workers`, under that same lock, read as re-entering it.
    fn from_shared(shared: &Shared) -> Self {
        Self {
            workers: Workers::default(),
            followed: HashMap::new(),
            settle: HashMap::new(),
            timer_at: None,
            full_epoch_seen: FULL_FOLLOW_EPOCH.load(Ordering::SeqCst),
            phase_epoch_seen: PHASE_FOLLOW_EPOCH.load(Ordering::SeqCst),
            held: HashMap::new(),
            forgiven_gen: shared.lock().cfg_gen,
            view: ViewLook::default(),
            disk_at: Instant::now() + DISK_FIRST_LOOK,
            holding: HashMap::new(),
            watches: Watches::default(),
            was_off: std::collections::HashSet::new(),
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
/// wrote. A write later still is read at the tab's next move or step. The
/// same writer names the conversation an in-app `/clear` or `/resume` moves
/// the agent to, so the record is looked at once more this long after a
/// tab's publication moved too ([`follow_entries`], LC-1), and a move of a
/// screen alone wakes the host at most this often
/// (`SessionTimeline::publish_agent`).
pub(crate) const STATUS_AFTER_SCREEN: Duration = Duration::from_secs(2);

/// What one look at the owner's view was taken over ([`ViewLook::shown`]):
/// the workers' acts and the activation notices seen, the supervised tabs,
/// and the tabs set aside behind another supervisor's claim, by Off and by
/// their agent's own switch ([`ViewLook::claimed`], [`ViewLook::off`],
/// [`ViewLook::agent_off`]).
type ViewSeen = (u64, u64, Vec<String>, Vec<String>, Vec<String>, Vec<String>);

/// What the host last looked at the owner's view of the upgrades over
/// ([`look_at_upgrades`]): the workers' acts and the activation notices seen,
/// the supervised tabs, and when the view said it changes next. `None`: it
/// is not shown (never looked at, or stood down).
#[derive(Default)]
struct ViewLook {
    shown: Option<ViewSeen>,
    next: Option<Instant>,
    /// The same instant as the view NAMED it, on the wall clock (unix seconds,
    /// [`ViewNext::at`]): the look is due once either has come. `next` is
    /// monotonic, which on macOS does not advance while the Mac sleeps, so an
    /// instant slept through came due only after as much awake time again
    /// ([`bell_wait`]); the wall clock has passed it at the wake.
    next_s: Option<u64>,
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
    /// THE TABS PARKED BEHIND ANOTHER SUPERVISOR'S CLAIM (the review of
    /// 2026-09-30, of C10): wanted, their worker ended held
    /// ([`HostState::held`]) and none started since, sorted — set by the
    /// host before each pass's look ([`host_pass`]). No step and no watch of
    /// this host's reaches them while the claim stands, so the view reads a
    /// move owed there as held by it, never as a record aterm stopped
    /// checking; a change of them is a look.
    claimed: Vec<String>,
    /// THE TABS WHOSE AUTOPILOT IS OFF (the review of a295792e4): wanted and
    /// read Off by the pass ([`OffSessions`]), sorted — set by the host before
    /// each pass's look ([`host_pass`]). Off stops their worker and no watch
    /// is fired for them ([`watch_upgrades`]), so no step or watch of this
    /// host's reaches them until the word turns, and the view reads a move
    /// owed there as held by the owner's Off, never as a record aterm stopped
    /// checking; a change of them is a look.
    off: Vec<String>,
    /// THE TABS WHOSE AGENT ITS OWN SWITCH HOLDS OFF (`[harness.<agent>]
    /// enabled = false`): named by the roster, admitted no worker
    /// (`switched_off` in [`host_pass`]), sorted. No step or watch of this
    /// host's reaches them until the agent is switched on again, so the view
    /// reads a move owed there as held by that switch, never as a record
    /// aterm stopped checking; a change of them is a look.
    agent_off: Vec<String>,
}

/// The host thread's passes over `host` until shutdown. `host` is the
/// caller's, so a pass that panics leaves what the host held to the run
/// that starts again ([`host_restarted`]).
fn host_loop(shared: &Arc<Shared>, host: &mut HostState, hooks: &Hooks) {
    let mut seen = bell_now();
    let mut timed_out = true;
    while let ControlFlow::Continue(until) = host_pass(shared, host, timed_out, hooks) {
        (seen, timed_out) = bell_wait(seen, until, host.view.next_s);
    }
}

/// Reset per-run discovery and view state in place after a host panic,
/// retaining workers and the roster snapshots in [`HostState::holding`].
/// Reconcile retained workers with current policy before republishing them
/// for [`HostHandle::suspend`], then arm exit watches during recovery.
///
/// The caller retains ownership of `host` throughout recovery, so a panic
/// here does not drop its workers or their pending relaunch state.
fn host_restarted(shared: &Shared, host: &mut HostState, hooks: &Hooks) {
    let HostState {
        followed,
        settle,
        timer_at,
        full_epoch_seen,
        phase_epoch_seen,
        held,
        forgiven_gen,
        view,
        disk_at,
        watches,
        ..
    } = HostState::from_shared(shared);
    host.followed = followed;
    host.settle = settle;
    host.timer_at = timer_at;
    host.full_epoch_seen = full_epoch_seen;
    host.phase_epoch_seen = phase_epoch_seen;
    host.held = held;
    host.forgiven_gen = forgiven_gen;
    host.view = view;
    host.disk_at = disk_at;
    host.watches = watches;
    restart_under_policy(shared, host);
    #[cfg(test)]
    assert!(
        !shared.restart_panics.swap(false, Ordering::SeqCst),
        "the host's restart panics (a test's)"
    );
    publish_workers(&mut shared.lock(), &host.workers);
    for (sid, w) in &host.workers.0 {
        if w.left.is(Word::Watching) && !w.stop.load(Ordering::SeqCst) {
            watch_exit(sid, &w.kept, hooks, None);
        }
    }
}

/// Pause requested before retrying a panicked [`restart_host`], preventing
/// a tight recovery loop. Shutdown interrupts the pause.
const RESTART_AGAIN: Duration = Duration::from_secs(1);

/// Recover after the run's `attempt`-th consecutive panic and wait for its
/// backoff, interrupted by shutdown or news since `seen` ([`State::news`]).
/// Reapply current policy before resuming normal passes.
///
/// Returns `false` after bounded worker shutdown and exit-watch cleanup.
/// The caller catches recovery panics while retaining ownership of `host`.
fn restart_host(
    shared: &Shared,
    host: &mut HostState,
    hooks: &Hooks,
    attempt: usize,
    seen: (u64, u64, u64),
) -> bool {
    host_restarted(shared, host, hooks);
    wait_for(Instant::now() + (hooks.backoff)(attempt), || {
        let state = shared.lock();
        state.shutting_down || state.news() != seen
    });
    if shared.lock().shutting_down {
        shutdown_workers(std::mem::take(&mut host.workers));
        follow_at_shutdown(shared, hooks);
        end_exit_watches(shared, EVERY_WATCH, |_| true);
        return false;
    }
    restart_under_policy(shared, host);
    true
}

/// Apply current policy to workers retained across a host restart.
/// Stop globally or individually disabled agents; hand over workers from
/// an older config generation so successors use the new policy.
/// A global switch-off also clears retained roster state and exit watches.
/// Called before and after recovery backoff to enforce policy in both phases.
fn restart_under_policy(shared: &Shared, host: &mut HostState) {
    let (on, cfg_gen) = {
        let state = shared.lock();
        (would_supervise(&state.cfg, shared.headless), state.cfg_gen)
    };
    for w in host.workers.0.values() {
        if !on || !shared.switches.on_for(w.agent) {
            if !w.left.is(Word::Stopped) {
                w.stop();
            }
        } else if w.cfg_gen != cfg_gen && !w.stop.load(Ordering::SeqCst) {
            w.hand_over();
        }
    }
    if !on {
        host.holding.clear();
        end_exit_watches(shared, EVERY_WATCH, |_| true);
    }
}

/// THE SESSIONS AUTOPILOT HAS OFF (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §2.2: "the harness keeps its hands off this session: plain Claude Code"),
/// as one read of the sessions' own words ([`Hooks::autopilot`]) under the
/// table's default (`[harness] autopilot`) says them: a session is Off when
/// its own word is `off`, or it has none and the default is — the rule of
/// [`SupervisorConfig::autopilot_for`], the one resolver. A session no word
/// names and the default leaves on is not Off; neither is any session of a
/// host that supervises nothing ([`Self::default`]: the master switch wins
/// over every word, and supervises nothing either way).
///
/// OFF IS THE HOST'S, NOT THE LOOP'S. An Off agent session STAYS in a pass's
/// `wanted` set — it is still an agent tab — and its worker is STOPPED, never
/// let go ([`visit_workers`]): letting go hands the worker its agent's exit
/// and starts the relaunch decision. No worker starts for it, and an exit no
/// worker was handed is forgotten rather than read ([`start_workers`]), and
/// so is what was read of its agent for a relaunch once the roster names it
/// no more ([`unwatched_exits`]); no keep holds it ([`keep_holding`]) and its
/// exit watch ends ([`host_pass`]); no upgrade watch is fired for it
/// ([`watch_upgrades`], which never types or signals in any case: the
/// aterm-census guard `watch-hands`), and
/// the owner's view reads a move owed there as held by the Off
/// ([`ViewLook::off`]); nothing the restored queue owes it is relaunched
/// ([`restored_off`]), nor a restart a predecessor left in flight there
/// ([`carry_in_flight_after_handoff`]). The stop releases the claim and clears
/// the badges ([`cleanup_request`]), so `status supervisor=-` follows by
/// itself, and the menu bar raises the session's boxes again
/// (`HarnessHosting::hosts`). Back to `cruise` or `land`, the pass the word's
/// ring brings starts its worker as for any agent tab — as one started as its
/// Off lifted ([`HostState::was_off`]): the person drove the session
/// meanwhile, so what the ledger shows the harness typed before carries no
/// task for it. The point it finds idle, no read having seen its turn run,
/// is backed off and acted on as any worker's first point is: a quick
/// exchange of the person's under Off gets that act, a visible one they can
/// answer or Esc, never a stall of the run it may belong to.
#[derive(Debug, Default)]
struct OffSessions {
    /// Every live session's own word.
    words: HashMap<String, Autopilot>,
    /// The table's default, for a session with no word.
    default: Autopilot,
}

impl OffSessions {
    /// The sessions' words as `hooks` reads them now, under the table's
    /// default `default`.
    fn read(hooks: &Hooks, default: Autopilot) -> Self {
        Self {
            words: (hooks.autopilot)(),
            default,
        }
    }

    /// Whether session `sid` is Off.
    fn contains(&self, sid: &str) -> bool {
        self.words.get(sid).copied().unwrap_or(self.default) == Autopilot::Off
    }
}

/// Process roster and policy changes and due work. `timed_out` means a
/// scheduled look woke the host rather than the bell.
/// Returns `Break` after bounded worker shutdown and exit-watch cleanup;
/// otherwise returns the earliest scheduled deadline, if any. Record-settle
/// deadlines are included; adopted-claim grace belongs to the watching worker
/// ([`WorkerJob::watch_until`]).
fn host_pass(
    shared: &Arc<Shared>,
    host: &mut HostState,
    timed_out: bool,
    hooks: &Hooks,
) -> ControlFlow<(), Option<Instant>> {
    // Capture wake cursors BEFORE reading the roster. A publication
    // arriving during that read remains owed on the next loop even if
    // this loop happened to see its new stamp.
    let full_now = FULL_FOLLOW_EPOCH.load(Ordering::SeqCst);
    let phase_now = PHASE_FOLLOW_EPOCH.load(Ordering::SeqCst);
    let now = Instant::now();
    // A wait a tab's settle alone ended owes that tab's record, never every
    // one ([`settle_alone`]).
    let follow_all = full_follow_due(
        timed_out && !settle_alone(host, now),
        host.full_epoch_seen,
        full_now,
        host.phase_epoch_seen,
        phase_now,
    );
    host.full_epoch_seen = full_now;
    host.phase_epoch_seen = phase_now;
    let (cfg, cfg_gen, active, suspended, shutting_down, switched_on) = {
        let mut state = shared.lock();
        let active = Shared::active(&state, shared.headless);
        // Supervision back after the master switch stood off: taken by the
        // pass that reads the roster ([`State::unsupervised`]).
        let switched_on = active && std::mem::take(&mut state.unsupervised);
        (
            state.cfg.clone(),
            state.cfg_gen,
            active,
            state.suspended,
            state.shutting_down,
            switched_on,
        )
    };
    reap(host);
    if shutting_down {
        shutdown_workers(std::mem::take(&mut host.workers));
        follow_at_shutdown(shared, hooks);
        end_exit_watches(shared, EVERY_WATCH, |_| true);
        return ControlFlow::Break(());
    }
    // The watches armed before the roster is read: a pass ends no other
    // ([`end_exit_watches`]).
    let armed_before = EXIT_WATCH_ARMINGS.load(Ordering::SeqCst);
    // Each worker's step stamp, read BEFORE the roster ([`StepsSeen`]).
    let steps = StepsSeen::read(&host.workers);
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
    // AUTOPILOT OFF ([`OffSessions`]): the sessions' own words, read once a
    // pass beside the roster, under the table's default. None is Off while
    // the host supervises nothing.
    let off = if active {
        OffSessions::read(hooks, cfg.autopilot)
    } else {
        OffSessions::default()
    };
    // A new program in the same tab is a new worker's agent. In particular,
    // a disabled replacement is a stop, never the old agent's relaunchable
    // exit, even when that old agent remains enabled. An Off session's
    // worker is stopped here too, mid-step or not, and never handed over:
    // the step's agent left the tab, and Off hands nothing to anyone
    // (`OffLetsNothingGo`). Skipped, the mid-step worker of an Off tab ran
    // on after another program took it, while Cruise had it stopped
    // ([`visit_workers`] waits out a step in flight). A disabled replacement
    // stops a worker reading its agent's exit too (handed it, or started for
    // it alone: its `stop` is set already), whose relaunch, put off while the
    // replacement holds the tab, was typed once that program exited, into
    // the tab the person had moved on in. An enabled replacement ends such
    // a read as the agent back ([`Worker::back`], [`visit_workers`]).
    for (sid, worker) in &host.workers.0 {
        let Some(agent) = wanted.get(sid).filter(|agent| **agent != worker.agent) else {
            continue;
        };
        if !worker.stop.load(Ordering::SeqCst) {
            if shared.switches.on_for(*agent) && !off.contains(sid) {
                worker.hand_over();
            } else {
                worker.stop();
            }
        } else if worker.left.is(Word::Left) && !shared.switches.on_for(*agent) {
            worker.stop();
        }
    }
    // THE SESSIONS WHOSE AGENT ITS OWN SWITCH HOLDS OFF: the person drives
    // them until it is on again, as under their own Off, so the worker the
    // switch-on starts there starts as Off lifted ([`HostState::was_off`]).
    // No worker is theirs, yet each agent runs in its tab: a fact for that
    // tab's restored relaunches all the same ([`note_agents_in_restored_tabs`]).
    let named_off: HashMap<String, Program> = wanted
        .iter()
        .filter(|(_, agent)| !shared.switches.on_for(**agent))
        .map(|(sid, agent)| (sid.clone(), *agent))
        .collect();
    let switched_off: Vec<String> = named_off.keys().cloned().collect();
    // A supervised agent the roster could not name, still holding its tab,
    // is still wanted, at the stamp it was last named with ([`keep_holding`]).
    let mut stamps = stamps;
    wanted.retain(|_, agent| shared.switches.on_for(*agent));
    stamps.retain(|sid, _| wanted.contains_key(sid));
    host.holding
        .retain(|_, held| shared.switches.on_for(held.agent));
    let hold_look = if active {
        let look = keep_holding(
            &mut host.holding,
            &host.workers,
            &steps,
            &mut wanted,
            &mut stamps,
            &off,
            hooks,
        );
        // An agent named in its tab is its worker's to watch again
        // ([`Kept::exit_watch`]; round seventeen).
        end_exit_watches(shared, armed_before, |sid| wanted.contains_key(sid));
        // An Off session's exit is nobody's: every watch of it ends, armed
        // when it may.
        end_exit_watches(shared, EVERY_WATCH, |sid| off.contains(sid));
        look
    } else {
        // A COMMIT'S HOLD FORGETS NOTHING THE ROSTER LAST NAMED (round
        // fifteen; the review of round fourteen): a pass that cannot name an
        // agent the moment the host supervises again is the same misread as
        // any other, and it is asked the same fact. Forgotten here, the first
        // pass after a failed Commit kept nothing, and the exit the Commit's
        // stop left to the host ([`Kept::unwatched`]) was handed for an agent
        // that still held its tab. Supervision switched off forgets it all,
        // and watches no exit ([`Kept::exit_watch`]): nobody's is handed.
        if !suspended {
            host.holding.clear();
            end_exit_watches(shared, EVERY_WATCH, |_| true);
        }
        None
    };
    // Every agent named is a fact for the restored relaunches of its tab.
    note_agents_in_restored_tabs(shared, &wanted, &switched_off);
    host.held.retain(|sid, _| wanted.contains_key(sid));
    age_and_forgive(shared, host, cfg_gen, &wanted);
    forget_closed(shared, &host.workers, &wanted, active, hooks);
    let settle_at = follow_workers(shared, host, &wanted, &stamps, follow_all, now, hooks);
    let next_look = visit_workers(
        shared,
        &mut host.workers,
        &steps,
        &mut host.watches.told,
        &wanted,
        &off,
        active,
        cfg_gen,
        hooks,
    );
    // The Off reading is published after its stops and BEFORE any start (the
    // review of 345c41647): a worker this pass starts is never seen beside the
    // last pass's Off reading of its session. Only sessions not Off start, so
    // the set is the one a publication after the starts would name.
    publish_off(shared, &host.workers, &wanted, &off);
    // The sessions read Off are marked for the worker that starts as Off
    // lifts ([`HostState::was_off`]); a session the roster names no more is
    // forgotten. A pass that read no roster (a Commit's hold, supervision
    // off) marks and forgets nothing. Supervision back after the master
    // switch stood off marks every session it names: each was the person's
    // meanwhile, as under its own Off ([`State::unsupervised`]) — and so
    // does an agent's own switch, for each session the roster names that
    // agent in while it stands off (`switched_off`): its worker was stopped
    // and revoked, and the one its switch-on starts carries nothing the
    // ledger shows from before.
    if active {
        host.was_off
            .retain(|sid| wanted.contains_key(sid) || switched_off.contains(sid));
        host.was_off.extend(
            wanted
                .keys()
                .filter(|sid| switched_on || off.contains(sid))
                .chain(&switched_off)
                .cloned(),
        );
    }
    if active {
        forget_left_while_switched_off(
            shared,
            &host.workers,
            &wanted,
            &named_off,
            |agent| shared.switches.on_for(agent),
            hooks,
        );
    }
    start_workers(shared, host, &wanted, &off, active, &cfg, cfg_gen, hooks);
    host.view.claimed = claimed_tabs(&host.held, &host.workers);
    host.view.off = off_tabs(&wanted, &off);
    host.view.agent_off = {
        let mut tabs = switched_off.clone();
        tabs.sort();
        tabs
    };
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
        &off,
        active && shared.switches.upgrade(),
        cfg.human_grace_s,
        hooks,
    );
    let disk_at = look_at_disk(
        &mut host.disk_at,
        Instant::now(),
        active,
        &wanted,
        &off,
        hooks,
    );
    host.timer_at = [next_look, hold_look, view_at, watch_at, disk_at]
        .into_iter()
        .flatten()
        .min();
    ControlFlow::Continue([host.timer_at, settle_at].into_iter().flatten().min())
}

/// Whether the timer that ended the host's wait was a SETTLE's alone (LC-1,
/// [`HostState::settle`]): a tab's settle has come, and no other instant the
/// last pass named ([`HostState::timer_at`]) nor the owner's view's wall-clock
/// one ([`ViewLook::next_s`], which [`bell_wait`] also ends the wait at). That
/// pass follows the settled tab's record alone ([`follow_entries`]) — never
/// every one, which a timed wait otherwise owes ([`full_follow_due`]): a
/// settle comes after a tab's screen moved, a few times a turn.
fn settle_alone(host: &HostState, now: Instant) -> bool {
    host.settle.values().any(|at| *at <= now)
        && host.timer_at.is_none_or(|at| at > now)
        && host
            .view
            .next_s
            .is_none_or(|at| at > crate::upgrade_host::now_s())
}

/// Follow retained agent records once more after the bounded worker stop/join,
/// before the quit layout reads them. A worker that joined cannot overwrite
/// this last sample with an older in-flight read. A worker left running past
/// the join bound may still race it, so the final sample remains best effort.
/// Each [`Kept`] is read under its own lock after the map's lock is released.
fn follow_at_shutdown(shared: &Shared, hooks: &Hooks) {
    let kept: Vec<(String, Arc<Mutex<Kept>>)> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(sid, kept)| (sid.clone(), Arc::clone(kept)))
        .collect();
    for (sid, kept) in kept {
        follow_snapshot(&sid, &kept, hooks);
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
///
/// AN OFF SESSION IS NOT THE WATCH'S ([`OffSessions`]; the review of
/// 345c41647): the harness keeps its hands off it, so its working directory
/// and its agent are never handed over — a look below the floor reclaims
/// build caches in the working directories it is handed — and a host whose
/// every session is Off hosts none for the watch, which then names no
/// instant.
fn look_at_disk(
    disk_at: &mut Instant,
    now: Instant,
    active: bool,
    wanted: &HashMap<String, Program>,
    off: &OffSessions,
    hooks: &Hooks,
) -> Option<Instant> {
    let mut sids: Vec<String> = wanted
        .keys()
        .filter(|sid| !off.contains(sid))
        .cloned()
        .collect();
    if !active || sids.is_empty() {
        return None;
    }
    if now >= *disk_at {
        sids.sort();
        (hooks.disk)(&sids);
        *disk_at = now + DISK_TICK;
    }
    Some(*disk_at)
}

/// The tabs parked behind another supervisor's claim ([`ViewLook::claimed`]):
/// held ([`reap`]: a worker that found the claim held ended so), and no
/// worker started for them since. Sorted.
fn claimed_tabs(held: &HashMap<String, u64>, workers: &Workers) -> Vec<String> {
    let mut tabs: Vec<String> = held
        .keys()
        .filter(|sid| !workers.0.contains_key(*sid))
        .cloned()
        .collect();
    tabs.sort();
    tabs
}

/// The agent tabs the pass read Off ([`ViewLook::off`]): wanted, and Off by
/// their own word or the table's default ([`OffSessions`]). Sorted.
fn off_tabs(wanted: &HashMap<String, Program>, off: &OffSessions) -> Vec<String> {
    let mut tabs: Vec<String> = wanted
        .keys()
        .filter(|sid| off.contains(sid))
        .cloned()
        .collect();
    tabs.sort();
    tabs
}

/// THE OWNER'S VIEW, looked at when something it shows may have moved: a
/// worker's upgrade or relaunch step and an activation notice or the owner's
/// word (`(acts, activations)`: [`UPGRADE_ACTS`], the host's count), the
/// supervised tabs, the instant the view last named (an upgrade turning
/// overdue, a word running out — come by the monotonic clock or by the wall
/// clock, whichever is first: after a sleep only the wall clock has,
/// [`ViewLook::next_s`]), or — for a tab whose row reads its agent's
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
            let _ = (hooks.upgrade_view)(false, &[], &[], &[]);
        }
        view.next = None;
        view.next_s = None;
        view.follows.clear();
        view.settle = None;
        view.watch.clear();
        return None;
    }
    let mut tabs: Vec<String> = wanted.keys().cloned().collect();
    tabs.sort();
    let now = Instant::now();
    // The view's instant on the wall clock too: after a sleep the monotonic
    // `next` is hours short of it ([`ViewLook::next_s`]).
    let wall_now = crate::upgrade_host::now_s();
    let seen = Some((
        acts,
        activations,
        tabs,
        view.claimed.clone(),
        view.off.clone(),
        view.agent_off.clone(),
    ));
    let rev = |tab: &str| stamps.get(tab).map(|s| s.rev);
    let moved = view
        .follows
        .iter()
        .any(|(tab, seen)| rev(tab) != Some(*seen));
    if view.shown != seen
        || view.next.is_some_and(|at| at <= now)
        || view.next_s.is_some_and(|at| at <= wall_now)
        || view.settle.is_some_and(|at| at <= now)
        || moved
    {
        let next = (hooks.upgrade_view)(true, &view.claimed, &view.off, &view.agent_off);
        let now_s = crate::upgrade_host::now_s();
        view.next = next
            .at
            .map(|at| now + Duration::from_secs(at.saturating_sub(now_s)));
        view.next_s = next.at;
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
/// (step/notice/resume are the loop's points' alone; the aterm-census guard
/// `watch-hands`). The instant to fire again, if one. An Off session
/// ([`OffSessions`]) is watched by nothing: what it was told is dropped, and
/// a watch is fired for it again only once it is on.
#[allow(
    clippy::too_many_arguments,
    reason = "one pass's reads (the view's due tabs, its roster, its Off sessions) beside the switches"
)]
fn watch_upgrades(
    watches: &mut Watches,
    due: &[String],
    workers: &Workers,
    wanted: &HashMap<String, Program>,
    off: &OffSessions,
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
        if off.contains(&sid) {
            continue;
        }
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
        crate::build_info::version(),
        crate::build_info::git_commit()
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
/// the aterm-census guard `watch-hands` fences this function and everything of
/// this file it reaches.
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
            "harness @{sid}: an upgrade is due that the tab's worker had stopped asking for; \
             its next idle point takes it up"
        );
    }
    watched
}

/// Reap the workers that ended (their last act set `done`, so the join
/// waits on nothing but the thread's return); one held behind another
/// claim is remembered with the epoch it started at, and one its agent's own
/// switch revoked ([`HostHandle::set_config`]) marks its session for a
/// worker started as Off lifted ([`HostState::was_off`]) — an off and on
/// between two passes, which no roster read saw off, marks it too.
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
        // Its handles go with it (a successor's, registered since, stay).
        if let Some(sides) = CLAIM_SIDES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            && sides
                .get(&sid)
                .is_some_and(|side| Arc::ptr_eq(&side.interrupt, &w.interrupt))
        {
            sides.remove(&sid);
        }
        if w.revoked.load(Ordering::SeqCst) {
            host.was_off.insert(sid.clone());
        }
        match w.join.take().map(JoinHandle::join) {
            Some(Ok(WorkerExit::Held)) => {
                host.held.insert(sid, w.epoch);
            }
            Some(Ok(WorkerExit::Stopped | WorkerExit::SelfClaimed)) | None => {}
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
/// tab is asked about with the map's lock released.) A tab that closed is a
/// session the accounts recorder forgets too ([`Acts::account_gone`]: told
/// once, with no lock held, as the session is forgotten here, whatever agent
/// ran in it — nothing kept says which, and the recorder forgets only what
/// it has on record) — while the host supervises (`active`): with the policy
/// off nothing of the harness's is written, and while a handoff has it
/// suspended a tab this process no longer has may live on in the successor,
/// under the same session.
fn forget_closed(
    shared: &Shared,
    workers: &Workers,
    wanted: &HashMap<String, Program>,
    active: bool,
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
    if closed.is_empty() {
        return;
    }
    // A closed tab's copy of its self-claimed agent's record goes with it.
    for sid in &closed {
        crate::self_claim::forget(sid);
    }
    let forgotten: Vec<Arc<Mutex<Kept>>> = {
        let mut kept = shared.kept.lock().unwrap_or_else(PoisonError::into_inner);
        closed.iter().filter_map(|sid| kept.remove(sid)).collect()
    };
    // Nobody is handed a closed tab's exit: its watch ends.
    for kept in forgotten {
        with_kept_of(&kept, |k| k.exit_watch = None);
    }
    if active {
        for sid in &closed {
            (hooks.acts.account_gone)(sid);
        }
    }
}

/// Follow each worker whose publication moved, and every worker after a
/// generic wake, coalesced phase wakes, or the old timer deadline; and each
/// whose settle has come ([`follow_entries`]). The instant the next settle
/// comes, if one is owed.
fn follow_workers(
    shared: &Shared,
    host: &mut HostState,
    wanted: &HashMap<String, Program>,
    stamps: &HashMap<String, FollowStamp>,
    follow_all: bool,
    now: Instant,
    hooks: &Hooks,
) -> Option<Instant> {
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
    host.settle
        .retain(|sid, _| following.iter().any(|(followed, _)| followed == sid));
    follow_entries(
        &following,
        stamps,
        (&mut host.followed, &mut host.settle),
        follow_all,
        now,
        hooks,
    )
}

/// Select and follow from the roster-backed candidates. The hook performs
/// the real session-file read in production and is counted in the test.
///
/// THE RECORD IS FOLLOWED A MOMENT AFTER THE SCREEN MOVED, ONCE MORE (LC-1 of
/// ATERM-KI-1015-1, the 2026-10-15 sweep against Claude Code 2.1.300). Claude
/// Code rewrites its record of the agent (`sessions/<pid>.json`: the
/// conversation an in-app `/clear` or `/resume` moves it to) after it draws
/// — the writer [`STATUS_AFTER_SCREEN`] looks again for — so a follow at the
/// move can read the conversation the agent is LEAVING; and a `/clear` moves
/// no verdict at all, only its screen ([`FollowStamp::screen`]). Nothing
/// followed again until a timed pass a minute or more on, and a crash or a
/// restart's quit in between relaunched the left conversation in the
/// reopened tab and typed the continuation into it. So a follow made for a
/// move of the tab's publication owes the tab one more look
/// [`STATUS_AFTER_SCREEN`] on (`settle`, re-armed by every later move), and
/// a move of its screen alone is followed at once only while no such look
/// is owed: a person typing into an idle composer costs at most one read of
/// its record per settle, a busy agent's screen none (it is no screen move:
/// `AgentPublication::screen_moves`), an untouched tab none. A tab's FIRST
/// look owes one too once its screen has moved: the ring that woke it may be
/// the one the timeline spends per settle (`SessionTimeline::note_screen_move`),
/// and a `/clear` a moment later rings nothing (measured live: followed 7.7 s
/// on, not 2 s); a screen that never moved has spent no ring. A tab no stamp
/// names owes none (it is followed at every pass). The instant the next
/// settle comes, if one is owed.
fn follow_entries(
    following: &[(String, Arc<Mutex<Kept>>)],
    stamps: &HashMap<String, FollowStamp>,
    (followed, settle): (
        &mut HashMap<String, FollowStamp>,
        &mut HashMap<String, Instant>,
    ),
    follow_all: bool,
    now: Instant,
    hooks: &Hooks,
) -> Option<Instant> {
    for (sid, kept) in following {
        let stamp = stamps.get(sid).copied();
        let before = followed.get(sid).copied();
        // A move since the last follow (none: the first look).
        let moved = stamp.zip(before).filter(|(now, then)| now != then);
        let owed = settle.get(sid).copied();
        let settled = owed.is_some_and(|at| at <= now);
        let now_owed = follow_all
            || stamp.is_none()
            || before.is_none()
            || settled
            || moved.is_some_and(|(now, then)| owed.is_none() || now.moved_beyond_screen(then));
        if !now_owed {
            continue;
        }
        // A settle that came is spent whatever the look finds: one kept
        // past its instant would end every wait of the host's at once.
        if settled {
            settle.remove(sid);
        }
        if !follow_snapshot(sid, kept, hooks) {
            continue;
        }
        if let Some(stamp) = stamp {
            followed.insert(sid.clone(), stamp);
        }
        if moved.is_some() || (before.is_none() && stamp.is_some_and(|stamp| stamp.screen > 0)) {
            settle.insert(sid.clone(), now + (hooks.pause)(STATUS_AFTER_SCREEN));
        }
    }
    settle.values().min().copied()
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
///
/// AN OFF SESSION IS NEVER KEPT ([`OffSessions`]): the roster naming it
/// forgets what was held of it, and hands no exit for another group named in
/// a kept one's stead (its worker is stopped, never let go), so a pass that
/// cannot name its agent keeps nothing wanted — there is nothing of the
/// harness's to keep it for.
///
/// A STEP SINCE THE ROSTER WAS READ ([`StepsSeen`]): an agent the roster did
/// not name while its worker's own step ran, or after one ran since the read,
/// is kept for the pass the step's end rings, never judged here.
#[allow(
    clippy::too_many_arguments,
    reason = "one pass's reads (its roster, its steps, its Off sessions) beside the keep"
)]
fn keep_holding(
    holding: &mut HashMap<String, Held>,
    workers: &Workers,
    steps: &StepsSeen,
    wanted: &mut HashMap<String, Program>,
    stamps: &mut HashMap<String, FollowStamp>,
    off: &OffSessions,
    hooks: &Hooks,
) -> Option<Instant> {
    for (sid, stamp) in stamps.iter() {
        if off.contains(sid) {
            holding.remove(sid);
            continue;
        }
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
        if off.contains(sid) {
            return false;
        }
        let worker = workers.0.get(sid);
        if worker.is_some_and(|w| steps.stepping(sid, w)) {
            held.look = None;
            held.kept = None;
            return true;
        }
        if worker.is_some_and(|w| w.left.is(Word::Left))
            || (hooks.acts.holds)(sid, held.stamp.group) != Some(true)
        {
            return false;
        }
        // The keep begins at the stamp the roster was read at: no step since
        // (`stepping` just said so), and a step begun from here on moves it.
        let (since, _) = *held.kept.get_or_insert((now, steps.at(sid)));
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

/// EACH WORKER'S STEP STAMP AS A PASS READS THE ROSTER
/// ([`WorkerIdle::stepped`]), read just before it. The roster is read once,
/// at the pass's start; the pass judges an agent it does not name only later
/// ([`keep_holding`], [`visit_workers`]). A worker's own step may end its
/// agent and relaunch it in between — the step's act, never an exit — and a
/// roster read inside that step misses the agent though the step may be over
/// by the time the absence is judged, its `acting` long cleared. The stamp
/// moves as each step begins and again as it ends ([`WorkerIdle::act`],
/// [`WorkerIdle::acted`]), so only a stamp unmoved since this read, its
/// worker not acting now, says no step ran at any time since: the absence is
/// then the roster's word on the agent.
struct StepsSeen(HashMap<String, u64>);

impl StepsSeen {
    fn read(workers: &Workers) -> Self {
        Self(
            workers
                .0
                .iter()
                .map(|(sid, w)| (sid.clone(), w.stepped.load(Ordering::SeqCst)))
                .collect(),
        )
    }

    /// Session `sid`'s worker's stamp at the read (`0`: no worker then).
    fn at(&self, sid: &str) -> u64 {
        self.0.get(sid).copied().unwrap_or(0)
    }

    /// Whether worker `w` of session `sid` has been in a step at any time
    /// since the roster was read: acting now, or its stamp moved (`acting`
    /// read first: a step's end moves the stamp before it clears `acting`,
    /// so a step that ends between the two reads is seen by the second). A
    /// worker the read did not see started after it: the roster says nothing
    /// of its agent either.
    fn stepping(&self, sid: &str, w: &Worker) -> bool {
        w.acting.load(Ordering::SeqCst)
            || self
                .0
                .get(sid)
                .is_none_or(|then| *then != w.stepped.load(Ordering::SeqCst))
    }
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
/// tab that lives on, unless its own step ran at any time since the roster
/// was read ([`StepsSeen`]: an exit that step leaves behind is handed by the
/// pass its end rings, and carried as the harness's own restart's,
/// [`on_agent_left`]); one whose agent is back has
/// its exit over, one under an older policy hands over, and one told of a new
/// activation, or whose look is due, is asked for its loop's next idle point.
/// The earliest look still to come.
///
/// FIRST, THE AGENT'S OWN SWITCH: a worker whose agent the policy no longer
/// admits is stopped (its exit claim already revoked by
/// [`HostHandle::set_config`]). THEN AUTOPILOT OFF ([`OffSessions`]): an Off
/// session's worker is STOPPED — its agent still named or just gone from a
/// tab that lives on —
/// and never let go ([`Worker::leave`] hands the exit and starts the
/// relaunch decision; a stop's word ends a relaunch's pause already begun,
/// and says it is no exit). Asked once, as every stop here is; one whose own
/// upgrade step runs is stopped at the pass after the step (its end rings).
#[allow(
    clippy::too_many_arguments,
    reason = "one pass's reads (its roster, its steps, its Off sessions, whether it supervises) beside the workers"
)]
fn visit_workers(
    shared: &Shared,
    workers: &mut Workers,
    steps: &StepsSeen,
    told: &mut BTreeSet<String>,
    wanted: &HashMap<String, Program>,
    off: &OffSessions,
    active: bool,
    cfg_gen: u64,
    hooks: &Hooks,
) -> Option<Instant> {
    let activations = shared.activations.load(Ordering::SeqCst);
    let now = Instant::now();
    let mut next_look: Option<Instant> = None;
    for (sid, w) in &mut workers.0 {
        if !shared.switches.on_for(w.agent) {
            if !w.left.is(Word::Stopped) {
                w.stop();
            }
            continue;
        }
        if off.contains(sid) {
            if !w.acting.load(Ordering::SeqCst) && !w.left.is(Word::Stopped) {
                aterm_log::info!(
                    "harness @{sid}: autopilot is off; its supervisor stops, hands off the session"
                );
                w.stop();
            }
            continue;
        }
        if !wanted.contains_key(sid) {
            // The agent a worker's own upgrade step ends is that step's while
            // it runs: it comes back relaunched, or — its relaunch waited —
            // is handed at the next pass and carried as the harness's own.
            // Otherwise an agent that left a tab that lives on (it holds the
            // tab no longer: [`keep_holding`] kept every one that does) is the
            // worker's to handle; a tab that closed (or a policy that stopped)
            // just stops it. Each is asked once: a stop rings the bell, and one
            // asked again at each pass woke the host at once through the
            // worker's whole wind-down. A worker handed its exit, or whose
            // agent was back since, is still stopped when its tab closes (the
            // stop's word ends its relaunch's pause, and says it is no exit).
            // A step that ran at any time since the roster was read — still
            // running, or over already — may be what the roster missed: the
            // absence is judged by the pass the step's end rings
            // ([`StepsSeen`]).
            if !steps.stepping(sid, w) {
                if active && (hooks.acts.open)(sid) {
                    if !w.stop.load(Ordering::SeqCst) {
                        w.leave();
                    }
                } else if !w.left.is(Word::Stopped) {
                    w.stop();
                }
            }
            continue;
        }
        // The agent is back (relaunched, or started by someone): the exit is
        // over, and the agent the host's until its own worker starts. A
        // worker under an older policy hands over — a worker no stop reached
        // yet, as the model's `HandOver` has it (round three of the joint
        // audit of 2026-10-01): a hand-over is a stop for a successor
        // (once, as a stop above), and a worker stopped already needs none.
        // One handed its exit ([`Worker::leave`] set its stop) only finishes
        // deciding it, its successor started at the pass that finds the
        // agent once it is reaped; handed over, its word read as a stop for
        // good, and the agent the host read back, gone again before the
        // worker read it, left the relaunch it owed to a reader with nothing
        // to decide it on ([`gone_again`]): the session forgotten,
        // `nothing-recorded`.
        if !w.back(wanted[sid]) && w.cfg_gen != cfg_gen && !w.stop.load(Ordering::SeqCst) {
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

/// Start a worker for every wanted session with none and not held behind a
/// claim not yet released — under the one hold of the state lock that
/// re-reads the policy and publishes the stop handles: a suspend() either
/// came first (nothing starts) or finds every worker started here. One held
/// off for its adopted-claim grace ([`ADOPTED_CLAIM_GRACE`]) is started
/// WATCHING until the grace is over ([`WorkerJob::watch_until`], round eight
/// of the update audit, #501): it claims nothing until then, and its agent's
/// exit is read. The worker keeps its own time: the host owes the grace's
/// end no wake.
///
/// And one that reads its agent's exit alone for every exit no worker was
/// handed ([`Kept::unwatched`], round thirteen; or that an exit watch hands
/// itself once read, round seventeen: [`ExitWatched::hands`]) — its worker
/// ended by a Commit's stop, handed over to a successor that never came
/// (round fourteen), or ended at the landing of a relaunch, and reaped, its tab open and the roster naming no agent
/// there — in the same hold: a host supervising again (a failed Commit's
/// resume, or the pass after a hand-over) hands it, where a worker handed it
/// had read it. Another supervisor's live claim at the exit makes it that
/// one's ([`on_agent_left`]) — past the session's adopted-claim grace, unless
/// the harness's own restart ended the agent, which is carried; inside it
/// the claim comes first even of that exit and is asked again before every
/// attempt, as the watcher would have. A harness switched off lets it go: an
/// exit nobody supervises is nobody's.
///
/// `wanted` is the roster as this pass read it, while the host supervised
/// (`read`) — or no roster at all: a pass that began while a Commit held the
/// host read none, and names no agent. Such a pass that finds the host
/// supervising again here (a failed Commit's resume, whose bell brings the
/// next pass) starts nothing on it (the joint audit of 2026-10-01): acted
/// on, every exit left to the host read as one, and an exit worker started
/// for every agent alive in its tab.
///
/// AN OFF SESSION GETS NO WORKER ([`OffSessions`]), beside a held one: none
/// for its agent, and none to read an exit nobody was handed — that exit, and
/// the exit watch kept for it, are forgotten in the very read that would hand
/// them ([`unwatched_exits`]; [`Kept::unwatched`], [`Kept::exit_watch`]): an
/// Off session's exit is nobody's, as one under a harness switched off is.
#[allow(
    clippy::too_many_arguments,
    reason = "one pass's reads (its roster, its Off sessions, whether it read one) beside the policy it runs under"
)]
fn start_workers(
    shared: &Arc<Shared>,
    host: &mut HostState,
    wanted: &HashMap<String, Program>,
    off: &OffSessions,
    read: bool,
    cfg: &SupervisorConfig,
    cfg_gen: u64,
    hooks: &Hooks,
) {
    let ctx = StartCtx {
        cfg,
        cfg_gen,
        epoch: (hooks.claim_epoch)(),
        activations: shared.activations.load(Ordering::SeqCst),
        hooks,
    };
    let mut starts: Vec<(String, Program)> = wanted
        .iter()
        .filter(|(sid, _)| !host.workers.0.contains_key(*sid))
        .filter(|(sid, _)| host.held.get(*sid).is_none_or(|e| ctx.epoch > *e))
        .filter(|(sid, _)| !off.contains(sid))
        .map(|(sid, agent)| (sid.clone(), *agent))
        .collect();
    starts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut exits = unwatched_exits(shared, &host.workers, wanted, off, hooks);
    let mut state = shared.lock();
    let active = Shared::active(&state, shared.headless) && !state.shutting_down;
    if !(active && read && state.cfg_gen == cfg_gen) {
        starts.clear();
        if !active && !state.suspended {
            for (_, kept, ..) in exits.drain(..) {
                with_kept_of(&kept, |k| {
                    k.unwatched = None;
                    k.exit_watch = None;
                });
            }
        }
        exits.clear();
    }
    // THE ADOPTED-CLAIM GRACE (round four, item 9): a session an update
    // handed over is not claimed while an external supervisor that held it
    // may still be on its way to claiming it again — and is WATCHED meanwhile
    // (round eight, #501): a worker held off was nobody's, and its agent's
    // crash inside the grace was never relaunched.
    let now = Instant::now();
    if !state.suspended {
        state.claim_grace.start(now);
    }
    state.claim_grace.prune(now);
    for (sid, agent) in starts {
        if !shared.switches.on_for(agent) {
            continue;
        }
        let watch = state.claim_grace.runs_until(&sid, now);
        start_worker(shared, host, &ctx, &sid, agent, None, watch);
    }
    for (sid, _, exit) in exits {
        if !shared.switches.on_for(exit.agent) {
            continue;
        }
        // Not "it exited": the roster names no agent in the tab and nothing
        // keeps it there, but a hold that cannot be read keeps nothing, nor
        // does one past its ceiling ([`keep_holding`]) — the worker reads
        // whether it did.
        aterm_log::info!(
            "harness @{sid}: no worker watched its agent (aterm's update held the harness, a \
             changed [harness] policy was handing it to a new worker, or a relaunch landed it \
             before any pass named it), and the roster names no agent in the tab now; reading \
             its exit"
        );
        // Inside the grace another supervisor's claim comes first even of an
        // exit the harness's own restart ended, and is asked again before
        // every attempt at its relaunch, for a worker that reads the exit
        // alone as for one that watched ([`WorkerJob::watch_until`]).
        let watch = state.claim_grace.runs_until(&sid, now);
        start_worker(shared, host, &ctx, &sid, exit.agent, Some(exit), watch);
    }
    publish_workers(&mut state, &host.workers);
}

/// The sessions the pass knows of — every agent it wants, every worker, every
/// session it keeps anything of — that are Off, published for
/// [`HostHandle::relaunching`] ([`State::off`]) BEFORE the pass starts any
/// worker ([`host_pass`]): a worker it starts is for a session not Off, so
/// the set names what the pass read before the start is seen. The kept map's
/// lock is taken and let go before the state's.
fn publish_off(
    shared: &Shared,
    workers: &Workers,
    wanted: &HashMap<String, Program>,
    off: &OffSessions,
) {
    let mut known: std::collections::HashSet<String> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .keys()
        .cloned()
        .collect();
    known.extend(wanted.keys().cloned());
    known.extend(workers.0.keys().cloned());
    known.retain(|sid| off.contains(sid));
    shared.lock().off = known;
}

/// The host's live workers as it leaves them ([`State::live`]), and their
/// stop handles for [`HostHandle::suspend`] ([`State::stops`]): published
/// under the state's lock, the one a Commit's stop takes.
fn publish_workers(state: &mut State, workers: &Workers) {
    let mut live: Vec<String> = workers.0.keys().cloned().collect();
    live.sort();
    state.live = live;
    state.stops = workers
        .0
        .values()
        .map(|w| StopHandle {
            agent: w.agent,
            revoked: Arc::clone(&w.revoked),
            handover: Arc::clone(&w.handover),
            stop: Arc::clone(&w.stop),
            left: Arc::clone(&w.left),
            interrupt: Arc::clone(&w.interrupt),
            kept: Arc::clone(&w.kept),
            left_at_commit: Arc::clone(&w.left_at_commit),
        })
        .collect();
}

/// What every worker one pass starts shares ([`start_worker`]).
struct StartCtx<'a> {
    cfg: &'a SupervisorConfig,
    cfg_gen: u64,
    /// The claim epoch at the pass: a held session is retried only past it.
    epoch: u64,
    activations: u64,
    hooks: &'a Hooks,
}

/// THE EXITS NO WORKER WAS HANDED ([`Kept::unwatched`], round thirteen —
/// and, round seventeen, an exit watch's that hands itself, read:
/// [`ExitWatched::hands`]): `(sid, kept, exit)` for every session so marked
/// whose worker is reaped,
/// the roster naming no agent in its tab and the tab open — a closed one
/// owes nothing, and [`forget_closed`] lets its session go. Each session's
/// lock is taken alone, after the map's is let go, and each tab is asked
/// with none held (the store's).
///
/// AUTOPILOT OFF FORGETS WHAT WAS LEFT TO THE HOST of an Off session's agent
/// ([`OffSessions`]), its agent named or not, its worker reaped or not: the
/// exit no worker was handed ([`Kept::unwatched`]) and the exit watch
/// ([`Kept::exit_watch`]) are cleared under the very lock hold that would
/// read them, and never handed, so no worker is started to read that exit and
/// relaunch it. (The review of 345c41647: a forget before this read left a
/// window in which a mark set again — an exit watch armed at a landing on
/// another thread, its exit read — was read and handed.)
///
/// AND WHAT WAS READ OF THAT AGENT FOR A RELAUNCH once nothing will handle
/// its exit (the review of a295792e4): an Off session the roster names no
/// agent in, its worker reaped, has its kept snapshot forgotten and
/// unpublished, as a handled exit's is ([`forget_snapshot`]), and the
/// relaunch on exit its stopped back-off still owed ([`Kept::owed`]) with it.
/// Off stops the worker without handing it the exit, so no exit path of a
/// worker forgets them, and no follow reaches a session with no worker
/// ([`follow_workers`]): once Off was lifted on the bare shell, the
/// conversation a person ended under Off was still the tab's agent to every
/// reader — on the tab's leaf in the crash journal and the quit layout
/// ([`published_snapshot`]), which resumed it at the next launch after aterm
/// ended uncleanly; in [`HostHandle::relaunching`]; and to the restore's
/// program check, which read any other program run there as the hosted
/// agent. A roster that misreads an Off agent still running costs only what
/// Off already refuses: a relaunch of it. Its worker, once Off is lifted,
/// reads its own at its attach.
fn unwatched_exits(
    shared: &Shared,
    workers: &Workers,
    wanted: &HashMap<String, Program>,
    off: &OffSessions,
    hooks: &Hooks,
) -> Vec<(String, Arc<Mutex<Kept>>, Unwatched)> {
    let kept: Vec<(String, Arc<Mutex<Kept>>)> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|(sid, _)| {
            off.contains(sid) || (!wanted.contains_key(*sid) && !workers.0.contains_key(*sid))
        })
        .map(|(sid, kept)| (sid.clone(), Arc::clone(kept)))
        .collect();
    let mut exits: Vec<(String, Arc<Mutex<Kept>>, Unwatched)> = kept
        .into_iter()
        .filter_map(|(sid, kept)| {
            let off = off.contains(&sid);
            // Its agent named nowhere and its worker reaped: nothing will
            // read that agent's exit.
            let gone = !wanted.contains_key(&sid) && !workers.0.contains_key(&sid);
            let exit = with_kept_of(&kept, |k| {
                if off {
                    k.unwatched = None;
                    k.exit_watch = None;
                    if gone {
                        // The published map's lock is a leaf, taken inside
                        // the session's as every publish takes it.
                        k.snapshot = None;
                        k.owed = None;
                        publish_snapshot(&sid, None);
                    }
                    return None;
                }
                k.unwatched.or_else(|| {
                    k.exit_watch
                        .as_ref()
                        .filter(|w| w.read.is_some())
                        .and_then(|w| w.hands)
                })
            })?;
            Some((sid, kept, exit))
        })
        .filter(|(sid, ..)| (hooks.acts.open)(sid))
        .collect();
    exits.sort_by(|a, b| a.0.cmp(&b.0));
    exits
}

/// AN AGENT THAT LEFT A TAB WHILE A SWITCHED-OFF AGENT'S IS FORGOTTEN, as
/// one that ended under Off is ([`unwatched_exits`]): no worker's exit path
/// forgets what was read of it, so its conversation stayed published
/// ([`published_snapshot`]) — on the tab's leaf in the crash journal and the
/// restart, logout and shutdown layouts — and the next launch resumed it in
/// a tab the person had moved on in. Asked only on a pass that read the
/// roster — never a Commit's hold, never under the master switch off, so an
/// agent `admits` refuses is one its OWN table switches off — of a tab no
/// worker of this host holds and nothing of whose agent is still in hand (no
/// exit left to the host, no watch on its process that has read no exit yet
/// or read one to hand, no relaunch owed, no Commit's stop to take up:
/// [`Kept::unwatched`], [`Kept::exit_watch`], [`Kept::owed`],
/// [`Kept::commit`]; what reads one, stopped by a switched-off replacement in
/// [`host_pass`], decides first), whose kept snapshot is:
///
/// * of an agent switched off, with no agent named in the tab and its
///   process no longer holding it (`Acts::holds` says so, read outside the
///   session's lock): the person ended it. A name the roster could not read,
///   or a hold that cannot be read, forgets nothing;
/// * of another agent than the switched-off one the roster names in the tab
///   (`named_off`): that agent left and the person runs another there, whose
///   worker its switch admits none of.
fn forget_left_while_switched_off(
    shared: &Shared,
    workers: &Workers,
    wanted: &HashMap<String, Program>,
    named_off: &HashMap<String, Program>,
    admits: impl Fn(Program) -> bool,
    hooks: &Hooks,
) {
    let kept: Vec<(String, Arc<Mutex<Kept>>)> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|(sid, _)| !wanted.contains_key(*sid) && !workers.0.contains_key(*sid))
        .map(|(sid, kept)| (sid.clone(), Arc::clone(kept)))
        .collect();
    fn pending(k: &Kept) -> bool {
        k.unwatched.is_some()
            || k.exit_watch
                .as_ref()
                .is_some_and(|w| w.read.is_none() || w.hands.is_some())
            || k.owed.is_some()
            || k.commit != Committed::No
    }
    for (sid, kept) in kept {
        let Some(snapshot) = with_kept_of(&kept, |k| {
            k.snapshot.as_ref().filter(|_| !pending(k)).cloned()
        }) else {
            continue;
        };
        let agent = snapshot.agent().program();
        let still_off = || match named_off.get(&sid) {
            Some(named) => *named != agent && !admits(*named),
            None => !admits(agent),
        };
        let left = still_off()
            && (named_off.contains_key(&sid)
                || (hooks.acts.holds)(&sid, i32::try_from(snapshot.pid).unwrap_or(0))
                    == Some(false));
        if left {
            // The probe held no locks. Revalidate under the same state -> Kept
            // order as watch_handed, so a newly armed claim is either already
            // visible here or is created after this clear and seeds its snapshot.
            let state = shared.lock();
            with_kept_of(&kept, |k| {
                if would_supervise(&state.cfg, shared.headless)
                    && still_off()
                    && !pending(k)
                    && k.snapshot.as_ref() == Some(&snapshot)
                {
                    k.unwatched = None;
                    k.exit_watch = None;
                    k.snapshot = None;
                    k.owed = None;
                    publish_snapshot(&sid, None);
                }
            });
        }
    }
}

/// Start session `sid`'s worker for its agent `agent` and keep it in
/// `host.workers` — or, `exit` naming an exit no worker was handed
/// ([`Kept::unwatched`]), one that reads that agent's exit alone
/// ([`exit_main`]), in the group the roster last named it in. `watch`: the
/// instant the session's adopted-claim grace runs out, until which the
/// worker only watches ([`WorkerJob::watch_until`]) — and, for one that
/// reads an exit alone, until which another supervisor's live claim comes
/// first even of an exit the harness's own restart ended, and is asked again
/// before every attempt at its relaunch.
fn start_worker(
    shared: &Arc<Shared>,
    host: &mut HostState,
    ctx: &StartCtx<'_>,
    sid: &str,
    agent: Program,
    exit: Option<Unwatched>,
    watch: Option<Instant>,
) {
    let StartCtx {
        cfg,
        cfg_gen,
        epoch,
        activations,
        hooks,
    } = *ctx;
    // A replacement worker attaches a fresh snapshot, even if the tab's
    // published phase stayed put across the handover.
    host.followed.remove(sid);
    host.held.remove(sid);
    // A session read Off since its last worker started: this one starts as
    // its Off lifted ([`HostState::was_off`]).
    let lifted_off = host.was_off.remove(sid);
    let faults = Arc::clone(
        shared
            .faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(sid.to_string())
            .or_default(),
    );
    let kept = Arc::clone(
        shared
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(sid.to_string())
            .or_default(),
    );
    let relaunch_word = with_kept_of(&kept, |k| {
        // Its exit is this worker's now, an ordinary exit of this host:
        // nothing of the Commit that left it unwatched is left to act on.
        k.unwatched = None;
        k.commit = Committed::No;
        if exit.is_some() {
            return false;
        }
        // An agent runs in the tab: nothing of a relaunch is due, and a word
        // one raised goes. Nor is one owed for the last agent's exit, nor a
        // Commit's stop of its worker (reaped before this start) anything
        // more to act on ([`Committed`]).
        k.owed = None;
        k.relaunches.running() == Say::Clear
    });
    let park = Arc::new(AtomicBool::new(false));
    let look_at: Arc<Mutex<Option<Instant>>> = Arc::default();
    let acting: Arc<AtomicBool> = Arc::default();
    // A worker started for a kept agent takes the stamp the keep began
    // at: its predecessor's step before the keep is no step since.
    let stepped = Arc::new(AtomicU64::new(
        host.holding
            .get(sid)
            .and_then(|h| h.kept)
            .map_or(0, |(_, then)| then),
    ));
    let note: Note = Arc::default();
    let group = Arc::new(AtomicI32::new(exit.map_or_else(
        || host.holding.get(sid).map_or(0, |h| h.stamp.group),
        |exit| exit.group,
    )));
    let points: Arc<Points> = Arc::default();
    let stop = Arc::new(AtomicBool::new(exit.is_some()));
    let revoked = Arc::new(AtomicBool::new(false));
    let beside: Arc<Mutex<Option<String>>> = Arc::default();
    let idle = Arc::new(WorkerIdle {
        sid: sid.to_string(),
        agent,
        stop: Arc::clone(&stop),
        grace: cfg.human_grace_s,
        park: Arc::clone(&park),
        look_at: Arc::clone(&look_at),
        acting: Arc::clone(&acting),
        stepped: Arc::clone(&stepped),
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
        memory: Mutex::default(),
        points: Arc::clone(&points),
        tap: Mutex::default(),
        lifted_off,
        beside: Arc::clone(&beside),
    });
    // The usage tap's readings of processes gone, and of a day ago.
    if let Some(seat) = (hooks.acts.account_seat)() {
        tap::sweep(
            &tap::rl_dir_in(&seat.state),
            seat.now,
            &aterm_uds::process::pid_alive,
        );
    }
    let job = WorkerJob {
        sid: sid.to_string(),
        agent,
        revoked: Arc::clone(&revoked),
        opts: SuperviseOpts {
            idle_host: Some(idle),
            ..hosted_opts(cfg, sid, aterm_types::dirs::state_dir().as_deref())
        },
        // An exit handed from the start, as [`Worker::leave`] hands it.
        stop,
        interrupt: Arc::default(),
        handover: Arc::default(),
        clear_badge: relaunch_word,
        faults,
        park,
        look_at,
        left: Arc::new(if exit.is_some() {
            ExitWord::handed()
        } else {
            ExitWord::default()
        }),
        left_at_commit: Arc::default(),
        acting,
        switches: Arc::clone(&shared.switches),
        kept: Arc::clone(&kept),
        note: Arc::clone(&note),
        group: Arc::clone(&group),
        host: Arc::downgrade(shared),
        watch_until: watch,
        beside: Arc::clone(&beside),
    };
    let (stop, interrupt) = (Arc::clone(&job.stop), Arc::clone(&job.interrupt));
    let handover = Arc::clone(&job.handover);
    let (park, look_at) = (Arc::clone(&job.park), Arc::clone(&job.look_at));
    let (left, acting) = (Arc::clone(&job.left), Arc::clone(&job.acting));
    let left_at_commit = Arc::clone(&job.left_at_commit);
    let done = Arc::new(AtomicBool::new(false));
    let main: fn(WorkerJob, &Hooks) -> WorkerExit = if exit.is_some() {
        exit_main
    } else {
        worker_main
    };
    let Some(join) = spawn_worker(sid, job, Arc::clone(&done), hooks, main) else {
        // Not started: the exit is still nobody's, for a later pass.
        if exit.is_some() {
            with_kept_of(&kept, |k| k.unwatched = exit);
        }
        return;
    };
    // The control thread reaches a loop's worker through these when a proven
    // self-claim moves ([`self_claimed`]).
    if exit.is_none() {
        CLAIM_SIDES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(HashMap::new)
            .insert(
                sid.to_string(),
                ClaimSide {
                    interrupt: Arc::clone(&interrupt),
                    beside,
                },
            );
    }
    if exit.is_none() {
        if watch.is_some() {
            aterm_log::info!(
                "harness @{sid}: watching (an update handed it over: claimed once its \
                 adopted-claim grace is over)"
            );
        } else {
            aterm_log::info!("harness @{sid}: supervising");
        }
    }
    host.workers.0.insert(
        sid.to_string(),
        Worker {
            agent,
            revoked,
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
            kept,
            left_at_commit,
        },
    );
    // A worker's start — a hot swap's, an attach's — is a watch told
    // (design record 2026-09-28, §3.2 C3): a record an older build
    // left is read, and retargeted, without waiting for a point.
    if exit.is_none() && upgrades(agent) {
        host.watches.told.insert(sid.to_string());
    }
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
/// ([`aterm_phase::Program::supervisable`]). The reader counts only under a
/// name that could host an agent ([`aterm_phase::may_host_agent`]) or no name
/// yet, the names the verdict identifies by the screen under
/// (`presence::agent_verdict`): under any other it is a verdict the program
/// has since left (a same-group exec to `zsh`, read before the next verdict).
pub(crate) fn agent_of(publication: &crate::session_timeline::AgentPublication) -> Option<Program> {
    identified_agent(publication).filter(|p| p.supervisable())
}

/// The agent that owns the foreground prompt, including one whose reader is
/// intentionally read-only. A known program name wins; a screen's reader is
/// current only under a runtime or no name yet, never a known shell that has
/// since replaced it. This is prompt ownership, not permission to supervise:
/// [`agent_of`] applies that separate predicate.
pub(crate) fn identified_agent(
    publication: &crate::session_timeline::AgentPublication,
) -> Option<Program> {
    let program = publication.program.as_deref();
    program
        .and_then(aterm_phase::program_of)
        .or(publication
            .reader
            .filter(|_| program.is_none_or(aterm_phase::may_host_agent)))
        .filter(|p| match p {
            Program::Claude | Program::Codex | Program::Agy | Program::Muse => true,
            Program::Generic => false,
        })
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
                    screen: publication.screen_moves,
                },
            ))
        })
        .collect()
}

/// Every live session's own Autopilot word ([`Hooks::autopilot`]), read
/// in-process: the store's handles (its read lock, let go first), then each
/// one's metadata — a leaf lock, taken alone. The host's own lock is never
/// held here, nor taken under the registry's (`App::harness_hosting`'s
/// order). A session with no word, or one this build cannot read, is not in
/// the map: the table's default decides it.
fn words_of(store: &Store) -> HashMap<String, Autopilot> {
    let handles = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .snapshot();
    handles
        .iter()
        .filter(|h| h.state != SessionState::Exited)
        .filter_map(|h| {
            let word = h
                .ctx
                .meta
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .autopilot_word()?;
            Some((h.sid.as_str().to_string(), word))
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

/// [`Acts::offered`] over tab `sid`'s live context: the one predicate
/// ([`crate::restart_act::offered`]) for its published stall's leader (none
/// published: the tab's foreground process is not named), as `Some(promised)`
/// where the act would be carried. The store's lock is held only to clone
/// the tab's context, its timeline's only to copy the leader; the predicate
/// takes its own leaf locks after both.
pub(crate) fn offered_now(store: &Store, sid: &str) -> Option<bool> {
    let ctx = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .map(|h| Arc::clone(&h.ctx))?;
    let leader = ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .agent()
        .input
        .as_ref()
        .and_then(|fact| fact.restart.leader);
    crate::restart_act::offered(&ctx, leader, crate::metrics::now_us()).map(|offer| offer.promised)
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
    ctl.admission = Some((Arc::clone(&job.switches), job.agent));
    // Watch-only beside a proven self-claim: the transport itself refuses
    // every write, whatever the loop asks (§4).
    ctl.watch_only = job.beside();
    // Every write the loop types is fenced on its own claim: once a proven
    // self-claim replaced it, none lands (the self-claim contract, §2).
    ctl.fence = Some(holder());
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
    // Beside a proven self-claim: watch only, the remedies standing (§4).
    session.set_beside(job.beside());
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
    let limit_home = home.clone();
    let account_home = home.clone();
    let usage_home = home.clone();
    let seat_home = home.clone();
    let cold_home = home.clone();
    let limit_read: Mutex<HashMap<String, (Instant, Option<u64>)>> = Mutex::default();
    let fg_store = store.clone();
    let holds_store = store.clone();
    let carry_store = store.clone();
    let offered_store = store.clone();
    let opts = move |sid: &str, grace: u32| {
        place.clone().map(|(home, state)| upgrade_drive::Opts {
            // `[harness] usage_tap`, read live ([`Switches::set`]): the
            // relaunch lines this worker types carry the tap where its gate
            // holds, naming this aterm — and on the login of the machine's
            // one account, read now (a renewal moves it).
            usage_tap: USAGE_TAP
                .load(Ordering::SeqCst)
                .then(aterm_agent::harness::tap::stable_exe)
                .flatten()
                .map(|exe| aterm_agent::harness::tap::Armed {
                    exe,
                    lone_login: aterm_time::unix_secs()
                        .and_then(|now| i64::try_from(now).ok())
                        .and_then(|now| {
                            accounts::lone_login_at(
                                accounts::default_roster_path().as_deref(),
                                &accounts::Store::at(&state),
                                now,
                            )
                        }),
                }),
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
            // `[harness] upgrade_ask`, read live ([`Switches::set`]).
            ask: UPGRADE_ASK.load(Ordering::SeqCst),
            // Two or more accounts listed: the home's model caches are
            // whichever login started last, and are not read.
            several_logins: several_logins(),
            instances: upgrade_drive::Instances::Machine,
            // `[harness.<agent>] enabled = false`, read live
            // ([`Switches::set`]): no step, notice or carry after an update
            // takes an off agent's lane or its restart in flight.
            agents_off: AGENTS_OFF
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        })
    };
    let opts = Arc::new(opts);
    let [o1, o2, o3, o4, o5, o6, o7, o8, o9, o10] = std::array::from_fn(|_| Arc::clone(&opts));
    let [o11, o12, o13, o14, o15, o16, o17, o18, o19] = std::array::from_fn(|_| Arc::clone(&opts));
    let o_self = Arc::clone(&opts);
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
        exit_look: Arc::new(move |sid, snap| match snap.agent() {
            // A Codex's exit is its shell's word, over the control socket.
            aterm_agent::harness::upgrade::Agent::Codex => {
                o13(sid, 0).map(|o| relaunch::look_at_codex_exit(&o, snap))
            }
            aterm_agent::harness::upgrade::Agent::Claude => exit_home
                .as_deref()
                .map(|home| relaunch::look_at_exit(home, snap)),
        }),
        // Another process under the pid, or none, is the snapshot's ended;
        // the pid's own end is the kernel's push, a zombie's `ESRCH` an end.
        exited: Arc::new(|snap, within| {
            !restart_stamp::alive_as(snap.pid, &snap.start)
                || aterm_agent::harness::upgrade_wake::wait_exit(snap.pid, within)
        }),
        asked: Arc::new(|sid, snap| {
            let state = harness_state()?;
            restart_stamp::lookup(
                &state,
                sid,
                snap.pid,
                &snap.start,
                aterm_time::unix_secs().unwrap_or(0),
            )
        }),
        offered: Arc::new(move |sid| offered_now(&offered_store, sid)),
        self_copy: Arc::new(crate::self_claim::copy_of_tab),
        self_relaunch: Arc::new(move |sid, grace, snap, resume, asked| {
            let Some(o) = o_self(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = upgrade_drive::self_claimed_after_exit(&o, snap, resume, asked);
            aterm_log::info!("harness @{sid}: {}", r.line_as("self-claimed-relaunch"));
            r.step
        }),
        relaunch: Arc::new(move |sid, grace, snap, left, upgrade, asked| {
            let Some(o) = o5(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::after_exit(&o, snap, left, upgrade, asked);
            aterm_log::info!("harness @{sid}: {}", r.line_as("relaunch"));
            r.step
        }),
        restarted: Arc::new(move |sid, agent, pid| {
            o15(sid, 0).is_some_and(|o| relaunch::restarted(&o, agent, pid))
        }),
        carry: Arc::new(move |sid, grace, agent, pid| {
            let Some(o) = o16(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::carry_restart(&o, agent, pid);
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
            // An account move's login is the ROSTER's, read as the relaunch
            // is made (a label it no longer lists is refused there, before
            // any lock) through this machine's logins (a renewed login is
            // the row's); every other restart re-applies the login its
            // process had, and reads no roster.
            let r = match why {
                Restart::Account { .. } => {
                    let roster = accounts::default_roster_path()
                        .and_then(|path| accounts::Roster::load(&path).ok())
                        .unwrap_or_default();
                    let roster = match (harness_state(), aterm_time::unix_secs()) {
                        (Some(state), Some(now)) => match accounts::Store::at(&state)
                            .bound(&roster, i64::try_from(now).unwrap_or(i64::MAX))
                        {
                            Ok(roster) => roster,
                            Err(_) => return "wait:account-state".to_string(),
                        },
                        _ => return "wait:account-state".to_string(),
                    };
                    relaunch::restart_under(&o, why, &roster)
                }
                _ => relaunch::restart_here(&o, why),
            };
            aterm_log::info!(
                "harness @{sid}: {}",
                r.line_as(&format!("restart:{}", why.word()))
            );
            r.step
        }),
        tasked: Arc::new(move |sid, snap, memo| {
            let o = o10(sid, 0)?;
            memo.of_snapshot(&o.home, snap, &upgrade_drive::supervisor_typed(&o, sid))
        }),
        limit_reset: Arc::new(move |snap| {
            let session = snap.session.as_deref()?;
            limit_reset_kept(&limit_read, session, || {
                limit_home
                    .as_deref()
                    .and_then(|home| upgrade_drive::named_limit_reset(home, snap))
            })
        }),
        transcript_tail: Arc::new(upgrade_drive::snapshot_transcript_tail),
        account_wall: Arc::new(move |sid, agent, settled| {
            account_wall_live(account_home.as_deref(), sid, agent, settled)
        }),
        account_usage: Arc::new(move |sid, agent, settled, seen, listed| {
            account_usage_live(usage_home.as_deref(), sid, agent, settled, seen, listed)
        }),
        account_gone: Arc::new(account_gone_live),
        account_seat: Arc::new(move || account_seat_live(seat_home.as_deref())),
        account_env: Arc::new(|pid| atpkg::caller_shell::process_args(pid).map(|args| args.env)),
        account_settings: Arc::new(crate::harness_netprobe::stored_login_of_agent),
        tap_born: Arc::new(|pid| {
            aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)
                .and_then(|born| i64::try_from(born.seconds).ok())
        }),
        account_restart: Arc::new(move |sid, pid| {
            o19(sid, 0).and_then(|o| relaunch::account_restart(&o, pid))
        }),
        account_made: Arc::new(|sid, to, at| {
            // Under the state root the session's loop keeps its ledger in
            // ([`hosted_opts`]).
            let made = aterm_types::dirs::state_dir()
                .is_some_and(|root| account_made_at(&root, sid, to, at));
            if !made {
                aterm_log::info!("harness @{sid}: account move to {to}: not written for its loop");
            }
        }),
        account_cold: Arc::new(move |snap| {
            account_seat_live(cold_home.as_deref()).and_then(|seat| {
                account_cold_at(&seat, snap, || {
                    atpkg::caller_shell::process_args(snap.shell).map(|args| args.env)
                })
            })
        }),
        codex_reset: Arc::new(upgrade_drive::codex_wall_reset),
        hold: Arc::new(move |sid, until| {
            o12(sid, 0).is_none_or(|o| upgrade_drive::hold_clock(&o, until))
        }),
        route: Arc::new(crate::harness_netprobe::route_of_agent),
        memory: {
            let board = Arc::new(MemoryBoard::default());
            Arc::new(move |sid: &str, pid| {
                let mut m = agent_memory(pid)?;
                m.largest_other_mib = board.record(sid, m.footprint_mib, Instant::now());
                Some(m)
            })
        },
        watch: Arc::new(move |sid, grace, how| watch_off_any_point(o18(sid, grace), sid, how)),
    }
}

/// THE PRODUCT'S [`Acts::account_wall`]: what the recorder is handed, read
/// now ([`account_wall_at`]) — under the home Claude's records live in, the
/// harness state root and the roster beside the `aterm.toml` this window
/// loads, on this machine's clock. Without one of them a word and nothing
/// more (`no-home`, `no-state`, `no-clock`). Nothing is typed, no session is
/// touched, and what is written is the accounts state alone.
fn account_wall_live(
    home: Option<&Path>,
    sid: &str,
    agent: &WalledAgent<'_>,
    settled: &dyn Fn() -> bool,
) -> (&'static str, Option<String>) {
    let Some(home) = home else {
        return ("no-home", None);
    };
    let Some(state) = harness_state() else {
        return ("no-state", None);
    };
    let Some(now) = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()) else {
        return ("no-clock", None);
    };
    let roster = accounts::default_roster_path();
    account_wall_at((home, &state, roster.as_deref()), sid, agent, settled, now)
}

/// [`account_wall_live`] over the places it is given — `(home, harness
/// state root, roster file)`, the last `None` where no config path is known
/// (then there is no roster) — and the caller's clock. In this order, each a
/// word and nothing more when it fails: the roster (`roster-refused` for a
/// file that does not admit; a missing file is the empty roster) — and WITH
/// NO ACCOUNT LISTED NOTHING OF THE AGENT IS READ (`not-listed`: the
/// packaged state, inert), but under `[harness] usage_notes`
/// ([`WalledAgent::own`]), where the machine's own row stands for the roster;
/// then the agent's exec environment, read once
/// (`no-env` where it cannot be read), its settings side (`settled`: its
/// route, its settings files, the vendor's profile store — asked only for a
/// process a row of the roster names, whose environment alone leaves it a
/// stored login in the default home), its conversation's standing quota wall
/// (one bounded read of the transcript's tail) and its process's start,
/// handed to [`account_wall_of`] — where the store's one short lock is
/// taken, for a bounded wait, and only for a wall that counts.
fn account_wall_at(
    (home, state, roster): (&Path, &Path, Option<&Path>),
    sid: &str,
    agent: &WalledAgent<'_>,
    settled: &dyn Fn() -> bool,
    now: i64,
) -> (&'static str, Option<String>) {
    let roster = match roster.map(accounts::Roster::load) {
        None => accounts::Roster::default(),
        Some(Ok(roster)) => match accounts::Store::at(state).bound(&roster, now) {
            Ok(roster) => roster,
            Err(_) => return ("roster-refused", None),
        },
        // The reason names the file: the owner reads it in `aterm harness
        // accounts`, never in this log.
        Some(Err(_)) => return ("roster-refused", None),
    };
    let roster = match roster {
        // With `[harness] usage_notes` on, the machine's own row: its walls
        // and what they say of its credits are kept, for the record.
        roster if roster.rows.is_empty() && agent.own => accounts::Roster::own(),
        roster if roster.rows.is_empty() => return ("not-listed", None),
        roster => roster,
    };
    let Some(args) = atpkg::caller_shell::process_args(agent.snap.pid) else {
        return ("no-env", None);
    };
    // Its settings are read only where the answer needs them: for a process
    // the roster's judgments could be about at all.
    let who = accounts::ProcessLogin::of_env(args.env.iter().filter_map(|kv| kv.split_once('=')));
    let read = WallRead {
        env: &args.env,
        settled: accounts::judged_label(&roster, &who).is_some() && settled(),
        wall: upgrade_drive::named_standing_wall(home, agent.snap),
        started: process_started(&agent.snap.start),
    };
    account_wall_of(&accounts::Store::at(state), &roster, sid, &read, now)
}

/// The unix second a process started, from its kernel start as the snapshot
/// keeps it ([`Snapshot::start`]: `ps -o lstart=` in UTC, as Claude renders
/// `procStart`) — read by the reader the upgrade reads it with.
fn process_started(start: &str) -> Option<i64> {
    aterm_agent::harness::upgrade_models::parse_lstart(start).and_then(|s| i64::try_from(s).ok())
}

/// What [`account_wall_live`] read of a session's agent at its wall, for
/// [`account_wall_of`].
struct WallRead<'a> {
    /// The agent's exec environment, `KEY=value` entries: which login it
    /// runs under (`accounts::ProcessLogin::of_env`).
    env: &'a [String],
    /// Its SETTINGS leave it its stored login ([`WorkerIdle::settled`]): its
    /// route is the default one, no settings source gives it a credential,
    /// a profile or an `apiKeyHelper`, and the vendor's profile store holds
    /// no config. `false` makes it no stored login, whatever its environment
    /// said.
    settled: bool,
    /// Its conversation's standing quota wall; `None`: none stands.
    wall: Option<aterm_agent::harness::upgrade::StandingWall>,
    /// The unix second its process started; `None`: unread.
    started: Option<i64>,
}

/// THE RECORDER'S CALL, ASSEMBLED ([`account_wall_live`]) — over what was
/// read ([`WallRead`]), so it reads nothing itself but the accounts store,
/// and that only through `accounts::record_wall`, whose word is answered. An
/// environment with nothing in it is `no-env` (below), no wall standing is
/// `no-wall`, and a process whose start cannot be told is `no-start`: a wall
/// nobody can place before or after its process is recorded for nobody. A
/// store whose lock stayed held past the recorder's bounded wait is
/// [`WALL_LOCK_HELD`]: nothing was read, and the recorder is asked again at
/// the session's next decision ([`wall_asked_again`]), as it is for
/// `no-wall`.
///
/// The label is the roster row the process's login names, answered wherever
/// the recorder judged the wall as that account's — recorded, on record
/// already, older than the process, or refused by the store — and never for
/// a process that is no row's, or no stored login.
fn account_wall_of(
    store: &accounts::Store,
    roster: &accounts::Roster,
    sid: &str,
    read: &WallRead<'_>,
    now: i64,
) -> (&'static str, Option<String>) {
    // AN ENVIRONMENT THAT READS EMPTY WAS NOT READ: the kernel hands none back
    // for a binary it does not show (measured 2026-10-01: an Apple platform
    // binary's reads empty, a Claude Code's whole), and no process that could
    // sign in has none. Read as "no variable set" it is the default login.
    let Some(who) = accounts::ProcessLogin::of_exec_env(read.env) else {
        return ("no-env", None);
    };
    let who = accounts::ProcessLogin {
        stored_login: who.stored_login && read.settled,
        ..who
    };
    let Some(wall) = read.wall.as_ref() else {
        return ("no-wall", None);
    };
    let Some(started) = read.started else {
        return ("no-start", None);
    };
    let recorded = accounts::record_wall(store, roster, sid, &who, wall, started, now);
    let label = match recorded {
        accounts::Recorded::NotListed | accounts::Recorded::NotStored => None,
        _ => who.me(roster).account.map(str::to_owned),
    };
    let word = if recorded.lock_held() {
        WALL_LOCK_HELD
    } else {
        recorded.as_str()
    };
    (word, label)
}

/// THE PRODUCT'S [`Acts::account_usage`]: what a session's screen says of its
/// account's usage, recorded for the account its Claude Code runs on — under
/// the home Claude's records live in, the harness state root and the roster
/// beside the `aterm.toml` this window loads, on this machine's clock and
/// zone. Without the state or the clock a word and nothing more (`no-state`,
/// `no-clock`). The roster as [`account_wall_at`] reads it, through this
/// machine's logins ([`usage_roster`]): refused, `roster-refused`; listing an
/// account, its rows only where `listed` (`[harness] accounts` is not `off`,
/// else `off`); listing none, the machine's own row under `[harness]
/// usage_notes` ([`WalledAgent::own`]), else `not-listed` — and then nothing
/// of the agent is read. Then its exec
/// environment and its settings side, as the wall's recorder reads them
/// (`no-env`), and the instant what the screen says was true: the newest
/// answer in its transcript that its screen can have been drawn after
/// (`accounts::NOTICES_DRAWN_S`; one bounded read of its tail, `no-answer`
/// without one, or with no home), at which the notices' resets are placed —
/// handed to `accounts::record_usage`, whose word is answered.
fn account_usage_live(
    home: Option<&Path>,
    sid: &str,
    agent: &WalledAgent<'_>,
    settled: &dyn Fn() -> bool,
    seen: &UsageSeen,
    listed: bool,
) -> &'static str {
    let Some(state) = harness_state() else {
        return "no-state";
    };
    let Some(now) = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()) else {
        return "no-clock";
    };
    let store = accounts::Store::at(&state);
    let roster = match usage_roster(
        accounts::default_roster_path().as_deref(),
        &store,
        (agent.own, listed),
        now,
    ) {
        Ok(roster) => roster,
        Err(word) => return word,
    };
    let Some(args) = atpkg::caller_shell::process_args(agent.snap.pid) else {
        return "no-env";
    };
    let answered = |by: i64| {
        let by = u64::try_from(by).ok()?;
        home.and_then(|home| upgrade_drive::named_last_answer(home, agent.snap, by))
            .and_then(|at| i64::try_from(at).ok())
    };
    let said = |at: i64| usage_said(seen, at, crate::presence::local_offset_at(at).unwrap_or(0));
    let read = UsageRead {
        env: &args.env,
        settled,
        answered: &answered,
        said: &said,
    };
    account_usage_of(&store, &roster, sid, &read, now)
}

/// What `seen` says, as true at `at` in the zone `offset` seconds east of
/// UTC: the notices' resets placed at that instant, as they were printed.
fn usage_said(seen: &UsageSeen, at: i64, offset: i64) -> accounts::UsageSaid {
    let place = |text: &str| {
        aterm_agent::supervise::limit::parse_reset(text).map(|spec| {
            aterm_agent::supervise::limit::reset_at_with(
                &spec,
                at,
                offset,
                &aterm_agent::supervise::limit::zone_offset_at,
            )
        })
    };
    accounts::UsageSaid {
        at,
        day: (at + offset).div_euclid(86_400),
        credits: seen.credits.as_ref().map(|c| accounts::CreditsRead {
            until: c
                .started
                .as_ref()
                .and_then(|s| s.reset.as_deref())
                .and_then(place),
        }),
        warning: seen.warning.as_ref().map(|w| accounts::WarningRead {
            weekly: w.window == aterm_phase::credits::Window::Weekly,
            pct: u32::from(w.pct),
            reset: w.reset.as_deref().and_then(place),
        }),
    }
}

/// The roster a session's usage is recorded against ([`account_usage_live`]),
/// or the word for why there is none: the file at `path` as this machine's
/// logins bind it at `now` (`accounts::Store::bound`, as at a wall: a renewed
/// login is its row's, and the one it replaced is that row's account on a
/// superseded login), where it lists accounts and `[harness] accounts` is
/// not `off` (`listed`) — a `logins.json` that cannot be read as this
/// build's refuses it, as everywhere identity is attributed; none listed,
/// the own row under `[harness] usage_notes` (`own`). The switch off reads
/// no `logins.json`.
fn usage_roster(
    path: Option<&Path>,
    store: &accounts::Store,
    (own, listed): (bool, bool),
    now: i64,
) -> Result<accounts::Roster, &'static str> {
    match path.map(accounts::Roster::load) {
        Some(Err(_)) => Err("roster-refused"),
        Some(Ok(roster)) if !roster.rows.is_empty() => {
            if !listed {
                return Err("off");
            }
            store.bound(&roster, now).map_err(|_| "roster-refused")
        }
        _ if own => Ok(accounts::Roster::own()),
        _ => Err("not-listed"),
    }
}

/// What [`account_usage_live`] read of a session's agent and its screen.
struct UsageRead<'a> {
    /// The agent's exec environment, as [`WallRead::env`].
    env: &'a [String],
    /// Its settings leave it its stored login, as [`WallRead::settled`]:
    /// asked only for a process a row of the roster names
    /// ([`account_usage_of`]).
    settled: &'a dyn Fn() -> bool,
    /// The newest answer in its transcript written by the second it is
    /// handed ([`upgrade_drive::named_last_answer`]), read as `settled` is:
    /// only for a process a row names.
    answered: &'a dyn Fn(i64) -> Option<i64>,
    /// What the screen says, as true at an instant ([`usage_said`]).
    said: &'a dyn Fn(i64) -> accounts::UsageSaid,
}

/// THE USAGE RECORDER'S CALL, ASSEMBLED ([`account_usage_live`]): over what
/// was read, so it reads nothing itself but the accounts store — and the
/// agent's settings side and its transcript's tail, asked only for a process
/// whose environment alone leaves it a stored login a row of the roster
/// names (as [`account_wall_at`] asks the first): the loop tells what the
/// screen says of usage at every change, for every Claude Code. An
/// environment that reads empty was not read (`no-env`), as for a wall; a
/// transcript with no answer gives nothing an instant (`no-answer`).
fn account_usage_of(
    store: &accounts::Store,
    roster: &accounts::Roster,
    sid: &str,
    read: &UsageRead<'_>,
    now: i64,
) -> &'static str {
    let Some(who) = accounts::ProcessLogin::of_exec_env(read.env) else {
        return "no-env";
    };
    let who = accounts::ProcessLogin {
        stored_login: accounts::judged_label(roster, &who).is_some() && (read.settled)(),
        ..who
    };
    // A process the record is not kept for is the recorder's to word: the
    // instant it is handed is never read. The screen was read just before
    // `now`: an answer written in the seconds before may not show on it yet.
    let at = if accounts::judged_label(roster, &who).is_some() {
        match (read.answered)(now.saturating_sub(accounts::NOTICES_DRAWN_S)) {
            Some(at) => at,
            None => return "no-answer",
        }
    } else {
        now
    };
    accounts::record_usage(store, roster, sid, &who, &(read.said)(at), now).as_str()
}

/// THE PRODUCT'S [`Acts::account_gone`]: the session is forgotten by the
/// accounts recorder (`accounts::record_gone`), whatever agent ran in it —
/// the recorder forgets only what it has on record, and where no accounts
/// state was ever written it reads, makes and locks nothing. One line of the
/// log when something was forgotten, or could not be (the reason names
/// files, and stays out of it).
fn account_gone_live(sid: &str) {
    let Some(state) = harness_state() else {
        return;
    };
    let Some(now) = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()) else {
        return;
    };
    match accounts::record_gone(&accounts::Store::at(&state), sid, now) {
        Ok(true) => aterm_log::info!("harness @{sid}: account session forgotten"),
        Ok(false) => {}
        Err(_) => aterm_log::info!("harness @{sid}: account session not forgotten: refused"),
    }
}

// ---------------------------------------------------------------------------
// The account's move (the accounts design, §6): the places it reads, what it
// reads of an agent, and the roster's advice as the policy takes it. The
// transaction itself is the worker's ([`WorkerIdle::account_move`]).
// ---------------------------------------------------------------------------

/// WHERE THE ACCOUNTS LIVE FOR THIS HOST, AND ITS CLOCK
/// ([`Acts::account_seat`]): every place the move reads and the time, so a
/// test hands it a temp roster, a temp store and a fixed second.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountSeat {
    /// The roster's file, beside the `aterm.toml` the window loads; `None`
    /// where no config path is known — then there is no roster.
    pub(crate) roster: Option<PathBuf>,
    /// The harness state root the accounts store is under
    /// (`accounts::Store::at`).
    pub(crate) state: PathBuf,
    /// The user's home: a session's standing wall and reset line are read
    /// from its transcript in its agent's own records, which are under it
    /// only where the agent's snapshot names no Claude home of its own
    /// (`upgrade_drive::named_standing_wall`).
    pub(crate) home: PathBuf,
    /// Now, unix seconds.
    pub(crate) now: i64,
}

/// THE PRODUCT'S [`Acts::account_seat`]: the roster beside the `aterm.toml`
/// this window loads, the harness state root, the home Claude's records live
/// under and this machine's clock. `None` without a home, a state root or a
/// clock. It reads the environment and the clock, and no file.
fn account_seat_live(home: Option<&Path>) -> Option<AccountSeat> {
    Some(AccountSeat {
        roster: accounts::default_roster_path(),
        state: harness_state()?,
        home: home?.to_path_buf(),
        now: aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok())?,
    })
}

/// Whether the process an account move's relaunch brought up runs under the
/// target ([`WorkerIdle::account_applied`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Applied {
    /// Its own environment names the target.
    Yes,
    /// Its environment was READ and names another login, or none of the
    /// roster's: the move was not applied.
    No,
    /// Nothing of it could be read: nobody knows, and nothing is taken back.
    Unread,
}

/// What the exec environment of process `pid` says of its login
/// ([`Acts::account_env`], one read; `accounts::ProcessLogin::of_exec_env`).
/// `None`: it cannot be read, or reads EMPTY — which was not read, and is
/// never the default login.
fn exec_login(hooks: &Hooks, pid: u32) -> Option<accounts::ProcessLogin> {
    (hooks.acts.account_env)(pid)
        .as_deref()
        .and_then(accounts::ProcessLogin::of_exec_env)
}

/// ONE SHORT TRANSACTION on the accounts state for session `sid`
/// (`accounts::Store::transact_within`: the lock, the read-edit-write, the
/// ledger rows, the wake marker where a row says so), on the seat's clock
/// read afresh — a relaunch may have run since the seat was read. `edit`
/// gets the state and that second. Every row it answers carries the
/// session's id where it names none. NEVER CALLED WITH ANOTHER LOCK HELD,
/// and `edit` waits on nothing ([`WorkerIdle::account_move`] says the order).
///
/// THE LOCK IS WAITED FOR A BOUNDED TIME (`accounts::LOCK_LADDER`, about two
/// seconds in all): every caller is a session's loop or a host thread, and
/// one that parked on a lock another process holds — a hand-run `aterm
/// harness accounts` stopped in a debugger, a process frozen mid-write —
/// would stop supervising its tab. Held past that, the answer is
/// `accounts::LOCK_HELD` and nothing was read: the caller says it could not
/// be done now, and is asked again.
fn account_transact_of(
    sid: &str,
    hooks: &Hooks,
    seat: &AccountSeat,
    edit: impl FnOnce(&mut accounts::State, i64) -> Result<Vec<accounts::Event>, String>,
) -> Result<Vec<accounts::Event>, String> {
    let now = (hooks.acts.account_seat)().map_or(seat.now, |seat| seat.now);
    let mut note = None;
    accounts::Store::at(&seat.state).transact_within(
        &accounts::LOCK_LADDER,
        now,
        &mut note,
        |state| {
            Ok(edit(state, now)?
                .into_iter()
                .map(|event| match event.sid {
                    Some(_) => event,
                    None => event.with_sid(sid),
                })
                .collect())
        },
    )
}

/// [`account_transact_of`] over the state AND the logins
/// (`accounts::Store::transact_logins_within`): the same bounded take, the
/// same session id on every row.
fn login_transact_of(
    sid: &str,
    hooks: &Hooks,
    seat: &AccountSeat,
    edit: impl FnOnce(
        &mut accounts::State,
        &mut accounts::Logins,
        i64,
    ) -> Result<Vec<accounts::Event>, String>,
) -> Result<Vec<accounts::Event>, String> {
    let now = (hooks.acts.account_seat)().map_or(seat.now, |seat| seat.now);
    let mut note = None;
    accounts::Store::at(&seat.state).transact_logins_within(
        &accounts::LOCK_LADDER,
        now,
        &mut note,
        |state, logins| {
            Ok(edit(state, logins, now)?
                .into_iter()
                .map(|event| match event.sid {
                    Some(_) => event,
                    None => event.with_sid(sid),
                })
                .collect())
        },
    )
}

/// AN ACCOUNT MOVE WHOSE RELAUNCH NOBODY SAW LAND, LOOKED AT AGAIN
/// ([`AccountKept::flying`]; the accounts design, §6): the loop that asked
/// for the move was told to wait — its relaunch was left in flight for the
/// host's carry, or the process it brought up could not be read — so
/// nothing told it the move was made. Called where the carry lands
/// ([`relaunch_until`]) and at the session's next ask for advice
/// ([`WorkerIdle::account`]), with the snapshot the host keeps:
///
/// * still the process before the move, or none read — the tab is read
///   once more here ([`refresh_snapshot`]: what was kept may be the read
///   from before the move) — or an environment that cannot be read: nothing
///   yet, asked again;
/// * ANOTHER PROCESS, WHOSE OWN ENVIRONMENT NAMES THE TARGET: A MADE MOVE,
///   like any other. It is kept until it is answered
///   ([`AccountKept::moved`], with the login before it for the way back),
///   the session is on record on the target, and ITS LOOP LEARNS OF IT
///   ([`Landing`]) — so that loop counts the move and tells what answered
///   it ([`IdleHost::account_served`], [`IdleHost::account_answered`]), and
///   the account is proven or struck on this session's word instead of
///   waiting out its canary's hold;
/// * another process, read, under ANOTHER login: the move was not applied —
///   the rotation is taken back where this session is still its canary
///   (`accounts::Strike::NotMoved`: nothing held against the account), and
///   the session is on record where it runs.
///
/// Decided once: the record is taken as it is decided. With no roster, or
/// where nothing of the accounts acts ([`Switches::accounts_live`],
/// [`WorkerIdle::accounts_act`]: `accounts = "off"` with `logins = "off"`,
/// or `off` over a roster with no row a renewal restores), no move was ever
/// left flying and this reads one lock and nothing else.
fn account_land(sid: &str, kept: &Mutex<Kept>, hooks: &Hooks, landing: Landing) {
    let Some(flying) = with_kept_of(kept, |k| k.account.flying.clone()) else {
        return;
    };
    // The process the tab holds, if it is another than the one before the
    // move.
    let other = || {
        with_kept_of(kept, |k| k.snapshot.clone())
            .filter(|s| (s.pid, s.start.as_str()) != (flying.pid, flying.start.as_str()))
    };
    let Some(snap) = other().or_else(|| {
        refresh_snapshot(sid, kept, hooks);
        other()
    }) else {
        return;
    };
    let Some(seat) = (hooks.acts.account_seat)() else {
        return;
    };
    let Some(roster) = account_roster(&seat) else {
        // No roster any more: nothing to land on, and nothing to take back.
        with_kept_of(kept, |k| k.account.flying = None);
        return;
    };
    let Some(who) = exec_login(hooks, snap.pid) else {
        return;
    };
    let on = who.me(&roster).account.map(str::to_owned);
    // Taken once, and only while it is still the move that was read.
    let taken = with_kept_of(kept, |k| {
        k.account.flying.take_if(|f| *f == flying).map(|_| {
            if on.as_deref() == Some(flying.to.as_str()) {
                k.account.held = None;
                k.account.unmade = None;
                k.account.moved = Some(Moved {
                    to: flying.to.clone(),
                    login: flying.login.clone(),
                    pid: snap.pid,
                    start: snap.start.clone(),
                    untold: landing == Landing::Asked,
                    answered: false,
                });
            }
        })
    });
    if taken.is_none() {
        return;
    }
    let to = flying.to.as_str();
    let landed = on.as_deref() == Some(to);
    if landed && landing == Landing::Carried {
        (hooks.acts.account_made)(sid, to, flying.at);
    }
    let done = account_transact_of(sid, hooks, &seat, |state, now| {
        let mut events = Vec::new();
        if !landed {
            let why = accounts::Strike::NotMoved("account-not-applied".to_string());
            events.extend(accounts::strike(state, to, sid, &why, now));
        }
        if let Some(on) = on.as_deref() {
            events.extend(accounts::note_session(state, sid, on, None, now));
        }
        Ok(events)
    });
    let word = match (landed, done.is_ok()) {
        (true, true) => "landed",
        (true, false) => "landed: not recorded",
        (false, true) => "not applied: taken back",
        (false, false) => "not applied: not recorded",
    };
    aterm_log::info!("harness @{sid}: account move to {to}: {word}");
}

/// HOW THE SESSION'S LOOP LEARNS OF A MOVE THAT LANDED AFTER ITS ASK WAS
/// ANSWERED `wait:…` ([`account_land`]): by whichever way reaches the loop
/// that will answer for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Landing {
    /// THE HOST'S CARRY LANDED IT, between two loops (the agent left its
    /// tab, so its loop ended, and none has started on the relaunched one
    /// yet): the move's row is written where the loop that TAKES the agent
    /// reads it as it starts ([`Acts::account_made`], `approvals::
    /// account_moves`).
    Carried,
    /// IT WAS READ AT AN ASK OF A LOOP THAT IS RUNNING — the one told
    /// `wait:account-unverified`, or one whose agent was relaunched under it
    /// without ever being seen gone. A row written now would be read by no
    /// loop until the next one starts (and the running loop appends to the
    /// same file), and this one would wait out the limit its session has
    /// already left; it is told through the move it asked for instead
    /// ([`Moved::untold`]), and writes that row itself.
    Asked,
}

/// [`account_land`] where an agent that left its tab is back in it
/// ([`relaunch_until`], [`Landing::Carried`]): for a Claude Code, and never
/// where nothing of the accounts acts ([`Switches::accounts_live`], read
/// live: `accounts = "off"` with `logins = "off"`) — the carry lands
/// whatever the switches say (its line is the record's), and nothing of the
/// move is then recorded.
fn account_land_for(job: &WorkerJob, hooks: &Hooks) {
    if accounts_for(job.agent) && job.switches.accounts_live() {
        account_land(&job.sid, &job.kept, hooks, Landing::Carried);
    }
}

/// THE PRODUCT'S [`Acts::account_made`], under the aterm state root `root`:
/// one row appended to session `sid`'s approval ledger — the file its
/// supervisor's loop keeps ([`hosted_opts`]) — in the loop's own words for a
/// move it made (`account-rotate@v1`, `typed`, `relaunch under account
/// <label>`, `approvals::account_switch_reason`). The LABEL and nothing else
/// of the account. Whether it was written. A loop that starts after it
/// reads it as its own (`approvals::account_moves`): the move counts toward
/// the session's bound, and is still to be answered.
fn account_made_at(root: &Path, sid: &str, to: &str, at: AccountAt) -> bool {
    use aterm_agent::supervise::approvals;
    use aterm_agent::supervise::policy::turn_end::RULE_ACCOUNT;
    let Some(path) = approvals::path_under(root, Some(sid)) else {
        return false;
    };
    let mut refused = Vec::new();
    let mut ledger = approvals::Ledger::open(Some(&path), Some(sid), &mut refused);
    ledger.write(
        &approvals::Row {
            rule_id: RULE_ACCOUNT,
            outcome: approvals::Outcome::Typed,
            command: &format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND),
            reason: &approvals::account_switch_reason(to, at),
            box_seq: 0,
        },
        &mut refused,
    );
    // The ledger says what it could not do on its warning stream.
    refused.is_empty()
}

/// THE LOGIN A RELAUNCH WITH NO PROCESS TO READ CARRIES, over the places it
/// is given ([`Acts::account_cold`]; `relaunch::cold_login`, the accounts
/// design §6): the machine's proven active account's, for the Claude Code a
/// reopened layout brought back as `snap` — whose leaf named neither a login
/// nor a Claude home of its own ([`RestoredAgent::unread`]; a leaf that names
/// them is relaunched under them and never asks this), so nothing says which
/// login it ran under. In this order, each `None`
/// (NOTHING IS ADDED: the relaunch as it was before accounts existed):
///
/// 1. the agent is a Claude Code with a conversation named (a Codex has no
///    account here);
/// 2. the roster lists two or more accounts — else nothing more is read;
/// 3. the state, WITHOUT the store's lock, names an active account that is
///    proven, enrolled and not known signed out (`relaunch::cold_login`);
/// 4. THE LEAF'S CLAUDE HOME IS THE DEFAULT ONE, on the two things that can
///    say so with no process to read:
///    * ITS CONVERSATION IS FILED UNDER THE DEFAULT HOME
///      (`upgrade_drive::conversation_under`): an agent under another home —
///      an aterm identity's, a `CLAUDE_CONFIG_DIR` of its shell's — files
///      its conversations there, and is not the roster's to place;
///    * and the exec environment of the shell the line is typed at
///      (`shell_env`, read only now), WHERE IT CAN BE READ, names no Claude
///      home, no login of its own and no credential, route or profile.
///
/// THE SHELL'S ENVIRONMENT OFTEN CANNOT BE READ: the kernel hands none back
/// for an Apple platform binary (measured 2026-10-01: `/bin/zsh` reads
/// empty, as `/usr/bin/perl` does), so for the stock shell of a Mac it says
/// nothing and the conversation's place decides alone — required to be
/// readable, this rule added nothing on any such machine. What a shell's
/// own startup files export is in no exec environment either, and is not
/// seen: a login variable set there is overridden by the line's.
///
/// What the line then carries is this feature's choice, not the launch's:
/// where it cannot ride the line, the relaunch is made without it
/// ([`relaunch_cold`]).
fn account_cold_at(
    seat: &AccountSeat,
    snap: &Snapshot,
    shell_env: impl FnOnce() -> Option<Vec<String>>,
) -> Option<String> {
    if !accounts_for(snap.agent().program()) {
        return None;
    }
    let session = snap.session.as_deref()?;
    let roster = account_roster(seat).filter(|roster| roster.rows.len() >= 2)?;
    let (state, _) = accounts::Store::at(&seat.state).load(seat.now).ok()?;
    let enrolled = accounts::enrolled(&roster);
    // The account first: with none to carry, nothing of the home is listed.
    relaunch::cold_login(&roster, &state, &enrolled, true)?;
    let default_home = upgrade_drive::conversation_under(&seat.home, session);
    let login = relaunch::cold_login(&roster, &state, &enrolled, default_home)?;
    let quiet = shell_env().is_none_or(|env| {
        accounts::ProcessLogin::of_exec_env(&env).is_none_or(|who| {
            let own = env
                .iter()
                .filter_map(|kv| kv.split_once('='))
                .any(|(name, _)| name == accounts::LOGIN_ENV);
            who.default_home && who.stored_login && !own
        })
    });
    quiet.then_some(login)
}

/// ONE STEP OF A COLD RELAUNCH UNDER `login` ([`Acts::account_cold`]'s
/// answer; `None`: the step as it ever was, `act` on the snapshot as the
/// layout carried it). The login rides the snapshot, so the line carries it
/// as every relaunch line carries its login — and a line that CANNOT carry
/// it (`refused:line`: a fish tab whose shell left the agent's directory, a
/// directory with a control character) is typed without it, where a
/// process's own login is refused rather than dropped: this login is the
/// roster's choice for a tab nobody could read, and a tab not brought back
/// at all is worse than one on its shell's login.
fn relaunch_cold(
    snap: &Snapshot,
    login: Option<String>,
    act: &dyn Fn(&Snapshot) -> String,
) -> String {
    let Some(login) = login else {
        return act(snap);
    };
    let step = act(&Snapshot {
        login: Some(login),
        ..snap.clone()
    });
    if step == "refused:line" {
        return act(snap);
    }
    step
}

/// THE ROSTER OF `seat`, WHERE IT LISTS AN ACCOUNT ([`roster_read`]). `None`
/// — and then nothing of any agent is read and nothing is made on disk —
/// where it lists none, and where it does not admit: a refused roster rotates
/// nothing.
fn account_roster(seat: &AccountSeat) -> Option<accounts::Roster> {
    roster_read(seat).ok()
}

/// THE ROSTER A LIMIT RESET, OR A USAGE TAP'S RECORD, IS JUDGED AGAINST
/// ([`roster_read`]): the file's rows where it lists an account; with `on`
/// (`[harness] limit_reset`, read live by the caller; always, for the tap's
/// [`WorkerIdle::tap_label`]), THE MACHINE'S OWN ROW (`accounts::Roster::own`)
/// where no path is known, the file is missing or it lists none — neither
/// needs a roster, and a claim, a clearing, a reading and the ledger's rows
/// all need a label. `None` where the file does not admit, whatever `on`
/// says: a refused roster resets and records nothing. With `on` false it is
/// [`account_roster`].
fn account_roster_or_own(seat: &AccountSeat, on: bool) -> Option<accounts::Roster> {
    match roster_read(seat) {
        Ok(roster) => Some(roster),
        Err(AccountsSaid::Nothing) if on => Some(accounts::Roster::own()),
        Err(_) => None,
    }
}

/// ONE READ OF `seat`'s ROSTER (one bounded read of its file): the roster
/// where it lists an account, else what that says of the accounts as a whole
/// — [`AccountsSaid::Nothing`] where no path is known, the file is missing
/// (the packaged state) or lists none, [`AccountsSaid::Refused`] where it
/// does not admit. The refusal's reason (which names the file) is dropped
/// here: it is `aterm harness accounts`' to say, never a log's or the
/// window's.
///
/// THROUGH THIS MACHINE'S LOGINS (`accounts::Store::bound`, one bounded
/// read of `logins.json` where the roster lists an account): a row a
/// renewal bound reads its renewed login, so every move, relaunch and
/// judgment made on it is made on that login, and a process still on the one
/// it replaced is that row's account on a superseded login.
fn roster_read(seat: &AccountSeat) -> Result<accounts::Roster, AccountsSaid> {
    let Some(path) = seat.roster.as_deref() else {
        return Err(AccountsSaid::Nothing);
    };
    match accounts::Roster::load(path) {
        Ok(roster) if roster.rows.is_empty() => Err(AccountsSaid::Nothing),
        Ok(roster) => accounts::Store::at(&seat.state)
            .bound(&roster, seat.now)
            .map_err(|_| AccountsSaid::Refused),
        Err(_) => Err(AccountsSaid::Refused),
    }
}

/// THE LOOK AT THE ACCOUNTS AS A WHOLE ([`AccountsSaid`]) under the
/// rotation's switch (`rotation`: `[harness] accounts` not off), the login
/// lifecycle's (`logins`) and the usage record's (`notes`: `[harness]
/// usage_notes`, read live), from `seat`'s files as they are. With all
/// three off NO FILE IS READ and there is nothing to say: the owner's
/// switches end every line. Else the roster ([`roster_read`]): where it
/// lists no account and `notes` is on, the machine's own row
/// (`accounts::Roster::own`) stands for it, for its usage alone, whatever
/// the other two say; one that lists an account says nothing with the
/// rotation and the lifecycle off, nor with the rotation off and no row
/// the lifecycle renews. Then the state WITHOUT the store's lock (it is
/// replaced by a rename) and one `read_dir` per row (`accounts::enrolled`):
/// what [`AccountsSaid::listed`] names. `None` where that state WAS NOT
/// READ WHOLE: it cannot be read, or its file held nothing this build
/// reads and the store answered an empty state beside a note of it
/// (`accounts::Store::load`: a file cut short). An empty state says nobody
/// is signed out, which that file did not say: nothing is told, and a line
/// that stands stays.
fn accounts_said(
    rotation: bool,
    logins: bool,
    notes: bool,
    seat: &AccountSeat,
) -> Option<AccountsSaid> {
    if !rotation && !logins && !notes {
        return Some(AccountsSaid::Nothing);
    }
    let roster = match roster_read(seat) {
        Err(AccountsSaid::Nothing) if notes => accounts::Roster::own(),
        _ if !rotation && !logins => return Some(AccountsSaid::Nothing),
        // The login lifecycle alone speaks only for a roster it renews in.
        Ok(roster) if !rotation && !renewable_listed(&roster) => {
            return Some(AccountsSaid::Nothing);
        }
        Ok(roster) => roster,
        Err(said) => return Some(said),
    };
    let store = accounts::Store::at(&seat.state);
    let (state, None) = store.load(seat.now).ok()? else {
        return None;
    };
    let (logins, None) = store.load_logins(seat.now).ok()? else {
        return None;
    };
    Some(AccountsSaid::listed(&roster, &state, &logins, seat.now))
}

/// TELL THE WINDOW WHAT THE ACCOUNTS SAY, ONCE PER CHANGE
/// ([`HostNote::Accounts`]): `said` is told where it is not what was told
/// last ([`Switches::accounts_said`]). The window's two lines are idempotent,
/// so a repeat would cost a wake of the app loop per turn end and nothing
/// else.
///
/// `again` (the host's slow look, [`activation_loop`]): a line that stands
/// ([`AccountsSaid::stands`]) is said though nothing changed, as every host
/// reporter says its fact at each look. The digest decides by its own rules
/// on that word: it restates the title and the detail a change left behind
/// (words that moved inside its restate quiet), and brings back a line whose
/// dismissal has run out or that aged out. Where no line stands there is
/// nothing to say again.
///
/// NO LOCK ORDERS THE TELLERS, so two that look at once may tell in the
/// other order than they read. A teller that finds another's word in place
/// of its own after its note went out forgets what was told, and the next
/// look tells again whatever it reads: the window is never left on the older
/// of two looks.
fn accounts_tell(switches: &Switches, hooks: &Hooks, said: AccountsSaid, again: bool) {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    said.hash(&mut hasher);
    // 0 is "none told yet".
    let hash = hasher.finish().max(1);
    let told = switches.accounts_said.swap(hash, Ordering::SeqCst) == hash;
    if told && !(again && said.stands()) {
        return;
    }
    (hooks.note)(HostNote::Accounts(said));
    if switches.accounts_said.load(Ordering::SeqCst) != hash {
        switches.accounts_said.store(0, Ordering::SeqCst);
    }
}

/// THE HOST'S OWN LOOK AT THE ACCOUNTS, TOLD ([`accounts_said`] under the
/// mode read live, [`accounts_tell`] with `again`): at its start, which ends
/// a line an earlier process left open, at each wake of the activation thread
/// after it ([`activation_loop`]), and at news of the accounts
/// ([`HostHandle::note_accounts`]). With the rotation and the login lifecycle
/// off ([`Switches::accounts_live`]) and `[harness] usage_notes` off too,
/// there is nothing to say and no place is asked for; else a host with no
/// seat looks at nothing. A mute host ([`Switches::mute`]) tells nothing at
/// all.
fn accounts_look(switches: &Switches, hooks: &Hooks, again: bool) {
    if switches.mute() {
        return;
    }
    let rotation = switches.accounts() != accounts::Mode::Off;
    let notes = switches.notes();
    let said = if switches.accounts_live() || notes {
        (hooks.acts.account_seat)()
            .and_then(|seat| accounts_said(rotation, switches.logins(), notes, &seat))
    } else {
        Some(AccountsSaid::Nothing)
    };
    if let Some(said) = said {
        accounts_tell(switches, hooks, said, again);
    }
}

/// What a worker read of its Claude Code for an account judgment
/// ([`WorkerIdle::account_look`]).
struct AccountLook {
    /// What its own exec environment says of its login, and its route.
    who: accounts::ProcessLogin,
    /// The roster's label for that login — the row whose login it is, or
    /// the row a renewal moved off it (`accounts::Me::label`); `None`: no
    /// row names it.
    label: Option<String>,
    /// The snapshot it was read from: its process, its program, its
    /// conversation, and the raw login the relaunch line re-applies.
    snap: Snapshot,
}

impl AccountLook {
    /// THE ACCOUNT THE ROSTER'S JUDGMENTS ARE ABOUT, for this process
    /// (`accounts::judged_label` over `roster`, the one the look was read
    /// against): its label, where it runs on a listed, STORED login in the
    /// DEFAULT home — the recorder's rule (`accounts::record_wall`) and
    /// `accounts::advise`'s first. `None` for a process with a credential or
    /// a route of its own in its environment, or a home of its own: its
    /// environment may name no login, which reads as the row with no
    /// directory, and what it says — a served turn, a lost login — is said
    /// of no account.
    fn judged<'a>(&self, roster: &'a accounts::Roster) -> Option<&'a str> {
        accounts::judged_label(roster, &self.who)
    }
}

/// Whether `roster` lists a row the login lifecycle renews: one with an
/// email. PURE.
fn renewable_listed(roster: &accounts::Roster) -> bool {
    roster.rows.iter().any(|row| row.email.is_some())
}

/// THE RENEWABLE ROW WHOSE OWN LOGIN THE PROCESS HOLDS: a row with an email
/// whose login — through this machine's bindings — is the very one the
/// process runs on (`accounts::Me::account`, never one a renewal
/// superseded: what is seen of a superseded login is no news of the renewed
/// one), on a stored login in the default home. PURE.
fn renewable_own(roster: &accounts::Roster, look: &AccountLook) -> Option<String> {
    let me = look.who.me(roster);
    let label = me.account.filter(|_| me.default_home && me.stored_login)?;
    roster
        .row(label)
        .filter(|row| row.email.is_some())
        .map(|row| row.label.clone())
}

/// A usage wall that is THIS PROCESS'S OWN ([`own_wall`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnWall {
    /// The vendor's limit type, or `accounts::UNNAMED_WINDOW`.
    window: String,
    /// The reset it names.
    until: Option<i64>,
    /// When its row was written (never ahead of the seat's clock).
    at: i64,
}

/// THE STANDING USAGE WALL OF `snap`'s OWN PROCESS (the accounts design §5,
/// "What counts as an observation"; the recorder's rule,
/// `accounts::record_wall`): the quota rejection that stands last in its
/// conversation's transcript (one bounded read under the seat's home), where
/// its row is NOT OLDER than the process — a row in the process's first
/// second counts. `None`: no conversation named, a start that cannot be
/// read, no wall standing, or a row the process before it wrote — under
/// rotation, the wall of the account the session LEFT, still standing in a
/// transcript no model has answered since.
fn own_wall(seat: &AccountSeat, snap: &Snapshot) -> Option<OwnWall> {
    match standing_wall(seat, snap) {
        Standing::Own(wall) => Some(wall),
        Standing::Unwritten | Standing::NotOwn => None,
    }
}

/// What a session's transcript says of the usage limit its screen shows
/// ([`standing_wall`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Standing {
    /// A quota rejection this process wrote stands last ([`own_wall`]).
    Own(OwnWall),
    /// NO quota rejection stands last in a transcript that could be asked
    /// for one (the conversation named, the process's start read): the
    /// limit's row is not written YET — it reaches the transcript a moment
    /// after the screen paints the notice — or never will be (a notice the
    /// resumed conversation repainted). The one answer that asking again
    /// can change ([`WorkerIdle::account_lags`]).
    Unwritten,
    /// Nothing that looking again changes: a row the process BEFORE this
    /// one wrote, no conversation named, or a start that cannot be read.
    NotOwn,
}

/// [`own_wall`]'s read, saying WHY there is none ([`Standing`]): one bounded
/// read of the transcript's tail, and none at all for a conversation not
/// named or a process whose start cannot be read.
fn standing_wall(seat: &AccountSeat, snap: &Snapshot) -> Standing {
    if snap.session.is_none() {
        return Standing::NotOwn;
    }
    let Some(started) = process_started(&snap.start) else {
        return Standing::NotOwn;
    };
    let Some(wall) = upgrade_drive::named_standing_wall(&seat.home, snap) else {
        return Standing::Unwritten;
    };
    let written = i64::try_from(wall.at).unwrap_or(i64::MAX);
    if written < started {
        return Standing::NotOwn;
    }
    Standing::Own(OwnWall {
        window: wall
            .window
            .filter(|window| !window.trim().is_empty())
            .unwrap_or_else(|| accounts::UNNAMED_WINDOW.to_string()),
        until: wall.until.and_then(|until| i64::try_from(until).ok()),
        at: written.min(seat.now),
    })
}

/// A usage limit as the loop's screen reads it: any wall but a lost login.
fn usage_limit(point: AccountPoint) -> bool {
    matches!(point, AccountPoint::Wall { kind } if kind != aterm_phase::WallKind::Auth)
}

/// THE SESSION AS THE ROSTER IS ASKED ABOUT IT AT `point`: at a usage limit
/// ([`usage_limit`]) AS ITS ACCOUNT, NOT ITS LOGIN — a login a renewal
/// superseded (`accounts::Me::left`) is read as the row's own
/// (`accounts::Me::account`). A limit is the account's, the same under every
/// login of it, so the login lifecycle's follow onto the renewed login has
/// nothing to give there (the wall's resume would be typed into the same
/// limit), and every rule of the roster reaches the session as it reaches
/// any session on that account: the owner's `accounts use`, an account
/// another session proved, the early leave, the wall's wait. The follow is
/// made at the next idle turn end. With login renewal off, this same
/// account attribution is used at every point: the existing login remains
/// the account's, while renewal itself is never a reason to move it.
/// Every ask about a point — the advice,
/// past a probe, and the commit's ([`WorkerIdle::account_move`]) — is made
/// through this, so the move the loop is advised is the one its commit
/// finds. PURE.
fn asked_at(me: accounts::Me<'_>, point: AccountPoint, logins: bool) -> accounts::Me<'_> {
    if !logins || usage_limit(point) {
        accounts::Me {
            account: me.label(),
            left: None,
            ..me
        }
    } else {
        me
    }
}

/// WHETHER THE EVIDENCE OF THE LIMIT ON A SESSION'S SCREEN LAGS IT, so that
/// the decision is worth making again a moment on
/// ([`WorkerIdle::account_lags`]): at a usage limit, for a process the
/// roster's judgments are about (`judged`: a listed, stored login in the
/// default home — another's limit is no account's, and nobody waits for its
/// row), where the transcript holds no limit row yet (`unwritten`,
/// [`Standing::Unwritten`]) or the recorder, asked at this decision, could
/// not take the store's lock (`owed`, [`WorkerIdle::account_wall_again`]).
/// PURE.
fn evidence_lags(point: AccountPoint, judged: bool, unwritten: bool, owed: bool) -> bool {
    usage_limit(point) && judged && (unwritten || owed)
}

/// THE LOOP'S POINT AS THE ROSTER IS ASKED ABOUT IT, with the wall it stands
/// on: a turn end; a lost login right after the session's own move (a wall
/// for the roster too — the session cannot run where it is — with no usage
/// wall behind it), met at `seen` ([`WorkerIdle::lost_seen`]; read at no
/// other point); a renewable login's own end, no move behind it
/// ([`AccountPoint::LoginEnded`]: the policy, which holds the move, says
/// which lost login it asks about); or a usage limit, which is the roster's
/// WALL only where the limit is this process's own ([`own_wall`]).
///
/// A LIMIT THAT IS ON THE SCREEN ALONE — no row, or one the process before
/// this one wrote (the session was relaunched while the notice stood) — IS A
/// PLAIN TURN END for the roster (the design's §6: a point is a wall "this
/// process hit", or a plain turn end): no wall is recorded for it and
/// nothing rotates from it AS A WALL, and the session still FOLLOWS from it
/// — the owner's `accounts use` and an account another session proved reach
/// it as they reach every turn end, a session on a superseded login included
/// ([`asked_at`]). Advised nothing instead, a session relaunched at its limit
/// (its memory, an exit, aterm's own restart) stood there until the limit's
/// own reset, whatever the owner said. UNDER `early` a plain turn end has a
/// rotation of its own, and this point gets it like any other: the session
/// is moved AHEAD where what is ON RECORD of its account — a wall another
/// session recorded, a `/usage` sample that shows it low — makes it one to
/// leave; never on the notice itself, and never with a probe (nothing is
/// probed at a limit: [`account_advice_of`]).
fn account_point(
    seat: &AccountSeat,
    look: &AccountLook,
    point: AccountPoint,
    seen: i64,
) -> (accounts::Point, Option<OwnWall>) {
    let (at, wall, _) = account_point_read(seat, look, point, seen);
    (at, wall)
}

/// [`account_point`], and WHETHER THE LIMIT'S ROW IS NOT WRITTEN YET
/// ([`Standing::Unwritten`]): at a usage limit, with no quota rejection
/// standing in a transcript that could be asked for one. Such a limit is a
/// plain turn end for the roster like any limit that is on the screen alone
/// — and the one of them a look a moment later may find a wall at
/// ([`WorkerIdle::account_lags`]).
fn account_point_read(
    seat: &AccountSeat,
    look: &AccountLook,
    point: AccountPoint,
    seen: i64,
) -> (accounts::Point, Option<OwnWall>, bool) {
    match point {
        AccountPoint::TurnEnd => (accounts::Point::TurnEnd, None, false),
        AccountPoint::Wall {
            kind: aterm_phase::WallKind::Auth,
        } => (accounts::Point::LostLogin { seen }, None, false),
        AccountPoint::LoginEnded => (accounts::Point::LoginEnded, None, false),
        AccountPoint::Wall { .. } => match standing_wall(seat, &look.snap) {
            Standing::Own(wall) => (
                accounts::Point::Wall {
                    window: wall.window.clone(),
                    at: Some(wall.at),
                },
                Some(wall),
                false,
            ),
            Standing::Unwritten => (accounts::Point::TurnEnd, None, true),
            Standing::NotOwn => (accounts::Point::TurnEnd, None, false),
        },
    }
}

/// THE ROSTER'S ADVICE AS THE POLICY TAKES IT: [`AccountAdvice::of_roster_at`],
/// the policy's own mapping (a move is a move; the owner's clearing is a
/// resume; a wait, the machine's rotation bound and — at a wall — a canary's
/// hold are waits until their instants; every other reason to stay is
/// nothing). It lives beside the vocabulary it maps to, in
/// aterm-agent, so that the Tier-1 walk of the rotation's model decides
/// through the SAME function this host advises through
/// (`tests/supervise_conformance_account_rotation.rs`).
///
/// A DUE PROBE (`accounts::Advice::Probe`, under `early` alone) is handed on
/// WITH `past`, what the roster advises past it
/// (`accounts::advise_past_probe`, on the same state and second): at a plain
/// turn end as [`AccountAdvice::Probe`] with `past` mapped as its
/// `otherwise`; at a limit on the screen (`at_wall`: one the roster was
/// asked about as a plain turn end) as `past` alone — nothing is probed at
/// a wall. `past` is read for no other advice.
///
/// `opening` is THE ROSTER'S NEXT OPENING (`accounts::next_opening`, on the
/// state and the second the advice was asked on): a session held at its wall
/// for a canary or for the machine's bound waits until the hold's end OR
/// that instant, whichever comes first — a reset that makes the active
/// account followable ends the wait, instead of the session sitting out the
/// rest of ten minutes, or of the hour, beside an account it could run on.
fn account_advice_of(
    advice: &accounts::Advice,
    past: Option<&accounts::Advice>,
    state: &accounts::State,
    opening: Option<i64>,
    at_wall: bool,
    now: i64,
    clock: Instant,
) -> Option<AccountAdvice> {
    AccountAdvice::of_roster_at(advice, past, state, opening, at_wall, now, clock)
}

/// How old a session's row may be before the host writes it again
/// ([`WorkerIdle::account_note`]): the report's `on: @s-…` then names
/// sessions seen within the hour, and a session whose every turn end asks
/// for advice writes one row an hour.
const ACCOUNT_ROW_STALE_S: i64 = 3600;

/// What a commit that found the advice changed answers its transaction with
/// ([`WorkerIdle::account_move`]): nothing is written, and the loop is told
/// `wait:account-changed`.
const ACCOUNT_CHANGED: &str = "account-changed";

/// WHY A RELAUNCH THAT WAS NOT MADE COULD NOT BE, in the report's word
/// (`accounts::SessionRow::held`), from the relaunch's own step
/// (`relaunch::restart_under`): `jobs` — its last look found the agent not
/// at rest, which is one word (`wait:changed`) for its own background work,
/// a draft in its composer and a hand on the tab, of which the work is the
/// one that stands; `person` — a hand or a hold on the tab (`wait:held`, a
/// signal refused for one); `line` — a relaunch this launch can never be
/// given (`refused:…`: a line that cannot be typed, a one-shot run). Any
/// other step (another sweep, a socket, a record not written yet) passes by
/// itself and is no reason to print.
fn held_word(step: &str) -> Option<&'static str> {
    match step {
        "wait:changed" => Some("jobs"),
        "wait:held" => Some("person"),
        step if step.starts_with("wait:signal-") => Some("person"),
        step if step.starts_with("refused:") => Some("line"),
        _ => None,
    }
}

/// THE WAY BACK FOR A MOVE THAT ENDED AT THE SHELL (the accounts design, §6:
/// a candidate that does not answer is struck, and "nobody else was
/// relaunched"; the review's "say under which login a relaunch that ended at
/// the shell is brought back (the previous one)"). `snap` is what the host
/// kept of an agent that LEFT its tab and is about to be relaunched on exit
/// ([`on_agent_left`]). Where that very process is the one this session's
/// account move brought up, and the move has not served a turn yet
/// ([`AccountKept::moved`]), WHAT THE ACCOUNT HAS ANSWERED SINCE decides,
/// read under the store's lock:
///
/// * NOTHING, and this session is still its canary (the active account, not
///   proven, the canary's record this session's): the relaunch line carries
///   THE LOGIN THE SESSION HAD BEFORE THE MOVE — the account it is known to
///   resume under — instead of the one whose only process just left, and
///   that account is struck on this session's word
///   (`accounts::Strike::Failed`: limited on the ladder of a wall that names
///   no reset, `active` put back).
/// * IT HAS SERVED A TURN (the active account, proven: this session followed
///   the others onto it, or another session proved it since): one session's
///   exit says nothing against an account the others are served by — the
///   rule a follower's failed relaunch has ([`WorkerIdle::account_move`]).
///   Nothing is struck, and the relaunch carries the login the process had.
/// * ANYTHING ELSE — another session took the canary's place, the account
///   was struck or left since, or the state cannot be read: the login
///   before the move, and nothing struck (the session that answers for the
///   account is another one, or its word is on record already).
///
/// NEVER ONTO A LOGIN THE STORE RECORDS SIGNED OUT WHILE THE OTHER IS NOT
/// ([`account_signed_out`], read in the same transaction, after the
/// strike): a login wall is how a move learns its login is gone, so the
/// login before the move can be one struck since, and so can the one the
/// process had. The login so chosen being signed out, the relaunch carries
/// THE OTHER ONE. BOTH SIGNED OUT, THE LOGIN THE PROCESS HAD — the move's
/// own, which the session's standing move names: the relaunch is
/// SUPERVISED, never left at the shell for a person to resume under
/// whatever login that shell has. The loop that takes it holds the move
/// still to answer (`TurnEndState::seed_account`, a told move however old)
/// and waits at the lost login, `/login` never typed; with no account to
/// move to it asks the owner to sign that login in (the policy's
/// `login_wall_stuck`: `aterm harness accounts login <label>`), beside the
/// signed-out line of the accounts' digest. The sign-in is the accounts'
/// news, and the point decided again moves the session back under it
/// (`accounts::Point::LostLogin`) — or onto the login before the move,
/// should that one be signed in instead.
///
/// THE LOGIN BEFORE THE MOVE, CARRIED, IS ONE NOBODY SAW LOST: marked so
/// ([`AccountKept::came_back`]), and the first lost login its next process
/// is asked about — the moved process's `Please run /login`, repainted by
/// the conversation it resumes — is met as long ago
/// ([`WorkerIdle::lost_seen`]): the login's standing `ok` moves the session
/// back under its own login, where the wall's resume tests it.
///
/// A FOLLOW ONTO A RENEWED LOGIN — the move to the very row whose login a
/// renewal moved off the one the process had (`accounts::Row::was`) — has
/// no way back: the login before it is the superseded one. Relaunched under
/// the renewed login it had, nothing struck.
///
/// Any other exit is relaunched under the login its process had, as ever.
/// With no roster nothing was ever moved, and this reads nothing.
fn account_way_back(job: &WorkerJob, hooks: &Hooks, snap: &mut Snapshot) {
    let moved = with_kept(job, |k| {
        k.account
            .moved
            .take_if(|m| (m.pid, m.start.as_str()) == (snap.pid, snap.start.as_str()))
    });
    let Some(moved) = moved else {
        return;
    };
    // `[harness] accounts = "off"` (read live): no login is chosen by this
    // feature any more — the relaunch carries the login its process had,
    // which is that process's environment — and nothing is struck.
    if job.switches.accounts() == accounts::Mode::Off {
        return;
    }
    let seat = (hooks.acts.account_seat)();
    let roster = seat.as_ref().and_then(account_roster);
    // A FOLLOW ONTO A RENEWED LOGIN (the move was to the very row whose login
    // a renewal moved off the one the process had): nothing to go back to —
    // the login before it is the superseded one — and nothing is struck: the
    // relaunch carries the renewed login.
    let follow = roster
        .as_ref()
        .and_then(|r| r.row(&moved.to))
        .is_some_and(|row| {
            let before = moved.login.as_deref().filter(|login| !login.is_empty());
            row.was.iter().any(|was| was.as_deref() == before)
        });
    if follow {
        return;
    }
    let mut serves = false;
    // (the login before the move, the login the process had): signed out.
    let mut dead = (false, false);
    // The lock is waited for a bounded time, as every take on a host thread
    // is ([`account_transact_of`]): held past it, nothing is struck or read,
    // and the login before the move is carried.
    let struck = seat.map(|seat| {
        account_transact_of(&job.sid, hooks, &seat, |state, now| {
            let active = state.active.as_deref() == Some(moved.to.as_str());
            serves = active && state.proven;
            let canary =
                active && !state.proven && state.canary.as_ref().is_some_and(|c| c.sid == job.sid);
            let events = if canary {
                let why = accounts::Strike::Failed("exited before it answered".to_string());
                accounts::strike(state, &moved.to, &job.sid, &why, now)
            } else {
                Vec::new()
            };
            if let Some(roster) = &roster {
                let out =
                    |login: &Option<String>| account_signed_out(roster, state, login.as_deref());
                dead = (out(&moved.login), out(&snap.login));
            }
            Ok(events)
        })
    });
    if matches!(struck, Some(Err(_))) {
        aterm_log::info!(
            "harness @{}: account {} not struck: refused",
            job.sid,
            moved.to
        );
    }
    // The login it had where the account serves, else the one before the
    // move; the other where that one is signed out; the one it had where
    // both are.
    let (first, other) = if serves {
        (dead.1, dead.0)
    } else {
        (dead.0, dead.1)
    };
    let had = match (first, other) {
        (false, _) => serves,
        (true, false) => !serves,
        (true, true) => {
            aterm_log::info!(
                "harness @{}: its agent left before its move to account {} was answered, and \
                 the login before the move and the one it had are both signed out; relaunched \
                 under the one it had, to wait there for its sign-in",
                job.sid,
                moved.to
            );
            return;
        }
    };
    if had {
        aterm_log::info!(
            "harness @{}: its agent left before its move to account {} was answered; relaunched \
             under the login it had: {}",
            job.sid,
            moved.to,
            if serves {
                "the account serves"
            } else {
                "the login before the move is signed out"
            }
        );
    } else {
        snap.login.clone_from(&moved.login);
        // A login nobody saw lost: the lost login its next process may
        // repaint is the moved process's ([`AccountKept::came_back`]).
        with_kept(job, |k| k.account.came_back = Some(moved.login.clone()));
        aterm_log::info!(
            "harness @{}: its move to account {} was not answered before its agent left; \
             relaunched under the login before it",
            job.sid,
            moved.to
        );
    }
}

/// WHETHER THE STORE RECORDS `login` SIGNED OUT ([`account_way_back`]): the
/// roster's row for it (`accounts::Roster::row_for_login`) is an account
/// whose login's last word is `signed-out` — the check a relaunch with no
/// process to read makes of the login it would carry
/// (`relaunch::cold_login`). A login no row names is not the roster's to
/// judge. PURE.
fn account_signed_out(
    roster: &accounts::Roster,
    state: &accounts::State,
    login: Option<&str>,
) -> bool {
    roster.row_for_login(login).is_some_and(|row| {
        state
            .accounts
            .get(&row.label)
            .is_some_and(|a| a.auth == accounts::Auth::SignedOut)
    })
}

/// [`upgrade_drive::Opts::several_logins`] for this window's sweeps: whether
/// the roster beside the `aterm.toml` it loads lists two or more accounts
/// (`accounts::several_listed`: one bounded read of a small file) — read at
/// most once every [`SEVERAL_LOGINS_EVERY_S`], since the sweeps' options are
/// made at every act. A roster the owner just grew is seen within that long,
/// and no second login can have started in this home sooner than its
/// session's own move, which reads the roster itself. Two atomics and no
/// lock: a read that races another only reads the file twice.
pub(crate) fn several_logins() -> bool {
    /// The unix second of the last read (`0`: none yet).
    static READ_AT: AtomicU64 = AtomicU64::new(0);
    static SEVERAL: AtomicBool = AtomicBool::new(false);
    let now = crate::upgrade_host::now_s().max(1);
    let at = READ_AT.load(Ordering::SeqCst);
    // A clock set back reads again.
    if at != 0 && now >= at && now - at < SEVERAL_LOGINS_EVERY_S {
        return SEVERAL.load(Ordering::SeqCst);
    }
    let several = accounts::several_listed(accounts::default_roster_path().as_deref());
    SEVERAL.store(several, Ordering::SeqCst);
    READ_AT.store(now, Ordering::SeqCst);
    several
}

/// How long [`several_logins`] answers from its last read, in seconds.
const SEVERAL_LOGINS_EVERY_S: u64 = 10;

/// THE PRODUCT'S [`Acts::memory`]: the physical footprint of `pid`'s whole
/// process group ([`aterm_sysprobe::group_memory`]: a Claude Code's Bun
/// process and what it forks into its group, never a Bash tool's shell,
/// which leads a group of its own), the machine's memory, the kernel's
/// pressure level and the machine's swap rate ([`machine_swap_rate`]) —
/// the two the ceiling reads a thrash from
/// ([`aterm_agent::supervise::memory::AgentMemory::strain`]). `None` where
/// the group cannot be read (the process is gone or not ours; every platform
/// but macOS). The FOOTPRINT, not the resident set: the runaway of
/// 2026-09-29 was 215 GiB by the one and 29.8 GiB by the other
/// (`aterm_sysprobe::GroupMemory`).
pub(crate) fn agent_memory(pid: u32) -> Option<AgentMemory> {
    let group = aterm_sysprobe::group_memory(pid)?;
    let machine = aterm_sysprobe::machine_memory();
    Some(AgentMemory {
        footprint_mib: group.footprint_mib,
        physical_mib: aterm_sysprobe::physical_memory_mib(),
        available_mib: machine.map(|m| m.available_mib),
        used_mib: machine.map(|m| m.used_mib),
        pressure: aterm_sysprobe::memory_pressure().map(pressure_word),
        swap_kib_s: machine_swap_rate(),
        largest_other_mib: None,
    })
}

/// How long a tab's measure counts on the [`MemoryBoard`]: a worker measures
/// at most every [`aterm_agent::supervise::memory::SAMPLE_EVERY`] while its
/// loop asks, so a minute is many of its readings; a tab whose worker ended
/// falls off after it.
pub(crate) const BOARD_FRESH: Duration = Duration::from_secs(60);

/// THE HOST'S AGENTS' FOOTPRINTS, one per tab, as each worker last measured
/// its own ([`Acts::memory`]): what tells the memory ceiling whether an
/// agent is the DOMINANT consumer
/// ([`aterm_agent::supervise::memory::AgentMemory::dominant`]), so a Mac in
/// trouble restarts the largest agent, not every agent past the ceiling.
/// The lock is held for a map update alone.
#[derive(Debug, Default)]
pub(crate) struct MemoryBoard {
    /// Per tab: when it was measured, and its footprint, MiB.
    seen: Mutex<HashMap<String, (Instant, u64)>>,
}

impl MemoryBoard {
    /// Record `sid`'s agent at `footprint_mib` at `now`, forget measures
    /// older than [`BOARD_FRESH`], and answer the largest footprint of the
    /// OTHER tabs' agents (`None`: none measured lately).
    pub(crate) fn record(&self, sid: &str, footprint_mib: u64, now: Instant) -> Option<u64> {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        seen.retain(|_, (at, _)| now.saturating_duration_since(*at) <= BOARD_FRESH);
        seen.insert(sid.to_string(), (now, footprint_mib));
        seen.iter()
            .filter(|(other, _)| other.as_str() != sid)
            .map(|(_, (_, mib))| *mib)
            .max()
    }
}

/// The sampler's pressure level as the ceiling's word for it.
fn pressure_word(level: aterm_sysprobe::MemoryLevel) -> aterm_agent::supervise::memory::Pressure {
    use aterm_agent::supervise::memory::Pressure;
    match level {
        aterm_sysprobe::MemoryLevel::Normal => Pressure::Normal,
        aterm_sysprobe::MemoryLevel::Warn => Pressure::Warn,
        aterm_sysprobe::MemoryLevel::Critical => Pressure::Critical,
    }
}

/// THE MACHINE'S SWAP RATE, KiB/s swapped in plus out, for every agent's
/// measure: ONE meter per process ([`aterm_sysprobe::SwapMeter`]) fed by
/// whichever worker measures next, so a rate spans at least a second of the
/// machine and two workers a moment apart read the same one. The counter is
/// read outside the lock; the lock is held for the arithmetic alone. `None`
/// until two readings a second apart (the first measure after a start or a
/// quiet minute), and on every platform but macOS.
fn machine_swap_rate() -> Option<u32> {
    static METER: Mutex<aterm_sysprobe::SwapMeter> = Mutex::new(aterm_sysprobe::SwapMeter::new());
    let kib = aterm_sysprobe::swapped_kib();
    let at = Instant::now();
    METER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .observe(at, kib)
}

/// THE PRODUCT'S [`Acts::watch`], as [`live_acts`] wires it: the tab handed
/// to [`upgrade_drive::watch`] and nothing else. A named function, not a
/// closure, so the aterm-census guard `watch-hands` can fence it (the watch's review,
/// 2026-09-28: the fence stopped at `(acts.watch)(…)` in [`watch_tab`], and
/// the tests drive their own `Acts`, so an edit here that called
/// `upgrade_drive::step` — a real visit, typing at the next read — or a
/// relaunch would have passed both). The guard holds it to an ALLOW-LIST: the one
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
/// OSC 7) and their agents' process groups, and, off the host thread and one
/// look at a time, hands `aterm_agent::harness::cli::disk_tick` where to
/// SEARCH for build directories ([`disk_roots`]: those working directories
/// and their worktrees, `$HOME`'s checkouts and their worktrees, x.py's
/// bootstrap trees, each agent's `CARGO_TARGET_DIR`, Claude Code's temp root,
/// the cells cache). Below `[disk] auto_free_gib` of free space on the home
/// volume it searches, and reclaims in the build directories ON THAT VOLUME
/// (one on another volume would free nothing there), in this order: the
/// `incremental/` of every cargo profile idle past `target_stale_days` (one
/// day unless the file says otherwise); then, the home volume's free space
/// measured again before each, the build directories whose owner is gone (an
/// ended Claude Code session's scratch build, a removed checkout's cells)
/// WHOLE, the longest unwritten first; then recent profiles' `incremental/`,
/// least recently used first — until free space is back at the floor plus a
/// GiB, or until the bytes released cover what it was short (a snapshot can
/// keep the figure down). Never a profile a build holds or one written into
/// within the last ten minutes; each deletion under cargo's own locks —
/// never a live checkout's directory, `deps/` or anything but `incremental/`
/// — journalling each removal's intent before it and its outcome after (and
/// where the pass stopped) in `<harness state>/disk.jsonl`. Above the floor
/// it reads the config and the free figure and nothing else.
fn live_disk(store: Store, note: Arc<NoteFn>) -> Arc<DiskFn> {
    static LOOKING: AtomicBool = AtomicBool::new(false);
    Arc::new(move |sids: &[String]| {
        let cwds = working_dirs(&store, sids);
        let agents: Vec<u32> = sids
            .iter()
            .filter_map(|sid| foreground_of(&store, sid))
            .filter_map(|pgid| u32::try_from(pgid).ok())
            .collect();
        if LOOKING.swap(true, Ordering::SeqCst) {
            return;
        }
        let note = Arc::clone(&note);
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}disk"))
            .spawn(move || {
                disk_look(&cwds, &agents, note.as_ref());
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

/// Where the disk watch SEARCHES for build directories
/// (`aterm_agent::harness::disk::discover`): the agents' working directories
/// `cwds` (and the worktree each is in), `$HOME`'s checkouts and their
/// worktrees, the `CARGO_TARGET_DIR` each agent in `agents` (a process group's
/// leader) was started with where its environment can be read, Claude Code's
/// temp root for this uid, and the cells cache; and who may hold a session,
/// `running` (the running Claude Codes and the directories the processes
/// were started with, or why a process that may be one could not be read),
/// its end read in those directories and the identities'.
fn disk_roots(
    cwds: &[std::path::PathBuf],
    agents: &[u32],
    home: &Path,
    running: aterm_agent::harness::disk::discover::ClaudeCensus,
) -> aterm_agent::harness::disk::discover::Roots {
    use aterm_agent::harness::disk::discover;
    let args: Vec<atpkg::caller_shell::ProcArgs> = agents
        .iter()
        .filter_map(|&pid| atpkg::caller_shell::process_args(pid))
        .collect();
    let agent_target_dirs = args
        .iter()
        .filter_map(|args| {
            args.env_var("CARGO_TARGET_DIR")
                .map(std::path::PathBuf::from)
        })
        .filter(|p| p.is_absolute())
        .collect();
    // Who may hold a session: each running Claude Code, by what the kernel
    // says of it. Where its end is read: `~/.claude`, the Claude Code
    // directory each running Claude Code was started with (a supervised
    // agent's among them) and each identity's. None ends while a process
    // that may be a Claude Code cannot be read, and a session no directory
    // named here holds the transcript of never does.
    let (running, claude_unread) = match running {
        Ok(running) => (running, None),
        Err(why) => (discover::Running::default(), Some(why)),
    };
    let home_var = aterm_agent::harness::accounts::HOME_ENV;
    let identities = aterm_types::dirs::identities_dir()
        .map(|root| {
            crate::agent_identity::roster()
                .into_iter()
                .flat_map(|name| crate::agent_identity::env(&root.join(name)))
                .filter(|(var, _)| var == home_var)
                .filter_map(|(_, dir)| {
                    aterm_agent::harness::footer::claude_dir_of(Some(&dir), None, || None)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let xdg_cache = std::env::var_os("XDG_CACHE_HOME").map(std::path::PathBuf::from);
    discover::Roots {
        cwds: cwds.to_vec(),
        home: Some(home.to_path_buf()),
        claude_dirs: discover::claude_dirs(Some(home), running.dirs.into_iter().chain(identities)),
        claudes: running.claudes,
        claude_unread,
        agent_target_dirs,
        claude_tmp: Some(discover::claude_tmp_root(discover::own_uid())),
        cells: Some(discover::cells_root(home, xdg_cache.as_deref())),
        // The home volume's, whose free space the watch measures; and the
        // mount table, so a stalled network mount is never `stat`ed.
        device: discover::device_of(home),
        mounts: discover::mount_points(),
    }
}

/// The disk watch's two runs of short looks ([`disk_look`]). A look of one
/// kind ends a run of the other, so neither form of the low-disk line is told
/// on a count the other run made: [`DISK_SHORT_LOOKS`] of one kind running
/// earn the owner a line of the digest.
struct DiskRuns {
    /// Passes in a row that LEFT the volume below the floor, whether or not
    /// they reclaimed anything ([`DiskRuns::pass`]): [`HostNote::DiskLow`].
    /// A look that runs no pass because a `[disk]` value aterm cannot read
    /// turned every removal off ends the run, as does a look at or above the
    /// floor.
    passes: std::sync::atomic::AtomicU32,
    /// Looks in a row that found the volume below the automatic floor still
    /// read while a `[disk]` value aterm cannot read turns every removal off,
    /// so no pass ran ([`disk_look_without_pass`]):
    /// [`HostNote::DiskLowCleanupOff`]. A pass (the settings read again), a
    /// look that cannot read the floor and a look at or above it end the run.
    cleanup_off: std::sync::atomic::AtomicU32,
}

impl DiskRuns {
    const fn new() -> Self {
        Self {
            passes: std::sync::atomic::AtomicU32::new(0),
            cleanup_off: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// What a pass that ran tells the window ([`disk_pass_outcome`] on
    /// [`Self::passes`]); it ends a run of looks with cleanup off.
    fn pass(
        &self,
        freed: bool,
        free_after: Option<u64>,
        config: aterm_agent::harness::disk::Config,
    ) -> DiskOutcome {
        self.cleanup_off.store(0, Ordering::SeqCst);
        disk_pass_outcome(freed, free_after, config, &self.passes)
    }
}

/// This process's runs of short looks ([`DiskRuns`]).
static DISK_RUNS: DiskRuns = DiskRuns::new();
/// Short looks in a row, of one kind ([`DiskRuns`]), that make the owner's
/// low-disk line: passes that left the volume short, or looks that found it
/// short while cleanup is off by fault (the disk watch ticks every
/// [`DISK_TICK`], so twelve hours of a volume aterm did not bring back).
const DISK_SHORT_LOOKS: u32 = 2;
/// The notes of the last reading of aterm.toml's `[disk]` knobs
/// ([`disk_look`]): each set is said in the log once, when it changes.
static DISK_NOTES_SAID: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// One look of the disk watch over `cwds` and the agents `agents` (off the
/// host thread). Above the floor, or once a pass brought the volume back to
/// it, nothing is the owner's; a volume that a pass LEAVES below the floor
/// [`DISK_SHORT_LOOKS`] times running — whatever each freed — is told to the
/// window ([`HostNote::DiskLow`], [`DiskRuns::pass`]), with the largest
/// build directories the pass could not reclaim and what frees each.
///
/// The knobs are read as `aterm harness disk` reads them
/// (`aterm_agent::harness::cli::disk_config_read`): where the file does not
/// say what the owner meant by a removal knob, every removal is off, and each
/// value not read as written is a warning in the log, said once per change of
/// the reading ([`DISK_NOTES_SAID`]). A reading that a value it cannot read
/// turned off is a line of the digest while it holds
/// ([`HostNote::DiskSettingsOff`], ended by [`HostNote::DiskSettingsRead`]),
/// and where its floor is still read, a volume below that floor through
/// [`DISK_SHORT_LOOKS`] looks opens a low-disk line of its own though no pass
/// runs ([`HostNote::DiskLowCleanupOff`], [`disk_look_without_pass`]). A
/// reading whose floor is unknown removes nothing and ends no low-disk line
/// on the volume's account: an open one stands until a look reads the floor
/// and finds the volume at or above it (or, for the cleanup-off form, reads
/// the settings again).
fn disk_look(cwds: &[std::path::PathBuf], agents: &[u32], note: &NoteFn) {
    let (Some(home), Some(state)) = (aterm_primer::home_dir(), harness_state()) else {
        return;
    };
    let config_path = crate::app_config::config_path();
    let reading = aterm_agent::harness::cli::disk_config_read(config_path.as_deref());
    {
        let mut said = DISK_NOTES_SAID
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *said != reading.notes {
            for line in &reading.notes {
                aterm_log::warn!("harness disk watch: {line}");
            }
            if reading.notes.is_empty() {
                aterm_log::info!(
                    "harness disk watch: aterm.toml's [disk] settings read as written again"
                );
            }
            said.clone_from(&reading.notes);
        }
    }
    // Which values turned removal off is the faults' to say, read from the
    // text again (the reading keeps its notes, not which of them did so), and
    // only while removal is off by fault.
    let faults = if reading.off_by_fault {
        config_path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| aterm_agent::harness::cli::disk_faults(&text))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    note(disk_settings_note(&reading, &faults));
    let config = reading.config;
    let free = atpkg::freespace::available_bytes(&home);
    if !config.below_auto_floor(free) {
        if let Some(told) = disk_look_without_pass(&reading, free, &DISK_RUNS) {
            note(told);
        }
        return;
    }
    // Below the floor only: every process of this uid read — which are
    // Claude Codes, where each works and when it started, from the kernel.
    let running = aterm_agent::harness::disk::discover::running_claude_census(Some(&home));
    let roots = disk_roots(cwds, agents, &home, running);
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    let transcripts = home.join(".claude").join("projects");
    let look = aterm_agent::harness::cli::DiskLook {
        state: &state,
        volume: &home,
        free,
        targets: &[],
        discover: Some(&roots),
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
    let left: &[aterm_agent::harness::disk::Hog] = match &tick {
        aterm_agent::harness::cli::DiskTick::Reclaimed(_, hogs) => hogs,
        aterm_agent::harness::cli::DiskTick::Plenty => &[],
    };
    // Whole in the log; on the glass, each fitted to the detail line that
    // carries it, so the path gives way before what keeps it.
    let hogs: Vec<String> = left.iter().map(|h| h.line(Some(&home))).collect();
    let glass: Vec<String> = left
        .iter()
        .map(|h| h.line_within(Some(&home), crate::message_reporters::DISK_LOW_HOG_CHARS))
        .collect();
    let freed = matches!(&tick, aterm_agent::harness::cli::DiskTick::Reclaimed(done, _)
        if !done.removed.is_empty());
    if let aterm_agent::harness::cli::DiskTick::Reclaimed(done, _) = &tick
        && freed
    {
        // What went: a gone owner's build directory whole, or only in part
        // (a delete error kept some of it, in its journal row's `trouble`);
        // every other unit is one cargo profile's incremental cache.
        let went = done.went();
        aterm_log::info!(
            "harness disk watch: below {} GiB free, freed about {} — {} build director{} of \
             ended owners, whole, {} in part, and the incremental caches of {} cargo profile{}; \
             each is listed in {}",
            config.auto_free_gib,
            aterm_agent::harness::disk::human_bytes(done.freed_bytes),
            went.whole,
            if went.whole == 1 { "y" } else { "ies" },
            went.in_part,
            went.profiles,
            if went.profiles == 1 { "" } else { "s" },
            state.join(aterm_agent::harness::cli::DISK_LEDGER).display(),
        );
    }
    // Where the pass LEFT the volume, measured again whatever it freed
    // ([`free_after_pass`]).
    let after = free_after_pass(freed, free, || atpkg::freespace::available_bytes(&home));
    match DISK_RUNS.pass(freed, after, config) {
        DiskOutcome::Plenty => note(HostNote::DiskPlenty),
        DiskOutcome::Short => {}
        DiskOutcome::Low {
            looks: passes,
            free_gib,
        } => {
            aterm_log::warn!(
                "harness disk watch: below {} GiB free ({free_gib} GiB) after {passes} passes; \
                 the largest it cannot reclaim: {}",
                config.auto_free_gib,
                if hogs.is_empty() {
                    "none found".to_owned()
                } else {
                    hogs.join("; ")
                },
            );
            note(HostNote::DiskLow {
                free_gib,
                floor_gib: config.auto_free_gib,
                hogs: glass,
            });
        }
    }
}

/// What a reading of the `[disk]` settings tells the window of itself, at
/// every look: every removal off because the file does not say what the
/// owner meant by a removal knob ([`HostNote::DiskSettingsOff`]), or not
/// ([`HostNote::DiskSettingsRead`]). An owner's own `auto_free_gib = 0` is no
/// fault: it is read as written.
///
/// The line carries the notes on the values that turned removal off: those of
/// `faults` (`aterm_agent::harness::cli::disk_faults`, read from the same
/// file) marked `removal_off`, as the reading holds them. A value that only
/// reads as its default, a negative number and a key `[disk]` does not have
/// are notes of the reading too, but none of them is why cleanup is off.
/// Where no such fault matches a note of the reading (a file that cannot be
/// read or parsed, whose one note is the reading's; or a file changed between
/// the two reads), the reading's every note is carried.
fn disk_settings_note(
    reading: &aterm_agent::harness::cli::DiskReading,
    faults: &[aterm_agent::harness::cli::DiskFault],
) -> HostNote {
    if !reading.off_by_fault {
        return HostNote::DiskSettingsRead;
    }
    let off: Vec<String> = reading
        .notes
        .iter()
        .filter(|note| {
            faults
                .iter()
                .any(|fault| fault.removal_off && fault.note == **note)
        })
        .cloned()
        .collect();
    HostNote::DiskSettingsOff {
        notes: if off.is_empty() {
            reading.notes.clone()
        } else {
            off
        },
    }
}

/// What a look that runs no pass (the reading's floor is not crossed: the
/// volume is at or above it, its figure is unknown, or the floor is `0`)
/// tells the window of the volume, counting in `runs` ([`DiskRuns`]).
///
/// At or above the reading's floor (`DiskReading::floor_gib`: the file's
/// `auto_free_gib`, or its default where none is written),
/// [`HostNote::DiskPlenty`], restated at every such look and not only after
/// this process counted short ones: the line may be one an earlier process
/// raised (the window adopts what the log left open), and only a look can end
/// it. It ends both runs. A figure that cannot be read is at or above any
/// floor the reading holds
/// (`aterm_agent::harness::cli::DiskReading::at_or_above_floor`), so it opens
/// no line.
///
/// Past that, a value aterm cannot read has turned every removal off (the
/// floor is otherwise crossed or read as `0`), so no pass runs and a run of
/// passes ends. Where the floor is still read, a volume below it is a
/// short look of the cleanup-off run, and [`DISK_SHORT_LOOKS`] of them running
/// are [`HostNote::DiskLowCleanupOff`]: the volume can fill while aterm frees
/// nothing, and the owner is told that, not that passes ran. A floor the file
/// does not say is never crossed: nothing is said of the volume, an open line
/// stands, and the run ends.
fn disk_look_without_pass(
    reading: &aterm_agent::harness::cli::DiskReading,
    free: Option<u64>,
    runs: &DiskRuns,
) -> Option<HostNote> {
    if reading.at_or_above_floor(free) {
        runs.passes.store(0, Ordering::SeqCst);
        runs.cleanup_off.store(0, Ordering::SeqCst);
        return Some(HostNote::DiskPlenty);
    }
    runs.passes.store(0, Ordering::SeqCst);
    let Some(floor_gib) = reading.floor_gib.filter(|_| reading.off_by_fault) else {
        runs.cleanup_off.store(0, Ordering::SeqCst);
        return None;
    };
    let floor = aterm_agent::harness::disk::Config {
        auto_free_gib: floor_gib,
        ..reading.config
    };
    match disk_pass_outcome(false, free, floor, &runs.cleanup_off) {
        DiskOutcome::Plenty => Some(HostNote::DiskPlenty),
        DiskOutcome::Short => None,
        DiskOutcome::Low { looks, free_gib } => {
            aterm_log::warn!(
                "harness disk watch: below {floor_gib} GiB free ({free_gib} GiB) at {looks} \
                 looks running, and no pass ran: every removal is off until aterm.toml's \
                 [disk] settings can be read"
            );
            Some(HostNote::DiskLowCleanupOff {
                free_gib,
                floor_gib,
            })
        }
    }
}

/// The free figure a pass LEFT the volume at: `remeasure`d after it, whether
/// or not it freed anything — the look's own `free` was read before the
/// census, the search and the sizing, and something else (a person, the
/// system purging under low space) may have freed space since: the pass's
/// journal then says it stopped on free space back. Unreadable again, a pass
/// that freed nothing keeps `free`, and one that freed something has no
/// figure ([`disk_pass_outcome`] counts it as helping).
fn free_after_pass(
    freed: bool,
    free: Option<u64>,
    remeasure: impl FnOnce() -> Option<u64>,
) -> Option<u64> {
    remeasure().or(free.filter(|_| !freed))
}

/// What one look of a run ([`DiskRuns`]) tells the window
/// ([`disk_pass_outcome`]): a pass, or a look that ran none because cleanup
/// is off by fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiskOutcome {
    /// At or above the floor: the run ends, and any low-disk line with it.
    Plenty,
    /// Below the floor, not yet [`DISK_SHORT_LOOKS`] looks of the run.
    Short,
    /// Below the floor `looks` looks of the run running: the owner is told
    /// (the passes' line names what aterm cannot reclaim).
    Low { looks: u32, free_gib: u64 },
}

/// THE OWNER IS TOLD WHEN THE VOLUME STAYS SHORT, not only when aterm freed
/// nothing: a pass that freed some incremental caches but left the volume
/// below the floor counts as a short pass like one that freed nothing (with
/// agents always building, some cache is always there to free, and the hogs
/// that only a person can clear — x.py's trees, a checkout's `deps/` — were
/// never named). `free_after` is the free figure measured after the pass;
/// unreadable, a pass that freed something counts as helping. `short` counts
/// the short looks of one run in a row, and is reset by one that is not; a
/// look of the cleanup-off run is counted here as a pass that freed nothing
/// ([`disk_look_without_pass`]).
fn disk_pass_outcome(
    freed: bool,
    free_after: Option<u64>,
    config: aterm_agent::harness::disk::Config,
    short: &std::sync::atomic::AtomicU32,
) -> DiskOutcome {
    let still_short = match free_after {
        Some(_) => config.below_auto_floor(free_after),
        None => !freed,
    };
    if !still_short {
        short.store(0, Ordering::SeqCst);
        return DiskOutcome::Plenty;
    }
    let looks = short.fetch_add(1, Ordering::SeqCst).saturating_add(1);
    if looks >= DISK_SHORT_LOOKS {
        DiskOutcome::Low {
            looks,
            free_gib: free_after.map_or(0, |bytes| bytes >> 30),
        }
    } else {
        DiskOutcome::Short
    }
}

/// The harness state directory the upgrade and the relaunch file their
/// records under (`<aterm state>/harness/<HARNESS>`), or `None` without one.
/// The owner's word from the window is written under it too
/// (`upgrade_host::live_word_writer`).
pub(crate) fn harness_state() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(dir) = STATE_FOR_TEST.with(|dir| dir.borrow().clone()) {
        return Some(dir);
    }
    aterm_agent::operator::default_state_root()
        .ok()
        .map(|root| {
            root.join("harness")
                .join(aterm_agent::harness::cli::HARNESS)
        })
}

#[cfg(test)]
thread_local! {
    /// The harness state directory [`harness_state`] answers on this thread
    /// while a test sets it ([`with_harness_state`]).
    static STATE_FOR_TEST: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// `f` with [`harness_state`] answering `dir` on this thread: a test drives
/// the real `restart` verb against its own stamp directory.
#[cfg(test)]
pub(crate) fn with_harness_state<R>(dir: &std::path::Path, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<std::path::PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let before = self.0.take();
            STATE_FOR_TEST.with(|dir| *dir.borrow_mut() = before);
        }
    }
    let _restore = Restore(STATE_FOR_TEST.with(|now| now.replace(Some(dir.to_path_buf()))));
    f()
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
    Arc::new(move |look, claimed, off, agent_off| {
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
        view.set_claimed(claimed);
        view.set_off(off);
        view.set_agent_off(agent_off);
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
                ask: UPGRADE_ASK.load(Ordering::SeqCst),
                several_logins: several_logins(),
                // The view types no line.
                usage_tap: None,
                instances: upgrade_drive::Instances::Machine,
                agents_off: Vec::new(),
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
/// notice lives (a Settings edit may move the package prefix), and before the
/// host looks at the accounts again ([`activation_loop`]). A newer build
/// wakes it at once, and so does the host's shutdown. Off macOS there is no
/// directory watch and this is how often the workers look — a known poll.
const WAKE_FALLBACK: Duration = Duration::from_secs(600);

/// THE ACTIVATION THREAD'S LOOP over `wait` — the parked
/// [`ActivationWake::wait`]: `true` for a newer build, `false` for the
/// fallback and for a pulled trigger — until the host shuts down. Each `true`
/// is handed to the host ([`HostHandle::note_activation`]).
///
/// AND THE HOST'S OWN LOOK AT THE ACCOUNTS ([`accounts_look`]). Once before
/// the first wait: a line an earlier process left open ends there where its
/// cause has, whatever the policy (under `off` it says there is nothing).
/// AGAIN after every wait, so once per [`WAKE_FALLBACK`] at the least and at
/// once where the trigger was pulled for it ([`HostHandle::accounts_again`]):
/// the look that needs no session at a turn end — a roster mended or deleted
/// by hand, an older look told after a newer one — and the one that says a
/// standing line again ([`accounts_tell`]).
fn activation_loop(host: &HostHandle, mut wait: impl FnMut() -> bool) {
    accounts_look(&host.shared.switches, &host.hooks, false);
    while !host.shared.lock().shutting_down {
        let newer = wait();
        if host.shared.lock().shutting_down {
            break;
        }
        if newer {
            host.note_activation();
        }
        accounts_look(&host.shared.switches, &host.hooks, true);
    }
}

/// Park on the pushes that say a newer Claude Code is installed — atpkg's
/// activation notice, and the native install's link repointed by Claude's
/// own updater ([`aterm_agent::harness::upgrade_wake`]) — and hand each one
/// to the host ([`HostHandle::note_activation`]), until the host shuts down
/// (its trigger ends the wait at once). One kqueue, no timer of its own but
/// the fallback. The thread's loop is [`activation_loop`], which also makes
/// the host's own look at the accounts.
fn spawn_activation_wake(host: HostHandle) {
    let home = aterm_primer::home_dir();
    // The accounts store's wake marker rides the same kqueue as a mark of its
    // own ([`ActivationWake::with_side`]): replaced, it is news of the
    // accounts ([`HostHandle::note_accounts`]) and no activation — no worker
    // is parked for an upgrade look, and no upgrade's counts start over. Its
    // directory is not made for the watch (a machine that lists no account
    // keeps nothing of the store). Off macOS the fallback is the wake for it
    // too: a known poll.
    let news = host.clone();
    let mut wake = ActivationWake::new(home.as_deref())
        .with_word(harness_state().map(|state| upgrade_drive::word_marker(&state)))
        .with_side(
            harness_state().map(|state| accounts::Store::at(&state).wake_path()),
            Box::new(move || news.note_accounts()),
        );
    let _ = host.shared.wake.set(wake.trigger());
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}activation"))
        .spawn(move || activation_loop(&host, || wake.wait(WAKE_FALLBACK)));
    if let Err(e) = spawned {
        aterm_log::warn!(
            "harness: the activation wake could not start: {e}; sessions are looked at for an \
             upgrade only when their supervisor attaches"
        );
    }
}

/// The hook that carries the loop's next try to the window: one
/// `Wake::HarnessWait` per plan the worker tells (or clears), handed to `send`
/// — the app loop's proxy in production, a channel in a test.
fn waiting_hook(send: impl Fn(crate::Wake) + Send + Sync + 'static) -> Arc<WaitingFn> {
    Arc::new(move |sid, plan| {
        send(crate::Wake::HarnessWait {
            sid: sid.to_string(),
            plan,
        });
    })
}

/// The hook that carries the loop's acts and its task's face to the window:
/// one `Wake::HarnessFact` per fact told, handed to `send` — the app loop's
/// proxy in production, a channel in a test.
fn fact_hook(send: impl Fn(crate::Wake) + Send + Sync + 'static) -> Arc<FactFn> {
    Arc::new(move |sid, fact| {
        send(crate::Wake::HarnessFact {
            sid: sid.to_string(),
            fact,
        });
    })
}

/// The hook that carries a Codex's placed reset to the window: one
/// `Wake::HarnessReset` per placement the worker tells (or clears), handed to
/// `send` — the app loop's proxy in production, a channel in a test.
fn placed_hook(send: impl Fn(crate::Wake) + Send + Sync + 'static) -> Arc<PlacedFn> {
    Arc::new(move |sid, placed| {
        send(crate::Wake::HarnessReset {
            sid: sid.to_string(),
            placed,
        });
    })
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
    let words = store.clone();
    let (s1, s2, s3, s4) = (store.clone(), store.clone(), store.clone(), store);
    let (k1, k2, k3, k4) = (sock.clone(), sock.clone(), sock.clone(), sock);
    // The window's half of what a host thread tells it about itself
    // ([`HostNote`]): one wake, applied on the app loop.
    let note: Arc<NoteFn> = {
        let proxy = proxy.clone();
        Arc::new(move |note| {
            let _ = proxy.send_event(crate::Wake::HostNote(note));
        })
    };
    let host = HostHandle::start(
        cfg,
        headless,
        suspended,
        Hooks {
            roster: Arc::new(move || roster_of(&s1)),
            autopilot: Arc::new(move || words_of(&words)),
            still_wanted: Arc::new(move |sid, group| still_wanted(&s2, sid, group)),
            body: Arc::new(move |job| hosted_body(&k1, job)),
            badge: Arc::new(move |sid, text| set_badge(&k2, sid, text)),
            waiting: {
                let proxy = proxy.clone();
                waiting_hook(move |wake| {
                    let _ = proxy.send_event(wake);
                })
            },
            fact: {
                let proxy = proxy.clone();
                fact_hook(move |wake| {
                    let _ = proxy.send_event(wake);
                })
            },
            placed: {
                let proxy = proxy.clone();
                placed_hook(move |wake| {
                    let _ = proxy.send_event(wake);
                })
            },
            claim_epoch: Arc::new(|| CLAIM_EPOCH.load(Ordering::SeqCst)),
            acts: live_acts(s3, k3),
            backoff: Arc::new(restart_backoff),
            pause: Arc::new(|pause| pause),
            upgrade_view: live_view(k4, proxy),
            disk: live_disk(s4, Arc::clone(&note)),
            note,
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
/// ([`aterm_primer::CLAUDE_DEFAULTS`]: the top-effort mode, `xhigh`, the rating
/// surveys off, the drafted-feedback card quiet, autocompaction at 400k tokens),
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
            exited: Arc::new(|_, _| true),
            asked: Arc::new(|_, _| None),
            offered: Arc::new(|_| None),
            relaunch: Arc::new(|_, _, _, _, _, _| "refused:inert".to_string()),
            self_copy: Arc::new(|_| None),
            self_relaunch: Arc::new(|_, _, _, _, _| "refused:inert".to_string()),
            restarted: Arc::new(|_, _, _| false),
            carry: Arc::new(|_, _, _, _| "refused:inert".to_string()),
            relaunch_restored: Arc::new(|_, _, _| "refused:inert".to_string()),
            carry_in_flight: Arc::new(|_, _| None),
            restart: Arc::new(|_, _, _| "refused:inert".to_string()),
            tasked: Arc::new(|_, _, _| None),
            limit_reset: Arc::new(|_| None),
            transcript_tail: Arc::new(|_| None),
            account_wall: Arc::new(|_, _, _| ("inert", None)),
            account_usage: Arc::new(|_, _, _, _, _| "inert"),
            account_gone: Arc::new(|_| {}),
            account_seat: Arc::new(|| None),
            account_env: Arc::new(|_| None),
            account_settings: Arc::new(|_, _, _| true),
            tap_born: Arc::new(|_| None),
            account_restart: Arc::new(|_, _| None),
            account_made: Arc::new(|_, _, _| {}),
            account_cold: Arc::new(|_| None),
            codex_reset: Arc::new(|_, _, _, _| None),
            hold: Arc::new(|_, _| true),
            route: Arc::new(|_, _, _| Route::Custom("inert".to_string())),
            memory: Arc::new(|_, _| None),
            watch: Arc::new(|_, _, _| upgrade_drive::Watched::default()),
        }
    }
}

/// The pid of the agent [`HostHandle::for_successor_test`]'s relaunches land.
#[cfg(test)]
const SUCCESSOR_TEST_LANDED: u32 = 5151;

#[cfg(test)]
impl HostHandle {
    /// A host over no real session — nothing supervised, every act inert —
    /// whose restored relaunch is `act`: another module's test of what a
    /// handoff carries from the restored queue drives it (round four, plan
    /// item 7).
    pub(crate) fn for_restored_test(act: Arc<RestoredFn>) -> Self {
        Self::restored_test_host(act, Arc::new(|_| false))
    }

    /// [`Self::for_restored_test`] whose every tab is OPEN, for a test that
    /// keeps a relaunch owed on one of its App's tabs ([`Self::owe_for_test`]).
    /// The inert host calls every tab closed, so its next pass forgets what is
    /// kept for one ([`forget_closed`]) — and any other test's harness in the
    /// process rings the bell that starts a pass, whenever it does. Measured
    /// 2026-09-30: `app_restore`'s two-captures test lost its owed relaunch
    /// between them in 3 of 16 full `-p aterm-gui --lib` runs, one of them on
    /// the tree before any change of that day, and in none of 24 runs of it
    /// alone; a [`ring`] between the captures failed it every time, and on
    /// this host it passes.
    pub(crate) fn for_restored_test_with_open_tabs(act: Arc<RestoredFn>) -> Self {
        Self::restored_test_host(act, Arc::new(|_| true))
    }

    /// A host that SUPERVISES `sids`, each a Claude Code tab, with `body` as
    /// every worker's loop, every act inert: another module's test of what a
    /// seamless update's Commit does to the host's workers drives it (the
    /// review of round eight's #18).
    pub(crate) fn for_supervised_test(
        sids: Vec<String>,
        body: Arc<dyn Fn(&WorkerJob) -> BodyEnd + Send + Sync>,
    ) -> Self {
        let (wanted, open) = (sids.clone(), sids.clone());
        let mut acts = Acts::inert();
        acts.open = Arc::new(move |sid| open.iter().any(|o| o == sid));
        let hooks = Hooks {
            roster: Arc::new(move || {
                sids.iter()
                    .map(|sid| (sid.clone(), Program::Claude, FollowStamp::default()))
                    .collect()
            }),
            autopilot: Arc::new(HashMap::new),
            still_wanted: Arc::new(move |sid, _| wanted.iter().any(|w| w == sid)),
            body,
            badge: Arc::new(|_, _| {}),
            waiting: Arc::new(|_, _| {}),
            fact: Arc::new(|_, _| {}),
            placed: Arc::new(|_, _| {}),
            claim_epoch: Arc::new(|| 0),
            acts,
            backoff: Arc::new(|_| Duration::from_millis(10)),
            pause: Arc::new(|pause| pause / 1000),
            upgrade_view: Arc::new(|_, _, _, _| ViewNext::default()),
            disk: Arc::new(|_| {}),
            note: Arc::new(|_| {}),
        };
        Self::start(SupervisorConfig::default(), false, false, hooks)
    }

    /// A SEAMLESS UPDATE'S SUCCESSOR whose tabs are all open with no agent
    /// named in any, started suspended as a successor is (round eighteen):
    /// the agent its parent supervised in a tab has ended, the kernel's
    /// word of it at once, and what it left is a crash's record that
    /// survived it. Every relaunch on exit is recorded in `relaunched` —
    /// `(tab, what the exit left)` — and lands: an agent of its own (pid
    /// [`SUCCESSOR_TEST_LANDED`]) that holds the tab from then on, reads,
    /// and runs on. Every other act is inert; `passes` counts its passes
    /// (each reads the roster, which names no agent). Another module's test
    /// of what a handoff's layout carries to the successor drives it.
    pub(crate) fn for_successor_test(
        relaunched: Arc<Mutex<Vec<(String, String)>>>,
        passes: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        let landed: Arc<Mutex<BTreeSet<String>>> = Arc::default();
        let mut acts = Acts::inert();
        let lands = Arc::clone(&landed);
        acts.snapshot = Arc::new(move |sid| {
            lands
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(sid)
                .then(|| Snapshot {
                    stored_login: false,
                    tab: sid.to_string(),
                    pid: SUCCESSOR_TEST_LANDED,
                    start: "Wed Oct 15 01:02:03 2026".to_string(),
                    shell: 0,
                    program: PathBuf::from("/opt/claude"),
                    argv: vec!["/opt/claude".to_string()],
                    session: None,
                    cwd: "/".to_string(),
                    version: None,
                    codex: None,
                    dialect: None,
                    login: None,
                    home: None,
                    claude_dir: None,
                })
        });
        acts.exited = Arc::new(|snap, within| {
            if snap.pid == SUCCESSOR_TEST_LANDED {
                std::thread::sleep(within);
                return false;
            }
            true
        });
        acts.open = Arc::new(|_| true);
        acts.status =
            Arc::new(|_| Some("OK program=zsh hold=0 hand=- human_ms=- supervisor=-".to_string()));
        acts.exit_look = Arc::new(|_, snap| {
            Some(ExitLook {
                running: false,
                record: Some(aterm_agent::harness::upgrade::SessionFile {
                    pid: snap.pid,
                    session_id: snap.session.clone().unwrap_or_default(),
                    cwd: snap.cwd.clone(),
                    version: "2.1.300".to_string(),
                    status: "busy".to_string(),
                    status_updated_at_ms: 1,
                    proc_start: snap.start.clone(),
                    kind: "interactive".to_string(),
                    entrypoint: "cli".to_string(),
                    claude_dir: None,
                    profile: None,
                }),
                agent: aterm_agent::harness::upgrade::Agent::Claude,
                crashed: None,
            })
        });
        let lands = Arc::clone(&landed);
        acts.relaunch = Arc::new(move |sid, _, _, left, _, _| {
            relaunched
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((sid.to_string(), left.word().to_string()));
            lands
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(sid.to_string());
            "adopted".to_string()
        });
        let hooks = Hooks {
            roster: Arc::new(move || {
                passes.fetch_add(1, Ordering::SeqCst);
                Vec::new()
            }),
            autopilot: Arc::new(HashMap::new),
            still_wanted: Arc::new(move |sid, _| {
                landed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .contains(sid)
            }),
            body: Arc::new(|_| BodyEnd::Stopped),
            badge: Arc::new(|_, _| {}),
            waiting: Arc::new(|_, _| {}),
            fact: Arc::new(|_, _| {}),
            placed: Arc::new(|_, _| {}),
            claim_epoch: Arc::new(|| 0),
            acts,
            backoff: Arc::new(|_| Duration::from_millis(10)),
            pause: Arc::new(|pause| pause / 1000),
            upgrade_view: Arc::new(|_, _, _, _| ViewNext::default()),
            disk: Arc::new(|_| {}),
            note: Arc::new(|_| {}),
        };
        Self::start(SupervisorConfig::default(), false, true, hooks)
    }

    fn restored_test_host(act: Arc<RestoredFn>, open: Arc<WantedFn>) -> Self {
        let mut acts = Acts::inert();
        acts.relaunch_restored = act;
        acts.open = open;
        let hooks = Hooks {
            roster: Arc::new(Vec::new),
            autopilot: Arc::new(HashMap::new),
            still_wanted: Arc::new(|_, _| false),
            body: Arc::new(|_| BodyEnd::Stopped),
            badge: Arc::new(|_, _| {}),
            waiting: Arc::new(|_, _| {}),
            fact: Arc::new(|_, _| {}),
            placed: Arc::new(|_, _| {}),
            claim_epoch: Arc::new(|| 0),
            acts,
            backoff: Arc::new(|_| Duration::from_millis(10)),
            pause: Arc::new(|pause| pause / 1000),
            upgrade_view: Arc::new(|_, _, _, _| ViewNext::default()),
            disk: Arc::new(|_| {}),
            note: Arc::new(|_| {}),
        };
        Self::start(SupervisorConfig::default(), false, false, hooks)
    }

    /// Tab `sid`'s relaunch on exit is owed on `snap` (`None`: owed no
    /// more), as a worker's exit handling or its settle leaves [`Kept::owed`]
    /// — another module's test of what a handoff's two captures read of it
    /// drives it (round seven, finding 111).
    pub(crate) fn owe_for_test(&self, sid: &str, snap: Option<Snapshot>) {
        self.owe_asked_for_test(sid, snap, None);
    }

    /// [`Self::owe_for_test`], the explicit restart's `asked` cause with it
    /// ([`OwedRelaunch::asked`]; round eighteen).
    pub(crate) fn owe_asked_for_test(
        &self,
        sid: &str,
        snap: Option<Snapshot>,
        asked: Option<restart_stamp::Cause>,
    ) {
        let kept = Arc::clone(
            self.shared
                .kept
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(sid.to_string())
                .or_default(),
        );
        kept.lock().unwrap_or_else(PoisonError::into_inner).owed =
            snap.map(|snap| OwedRelaunch { snap, asked });
    }

    /// The restored queue's worker has ended (or never ran): after
    /// [`Self::shutdown_and_join`], it has seen the shutdown.
    pub(crate) fn restored_worker_ended(&self) -> bool {
        !self
            .shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
    }
}

#[cfg(test)]
#[path = "harness_host_tests.rs"]
mod tests;
