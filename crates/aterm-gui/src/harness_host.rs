// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-GUI supervisor host (`aterm-harness-host`): discovers agent sessions from the session store
//! and runs their supervisor workers, relaunches and live upgrades.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_agent::harness::accounts;
use aterm_agent::harness::accounts_sync::UpkeepStep;
use aterm_agent::harness::laps;
use aterm_agent::harness::login_latch::{self, LoginLatches};
use aterm_agent::harness::netwatch::Route;
use aterm_agent::harness::profile;
use aterm_agent::harness::relaunch::{
    self, ExitLook, ExitRecord, Foreground, OnExit, Outcome, Relaunches, Restart, Say, Snapshot,
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
    AccountAdvice, AccountAt, AccountBad, AccountPoint, HeldBy, Reach, Ready, RosterWord,
    SignInNext,
};
use aterm_agent::supervise::policy::{
    ResetAsk, ResetEnd, ResetGo, ResetNo, ResetOffer, ResetOutcome, ResetWord,
};
use aterm_agent::supervise::tap_clock::{TapWindow, UsageWindow};
use aterm_agent::supervise::tape::store as tape;
use aterm_agent::supervise::{
    Autopilot, Ctl, CtlReply, Endpoint, Guard, HostStep, IdleHost, IdlePoint, Interrupter,
    ProbeMiss, RelayCtl, Session, StallSeen, SuperviseOpts, SupervisorConfig, UsageSeen,
};

use crate::harness_netprobe::NetProbe;

use aterm_phase::Program;

use crate::message_reporters::DiskFullCause;
use crate::session_store::{SessionState, Store};

/// `cfg` with every key the engine does not read at its default.
pub(crate) fn effective(cfg: &SupervisorConfig) -> SupervisorConfig {
    let d = SupervisorConfig::default();
    SupervisorConfig {
        upgrade: d.upgrade,
        relaunch: d.relaunch,
        probe_api: d.probe_api,
        off: d.off,
        land_at: d.land_at,
        accounts: match cfg.accounts {
            accounts::Mode::Off => accounts::Mode::Off,
            accounts::Mode::Early | accounts::Mode::Wall => d.accounts,
        },
        ..cfg.clone()
    }
}

static UPGRADE_ASK: AtomicBool = AtomicBool::new(false);

static USAGE_TAP: AtomicBool = AtomicBool::new(false);

static USAGE_CREDITS: AtomicBool = AtomicBool::new(false);

const TAP_NOT_OWN: &str = "reading not this machine's own login's";

static AGENTS_OFF: Mutex<Vec<Program>> = Mutex::new(Vec::new());

/// [`UPGRADE_ASK`], read now.
pub(crate) fn upgrade_ask() -> bool {
    UPGRADE_ASK.load(Ordering::SeqCst)
}

/// [`USAGE_CREDITS`], read now.
pub(crate) fn usage_credits() -> bool {
    USAGE_CREDITS.load(Ordering::SeqCst)
}

/// [`USAGE_TAP`]: `[harness] usage_tap` is on and the harness supervises Claude Code
/// ([`SupervisorConfig::usage_tap_on`]). It decides whether the relaunch lines aterm types are
/// offered the usage tap ([`window_drive_opts_with_tap`]) and what the Claude Code footer says
/// of a weekly figure it has not got (`claude_footer`, `profile::WeeklyGap`).
pub(crate) fn usage_tap_on() -> bool {
    USAGE_TAP.load(Ordering::SeqCst)
}

/// Whether `cfg` supervises this instance's agent sessions.
pub(crate) const fn would_supervise(cfg: &SupervisorConfig, headless: bool) -> bool {
    cfg.enabled && (!headless || cfg.headless)
}

/// The switch that would leave a worker (`spawn agent=<word>`) with nothing to see its task through in this instance.
pub(crate) fn worker_unsupervised(
    cfg: &SupervisorConfig,
    headless: bool,
    agent: Program,
) -> Option<&'static str> {
    if !would_supervise(cfg, headless) {
        Some(if cfg.enabled {
            "[harness] headless = false"
        } else {
            "[harness] enabled = false"
        })
    } else if admitted_mask(cfg) & agent_bit(agent) == 0 {
        Some(match agent {
            Program::Claude => "[harness.claude] enabled = false",
            Program::Codex => "[harness.codex] enabled = false",
            Program::Agy => "[harness.agy] enabled = false",
            Program::Muse => "[harness.muse] enabled = false",
            Program::Grok | Program::Kimi | Program::Generic => "[harness.generic] enabled = false",
        })
    } else if cfg.autopilot_for(None) == Autopilot::Off {
        Some("[harness] autopilot = \"off\"")
    } else {
        None
    }
}

/// The host's own `[harness]` switches — the master switch and each act's.
#[derive(Default)]
pub(crate) struct Switches {
    /// Agent admission is live, separately from the loop policy.
    agents: AtomicU8,
    upgrade: AtomicBool,
    relaunch: AtomicBool,
    /// The policy supervises this instance at all ([`would_supervise`]).
    supervises: AtomicBool,
    probe_api: AtomicBool,
    /// `[harness] upgrade_ask` for this host's workers.
    ask: AtomicBool,
    /// A seamless update's park holds the terminal ([`HostHandle::pause_restored`] until [`HostHandle::resume_restored`]).
    parked: AtomicBool,
    /// `[harness] accounts` as a rank ([`ACCOUNTS_OFF`], [`ACCOUNTS_WALL`], [`ACCOUNTS_EARLY`]).
    accounts: AtomicU8,
    /// The host says nothing of the accounts as a whole.
    mute: AtomicBool,
    /// `[harness] limit_reset` as the window last parsed it, and `false` with `[harness] enabled = false`.
    limit_reset: AtomicBool,
    /// `[harness] usage_tap` under the master switch, for this host's workers.
    usage_tap: AtomicBool,
    /// `[harness] usage_notes`, `false` where Claude Code is not supervised here (as for the other account switches).
    usage_notes: AtomicBool,
    /// `[harness] logins`, `false` where the policy supervises nothing here or Claude Code is switched off (as for [`Self::accounts`]).
    logins: AtomicBool,
    /// News of the accounts, counted ([`HostHandle::note_accounts`]: the store's wake marker was
    /// replaced; [`activation_loop`]: the roster's file reads otherwise than at its last look).
    accounts_news: AtomicU64,
    /// The hash of the [`AccountsSaid`] the window was last told ([`accounts_tell`]).
    accounts_said: AtomicU64,
    /// The hash of the [`PoolSaid`]s the window was last told ([`pools_tell`]), `0` for none.
    pools_said: AtomicU64,
    /// `[harness] autopilot`: the default Autopilot of a session with no word of its own ([`OffSessions`]).
    autopilot: AtomicU8,
}

const fn autopilot_rank(mode: Autopilot) -> u8 {
    match mode {
        Autopilot::Cruise => 0,
        Autopilot::Land => 1,
        Autopilot::Off => 2,
    }
}

const ACCOUNTS_OFF: u8 = 0;
const ACCOUNTS_WALL: u8 = 1;
const ACCOUNTS_EARLY: u8 = 2;

impl Switches {
    #[cfg(any(unix, test))]
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
        USAGE_CREDITS.store(cfg.usage_credits, Ordering::SeqCst);
        *AGENTS_OFF.lock().unwrap_or_else(PoisonError::into_inner) = cfg.off.clone();
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
        USAGE_TAP.store(cfg.usage_tap_on(), Ordering::SeqCst);
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

    /// `[harness] autopilot`, the default Autopilot, read live ([`Self::autopilot`]).
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

    /// Whether anything of the accounts acts here.
    fn accounts_live(&self) -> bool {
        self.accounts() != accounts::Mode::Off || self.logins()
    }

    /// `[harness] limit_reset`, read live ([`Self::limit_reset`]).
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

/// Which agents the policy's switches admit, as one copyable mask ([`Switches::on_for`], [`HostHandle::admission`]).
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

/// The policy admission snapshot shared by the live switches and their UI projection (`autopilot::HarnessPosture::Restricted`).
pub(crate) fn admitted_mask(cfg: &SupervisorConfig) -> u8 {
    Admission::of(cfg).0
}

/// `agent`'s bit of an [`Admission`] mask.
pub(crate) const fn agent_bit(agent: Program) -> u8 {
    const _: () = assert!(
        Program::ALL.len() <= u8::BITS as usize,
        "Admission's u8 is full"
    );
    1 << agent as u8
}

/// Restarts per session per [`RESTART_WINDOW`] on the short pauses.
pub(crate) const RESTART_BUDGET: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(3600);
const WORKER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const HOST_JOIN_TIMEOUT: Duration = Duration::from_secs(3);
/// Every harness thread's name starts with this (`logging.rs` routes their panics by it).
pub(crate) const THREAD_PREFIX: &str = "aterm-harness-";

const RESTORED_TRIES: u32 = 8;
const RESTORED_FIRST_PAUSE: Duration = Duration::from_secs(1);
const RESTORED_MAX_PAUSE: Duration = Duration::from_secs(30);
/// Whether a restored tab's relaunch step that came to `outcome` (its word `step`) is tried again.
fn restored_again(step: &str, outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::NotYet(_) | Outcome::Busy) && !step.starts_with("failed:")
}

/// [`restored_again`] within [`RESTORED_TRIES`].
fn restored_retry(step: &str, outcome: &Outcome, tried: u32, waiting: Duration) -> bool {
    restored_again(step, outcome)
        && (tried < RESTORED_TRIES
            || (step == "wait:resume" && waiting < Duration::from_secs(relaunch::STALE_S)))
}

/// One agent a cold restore hands the host ([`HostHandle::relaunch_restored`]).
#[derive(Clone)]
pub(crate) struct RestoredAgent {
    pub(crate) sid: String,
    pub(crate) snap: Snapshot,
    pub(crate) place: String,
    /// The snapshot is the layout's: no process was read for it here.
    pub(crate) cold: bool,
    /// The explicit restart ended it.
    pub(crate) asked: Option<restart_stamp::Cause>,
    /// The tab it ran in, where that was another sid than [`Self::sid`] (`restore::AgentRestore::tab`).
    pub(crate) was: Option<String>,
}

impl RestoredAgent {
    /// Nothing says which login this agent ran under.
    const fn unread(&self) -> bool {
        self.cold && self.snap.home.is_none() && self.snap.login.is_none()
    }
}

/// One cold-restore relaunch's next eligible step.
struct RestoredAttempt {
    agent: RestoredAgent,
    revoked: Arc<AtomicBool>,
    order: usize,
    due: Instant,
    pause: Duration,
    tried: u32,
    resume_wait_since: Option<Instant>,
    acting: bool,
    agent_seen: bool,
    carried: bool,
    landed: bool,
    faults: u32,
    in_flight_only: bool,
}

impl RestoredAttempt {
    /// Whether this relaunch has typed its line.
    const fn typed(&self) -> bool {
        self.resume_wait_since.is_some() || self.landed
    }
}

/// Whether a restored relaunch of tab `sid` is over because an agent ran in its tab.
fn restored_overtaken(
    typed: bool,
    seen: bool,
    runs: &dyn Fn(&str, i32) -> bool,
    sid: &str,
) -> bool {
    !typed && (seen || runs(sid, 0))
}

/// Marks for good every queued relaunch of a tab the roster names an agent in.
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

/// How long after the first cold-restore relaunch of this process the automatic update holds off while agents are still queued ([`HostHandle::restored_pending`]).
pub(crate) const RESTORED_HOLD: Duration = Duration::from_secs(relaunch::STALE_S + 120);

/// The restored agents' queue, shared by its one worker thread ([`HostHandle::relaunch_restored`]) and a seamless update.
#[derive(Default)]
struct RestoredQueue {
    attempts: VecDeque<RestoredAttempt>,
    next_order: usize,
    missed: Vec<RestoredMiss>,
    said: Vec<RestoredMiss>,
    paused: bool,
    since: Option<Instant>,
    running: bool,
    closed: bool,
    #[cfg(test)]
    parked_looks: usize,
}

/// One restored agent that did not come back, for the restored agents' row ([`say_restored`]).
#[derive(Clone, Debug, PartialEq)]
struct RestoredMiss {
    order: usize,
    sid: String,
    place: String,
    agent: upgrade::Agent,
    outcome: Outcome,
    said: u64,
}

/// The restored agents' row as the host says it ([`say_restored`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestoredRow {
    /// Where this row falls among every row this process said, on the one count [`RESTORED_SAID`].
    pub(crate) seq: u64,
    /// Every miss it names, in the order said.
    pub(crate) lines: Vec<RestoredLine>,
    /// The log its `Open log` opens.
    pub(crate) log: Option<PathBuf>,
}

/// One miss of a [`RestoredRow`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestoredLine {
    /// Its tab's session: where the window finds its tab now.
    pub(crate) sid: String,
    /// Where its tab was when it was said (`in tab 2`).
    pub(crate) place: String,
    /// The agent that ran there.
    pub(crate) agent: upgrade::Agent,
    /// Its last step's outcome.
    pub(crate) outcome: Outcome,
    /// When it was said ([`RestoredMiss::said`]).
    pub(crate) said: u64,
}

impl RestoredRow {
    /// The row in the words it was said with.
    pub(crate) fn message(&self) -> aterm_messages::Message {
        let lines: Vec<(String, upgrade::Agent, Outcome)> = self
            .lines
            .iter()
            .map(|l| (l.place.clone(), l.agent, l.outcome.clone()))
            .collect();
        crate::message_reporters::restored_agents_not_resumed(&lines, self.log.as_deref())
    }
}

static RESTORED_SAID: AtomicU64 = AtomicU64::new(0);

/// The next count of [`RESTORED_SAID`].
fn restored_said_next() -> u64 {
    RESTORED_SAID.fetch_add(1, Ordering::Relaxed) + 1
}

/// The keyed-attention owner the loop writes (`meta set attention owner=supervisor …`).
pub(crate) use aterm_agent::supervise::ATTENTION_OWNER;

use aterm_agent::supervise::CLAIM_HELD;
/// The keyed-attention owner of a relaunch's word, the tab's mark this host sets for an agent that is not back ([`set_badge`]).
pub(crate) use aterm_agent::supervise::RELAUNCH_ATTENTION_OWNER;

/// The keyed-attention owner of a worker's needs (`meta set attention owner=needs kind=ask "<program> needs you: … — carrying on"`).
pub(crate) use aterm_agent::supervise::NEEDS_ATTENTION_OWNER;

/// A generation counter and its condition variable.
struct Bell {
    rung: Mutex<u64>,
    cv: Condvar,
}

static BELL: Bell = Bell {
    rung: Mutex::new(0),
    cv: Condvar::new(),
};

static FULL_FOLLOW_EPOCH: AtomicU64 = AtomicU64::new(0);
static PHASE_FOLLOW_EPOCH: AtomicU64 = AtomicU64::new(0);

static CLAIM_EPOCH: AtomicU64 = AtomicU64::new(0);

static UPGRADE_ACTS: AtomicU64 = AtomicU64::new(0);

static STEP_STAMPS: AtomicU64 = AtomicU64::new(0);

/// A worker took an upgrade's or a relaunch's step.
fn note_upgrade_act() {
    UPGRADE_ACTS.fetch_add(1, Ordering::SeqCst);
    ring();
}

/// The window learned the owner's view is stale (a press refused because its round has since stopped).
pub(crate) fn look_at_upgrades_soon() {
    note_upgrade_act();
}

/// Wake the host: something it decides from moved (the roster, a program, a claim, a worker, the config).
pub(crate) fn ring() {
    FULL_FOLLOW_EPOCH.fetch_add(1, Ordering::SeqCst);
    #[cfg(test)]
    RINGS_HERE.with(|n| n.set(n.get() + 1));
    ring_bell();
}

#[cfg(test)]
thread_local! {
    static RINGS_HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many rings ([`ring`]) this thread has made, for a test outside this module (the host's own tests read `RINGS_HERE`).
#[cfg(test)]
pub(crate) fn rings_here() -> u64 {
    RINGS_HERE.with(std::cell::Cell::get)
}

/// A changed agent phase wakes the host, but only the tab whose publication stamp moved needs its Claude session file followed.
pub(crate) fn ring_phase() {
    PHASE_FOLLOW_EPOCH.fetch_add(1, Ordering::SeqCst);
    #[cfg(test)]
    PHASE_RINGS_HERE.with(|n| n.set(n.get() + 1));
    ring_bell();
}

#[cfg(test)]
thread_local! {
    static PHASE_RINGS_HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn ring_bell() {
    let mut rung = BELL.rung.lock().unwrap_or_else(PoisonError::into_inner);
    *rung = rung.wrapping_add(1);
    drop(rung);
    BELL.cv.notify_all();
}

/// A supervisor claim was released or lapsed.
pub(crate) fn note_claim_released() {
    CLAIM_EPOCH.fetch_add(1, Ordering::SeqCst);
    ring();
}

/// What the control thread reaches of a session's worker when a proven self-claim moves
/// ([`fn@self_claimed`], [`self_claim_released`]).
struct ClaimSide {
    interrupt: Arc<Mutex<Option<Interrupter>>>,
    beside: Arc<Mutex<Option<String>>>,
}

static CLAIM_SIDES: Mutex<Option<HashMap<String, ClaimSide>>> = Mutex::new(None);

/// Cut tab `sid`'s worker's loop where `cut` says so of its beside state.
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

/// A proven self-claim landed on tab `sid` ([`crate::self_claim`]), replacing `replaced` if it replaced a claim.
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

/// The proven self-claim on tab `sid` was cleared.
pub(crate) fn self_claim_released(sid: &str, alive: bool) {
    if alive && cut_claim_side(sid, |beside| beside.is_some()) {
        aterm_log::info!(
            "harness @{sid}: the proven self-claim was given back while its agent runs; this \
             host watches it on, and supervises nothing of it"
        );
    }
}

/// The proven self-claim a session's `status` names, when it is one this host watches beside.
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

/// Park on the bell until `done` is set or `deadline` passes.
fn wait_done(done: &AtomicBool, deadline: Instant) -> bool {
    wait_for(deadline, || done.load(Ordering::SeqCst))
}

/// [`wait_done`] for any condition over state whose setters ring the bell.
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

/// Park until the bell has moved past `seen`.
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

/// [`HostHandle::relaunching`] over the host's per-session [`Kept`] map.
fn relaunching_of(
    relaunches: bool,
    kept: &Mutex<HashMap<String, Arc<Mutex<Kept>>>>,
) -> std::collections::HashSet<String> {
    let Some(home) = aterm_primer::home_dir().filter(|_| relaunches) else {
        return std::collections::HashSet::new();
    };
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
                .is_some_and(|snap| relaunch::resumes_on_exit(&home, snap))
        })
        .map(|(sid, _)| sid)
        .collect()
}

/// The holder name this process claims sessions under.
pub(crate) fn holder() -> String {
    upgrade_drive::hand_name()
}

/// Another supervisor's live claim, from one `status` reply.
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

/// What a stopped worker is still allowed to send: a bare `meta` read and a `meta unset …`.
fn cleanup_request(args: &[&str]) -> bool {
    let mut words = args.iter().copied().skip_while(|a| a.starts_with('@'));
    matches!(
        (words.next(), words.next()),
        (Some("meta"), None | Some("unset"))
    )
}

const STOPPED: &str = "the supervisor was stopped";

/// [`RelayCtl`] to this instance's own socket.
pub(crate) struct HostCtl {
    endpoint: Endpoint,
    token: Option<String>,
    /// `token` is the harness's in-memory one ([`Self::as_harness`]).
    prefer: bool,
    inner: RelayCtl,
    cut: Arc<AtomicBool>,
    redialed: bool,
    admission: Option<(Arc<Switches>, Program)>,
    /// The loop's own holder, fencing every write it types (`if-supervisor=<holder>`, [`fenced_write`]).
    fence: Option<String>,
    /// Watch-only beside this proven self-claim's holder ([`WorkerJob::beside`]).
    watch_only: Option<String>,
}

/// Whether `args` types into the session.
fn is_typing(args: &[&str]) -> bool {
    args.iter()
        .find(|a| !a.starts_with('@'))
        .is_some_and(|verb| matches!(*verb, "key" | "send" | "paste" | "turn" | "feed-bin"))
}

/// `args` with `if-supervisor=<holder>` put right after the verb of a write the server fences on its claim (`key`, `send`, `paste`, `turn`).
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

    /// [`Self::new`] with the token given up front (the tests' scratch servers).
    fn with_token(endpoint: Endpoint, token: Option<String>) -> Self {
        Self {
            inner: RelayCtl::new(endpoint.clone(), token.clone()),
            endpoint,
            token,
            prefer: false,
            cut: Arc::default(),
            redialed: false,
            admission: None,
            fence: None,
            watch_only: None,
        }
    }

    /// The harness's own transport.
    pub(crate) fn as_harness(endpoint: Endpoint) -> Self {
        let Some(token) = crate::control_auth::harness_token() else {
            return Self::new(endpoint);
        };
        let mut ctl = Self::with_token(endpoint, Some(token.to_string()));
        ctl.prefer = true;
        ctl.inner = ctl.relay();
        ctl
    }

    /// A fresh transport on this one's endpoint and token.
    fn relay(&self) -> RelayCtl {
        match (&self.token, self.prefer) {
            (Some(token), true) => RelayCtl::preferring(self.endpoint.clone(), token.clone()),
            (token, _) => RelayCtl::new(self.endpoint.clone(), token.clone()),
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
                self.inner = self.relay();
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

/// One session's failed runs inside [`RESTART_WINDOW`], oldest first, at most [`RESTART_BUDGET`]` + 1` kept.
pub(crate) type FaultHistory = Arc<Mutex<VecDeque<Instant>>>;

/// What one worker runs under.
pub(crate) struct WorkerJob {
    pub(crate) sid: String,
    /// The agent the session's program named when the worker started.
    pub(crate) agent: Program,
    pub(crate) opts: SuperviseOpts,
    pub(crate) stop: Arc<AtomicBool>,
    /// Policy admission revoked for this worker, for good.
    pub(crate) revoked: Arc<AtomicBool>,
    /// The live connection's cut, published by the body so a stop ends a parked wait at once.
    pub(crate) interrupt: Arc<Mutex<Option<Interrupter>>>,
    /// Set before `stop` when a new worker takes the session straight after (a changed policy).
    pub(crate) handover: Arc<AtomicBool>,
    /// Clear the badge a previous worker's relaunch left before running.
    pub(crate) clear_badge: bool,
    /// Unset the tab's relaunch mark ([`RELAUNCH_ATTENTION_OWNER`]) before running, saying nothing.
    pub(crate) unset_mark: bool,
    /// The session's failed runs ([`FaultHistory`]), shared with the host.
    pub(crate) faults: FaultHistory,
    /// The request for the loop's next idle point ([`WorkerIdle`], the loop's `opts.idle_host`).
    pub(crate) park: Arc<AtomicBool>,
    /// When the worker wants its session looked at again: an upgrade step that waited.
    pub(crate) look_at: Arc<Mutex<Option<Instant>>>,
    /// The host's word on the worker's agent ([`ExitWord`]).
    pub(crate) left: Arc<ExitWord>,
    /// A park's Commit stopped the worker with its agent's exit handed to it ([`Self::left`]).
    pub(crate) left_at_commit: Arc<AtomicBool>,
    /// Set while the worker takes an upgrade step.
    pub(crate) acting: Arc<AtomicBool>,
    /// The host's own `[harness]` switches, live.
    pub(crate) switches: Arc<Switches>,
    /// What the host keeps of the session for a relaunch ([`Kept`]), across its workers.
    pub(crate) kept: Arc<Mutex<Kept>>,
    /// The session's note behind still to be taken ([`Owed`]).
    note: Note,
    /// The foreground group the host last saw the roster name the agent in ([`Held::stamp`]; `0`: none yet).
    group: Arc<AtomicI32>,
    /// The host, for the one act a stopped worker hands it.
    host: std::sync::Weak<Shared>,
    /// The adopted-claim grace this worker watches through ([`ADOPTED_CLAIM_GRACE`]).
    watch_until: Option<Instant>,
    /// The proven self-claim this worker watches beside: its holder, or `None`.
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

    /// The holder this worker's loop watches beside, if it watches only.
    fn watch_beside(&self) -> Option<String> {
        self.beside()
            .or_else(|| watches_only(self.agent).then(|| UNCLAIMED_CODEX_ALAB.to_string()))
    }

    fn admitted(&self) -> bool {
        !self.revoked.load(Ordering::SeqCst) && self.switches.on_for(self.agent)
    }
}

/// What the host last said of a worker's agent ([`ExitWord`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Word {
    /// No exit handed: the agent runs in the tab as the host last read it.
    Watching = 0,
    /// The agent's exit is the worker's to read ([`Worker::leave`]; a worker that reads its exit alone starts so, [`exit_main`]).
    Left = 1,
    /// A stop reached the worker ([`request_stop`]).
    Stopped = 2,
}

/// The host's word on a worker's agent, one word: its exit handed, the agent back, or a stop.
#[derive(Debug, Default)]
pub(crate) struct ExitWord(AtomicU8);

impl ExitWord {
    /// A word with the agent's exit handed from the start ([`exit_main`]).
    const fn handed() -> Self {
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

    /// `from` moved to `to`, unless the word is `from` no longer (a stop came first: it stands).
    fn moved(&self, from: Word, to: Word) -> bool {
        self.0
            .compare_exchange(from as u8, to as u8, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// The agent's exit handed.
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

/// What the host keeps of one open session for a relaunch, across its workers.
#[derive(Default)]
pub(crate) struct Kept {
    /// The session's relaunch state.
    relaunches: Relaunches,
    /// What a relaunch of its agent needs, read while it ran and followed while it runs ([`follow_snapshot`]).
    snapshot: Option<Snapshot>,
    /// The follow of its agent panicked ([`follow_caught`]).
    follow_faulted: bool,
    /// One follower at a time ([`follow_caught`]).
    following: Arc<Mutex<()>>,
    /// The relaunch on exit this session is owed while its back-off runs ([`relaunch_until`]).
    owed: Option<OwedRelaunch>,
    /// What a park's Commit made of the session's worker and of what it owes ([`Committed`]).
    commit: Committed,
    /// The session's worker has ended with no exit of its agent handed to it, and nobody watches that agent's exit.
    unwatched: Option<Unwatched>,
    /// The exit watch ([`watch_exit`]).
    exit_watch: Option<ExitWatched>,
    /// What the host keeps of the session's account ([`AccountKept`]).
    account: AccountKept,
    /// The word that the tab's agent cannot be brought back, standing.
    lost: Option<Lost>,
    /// A relaunch after aterm ended landed in the tab.
    carry_asked: bool,
    /// The relaunch snapshot read still out ([`start_point_read`]).
    point_read: Option<Arc<PointRead>>,
}

/// Whether a relaunch's answer says it took (`adopted`, `relaunched`, `continued`, `done`, each with or without its `:<detail>`).
fn relaunch_took(step: &str) -> bool {
    let word = step.split_once(':').map_or(step, |(head, _)| head);
    matches!(word, "adopted" | "relaunched" | "continued" | "done")
}

/// What a settle of a relaunch's last step said, told as at an idle point.
fn settle_said(sid: &str, step: &str, hooks: &Hooks) -> bool {
    if step == "done:lost" {
        (hooks.note)(HostNote::Relaunch {
            sid: sid.to_string(),
            said: RelaunchSay::NotRestored,
        });
    }
    if !step.starts_with("done") {
        return false;
    }
    note_upgrade_act();
    true
}

/// The word that an agent cannot be brought back, as [`tell`] said it ([`Kept::lost`]).
struct Lost {
    session: String,
    pid: u32,
    marked: bool,
    reason: String,
    restarted: bool,
    command: Option<String>,
    other: upgrade_drive::Tasked,
    faulted: bool,
}

/// A relaunch on exit a session is owed ([`Kept::owed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwedRelaunch {
    pub(crate) snap: Snapshot,
    pub(crate) asked: Option<restart_stamp::Cause>,
}

/// An exit no worker was handed ([`Kept::unwatched`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unwatched {
    agent: Program,
    group: i32,
}

/// What the exit watch keeps ([`Kept::exit_watch`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct ExitWatched {
    pid: u32,
    start: String,
    arming: u64,
    hands: Option<Unwatched>,
    read: Option<ExitRecord>,
}

impl ExitWatched {
    /// Whether it watches the very process `snap` names.
    fn names(&self, snap: &Snapshot) -> bool {
        self.pid == snap.pid && self.start == snap.start
    }
}

/// What the host keeps of a session's account across its workers.
#[derive(Default)]
struct AccountKept {
    advised: Option<Advised>,
    held: Option<&'static str>,
    stay: Option<&'static str>,
    unmade: Option<String>,
    moved: Option<Moved>,
    lost: Option<LostSeen>,
    came_back: Option<Option<String>>,
    flying: Option<Flying>,
    settled: Option<(u32, String, String, bool)>,
    wall_again: u8,
    lagged: u8,
    probing: Option<Probing>,
    reset_owed: Option<ResetOwed>,
}

/// [`AccountKept::probing`]: the claim of one `/usage` probe and the process it was made for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Probing {
    label: String,
    pid: u32,
    start: String,
}

use aterm_agent::supervise::ProbeNo as NoProbe;

/// Why a limit reset was not claimed ([`WorkerIdle::reset_claimed`]), each by its own word for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoReset {
    /// `[harness] limit_reset`, read live, is off (or the whole policy).
    Off,
    /// The agent is no Claude Code, the host has no seat, or nothing of the agent's process could be read.
    Unread,
    /// The roster's file does not admit: a refused roster resets nothing.
    Roster,
    /// The process names a login directory: it is not on the default login.
    LoginDir,
    /// Its Claude home is one of its own.
    OwnHome,
    /// It does not authenticate by its stored login.
    NotStored,
    /// No row of the roster is the default login's (every row names a directory), or the row is no longer the one with none.
    NotDefaultRow,
    /// The machine's active account is another row.
    ActiveOther,
    /// Another account ran within the vendor's profile window of this process's start (`accounts::RESET_PROFILE_S`).
    OtherLogin,
    /// `[harness] accounts = "off"` and the roster lists another login.
    Unrecorded,
    /// No quota rejection stands last in the transcript yet ([`Standing::Unwritten`]): asked again.
    Unwritten,
    /// The one that stands is not this process's ([`Standing::NotOwn`]).
    NotOwnWall,
    /// It names no reset, one that is not believed (`accounts::believed_reset`), or one that has come.
    NoUntil,
    /// Its window is not one a grant clears ([`aterm_phase::reset_limit_name`]).
    Window,
    /// The wall is neither a long one nor one whose grant could end within a day ([`reset_qualifies`]).
    NotQualifying,
    /// The account's walls were ended after this wall's row was written — by a reset, or by the owner's `accounts use`.
    Cleared,
    /// Another claim is in flight (`accounts::ResetClaimed::Held`).
    Held,
    /// This wall has had its one attempt (`accounts::ResetClaimed::Attempted`).
    Attempted,
    /// The claim is this session's, and too near its lapse for an Enter written now to be answered inside it ([`RESET_ENTER_MARGIN_S`]).
    Late,
    /// The claim is this session's, and the dialog it was made for has already answered.
    Answered,
    /// The store's lock stayed held past its bounded wait: asked again.
    LockHeld,
    /// The state could not be read or written as this build's.
    State,
    /// macOS's program check is stuck ([`WorkerIdle::program_check_stuck`]).
    ProgramCheck,
}

impl NoReset {
    const fn as_str(self) -> &'static str {
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
            NoReset::ProgramCheck => "program-check",
        }
    }

    /// What the loop is answered (`IdleHost::reset_take`).
    const fn word(self) -> ResetWord {
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
            NoReset::ProgramCheck => ResetNo::ProgramCheck,
        })
    }
}

const RESET_LONG_WALL_S: i64 = 24 * 3600;

const RESET_ENTER_MARGIN_S: i64 = 120;

const RESET_LINE_WINDOW_S: i64 = accounts::RESET_CLAIM_S + RESET_ENTER_MARGIN_S;

/// Whether a wall qualifies for a reset at `now`, and whether it is a long one.
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

/// How a claimed limit reset ended, as the host read it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ResetRead {
    /// The vendor's own line said a reset of `kind` was spent and the limits reset, with the count of that kind it named.
    Used {
        kind: aterm_phase::ResetKind,
        left: Option<u32>,
    },
    /// The vendor's own line said a reset of `kind` was spent and the walled limit still stands.
    Walled {
        kind: aterm_phase::ResetKind,
        left: u32,
    },
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
            ResetRead::Used { kind, left } => accounts::ResetDone::Used {
                kind: *kind,
                left: *left,
            },
            ResetRead::Walled { kind, left } => accounts::ResetDone::Walled {
                kind: *kind,
                left: *left,
            },
            ResetRead::Lifted => accounts::ResetDone::Lifted,
            ResetRead::Kept(why) => accounts::ResetDone::Kept(why),
            ResetRead::Failed(why) => accounts::ResetDone::Failed(why),
        }
    }
}

/// What the end of a limit reset's step is read as ([`WorkerIdle::reset_tell`]).
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
            Some((_, ResetLine::Spent { kind, left, asks })) => (
                ResetRead::Used { kind, left },
                ResetOutcome::Spent {
                    kind,
                    asks,
                    left,
                    cleared_at: at,
                },
            ),
            Some((_, ResetLine::Walled { kind, left })) => (
                ResetRead::Walled { kind, left },
                ResetOutcome::Walled { kind, left },
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

/// The offer a limit reset's end puts on record (`accounts::note_offer`): the kind, its count, and
/// the deadline where one was read. Claude Code 2.1.304 counts each kind apart.
fn reset_offer(read: &ResetRead, end: &ResetEnd) -> Option<ResetOffer> {
    match *read {
        ResetRead::Used {
            kind,
            left: Some(left),
        }
        | ResetRead::Walled { kind, left } => Some(ResetOffer {
            kind,
            left,
            use_by: None,
        }),
        ResetRead::Used { kind, left: None } => {
            end.offer
                .filter(|seen| seen.kind == kind)
                .map(|seen| ResetOffer {
                    left: seen.left.saturating_sub(1),
                    use_by: None,
                    ..seen
                })
        }
        ResetRead::Lifted | ResetRead::Kept(_) => end.offer,
        ResetRead::Failed(_) => None,
    }
}

/// [`AccountKept::reset_owed`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct ResetOwed {
    label: String,
    until: i64,
    read: ResetRead,
    offer: Option<ResetOffer>,
    at: i64,
}

impl ResetOwed {
    /// Record it on `state` for session `sid` (`accounts::reset_outcome`, then the offer).
    fn write(&self, state: &mut accounts::State, sid: &str, now: i64) -> Vec<accounts::Event> {
        let events = accounts::reset_outcome(
            state,
            &self.label,
            self.until,
            sid,
            &self.read.done(),
            self.at,
        );
        if let (false, Some(offer)) = (events.is_empty(), self.offer) {
            accounts::note_offer(
                state,
                &self.label,
                Some(offer.kind),
                offer.left,
                offer.use_by,
                self.at,
                now,
            );
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
    /// The claim in the state is not this session's open one: nothing recorded, and the outcome dropped.
    NotMine,
    /// The store's lock stayed held: still held here, for the next transaction.
    Held,
    /// The state could not be read or written as this build's: dropped.
    Refused,
}

/// [`AccountKept::advised`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct Advised {
    to: String,
    point: AccountPoint,
}

/// [`AccountKept::moved`]: an account move made, verified, and not yet served.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Moved {
    to: String,
    login: Option<String>,
    pid: u32,
    start: String,
    answered: bool,
    untold: bool,
}

/// [`AccountKept::flying`]: an account move the rotation was committed for and whose relaunch has not been seen to land.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Flying {
    to: String,
    at: AccountAt,
    login: Option<String>,
    pid: u32,
    start: String,
}

/// [`AccountKept::lost`]: when this host first met one process's lost login.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LostSeen {
    pid: u32,
    start: String,
    at: i64,
}

const WALL_AGAIN: u8 = 3;

const WALL_AGAIN_AFTER: Duration = Duration::from_secs(30);

/// The advice of a session whose evidence lags its screen ([`WorkerIdle::account_lags`]).
fn account_lagging(advised: Option<AccountAdvice>, again: Instant) -> Option<AccountAdvice> {
    match advised {
        None => Some(AccountAdvice::Wait { until: again }),
        Some(AccountAdvice::Wait { until }) => Some(AccountAdvice::Wait {
            until: until.min(again),
        }),
        other => other,
    }
}

const RESET_AGAIN_AFTER: Duration = WALL_AGAIN_AFTER;

/// The advice of a session whose account's limit reset is in flight (`accounts::reset_in_flight`).
fn account_resetting(advised: Option<AccountAdvice>, again: Instant) -> Option<AccountAdvice> {
    match advised {
        Some(AccountAdvice::Wait { until }) => Some(AccountAdvice::Wait {
            until: until.min(again),
        }),
        Some(resume @ AccountAdvice::Resume { .. }) => Some(resume),
        _ => Some(AccountAdvice::Wait { until: again }),
    }
}

const WALL_LOCK_HELD: &str = "lock-held";

/// Whether the recorder's word is one asking again can change ([`AccountKept::wall_again`]).
fn wall_asked_again(word: &str) -> bool {
    matches!(word, "no-wall" | WALL_LOCK_HELD)
}

/// A park's Commit stopped the session's worker: a fact on the session, under its lock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(any(unix, test)), allow(dead_code))]
enum Committed {
    /// No park's Commit stopped the session's worker.
    #[default]
    No,
    /// A Commit stopped the worker, and neither it has decided nor a rollback come since.
    Stopped,
    /// The worker the Commit stopped decided, its agent not back: what [`Kept::owed`] holds stays owed.
    Stranded,
    /// The Commit failed and its rollback came before the worker decided.
    TakenUp(String),
}

/// How the worker whose wait a stop ended reads it, its agent not back ([`Kept::stopped`]).
enum Stop {
    /// A stop for good, or nothing owed: what was owed is settled.
    ForGood,
    /// A Commit's, not rolled back: the relaunch stays owed.
    ByCommit,
    /// A Commit's that has rolled back: this relaunch is taken up now.
    RolledBack(Box<RestoredAgent>),
}

impl Kept {
    /// What the stop that ended session `sid`'s worker's wait was, its agent not back ([`Committed`]).
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
                was: None,
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
    /// It ended watching beside a proven self-claim ([`WorkerJob::beside`]).
    SelfClaimed,
}

/// A tab's publication as read under its timeline lock with the roster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FollowStamp {
    group: i32,
    reader: Option<Program>,
    rev: u64,
    /// The reads of its screen that moved under a verdict that stood while its agent was not busy (`AgentPublication::screen_moves`).
    screen: u64,
    /// A vendor codex: a session whose agent is Codex with no proven self-claim.
    vendor: bool,
}

impl FollowStamp {
    /// Whether `self` moved from `before` in what is not its screen alone.
    fn moved_beyond_screen(self, before: Self) -> bool {
        Self {
            screen: before.screen,
            ..self
        } != before
    }
}

type RosterFn = dyn Fn() -> Vec<(String, Program, FollowStamp)> + Send + Sync;
/// Every live session's own Autopilot word ([`Hooks::autopilot`]), by sid.
type WordsFn = dyn Fn() -> HashMap<String, Autopilot> + Send + Sync;
type WantedFn = dyn Fn(&str) -> bool + Send + Sync;
/// Whether tab `sid` is owed a relaunch's continuation ([`Acts::owed`]).
type OwedFn = dyn Fn(&str, Option<upgrade::Agent>) -> bool + Send + Sync;
/// [`Acts::offered`]: for tab `sid`, `None` where the explicit restart would not be carried, else `Some(promised)`.
type OfferedFn = dyn Fn(&str) -> Option<bool> + Send + Sync;
/// Whether a session is still its worker's ([`Hooks::still_wanted`]): `(sid, group)`.
type StillFn = dyn Fn(&str, i32) -> bool + Send + Sync;
/// Whether a tab's agent still holds it ([`Acts::holds`]): `(sid, group)`.
type HoldsFn = dyn Fn(&str, i32) -> Option<bool> + Send + Sync;
/// When tab `sid`'s last command started with no person's gesture since ([`Acts::launch`]), unix ms.
type LaunchFn = dyn Fn(&str) -> Option<i64> + Send + Sync;
/// What the upgrade reads of a session ([`upgrade_drive::due`]).
type DueFn = dyn Fn(&str) -> Due + Send + Sync;
type BodyFn = dyn Fn(&WorkerJob) -> BodyEnd + Send + Sync;
type BadgeFn = dyn Fn(&str, Option<&str>) + Send + Sync;
/// The loop's next try, told to the window ([`IdleHost::waiting`]).
type WaitingFn = dyn Fn(&str, Option<crate::presence::RetryPlan>) + Send + Sync;
/// What the loop tells the window of the harness's glass ([`Hooks::fact`]): `(sid, fact)`.
type FactFn = dyn Fn(&str, crate::HarnessFact) + Send + Sync;
/// The worker's needs you set, whole, stored where its entry lands ([`Hooks::needs`]).
type NeedsFn = dyn Fn(&str, &[String], &str) -> bool + Send + Sync;
/// Why Land still holds, stored where the window reads it ([`Hooks::hold`]).
type LandHoldFn = dyn Fn(&str, Option<aterm_agent::harness::acts::LandHeld>) -> bool + Send + Sync;

type EpochFn = dyn Fn() -> u64 + Send + Sync;
type StepFn = dyn Fn(&str, u32) -> String + Send + Sync;
/// A relaunched agent's owed carry-on, taken ([`Acts::resume`]).
type ResumeFn = dyn Fn(&str, u32, CarryPoint) -> String + Send + Sync;
/// That carry-on settled with nothing typed where the agent rests at a point its loop withheld from the host ([`Acts::settle`]).
type SettleFn = dyn Fn(&str) -> String + Send + Sync;

/// What a relaunched agent's owed carry-on is told at the tab's idle point ([`Acts::resume`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CarryPoint {
    /// The session's Autopilot is Land ([`upgrade_drive::Opts::land`]).
    pub(crate) land: bool,
    /// The loop read the point as a task landed under Land ([`IdlePoint::done_under_land`]: a clean `Nothing left:` defense at a land act's yield).
    pub(crate) done_under_land: bool,
    /// The one agent whose record is carried ([`carries_only`]).
    pub(crate) only: Option<upgrade::Agent>,
}
/// The session noted behind since a unix second ([`upgrade_drive::note_behind`]).
type BehindFn = dyn Fn(&str, u64) -> Behind + Send + Sync;
/// The upgrade's clocks held until a unix second ([`Acts::hold`]).
type HoldFn = dyn Fn(&str, u64) -> bool + Send + Sync;
/// Type a text into a tab, the step's word back ([`Acts::disk_resume`]): `(sid, text)`.
type DiskResumeFn = dyn Fn(&str, &str) -> String + Send + Sync;
/// Type or release the `disk-free@v1` line owed a tab, the step's word back ([`Acts::disk_free`]):
/// `(sid, owed, unfit, held)`, `unfit` naming why the session may not be asked, `held` what holds
/// the line at this turn break ([`IdlePoint::held`]'s word: a wait, the line kept owed).
type DiskFreeFn =
    dyn Fn(&str, &crate::disk_board::AskOwed, AskWord, AskWord) -> String + Send + Sync;
/// A word [`DiskFreeFn`] is handed: why a session may not be asked, or what holds its line.
type AskWord = Option<&'static str>;
type SnapshotFn = dyn Fn(&str) -> Option<Snapshot> + Send + Sync;
/// The occupancy facts board as session `sid`'s host reads it ([`Acts::occupancy`]).
type OccupancyFn =
    dyn Fn(&str) -> aterm_agent::supervise::occupancy::OccupancyReading + Send + Sync;
/// The machine's pace and a slot's reservation for session `sid`'s fact line of a rule, its hold begun at a unix ms ([`Acts::occupancy_pace`]).
type OccupancyPaceFn = dyn Fn(
        &str,
        aterm_agent::supervise::occupancy::RuleId,
        i64,
    ) -> aterm_agent::supervise::occupancy::PaceGrant
    + Send
    + Sync;
/// The task door's fact word on session `sid`'s task record, `None` clearing it ([`Acts::task_fact`]).
type TaskFactFn = dyn Fn(
        &str,
        Option<aterm_agent::supervise::occupancy::FactWord>,
    ) -> aterm_agent::supervise::occupancy::NoteOutcome
    + Send
    + Sync;
/// Session `sid`'s book of wakes as its loop reads it ([`Acts::wakes`]).
type WakesFn = dyn Fn(&str) -> aterm_agent::supervise::occupancy::WakesSeen + Send + Sync;
/// Session `sid`'s loop told a wake of its book, by ordinal and episode, through a door ([`Acts::wake_told`]).
type WakeToldFn = dyn Fn(&str, u16, u64, aterm_agent::supervise::occupancy::WakeDoor) + Send + Sync;
/// Session `sid`'s loop claims a wake of its book, by ordinal and episode, for a door's line, with the refusal its worker judged ([`Acts::wake_claim`]).
type WakeClaimFn = dyn Fn(
        &str,
        u16,
        u64,
        aterm_agent::supervise::occupancy::WakeDoor,
        Option<aterm_agent::supervise::occupancy::PaceRefusal>,
    ) -> aterm_agent::supervise::occupancy::WakeClaim
    + Send
    + Sync;
/// Session `sid`'s loop gives back the claim one of its doors holds on a wake, by ordinal, episode and door ([`Acts::wake_release`]).
type WakeReleaseFn =
    dyn Fn(&str, u16, u64, aterm_agent::supervise::occupancy::WakeDoor) + Send + Sync;
/// Session `sid`'s worker went ([`Acts::wakes_left`]).
type WakesLeftFn = dyn Fn(&str) + Send + Sync;
type FollowFn = dyn Fn(&str, &mut Snapshot) -> Foreground + Send + Sync;
type StatusFn = dyn Fn(&str) -> Option<String> + Send + Sync;
type ExitLookFn = dyn Fn(&str, &Snapshot) -> Option<ExitLook> + Send + Sync;
/// The kernel's word that the process a snapshot names has ended ([`Acts::exited`]).
type ExitedFn = dyn Fn(&Snapshot, Duration) -> bool + Send + Sync;
/// The explicit restart's stamp naming the agent that left ([`Acts::asked`]).
type AskedFn = dyn Fn(&str, &Snapshot) -> Option<restart_stamp::Cause> + Send + Sync;
/// Tab `sid`'s copy of its proven self-claim's record ([`Acts::self_copy`]).
type SelfCopyFn = dyn Fn(&str) -> Option<self_claimed::Copy> + Send + Sync;
/// Whether tab `sid`'s proven self-claimed agent, the process `pid` started at `start`, is idle.
type SelfIdleFn = dyn Fn(&str, u32, &str) -> bool + Send + Sync;
/// The crash rule's relaunch ([`Acts::self_relaunch`]).
type SelfRelaunchFn = dyn Fn(&str, u32, &Snapshot, &self_claimed::Resume, Option<restart_stamp::Cause>) -> String
    + Send
    + Sync;
type SelfSnapshotFn =
    dyn Fn(&str, &self_claimed::Copy, &self_claimed::Resume) -> Option<Snapshot> + Send + Sync;
type RelaunchFn = dyn Fn(&str, u32, &Snapshot, &ExitRecord, bool, Option<restart_stamp::Cause>) -> String
    + Send
    + Sync;
/// Whether the harness's own restart ended the agent that left ([`Acts::restarted`]).
type RestartedFn =
    dyn Fn(&str, aterm_agent::harness::upgrade::Agent, Option<u32>) -> bool + Send + Sync;
/// One attempt at carrying that restart on ([`Acts::carry`]).
type CarryFn = dyn Fn(&str, u32, aterm_agent::harness::upgrade::Agent, Option<u32>, bool) -> String
    + Send
    + Sync;
/// The relaunch of an agent whose tab a crash of aterm took.
type RestoredFn = dyn Fn(&str, &Snapshot, bool, Option<&str>) -> String + Send + Sync;
/// A cold restore reopened tab `sid` from the tab `was` its agent ran in ([`upgrade_drive::note_earlier_tab`]).
type EarlierTabFn = dyn Fn(&str, &str) + Send + Sync;
/// Whether the agent the relaunch of tab `sid`'s conversation (`snap`'s) adopted is gone.
type AdoptedGoneFn = dyn Fn(&str, &Snapshot) -> Option<bool> + Send + Sync;
/// Whether the conversation `snap` names has a relaunch in flight in tab `sid` by its record ([`relaunch::in_flight_here`]).
type InFlightHereFn = dyn Fn(&str, &Snapshot) -> bool + Send + Sync;
/// One step of a restart of tab `sid` that an outgoing instance left in flight at a seamless update's Commit ([`HostHandle::resume_after_handoff`]).
type CarryInFlightFn = dyn Fn(&str, u32) -> Option<(String, upgrade::Agent)> + Send + Sync;

const CARRY_SUPERVISED: &str = "wait:supervised";
/// The owner's view of the upgrades ([`Hooks::upgrade_view`]).
type ViewFn = dyn Fn(bool, &[String], &[String], &[String]) -> ViewNext + Send + Sync;

/// What a look at the owner's view says of the next one ([`Hooks::upgrade_view`], [`look_at_upgrades`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ViewNext {
    /// The unix second a row next changes by time alone ([`upgrade_drive::View::refresh`]), if one does.
    pub(crate) at: Option<u64>,
    /// The tabs whose row follows its agent's live status ([`upgrade_drive::View::follows`]).
    pub(crate) follows: Vec<String>,
    /// The tabs a record of which is past its watch ([`upgrade_drive::View::watch_due`]).
    pub(crate) watch: Vec<String>,
    /// The records' folder the look could not list whole ([`upgrade_drive::View::unlisted`]).
    pub(crate) unlisted: Option<PathBuf>,
    /// The records the look could not read that keep a live tab as it stands ([`upgrade_drive::View::unreadable`]).
    pub(crate) unreadable: Vec<PathBuf>,
}
type RestartFn = dyn Fn(&str, u32, &Restart) -> String + Send + Sync;
/// The watch of one tab's upgrade records off any point ([`upgrade_drive::watch`]).
type WatchFn = dyn Fn(&str, u32, &upgrade_drive::Watcher) -> upgrade_drive::Watched + Send + Sync;
/// An agent's route to its API, read now from its pid, argv and directory ([`Acts::route`]).
type RouteFn = dyn Fn(u32, &[String], &str) -> Route + Send + Sync;
/// An agent's memory, measured now from its tab and pid ([`Acts::memory`]).
type MemoryFn = dyn Fn(&str, u32) -> Option<AgentMemory> + Send + Sync;
/// Whether the conversation a tab's agent holds has a task ([`upgrade_drive::Tasked::of_snapshot`], the tab's supervisor's own turns none).
type TaskedFn = dyn Fn(&str, &Snapshot, &mut upgrade_drive::Tasked) -> Option<bool> + Send + Sync;
/// Whether the conversation a tab's agent holds says its task is finished ([`upgrade_drive::Finished::of_snapshot`], the tab's supervisor's own turns none).
type FinishedFn =
    dyn Fn(&str, &Snapshot, &mut upgrade_drive::Finished) -> Option<bool> + Send + Sync;
/// The reset named by the standing usage limit of the conversation a snapshot names.
type LimitResetFn = dyn Fn(&Snapshot) -> Option<u64> + Send + Sync;
/// The tail of the transcript the Claude Code a snapshot names writes now ([`upgrade_drive::snapshot_transcript_tail`]; [`Acts::transcript_tail`]).
type TranscriptTailFn = dyn Fn(&Snapshot) -> Option<String> + Send + Sync;
/// What the repository the session a snapshot names works in holds that no remote has ([`Acts::repo_work`]).
type RepoWorkFn = dyn Fn(&Snapshot) -> aterm_agent::harness::repo_work::RepoRead + Send + Sync;
/// That transcript and the file it is ([`upgrade_drive::snapshot_transcript`]; [`Acts::transcript`]).
type TranscriptFn = dyn Fn(&Snapshot) -> Option<(PathBuf, String)> + Send + Sync;
/// A tasked session's record told its agent is gone for good ([`Acts::task_exited`]): its sid.
type TaskExitedFn = dyn Fn(&str) + Send + Sync;
/// Every session with a task record, as the startup deadline reads it ([`Acts::startups`]).
type StartupsFn = dyn Fn(&std::collections::HashSet<String>) -> Vec<Startup> + Send + Sync;
/// The startup deadline's mark on session `sid` ([`Acts::startup_mark`]).
type StartupMarkFn = dyn Fn(&str, &StartupMark<'_>) + Send + Sync;
/// The limit row the conversation of a tab's agent stands at, by its transcript.
type StandingLimitFn =
    dyn Fn(&str, &Snapshot) -> Option<aterm_agent::harness::upgrade::StandingLimit> + Send + Sync;
/// The accounts recorder at a session's usage wall ([`Acts::account_wall`]).
type AccountWallFn = dyn Fn(&str, &WalledAgent<'_>, &dyn Fn() -> bool) -> (&'static str, Option<String>)
    + Send
    + Sync;
/// What a session's screen says of its account's usage, recorded ([`Acts::account_usage`]).
type AccountUsageFn = dyn Fn(&str, &WalledAgent<'_>, &dyn Fn() -> bool, &UsageSeen, bool) -> &'static str
    + Send
    + Sync;
/// A session the accounts recorder keeps nothing of any more ([`Acts::account_gone`]).
type AccountGoneFn = dyn Fn(&str) + Send + Sync;
/// Where the accounts live for this host, and its clock ([`Acts::account_seat`]).
type AccountSeatFn = dyn Fn() -> Option<AccountSeat> + Send + Sync;
/// A process's exec environment, `KEY=value` entries ([`Acts::account_env`]).
type AccountEnvFn = dyn Fn(u32) -> Option<Vec<String>> + Send + Sync;
/// Whether an agent's settings files and the vendor's profile store leave it its stored login.
type AccountSettingsFn = dyn Fn(u32, &[String], &str) -> bool + Send + Sync;
/// [`Acts::tap_born`].
type TapBornFn = dyn Fn(u32) -> Option<i64> + Send + Sync;
/// The label of the account move whose restart signalled the agent `pid` of tab `sid` ([`Acts::account_restart`]).
type AccountRestartFn = dyn Fn(&str, u32) -> Option<String> + Send + Sync;
/// An account move made, written where the session's loop reads it ([`Acts::account_made`]).
type AccountMadeFn = dyn Fn(&str, &str, AccountAt) + Send + Sync;
/// The login a relaunch with no process to read carries ([`Acts::account_cold`]), for the snapshot the layout carried.
type AccountColdFn = dyn Fn(&Snapshot) -> Option<String> + Send + Sync;
type BackoffFn = dyn Fn(usize) -> Duration + Send + Sync;
type PauseFn = dyn Fn(Duration) -> Duration + Send + Sync;

/// The carry's clock ([`carry_in_flight_after_handoff`], [`Hooks::carry_clock`]).
#[cfg_attr(
    not(any(unix, test)),
    expect(
        dead_code,
        reason = "only unix carries restarts across a seamless handoff"
    )
)]
pub(crate) trait CarryClock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, wait: Duration);
}

/// The host's [`CarryClock`]: `Instant::now` and `std::thread::sleep`.
pub(crate) struct MonotonicClock;

impl CarryClock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, wait: Duration) {
        std::thread::sleep(wait);
    }
}
/// One look of the disk watch over the supervised sessions ([`Hooks::disk`]).
type DiskFn = dyn Fn(&[String]) + Send + Sync;
/// The Codex sessions the disk watch covers whether supervised or not ([`Hooks::codex_roster`]), by sid.
type CodexRosterFn = dyn Fn() -> Vec<String> + Send + Sync;
/// [`Hooks::log_usage`]: tab `sid`'s usage tap reading taken, its five-hour and seven-day percents.
type UsageLogFn = dyn Fn(&str, Option<u32>, Option<u32>) + Send + Sync;
/// What a host thread tells the window ([`Hooks::note`], [`HostNote`]).
type NoteFn = dyn Fn(HostNote) + Send + Sync;
/// One relaunch or upgrade row of the live acts, tab `sid`'s, under its name.
type RowFn = dyn Fn(&str, &str, &upgrade_drive::Report) + Send + Sync;

/// What the host's threads tell the window about themselves, as facts, not alerts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostNote {
    /// The tab's supervisor has failed past its restart budget and is being started again after `pause`, on a growing pause.
    SupervisorFailing {
        sid: String,
        why: String,
        pause: Duration,
    },
    /// That supervisor runs steadily again, or its worker ended.
    SupervisorSteady { sid: String },
    /// The disk watch left the home volume below the automatic floor [`DISK_SHORT_LOOKS`] passes running.
    DiskLow {
        free_gib: u64,
        floor_gib: u64,
        hogs: Vec<String>,
        /// What the ask to free space (`disk-free@v1`) did ([`disk_ask::owner_said`]).
        asked: Option<String>,
    },
    /// A `[disk]` value aterm cannot read turns every removal off.
    DiskLowCleanupOff {
        free_gib: u64,
        floor_gib: u64,
        asked: Option<String>,
    },
    /// The volume is at or above the floor.
    DiskPlenty,
    /// `aterm.toml` does not say what the owner meant by a `[disk]` removal knob.
    DiskSettingsOff { notes: Vec<String> },
    /// A pass of the disk watch freed `bytes` (`went`: the largest of what went, each a line, biggest first).
    DiskFreed { bytes: u64, went: Vec<String> },
    /// The HOME volume is full and only the owner can free it.
    DiskFull {
        free: Option<u64>,
        freed: u64,
        hogs: Vec<String>,
        cause: crate::message_reporters::DiskFullCause,
        thresholds: aterm_agent::harness::disk::Thresholds,
        /// What the ask to free space (`disk-free@v1`) did ([`disk_ask::owner_said`]).
        asked: Option<String>,
    },
    /// The volume is back above the emergency line: the alert ends.
    DiskFullOver,
    /// macOS's program check is stuck ([`crate::program_check_watch`]).
    ProgramCheckStuck { workers: u64, limit: Option<u64> },
    /// The program check is not stuck.
    ProgramCheckUnstuck,
    /// The disk watch looked ([`look_at_disk`], once a [`DISK_TICK`]).
    DiskLooked,
    /// No `[disk]` value turns removal off.
    DiskSettingsRead,
    /// The live upgrade cannot run at all in this tab (`step` names why: its state folder, `dir`, cannot hold the lock).
    UpgradeCannotRun {
        sid: String,
        step: String,
        dir: Option<PathBuf>,
    },
    /// The upgrade runs in this tab (said at a run's first fine step and when it recovers), or its worker ended.
    UpgradeRuns { sid: String },
    /// The folder the upgrades' records are kept in, as the owner's view last listed it ([`upgrade_drive::View::unlisted`]).
    UpgradeRecords { unlisted: Option<PathBuf> },
    /// The upgrades' records the owner's view cannot read that keep a live tab as it stands.
    UpgradeRecordsUnreadable { files: Vec<PathBuf> },
    /// A rating survey the loop cannot close is open in the tab.
    SurveyStuck {
        sid: String,
        kind: aterm_agent::supervise::StuckSurvey,
        why: String,
    },
    /// No survey is open in the tab (the loop's first look saw none, or the card left), or its worker ended.
    SurveyEnded { sid: String },
    /// What the tab's occupancy door says now, once a change ([`IdleHost::occupancy_said`]).
    OccupancySaid {
        sid: String,
        said: aterm_agent::supervise::occupancy::OccupancySaid,
    },
    /// The tab's wake K Entry moved: a ready wake of its book stood K looks untold, or no longer
    /// does ([`crate::occupancy_wakes::idle_of`] says which).
    WakeIdle { sid: String },
    /// The tab's resume loop found a point only a person can answer ([`IdleHost::exception`]).
    ResumeException {
        sid: String,
        agent: Program,
        standing: Option<aterm_agent::supervise::policy::exception::Exception>,
    },
    /// The tab's lost login is back ([`IdleHost::login_back`], told once a loss), or the tab moved
    /// onto another login that works (an account move that landed, [`WorkerIdle::restart`]).
    LoginBack { sid: String },
    /// The tab's usage or model limit ended with no sign-in that took ([`IdleHost::limit_over`]:
    /// the reset continued it, a `/clear` left it, another wall took its place).
    LimitOver { sid: String },
    /// The stall canary's second rung ([`IdleHost::stall_unread`]).
    ResumeUnread {
        sid: String,
        agent: Program,
        stall: Option<StallSeen>,
    },
    /// What the tab's relaunch has to say about itself ([`RelaunchSay`]).
    Relaunch { sid: String, said: RelaunchSay },
    /// What the Claude Code accounts as a whole say ([`AccountsSaid`]): the machine's fact and no tab's.
    Accounts(AccountsSaid),
    /// What the other CLIs' pools this machine keeps say ([`PoolSaid`]), every one in one note:
    /// the machine's fact and no tab's. None is a note only after some pool said something.
    Pools(Vec<PoolSaid>),
    /// The tab's supervisor informs a person ([`IdleHost::inform`]).
    Inform { sid: String, text: String },
    /// The tab's supervisor warns a person ([`IdleHost::warn`], the memory ceiling's).
    Warn { sid: String, text: String },
    /// What a layout's leaf carries of the agent the host publishes for the tab moved.
    AgentPublished { sid: String },
    /// The usage tap of the tab's Claude Code went silent or is heard again ([`WorkerIdle::usage_tap`]).
    UsageTap { sid: String, silent: bool },
}

/// What the host says of the accounts as a whole ([`HostNote::Accounts`]; the look is [`accounts_said`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AccountsSaid {
    /// No roster, none listed, or `[harness] accounts = "off"` with `logins` off (or over a roster with no email to renew by).
    Nothing,
    /// The roster does not admit: its line stands, and every line of [`Self::Listed`] ends.
    Refused,
    /// The roster admits, but this machine's logins (`logins.json`, which binds each row to the login a renewal made it) cannot be read as this build's.
    LoginsUnread,
    /// The roster admits (or, with none and `[harness] usage_notes` on, the machine's own row stands for it).
    Listed {
        /// The signed-out line: accounts with no email whose login is known signed out
        /// (`accounts::signed_out`), which no sign-in route can sign in.
        signed_out: Vec<String>,
        /// The credits line: accounts usage credits carry (`accounts::credits_noted`).
        credits: Vec<String>,
        /// The week's line: accounts whose week is nearly used with no credits.
        window_full: Vec<(String, String)>,
        /// The due line: renewable logins due or ended (`accounts::renew_due`), which the upkeep renews by itself.
        due: Vec<String>,
        /// The warning that no renewable login is left working within a day (`accounts::none_working_within`).
        none_soon: bool,
        /// The line of renewals that answered another account (`accounts::wrong_accounts`).
        wrong: Vec<String>,
        /// The rows whose automatic sign-in failed and that still need one (no login to run on,
        /// or one due for renewal), each with the routes it tried (`accounts::failing_sign_ins`):
        /// a fact for later, since aterm tries each again by itself.
        no_sign_in: Vec<(String, String)>,
    },
}

impl AccountsSaid {
    /// Whether a line of the digest stands for this.
    const fn stands(&self) -> bool {
        match self {
            Self::Nothing => false,
            Self::Refused | Self::LoginsUnread => true,
            Self::Listed {
                signed_out,
                credits,
                window_full,
                due,
                none_soon,
                wrong,
                no_sign_in,
            } => {
                !signed_out.is_empty()
                    || !credits.is_empty()
                    || !window_full.is_empty()
                    || !due.is_empty()
                    || *none_soon
                    || !wrong.is_empty()
                    || !no_sign_in.is_empty()
            }
        }
    }

    /// What a roster that admits (or the machine's own row standing for none) says.
    fn listed(
        roster: &accounts::Roster,
        state: &accounts::State,
        logins: &accounts::Logins,
        now: i64,
        carried: &BTreeSet<String>,
    ) -> AccountsSaid {
        let enrolled = accounts::enrolled(roster);
        let open = |labels: Vec<String>| -> Vec<String> {
            labels
                .into_iter()
                .filter(|label| !carried.contains(label))
                .collect()
        };
        AccountsSaid::Listed {
            signed_out: open(accounts::signed_out(roster, state, &enrolled, now)),
            credits: accounts::credits_noted(roster, state, now),
            window_full: accounts::windows_full(roster, state, now),
            due: open(accounts::renew_due(roster, logins, state, &enrolled, now)),
            none_soon: accounts::none_working_within(
                roster,
                logins,
                state,
                &enrolled,
                now,
                accounts::NONE_SOON_S,
            ),
            wrong: accounts::wrong_accounts(roster, logins),
            no_sign_in: accounts::failing_sign_ins(roster, state, logins, now)
                .into_iter()
                .map(|(label, attempt)| (label, attempt.routes))
                .collect(),
        }
    }
}

/// What one other CLI's pool says to the digest ([`HostNote::Pools`], [`pools_said`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PoolSaid {
    /// The CLI.
    pub(crate) cli: accounts::Cli,
    /// Its renewable logins due or ended (`accounts::renew_due`).
    pub(crate) due: Vec<String>,
    /// Its renewals that answered another account (`accounts::wrong_accounts`).
    pub(crate) wrong: Vec<String>,
    /// Its pending human sign-in steps, kept across restarts in the pool's bindings.
    pub(crate) sign_in: Vec<(String, accounts::SignInAction)>,
}

/// What a tab's relaunch says ([`HostNote::Relaunch`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RelaunchSay {
    Failing {
        /// The cause a person clears, when it is one.
        blocked_by: Option<&'static str>,
        /// The relaunch is aterm's own restart's, carried.
        restarted: bool,
        /// The command the tab's mark names, which starts the agent by hand.
        command: Option<String>,
    },
    Limited {
        why: String,
    },
    Cannot {
        /// The refusal in a person's words.
        reason: String,
        restarted: bool,
        /// `claude --resume <id>` (with its carried flags, and the Claude home its agent ran under), or a Codex's `codex resume <thread>`.
        command: Option<String>,
        /// The word is the tab's mark ([`Hooks::badge`]).
        marked: bool,
        /// The conversation it is about is known ([`Kept::lost`]).
        lost: bool,
        /// It ran as its tab's own program ([`resume_place`]).
        new_tab: bool,
        /// What a person does where resuming it is no remedy ([`cannot_then`]).
        then: Option<&'static str>,
    },
    SteppedDown {
        reason: String,
        restarted: bool,
        command: Option<String>,
    },
    Back,
    Ended,
    NotRestored,
}

/// The host's seams onto the process.
#[derive(Clone)]
pub(crate) struct Hooks {
    pub(crate) roster: Arc<RosterFn>,
    /// The sessions' own Autopilot words, every live session's, an agent's or not.
    pub(crate) autopilot: Arc<WordsFn>,
    /// Whether session `sid` is still its worker's after a failed run.
    pub(crate) still_wanted: Arc<StillFn>,
    pub(crate) body: Arc<BodyFn>,
    /// The tab's mark of a relaunch's word (`Some`: raised, `None`: lowered), under its own owner, which no loop's start ends.
    pub(crate) badge: Arc<BadgeFn>,
    /// Where the loop's next try goes ([`IdleHost::waiting`], the tab's retry plan).
    pub(crate) waiting: Arc<WaitingFn>,
    /// What the harness just did, and the task's face.
    pub(crate) fact: Arc<FactFn>,
    /// The worker's needs you, whole, stored where its entry lands.
    pub(crate) needs: Arc<NeedsFn>,
    /// Why land still holds, stored where the window reads it.
    pub(crate) hold: Arc<LandHoldFn>,

    pub(crate) claim_epoch: Arc<EpochFn>,
    pub(crate) acts: Acts,
    pub(crate) backoff: Arc<BackoffFn>,
    /// The length a named pause is waited.
    pub(crate) pause: Arc<PauseFn>,
    /// The clock the successor's carry of the restarts left in flight reads and waits on ([`CarryClock`]; the host's [`MonotonicClock`]).
    #[cfg_attr(
        not(any(unix, test)),
        expect(
            dead_code,
            reason = "only unix carries restarts across a seamless handoff"
        )
    )]
    pub(crate) carry_clock: Arc<dyn CarryClock>,
    /// The owner's view of this instance's upgrades ([`upgrade_drive::View`]).
    pub(crate) upgrade_view: Arc<ViewFn>,
    /// The disk watch ([`look_at_disk`]), the host's one timer.
    pub(crate) disk: Arc<DiskFn>,
    /// The disk watch's codex sessions, supervised or not ([`disk_roster`]).
    pub(crate) codex_roster: Arc<CodexRosterFn>,
    /// What the workers and the disk watch tell the window about themselves ([`HostNote`]).
    pub(crate) note: Arc<NoteFn>,
    /// The usage tap's session log mark ([`WorkerIdle::tap_step`]).
    pub(crate) log_usage: Arc<UsageLogFn>,
}

/// The worker's acts beyond the loop, each over one session (`sid`).
#[derive(Clone)]
pub(crate) struct Acts {
    pub(crate) open: Arc<WantedFn>,
    /// Whether tab `sid`'s agent still holds it.
    pub(crate) holds: Arc<HoldsFn>,
    /// When tab `sid`'s last command started, unix ms, while no person's gesture has reached the tab since.
    pub(crate) launch: Arc<LaunchFn>,
    pub(crate) due: Arc<DueFn>,
    /// The session is behind — from its worker's attach, and from each activation notice.
    pub(crate) behind: Arc<BehindFn>,
    pub(crate) step: Arc<StepFn>,
    /// The upgrade's step at a break of the agent's own background work.
    pub(crate) notice: Arc<StepFn>,
    pub(crate) owed: Arc<OwedFn>,
    /// The owed carry-on, at the loop's idle point ([`WorkerIdle::at_idle_on`]).
    pub(crate) resume: Arc<ResumeFn>,
    /// The carry-on [`Self::owed`] names, settled with nothing typed where the agent rests at a point the tab's loop withheld from this host.
    pub(crate) settle: Arc<SettleFn>,
    /// The disk resume ([`WorkerIdle::at_idle`]).
    pub(crate) disk_resume: Arc<DiskResumeFn>,
    /// The ask to free disk space ([`WorkerIdle::at_idle`], `harness_disk_ask.rs`).
    pub(crate) disk_free: Arc<DiskFreeFn>,
    /// The disk board the worker reads its session's episode from ([`disk_withholds`], [`disk_owed`]) and spends the resume on ([`disk_resume_stepped`]).
    pub(crate) disk_board: &'static Shelf,
    /// The jobs board the worker reads its agent's background calls from ([`WorkerIdle::background_jobs`], the stall canary's evidence).
    pub(crate) jobs_board: &'static crate::vitals_jobs::JobsBoard,
    /// Whether a call's leader still lives at its birth ([`WorkerIdle::facts`]' re-proof): live [`crate::vitals_jobs::leader_lives`], a test's own in tests.
    pub(crate) leader_lives: fn(u32, u64) -> bool,
    /// The program check's board the worker reads whether macOS's program check is stuck from ([`WorkerIdle::program_check_stuck`]).
    pub(crate) program_check: &'static Mutex<crate::program_check_watch::Board>,
    /// The occupancy facts board the worker reads the machine from ([`WorkerIdle::occupancy`], the occupancy door's reading).
    pub(crate) occupancy: Arc<OccupancyFn>,
    /// The board's pace and reservation table the worker takes a fact line's pace from ([`WorkerIdle::occupancy_pace`], live [`crate::occupancy_host::pace_for`]).
    pub(crate) occupancy_pace: Arc<OccupancyPaceFn>,
    /// The task record's fact word the worker's task door notes or clears ([`WorkerIdle::task_fact`], live [`task_fact_live`]).
    pub(crate) task_fact: Arc<TaskFactFn>,
    /// The session's book of wakes the worker's loop reads ([`WorkerIdle::wakes`], live [`crate::occupancy_wakes::seen_for`]): from that read on the session is hosted, and an answer written waits for the loop to tell it heard.
    pub(crate) wakes: Arc<WakesFn>,
    /// A wake the worker's loop told ([`WorkerIdle::wake_told`], live [`crate::occupancy_wakes::told_for`]): an answer heard, or a claimed line written.
    pub(crate) wake_told: Arc<WakeToldFn>,
    /// The worker's loop claims a wake for its line ([`WorkerIdle::wake_claim`], live [`crate::occupancy_wakes::claim_for`]).
    pub(crate) wake_claim: Arc<WakeClaimFn>,
    /// The worker's loop gives back a claim its line did not use ([`WorkerIdle::wake_release`], live [`crate::occupancy_wakes::release_for`]).
    pub(crate) wake_release: Arc<WakeReleaseFn>,
    /// The worker went: no loop is left to tell its session's answers heard ([`WorkerIdle`]'s drop, live [`crate::occupancy_wakes::left`]).
    pub(crate) wakes_left: Arc<WakesLeftFn>,
    pub(crate) snapshot: Arc<SnapshotFn>,
    /// How long an idle point waits for its relaunch snapshot read ([`refresh_snapshot_at_point`]):
    /// [`POINT_READ_BOUND`] live; `Acts::inert`'s a hang detector's minute, so a test's read lands
    /// before its point goes on unless the test shortens the wait.
    pub(crate) point_read: Duration,
    pub(crate) follow: Arc<FollowFn>,
    pub(crate) status: Arc<StatusFn>,
    /// One look at an agent that left.
    pub(crate) exit_look: Arc<ExitLookFn>,
    /// The kernel's word that the agent ended, waited for at most the time given.
    pub(crate) exited: Arc<ExitedFn>,
    /// The explicit restart's stamp naming the agent that left ([`restart_stamp::lookup`]).
    pub(crate) asked: Arc<AskedFn>,
    /// Whether the explicit restart would be carried for tab `sid`'s agent now.
    pub(crate) offered: Arc<OfferedFn>,
    pub(crate) relaunch: Arc<RelaunchFn>,
    /// Aterm's copy of a proven self-claimed codex-alab's session record for tab `sid` ([`crate::self_claim::copy_of_tab`]: read only while its identity was live).
    pub(crate) self_copy: Arc<SelfCopyFn>,
    /// That copy only while its exact proven claim is currently live ([`crate::self_claim::live_copy`]).
    pub(crate) live_self_copy: Arc<SelfCopyFn>,
    /// The watch-only worker's idle point beside a proven self-claim ([`WorkerIdle::beside_idle`]).
    pub(crate) self_idle: Arc<SelfIdleFn>,
    /// The crash rule's relaunch of such an agent, on the line its copy validated ([`upgrade_drive::self_claimed_after_exit`]).
    pub(crate) self_relaunch: Arc<SelfRelaunchFn>,
    /// The snapshot of such an agent no read reached while it ran ([`relaunch::self_claimed_snapshot`]).
    pub(crate) self_snapshot: Arc<SelfSnapshotFn>,
    /// Whether the harness's own restart (the upgrade's or the restart in place's, left in flight
    /// by its step) ended the agent that left ([`relaunch::restarted`], by the snapshot's pid).
    pub(crate) restarted: Arc<RestartedFn>,
    /// One attempt at carrying that restart on ([`relaunch::carry_restart`]).
    pub(crate) carry: Arc<CarryFn>,
    /// The relaunch of a restored tab's agent after aterm itself ended ([`HostHandle::relaunch_restored`]).
    pub(crate) relaunch_restored: Arc<RestoredFn>,
    /// The tab a restored agent ran in, noted beside its reopened tab's ledger as the restore hands it over.
    pub(crate) earlier_tab: Arc<EarlierTabFn>,
    /// Whether a restored landing's adopted agent is gone, by the conversation's record.
    pub(crate) adopted_gone: Arc<AdoptedGoneFn>,
    /// Whether a restored step that panicked may have typed its line, by the conversation's record.
    pub(crate) in_flight_here: Arc<InFlightHereFn>,
    /// One step of a restart the outgoing instance left in flight at a seamless update's Commit.
    #[cfg_attr(not(any(unix, test)), allow(dead_code))]
    pub(crate) carry_in_flight: Arc<CarryInFlightFn>,
    pub(crate) restart: Arc<RestartFn>,
    /// Whether the conversation the session's agent holds has a task (a person's or an
    /// orchestrator's prompt; the harness's own turns are none, [`WorkerIdle::taskless`]).
    pub(crate) tasked: Arc<TaskedFn>,
    /// Whether the conversation the session's agent holds says its task is finished.
    pub(crate) finished: Arc<FinishedFn>,
    /// The usage limit's reset the conversation's transcript names, past or to come ([`WorkerIdle::limit_until`]).
    pub(crate) limit_reset: Arc<LimitResetFn>,
    /// The transcript that confirms a wall ([`WorkerIdle::transcript_tail`]).
    pub(crate) transcript_tail: Arc<TranscriptTailFn>,
    /// What the session's repository holds that no remote has.
    pub(crate) repo_work: Arc<RepoWorkFn>,
    /// The transcript a task's result is snapshotted from.
    pub(crate) transcript: Arc<TranscriptFn>,
    /// The task is over with its agent: a tasked session whose agent left and that nothing brings back.
    pub(crate) task_exited: Arc<TaskExitedFn>,
    /// The deadlines' look ([`look_at_startups`]): every session of this instance with a task record.
    pub(crate) startups: Arc<StartupsFn>,
    /// The deadlines' mark ([`StartupMark`]).
    pub(crate) startup_mark: Arc<StartupMarkFn>,
    /// The limit only the transcript shows ([`WorkerIdle::standing_limit`]).
    pub(crate) standing_limit: Arc<StandingLimitFn>,
    /// The session's own usage wall, recorded for its account ([`WorkerIdle::limited`]).
    pub(crate) account_wall: Arc<AccountWallFn>,
    /// The fleet accounts' upkeep, as injected.
    pub(crate) accounts_upkeep: AccountsUpkeep,
    /// What the session's screen says of its account's usage, recorded for the account ([`WorkerIdle::usage_seen`]).
    pub(crate) account_usage: Arc<AccountUsageFn>,
    /// The session is over for the accounts recorder (`accounts::record_gone`).
    pub(crate) account_gone: Arc<AccountGoneFn>,
    /// Where the accounts live, and the time ([`AccountSeat`]).
    pub(crate) account_seat: Arc<AccountSeatFn>,
    /// The agent's own exec environment by its pid (`KERN_PROCARGS2`).
    pub(crate) account_env: Arc<AccountEnvFn>,
    /// Whether the agent's settings leave it its stored login.
    pub(crate) account_settings: Arc<AccountSettingsFn>,
    /// When the kernel started process `pid`, in Unix seconds (`aterm_uds::process::read_process_birth`).
    pub(crate) tap_born: Arc<TapBornFn>,
    /// The label of the account move whose restart signalled the agent (`relaunch::account_restart`: the restart's own record, in flight or just stopped).
    pub(crate) account_restart: Arc<AccountRestartFn>,
    /// An account move the host landed itself.
    pub(crate) account_made: Arc<AccountMadeFn>,
    /// The login a relaunch with no process to read carries ([`account_cold_at`]).
    pub(crate) account_cold: Arc<AccountColdFn>,
    /// The live upgrade's clocks held through the loop's limit episode ([`upgrade_drive::hold_clock`], [`WorkerIdle::limited`]).
    pub(crate) hold: Arc<HoldFn>,
    /// The route a Claude Code agent's API requests take.
    pub(crate) route: Arc<RouteFn>,
    /// The memory ceiling's measure of a tab's agent by its pid ([`WorkerIdle::memory`]).
    pub(crate) memory: Arc<MemoryFn>,
    /// The watch: one look at a tab's upgrade records off any point ([`upgrade_drive::watch`]), a dry-run read that writes.
    pub(crate) watch: Arc<WatchFn>,
}

/// What a worker hands the accounts recorder of its Claude Code at a usage wall ([`Acts::account_wall`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WalledAgent<'a> {
    /// The agent's snapshot: its pid, whose exec environment names the login it runs under.
    pub(crate) snap: &'a Snapshot,
    /// The conversation it holds: the transcript the wall stands in.
    pub(crate) session: &'a str,
    /// `[harness] usage_notes`, read live.
    pub(crate) own: bool,
}

/// Mark a stop without waking the worker or invoking its connection callback.
fn mark_stop(stop: &AtomicBool, left: &ExitWord) {
    left.stop();
    stop.store(true, Ordering::SeqCst);
}

/// Stop one worker: its flag, then its connection's cut.
fn request_stop(stop: &AtomicBool, left: &ExitWord, interrupt: &Mutex<Option<Interrupter>>) {
    mark_stop(stop, left);
    ring();
    if let Some(cut) = interrupt
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        cut();
    }
}

/// Preserve the worker's exit custody when a seamless Commit stops it.
#[cfg(any(unix, test))]
fn mark_commit_stop(kept: &mut Kept, left: &ExitWord, left_at_commit: &AtomicBool) {
    let word = left.read();
    if word != Word::Stopped {
        kept.commit = Committed::Stopped;
        if word == Word::Left {
            left_at_commit.store(true, Ordering::SeqCst);
        }
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

/// One tab's act that the host thread runs, caught: `act`, done for tab `sid` on the host's own pass.
fn host_caught<T>(sid: &str, what: &str, act: impl FnOnce() -> T) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(act))
        .map_err(|payload| {
            aterm_log::warn!(
                "harness @{sid}: {what} stopped after an internal panic on the host thread: {}",
                panic_text(payload.as_ref())
            );
        })
        .ok()
}

/// One tab's part of a host pass, caught and asked again on the host's back-off.
fn tab_caught<T>(
    shared: &Shared,
    sid: &str,
    what: &'static str,
    hooks: &Hooks,
    act: impl FnOnce() -> T,
) -> Option<T> {
    let faults = || {
        shared
            .host_faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    };
    let waiting = faults()
        .get(sid)
        .and_then(|faulted| faulted.get(what))
        .is_some_and(|fault| Instant::now() < fault.again_at);
    if waiting {
        return None;
    }
    let done = host_caught(sid, what, act);
    if done.is_some() {
        let mut faults = faults();
        if let Some(faulted) = faults.get_mut(sid) {
            faulted.remove(what);
            if faulted.is_empty() {
                faults.remove(sid);
            }
        }
        return done;
    }
    let panics = faults()
        .get(sid)
        .and_then(|faulted| faulted.get(what))
        .map_or(0, |fault| fault.panics)
        .saturating_add(1);
    let wait = (hooks.backoff)(panics);
    aterm_log::warn!(
        "harness @{sid}: {what} is asked again for this tab in {wait:?} ({panics} panic(s) in a row)"
    );
    let again_at = Instant::now() + wait;
    faults()
        .entry(sid.to_string())
        .or_default()
        .insert(what, HostFault { panics, again_at });
    None
}

/// One tab's stage of a host pass that panicked ([`tab_caught`]).
#[derive(Clone, Copy, Debug)]
struct HostFault {
    panics: usize,
    again_at: Instant,
}

/// The earliest instant after `now` a tab's stage that panicked is asked again ([`tab_caught`]).
fn host_faults_due(shared: &Shared, now: Instant) -> Option<Instant> {
    shared
        .host_faults
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .flat_map(|faulted| faulted.values().map(|fault| fault.again_at))
        .filter(|at| *at > now)
        .min()
}

/// An act no tab owns, caught, wherever it runs: the window's own acts on the host thread.
fn caught<T>(what: &str, then: &str, act: impl FnOnce() -> T) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(act))
        .map_err(|payload| {
            aterm_log::warn!(
                "harness: {what} stopped after an internal panic; {then}: {}",
                panic_text(payload.as_ref())
            );
        })
        .ok()
}

/// How long a worker waits before its `n`-th restart (1-based).
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

/// One worker thread's life.
fn worker_main(job: WorkerJob, hooks: &Hooks) -> WorkerExit {
    let (sid, agent) = (job.sid.clone(), job.agent);
    let exit = worker_run(job, hooks);
    end_notes(hooks, &sid, agent);
    exit
}

/// The facts a worker told the window about itself end with it ([`HostNote`]).
fn end_notes(hooks: &Hooks, sid: &str, agent: Program) {
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
        agent,
        standing: None,
    });
    (hooks.note)(HostNote::ResumeUnread {
        sid: sid.to_string(),
        agent,
        stall: None,
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
    if let Some(until) = job.watch_until {
        if wait_done(&job.stop, until) {
            return stopped(&job, hooks);
        }
        job.watch_until = None;
    }
    let mut over = 0usize;
    let mut noted = false;
    loop {
        if job.stop.load(Ordering::SeqCst) || !job.admitted() {
            return stopped_beside(&job, hooks);
        }
        if watches_only(job.agent) {
            *job.beside.lock().unwrap_or_else(PoisonError::into_inner) =
                (hooks.acts.status)(&job.sid)
                    .as_deref()
                    .and_then(proven_self_claim);
        }
        let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (hooks.body)(&job)))
            .unwrap_or_else(|payload| BodyEnd::Failed(panic_text(payload.as_ref())));
        job.clear_badge = false;
        job.unset_mark = false;
        *job.interrupt.lock().unwrap_or_else(PoisonError::into_inner) = None;
        (hooks.waiting)(&job.sid, None);
        (hooks.fact)(&job.sid, crate::HarnessFact::Task(None));
        (hooks.fact)(&job.sid, crate::HarnessFact::Pause(None));
        if (hooks.hold)(&job.sid, None) {
            (hooks.fact)(&job.sid, crate::HarnessFact::LandHeld);
        }
        if job.stop.load(Ordering::SeqCst) {
            return stopped_beside(&job, hooks);
        }
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
                    "harness @{}: the proven self-claim is gone; watching only (aterm \
                     supervises no Codex) — its stall and memory remedies stand",
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

/// [`stopped`], said as the watch-only worker's end where it watched beside a proven self-claim ([`WorkerExit::SelfClaimed`]: never a `host.held` entry).
fn stopped_beside(job: &WorkerJob, hooks: &Hooks) -> WorkerExit {
    let exit = stopped(job, hooks);
    if exit == WorkerExit::Stopped && job.beside().is_some() {
        WorkerExit::SelfClaimed
    } else {
        exit
    }
}

/// A stopped worker's end.
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

/// A worker that reads its agent's exit alone.
fn exit_main(job: WorkerJob, hooks: &Hooks) -> WorkerExit {
    let exit = stopped(&job, hooks);
    end_notes(hooks, &job.sid, job.agent);
    exit
}

/// Whether the agent's exit is the worker's to read.
fn exit_handed(job: &WorkerJob) -> bool {
    job.left.is(Word::Left) || job.left_at_commit.load(Ordering::SeqCst)
}

/// The worker's first act, before its loop.
fn attach(job: &WorkerJob, hooks: &Hooks) {
    if !upgrades(job.agent) && !relaunches(job.agent) {
        return;
    }
    with_kept(job, |k| k.carry_asked = false);
    let due = if job.switches.upgrade() && upgrades(job.agent) {
        (hooks.acts.due)(&job.sid)
    } else {
        Due::No
    };
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
    if looks {
        owe_note(&job.note, crate::upgrade_host::now_s());
        if matches!(
            ask_note(&job.sid, &job.note, hooks),
            Some(Behind::Busy | Behind::Unread(_))
        ) {
            ring();
        }
    }
    refresh_snapshot(&job.sid, &job.kept, hooks);
    let incomplete = with_kept(job, |k| {
        k.snapshot.as_ref().is_none_or(|s| s.session.is_none())
    });
    if incomplete || (hooks.acts.owed)(&job.sid, carries_only(job.agent)) || looks {
        job.park.store(true, Ordering::SeqCst);
    }
}

/// Whether the live upgrade is written for `agent`.
const fn upgrades(agent: Program) -> bool {
    aterm_agent::harness::lanes::lanes(agent).upgrade
}

/// Whether the relaunch on exit is written for `agent` ([`on_agent_left`]).
const fn relaunches(agent: Program) -> bool {
    aterm_agent::harness::lanes::lanes(agent).relaunch
}

/// Whether this host carries `agent`'s relaunch on exit at its idle point ([`WorkerIdle::idle_step`]).
fn host_carries_relaunch(agent: Program) -> bool {
    relaunches(agent) && !aterm_agent::harness::lanes::sweep_carries_relaunch(agent)
}

/// The one agent whose owed relaunch `agent`'s worker carries ([`Acts::owed`], [`CarryPoint::only`]).
fn carries_only(agent: Program) -> Option<upgrade::Agent> {
    if host_carries_relaunch(agent) {
        upgrade::Agent::of(agent)
    } else {
        None
    }
}

/// Whether the host reads `agent`'s conversation through its transcript.
const fn reads_transcript(agent: Program) -> bool {
    match agent {
        Program::Claude => true,
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => false,
    }
}

/// The pooled CLI whose roster and store a session of `agent` reads its accounts from
/// (`accounts.toml` for Claude Code, `accounts-<cli>.toml` for Codex, Grok Build and Kimi Code):
/// what its Account fact, menu and a11y node name. A program no pool keeps is `None`. Whether
/// the host records or moves its accounts is [`accounts_for`]'s alone.
pub(crate) const fn pool_cli(agent: Program) -> Option<accounts::Cli> {
    accounts::Cli::of(agent)
}

/// Whether accounts state is recorded for `agent` by the host's own flow, from its transcripts
/// and `/usage` reads ([`WorkerIdle::account_ctx`], [`account_gone`]): Claude Code's pool alone.
/// A session of every other pooled CLI is asked of by [`session_pool`].
const fn accounts_for(agent: Program) -> bool {
    matches!(accounts::Cli::of(agent), Some(accounts::Cli::Claude))
}

/// The pool a running session of `agent`, which `supervisor` answers, is advised from by
/// [`aterm_agent::harness::accounts_pool::session_move`]: a pooled CLI other than Claude Code
/// (whose flow is [`accounts_for`]'s), and none for a session that answers its own walls, as a
/// self-claimed codex-alab does ([`accounts::StayWhy::OtherSupervisor`]).
const fn session_pool(agent: Program, supervisor: accounts::Supervisor) -> Option<accounts::Cli> {
    match (accounts::Cli::of(agent), supervisor) {
        (Some(accounts::Cli::Claude) | None, _) | (Some(_), accounts::Supervisor::Own) => None,
        (Some(cli), accounts::Supervisor::Aterm) => Some(cli),
    }
}

/// Whether codex-alab's own attention can name the startup screen `agent` holds: codex's
/// alone (OpenAI's codex shares the name); another agent's screen is that agent's.
const fn codex_alab_names_screen(agent: Program) -> bool {
    match agent {
        Program::Codex => true,
        Program::Claude
        | Program::Agy
        | Program::Muse
        | Program::Grok
        | Program::Kimi
        | Program::Generic => false,
    }
}

const BACKGROUND_LOOK: Duration = Duration::from_secs(60);

/// A session's note behind still to be taken.
#[derive(Clone, Copy, Debug)]
struct Owed {
    since: u64,
    unread: u32,
    asked: Option<Instant>,
    by: Option<Instant>,
    flying: bool,
    skipped: bool,
    reowed: bool,
}

/// A worker's owed note ([`Owed`]).
type Note = Arc<Mutex<Option<Owed>>>;

const NOTE_AGAIN: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

const NOTE_GAP: Duration = Duration::from_secs(1);

/// Owe the session's note behind since `since` — the earlier second kept when one is owed already — to be asked at once.
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

/// An owed note's ask lands ([`Owed::flying`] cleared): whether the host is to be rung for it.
fn land(owed: &mut Option<Owed>) -> (bool, bool) {
    owed.as_mut().map_or((false, false), |o| {
        o.flying = false;
        let reowed = std::mem::take(&mut o.reowed);
        (std::mem::take(&mut o.skipped) || reowed, reowed)
    })
}

/// An ask in flight ([`ask_note`]) that has not landed: an act that panics lands it as it unwinds.
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

/// Ask the session's owed note once ([`Acts::behind`], with no lock held over the ask) and keep what it decides.
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

/// Query a worker's owed note at a wake ([`Owed`]) once its instant arrives or the worker ends.
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
    if asks && host_caught(sid, "its note behind", || ask_note(sid, note, hooks)).is_none() {
        *note.lock().unwrap_or_else(PoisonError::into_inner) = None;
        return None;
    }
    host_wake(note)
}

/// The instant the host is to wake for a worker's owed note ([`host_note`]).
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
    waits: u32,
    attended: u32,
    same: u32,
    last: String,
    broken: bool,
    runs_told: bool,
    unread: u32,
    refused: u32,
    activations: u64,
    warned: Vec<String>,
    quiet: bool,
}

const UNKNOWN_WORDS_WARNED: usize = 8;

impl UpgradeRun {
    /// Whether the step that said `step` leaves the upgrade owning the session's turn ends ([`upgrade_drive::owns_turn_ends_as`], under [`Self::quiet`]).
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

    /// Whether `step` is the same person's hold as the step before it at this point (`wait:attended` again, no turn between).
    fn repeats(&self, step: &str) -> bool {
        upgrade_drive::attended(step) && self.attended > 0 && self.last == step
    }

    /// [`upgrade_drive::after`] for one step's word, the counts kept.
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

    /// An act of the upgrade's at a break of the agent's own background work ([`WorkerIdle::at_background`]: a notice, a release, a give-up, a re-arm).
    fn acted(&mut self) {
        self.waits = 0;
        self.refused = 0;
        self.same = 0;
        self.last.clear();
        self.attended = 0;
    }

    /// A word since the last step.
    fn worded(&mut self) {
        self.waits = 0;
        self.refused = 0;
        self.same = 0;
        self.last.clear();
    }

    /// A turn ran since the last look.
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

    /// A look whose read failed: the pause before the next one ([`UNREAD_LOOK`]), the step's own counts untouched.
    fn unread(&mut self) -> Duration {
        let pause = aterm_agent::supervise::ladder::Ladder(&UNREAD_LOOK)
            .step(usize::try_from(self.unread).unwrap_or(usize::MAX));
        self.unread = self.unread.saturating_add(1);
        pause
    }
}

/// What the harness typed into the conversation `snap` names.
fn conversation_typed(
    o: &upgrade_drive::Opts,
    sid: &str,
    snap: &Snapshot,
) -> Vec<aterm_agent::supervise::approvals::Typed> {
    upgrade_drive::conversation_typed(o, sid, snap.session.as_deref(), &[])
}

const LIMIT_READ_AGAIN: Duration = Duration::from_secs(60);

/// The live [`Acts::limit_reset`]'s read of `session`, kept.
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

const UNREAD_LOOK: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// What the worker's loop last offered its host and what it withheld.
#[derive(Debug, Default)]
pub(crate) struct Points {
    /// [`Guard::code`] of the last guard said.
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

    /// The last guard said and its second (`None`: none), and the last point offered (`0`: none).
    pub(crate) fn last(&self) -> (Option<(Guard, u64)>, u64) {
        let guard = Guard::from_code(self.guard.load(Ordering::SeqCst))
            .map(|g| (g, self.guard_at.load(Ordering::SeqCst)));
        (guard, self.point_at.load(Ordering::SeqCst))
    }
}

/// The worker's part at its loop's idle points ([`IdleHost`]).
pub(crate) struct WorkerIdle {
    sid: String,
    agent: Program,
    stop: Arc<AtomicBool>,
    grace: u32,
    park: Arc<AtomicBool>,
    look_at: Arc<Mutex<Option<Instant>>>,
    acting: Arc<AtomicBool>,
    /// The stamp of the last step this worker took that may end its agent.
    stepped: Arc<AtomicU64>,
    switches: Arc<Switches>,
    kept: Arc<Mutex<Kept>>,
    hooks: Hooks,
    run: Mutex<UpgradeRun>,
    /// Count of activation notices from the owner and the agent ([`HostHandle::note_activation`]).
    activations: Arc<Activations>,
    /// The last step left the upgrade owning the session's turn ends.
    owns: AtomicBool,
    /// Not before this instant is a break of the agent's own background work looked at again for the notice ([`BACKGROUND_LOOK`]).
    background_at: Mutex<Option<Instant>>,
    /// Whether the session's conversation has a task, as far as its record has been read ([`WorkerIdle::taskless`]).
    tasked: Mutex<upgrade_drive::Tasked>,
    /// Whether its task is finished, as far as its record has been read ([`WorkerIdle::finished`]).
    finished: Mutex<upgrade_drive::Finished>,
    /// The session's note behind still to be taken ([`Owed`]).
    note: Note,
    /// The unix second the loop last knew the session at its limit.
    clock_hold: Mutex<Option<u64>>,
    /// The instance's one API reach probe ([`NetProbe`]).
    net: Arc<NetProbe>,
    /// The agent's route as last read, by its pid, kernel start and working directory.
    route: Mutex<Option<(u32, String, String, Route)>>,
    /// The agent's memory as last measured, by its pid and when ([`WorkerIdle::memory`]).
    memory: Mutex<Option<(u32, Instant, Option<AgentMemory>)>>,
    /// What the loop last offered and withheld ([`Points`]).
    points: Arc<Points>,
    /// Whether the last settle at its loop's withheld point left what a later one can settle ([`relaunch::settles_again`]).
    settle_later: AtomicBool,
    /// What this worker keeps of its session's usage tap ([`TapKept`]).
    tap: Mutex<TapKept>,
    /// The host's [`Shared::login_latches`].
    login_latches: Arc<LoginLatches>,
    /// The host started this worker as the session's Autopilot left Off ([`HostState::was_off`], [`IdleHost::lifted_off`]).
    lifted_off: bool,
    /// [`WorkerJob::beside`]: while set the loop is asked for no step — no upgrade, no notice, no restart in place.
    beside: Arc<Mutex<Option<String>>>,
    /// Which of the loop's told waits this worker last told the window ([`Told`], as a byte).
    told: AtomicU8,
    /// The session's repository probe, read on its own thread ([`IdleHost::repo_work`]).
    repo_probe: aterm_agent::harness::repo_work::Probing,
}

/// Which told wait a worker last handed the window ([`WorkerIdle::told`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Told {
    Nothing = 0,
    Retry = 1,
    Pause = 2,
}

impl Told {
    /// The byte [`WorkerIdle::told`] keeps, read back.
    const fn of(byte: u8) -> Self {
        match byte {
            1 => Told::Retry,
            2 => Told::Pause,
            _ => Told::Nothing,
        }
    }
}

impl WorkerIdle {
    /// The kept snapshot, followed first: what this worker's own readers decide by.
    fn followed_kept(&self) -> Option<Snapshot> {
        follow_caught(&self.sid, &self.kept, &self.hooks);
        with_kept_of(&self.kept, |k| k.snapshot.clone())
    }

    /// Whether macOS's program check reads stuck on the worker's board ([`Acts::program_check`]).
    fn program_check_stuck(&self) -> bool {
        crate::program_check_watch::stuck(self.hooks.acts.program_check)
    }

    /// Logs the host's account advice at `point` for a session of a pooled CLI other than Claude
    /// Code, always a stay: a hosted Codex is a codex-alab the worker watches beside, which
    /// answers its own walls (`stay other-supervisor`), and no other such CLI is hosted
    /// ([`agent_of`]) or has a relaunch lane to write a move's line, so
    /// [`aterm_agent::harness::accounts_pool::session_move`]'s advice is not taken here.
    fn pool_said(&self, point: AccountPoint) {
        let supervisor = if self.beside() {
            accounts::Supervisor::Own
        } else {
            accounts::Supervisor::Aterm
        };
        let said = match session_pool(self.agent, supervisor) {
            None => accounts::StayWhy::OtherSupervisor.as_str(),
            Some(_) => "no-relaunch-lane",
        };
        aterm_log::info!(
            "harness @{}: account advice at {point:?} for {}: stay {said}",
            self.sid,
            self.agent.name()
        );
    }

    /// Whether the worker watches only ([`WorkerJob::watch_beside`]).
    fn beside(&self) -> bool {
        watches_only(self.agent)
            || self
                .beside
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some()
    }

    /// The stored login the session's agent signs in with, as the host's latch keys its sign-in ([`login_latch::login_key`]).
    fn latch_key(&self) -> Option<String> {
        if !accounts_for(self.agent) {
            return None;
        }
        let snap = with_kept_of(&self.kept, |k| k.snapshot.clone())?;
        let env = (self.hooks.acts.account_env)(snap.pid)?;
        let key = login_latch::login_key(&env)?;
        self.settled(snap.pid, &snap.start, &snap.argv, &snap.cwd)
            .then_some(key)
    }
}

/// A tab as the login latch names it to the other tabs (`@<sid>`, the name `aterm ctl` addresses it by).
fn latch_tab(sid: &str) -> String {
    format!("@{sid}")
}

/// What a worker keeps of its session's usage tap ([`WorkerIdle::usage_tap`]).
#[derive(Debug, Default)]
pub(crate) struct TapKept {
    fresh: tap::Fresh,
    silence: tap::Silence,
    /// The process a turn was last seen to run in ([`IdleHost::turn_ran`]).
    served: Option<u32>,
    /// The windows this worker last recorded, and when.
    noted: Option<(Option<tap::Window>, Option<tap::Window>, i64)>,
    /// When a busy read last took the tap's reading ([`TapAt::Busy`]).
    busy_at: Option<i64>,
}

/// Where a usage tap's reading is taken ([`WorkerIdle::tap_step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TapAt {
    /// A decided idle turn end ([`IdleHost::usage_tap`]): the silent-tap rule judges it too.
    Point,
    /// A busy read mid-turn ([`IdleHost::busy_tap`]), once per [`BUSY_TAP_S`] at most: the
    /// reading alone, since a turn still running owes no reading yet.
    Busy,
}

/// How often a busy read takes the usage tap's reading ([`TapAt::Busy`]). The tap writes every
/// [`tap::TAP_REFRESH_S`] seconds while the agent works, so the accounts store's figures, the
/// `aterm harness accounts` line, an untapped tab's footer and another machine's read of this
/// one stay within a minute of it.
const BUSY_TAP_S: i64 = 60;

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
    /// A point asked for, or a look the worker scheduled come due.
    fn wants(&self) -> bool {
        // Beside a proven self-claim the host takes no step, nor while macOS's program
        // check is stuck (`Self::at_idle_on`): what it owes stays kept and is asked for at
        // the first wait step after the check clears, before the loop's own decision.
        if self.beside() || (self.on() && self.program_check_stuck()) {
            return false;
        }
        self.park.load(Ordering::SeqCst)
            || self
                .look_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some_and(|at| at <= Instant::now())
            || (self.on() && disk_owed(self.hooks.acts.disk_board, &self.sid).is_some())
            || (self.on() && disk_ask_owed(self.hooks.acts.disk_board, &self.sid).is_some())
            || (self.on()
                && host_carries_relaunch(self.agent)
                && with_kept_of(&self.kept, |k| k.carry_asked))
    }

    /// The scheduled look.
    fn wants_at(&self) -> Option<Instant> {
        if self.on() && self.program_check_stuck() {
            return None;
        }
        *self.look_at.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What the jobs reading says of this session's agent's background calls ([`IdleHost::background_jobs`], the stall canary's evidence).
    fn background_jobs(&self) -> aterm_agent::supervise::BackgroundEvidence {
        self.hooks
            .acts
            .jobs_board
            .evidence(&self.sid, Instant::now())
    }

    /// This tab's live agent calls at no progress ([`IdleHost::facts`]), worst first, as of the
    /// board's view; `None` with no view or one older than [`crate::vitals_jobs::JOBS_FRESH`]. A
    /// call is named only once its leader is proved alive at its birth ([`Acts::leader_lives`],
    /// asked after the board's lock is let go).
    fn facts(&self) -> Option<aterm_agent::supervise::FactsReading> {
        let (age, calls) = self
            .hooks
            .acts
            .jobs_board
            .facts(&self.sid, Instant::now())?;
        let now_ms = i64::try_from(aterm_time::unix_millis()?).ok()?;
        let age_ms = i64::try_from(age.as_millis()).unwrap_or(i64::MAX);
        let lives = self.hooks.acts.leader_lives;
        Some(aterm_agent::supervise::FactsReading {
            as_of_ms: now_ms.saturating_sub(age_ms),
            no_progress: calls
                .into_iter()
                .filter(|(leader, call)| call.birth.is_some_and(|birth| lives(*leader, birth)))
                .map(|(_, call)| call)
                .collect(),
        })
    }

    /// The machine as the occupancy facts board last judged it for this session ([`Acts::occupancy`], live [`crate::occupancy_host::reading_for`]).
    fn occupancy(&self) -> aterm_agent::supervise::occupancy::OccupancyReading {
        (self.hooks.acts.occupancy)(&self.sid)
    }

    /// The loop typed its fact line at a break: the point offered, as [`Self::at_background`] stamps it.
    fn break_taken(&self) {
        self.points.offered(crate::upgrade_host::now_s());
    }

    /// What the occupancy door says now, once a change.
    fn occupancy_said(&self, said: &aterm_agent::supervise::occupancy::OccupancySaid) {
        (self.hooks.note)(HostNote::OccupancySaid {
            sid: self.sid.clone(),
            said: said.clone(),
        });
    }

    /// The upgrade's notice is due for this session, whether or not this break's look at it is paused ([`BACKGROUND_LOOK`]).
    fn upgrade_due(&self) -> bool {
        self.on()
            && !self.beside()
            && upgrades(self.agent)
            && self.switches.upgrade()
            && (self.hooks.acts.due)(&self.sid) == Due::Yes
    }

    /// Pace grant for a fact line ([`Acts::occupancy_pace`]), refused where this worker takes no step for its session.
    fn occupancy_pace(
        &self,
        rule: aterm_agent::supervise::occupancy::RuleId,
        hold_since_ms: i64,
    ) -> aterm_agent::supervise::occupancy::PaceGrant {
        use aterm_agent::supervise::occupancy::PaceGrant;
        match self.pace_refusal() {
            Some(why) => PaceGrant::Refused(why),
            None => (self.hooks.acts.occupancy_pace)(&self.sid, rule, hold_since_ms),
        }
    }

    /// The claim on wake `ord` of `episode` for `door`'s line ([`Acts::wake_claim`]), refused
    /// (`refused:off`) unless the owner's mode in force here is `prompt`
    /// ([`crate::occupancy_host::prompt_now`]): the GUI's own gate beside the loop's grant (the
    /// slice-2 plan's W1). The book judges it under its lock with the refusal this worker judges
    /// for every fact line ([`Self::occupancy_pace`]'s).
    fn wake_claim(
        &self,
        ord: u16,
        episode: u64,
        door: aterm_agent::supervise::occupancy::WakeDoor,
    ) -> aterm_agent::supervise::occupancy::WakeClaim {
        use aterm_agent::supervise::occupancy::{PaceRefusal, WakeClaim};
        if !crate::occupancy_host::prompt_now() {
            return WakeClaim::Refused(PaceRefusal::Off);
        }
        (self.hooks.acts.wake_claim)(&self.sid, ord, episode, door, self.pace_refusal())
    }

    /// The loop gives back `door`'s claim on wake `ord` of `episode` ([`Acts::wake_release`]): the
    /// book honours it only where that door holds the claim.
    fn wake_release(
        &self,
        ord: u16,
        episode: u64,
        door: aterm_agent::supervise::occupancy::WakeDoor,
    ) {
        (self.hooks.acts.wake_release)(&self.sid, ord, episode, door);
    }

    /// The task door's fact word on this session's record ([`Acts::task_fact`]), refused
    /// (`Fenced`) unless the owner's mode in force here is `prompt` ([`crate::occupancy_host::prompt_now`]):
    /// the GUI's own gate beside the loop's grant, so no loop's word, replay or older build writes
    /// a fact under `observe` or `off`.
    fn task_fact(
        &self,
        word: Option<aterm_agent::supervise::occupancy::FactWord>,
    ) -> aterm_agent::supervise::occupancy::NoteOutcome {
        if !crate::occupancy_host::prompt_now() {
            return aterm_agent::supervise::occupancy::NoteOutcome::Fenced;
        }
        (self.hooks.acts.task_fact)(&self.sid, word)
    }

    /// The session's book of wakes as the loop reads it ([`Acts::wakes`]).
    fn wakes(&self) -> aterm_agent::supervise::occupancy::WakesSeen {
        (self.hooks.acts.wakes)(&self.sid)
    }

    /// The loop told wake `ord` of `episode` through `door` ([`Acts::wake_told`]): an answer heard,
    /// or the line its claim let go written.
    fn wake_told(&self, ord: u16, episode: u64, door: aterm_agent::supervise::occupancy::WakeDoor) {
        (self.hooks.acts.wake_told)(&self.sid, ord, episode, door);
    }

    /// A supervisor's information for a person (Codex's save-then-wait hold among them).
    fn inform(&self, text: &str) {
        (self.hooks.note)(HostNote::Inform {
            sid: self.sid.clone(),
            text: text.to_string(),
        });
    }

    /// A warning (the memory ceiling's, once an episode and again when what it says comes next changes).
    fn warn(&self, text: &str) {
        (self.hooks.note)(HostNote::Warn {
            sid: self.sid.clone(),
            text: text.to_string(),
        });
    }

    /// What the session's screen says of its account's usage ([`IdleHost::usage_seen`]).
    fn usage_seen(&self, seen: &UsageSeen) {
        let listed = self.switches.accounts() != accounts::Mode::Off;
        let own = self.switches.notes();
        let claude = match self.agent {
            Program::Claude => true,
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
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
            accounts_look(&self.switches, &self.hooks, &self.login_latches, false);
        }
    }

    /// The live upgrade's wind-down owns the turn ends.
    fn owns_turn_end(&self) -> bool {
        self.on() && self.owns.load(Ordering::SeqCst) && self.switches.upgrade()
    }

    /// The disk is full for this session.
    fn withholds_turn_end(&self) -> bool {
        self.on()
            && (disk_withholds(self.hooks.acts.disk_board, &self.sid) || self.program_check_stuck())
    }

    /// A rating survey the loop cannot close, or its end.
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

    /// A point only a person can answer, or its end.
    fn exception(&self, standing: Option<&aterm_agent::supervise::policy::exception::Exception>) {
        (self.hooks.note)(HostNote::ResumeException {
            sid: self.sid.clone(),
            agent: self.agent,
            standing: standing.cloned(),
        });
    }

    /// The stall canary's second rung (90 minutes of background work, 30 of a dialog), or its end ([`HostNote::ResumeUnread`]).
    fn stall_unread(&self, stall: Option<StallSeen>) {
        (self.hooks.note)(HostNote::ResumeUnread {
            sid: self.sid.clone(),
            agent: self.agent,
            stall,
        });
    }

    /// Where the loop is at idle for the host.
    fn at_idle(&self) -> Option<HostStep> {
        self.at_idle_on(IdlePoint::default())
    }

    /// [`Self::at_idle`] at a point the loop describes ([`IdlePoint`]), timed: a step that took
    /// [`SLOW_STEP`] or more is logged with where its time went ([`WorkerIdle::idle_point`]).
    fn at_idle_on(&self, point: IdlePoint) -> Option<HostStep> {
        let (step, took) = laps::within(|| self.idle_point(point));
        if took.total() >= SLOW_STEP {
            aterm_log::info!(
                "harness @{}: its idle step took {} ms ({took})",
                self.sid,
                took.total().as_millis()
            );
        }
        step
    }

    /// A break of the agent's own background work its loop offers.
    fn at_background(
        &self,
    ) -> Option<(String, aterm_agent::supervise::policy::turn_end::HostTurn)> {
        self.points.offered(crate::upgrade_host::now_s());
        if !self.on()
            || self.beside()
            || !upgrades(self.agent)
            || !self.switches.upgrade()
            || self.program_check_stuck()
        {
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
        let hooks = &self.hooks;
        *lock() = Some(Instant::now() + (hooks.pause)(BACKGROUND_LOOK));
        if (hooks.acts.owed)(&self.sid, carries_only(self.agent))
            || (hooks.acts.due)(&self.sid) != Due::Yes
        {
            return None;
        }
        if !self.on() {
            return None;
        }
        let step = self.act(|| (hooks.acts.notice)(&self.sid, self.grace));
        if !self.on() {
            return None;
        }
        if !step.starts_with("wait:") && !step.starts_with("busy:") {
            self.run
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .acted();
        }
        note_upgrade_act();
        if upgrade_drive::released_at_break(&step) {
            self.owns.store(false, Ordering::SeqCst);
        }
        let kind = upgrade_drive::host_turn(&step)?;
        if step.starts_with("announced:") {
            self.owns.store(true, Ordering::SeqCst);
        }
        Some((format!("upgrade step={step}"), kind))
    }

    /// Whether a line of the ask to free space is owed this session, which its loop may offer at a
    /// break of the agent's own background work ([`IdleHost::fact_owed`]).
    fn fact_owed(&self) -> bool {
        self.on()
            && !self.beside()
            && disk_ask_owed(self.hooks.acts.disk_board, &self.sid).is_some()
    }

    /// The ask's line at a settled break of the agent's own background work, where its turn is
    /// over and only a shell or a monitor it left runs on ([`IdleHost::at_background_fact`]): the
    /// same step as at an idle point ([`Self::disk_free_step`]), under the same guards. A session
    /// whose turns end with its own background work still running (a manager awaiting its
    /// workers) reads busy for hours, and its line would otherwise be handed on undelivered.
    fn at_background_fact(&self, point: IdlePoint) -> Option<HostStep> {
        if !self.on() || self.beside() || self.program_check_stuck() {
            return None;
        }
        self.disk_free_step(point)
    }

    /// The agent rests at a point its loop's guard withheld from this worker
    /// ([`IdleHost::at_withheld_point`]: a limit episode, a wall, the loop's own act in flight).
    fn at_withheld_point(&self, wake: bool) -> Option<String> {
        if !self.on() || (wake && !self.settle_later.load(Ordering::SeqCst)) {
            return None;
        }
        if !(self.hooks.acts.owed)(&self.sid, carries_only(self.agent)) {
            self.settle_later.store(false, Ordering::SeqCst);
            return None;
        }
        let step = (self.hooks.acts.settle)(&self.sid);
        self.settle_later
            .store(relaunch::settles_again(&step), Ordering::SeqCst);
        settle_said(&self.sid, &step, &self.hooks).then(|| format!("carry-on step={step}"))
    }

    /// The loop withheld the point this worker asked for ([`Points`]).
    fn withheld(&self, guard: Guard) {
        self.points.withheld(guard, crate::upgrade_host::now_s());
    }

    /// The restart in place its loop asks for ([`Restart`]).
    fn restart(&self, why: &Restart) -> Option<String> {
        if !self.can_restart() || self.beside() {
            return None;
        }
        if let Restart::Account { to, at } = why {
            let step = self.account_move(why, to, *at);
            aterm_log::info!("harness @{}: account move to {to}: {step}", self.sid);
            if relaunch_took(&step) {
                self.login_abandon();
                (self.hooks.note)(HostNote::LoginBack {
                    sid: self.sid.clone(),
                });
            }
            return self.on().then_some(step);
        }
        let hooks = &self.hooks;
        let step = self.act(|| (hooks.acts.restart)(&self.sid, self.grace, why));
        if !self.on() {
            return None;
        }
        if step == "adopted" {
            self.park.store(true, Ordering::SeqCst);
        }
        note_upgrade_act();
        refresh_snapshot(&self.sid, &self.kept, hooks);
        Some(step)
    }

    /// Whether the restart in place is written for this agent (`lanes::lanes`: Claude Code's alone) and `[harness] relaunch` (read live) allows it.
    fn can_restart(&self) -> bool {
        self.relaunch_lane() && self.switches.relaunch()
    }

    /// The owner took the relaunch away ([`IdleHost::relaunch_off`]).
    fn relaunch_off(&self) -> bool {
        self.relaunch_lane() && !self.switches.relaunch()
    }

    /// An agent the stall's remedy ends is relaunched here ([`on_agent_left`], the exit read as the remedy's) for the agents the relaunch on exit is written for.
    fn relaunches_after_stall(&self) -> bool {
        self.on()
            && relaunches(self.agent)
            && self.switches.relaunch()
            && !self.program_check_stuck()
    }

    /// Beside a proven self-claim, the agent's own idle point ([`IdleHost::beside_idle`]).
    fn beside_idle(&self) -> bool {
        if !self.on() || !self.beside() {
            return false;
        }
        let agent = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .as_ref()
            .map(|snap| (snap.pid, snap.start.clone()));
        agent.is_some_and(|(pid, start)| (self.hooks.acts.self_idle)(&self.sid, pid, &start))
    }

    /// The agent's process as the kept snapshot names it.
    fn agent_pid(&self) -> Option<u32> {
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .as_ref()
            .map(|snap| snap.pid)
    }

    /// When the kept snapshot's process started ([`process_started`]).
    fn agent_started(&self) -> Option<i64> {
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .as_ref()
            .and_then(|snap| process_started(&snap.start))
    }

    /// The tab's agent's relaunch is this host's, its continuation still owed
    /// ([`IdleHost::relaunch_owed`], [`relaunch::owed_of`] of the agent's own record, as
    /// [`Self::wants`] asks it).
    fn relaunch_owed(&self) -> bool {
        (self.hooks.acts.owed)(&self.sid, carries_only(self.agent))
    }

    /// The conversation the kept snapshot names ([`IdleHost::agent_conversation`]).
    fn agent_conversation(&self) -> Option<String> {
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .as_ref()
            .and_then(|snap| snap.session.clone())
    }

    /// Whether the explicit restart would be carried for the agent now.
    fn restart_offered(&self) -> Option<bool> {
        (self.hooks.acts.offered)(&self.sid)
    }

    /// The line a person runs to resume this session's agent on its own conversation.
    fn resume_command(&self) -> Option<String> {
        if !relaunches(self.agent) {
            return None;
        }
        let snap = self.followed_kept()?;
        resume_told(&snap)
    }

    /// What the host measures of Claude Code's default API route. Other agents'
    /// endpoints are not measured by the Anthropic probe.
    fn reach(&self) -> Reach {
        let probed = match self.agent {
            Program::Claude => self.switches.probe_api(),
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
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

    /// What the host measures of the agent's memory ([`IdleHost::memory`]).
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
        let m = (self.hooks.acts.memory)(&self.sid, pid);
        *self.memory.lock().unwrap_or_else(PoisonError::into_inner) = Some((pid, now, m));
        m
    }

    /// A turn ran since the loop's last point ([`IdleHost::turn_ran`]).
    fn turn_ran(&self) {
        if let Some(pid) = with_kept_of(&self.kept, |k| k.snapshot.as_ref().map(|s| s.pid)) {
            self.tap
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .served = Some(pid);
        }
        let unnamed = self.unnamed();
        let mut run = self.run.lock().unwrap_or_else(PoisonError::into_inner);
        let climbed = run.same > 0;
        run.new_point();
        if !climbed && !unnamed {
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

    /// The loop decided a turn break, whether or not this worker wants a step there
    /// ([`IdleHost::turn_break`]): the ask to free space (`disk-free@v1`) seats a session at
    /// every disk look as its worker found it at its last turn break, so the finding is made here,
    /// once per decided turn break, not only where a line is owed and the worker steps
    /// ([`Self::idle_point`]). Otherwise a session found unfit once stayed unfit in every later
    /// episode after it was fit again, and one never owed a line was never found unfit at all.
    /// The finding reads the session's records ([`Self::disk_free_unfit`]), so the wait's later
    /// looks at the same point, which would find the same, are not told.
    fn turn_break(&self, theirs: Option<Guard>) {
        if self.on() {
            self.disk_ask_found(theirs);
        }
    }

    /// The usage tap at a decided idle turn end.
    fn usage_tap(&self) {
        if let Some(word) = self.tap_step(TapAt::Point) {
            aterm_log::info!("harness @{}: usage tap: {word}", self.sid);
        }
    }

    /// The usage tap at a busy read, once per [`BUSY_TAP_S`] at most. The session works again,
    /// so what its last turn break found of it (`finished`, `taskless`) no longer bars the ask to
    /// free space from seating it ([`disk_ask_busy`]).
    fn busy_tap(&self) {
        disk_ask_busy(self.hooks.acts.disk_board, &self.sid);
        if let Some(word) = self.tap_step(TapAt::Busy) {
            aterm_log::info!("harness @{}: usage tap mid-turn: {word}", self.sid);
        }
    }

    /// The usage tap's newest reading of `window` for this tab's own process ([`IdleHost::tap_window`]).
    fn tap_window(&self, window: UsageWindow) -> Option<TapWindow> {
        if !self.switches.usage_tap.load(Ordering::SeqCst) || !accounts_for(self.agent) {
            return None;
        }
        let snap = with_kept_of(&self.kept, |k| k.snapshot.clone())?;
        if !tap::carried_by(&snap.argv) {
            return None;
        }
        let seat = (self.hooks.acts.account_seat)()?;
        let born = (self.hooks.acts.tap_born)(snap.pid)?;
        let (reading, mtime) =
            tap::Reading::read(&tap::rl_dir_in(&seat.state), &self.sid, snap.pid, born)?;
        let figure = match window {
            UsageWindow::FiveHour => reading.five_hour,
            UsageWindow::SevenDay => reading.seven_day,
        };
        Some(TapWindow {
            figure,
            read_at: mtime,
        })
    }

    /// The account this tab's Claude Code profile is signed in to now ([`IdleHost::login_account`]).
    fn login_account(&self) -> Option<profile::AccountKey> {
        if !accounts_for(self.agent) {
            return None;
        }
        let snap = with_kept_of(&self.kept, |k| k.snapshot.clone())?;
        let env = (self.hooks.acts.account_env)(snap.pid)?;
        if !accounts::ProcessLogin::of_exec_env(&env)?.stored_login
            || !self.settled(snap.pid, &snap.start, &snap.argv, &snap.cwd)
        {
            return None;
        }
        let seat = (self.hooks.acts.account_seat)()?;
        profile::account_of(&env, &seat.home)
    }

    /// Whether the host started this worker as the session's Autopilot left Off ([`HostState::was_off`]).
    fn lifted_off(&self) -> bool {
        self.lifted_off
    }

    /// The agent's launch as this instance observed it ([`IdleHost::launch`]).
    fn launch(&self) -> Option<i64> {
        let at = (self.hooks.acts.launch)(&self.sid)?;
        let began = self.agent_started()?;
        (began >= at.div_euclid(1000)).then_some(at)
    }

    /// Nobody has asked its conversation anything.
    fn taskless(&self) -> bool {
        if !reads_transcript(self.agent) {
            return false;
        }
        let held = self.followed_kept().filter(|s| s.session.is_some());
        let Some(snap) = held else {
            return false;
        };
        let mut memo = self.tasked.lock().unwrap_or_else(PoisonError::into_inner);
        (self.hooks.acts.tasked)(&self.sid, &snap, &mut memo) == Some(false)
    }

    /// Its worker landed its task and nobody has asked anything since.
    fn finished(&self) -> Option<bool> {
        if !reads_transcript(self.agent) {
            return None;
        }
        let snap = self.followed_kept().filter(|s| s.session.is_some())?;
        let mut memo = self.finished.lock().unwrap_or_else(PoisonError::into_inner);
        (self.hooks.acts.finished)(&self.sid, &snap, &mut memo)
    }

    /// What the session's repository holds that no remote has ([`IdleHost::repo_work`]): read on
    /// the probe's own thread, each ask waiting [`aterm_agent::harness::repo_work::PROBE_ASK`] at
    /// most, or until the worker is stopped.
    fn repo_work(&self) -> Option<aterm_agent::harness::repo_work::RepoRead> {
        self.repo_probe.ask(
            || {
                let snap = self.followed_kept();
                let read = Arc::clone(&self.hooks.acts.repo_work);
                move || snap.map_or_else(Default::default, |snap| read(&snap))
            },
            aterm_agent::harness::repo_work::PROBE_ASK,
            &self.stop,
        )
    }

    /// The turn the conversation ends on, by every key that names it.
    fn last_turn(&self) -> Option<aterm_agent::harness::upgrade::LastTurn> {
        if !reads_transcript(self.agent) {
            return None;
        }
        self.finished
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last_turn()
    }

    /// The usage limit's reset the conversation's transcript names ([`IdleHost::limit_until`]) for the kept record a Claude Code holds.
    fn limit_until(&self) -> Option<u64> {
        if !reads_transcript(self.agent) {
            return None;
        }
        let snap = self.followed_kept().filter(|s| s.session.is_some())?;
        (self.hooks.acts.limit_reset)(&snap)
    }

    /// The transcript that confirms a wall ([`IdleHost::transcript_tail`]).
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

    /// The transcript a task's result is snapshotted from ([`IdleHost::transcript`]).
    fn transcript(&self) -> Option<(PathBuf, String)> {
        if !reads_transcript(self.agent) || !refresh_snapshot(&self.sid, &self.kept, &self.hooks) {
            return None;
        }
        let snap = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .clone()?;
        (self.hooks.acts.transcript)(&snap)
    }

    /// The same transcript read again ([`IdleHost::transcript_tail_again`]).
    fn transcript_tail_again(&self) -> Option<String> {
        if !reads_transcript(self.agent) {
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

    /// When the Claude Code whose transcript was last read was launched
    /// ([`IdleHost::launched_at`]).
    fn launched_at(&self) -> Option<i64> {
        if !reads_transcript(self.agent) {
            return None;
        }
        self.agent_started()
    }

    /// The limit only the transcript shows ([`IdleHost::standing_limit`]): the Claude Code in the tab now.
    fn standing_limit(&self) -> Option<aterm_agent::harness::upgrade::StandingLimit> {
        if !reads_transcript(self.agent) || !refresh_snapshot(&self.sid, &self.kept, &self.hooks) {
            return None;
        }
        let snap = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot
            .clone()?;
        (self.hooks.acts.standing_limit)(&self.sid, &snap)
    }
    /// The loop's limit episode opened or closed.
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
        // The episode's wall is published as the loop dates it ([`IdleHost::limit_wall`], told
        // right after this), and taken down with the episode.
        if !open {
            disk_ask_walled(self.hooks.acts.disk_board, &self.sid, None);
        }
    }

    /// The loop's open limit episode holds its wall to `reset_unix` ([`IdleHost::limit_wall`]),
    /// told as it opens and whenever a later notice moves it: the ask to free space holds the
    /// session out of its seats, and waits for it rather than running out, until the wall is
    /// spent where the loop takes it so, that reset plus the loop's grace
    /// ([`crate::disk_board::Board::walls`]). Where the loop knows no reset, the transcript's
    /// dates it, as the loop's own judgement falls back to it; with neither, the wall holds until
    /// the episode closes.
    fn limit_wall(&self, reset_unix: Option<i64>) {
        let reset = reset_unix.or_else(|| self.limit_until().and_then(|r| i64::try_from(r).ok()));
        let grace =
            i64::try_from(aterm_agent::harness::upgrade_drive::LIMIT_GRACE_S).unwrap_or(i64::MAX);
        let wall = crate::disk_board::Wall {
            until_s: reset.map(|r| r.saturating_add(grace)),
        };
        disk_ask_walled(self.hooks.acts.disk_board, &self.sid, Some(wall));
    }

    /// The lasting withhold the loop's last decided turn break holds the session at
    /// ([`IdleHost::lasting_hold`]): the ask to free space holds the session out of its seats
    /// while it stands ([`crate::disk_board::Board::withholds`]), as it does at a usage-limit wall.
    fn lasting_hold(&self, hold: Option<aterm_agent::supervise::Lasting>) {
        use aterm_agent::supervise::Lasting;
        let withhold = hold.map(|hold| match hold {
            Lasting::Credits { lifts_unix } => crate::disk_board::Withhold::Credits {
                until_s: lifts_unix,
            },
            Lasting::Wall { wire } => crate::disk_board::Withhold::Wall { wire },
        });
        disk_ask_withheld(self.hooks.acts.disk_board, &self.sid, withhold);
    }

    /// The loop's next try ([`IdleHost::waiting`]), told to the window, which shows it beside an API wall.
    fn waiting(&self, plan: Option<aterm_agent::supervise::WaitPlan>) {
        use aterm_agent::supervise::WaitKind;
        let was = Told::of(self.told.load(Ordering::SeqCst));
        let tell_retry = |retry| (self.hooks.waiting)(&self.sid, retry);
        let tell_pause = |pause| (self.hooks.fact)(&self.sid, crate::HarnessFact::Pause(pause));
        let now = match plan.map(|p| (p.kind, p.at_unix)) {
            Some((WaitKind::Retry, at_unix)) => {
                if was == Told::Pause {
                    tell_pause(None);
                }
                tell_retry(Some(crate::presence::RetryPlan::at(at_unix)));
                Told::Retry
            }
            Some((WaitKind::BackOff { short }, at_unix)) => {
                if was == Told::Retry {
                    tell_retry(None);
                }
                tell_pause(Some(crate::PausePlan { at_unix, short }));
                Told::Pause
            }
            Some((WaitKind::Other, _)) | None => {
                if was == Told::Pause {
                    tell_pause(None);
                } else {
                    tell_retry(None);
                }
                Told::Nothing
            }
        };
        self.told.store(now as u8, Ordering::SeqCst);
    }

    /// An act of the harness's ([`IdleHost::act_taken`]).
    fn act_taken(&self, act: aterm_agent::harness::acts::Act) {
        (self.hooks.fact)(&self.sid, crate::HarnessFact::Act(act));
    }

    /// The task's face ([`IdleHost::task_face`]).
    fn task_face(&self, face: aterm_agent::harness::acts::TaskFace) {
        (self.hooks.fact)(&self.sid, crate::HarnessFact::Task(Some(face)));
    }

    /// The task's face restated at the loop's start ([`IdleHost::task_face_noted`]), with the time its end was journaled.
    fn task_face_noted(&self, face: aterm_agent::harness::acts::TaskFace, noted_ms: i64) {
        (self.hooks.fact)(&self.sid, crate::HarnessFact::TaskNoted(face, noted_ms));
    }

    /// The worker's needs you, whole.
    fn needs_standing(&self, needs: &[String], entry: &str) {
        if (self.hooks.needs)(&self.sid, needs, entry) {
            (self.hooks.fact)(&self.sid, crate::HarnessFact::Needs);
        }
    }

    /// Why land still holds: the hold the loop tells where it moved.
    fn land_held(&self, held: Option<aterm_agent::harness::acts::LandHeld>) {
        if (self.hooks.hold)(&self.sid, held) {
            (self.hooks.fact)(&self.sid, crate::HarnessFact::LandHeld);
        }
    }

    /// What the roster advises about this session's account at `point` ([`IdleHost::account`]).
    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let owed = self.account_wall_again(point);
        let advice = self.account_advice(point, owed);
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

    /// The account this session moved to served a turn ([`IdleHost::account_served`]).
    fn account_served(&self) {
        if !self.switches.accounts_live() {
            return;
        }
        let Some((seat, roster, look)) = self.account_ctx() else {
            return;
        };
        let me = look.who.me(&roster);
        let Some(label) = me.account.filter(|_| me.default_home && me.stored_login) else {
            return;
        };
        if with_kept_of(&self.kept, |k| k.account.flying.is_some()) {
            aterm_log::info!(
                "harness @{}: account {label} served: a move's landing is unread; nothing proven",
                self.sid
            );
            return;
        }
        let confirmed = self.login_identity_confirmed(&seat, &roster, &look, label);
        let done = login_transact_of(
            &self.sid,
            &self.hooks,
            &seat,
            &roster,
            &look,
            label,
            |state, logins, now| {
                let signed_out = state.account(label).auth == accounts::Auth::SignedOut;
                if signed_out && !confirmed {
                    return Err("sign-in identity is not confirmed; nothing proven".into());
                }
                let mut events = if signed_out {
                    accounts::note_signed_back(state, logins, label, now)
                } else {
                    Vec::new()
                };
                events.extend(accounts::prove(state, label, now));
                events.extend(accounts::note_session(state, &self.sid, label, None, now));
                Ok(events)
            },
        );
        if done.is_ok() {
            with_kept_of(&self.kept, |k| k.account.moved = None);
        }
        let word = match &done {
            Ok(events) if events.iter().any(|e| e.kind == accounts::Kind::Proven) => "proven",
            Ok(_) => "served",
            Err(_) => "served: not recorded",
        };
        aterm_log::info!("harness @{}: account {label} {word}", self.sid);
    }

    /// The account this session moved to answered with a lost login or a usage limit ([`IdleHost::account_answered`]).
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
            with_kept_of(&self.kept, |k| k.account.came_back = None);
            let lost = AccountPoint::Wall {
                kind: aterm_phase::WallKind::Auth,
            };
            self.lost_seen(&look.snap, lost, seat.now);
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
            (self.hooks.acts.accounts_upkeep).make(UpkeepStep::Renew);
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

    /// News of the accounts since the host started ([`IdleHost::account_stamp`]; [`Switches::accounts_news`]).
    fn account_stamp(&self) -> u64 {
        self.switches.accounts_news.load(Ordering::SeqCst)
    }

    /// The renewable login the session runs on ([`IdleHost::login_label`]).
    fn login_label(&self) -> Option<String> {
        self.renewable_look().map(|(_, _, label)| label)
    }

    /// The lost login no roster row names, before any `/login` ([`IdleHost::login_discover`]):
    /// the accounts' upkeep's sync, made now, whose discovery adopts the default login's address
    /// as a row at once where the roster has rows and a fleet source is configured
    /// (`accounts_sync`'s `default_login_unlisted`). A login under its own `CLAUDE_CONFIG_DIR`
    /// has no discovery that adopts its address.
    fn login_discover(&self) {
        if !self.switches.logins() || !accounts_for(self.agent) {
            return;
        }
        (self.hooks.acts.accounts_upkeep).make(UpkeepStep::Sync);
    }

    /// Whether the session's process holds a login a renewal replaced ([`IdleHost::login_superseded`]).
    fn login_superseded(&self) -> bool {
        self.renewable_look()
            .is_some_and(|(roster, look, _)| renewable_own(&roster, &look).is_none())
    }

    /// The method the session's login already is ([`IdleHost::login_method`]), for a login no
    /// roster row renews. A renewable login's picker is the loop's to back out of
    /// (`PickerGate::Roster`), never to pick in, so its method is `None`: a row named between
    /// the picker's gate and this read leaves the picker unpicked rather than opens its browser
    /// sign-in.
    fn login_method(&self) -> Option<aterm_phase::LoginMethod> {
        if !accounts_for(self.agent) {
            return None;
        }
        let snap = with_kept_of(&self.kept, |k| k.snapshot.clone())?;
        let env = (self.hooks.acts.account_env)(snap.pid)?;
        if !accounts::ProcessLogin::of_exec_env(&env)?.stored_login
            || !self.settled(snap.pid, &snap.start, &snap.argv, &snap.cwd)
        {
            return None;
        }
        let seat = (self.hooks.acts.account_seat)()?;
        if self.renewable_look().is_some() {
            return None;
        }
        aterm_agent::harness::profile::login_method(&env, &seat.home)
    }

    /// Another tab's sign-in of this session's login ([`IdleHost::login_shared`]), asked at its lost login.
    fn login_shared(&self) -> Option<aterm_agent::supervise::policy::login::LoginShared> {
        let key = self.latch_key()?;
        self.login_latches.shared(&key, &latch_tab(&self.sid))
    }

    /// What the roster says of the session's login ([`IdleHost::roster_word`]), asked where a usage limit's notice reads it.
    fn roster_word(&self) -> Option<RosterWord> {
        if !self.switches.accounts_live() || !accounts_for(self.agent) {
            return None;
        }
        let seat = (self.hooks.acts.account_seat)()?;
        let roster = match roster_read(&seat) {
            Ok(roster) => roster,
            Err(RosterUnread::Refused | RosterUnread::LoginsUnread) => {
                return Some(RosterWord::Unread);
            }
            Err(RosterUnread::Nothing) => return Some(RosterWord::Unlisted),
        };
        let Some(look) = self.account_look(&roster) else {
            return Some(RosterWord::Unread);
        };
        if look.label.is_none() {
            return Some(RosterWord::Unlisted);
        }
        let Ok((state, _)) = accounts::Store::at(&seat.state).load(seat.now) else {
            return Some(RosterWord::Unread);
        };
        let enrolled = accounts::enrolled(&roster);
        let limit = AccountPoint::Wall {
            kind: aterm_phase::WallKind::UsageWeekly,
        };
        let (at, _, _) = account_point_read(&seat, &look, limit, seat.now);
        Some(roster_word_of(
            &roster,
            &state,
            &enrolled,
            &look.who.me(&roster),
            &at,
            self.switches.accounts(),
            seat.now,
        ))
    }

    /// This tab takes its login's one sign-in ([`IdleHost::login_carry`]), as its loop is about to type `/login`.
    fn login_carry(&self) -> Result<(), String> {
        let Some(key) = self.latch_key() else {
            return Ok(());
        };
        let row = self
            .account_ctx()
            .and_then(|(_, roster, look)| look.label.filter(|l| roster.row(l).is_some()));
        let carried =
            self.login_latches
                .carry_for(&key, &latch_tab(&self.sid), &self.stop, row.as_deref());
        match &carried {
            Err(by) => {
                aterm_log::info!("harness @{}: the login's sign-in is {by}'s", self.sid);
            }
            Ok(()) if row.is_some() => {
                accounts_look(&self.switches, &self.hooks, &self.login_latches, false);
            }
            Ok(()) => {}
        }
        carried
    }

    /// This worker's sign-in reservation ends ([`IdleHost::login_abandon`]).
    fn login_abandon(&self) {
        if self
            .login_latches
            .abandon_for(&latch_tab(&self.sid), &self.stop)
        {
            accounts_look(&self.switches, &self.hooks, &self.login_latches, false);
        }
    }

    /// This session's lost login is back ([`IdleHost::login_back`]).
    fn login_back(&self) {
        let tab = latch_tab(&self.sid);
        match self.latch_key() {
            Some(key) => self.login_latches.back(&key, &tab),
            None => self.login_latches.gone(&tab),
        }
        if self.switches.accounts_live()
            && let Some((seat, roster, look)) = self.account_ctx()
            && let Some(label) = renewable_own(&roster, &look)
        {
            self.signed_back(&seat, &roster, &look, &label);
        }
        (self.hooks.note)(HostNote::LoginBack {
            sid: self.sid.clone(),
        });
    }

    /// This session's limit ended with no sign-in that took ([`IdleHost::limit_over`]): its
    /// wall holds it from the ask to free space no more.
    fn limit_over(&self) {
        disk_ask_walled(self.hooks.acts.disk_board, &self.sid, None);
        (self.hooks.note)(HostNote::LimitOver {
            sid: self.sid.clone(),
        });
    }

    /// Claude Code's `Your login expires in N days` ([`IdleHost::login_notice`]).
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
        let done = login_transact_of(
            &self.sid,
            &self.hooks,
            &seat,
            &roster,
            &look,
            &label,
            |_, logins, now| {
                accounts::note_login_notice(logins, &label, days, now);
                Ok(Vec::new())
            },
        );
        let word = if done.is_ok() {
            (self.hooks.acts.accounts_upkeep).make(UpkeepStep::Renew);
            "recorded"
        } else {
            "not recorded"
        };
        aterm_log::info!(
            "harness @{}: account {label}: Claude Code says its login ends in {days} day(s); {word}",
            self.sid
        );
    }

    /// The claim of the account's one `/usage` probe ([`IdleHost::account_probe`]): [`Self::probe_claim`].
    fn account_probe(&self) -> Result<(), NoProbe> {
        self.probe_claim()
    }

    /// What the claimed probe read ([`IdleHost::account_probed`]): [`Self::probe_record`].
    fn account_probed(&self, read: Result<&accounts::Sample, ProbeMiss>) {
        self.probe_record(read.map_err(ProbeMiss::word));
    }

    /// May this session take its account's limit reset ([`IdleHost::reset_take`]): [`Self::reset_claim`].
    fn reset_take(&self, ask: &ResetAsk) -> ResetWord {
        self.reset_claim(ask)
    }

    /// How the claimed reset ended ([`IdleHost::reset_told`]): [`Self::reset_tell`].
    fn reset_told(&self, end: &ResetEnd) -> ResetOutcome {
        self.reset_tell(end)
    }
}

/// A worker that ends takes its session's next try with it.
impl Drop for WorkerIdle {
    fn drop(&mut self) {
        self.login_abandon();
        (self.hooks.acts.wakes_left)(&self.sid);
        (self.hooks.waiting)(&self.sid, None);
        (self.hooks.fact)(&self.sid, crate::HarnessFact::Task(None));
        (self.hooks.fact)(&self.sid, crate::HarnessFact::Pause(None));
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
    /// The session on a renewable login ([`IdleHost::login_label`], [`IdleHost::login_superseded`]).
    fn renewable_look(&self) -> Option<(accounts::Roster, AccountLook, String)> {
        if !self.switches.logins() || !accounts_for(self.agent) {
            return None;
        }
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

    /// [`IdleHost::usage_tap`]'s work (`at` [`TapAt::Point`]) or [`IdleHost::busy_tap`]'s
    /// ([`TapAt::Busy`]), and its word for the log (`None`: nothing to say).
    fn tap_step(&self, at: TapAt) -> Option<&'static str> {
        if !accounts_for(self.agent) {
            return None;
        }
        if at == TapAt::Busy {
            let now = (self.hooks.acts.account_seat)()?.now;
            let mut kept = self.tap.lock().unwrap_or_else(PoisonError::into_inner);
            // A clock set back makes the next busy read due rather than holding it off.
            if kept
                .busy_at
                .is_some_and(|then| (0..BUSY_TAP_S).contains(&now.saturating_sub(then)))
            {
                return None;
            }
            kept.busy_at = Some(now);
        }
        let snap = with_kept_of(&self.kept, |k| k.snapshot.clone())?;
        if !tap::carried_by(&snap.argv) {
            if at == TapAt::Point {
                let heard = self
                    .tap
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .silence
                    .untapped(snap.pid);
                if heard == tap::Heard::Again {
                    self.tap_heard();
                }
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
        let (heard, before) = {
            let mut kept = self.tap.lock().unwrap_or_else(PoisonError::into_inner);
            let served = kept.served == Some(snap.pid);
            let heard = match at {
                TapAt::Point => kept.silence.step(
                    seat.now,
                    snap.pid,
                    seat.now.saturating_sub(born),
                    served,
                    age,
                ),
                TapAt::Busy => tap::Heard::Same,
            };
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
        let (label, several) = match self.tap_label(&seat, snap.pid) {
            Ok(Some(judged)) => judged,
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
        let beside = before
            .as_ref()
            .map(|b| b.beside(&reading))
            .unwrap_or_default();
        let key = profile::default_home_key(&seat.home).filter(|_| !several);
        let read = accounts::TapRead {
            born: Some(born),
            ..reading.tap_read(beside, key)
        };
        let mut held_back = false;
        let mut low_or_limited = false;
        let noted = self.account_transact(&seat, |state, now| {
            let (events, held) = accounts::note_reading_held(state, &label, &read, now);
            held_back = held;
            low_or_limited = !events.is_empty()
                && state
                    .accounts
                    .get(&label)
                    .is_some_and(|a| a.low(now) || a.limited_until(now).is_some());
            Ok(events)
        });
        match noted {
            Ok(events) => {
                if low_or_limited {
                    (self.hooks.acts.accounts_upkeep).make(UpkeepStep::Renew);
                }
                if !held_back {
                    self.tap
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .noted = Some((windows.0, windows.1, seat.now));
                    if self.switches.usage_tap.load(Ordering::SeqCst) {
                        (self.hooks.log_usage)(
                            &self.sid,
                            windows.0.map(|w| w.pct),
                            windows.1.map(|w| w.pct),
                        );
                    }
                }
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

    /// A fresh reading this worker could not take ([`Self::tap_step`]).
    fn tap_untake(&self, before: Option<tap::Fresh>) {
        if let Some(before) = before {
            self.tap
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .fresh = before;
        }
    }

    /// A silent tap's mark ends ([`tap::Heard::Again`]; the worker's end).
    fn tap_heard(&self) {
        (self.hooks.note)(HostNote::UsageTap {
            sid: self.sid.clone(),
            silent: false,
        });
    }

    /// Whose a usage tap's record is — a reading ([`Self::tap_step`]), a silence ([`Self::tap_silent`]) of the agent process `pid`.
    fn tap_label(
        &self,
        seat: &AccountSeat,
        pid: u32,
    ) -> Result<Option<(String, bool)>, &'static str> {
        let roster = accounts::tap_roster(
            seat.roster.as_deref(),
            &accounts::Store::at(&seat.state),
            seat.now,
        )
        .ok_or(TAP_NOT_OWN)?;
        let Some(look) = self.account_look(&roster) else {
            return Ok(None);
        };
        look.judged(&roster)
            .filter(|_| look.snap.pid == pid)
            .map(|label| Some((label.to_owned(), roster.several())))
            .ok_or(TAP_NOT_OWN)
    }

    /// A tapped process went silent ([`Self::tap_step`]).
    fn tap_silent(&self, seat: &AccountSeat, pid: u32) {
        if let Ok(Some((label, _))) = self.tap_label(seat, pid) {
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

    /// Whether this worker's agent supports relaunch and its kept snapshot names no conversation yet.
    fn unnamed(&self) -> bool {
        relaunches(self.agent)
            && with_kept_of(&self.kept, |k| {
                k.snapshot.as_ref().is_some_and(|s| s.session.is_none())
            })
    }

    /// The host's step at an idle point ([`IdleHost::at_idle_on`]), each part marked ([`laps::lap`]).
    fn idle_point(&self, point: IdlePoint) -> Option<HostStep> {
        if !self.on() || self.beside() {
            return None;
        }
        if self.program_check_stuck() {
            return None;
        }
        laps::lap("check");
        self.points.offered(crate::upgrade_host::now_s());
        let mut look = self.look_at.lock().unwrap_or_else(PoisonError::into_inner);
        if look.is_some_and(|at| at <= Instant::now()) {
            *look = None;
        }
        self.park.store(false, Ordering::SeqCst);
        drop(look);
        with_kept_of(&self.kept, |k| k.carry_asked = false);
        // The resume is a carry-on: the loop types none into the person's own exchange, and none
        // while a person is at the session (their grace, or another driver's hand). A person's
        // grace keeps it owed for a later point; an exchange gives it up, as the conversation is
        // theirs until they hand it back.
        let owed = disk_owed(self.hooks.acts.disk_board, &self.sid);
        if owed.is_some()
            && let Some(why) = self.disk_resume_unowed().or_else(|| {
                (point.theirs == Some(Guard::Exchange)).then_some(Guard::Exchange.word())
            })
        {
            disk_resume_stepped(self.hooks.acts.disk_board, &self.sid, true);
            aterm_log::info!(
                "harness @{}: {} step=done:{why}: nothing typed",
                self.sid,
                aterm_agent::harness::disk::RULE_DISK_RESUME
            );
        } else if owed.is_some() && point.held == Some(Guard::Person) {
            aterm_log::info!(
                "harness @{}: {} step=wait:{}: nothing typed, still owed",
                self.sid,
                aterm_agent::harness::disk::RULE_DISK_RESUME,
                Guard::Person.word()
            );
        } else if let Some(text) = owed {
            let step = (self.hooks.acts.disk_resume)(&self.sid, &text);
            let typed = step == "typed";
            disk_resume_stepped(self.hooks.acts.disk_board, &self.sid, typed);
            aterm_log::info!(
                "harness @{}: {} step={step}",
                self.sid,
                aterm_agent::harness::disk::RULE_DISK_RESUME
            );
            if typed {
                return Some(HostStep {
                    line: format!(
                        "{} step={step}",
                        aterm_agent::harness::disk::RULE_DISK_RESUME
                    ),
                    moved: true,
                    typed: Some(aterm_agent::supervise::policy::turn_end::HostTurn::CarryOn),
                    repeat: false,
                });
            }
        }
        // Whether the ask to free space may seat this session, as of this turn break: the look
        // nominates none the board names unfit, and one owed a line is released, at no
        // election's cost. A session the loop's own guards hold for someone else for as long as
        // they keep it (the person's own exchange, a manager's wait: `IdlePoint::theirs`) is
        // unfit too, as no later turn break of theirs would take the line; one a person's grace
        // alone holds keeps its line owed for a later turn break, as the occupancy door's does.
        // The loop told the finding at the turn break's decision, before it asked whether this
        // worker wants a step there ([`IdleHost::turn_break`]); it is made again as the act reads
        // it, as the line may come owed at a later look of the wait.
        if let Some(step) = self.disk_free_step(point) {
            return Some(step);
        }
        laps::lap("disk");
        if with_kept_of(&self.kept, |k| k.account.reset_owed.is_some())
            && let Some(seat) = (self.hooks.acts.account_seat)()
        {
            self.reset_flush(&seat);
        }
        laps::lap("account");
        let step = self.idle_step(point);
        if !self.on() {
            return None;
        }
        if self.unnamed() {
            let mut look = self.look_at.lock().unwrap_or_else(PoisonError::into_inner);
            if look.is_none() {
                *look = Some(Instant::now() + (self.hooks.pause)(upgrade_drive::first_look()));
                drop(look);
                ring();
            }
        }
        step
    }

    /// The ask to free space's step at a turn break `point` describes: an idle point
    /// ([`Self::idle_point`]) or a break of the agent's own background work where its turn is over
    /// ([`IdleHost::at_background_fact`]). The finding is made first ([`Self::disk_ask_found`]),
    /// then a line owed is stepped ([`Acts::disk_free`]); `Some` where it was typed.
    fn disk_free_step(&self, point: IdlePoint) -> Option<HostStep> {
        let unfit = self.disk_ask_found(point.theirs);
        let owed = disk_ask_owed(self.hooks.acts.disk_board, &self.sid)?;
        let held = point.held.map(Guard::word);
        let step = (self.hooks.acts.disk_free)(&self.sid, &owed, unfit, held);
        if !step.starts_with("wait:") {
            disk_ask_stepped(self.hooks.acts.disk_board, &self.sid);
        }
        aterm_log::info!(
            "harness @{}: {} step={step}",
            self.sid,
            aterm_agent::harness::disk::ask::RULE_DISK_FREE
        );
        (step == "typed").then(|| HostStep {
            line: format!(
                "{} step={step}",
                aterm_agent::harness::disk::ask::RULE_DISK_FREE
            ),
            moved: true,
            typed: Some(aterm_agent::supervise::policy::turn_end::HostTurn::Fact),
            repeat: false,
        })
    }

    /// The host's step at an idle point ([`Self::idle_point`], which sets a look after it while the conversation is unnamed).
    fn idle_step(&self, point: IdlePoint) -> Option<HostStep> {
        let upgraded = upgrades(self.agent);
        let relaunched = relaunches(self.agent);
        if !self.on() || !(upgraded || host_carries_relaunch(self.agent)) {
            return None;
        }
        let hooks = &self.hooks;
        if relaunched {
            refresh_snapshot_at_point(&self.sid, &self.kept, hooks, hooks.acts.point_read);
        }
        laps::lap("snapshot");
        self.hold_clock();
        laps::lap("clock");
        if upgraded && self.switches.upgrade() {
            ask_note(&self.sid, &self.note, hooks);
            laps::lap("note");
        }
        let mut run = self.run.lock().unwrap_or_else(PoisonError::into_inner);
        let activations = self.activations.of(self.agent);
        if run.activations != activations {
            run.activations = activations;
            run.worded();
        }
        let due = || {
            if upgraded && self.switches.upgrade() {
                (hooks.acts.due)(&self.sid)
            } else {
                Due::No
            }
        };
        let only = carries_only(self.agent);
        let owed = (hooks.acts.owed)(&self.sid, only);
        laps::lap("owed");
        let (what, step) = if owed {
            if !self.on() {
                return None;
            }
            let carry = CarryPoint {
                land: self.autopilot() == Autopilot::Land,
                done_under_land: point.done_under_land,
                only,
            };
            let step = (hooks.acts.resume)(&self.sid, self.grace, carry);
            laps::lap("resume");
            if step == "done:lost" {
                (hooks.note)(HostNote::Relaunch {
                    sid: self.sid.clone(),
                    said: RelaunchSay::NotRestored,
                });
            }
            ("carry-on", step)
        } else {
            let due = due();
            laps::lap("due");
            if !self.on() {
                return None;
            }
            match due {
                Due::Yes => {
                    if !self.on() {
                        return None;
                    }
                    let step = self.act(|| (hooks.acts.step)(&self.sid, self.grace));
                    laps::lap("upgrade");
                    ("upgrade", step)
                }
                Due::Unread(why) => {
                    let pause = run.unread();
                    *self.look_at.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(Instant::now() + (hooks.pause)(pause));
                    ring();
                    return Some(HostStep {
                        line: format!("upgrade step=wait:{why}"),
                        moved: false,
                        typed: None,
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
        note_upgrade_act();
        if relaunched {
            refresh_snapshot(&self.sid, &self.kept, hooks);
            laps::lap("snapshot.after");
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
                if (hooks.acts.owed)(&self.sid, only) {
                    *self.look_at.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(Instant::now() + (hooks.pause)(upgrade_drive::first_look()));
                    ring();
                }
            }
            After::Broken => {
                if !std::mem::replace(&mut run.broken, true) {
                    aterm_log::warn!(
                        "harness @{}: the live upgrade cannot run: {step} (its state directory \
                         cannot hold the lock); it is tried again at the next activation notice",
                        self.sid
                    );
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
            typed: upgrade_drive::host_turn(&step),
            repeat,
        })
    }

    /// The upgrade runs in this tab (a step that is not `Broken`).
    fn upgrade_runs(&self, run: &mut UpgradeRun) {
        let was_broken = std::mem::replace(&mut run.broken, false);
        if !std::mem::replace(&mut run.runs_told, true) || was_broken {
            (self.hooks.note)(HostNote::UpgradeRuns {
                sid: self.sid.clone(),
            });
        }
    }

    /// A step that may end the agent, taken ([`WorkerJob::acting`]).
    fn act<T>(&self, step: impl FnOnce() -> T) -> T {
        /// Dropped as the step returns or unwinds: it is over.
        struct Acted<'a>(&'a WorkerIdle);
        impl Drop for Acted<'_> {
            fn drop(&mut self) {
                self.0.acted();
            }
        }
        self.stepped.store(
            STEP_STAMPS.fetch_add(1, Ordering::SeqCst) + 1,
            Ordering::SeqCst,
        );
        self.acting.store(true, Ordering::SeqCst);
        let _over = Acted(self);
        step()
    }

    /// That step is over, returned or unwound ([`Self::act`]).
    fn acted(&self) {
        self.stepped.store(
            STEP_STAMPS.fetch_add(1, Ordering::SeqCst) + 1,
            Ordering::SeqCst,
        );
        self.acting.store(false, Ordering::SeqCst);
        ring();
    }

    /// The worker still acts: not stopped, and its agent still admitted ([`Switches::on_for`]).
    fn on(&self) -> bool {
        !self.stop.load(Ordering::SeqCst) && self.switches.on_for(self.agent)
    }

    /// Why this worker refuses every fact line of its session (slice 1's refusals, a wake's
    /// claim's too): off, beside a self-supervising agent, the wind-down owning its turn ends, or
    /// a relaunch's carry-on owed; `None` where it takes them.
    fn pace_refusal(&self) -> Option<aterm_agent::supervise::occupancy::PaceRefusal> {
        use aterm_agent::supervise::occupancy::PaceRefusal;
        if !self.on() {
            Some(PaceRefusal::Off)
        } else if self.beside() {
            Some(PaceRefusal::Beside)
        } else if self.owns_turn_end() {
            Some(PaceRefusal::Owns)
        } else if host_carries_relaunch(self.agent) && with_kept_of(&self.kept, |k| k.carry_asked) {
            Some(PaceRefusal::CarryOwed)
        } else {
            None
        }
    }

    /// Whether the session's agent is one this host relaunches in place.
    fn relaunch_lane(&self) -> bool {
        self.on() && aterm_agent::harness::lanes::lanes(self.agent).restart_in_place
    }

    /// The session's Autopilot now.
    fn autopilot(&self) -> Autopilot {
        OffSessions::read(&self.hooks, self.switches.autopilot()).mode(&self.sid)
    }

    /// Why the session may not be asked to free disk space (`disk-free@v1`): its Autopilot is
    /// off, its task finished or landed, or nobody gave it one. Landing is no bar: a landing
    /// session still works in its checkout.
    fn disk_free_unfit(&self) -> Option<&'static str> {
        if self.autopilot() == Autopilot::Off {
            Some("autopilot-off")
        } else if self.finished() == Some(true) {
            Some("finished")
        } else if self.taskless() {
            Some("taskless")
        } else {
            None
        }
    }

    /// Why the ask to free space may not seat this session as of the turn break it rests at
    /// ([`Self::disk_free_unfit`], or `theirs`: the person's own exchange or a manager's wait,
    /// [`IdlePoint::theirs`]), written to the board every disk look reads ([`disk_ask_fit`]).
    fn disk_ask_found(&self, theirs: Option<Guard>) -> Option<&'static str> {
        let found = crate::disk_board::Unfit {
            own: self.disk_free_unfit(),
            theirs: theirs.map(Guard::word),
        };
        disk_ask_fit(self.hooks.acts.disk_board, &self.sid, found);
        found.bars(false)
    }

    /// Why the disk resume is owed no longer, by the gates a continuation keeps.
    fn disk_resume_unowed(&self) -> Option<&'static str> {
        if self.autopilot() == Autopilot::Land {
            Some("landing")
        } else if self.finished() == Some(true) {
            Some("finished")
        } else if self.taskless() {
            Some("taskless")
        } else {
            None
        }
    }

    /// The agent's route ([`Acts::route`]), read once per agent process and directory (again when the directory is filled in).
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

    /// The session's own usage wall for its account, as the loop's limit episode opens ([`IdleHost::limited`]).
    fn account_wall(&self) -> &'static str {
        if !accounts_for(self.agent) {
            return "unasked";
        }
        let agent = self
            .followed_kept()
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

    /// The recorder, asked again at the session's next decision of the standing limit episode ([`IdleHost::account`] is asked once a decision).
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

    /// The move this loop is still to be told of ([`Moved::untold`]), by its label.
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

    /// Whether the loop is told to ask again soon ([`account_lagging`]).
    fn account_lags(&self, lags: bool) -> bool {
        lags && with_kept_of(&self.kept, |k| {
            let more = k.account.lagged < WALL_AGAIN;
            if more {
                k.account.lagged += 1;
            }
            more
        })
    }

    /// The settings side of the agent's login.
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

    /// A renewable login's own end ([`accounts::Point::LoginEnded`]), at the advice it is asked for.
    fn login_ended(&self, seat: &AccountSeat, roster: &accounts::Roster, look: &AccountLook) {
        let Some(label) = renewable_own(roster, look) else {
            return;
        };
        let done = login_transact_of(
            &self.sid,
            &self.hooks,
            seat,
            roster,
            look,
            &label,
            |state, logins, now| Ok(accounts::note_login_end(state, logins, &label, now)),
        );
        match done {
            Ok(events) if events.iter().any(|e| e.kind == accounts::Kind::LoginEnded) => {
                aterm_log::info!("harness @{}: account {label}: its login ended", self.sid);
                (self.hooks.acts.accounts_upkeep).make(UpkeepStep::Renew);
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

    /// Whether a login has the identity evidence needed to be marked back in.
    /// The recording transaction still checks the captured row and login under its lock.
    fn login_identity_confirmed(
        &self,
        seat: &AccountSeat,
        roster: &accounts::Roster,
        look: &AccountLook,
        label: &str,
    ) -> bool {
        let me = look.who.me(roster);
        if !me.default_home || !me.stored_login || me.account != Some(label) {
            return false;
        }
        if let Some(email) = roster
            .row(label)
            .and_then(accounts::Row::expected_identity_email)
        {
            let Some(env) = (self.hooks.acts.account_env)(look.snap.pid) else {
                return false;
            };
            match aterm_agent::harness::profile::answers_to(&env, &seat.home, email) {
                Some(true) => {}
                Some(false) => {
                    aterm_log::info!(
                        "harness @{}: account {label}: profile belongs to another account; \
                         sign-in identity is unconfirmed",
                        self.sid
                    );
                    return false;
                }
                None => return false,
            }
        }
        true
    }

    /// A login signed back in where it stands.
    fn signed_back(
        &self,
        seat: &AccountSeat,
        roster: &accounts::Roster,
        look: &AccountLook,
        label: &str,
    ) {
        if !self.login_identity_confirmed(seat, roster, look, label) {
            return;
        }
        let done = login_transact_of(
            &self.sid,
            &self.hooks,
            seat,
            roster,
            look,
            label,
            |state, logins, now| Ok(accounts::note_signed_back(state, logins, label, now)),
        );
        match done {
            Ok(events) if !events.is_empty() => {
                aterm_log::info!("harness @{}: account {label}: signed in again", self.sid);
            }
            Ok(_) => {}
            Err(_) => {
                aterm_log::info!(
                    "harness @{}: account {label}: signed in again, not recorded",
                    self.sid
                );
            }
        }
    }

    /// A turn end under a renewable login.
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
        let done = login_transact_of(
            &self.sid,
            &self.hooks,
            seat,
            roster,
            look,
            &label,
            |_, logins, now| {
                accounts::note_served_login(logins, &label, now);
                Ok(Vec::new())
            },
        );
        if done.is_err() {
            aterm_log::info!(
                "harness @{}: account {label}: served, not recorded",
                self.sid
            );
        }
    }

    /// The accounts as this host has them and this session's agent among them.
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

    /// Whether anything of the accounts acts for this session under `roster`, the switches read live.
    fn accounts_act(&self, roster: &accounts::Roster) -> bool {
        self.switches.accounts() != accounts::Mode::Off
            || (self.switches.logins() && renewable_listed(roster))
    }

    /// When this host first met the lost login of process `snap` ([`AccountKept::lost`]).
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

    /// What the agent's own process says of its login ([`AccountLook`]; the recorder's read is [`account_wall_of`]).
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

    /// [`IdleHost::account`]'s judgment, before it is kept.
    fn account_advice(&self, point: AccountPoint, owed: bool) -> Option<AccountAdvice> {
        let mode = self.switches.accounts();
        let notes = self.switches.notes();
        if !self.switches.accounts_live() {
            if !notes {
                accounts_tell(&self.switches, &self.hooks, AccountsSaid::Nothing, false);
            }
            return None;
        }
        account_land(&self.sid, &self.kept, &self.hooks, Landing::Asked);
        if accounts::Cli::of(self.agent).is_some_and(|cli| cli != accounts::Cli::Claude) {
            self.pool_said(point);
            return None;
        }
        if !accounts_for(self.agent) {
            return None;
        }
        let seat = (self.hooks.acts.account_seat)()?;
        let roster = match roster_read(&seat) {
            Ok(roster) => roster,
            Err(unread) => {
                let none = unread == RosterUnread::Nothing;
                let own = none && self.switches.reset_on();
                let said = if none && notes {
                    accounts_said(
                        mode != accounts::Mode::Off,
                        self.switches.logins(),
                        notes,
                        &seat,
                        &self.login_latches.carried_rows(),
                    )
                } else {
                    Some(unread.into())
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
        if let Some(to) = self.account_untold(&look.snap, usage_limit(point) && wall.is_none()) {
            aterm_log::info!(
                "harness @{}: account advice: its move to {to} has landed; told as made",
                self.sid
            );
            return Some(AccountAdvice::Move { to });
        }
        let store = accounts::Store::at(&seat.state);
        if at == accounts::Point::LoginEnded {
            self.login_ended(&seat, &roster, &look);
        }
        let (state, unread) = store.load(seat.now).ok()?;
        let enrolled = accounts::enrolled(&roster);
        if unread.is_none()
            && let Ok((logins, None)) = store.load_logins(seat.now)
        {
            accounts_tell(
                &self.switches,
                &self.hooks,
                AccountsSaid::listed(
                    &roster,
                    &state,
                    &logins,
                    seat.now,
                    &self.login_latches.carried_rows(),
                ),
                false,
            );
        }
        if at == accounts::Point::TurnEnd {
            self.login_served(&seat, &roster, &look);
        }
        let me = asked_at(look.who.me(&roster), point, self.switches.logins());
        let advice = accounts::advise(&roster, &state, &enrolled, &me, &at, mode, seat.now);
        let past = matches!(advice, accounts::Advice::Probe).then(|| {
            accounts::advise_past_probe(&roster, &state, &enrolled, &me, &at, mode, seat.now)
        });
        self.account_note(&seat, &roster, &look, &state);
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
        if self.switches.reset_on()
            && let Some(label) = look.judged(&roster)
            && accounts::reset_in_flight(&state, label, seat.now).is_some()
        {
            aterm_log::info!(
                "harness @{}: account advice: a limit reset is in flight on {label}; nobody moves \
                 off it, asked again shortly",
                self.sid
            );
            return account_resetting(
                advised,
                Instant::now() + (self.hooks.pause)(RESET_AGAIN_AFTER),
            );
        }
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
            (accounts::Advice::Wait { until }, Some(_)) => aterm_log::info!(
                "harness @{}: account advice: wait on {} (until {until})",
                self.sid,
                look.label.as_deref().unwrap_or("-")
            ),
            (_, Some(_)) => aterm_log::info!(
                "harness @{}: account advice: wait on {}",
                self.sid,
                look.label.as_deref().unwrap_or("-")
            ),
            _ => {
                if let Some(why) = stay.filter(|why| said != Some(*why)) {
                    aterm_log::info!("harness @{}: account advice: stay ({why})", self.sid);
                }
            }
        }
        let lags = evidence_lags(point, look.judged(&roster).is_some(), unwritten, owed);
        if matches!(advised, None | Some(AccountAdvice::Wait { .. })) && self.account_lags(lags) {
            aterm_log::info!(
                "harness @{}: account advice: the limit's record is not there yet; asked again \
                 shortly",
                self.sid
            );
            return account_lagging(
                advised,
                Instant::now() + (self.hooks.pause)(WALL_AGAIN_AFTER),
            );
        }
        if at_wall
            && !matches!(
                decided,
                Some(AccountAdvice::Move { .. } | AccountAdvice::Resume { .. })
            )
        {
            (self.hooks.acts.accounts_upkeep).make(UpkeepStep::Renew);
        }
        advised
    }

    /// The session's row for the report (`aterm harness accounts`: `on: @s-… (held: jobs)`).
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
            aterm_log::info!("harness @{}: account session not noted: refused", self.sid);
        }
    }

    /// One short transaction on the accounts state for this session ([`account_transact_of`]).
    fn account_transact(
        &self,
        seat: &AccountSeat,
        edit: impl FnOnce(&mut accounts::State, i64) -> Result<Vec<accounts::Event>, String>,
    ) -> Result<Vec<accounts::Event>, String> {
        self.reset_flush(seat);
        account_transact_of(&self.sid, &self.hooks, seat, edit)
    }

    /// The claim of this session's account's one `/usage` probe ([`IdleHost::account_probe`]).
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
                why.word()
            ),
        }
        let (probing, taken) = match claimed {
            Ok(probing) => (Some(probing), Ok(())),
            Err(why) => (None, Err(why)),
        };
        with_kept_of(&self.kept, |k| k.account.probing = probing);
        taken
    }

    /// [`Self::probe_claim`]'s decision.
    fn probe_claimed(&self) -> Result<Probing, NoProbe> {
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

    /// What the claimed probe read, told by the loop once after a claim ([`IdleHost::account_probed`]).
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

    /// [`Self::probe_record`] for a settled panel's `sample`.
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
        let mut low_or_limited = false;
        let noted = self.account_transact(&seat, |state, now| {
            let sample = accounts::Sample {
                at: now,
                ..sample.clone()
            };
            let events = accounts::note_sample(state, &probing.label, sample, now);
            low_or_limited = !events.is_empty()
                && state
                    .accounts
                    .get(&probing.label)
                    .is_some_and(|a| a.low(now) || a.limited_until(now).is_some());
            Ok(events)
        });
        match noted {
            Ok(events) if events.is_empty() => "sample dropped: no figure, or none newer",
            Ok(_) => {
                if low_or_limited {
                    (self.hooks.acts.accounts_upkeep).make(UpkeepStep::Renew);
                }
                "sampled"
            }
            Err(why) if why == accounts::LOCK_HELD => "sample dropped: lock-held",
            Err(_) => "sample dropped: not recorded",
        }
    }

    /// [`Self::account_ctx`] for a limit reset, asked only with `[harness] limit_reset` on.
    fn reset_ctx(&self) -> Result<(AccountSeat, accounts::Roster, AccountLook), NoReset> {
        if !accounts_for(self.agent) {
            return Err(NoReset::Unread);
        }
        let seat = (self.hooks.acts.account_seat)().ok_or(NoReset::Unread)?;
        let roster = account_roster_or_own(&seat).ok_or(NoReset::Roster)?;
        let look = self.account_look(&roster).ok_or(NoReset::Unread)?;
        Ok((seat, roster, look))
    }

    /// The claim of this session's account's limit reset.
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
    fn reset_claimed(&self, ask: &ResetAsk, said: &mut Option<String>) -> Result<ResetGo, NoReset> {
        if !self.switches.reset_on() {
            return Err(NoReset::Off);
        }
        if self.program_check_stuck() {
            return Err(NoReset::ProgramCheck);
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
                .and_then(|record| record.cleared_since(&wall.window, wall.at, now))
                .is_some();
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
            match accounts::claim_reset(
                state,
                &label,
                (&wall.window, until),
                &self.sid,
                pid,
                start,
                now,
            ) {
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

    /// Ends this worker's claim if a failed transaction still left it readable as open. Under the sync
    /// commit a pre-admission refusal saves nothing and an interrupted publication fences reads until
    /// recovery, so this is a belt for any path that saved the claim without its outcome.
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

    /// How the claimed reset ended.
    fn reset_tell(&self, end: &ResetEnd) -> ResetOutcome {
        let unread = |why: &str| {
            aterm_log::info!("harness @{}: limit reset's end not read: {why}", self.sid);
            ResetOutcome::Unread
        };
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

    /// Write the limit reset's outcome this session holds ([`AccountKept::reset_owed`]), if any.
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
                with_kept_of(&self.kept, |k| {
                    k.account.reset_owed.get_or_insert(owed);
                });
                Flushed::Held
            }
            Err(_) => Flushed::Refused,
        }
    }

    /// The advice of a session on a machine that lists no account.
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
            return account_resetting(None, Instant::now() + (self.hooks.pause)(RESET_AGAIN_AFTER));
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

    /// The move ([`Restart::Account`]), returning one step word as every restart does.
    fn account_move(&self, why: &Restart, to: &str, at: AccountAt) -> String {
        const CHANGED: &str = "wait:account-changed";
        const STATE: &str = "wait:account-state";
        let hooks = &self.hooks;
        let at_wall = at == AccountAt::Wall;
        let advised = with_kept_of(&self.kept, |k| k.account.advised.take())
            .filter(|a| a.to == to && at_wall == a.point.at_wall());
        let mode = self.switches.accounts();
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
        let before = (look.snap.pid, look.snap.start.clone());
        let step = if self.can_restart() {
            self.act(|| (hooks.acts.restart)(&self.sid, self.grace, why))
        } else {
            "refused:supervisor-off".to_string()
        };
        note_upgrade_act();
        let fresh = refresh_snapshot(&self.sid, &self.kept, hooks);
        let after = with_kept_of(&self.kept, |k| k.snapshot.clone()).filter(|_| fresh);
        let flying = Flying {
            to: to.to_string(),
            at,
            login: back.clone(),
            pid: before.0,
            start: before.1.clone(),
        };
        if relaunch_took(&step) {
            if step == "adopted" {
                self.park.store(true, Ordering::SeqCst);
            }
            let applied = match after {
                None => Applied::Unread,
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
                    with_kept_of(&self.kept, |k| k.account.flying = Some(flying));
                    self.account_unmade(&seat, (to, label.as_str()), canary, owed, None, None);
                    "wait:account-unverified".to_string()
                }
            };
        }
        if step == relaunch::IN_FLIGHT_OTHER {
            self.park.store(true, Ordering::SeqCst);
            let why = accounts::Strike::NotMoved(step.clone());
            self.account_unmade(&seat, (to, label.as_str()), canary, owed, Some(&why), None);
            return step;
        }
        let signalled = (hooks.acts.account_restart)(&self.sid, before.0).as_deref() == Some(to);
        let why = match (signalled, step.starts_with("failed:")) {
            (true, true) => Some(accounts::Strike::Failed(step.clone())),
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

    /// Whether the process an account move's relaunch brought up runs under the target ([`Applied`]).
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

    /// The answer of a move that was not made ([`Self::account_move`]), in one transaction.
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

    /// Apply the limit stamp [`IdleHost::limited`] left, if any ([`Acts::hold`]).
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

/// The word each tab's last relaunch snapshot read was refused by, said when it changes.
fn snapshot_refused(sid: &str, why: Option<&'static str>) {
    static SAID: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);
    let mut said = SAID.lock().unwrap_or_else(PoisonError::into_inner);
    let said = said.get_or_insert_with(HashMap::new);
    let before = match why {
        Some(why) => said.insert(sid.to_string(), why),
        None => said.remove(sid),
    };
    if let Some(why) = why.filter(|why| before != Some(why)) {
        aterm_log::info!("harness @{sid}: the relaunch snapshot could not be read ({why})");
    }
}

/// Read the relaunch record again.
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
        Some(snap) => land_snapshot(sid, kept, hooks, snap, |_| {
            revoked.is_some_and(|revoked| revoked.load(Ordering::SeqCst))
        }),
        None => {
            follow_snapshot_guarded(sid, kept, hooks, revoked);
            false
        }
    }
}

/// Land the relaunch snapshot `snap` in what the host keeps of `sid` unless `stale` says not: whether it landed.
fn land_snapshot(
    sid: &str,
    kept: &Mutex<Kept>,
    hooks: &Hooks,
    mut snap: Snapshot,
    stale: impl FnOnce(&Kept) -> bool,
) -> bool {
    let (moved, said) = {
        let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
        if stale(&kept) {
            return false;
        }
        keep_session(kept.snapshot.as_ref(), &mut snap);
        let moved = publish_snapshot(sid, Some(&snap));
        let said = asked_runs(sid, &mut kept, &snap);
        kept.snapshot = Some(snap);
        (moved, said)
    };
    tell_published(sid, moved, hooks);
    told_asked_runs(sid, said, hooks);
    true
}

/// How long an idle point waits for its relaunch snapshot read before it goes on without it: at
/// load average 268 one read took 57 s.
const POINT_READ_BOUND: Duration = Duration::from_millis(500);

/// What [`Acts::inert`]'s idle point waits for its read: a hang detector's minute.
#[cfg(any(test, feature = "bench-support"))]
const INERT_POINT_READ: Duration = Duration::from_secs(60);

/// An idle step that took this long is logged with where its time went ([`laps`]).
const SLOW_STEP: Duration = Duration::from_secs(1);

/// A relaunch snapshot read started on a thread of its own: whether it ended.
#[derive(Default)]
struct PointRead {
    ended: Mutex<bool>,
    end: Condvar,
}

impl PointRead {
    /// Wait for the read to end, `bound` at most: whether it ended.
    fn wait(&self, bound: Duration) -> bool {
        let ended = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
        let (ended, _) = self
            .end
            .wait_timeout_while(ended, bound, |ended| !*ended)
            .unwrap_or_else(PoisonError::into_inner);
        *ended
    }

    /// The read `read` of the session `kept` is over: no longer out, and every point waiting on it goes on.
    fn over(kept: &Mutex<Kept>, read: &Arc<Self>) {
        with_kept_of(kept, |k| {
            if k.point_read
                .as_ref()
                .is_some_and(|out| Arc::ptr_eq(out, read))
            {
                k.point_read = None;
            }
        });
        *read.ended.lock().unwrap_or_else(PoisonError::into_inner) = true;
        read.end.notify_all();
    }
}

/// Read the relaunch record again at an idle point on a thread of its own, one per session, waiting `bound` at most: whether it ended.
fn refresh_snapshot_at_point(
    sid: &str,
    kept: &Arc<Mutex<Kept>>,
    hooks: &Hooks,
    bound: Duration,
) -> bool {
    let read = start_point_read(sid, kept, hooks, None);
    let ended = read.wait(bound);
    if !ended {
        aterm_log::info!(
            "harness @{sid}: its relaunch snapshot read is still out after {} ms; the idle point \
             goes on with the snapshot kept, and the read lands as it ends",
            bound.as_millis()
        );
    }
    ended
}

/// Start or join the one outstanding snapshot read. Passive repairs do not wait or read inline.
fn start_point_read(
    sid: &str,
    kept: &Arc<Mutex<Kept>>,
    hooks: &Hooks,
    owner: Option<PassiveReadOwner>,
) -> Arc<PointRead> {
    let (read, begun) = {
        let mut k = kept.lock().unwrap_or_else(PoisonError::into_inner);
        match &k.point_read {
            Some(out) => (Arc::clone(out), None),
            None => {
                let read = Arc::new(PointRead::default());
                k.point_read = Some(Arc::clone(&read));
                (read, Some(k.snapshot.clone()))
            }
        }
    };
    if let Some(before) = begun {
        let passive = owner.is_some();
        let (job_sid, job_kept, job_hooks, job_read) = (
            sid.to_string(),
            Arc::clone(kept),
            hooks.clone(),
            Arc::clone(&read),
        );
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}point-read"))
            .spawn(move || {
                /// The read is over as its thread returns or unwinds.
                struct Over(Arc<Mutex<Kept>>, Arc<PointRead>);
                impl Drop for Over {
                    fn drop(&mut self) {
                        PointRead::over(&self.0, &self.1);
                    }
                }
                let over = Over(job_kept, job_read);
                match owner {
                    Some(owner) => {
                        land_passive_read(&job_sid, &over.0, &job_hooks, before.as_ref(), &owner);
                    }
                    None => {
                        land_point_read(&job_sid, &over.0, &job_hooks, before.as_ref());
                    }
                }
            });
        if let Err(e) = spawned {
            if passive {
                aterm_log::warn!("harness @{sid}: no thread for the passive snapshot read ({e})");
            } else {
                aterm_log::warn!(
                    "harness @{sid}: no thread for the relaunch snapshot read ({e}); the idle point \
                     reads it itself"
                );
                refresh_snapshot(sid, kept, hooks);
            }
            PointRead::over(kept, &read);
        }
    }
    read
}

/// The primary's restart identity, excluding ordinary record progress and heartbeat changes.
#[derive(Clone, PartialEq, Eq)]
struct PassivePrimary {
    pid: u32,
    start: String,
    home: std::path::PathBuf,
    resume: self_claimed::Resume,
}

impl PassivePrimary {
    fn current(sid: &str, hooks: &Hooks) -> Option<Self> {
        let copy = (hooks.acts.live_self_copy)(sid)?;
        let resume = copy.resume.ok()?;
        let self_claimed::Seen::Present(record) = copy.seen else {
            return None;
        };
        if record.pid != copy.pid
            || record.exit.is_some()
            || record.session_id.as_deref() != Some(resume.session.as_str())
            || record.resume_argv.as_ref() != Some(&resume.argv)
        {
            return None;
        }
        Some(Self {
            pid: copy.pid,
            start: copy.start,
            home: copy.home,
            resume,
        })
    }

    fn matches(&self, sid: &str, snap: &Snapshot) -> bool {
        snap.tab == sid
            && snap.pid == self.pid
            && snap.start == self.start
            && snap.session.as_deref() == Some(self.resume.session.as_str())
            && matches!(&snap.run, relaunch::AgentRun::Codex(run)
                if run.home == self.home && run.self_claimed.as_ref() == Some(&self.resume))
    }
}

/// A passive read belongs to one worker generation and the live primary that asked for it.
struct PassiveReadOwner {
    primary: PassivePrimary,
    claim_epoch: u64,
    revoked: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    group: Arc<AtomicI32>,
    group_at: i32,
    switches: Arc<Switches>,
}

impl PassiveReadOwner {
    fn current(&self, hooks: &Hooks) -> bool {
        !self.revoked.load(Ordering::SeqCst)
            && !self.stop.load(Ordering::SeqCst)
            && !self.done.load(Ordering::SeqCst)
            && self.group.load(Ordering::SeqCst) == self.group_at
            && self.switches.on_for(Program::Codex)
            && (hooks.claim_epoch)() == self.claim_epoch
    }
}

/// Read with the normal process/tab validators, then admit only the same still-live primary.
fn land_passive_read(
    sid: &str,
    kept: &Mutex<Kept>,
    hooks: &Hooks,
    before: Option<&Snapshot>,
    owner: &PassiveReadOwner,
) -> bool {
    if !owner.current(hooks) {
        return false;
    }
    let Some(snap) = (hooks.acts.snapshot)(sid) else {
        return false;
    };
    let mut snap = with_self_claimed_line(sid, snap, false);
    if !owner.primary.matches(sid, &snap)
        || (hooks.acts.follow)(sid, &mut snap) != Foreground::Agent
        || !owner.primary.matches(sid, &snap)
        || PassivePrimary::current(sid, hooks).as_ref() != Some(&owner.primary)
    {
        return false;
    }
    // All hooks that can take session metadata ran before Kept. Revocation and worker
    // replacement fence the actual publication as well as the start of the read.
    land_snapshot(sid, kept, hooks, snap, |k| {
        !owner.current(hooks) || k.snapshot.as_ref() != before
    })
}

/// The end of an idle point's relaunch snapshot read, landed only while the kept snapshot is still `before`.
fn land_point_read(
    sid: &str,
    kept: &Mutex<Kept>,
    hooks: &Hooks,
    before: Option<&Snapshot>,
) -> bool {
    match (hooks.acts.snapshot)(sid) {
        Some(snap) => land_snapshot(sid, kept, hooks, snap, |k| k.snapshot.as_ref() != before),
        None => {
            if with_kept_of(kept, |k| k.snapshot.as_ref() != before) {
                return false;
            }
            follow_snapshot_guarded(sid, kept, hooks, None);
            false
        }
    }
}

/// A read `snap` that cannot name the conversation (a codex-alab whose thread lock the read missed)
/// keeps what the earlier read `before` of the same process named.
fn keep_session(before: Option<&Snapshot>, snap: &mut Snapshot) {
    if snap.session.is_none()
        && let Some(before) = before
        && (before.pid, before.start.as_str()) == (snap.pid, snap.start.as_str())
    {
        snap.session.clone_from(&before.session);
    }
}

static PUBLISHED: Mutex<Option<HashMap<String, Snapshot>>> = Mutex::new(None);

/// Publish `sid`'s kept snapshot (`None`: it has none).
fn publish_snapshot(sid: &str, snap: Option<&Snapshot>) -> bool {
    publish_snapshot_from(sid, snap, false)
}

/// `handed` is true only for the snapshot admitted by `watch_handed`.
fn publish_snapshot_from(sid: &str, snap: Option<&Snapshot>, handed: bool) -> bool {
    let carried = |snap: &Snapshot| crate::restore::AgentRestore::of(snap.clone());
    let snap = snap.map(|snap| with_self_claimed_line(sid, snap.clone(), handed));
    let moved = {
        let mut map = PUBLISHED.lock().unwrap_or_else(PoisonError::into_inner);
        let map = map.get_or_insert_with(HashMap::new);
        let before = match &snap {
            Some(snap) => map.insert(sid.to_string(), snap.clone()),
            None => map.remove(sid),
        };
        before.as_ref().map(carried) != snap.as_ref().map(carried)
    };
    if snap
        .as_ref()
        .and_then(|snap| snap.session.as_deref())
        .is_some_and(lost_session)
    {
        ring();
    }
    moved
}

/// A proven self-claimed codex-alab's snapshot carries its validated line.
fn with_self_claimed_line(sid: &str, mut snap: Snapshot, handed: bool) -> Snapshot {
    let (pid, start) = (snap.pid, snap.start.clone());
    let session = snap.session.clone();
    if let Some(run) = snap.codex_mut() {
        run.self_claimed = match crate::self_claim::copy_of_tab(sid) {
            Some(copy) if copy.pid == pid && copy.start == start => {
                copy.resume.ok().filter(|resume| {
                    session.as_deref() == Some(resume.session.as_str())
                        && run.self_claimed.as_ref().is_none_or(|old| old == resume)
                })
            }
            None if handed => run.self_claimed.take(),
            Some(_) | None => None,
        };
    }
    snap
}

/// Tells the window when what a restore carries of a tab's agent moved (`moved`, [`publish_snapshot`]).
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

/// The relaunch snapshot the host keeps for session `sid`, as last published ([`publish_snapshot`]).
pub(crate) fn published_snapshot(sid: &str) -> Option<Snapshot> {
    PUBLISHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|m| m.get(sid).cloned())
}

/// The agent a layout that outlives it names.
pub(crate) fn followed_snapshot(sid: &str) -> Option<Snapshot> {
    static RECORDS: std::sync::LazyLock<RecordReader> =
        std::sync::LazyLock::new(|| RecordReader::new(relaunch::follow));
    followed_snapshot_by(sid, &RECORDS, RECORD_READ_BOUND)
}

/// [`followed_snapshot`] through `records`, waited for `bound` at most.
fn followed_snapshot_by(sid: &str, records: &RecordReader, bound: Duration) -> Option<Snapshot> {
    let snap = published_snapshot(sid)?;
    if snap.claude_dir.is_none() {
        return Some(snap);
    }
    let Some(home) = aterm_primer::home_dir() else {
        return Some(snap);
    };
    let Some(followed) = records.follow(&home, &snap, bound) else {
        return Some(snap);
    };
    if followed != snap {
        ring();
    }
    Some(followed)
}

#[cfg(not(test))]
const RECORD_READ_BOUND: Duration = Duration::from_millis(50);
#[cfg(test)]
const RECORD_READ_BOUND: Duration = aterm_spec::wait::MUST_HAPPEN;

/// What the agents' records thread does with one snapshot.
type RecordReadFn = dyn Fn(&Path, &mut Snapshot) -> bool + Send + Sync;

/// One read for the agents' records thread.
type RecordJob = (
    u64,
    PathBuf,
    Snapshot,
    std::sync::mpsc::SyncSender<Snapshot>,
);

/// The agents' records thread ([`followed_snapshot`]).
pub(crate) struct RecordReader {
    read: Arc<RecordReadFn>,
    jobs: Mutex<Option<std::sync::mpsc::Sender<RecordJob>>>,
    /// The last read asked, the last one done, and the last one whose asker gave up waiting.
    asked: AtomicU64,
    done: Arc<AtomicU64>,
    given_up: AtomicU64,
}

impl RecordReader {
    fn new(read: impl Fn(&Path, &mut Snapshot) -> bool + Send + Sync + 'static) -> Self {
        Self {
            read: Arc::new(read),
            jobs: Mutex::new(None),
            asked: AtomicU64::new(0),
            done: Arc::new(AtomicU64::new(0)),
            given_up: AtomicU64::new(0),
        }
    }

    /// `snap` followed through its record under `home`, waited for `bound` at most.
    fn follow(&self, home: &Path, snap: &Snapshot, bound: Duration) -> Option<Snapshot> {
        if self.given_up.load(Ordering::SeqCst) > self.done.load(Ordering::SeqCst) {
            return None;
        }
        let seq = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        if !self.send((seq, home.to_path_buf(), snap.clone(), reply)) {
            return None;
        }
        match answer.recv_timeout(bound) {
            Ok(followed) => Some(followed),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.given_up.fetch_max(seq, Ordering::SeqCst);
                aterm_log::warn!(
                    "harness: an agent's record did not answer within {} ms; the layout names \
                     the host's copy until it does",
                    bound.as_millis()
                );
                None
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Hand `job` to the thread, starting it at the first.
    fn send(&self, job: RecordJob) -> bool {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if jobs.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<RecordJob>();
            let (read, done) = (Arc::clone(&self.read), Arc::clone(&self.done));
            let started = std::thread::Builder::new()
                .name("aterm-agent-records".into())
                .spawn(move || {
                    for (seq, home, mut snap, reply) in rx {
                        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            read(&home, &mut snap)
                        }));
                        done.store(seq, Ordering::SeqCst);
                        if read.is_ok() {
                            let _ = reply.send(snap);
                        }
                    }
                });
            if let Err(error) = started {
                aterm_log::warn!("harness: the agents' records thread did not start ({error})");
                return false;
            }
            *jobs = Some(tx);
        }
        let sent = jobs.as_ref().is_some_and(|tx| tx.send(job).is_ok());
        if !sent {
            *jobs = None;
        }
        sent
    }
}

/// [`follow_snapshot`], caught, for whoever follows the tab.
fn follow_caught(sid: &str, kept: &Mutex<Kept>, hooks: &Hooks) -> Option<bool> {
    let following = with_kept_of(kept, |k| Arc::clone(&k.following));
    let _one = following.lock().unwrap_or_else(PoisonError::into_inner);
    if with_kept_of(kept, |k| k.follow_faulted) {
        return None;
    }
    let followed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        follow_snapshot(sid, kept, hooks)
    }))
    .map_err(|payload| {
        aterm_log::warn!(
            "harness @{sid}: following its agent stopped after an internal panic; it is \
                     not followed again for this tab: {}",
            panic_text(payload.as_ref())
        );
    })
    .ok();
    if followed.is_none() {
        with_kept_of(kept, |k| k.follow_faulted = true);
    }
    followed
}

/// Follow the session's kept snapshot against the tab's foreground job, cheaply (no socket, no `ps`; [`Acts::follow`]).
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
    let mut said = (Say::Nothing, false);
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
                said = asked_runs(sid, &mut session, &snap);
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
    told_asked_runs(sid, said, hooks);
    true
}

/// The tab's agent, read, holds the conversation its standing word asks for: the word is answered.
fn asked_runs(sid: &str, kept: &mut Kept, snap: &Snapshot) -> (Say, bool) {
    let Some(conversation) = snap.session.as_deref() else {
        return (Say::Nothing, false);
    };
    let said = kept.relaunches.conversation_runs(conversation);
    let back = kept
        .lost
        .as_ref()
        .is_some_and(|lost| lost.session == conversation && lost.pid != snap.pid);
    if back && let Some(lost) = kept.lost.take() {
        forget_lost(&lost.session, sid);
    }
    (said, back)
}

/// [`asked_runs`]' word, told with no lock held: the tab's mark and the lines it made end.
fn told_asked_runs(sid: &str, (said, back): (Say, bool), hooks: &Hooks) {
    if said == Say::Clear || back {
        aterm_log::info!(
            "harness @{sid}: the conversation the tab's mark asked for runs in it again; the \
             mark ends"
        );
        (hooks.badge)(sid, None);
    }
    if said == Say::Clear {
        (hooks.note)(HostNote::Relaunch {
            sid: sid.to_string(),
            said: RelaunchSay::Ended,
        });
    }
    if back {
        (hooks.note)(HostNote::Relaunch {
            sid: sid.to_string(),
            said: RelaunchSay::Back,
        });
    }
}

/// Whether the tab's mark is a standing word that its agent could not be brought back ([`Kept::lost`], marked).
fn lost_marked(k: &Kept) -> bool {
    k.lost.as_ref().is_some_and(|lost| lost.marked)
}

/// The agent's exit is handled: what was read of it goes with it.
fn forget_snapshot(job: &WorkerJob, hooks: &Hooks) {
    let moved = publish_snapshot(&job.sid, None);
    with_kept(job, |k| k.snapshot = None);
    tell_published(&job.sid, moved, hooks);
}

/// The session's agent left and nothing brings it back, so the accounts recorder forgets the session ([`Acts::account_gone`]).
fn account_gone(job: &WorkerJob, hooks: &Hooks) {
    if accounts_for(job.agent) {
        with_kept(job, |k| k.account = AccountKept::default());
        (hooks.acts.account_gone)(&job.sid);
    }
}

/// The agent is gone for good, as its task record hears it.
fn task_exited(job: &WorkerJob, hooks: &Hooks) {
    (hooks.acts.task_exited)(&job.sid);
}

/// `f` over the session's [`Kept`], under its lock (a leaf: `f` takes no other lock).
fn with_kept<R>(job: &WorkerJob, f: impl FnOnce(&mut Kept) -> R) -> R {
    with_kept_of(&job.kept, f)
}

/// [`with_kept`] over the [`Kept`] itself.
fn with_kept_of<R>(kept: &Mutex<Kept>, f: impl FnOnce(&mut Kept) -> R) -> R {
    let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut kept)
}

const EXIT_WATCH_LOOK: Duration = Duration::from_secs(1);

static EXIT_WATCH_ARMINGS: AtomicU64 = AtomicU64::new(0);

const EVERY_WATCH: u64 = u64::MAX;

/// Arms the exit watch ([`Kept::exit_watch`]) on the process session `sid`'s snapshot names, unless one watches it already.
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

/// [`watch_exit`]'s thread: the end of `snap`'s process, then what it left, kept while the watch is still armed as `arming`.
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

/// What `snap`'s exit left as it was seen, for the worker handed it.
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

/// What a crash left is what the layout names.
fn keep_exit_read(job: &WorkerJob, hooks: &Hooks, snap: &mut Snapshot, left: &ExitRecord) {
    let ExitRecord::Survived(sf) = left else {
        return;
    };
    relaunch::follow_record(snap, sf);
    let moved = with_kept(job, |k| {
        if job.revoked.load(Ordering::SeqCst) {
            return false;
        }
        let Some(kept) = k
            .snapshot
            .as_mut()
            .filter(|kept| (kept.pid, kept.start.as_str()) == (snap.pid, snap.start.as_str()))
        else {
            return false;
        };
        if !relaunch::follow_record(kept, sf) {
            return false;
        }
        aterm_log::info!(
            "harness @{}: the record its agent left names conversation {}",
            job.sid,
            kept.session.as_deref().unwrap_or("-")
        );
        publish_snapshot(&job.sid, Some(&*kept))
    });
    tell_published(&job.sid, moved, hooks);
}

/// The host sees its tabs again.
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

/// Relaunch on exit: the session's agent left a tab that lives on.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "Read",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "ReadByClaim",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
fn on_agent_left(job: &WorkerJob, hooks: &Hooks) {
    if !job.admitted() {
        crate::self_claim::discard_dead(job.sid.as_str());
        settled(job);
        return;
    }
    let sid = job.sid.as_str();
    let grace = job.opts.policy.human_grace_s;
    let snap = with_kept(job, |k| k.snapshot.clone());
    let asked = snap.as_ref().and_then(|snap| (hooks.acts.asked)(sid, snap));
    let codex_exit = match job.agent {
        Program::Codex => true,
        Program::Claude
        | Program::Grok
        | Program::Kimi
        | Program::Generic
        | Program::Agy
        | Program::Muse => false,
    };
    let copy = (hooks.acts.self_copy)(sid).filter(|_| codex_exit);
    match copy.filter(|copy| match snap.as_ref() {
        Some(snap) => snap.pid == copy.pid && snap.start == copy.start,
        None => !restart_stamp::alive_as(copy.pid, &copy.start),
    }) {
        Some(copy) => {
            let mut next = self_claimed_exit(job, hooks, snap, &copy, asked);
            while let Some(copy) = next {
                let snap = with_kept(job, |k| k.snapshot.clone());
                next = self_claimed_exit(job, hooks, snap, &copy, None);
            }
            return;
        }
        None => crate::self_claim::discard_dead(sid),
    }
    if codex_exit {
        unproven_codex_exit(job, hooks, snap.as_ref(), asked);
        return;
    }
    let lane = aterm_agent::harness::upgrade::Agent::of(job.agent).filter(|agent| {
        let lanes = aterm_agent::harness::lanes::lanes(agent.program());
        lanes.upgrade || lanes.restart_in_place
    });
    let pid = snap.as_ref().map(|s| s.pid);
    let restarted = lane.is_some_and(|agent| (hooks.acts.restarted)(sid, agent, pid));
    let act_snap = snap.clone().filter(|_| asked.is_some() && !restarted);
    let cause = std::cell::Cell::new(asked);
    let put_off = std::cell::Cell::new(false);
    let end_owed = std::cell::Cell::new(false);
    let hand_waits = std::cell::Cell::new(false);
    let lifted = std::cell::Cell::new(false);
    let watched = job.watch_until.is_some();
    let decide_at = |exit: bool| {
        put_off.set(false);
        let carried = hand_waits.replace(false);
        let mut exit = exit;
        if let Some(snap) = act_snap.as_ref() {
            let runs = restart_stamp::alive_as(snap.pid, &snap.start);
            if runs && (hooks.acts.asked)(sid, snap).is_some() {
                put_off.set(true);
                end_owed.set(true);
                return OnExit::Asked;
            }
            if end_owed.get() && !runs {
                end_owed.set(false);
                cause.set((hooks.acts.asked)(sid, snap));
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
        let carries = matches!(decision, OnExit::Restarted | OnExit::Asked);
        let waits = carries && status.as_deref().is_some_and(relaunch::hand_on_tab);
        hand_waits.set(waits);
        lifted.set(carried && !waits);
        decision
    };
    let decide = || decide_at(false);
    let decision = decide_at(true);
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
    let crash = asked.is_none() && !restarted;
    let pause = match with_kept(job, |k| {
        k.relaunches.exited_as(decision, crash, Instant::now())
    }) {
        Ok(_) if asked.is_some() => Duration::ZERO,
        Ok(pause) => pause,
        Err(said) => {
            forget_snapshot(job, hooks);
            left_alone(job, hooks, decision, said, false);
            return;
        }
    };
    if let Some(agent) = lane.filter(|_| restarted) {
        aterm_log::info!(
            "harness @{sid}: the agent exited; the harness's own restart ended it, and its \
             relaunch is carried"
        );
        let carry_said = std::cell::Cell::new(false);
        relaunch_until(job, hooks, pause, &decide, true, || {
            if hand_waits.get() {
                if !carry_said.replace(true) {
                    aterm_log::info!(
                        "harness @{sid}: the agent the harness's own restart ended is carried \
                         through another hand on its tab (a hold, a lease, a turn); its \
                         relaunch is typed once that hand lifts"
                    );
                }
                return relaunch::WAIT_HAND_LIFTS.to_string();
            }
            let step = (hooks.acts.carry)(sid, grace, agent, pid, lifted.replace(false));
            if step != relaunch::WAIT_HAND_LIFTS {
                carry_said.set(false);
            } else if !carry_said.replace(true) {
                aterm_log::info!(
                    "harness @{sid}: the restart the harness made is carried through a hand on \
                     its tab (a hold, a lease, a turn); its relaunch line is typed once that \
                     hand lifts"
                );
            }
            step
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
        task_exited(job, hooks);
        return;
    };
    let mut snap = snap;
    account_way_back(job, hooks, &mut snap);
    let left = read_at_exit(job, hooks, &snap);
    aterm_log::info!(
        "harness @{sid}: the agent exited; read at the exit: {}",
        left.word()
    );
    keep_exit_read(job, hooks, &mut snap, &left);
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
    let away_said = std::cell::Cell::new(false);
    let hand_said = std::cell::Cell::new(false);
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

/// The crash rule: the exit of an agent a proven self-claim supervised, decided by this host's watch-only worker alone.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "CopyRelaunches",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
) -> Option<self_claimed::Copy> {
    let sid = job.sid.as_str();
    let grace = job.opts.policy.human_grace_s;
    crate::self_claim::take_copy(sid, copy.pid, &copy.start);
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
        return None;
    }
    let pause = match with_kept(job, |k| {
        k.relaunches
            .exited_as(decision, asked.is_none(), Instant::now())
    }) {
        Ok(_) if asked.is_some() => Duration::ZERO,
        Ok(pause) => pause,
        Err(said) => {
            forget_snapshot(job, hooks);
            left_alone(job, hooks, decision, said, false);
            return None;
        }
    };
    if let Some(other) = snap
        .as_ref()
        .filter(|snap| (snap.pid, snap.start.as_str()) != (copy.pid, copy.start.as_str()))
    {
        aterm_log::info!(
            "harness @{sid}: the snapshot kept names codex-alab {} (started {}), not the crashed \
             {} (started {}): this crash is judged by its own copy alone, never relaunched from \
             that snapshot",
            other.pid,
            other.start,
            copy.pid,
            copy.start
        );
    }
    let resume = match &copy.resume {
        Ok(resume) => resume.clone(),
        Err(why) if copy.seen == self_claimed::Seen::Never => {
            settled(job);
            forget_snapshot(job, hooks);
            task_exited(job, hooks);
            aterm_log::info!(
                "harness @{sid}: the self-supervised codex-alab crashed before its record \
                 named a line; not relaunched ({why})"
            );
            inform(format!(
                "codex-alab ({}) crashed before its session record named a line, and is not \
                 relaunched",
                copy.pid
            ));
            return None;
        }
        Err(why) => {
            settled(job);
            forget_snapshot(job, hooks);
            task_exited(job, hooks);
            aterm_log::info!(
                "harness @{sid}: the self-supervised codex-alab crashed; not relaunched: its \
                 record's resume line did not validate against the live process ({why})"
            );
            inform(format!(
                "codex-alab ({}) crashed and is not relaunched: its session record's resume \
                 line did not match the process aterm saw run ({why})",
                copy.pid
            ));
            return None;
        }
    };
    let snap = crash_snapshot(snap, copy).or_else(|| {
        let snap = (hooks.acts.self_snapshot)(sid, copy, &resume)?;
        aterm_log::info!(
            "harness @{sid}: no snapshot of the crashed codex-alab {} was read while it ran; \
             its copy's line is relaunched at the shell holding the tab ({})",
            copy.pid,
            snap.shell
        );
        Some(snap)
    });
    let Some(snap) = snap else {
        settled(job);
        forget_snapshot(job, hooks);
        task_exited(job, hooks);
        aterm_log::info!(
            "harness @{sid}: the self-supervised codex-alab crashed; not relaunched: it was the \
             tab's own program, with no shell to relaunch it at"
        );
        inform(format!(
            "codex-alab ({}) crashed and is not relaunched: it was this tab's own program, with \
             no shell to start it again at",
            copy.pid
        ));
        return None;
    };
    aterm_log::info!(
        "harness @{sid}: the self-supervised codex-alab crashed; relaunching it on its \
         conversation {}",
        resume.session
    );
    crate::self_claim::typed(sid, &resume);
    let mut owed = snap.clone();
    if let Some(run) = owed.codex_mut() {
        run.self_claimed = Some(resume.clone());
    }
    with_kept(job, |k| {
        if !job.revoked.load(Ordering::SeqCst) {
            k.owed = Some(OwedRelaunch { snap: owed, asked });
        }
    });
    let gone = relaunch_until_with(
        job,
        hooks,
        pause,
        &decide,
        false,
        GoneUnread::HandedBack,
        || (hooks.acts.self_relaunch)(sid, grace, &snap, &resume, asked),
    );
    if gone {
        relaunched_gone(job, hooks, copy)
    } else {
        None
    }
}

/// The snapshot a self-claimed crash is relaunched from ([`self_claimed_exit`]).
fn crash_snapshot(snap: Option<Snapshot>, copy: &self_claimed::Copy) -> Option<Snapshot> {
    snap.filter(|snap| snap.shell != 0 && snap.pid == copy.pid && snap.start == copy.start)
}

/// The codex-alab the crash rule relaunched for `crashed`.
fn relaunched_gone(
    job: &WorkerJob,
    hooks: &Hooks,
    crashed: &self_claimed::Copy,
) -> Option<self_claimed::Copy> {
    let sid = job.sid.as_str();
    settled(job);
    let other = (hooks.acts.self_copy)(sid)
        .filter(|copy| (copy.pid, copy.start.as_str()) != (crashed.pid, crashed.start.as_str()));
    match other {
        Some(copy) if !restart_stamp::alive_as(copy.pid, &copy.start) => {
            aterm_log::info!(
                "harness @{sid}: the codex-alab relaunched for the crashed {} ended before any \
                 read of it; its exit is decided by its own copy (codex-alab {})",
                crashed.pid,
                copy.pid
            );
            Some(copy)
        }
        Some(copy) => {
            forget_snapshot(job, hooks);
            aterm_log::info!(
                "harness @{sid}: the codex-alab relaunched for the crashed {} runs on (codex-alab \
                 {}); its exit is its own worker's to decide",
                crashed.pid,
                copy.pid
            );
            None
        }
        None => {
            crate::self_claim::discard_dead(sid);
            forget_snapshot(job, hooks);
            task_exited(job, hooks);
            aterm_log::info!(
                "harness @{sid}: the codex-alab relaunched for the crashed {} ended before any \
                 read of it, and aterm holds no copy of its own record; not relaunched (never on \
                 {}'s record again)",
                crashed.pid,
                crashed.pid
            );
            (hooks.note)(HostNote::Inform {
                sid: sid.to_string(),
                text: format!(
                    "the codex-alab relaunched after codex-alab ({}) crashed ended before aterm \
                     could read it, with no claim proven to this aterm: it is not relaunched",
                    crashed.pid
                ),
            });
            None
        }
    }
}

/// Whose exit it is when the agent a relaunch landed is gone again before any read of it and before any pass named it ([`relaunch_until_with`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GoneUnread {
    /// Still that relaunch's: tried again on the record read at the exit (a Claude Code's relaunch
    /// reads its conversation's record again; a restart's carry never types twice).
    TriedAgain,
    /// The landed process's own (the crash rule, [`self_claimed_exit`]).
    HandedBack,
}

/// The exit of a codex no copy decides, read by its watch-only worker.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HarnessRestartStamp",
        action = "Unclaimed",
        project = "harness_host::tests::project_restart_stamp"
    )
)]
fn unproven_codex_exit(
    job: &WorkerJob,
    hooks: &Hooks,
    snap: Option<&Snapshot>,
    asked: Option<restart_stamp::Cause>,
) {
    let sid = job.sid.as_str();
    settled(job);
    forget_snapshot(job, hooks);
    task_exited(job, hooks);
    let pid = snap.map_or_else(|| "-".to_string(), |snap| snap.pid.to_string());
    aterm_log::info!(
        "harness @{sid}: the codex exited (pid {pid}{}); not relaunched: no proven self-claim \
         left aterm a copy of its record to decide its end by",
        if asked.is_some() {
            ", ended by a restart"
        } else {
            ""
        }
    );
    (hooks.note)(HostNote::Inform {
        sid: sid.to_string(),
        text: format!(
            "codex-alab ({pid}) exited with no claim proven to this aterm, so how it ended \
             cannot be read: it is not relaunched"
        ),
    });
}

/// The relaunch's attempts after an agent left ([`on_agent_left`]): each after its pause.
fn relaunch_until(
    job: &WorkerJob,
    hooks: &Hooks,
    pause: Duration,
    decide: &dyn Fn() -> OnExit,
    restarted: bool,
    attempt: impl FnMut() -> String,
) {
    relaunch_until_with(
        job,
        hooks,
        pause,
        decide,
        restarted,
        GoneUnread::TriedAgain,
        attempt,
    );
}

/// [`relaunch_until`], `gone_unread` saying whose exit a landed agent's is.
fn relaunch_until_with(
    job: &WorkerJob,
    hooks: &Hooks,
    mut pause: Duration,
    decide: &dyn Fn() -> OnExit,
    restarted: bool,
    gone_unread: GoneUnread,
    mut attempt: impl FnMut() -> String,
) -> bool {
    let sid = job.sid.as_str();
    loop {
        if !job.admitted() {
            settled(job);
            return false;
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
            // ([`request_stop`]), each its own word (`stop` says nothing: the exit's own
            // [`Worker::leave`] set it). The tab says whether its agent is back now.
            if (hooks.still_wanted)(sid, job.group.load(Ordering::SeqCst)) {
                runs_again(job, hooks, false);
                let said = with_kept(job, |k| k.relaunches.running(Instant::now()));
                tell(job, hooks, said, "", restarted, false);
                account_land_for(job, hooks);
                return false;
            }
            match gone_again(job) {
                None => {
                    ring();
                    pause = with_kept(job, |k| k.relaunches.pause());
                    continue;
                }
                Some(Stop::ByCommit) => {}
                Some(Stop::RolledBack(agent)) => take_up_after_rollback(job, hooks, *agent),
                Some(Stop::ForGood) => settled(job),
            }
            return false;
        }
        if job.switches.parked() {
            pause = RESTORED_FIRST_PAUSE;
            continue;
        }
        let decision = decide();
        if !job.admitted() {
            settled(job);
            return false;
        }
        if !job.left.is(Word::Left) {
            continue;
        }
        if let Some((said, marked)) = with_kept(job, |k| {
            let marked = failing_marked(k);
            k.relaunches.decide(decision).map(|said| (said, marked))
        }) {
            settled(job);
            forget_snapshot(job, hooks);
            left_alone(job, hooks, decision, said, marked);
            return false;
        }
        let step = attempt();
        if !job.admitted() {
            settled(job);
            return false;
        }
        if step != relaunch::WAIT_HAND_LIFTS {
            note_upgrade_act();
        }
        let outcome = relaunch::outcome(&step);
        let (said, marked) = with_kept(job, |k| {
            let marked = failing_marked(k);
            (k.relaunches.attempted(&outcome, Instant::now()), marked)
        });
        tell(job, hooks, said, &step, restarted, marked);
        match outcome {
            Outcome::NotYet(_) => pause = with_kept(job, |k| k.relaunches.pause()),
            Outcome::Busy => pause = relaunch::BUSY,
            Outcome::Relaunched => {
                let fresh = relaunches(job.agent)
                    && read_landed(
                        sid,
                        &job.kept,
                        hooks,
                        job.group.load(Ordering::SeqCst),
                        &|| job.left.is(Word::Left),
                    );
                let read = !relaunches(job.agent) || fresh;
                if !restarted
                    && !read
                    && !(hooks.still_wanted)(sid, job.group.load(Ordering::SeqCst))
                {
                    match gone_again(job) {
                        None => {
                            ring();
                            if gone_unread == GoneUnread::HandedBack {
                                return true;
                            }
                            pause = with_kept(job, |k| k.relaunches.pause());
                            continue;
                        }
                        Some(Stop::ByCommit) => return false,
                        Some(Stop::RolledBack(agent)) => {
                            take_up_after_rollback(job, hooks, *agent);
                            return false;
                        }
                        Some(Stop::ForGood) => {}
                    }
                }
                runs_again(job, hooks, fresh);
                account_land_for(job, hooks);
                return false;
            }
            Outcome::Left(why) => {
                left_as_its_end(job, hooks, &why, said);
                return false;
            }
            Outcome::Cannot(_) => {
                settled(job);
                forget_snapshot(job, hooks);
                account_gone(job, hooks);
                task_exited(job, hooks);
                return false;
            }
        }
    }
}

const LANDED_LOOK: Duration = Duration::from_secs(1);

/// The agent a relaunch landed, read.
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

/// An exit left as its launch's own end, `why` ([`Outcome::Left`], `said` the attempt's word).
fn left_as_its_end(job: &WorkerJob, hooks: &Hooks, why: &str, said: Say) {
    let sid = job.sid.as_str();
    settled(job);
    forget_snapshot(job, hooks);
    aterm_log::info!("harness @{sid}: the agent exited; not relaunched: {why}");
    if why != "not-exited" {
        account_gone(job, hooks);
        task_exited(job, hooks);
    }
    let asks = with_kept(job, |k| k.relaunches.asks().is_some());
    if left_ends_relaunch_line(said) && !asks {
        (hooks.note)(HostNote::Relaunch {
            sid: sid.to_string(),
            said: RelaunchSay::Ended,
        });
    }
}

/// Whether the exit of the agent `snap` describes is its launch's own end.
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

/// Check whether the agent read back by the host exited again before this worker read it ([`relaunch_until`]).
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

/// Whether an exit the relaunch left ([`Outcome::Left`]) ends the lines the failing relaunch raised.
fn left_ends_relaunch_line(said: Say) -> bool {
    said != Say::Cannot
}

/// The relaunch this session's back-off owed is no longer owed ([`Kept::owed`]).
fn settled(job: &WorkerJob) {
    with_kept(job, settle);
}

/// [`settled`], under the session's lock.
fn settle(k: &mut Kept) {
    k.owed = None;
    k.commit = Committed::No;
}

/// Revoke only debt that names an agent the policy denies.
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

/// The agent runs in the tab again — the relaunch landed, or the agent is back.
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

/// A worker the commit stopped decides after the rollback ([`Committed::TakenUp`]).
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

/// An exit the host leaves alone ([`relaunch::on_exit`] said `decision`).
fn left_alone(job: &WorkerJob, hooks: &Hooks, decision: OnExit, said: Say, marked: bool) {
    let sid = job.sid.as_str();
    let why = match decision {
        OnExit::PersonAsked => "a person typed into the session just before: the exit is theirs",
        OnExit::Held => {
            "the session is held (a halt, a lease or a driver's turn): the exit is theirs"
        }
        OnExit::Limited => "[harness] relaunch = false",
        OnExit::Relaunch | OnExit::Asked | OnExit::Restarted => "",
    };
    aterm_log::info!(
        "harness @{sid}: {} exited; not relaunched: {why}",
        job.agent.name()
    );
    tell(job, hooks, said, why, false, marked);
    account_gone(job, hooks);
    task_exited(job, hooks);
}

/// Say a relaunch's word (`why`: the step, or the limit, that said it).
fn tell(job: &WorkerJob, hooks: &Hooks, said: Say, why: &str, restarted: bool, marked: bool) {
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
    let unmark = || {
        if marked {
            (hooks.badge)(sid, None);
        }
    };
    match said {
        Say::Nothing => {}
        Say::Clear => {
            if !with_kept(job, |k| lost_marked(k)) {
                (hooks.badge)(sid, None);
            }
            told(RelaunchSay::Ended);
        }
        Say::Cannot if why.starts_with("told:") => {
            aterm_log::info!(
                "harness @{sid}: aterm ended the agent and cannot bring it back ({why}); its \
                 upgrade's own row asks the owner"
            );
            unmark();
            told(RelaunchSay::Ended);
        }
        Say::Cannot => {
            let asks = resume_line(job);
            let reason = cannot_reason(why);
            let command = asks.as_ref().map(|(command, _)| command.clone());
            let place = resume_place(why);
            aterm_log::warn!(
                "harness @{sid}: {} ({why})",
                cannot_said(restarted, &reason, command.as_deref(), place)
            );
            let mark = asks.as_ref().and_then(|(command, _)| {
                mark_text(cannot_head(restarted), &reason, "run", command, place)
            });
            let _ = mark_with(sid, mark.as_deref(), asks, job, hooks);
            let ended = with_kept(job, |k| {
                k.snapshot
                    .as_ref()
                    .or_else(|| k.owed.as_ref().map(|owed| &owed.snap))
                    .and_then(|snap| Some((snap.session.clone()?, snap.pid)))
            });
            let lost = ended.is_some();
            let before = ended.and_then(|(session, pid)| {
                lose(
                    sid,
                    job,
                    Lost {
                        session,
                        pid,
                        marked: mark.is_some(),
                        reason: reason.clone(),
                        restarted,
                        command: command.clone(),
                        other: upgrade_drive::Tasked::default(),
                        faulted: false,
                    },
                )
            });
            if mark.is_none() && (marked || before.is_some_and(|before| before.marked)) {
                (hooks.badge)(sid, None);
            }
            told(RelaunchSay::Cannot {
                reason,
                restarted,
                command,
                new_tab: tab_program(why),
                marked: mark.is_some(),
                lost,
                then: cannot_then(why),
            });
        }
        Say::Failing => {
            aterm_log::warn!(
                "harness @{sid}: the agent exited and its relaunch keeps failing (still trying) \
                 ({why})"
            );
            let asks = resume_line(job).filter(|_| {
                failing_names_command(restarted, why) && !with_kept(job, |k| lost_marked(k))
            });
            let mark = asks
                .as_ref()
                .and_then(|(command, _)| failing_text(why, command));
            let command = mark_with(sid, mark.as_deref(), asks, job, hooks);
            told(RelaunchSay::Failing {
                blocked_by: blocked_cause(why),
                restarted,
                command,
            });
        }
        Say::Limited => {
            aterm_log::warn!("harness @{sid}: the agent exited and is not relaunched: {why}");
            unmark();
            told(RelaunchSay::Limited {
                why: why.to_string(),
            });
        }
    }
}

/// The tab's mark, set (`mark`, the whole ask) for the told command and its conversation (`asks`).
fn mark_with(
    sid: &str,
    mark: Option<&str>,
    asks: Option<(String, String)>,
    job: &WorkerJob,
    hooks: &Hooks,
) -> Option<String> {
    let (command, conversation) = asks?;
    let Some(mark) = mark else {
        aterm_log::warn!(
            "harness @{sid}: the command that starts the agent by hand is too long for the tab's \
             mark whole: {command}"
        );
        return None;
    };
    (hooks.badge)(sid, Some(mark));
    with_kept(job, |k| k.relaunches.ask(&conversation));
    Some(command)
}

/// Whether the tab's mark is the relaunch's that keeps failing, as `k` holds it.
fn failing_marked(k: &Kept) -> bool {
    k.relaunches.failing() && k.relaunches.asks().is_some()
}

/// Whether a relaunch that keeps failing after `step` marks the tab with the command that starts it by hand.
fn failing_names_command(restarted: bool, step: &str) -> bool {
    !restarted && blocked_cause(step) != Some("other-tab")
}

/// The cause a person clears, of a relaunch that keeps failing after `step`.
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
        "not-a-shell-job" => {
            "it ran as its tab's own program, with no shell to come back in".to_string()
        }
        "one-shot" => "it ran one prompt and ended".to_string(),
        "self" => "it is aterm's own agent".to_string(),
        "home" => "it runs under a Claude home its tab's shell does not have".to_string(),
        other if other.starts_with("argv:") => {
            "it was started with options aterm cannot repeat".to_string()
        }
        other if other.starts_with("state") => "aterm's state folder cannot be used".to_string(),
        "stale-exit" => "no try could type at its prompt in time".to_string(),
        "no-resume" => "the new agent never picked the conversation back up".to_string(),
        "resume-refused" => "its relaunch's resume was refused".to_string(),
        "relaunch-unwitnessed" => "how its relaunch ended cannot be read".to_string(),
        "no-managed-muse" => "no managed `muse` is installed to start it".to_string(),
        "no-managed-agy" => "no managed `agy` is installed to start it".to_string(),
        "started-fresh" => {
            "the relaunched agy started a fresh conversation, not this one".to_string()
        }
        "no-witness" => "aterm could not tell which session it ran".to_string(),
        "no-turn" => "its session took no turn, so there is nothing to resume".to_string(),
        "history-unsafe" => {
            "its own resume would bring the session back without some of its turns".to_string()
        }
        _ => "aterm refused it".to_string(),
    }
}

/// What a person does where resuming the agent is no remedy, by the refusal `step` ([`cannot_reason`] reads it).
pub(crate) fn cannot_then(step: &str) -> Option<&'static str> {
    let step = step
        .strip_prefix("refused:")
        .or_else(|| step.strip_prefix("busy:"))
        .unwrap_or(step);
    match step {
        "history-unsafe" => Some(
            "Look at its session files before resuming: a resume now loads a copy missing turns",
        ),
        "no-turn" => Some("Start the agent again in that tab: its session took no turn to resume"),
        _ => None,
    }
}

/// Where a person resumes an agent that cannot be brought back (`step`, the refusal [`cannot_reason`] reads).
fn resume_place(step: &str) -> &'static str {
    if tab_program(step) {
        "in a new tab"
    } else {
        "in this tab"
    }
}

/// Whether the refusal `step` says the agent ran as its tab's own program (`not-a-shell-job`: `aterm -e claude`, `exec codex`).
fn tab_program(step: &str) -> bool {
    let step = step
        .strip_prefix("refused:")
        .or_else(|| step.strip_prefix("busy:"))
        .unwrap_or(step);
    step == "not-a-shell-job"
}

/// What an agent that cannot be brought back is said to be, by whom it ended.
const fn cannot_head(restarted: bool) -> &'static str {
    if restarted {
        "aterm ended the agent and cannot bring it back"
    } else {
        "the agent exited and aterm cannot bring it back"
    }
}

/// The whole word of an agent that cannot be brought back, for the log.
fn cannot_said(restarted: bool, reason: &str, command: Option<&str>, place: &str) -> String {
    let head = cannot_head(restarted);
    match command {
        Some(command) => format!("{head} ({reason}); run {command} {place}"),
        None => format!("{head} ({reason})"),
    }
}

/// The tab's mark for a relaunch that keeps failing.
fn failing_text(step: &str, command: &str) -> Option<String> {
    mark_text(
        "the agent exited; its relaunch keeps failing",
        failing_reason(step),
        "aterm still tries; or run",
        command,
        "in this tab",
    )
}

/// Why a relaunch keeps failing, in a person's words.
fn failing_reason(step: &str) -> &'static str {
    match blocked_cause(step) {
        Some("held") => "its tab is held",
        Some("shell-prompt") => "a job keeps its shell from its prompt",
        Some(_) => "",
        None if matches!(step, "failed:no-resume" | "wait:resume") => {
            "the new agent never picked the conversation back up"
        }
        None => "",
    }
}

/// A tab's mark that asks a person to run `command` `place` (`ask`: the words in front of it; `place` where, [`resume_place`]).
fn mark_text(head: &str, reason: &str, ask: &str, command: &str, place: &str) -> Option<String> {
    let mode = resume::mode_of(command);
    let bare = resume::bare_of(command);
    let with = |command: &str| {
        if reason.is_empty() {
            format!("{head}; {ask} {command} {place}")
        } else {
            format!("{head} ({reason}); {ask} {command} {place}")
        }
    };
    let without = |command: &str| format!("{head}; {ask} {command} {place}");
    std::iter::once(with(command))
        .chain(mode.as_deref().map(with))
        .chain(mode.as_deref().map(without))
        .chain(bare.as_deref().map(with))
        .chain(bare.as_deref().map(without))
        .chain(
            bare.as_deref()
                .map(|bare| format!("agent not back: run {bare} {place}")),
        )
        .find(|text| text.len() <= 190)
}

/// The command that resumes the tab's agent by hand, from the snapshot the host kept of it.
fn resume_line(job: &WorkerJob) -> Option<(String, String)> {
    let snap = with_kept(job, |k| {
        k.snapshot
            .clone()
            .or_else(|| k.owed.as_ref().map(|owed| owed.snap.clone()))
    })?;
    let conversation = snap.session.clone();
    Some((resume_told(&snap)?, conversation?))
}

/// The line a person is told to run to resume the agent `snap` describes on its own conversation.
fn resume_told(snap: &Snapshot) -> Option<String> {
    match &snap.run {
        relaunch::AgentRun::Claude => {
            relaunch::told_command(&aterm_primer::home_dir()?, harness_state().as_deref(), snap)
        }
        relaunch::AgentRun::Codex(_) => None,
        relaunch::AgentRun::Muse(run) => {
            aterm_agent::harness::upgrade_muse::told_line(snap.session.as_deref()?, &run.env)
        }
        relaunch::AgentRun::Agy(run) => aterm_agent::harness::upgrade_agy::told_line(
            snap.session.as_deref()?,
            &run.env,
            &snap.argv,
        ),
    }
}

/// The conversations a word of [`Kept::lost`] stands for, each with the tabs whose word it is.
type LostTabs = HashMap<String, std::collections::HashSet<String>>;
static LOST_SESSIONS: Mutex<Option<LostTabs>> = Mutex::new(None);

/// Stand `lost` for session `sid`'s tab ([`Kept::lost`]), in place of any earlier word of the tab, which it answers.
fn lose(sid: &str, job: &WorkerJob, lost: Lost) -> Option<Lost> {
    let session = lost.session.clone();
    let earlier = with_kept(job, |k| k.lost.as_ref().map(|b| b.session.clone()));
    {
        let mut words = LOST_SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
        let words = words.get_or_insert_with(HashMap::new);
        words
            .entry(session.clone())
            .or_default()
            .insert(sid.to_string());
        if let Some(earlier) = earlier.as_ref().filter(|earlier| **earlier != session) {
            forget_lost_in(words, earlier, sid);
        }
    }
    aterm_log::info!(
        "harness @{sid}: the word that its agent cannot be brought back stands until \
         conversation {session} is held again or its tab closes"
    );
    let before = with_kept(job, |k| k.lost.replace(lost));
    if let Some(stale) = before
        .as_ref()
        .map(|b| &b.session)
        .filter(|b| **b != session && Some(*b) != earlier.as_ref())
    {
        forget_lost(stale, sid);
    }
    before
}

/// Whether a word stands for conversation `session` ([`LOST_SESSIONS`]).
fn lost_session(session: &str) -> bool {
    LOST_SESSIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|lost| lost.contains_key(session))
}

/// The words that an agent cannot be brought back, ended by what they are about.
fn end_lost_words(shared: &Shared, hooks: &Hooks) {
    let standing: Vec<(String, Arc<Mutex<Kept>>)> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(sid, kept)| (sid.clone(), Arc::clone(kept)))
        .collect();
    for (sid, kept) in standing {
        let _ = tab_caught(shared, &sid, "the end of its lost word", hooks, || {
            end_lost_word(&sid, &kept, hooks);
        });
    }
}

/// [`end_lost_words`] for one tab `sid`.
fn end_lost_word(sid: &str, kept: &Arc<Mutex<Kept>>, hooks: &Hooks) {
    let Some((session, pid)) = with_kept_of(kept, |k| {
        k.lost.as_ref().map(|l| (l.session.clone(), l.pid))
    }) else {
        return;
    };
    if held_again(&session, pid) {
        let Some(lost) = with_kept_of(kept, |k| {
            let _ = k.relaunches.conversation_runs(&session);
            k.lost.take()
        }) else {
            return;
        };
        forget_lost(&lost.session, sid);
        aterm_log::info!(
            "harness @{sid}: conversation {session} is held again; the word that its \
                 agent could not be brought back ends"
        );
        if lost.marked {
            (hooks.badge)(sid, None);
        }
        (hooks.note)(HostNote::Relaunch {
            sid: sid.to_string(),
            said: RelaunchSay::Back,
        });
        return;
    }
    step_down(sid, kept, hooks);
}

/// Whether a live agent other than `ended` holds `session` now, in any tab.
fn held_again(session: &str, ended: u32) -> bool {
    PUBLISHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|map| {
            map.values()
                .any(|snap| snap.pid != ended && snap.session.as_deref() == Some(session))
        })
}

/// The tab `sid`'s standing mark steps down to the digest where its agent now holds another conversation someone prompted ([`end_lost_words`]).
fn step_down(sid: &str, kept: &Mutex<Kept>, hooks: &Hooks) {
    let Some((snap, mut other)) = with_kept_of(kept, |k| {
        let lost = k.lost.as_mut().filter(|l| l.marked && !l.faulted)?;
        let snap = k
            .snapshot
            .clone()
            .filter(|s| s.pid != lost.pid && s.session.is_some())
            .filter(|s| s.session.as_deref() != Some(lost.session.as_str()))
            .filter(|s| reads_transcript(s.agent().program()))?;
        Some((snap, std::mem::take(&mut lost.other)))
    }) else {
        return;
    };
    let tasked = host_caught(sid, "its mark's step down", || {
        (hooks.acts.tasked)(sid, &snap, &mut other)
    });
    let said = with_kept_of(kept, |k| {
        let lost = k.lost.as_mut()?;
        lost.other = other;
        lost.faulted |= tasked.is_none();
        if tasked != Some(Some(true)) {
            return None;
        }
        lost.marked = false;
        let said = RelaunchSay::SteppedDown {
            reason: lost.reason.clone(),
            restarted: lost.restarted,
            command: lost.command.clone(),
        };
        let session = lost.session.clone();
        let _ = k.relaunches.conversation_runs(&session);
        Some(said)
    });
    if let Some(said) = said {
        aterm_log::info!(
            "harness @{sid}: a person works in the tab on another conversation; the mark that \
             its agent could not be brought back steps down to the digest"
        );
        (hooks.badge)(sid, None);
        (hooks.note)(HostNote::Relaunch {
            sid: sid.to_string(),
            said,
        });
    }
}

/// Tab `sid`'s word no longer stands for `session` ([`LOST_SESSIONS`]).
fn forget_lost(session: &str, sid: &str) {
    if let Some(lost) = LOST_SESSIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
    {
        forget_lost_in(lost, session, sid);
    }
}

/// [`forget_lost`] in the held list `lost`.
fn forget_lost_in(lost: &mut LostTabs, session: &str, sid: &str) {
    if let Some(tabs) = lost.get_mut(session) {
        tabs.remove(sid);
        if tabs.is_empty() {
            lost.remove(session);
        }
    }
}

struct Worker {
    agent: Program,
    revoked: Arc<AtomicBool>,
    join: Option<JoinHandle<WorkerExit>>,
    done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handover: Arc<AtomicBool>,
    interrupt: Arc<Mutex<Option<Interrupter>>>,
    cfg_gen: u64,
    epoch: u64,
    park: Arc<AtomicBool>,
    look_at: Arc<Mutex<Option<Instant>>>,
    left: Arc<ExitWord>,
    acting: Arc<AtomicBool>,
    stepped: Arc<AtomicU64>,
    activations: u64,
    note: Note,
    group: Arc<AtomicI32>,
    points: Arc<Points>,
    kept: Arc<Mutex<Kept>>,
    left_at_commit: Arc<AtomicBool>,
    unfollowed: bool,
    passive_read: Option<(PassivePrimary, Instant)>,
}

impl Worker {
    fn stop(&self) {
        request_stop(&self.stop, &self.left, &self.interrupt);
    }

    /// Stop it because its agent left a tab that lives on.
    fn leave(&self) {
        hand_exit(
            &self.kept,
            &self.left_at_commit,
            &self.left,
            &self.stop,
            &self.interrupt,
        );
    }

    /// Its agent is back in the tab (relaunched, or started by someone) while the worker was handed its exit.
    fn back(&self, agent: Program) -> bool {
        let back = with_kept_of(&self.kept, |k| {
            let back = self.left.back();
            if back {
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

    /// Stop it for a worker that takes the session straight after.
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

/// [`Worker::leave`]'s act, over the handles it reaches the worker through.
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

/// The host thread's workers.
#[derive(Default)]
struct Workers(HashMap<String, Worker>);

impl Drop for Workers {
    fn drop(&mut self) {
        for w in self.0.values() {
            w.stop();
        }
    }
}

/// How long [`HostHandle::defer_adopted_claims`] holds a session off: more than two claim
/// renewals.
pub(crate) const ADOPTED_CLAIM_GRACE: Duration =
    Duration::from_secs(2 * aterm_agent::supervise::CLAIM_RENEW.as_secs() + 5);

/// The sessions [`HostHandle::defer_adopted_claims`] named, and when each may be claimed ([`ADOPTED_CLAIM_GRACE`]).
#[derive(Default)]
struct ClaimGrace {
    pending: Vec<(String, Duration)>,
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

    /// When `sid`'s grace runs out, while it holds at `now`.
    fn runs_until(&self, sid: &str, now: Instant) -> Option<Instant> {
        self.until.get(sid).copied().filter(|at| *at > now)
    }
}

#[derive(Default)]
struct State {
    cfg: SupervisorConfig,
    cfg_gen: u64,
    suspended: bool,
    admission_gen: u64,
    resumes: u64,
    shutting_down: bool,
    activation: ActivationStart,
    claim_grace: ClaimGrace,
    live: Vec<String>,
    stops: Vec<StopHandle>,
    off: std::collections::HashSet<String>,
    unsupervised: bool,
}

/// A worker's stop flag, exit word, interrupter slot.
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
    /// Lets its worker go as [`Worker::leave`] does.
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

/// The host's activation counts ([`HostHandle::note_activation`], [`HostHandle::note_agent_activation`]).
#[derive(Debug, Default)]
pub(crate) struct Activations {
    all: AtomicU64,
    own: [AtomicU64; AGENT_SLOTS],
}

const AGENT_SLOTS: usize = Program::ALL.len();

/// The slot of `agent` in [`Activations::own`].
const fn agent_slot(agent: Program) -> usize {
    agent as usize
}

/// A pass's reading of [`Activations`]: what each worker it starts is owed.
#[derive(Clone, Copy, Debug, Default)]
struct ActivationCounts {
    all: u64,
    own: [u64; AGENT_SLOTS],
}

impl ActivationCounts {
    /// What a worker of `agent` is owed: every worker's, and its agent's own.
    fn of(&self, agent: Program) -> u64 {
        self.all.wrapping_add(self.own[agent_slot(agent)])
    }
}

impl Activations {
    /// An activation every worker looks at.
    fn note(&self) {
        self.all.fetch_add(1, Ordering::SeqCst);
    }

    /// An activation only `agent`'s workers look at.
    fn note_of(&self, agent: Program) {
        self.own[agent_slot(agent)].fetch_add(1, Ordering::SeqCst);
    }

    /// What a worker of `agent` is owed now ([`ActivationCounts::of`]).
    fn of(&self, agent: Program) -> u64 {
        self.read().of(agent)
    }

    /// Every activation, of every agent: what the owner's view looks again on.
    fn any(&self) -> u64 {
        let counts = self.read();
        counts
            .own
            .iter()
            .fold(counts.all, |sum, own| sum.wrapping_add(*own))
    }

    /// One reading of every count.
    fn read(&self) -> ActivationCounts {
        let mut own = [0; AGENT_SLOTS];
        for (slot, count) in own.iter_mut().zip(&self.own) {
            *slot = count.load(Ordering::SeqCst);
        }
        ActivationCounts {
            all: self.all.load(Ordering::SeqCst),
            own,
        }
    }
}

struct Shared {
    state: Mutex<State>,
    headless: bool,
    desk: crate::desk::Desk,
    faults: Mutex<HashMap<String, FaultHistory>>,
    host_faults: Mutex<HashMap<String, HashMap<&'static str, HostFault>>>,
    kept: Mutex<HashMap<String, Arc<Mutex<Kept>>>>,
    switches: Arc<Switches>,
    activations: Arc<Activations>,
    wake: std::sync::OnceLock<WakeTrigger>,
    host_done: AtomicBool,
    net: Arc<NetProbe>,
    login_latches: Arc<LoginLatches>,
    restored: Mutex<RestoredQueue>,
    restored_wake: Condvar,
    inbox_lane: u64,
    #[cfg(test)]
    restart_panics: AtomicBool,
    #[cfg(test)]
    passes_panic: AtomicBool,
}

/// Period of the host's disk-space watch timer.
pub(crate) const DISK_TICK: Duration =
    Duration::from_millis(aterm_agent::supervise::occupancy::reading::LOOK_EVERY_MS as u64);
const DISK_FIRST_LOOK: Duration = Duration::from_secs(120);

const WORST_FILL_GIB_PER_HOUR: u64 = 100;

const fn lost_mib(gap: Duration) -> u64 {
    gap.as_secs() * WORST_FILL_GIB_PER_HOUR * 1024 / 3600
}

const _: () = {
    use aterm_agent::harness::disk::{
        DEFAULT_AUTO_FREE_GIB, EMERGENCY_FREE_GIB, PRESSURE_MARGIN_GIB,
    };
    let below_floor_mib = (DEFAULT_AUTO_FREE_GIB - EMERGENCY_FREE_GIB) * 1024;
    let margin_mib = PRESSURE_MARGIN_GIB * 1024;
    assert!(
        lost_mib(DISK_TICK) < below_floor_mib,
        "a disk tick lets the floor fall to the emergency line"
    );
    assert!(
        lost_mib(DISK_TICK) < margin_mib,
        "a disk tick loses more than a pass's margin above the floor"
    );
    assert!(
        lost_mib(DISK_FIRST_LOOK) < below_floor_mib,
        "the first disk look comes after the floor falls to the emergency line"
    );
    assert!(
        lost_mib(DISK_FIRST_LOOK) < margin_mib,
        "the first disk look loses more than a pass's margin above the floor"
    );
    // A look takes over what an ended host left of its sessions as it begins, before its pass
    // (`disk_ask::before_pass`), so the pass's own time, which can outlast a tick, is not
    // counted here: a successor's first look begins `DISK_FIRST_LOOK` after it starts, and a
    // take-over a contended lease lock skipped there is made at the step after that pass.
    assert!(
        (DISK_FIRST_LOOK.as_millis() + DISK_TICK.as_millis())
            <= aterm_agent::harness::disk::ask::ADOPT_MS as u128,
        "a seamless update's successor begins a look, and takes its sessions' pick over, before \
         other hosts hand it on (disk-free@v1)"
    );
};
/// The startup deadline ([`look_at_startups`]).
pub(crate) const STARTUP_DEADLINE: Duration = Duration::from_secs(120);
const STARTUP_UNREAD_LOOK: Duration = Duration::from_secs(2);
/// How the startup deadline's attention begins ([`look_at_startups`]).
pub(crate) use aterm_agent::supervise::STARTUP_HEAD;
/// The held deadline ([`look_at_startups`]).
pub(crate) const HELD_DEADLINE: Duration = Duration::from_secs(120);
const HELD_DEADLINE_SEAM: &str = "ATERM_TASK_HELD_MS";
const HELD_RELOOK: Duration = Duration::from_secs(10);
/// How the held deadline's attention begins.
pub(crate) use aterm_agent::supervise::HELD_HEAD;

/// [`HELD_DEADLINE`], or the development seam's ([`HELD_DEADLINE_SEAM`]).
fn held_deadline() -> Duration {
    aterm_types::dev_seam!(HELD_DEADLINE_SEAM)
        .and_then(|v| v.to_str()?.parse::<u64>().ok())
        .map_or(HELD_DEADLINE, Duration::from_millis)
}

impl State {
    /// The news a host thread waiting out its restart's back-off acts on at once.
    const fn news(&self) -> (u64, u64, u64) {
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
    /// A host under `cfg`.
    pub(crate) fn start(
        cfg: SupervisorConfig,
        headless: bool,
        suspended: bool,
        hooks: Hooks,
    ) -> Self {
        let switches = Arc::new(Switches::default());
        switches.set(&cfg, headless);
        crate::disk_board::watching(would_supervise(&effective(&cfg), headless));
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
                host_faults: Mutex::default(),
                kept: Mutex::default(),
                switches,
                activations: Arc::default(),
                wake: std::sync::OnceLock::new(),
                host_done: AtomicBool::new(false),
                net: NetProbe::new(),
                login_latches: Arc::default(),
                restored: Mutex::default(),
                restored_wake: Condvar::new(),
                inbox_lane: crate::message_inbox::current_lane(),
                #[cfg(test)]
                restart_panics: AtomicBool::new(false),
                #[cfg(test)]
                passes_panic: AtomicBool::new(false),
            }),
            thread: Arc::default(),
            hooks,
        };
        handle.ensure_thread();
        handle
    }

    /// Whether the host supervises agent sessions now.
    pub(crate) fn supervising(&self) -> bool {
        let state = self.shared.lock();
        Shared::active(&state, self.shared.headless)
    }

    /// Whether the host is suspended for a seamless update's handoff ([`Self::suspend`]).
    pub(crate) fn suspended(&self) -> bool {
        self.shared.lock().suspended
    }

    /// The live agent admission snapshot for owner-facing projections.
    pub(crate) fn admission(&self) -> Admission {
        self.shared.switches.admission()
    }

    /// The agent's live policy gate, independently of a claim that its old worker may still be returning after a stop.
    pub(crate) fn admits(&self, agent: Program) -> bool {
        self.shared.switches.on_for(agent)
    }

    /// Whether the host relaunches the agents it supervises on its own (an exit, its memory restart).
    pub(crate) fn relaunches(&self) -> bool {
        self.supervising() && self.shared.switches.relaunch()
    }

    /// Whether this host types the line that re-keys an agent's degraded shell (`integration_notice`).
    pub(crate) fn rekeys_agents(&self) -> bool {
        self.supervising() && (self.shared.switches.upgrade() || self.shared.switches.relaunch())
    }

    /// The sessions whose agent this host would relaunch on its conversation if it ended now.
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

    /// Let every worker the host last published go ([`StopHandle::leave`]), each with no lock of the host's held.
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
        if (!Shared::active(&state, self.shared.headless) && state.activation.next().is_none())
            || state.shutting_down
        {
            return;
        }
        drop(state);
        let mut slot = self.thread.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = slot.as_ref() {
            // The host thread ends only at shutdown (checked above): a panic of its loop
            // restarts it in place, and a policy switched off leaves it parked. One that has
            // finished ended in a panic past that restart: start it again, with no budget.
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
                let mut attempt = 0usize;
                let mut host = HostState::from_shared(&shared);
                'host: loop {
                    let began = Instant::now();
                    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        host_loop(&shared, &mut host, &hooks);
                    }));
                    let Err(payload) = run else { break };
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

    /// The agents whose tabs the next launch reopened after aterm ended (the crash journal, or a restart's quit layout).
    pub(crate) fn relaunch_restored(&self, restored: Vec<RestoredAgent>) {
        let _ = queue_restored(&self.shared, &self.hooks, restored);
    }

    /// The tab a held tab's agent ran in, where a cold restore minted its reopened tab another sid.
    pub(crate) fn note_earlier_tab(&self, sid: &str, was: &str) {
        if was != sid {
            (self.hooks.acts.earlier_tab)(sid, was);
        }
    }

    /// The word standing that tab `sid`'s agent cannot be brought back ([`Kept::lost`]).
    pub(crate) fn lost_word(&self, sid: &str) -> Option<crate::restore::LostRestore> {
        let kept = self
            .shared
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(sid)
            .map(Arc::clone)?;
        with_kept_of(&kept, |k| {
            k.lost.as_ref().map(|lost| crate::restore::LostRestore {
                session: lost.session.clone(),
                pid: lost.pid,
                marked: lost.marked,
                reason: lost.reason.clone(),
                restarted: lost.restarted,
                command: lost.command.clone(),
            })
        })
    }

    /// The words a seamless update's outgoing host kept that a tab's agent cannot be brought back, stood again here.
    #[cfg(any(unix, test))]
    pub(crate) fn adopt_lost_words(&self, words: Vec<(String, crate::restore::LostRestore)>) {
        let mut stood = false;
        for (sid, word) in words {
            if word.session.is_empty() || word.pid == 0 {
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
            let session = word.session.clone();
            let adopted = with_kept_of(&kept, |k| {
                if k.lost.is_some() {
                    return false;
                }
                k.lost = Some(Lost {
                    session: word.session,
                    pid: word.pid,
                    marked: word.marked,
                    reason: word.reason,
                    restarted: word.restarted,
                    command: word.command,
                    other: upgrade_drive::Tasked::default(),
                    faulted: false,
                });
                true
            });
            if !adopted {
                continue;
            }
            LOST_SESSIONS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert_with(HashMap::new)
                .entry(session.clone())
                .or_default()
                .insert(sid.clone());
            aterm_log::info!(
                "harness @{sid}: the previous aterm's word that its agent cannot be brought back \
                 stands until conversation {session} is held again or its tab closes"
            );
            stood = true;
        }
        if stood {
            ring();
        }
    }

    /// The automatic update waits for the restored agents.
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

    /// A handoff parked the readers.
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

    /// Every session whose relaunch on exit is owed now ([`Kept::owed`]), with the snapshot it is owed on.
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

    /// A Commit that failed after a park's stop.
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
                let open = (self.hooks.acts.open)(&sid);
                let place = place(&sid);
                let owed = with_kept_of(&kept, |k| match std::mem::take(&mut k.commit) {
                    Committed::Stranded => k.owed.take().filter(|_| open),
                    Committed::Stopped => {
                        k.commit = Committed::TakenUp(place.clone());
                        None
                    }
                    other => {
                        k.commit = other;
                        None
                    }
                })?;
                aterm_log::info!(
                    "harness @{sid}: aterm's update did not complete; relaunching the agent \
                     that exited {place}"
                );
                Some(RestoredAgent {
                    sid,
                    snap: owed.snap,
                    place,
                    cold: false,
                    asked: owed.asked,
                    was: None,
                })
            })
            .collect();
        stranded.sort_by(|a, b| a.sid.cmp(&b.sid));
        self.relaunch_restored(stranded);
    }

    /// The handoff failed and this process keeps its sessions.
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

    /// The restored agent the queue still owes tab `sid`, for the handoff layout's leaf (`App::capture_handoff_layout`).
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

    /// The snapshot [`Self::pending_restored_for`] carries for tab `sid` (the tests' view of it, which most of them read).
    #[cfg(test)]
    pub(crate) fn pending_restored_snapshot_for(&self, sid: &str) -> Option<Snapshot> {
        self.pending_restored_for(sid).map(|owed| owed.snap)
    }

    /// Every tab the restored queue still owes an agent, in the layout's order (the tests' view of [`Self::pending_restored_for`]).
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

    /// Whether the agents a cold restore hands this host are relaunched ([`Self::relaunch_restored`]).
    pub(crate) fn relaunches_restored(&self) -> bool {
        relaunches_restored(&self.shared)
    }

    /// Whether a restored agent of `agent` is relaunched.
    pub(crate) fn relaunches_restored_of(&self, agent: upgrade::Agent) -> bool {
        relaunches_restored(&self.shared)
            && relaunches(agent.program())
            && self.admits(agent.program())
    }

    /// Who answers a wall of `kind` on `agent`'s session this host supervises (the presence row
    /// and tab chip): nobody where the host does not admit `agent`, where the loop types nothing
    /// into it ([`Program::supervisable`], the predicate its turn-end loop's every act checks
    /// first: Codex, Grok Build and Kimi Code), or where the wall is a lost login the loop does
    /// not carry for it ([`aterm_agent::supervise::policy::login::carried`]), so the need stays
    /// the person's rather than a `waiting:` nothing ends; else what the policy's outlook says.
    /// A Codex's own resumes are codex-alab's, answered beside its proven self-claim
    /// (`crate::self_claim::wall_answer`), never here: its worker watches beside codex-alab
    /// (`watches_only`) and takes no claim of this host's.
    pub(crate) fn wall_answer(
        &self,
        agent: Program,
        kind: aterm_phase::WallKind,
        has_reset: bool,
    ) -> aterm_messages::presence::WallAnswer {
        use aterm_agent::supervise::policy::outlook::{Answer, answer};
        use aterm_messages::presence::WallAnswer;
        if !self.admits(agent)
            || !agent.supervisable()
            || (kind == aterm_phase::WallKind::Auth
                && !aterm_agent::supervise::policy::login::carried(agent))
        {
            return WallAnswer::Nobody;
        }
        let cfg = self.shared.lock().cfg.clone();
        match answer(kind, has_reset, &cfg, cfg.enabled) {
            Answer::Nobody => WallAnswer::Nobody,
            Answer::Resumes => WallAnswer::Harness,
            Answer::ResumesAsks(action) => WallAnswer::HarnessAsks(action),
        }
    }

    /// A reloaded `[harness]` policy.
    pub(crate) fn set_config(&self, cfg: SupervisorConfig) {
        let switches = &self.shared.switches;
        let was = (switches.accounts(), switches.notes());
        let relaunch = switches.relaunch();
        let agents = switches.agents.load(Ordering::SeqCst);
        switches.set(&cfg, self.shared.headless);
        self.accounts_switched(was);
        if relaunch != switches.relaunch() {
            switches.accounts_news.fetch_add(1, Ordering::SeqCst);
        }
        let cfg = effective(&cfg);
        let mut state = self.shared.lock();
        let admission_changed = agents != self.shared.switches.agents.load(Ordering::SeqCst);
        if state.cfg == cfg && !admission_changed {
            return;
        }
        state.unsupervised |= !would_supervise(&cfg, self.shared.headless);
        for worker in &state.stops {
            if !self.shared.switches.on_for(worker.agent) {
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

    /// Stop every worker and supervise nothing until [`Self::resume`].
    #[cfg(any(unix, test))]
    pub(crate) fn suspend(&self) {
        let mut state = self.shared.lock();
        state.suspended = true;
        for worker in &state.stops {
            with_kept_of(&worker.kept, |k| {
                mark_commit_stop(k, &worker.left, &worker.left_at_commit);
            });
            request_stop(&worker.stop, &worker.left, &worker.interrupt);
        }
        drop(state);
        ring();
    }

    /// Stop at the Commit write while session claim guards are held. Acquire every
    /// host/kept lock without waiting before changing any state, and retain them
    /// through `write`. Callbacks and wakes wait until a failed Commit has released
    /// those guards and calls [`Self::suspend`].
    #[cfg(any(unix, test))]
    pub(crate) fn with_suspended_at_commit<R>(&self, write: impl FnOnce() -> R) -> Option<R> {
        fn try_guard<T>(lock: &Mutex<T>) -> Option<std::sync::MutexGuard<'_, T>> {
            match lock.try_lock() {
                Ok(guard) => Some(guard),
                Err(std::sync::TryLockError::Poisoned(poison)) => Some(poison.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => None,
            }
        }

        let mut state = try_guard(&self.shared.state)?;
        let State {
            stops, suspended, ..
        } = &mut *state;
        let mut kept = stops
            .iter()
            .map(|worker| try_guard(&worker.kept))
            .collect::<Option<Vec<_>>>()?;
        *suspended = true;
        for (worker, kept) in stops.iter().zip(&mut kept) {
            mark_commit_stop(kept, &worker.left, &worker.left_at_commit);
            mark_stop(&worker.stop, &worker.left);
        }
        let result = write();
        drop(kept);
        drop(state);
        Some(result)
    }

    /// A newer agent build may be installed (atpkg's activation notice, or Claude's own native updater repointing its link).
    pub(crate) fn note_activation(&self) {
        self.shared.activations.note();
        ring();
    }

    /// A newer build of `agent` alone may be installed (atpkg's marker of that agent's own twin,
    /// as Muse Code and agy have: [`own_marker_agents`]).
    pub(crate) fn note_agent_activation(&self, agent: Program) {
        self.shared.activations.note_of(agent);
        ring();
    }

    /// News of the accounts (the store's wake marker was replaced: `aterm harness accounts use`, a
    /// login found signed in, an account proven or struck, a limit reset's outcome).
    pub(crate) fn note_accounts(&self) {
        self.shared
            .switches
            .accounts_news
            .fetch_add(1, Ordering::SeqCst);
        accounts_look(
            &self.shared.switches,
            &self.hooks,
            &self.shared.login_latches,
            false,
        );
    }

    /// The owner's switches moved.
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

    /// The accounts are looked at again, off the caller's thread.
    fn accounts_again(&self) {
        self.shared
            .switches
            .accounts_said
            .store(0, Ordering::SeqCst);
        if let Some(wake) = self.shared.wake.get() {
            wake.pull();
        }
    }

    /// Supervise again (the successor's Commit activated it, or a Commit failed and this process keeps its sessions).
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

    /// The successor's Commit.
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
        let runs = Arc::clone(&self.hooks.still_wanted);
        let pause = Arc::clone(&self.hooks.pause);
        let clock = Arc::clone(&self.hooks.carry_clock);
        let shared = Arc::clone(&self.shared);
        let spawned = crate::qos::spawn_lane_thread(
            format!("{THREAD_PREFIX}carried"),
            crate::qos::Role::Background,
            move || {
                carry_in_flight_after_handoff(
                    &shared,
                    carry.as_ref(),
                    words.as_ref(),
                    runs.as_ref(),
                    pause.as_ref(),
                    clock.as_ref(),
                    &sessions,
                    grace,
                );
            },
        );
        if let Err(e) = spawned {
            aterm_log::warn!(
                "harness: the restarts the previous aterm left in flight could not be carried \
                 on: {e}"
            );
        }
    }

    /// The agents the outgoing instance supervised, watched from this successor's Commit.
    #[cfg(any(unix, test))]
    pub(crate) fn watch_handed(&self, agents: Vec<(String, Snapshot)>) {
        if agents.is_empty() || !would_supervise(&self.shared.lock().cfg, self.shared.headless) {
            return;
        }
        for (sid, snap) in agents {
            if !self.shared.switches.on_for(snap.agent().program()) {
                continue;
            }
            let watched = match &snap.run {
                relaunch::AgentRun::Codex(_) => {
                    relaunch::self_claimed_line(&snap).is_some()
                        || crate::self_claim::runs_alab_build(snap.pid)
                }
                relaunch::AgentRun::Claude
                | relaunch::AgentRun::Muse(_)
                | relaunch::AgentRun::Agy(_) => true,
            };
            if !watched {
                aterm_log::info!(
                    "harness @{sid}: the vendor codex the previous aterm supervised here is shown, \
                     not watched: aterm supervises no Codex"
                );
                continue;
            }
            if (self.hooks.acts.restarted)(&sid, snap.agent(), Some(snap.pid)) {
                aterm_log::info!(
                    "harness @{sid}: the agent the previous aterm supervised here was ended by its \
                     own restart; its carry takes it on"
                );
                continue;
            }
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
            let mut decided = snap.clone();
            if let Some(run) = decided.codex_mut() {
                run.self_claimed = None;
            }
            let (ours, moved) = with_kept_of(&kept, |k| match &k.snapshot {
                None => {
                    k.snapshot = Some(decided);
                    (true, publish_snapshot_from(&sid, Some(&snap), true))
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

    /// Hold off `sids`, sessions a seamless update handed this instance whose claim the outgoing process could not vouch for (`SessionRecord::claim_grace`).
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

    /// The sessions held off for their claim grace right now ([`Self::defer_adopted_claims`]).
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

    /// Stop every worker and the host thread, bounded.
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
        let activation = {
            let mut state = self.shared.lock();
            state.shutting_down = true;
            std::mem::take(&mut state.activation)
        };
        drop(activation);
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

/// Whether the restored queue relaunches `agent` ([`queue_restored`], [`relaunch_restored_queue`]).
fn restored_allowed(shared: &Shared, agent: &RestoredAgent) -> bool {
    shared.switches.on_for(agent.snap.agent().program())
        && (relaunches_restored(shared)
            || (agent.asked.is_some() && shared.desk.kept() && shared.switches.supervises()))
}

/// A delayed read may apply only while its original queue entry is still admitted.
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

/// An Off session's restored agent is not relaunched ([`OffSessions`]).
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

/// [`HostHandle::relaunch_restored`].
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
    for agent in &restored {
        if let Some(was) = agent.was.as_deref().filter(|w| *w != agent.sid) {
            (hooks.acts.earlier_tab)(&agent.sid, was);
        }
    }
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
                        faults: 0,
                        in_flight_only: false,
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
    let spawned = crate::qos::spawn_lane_thread(
        format!("{THREAD_PREFIX}restored"),
        crate::qos::Role::Background,
        move || {
            // The worker clears `running` only under the queue's lock, as it ends with
            // nothing left to say, so a panic caught here is always one of the queue it
            // still runs, never one of a worker a later restore started, whose attempts
            // this would end as not relaunched while that worker landed them.
            if caught(
                "the restored agents' relaunch",
                "what it still owed is said as not relaunched",
                || relaunch_restored_queue(&queue_shared, &queue_hooks),
            )
            .is_none()
            {
                let missed = restored_queue_panicked(&queue_shared, queue_hooks.badge.as_ref());
                let _ = caught(
                    "the restored agents' row",
                    "the tabs it would name are in the log",
                    || say_restored(&queue_shared, queue_hooks.still_wanted.as_ref(), missed),
                );
            }
        },
    );
    if let Err(e) = spawned {
        aterm_log::warn!("harness: the restored tabs' agents could not be relaunched: {e}");
        let mut queue = shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.attempts.clear();
        queue.running = false;
    }
    Ok(())
}

/// The restored agents' one worker ([`HostHandle::relaunch_restored`]) over the host's queue ([`RestoredQueue`]) while `[harness] relaunch` allows it.
fn relaunch_restored_queue(shared: &Arc<Shared>, hooks: &Hooks) {
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
                say_restored(shared, hooks.still_wanted.as_ref(), missed);
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
        queue
            .attempts
            .retain(|attempt| restored_allowed(shared, &attempt.agent));
        if queue.attempts.is_empty() {
            let missed = std::mem::take(&mut queue.missed);
            if missed.is_empty() {
                queue.running = false;
                return;
            }
            drop(queue);
            say_restored(shared, hooks.still_wanted.as_ref(), missed);
            queue = shared
                .restored
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        }
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
        let taken = RestoredTaken {
            order: attempt.order,
            agent: attempt.agent.clone(),
            revoked: Arc::clone(&attempt.revoked),
            typed: attempt.typed(),
            seen: attempt.agent_seen,
            carried: attempt.carried,
            landed: attempt.landed,
        };
        drop(queue);
        let relaunched = AtomicBool::new(taken.landed);
        if caught(
            &format!("@{}: the relaunch after aterm ended", taken.agent.sid),
            "its attempt is over, said as one that could not be made, and the queue goes on",
            || restored_step(shared, hooks, &taken, &relaunched),
        )
        .is_none()
        {
            restored_step_panicked(shared, hooks, &taken, relaunched.load(Ordering::SeqCst));
        }
        queue = shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// One restored attempt as [`relaunch_restored_queue`] takes it for a step, its lock released.
struct RestoredTaken {
    order: usize,
    agent: RestoredAgent,
    revoked: Arc<AtomicBool>,
    typed: bool,
    seen: bool,
    carried: bool,
    landed: bool,
}

/// One step of one restored attempt ([`relaunch_restored_queue`]).
fn restored_step(shared: &Shared, hooks: &Hooks, taken: &RestoredTaken, relaunched: &AtomicBool) {
    let (act, cold, badge, runs) = (
        hooks.acts.relaunch_restored.as_ref(),
        hooks.acts.account_cold.as_ref(),
        hooks.badge.as_ref(),
        hooks.still_wanted.as_ref(),
    );
    let switches = &shared.switches;
    let RestoredTaken {
        order,
        ref agent,
        ref revoked,
        typed,
        seen,
        carried,
        landed,
    } = *taken;
    let restored = || {
        shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    };
    if landed {
        let look = restored_landing_look(shared, hooks, &agent.sid, seen, &agent.snap, revoked);
        let next_look = (hooks.pause)(LANDED_LOOK);
        let mut queue = restored();
        if queue.closed {
            return;
        }
        let Some(at) = queue.attempts.iter().position(|a| a.order == order) else {
            return;
        };
        let back_off = (hooks.pause)(queue.attempts[at].pause);
        let mut attempt = queue.attempts.remove(at).expect("the attempt was found");
        attempt.acting = false;
        match restored_landing_ends(attempt, look, next_look, back_off) {
            LandingEnd::Keep(attempt) => queue.attempts.push_back(attempt),
            LandingEnd::Over(attempt, last, step) => {
                drop(queue);
                let missed = restored_over(badge, attempt, last, step);
                restored().missed.extend(missed);
            }
        }
        return;
    }
    if restored_off(shared, hooks, agent) {
        {
            let mut queue = restored();
            if queue.closed {
                return;
            }
            if let Some(at) = queue.attempts.iter().position(|a| a.order == order) {
                queue.attempts.remove(at);
            }
        }
        restored_word_cleared(badge, &agent.sid);
        return;
    }
    if restored_overtaken(typed, seen, runs, &agent.sid) {
        aterm_log::info!(
            "harness @{}: an agent ran {} since; the relaunch after aterm ended is over",
            agent.sid,
            agent.place
        );
        let mut queue = restored();
        if queue.closed {
            return;
        }
        if let Some(at) = queue.attempts.iter().position(|a| a.order == order) {
            queue.attempts.remove(at);
        }
        return;
    }
    if let Some(look) = restored_carried(hooks, agent, carried) {
        let mut queue = restored();
        if queue.closed {
            return;
        }
        if let Some(at) = queue.attempts.iter().position(|a| a.order == order) {
            match look {
                Carried::Claimed => {
                    queue.attempts.remove(at);
                    drop(queue);
                    restored_word_cleared(badge, &agent.sid);
                }
                Carried::Hand => {
                    let attempt = &mut queue.attempts[at];
                    attempt.acting = false;
                    attempt.carried = true;
                    attempt.due = Instant::now() + (hooks.pause)(relaunch::BUSY);
                }
            }
        }
        return;
    }
    let login = (agent.unread() && switches.accounts() != accounts::Mode::Off)
        .then(|| cold(&agent.snap))
        .flatten();
    let grace = shared.lock().cfg.human_grace_s;
    if !restored_current(shared, order, agent) {
        let mut queue = restored();
        if !queue.closed {
            queue.attempts.retain(|attempt| attempt.order != order);
        }
        return;
    }
    let step = relaunch_cold(&agent.snap, login, &|snap| match agent.asked {
        Some(cause) => (hooks.acts.relaunch)(
            &agent.sid,
            grace,
            snap,
            &ExitRecord::Unread,
            switches.upgrade(),
            Some(cause),
        ),
        None => act(&agent.sid, snap, switches.upgrade(), agent.was.as_deref()),
    });
    let last = relaunch::outcome(&step);
    relaunched.store(last == Outcome::Relaunched, Ordering::SeqCst);
    if !restored_current(shared, order, agent) {
        let mut queue = restored();
        if !queue.closed {
            queue.attempts.retain(|attempt| attempt.order != order);
        }
        return;
    }
    let landing = (last == Outcome::Relaunched).then(|| {
        let look = restored_landing_look(shared, hooks, &agent.sid, false, &agent.snap, revoked);
        (look, (hooks.pause)(LANDED_LOOK))
    });
    let mut queue = restored();
    if queue.closed {
        return;
    }
    let Some(at) = queue.attempts.iter().position(|a| a.order == order) else {
        return;
    };
    let back_off = (hooks.pause)(queue.attempts[at].pause);
    let mut attempt = queue.attempts.remove(at).expect("the attempt was found");
    attempt.acting = false;
    attempt.tried += 1;
    if let Some((look, next_look)) = landing {
        match restored_landing_ends(attempt, look, next_look, back_off) {
            LandingEnd::Keep(attempt) => queue.attempts.push_back(attempt),
            LandingEnd::Over(attempt, last, step) => {
                drop(queue);
                let missed = restored_over(badge, attempt, last, step);
                restored().missed.extend(missed);
            }
        }
        return;
    }
    let now = Instant::now();
    if step == "wait:resume" {
        attempt.resume_wait_since.get_or_insert(now);
    }
    let waiting = attempt
        .resume_wait_since
        .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
    if restored_retry(&step, &last, attempt.tried, waiting) {
        attempt.due = now + back_off;
        attempt.pause = (attempt.pause * 2).min(RESTORED_MAX_PAUSE);
        queue.attempts.push_back(attempt);
        return;
    }
    drop(queue);
    let missed = restored_over(badge, attempt, last, &step);
    restored().missed.extend(missed);
}

/// A restored attempt whose step panicked is a fault of that attempt, never a relaunch made again from nothing.
fn restored_step_panicked(shared: &Shared, hooks: &Hooks, taken: &RestoredTaken, relaunched: bool) {
    let sid = taken.agent.sid.as_str();
    let in_flight = !relaunched
        && !taken.landed
        && !taken.typed
        && caught(
            &format!("@{sid}: the record of a relaunch that panicked"),
            "it is taken for one with nothing in flight",
            || (hooks.acts.in_flight_here)(sid, &taken.agent.snap),
        ) == Some(true);
    let landing = relaunched || taken.landed || in_flight;
    let stepped_landed = relaunched && !taken.landed;
    let held = !stepped_landed && {
        let looked = "the look at the tab of a relaunch that panicked";
        let none = "it is taken for a tab nothing holds";
        caught(&format!("@{sid}: {looked}"), none, || {
            (hooks.acts.adopted_gone)(sid, &taken.agent.snap)
        }) == Some(Some(false))
            || caught(&format!("@{sid}: {looked}"), none, || {
                (hooks.still_wanted)(sid, 0)
            }) == Some(true)
    };
    let mut queue = shared
        .restored
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if queue.closed {
        return;
    }
    let Some(at) = queue.attempts.iter().position(|a| a.order == taken.order) else {
        return;
    };
    let (last, step) = if landing || taken.typed {
        let attempt = &mut queue.attempts[at];
        attempt.acting = false;
        attempt.faults += 1;
        let now = Instant::now();
        if landing {
            let only = !stepped_landed && (in_flight || attempt.in_flight_only);
            if attempt.faults < RESTORED_TRIES {
                attempt.landed = true;
                attempt.in_flight_only = only;
                attempt.due = now + (hooks.pause)(LANDED_LOOK);
                return;
            }
            if only {
                // Over as a typed line is: said, unless an agent holds the tab
                // now, whose remedy would be typed into that agent.
                if held {
                    (Outcome::Relaunched, RESTORED_PANIC_HELD)
                } else {
                    (Outcome::NotYet("wait:resume".to_string()), "wait:resume")
                }
            } else {
                aterm_log::warn!(
                    "harness @{sid}: the relaunch after aterm ended landed {}, but no look at its \
                     agent could be made; it is over, its exit not watched until a pass names it",
                    taken.agent.place
                );
                (Outcome::Relaunched, "landed")
            }
        } else {
            let last = Outcome::NotYet("wait:resume".to_string());
            attempt.tried += 1;
            let waiting = attempt
                .resume_wait_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
            if attempt.faults < RESTORED_TRIES
                && restored_retry("wait:resume", &last, attempt.tried, waiting)
            {
                attempt.due = now + (hooks.pause)(attempt.pause);
                attempt.pause = (attempt.pause * 2).min(RESTORED_MAX_PAUSE);
                return;
            }
            if held {
                (Outcome::Relaunched, RESTORED_PANIC_HELD)
            } else {
                (last, "wait:resume")
            }
        }
    } else if held {
        (Outcome::Relaunched, RESTORED_PANIC_HELD)
    } else {
        (Outcome::Cannot(RESTORED_PANIC.to_string()), RESTORED_PANIC)
    };
    let attempt = queue.attempts.remove(at).expect("the attempt was found");
    drop(queue);
    if step == RESTORED_PANIC_HELD {
        aterm_log::info!(
            "harness @{sid}: an agent holds the tab {} whose relaunch after aterm ended \
             panicked; the relaunch is over, the agent its own",
            taken.agent.place
        );
    }
    let missed = caught(
        &format!("@{sid}: the end of the relaunch after aterm ended"),
        "its tab's word may stand",
        || restored_over(hooks.badge.as_ref(), attempt, last, step),
    )
    .flatten();
    shared
        .restored
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .missed
        .extend(missed);
}

const RESTORED_PANIC_HELD: &str = "internal-panic:tab-held";

const RESTORED_PANIC: &str = "internal-panic";

/// The restored queue's worker that panicked outside any one tab's step, the last resort.
fn restored_queue_panicked(shared: &Shared, badge: &BadgeFn) -> Vec<RestoredMiss> {
    let (attempts, mut missed) = {
        let mut queue = shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.running = false;
        if queue.closed {
            queue.missed.clear();
            return Vec::new();
        }
        (
            std::mem::take(&mut queue.attempts),
            std::mem::take(&mut queue.missed),
        )
    };
    for attempt in attempts {
        let sid = attempt.agent.sid.clone();
        let last = Outcome::Cannot(RESTORED_PANIC.to_string());
        missed.extend(
            caught(
                &format!("@{sid}: the end of the relaunch after aterm ended"),
                "its tab's word may stand",
                || restored_over(badge, attempt, last, RESTORED_PANIC),
            )
            .flatten(),
        );
    }
    missed
}

/// A restored attempt is over, `last` its last step's outcome (`step` its word).
fn restored_over(
    badge: &BadgeFn,
    attempt: RestoredAttempt,
    last: Outcome,
    step: &str,
) -> Option<RestoredMiss> {
    let RestoredAgent {
        sid, snap, place, ..
    } = attempt.agent;
    let miss = (last != Outcome::Relaunched
        && crate::restore::agent_resumes(snap.session.as_deref(), &snap.argv))
    .then(|| {
        aterm_log::warn!(
            "harness @{sid}: the agent aterm hosted {place} was not relaunched after aterm \
             ended (last step: {step})"
        );
        RestoredMiss {
            order: attempt.order,
            sid: sid.clone(),
            place,
            agent: snap.agent(),
            outcome: last,
            said: 0,
        }
    });
    restored_word_cleared(badge, &sid);
    miss
}

/// The word a relaunch raised on tab `sid`.
fn restored_word_cleared(badge: &BadgeFn, sid: &str) {
    let _ = caught(
        &format!("@{sid}: the word of the relaunch after aterm ended"),
        "its tab's word may stand",
        || badge(sid, None),
    );
}

const RESTORED_LINE: &str = "relaunch-after-host-ended";

/// The journal's line for a held tab's end.
pub(crate) fn restored_held_line(
    pid: u32,
    tab: &str,
    session: Option<&str>,
    version: Option<&str>,
) -> String {
    let report = upgrade_drive::Report {
        pid,
        tab: tab.to_string(),
        session: session.unwrap_or("-").to_string(),
        from: version.unwrap_or("-").to_string(),
        to: "-".to_string(),
        step: "ended:held".to_string(),
    };
    report.line_as(RESTORED_LINE)
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

/// A restored asked exit's look before its step.
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

/// What a look at the agent a restored step landed found ([`restored_landing_look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LandingLook {
    /// Read, its exit watched until a pass names it — or named by a pass already: its own worker's.
    Read,
    /// It holds its tab, and no read reaches it yet.
    Unread,
    /// Gone before any read reached it.
    Gone,
    /// Gone, by the conversation's record.
    Died,
}

const LANDED_GONE: &str = "exited-before-read";

/// The agent a restored step landed in tab `sid`.
fn restored_landing_look(
    shared: &Shared,
    hooks: &Hooks,
    sid: &str,
    seen: bool,
    snap: &Snapshot,
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
    if host_carries_relaunch(snap.agent().program()) {
        with_kept_of(&kept, |k| k.carry_asked = true);
    }
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
        return LandingLook::Read;
    }
    match (hooks.acts.adopted_gone)(sid, snap) {
        Some(false) => return LandingLook::Unread,
        Some(true) => return LandingLook::Died,
        None => {}
    }
    if (hooks.still_wanted)(sid, 0) {
        LandingLook::Unread
    } else {
        LandingLook::Gone
    }
}

/// The restored attempt whose step landed, after `look` at its agent.
fn restored_landing_ends(
    mut attempt: RestoredAttempt,
    look: LandingLook,
    next_look: Duration,
    back_off: Duration,
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
            attempt.in_flight_only = false;
            attempt.due = now + next_look;
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
        LandingLook::Gone | LandingLook::Died if attempt.tried < RESTORED_TRIES => {
            aterm_log::info!(
                "harness @{}: the agent the relaunch after aterm ended landed {} exited before \
                 any read reached it; its relaunch is made again",
                attempt.agent.sid,
                attempt.agent.place
            );
            attempt.landed = false;
            attempt.in_flight_only = false;
            attempt.resume_wait_since = None;
            attempt.due = now + back_off;
            attempt.pause = (attempt.pause * 2).min(RESTORED_MAX_PAUSE);
            LandingEnd::Keep(attempt)
        }
        LandingLook::Gone | LandingLook::Died => LandingEnd::Over(
            attempt,
            Outcome::Cannot(LANDED_GONE.to_string()),
            LANDED_GONE,
        ),
    }
}

/// A landing's agent, read into `sid`'s [`Kept`], is watched until a pass names it.
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

/// The one row for the restored agents that did not come back ([`crate::message_reporters::restored_agents_not_resumed`]).
fn say_restored(shared: &Shared, runs: &StillFn, mut missed: Vec<RestoredMiss>) {
    if missed.is_empty() {
        return;
    }
    let restored = || {
        shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    };
    let earlier: Vec<String> = restored().said.iter().map(|m| m.sid.clone()).collect();
    let back: Vec<String> = earlier
        .into_iter()
        .filter(|sid| {
            caught(
                &format!("@{sid}: the look at the tab of an agent said not to have come back"),
                "it is said again",
                || runs(sid, 0),
            ) == Some(true)
        })
        .collect();
    missed.sort_by_key(|m| m.order);
    let log = crate::logging::log_dir().map(|dir| dir.join("aterm.log"));
    let row = {
        let mut queue = restored();
        queue.said.retain(|m| !back.contains(&m.sid));
        for mut miss in missed {
            miss.said = restored_said_next();
            match queue.said.iter_mut().find(|m| m.sid == miss.sid) {
                Some(said) => *said = miss,
                None => queue.said.push(miss),
            }
        }
        RestoredRow {
            seq: restored_said_next(),
            lines: queue
                .said
                .iter()
                .map(|m| RestoredLine {
                    sid: m.sid.clone(),
                    place: m.place.clone(),
                    agent: m.agent,
                    outcome: m.outcome.clone(),
                    said: m.said,
                })
                .collect(),
            log,
        }
    };
    crate::message_inbox::queue_restored_row(shared.inbox_lane, row);
}

#[cfg(any(unix, test))]
#[allow(
    clippy::too_many_arguments,
    reason = "the host's hooks the carry reads (its step, words, runs, pause and clock) beside the tabs it owes"
)]
fn carry_in_flight_after_handoff(
    shared: &Shared,
    carry: &CarryInFlightFn,
    words: &WordsFn,
    runs: &StillFn,
    pause: &PauseFn,
    clock: &dyn CarryClock,
    sessions: &[(String, String)],
    grace: u32,
) {
    type Owed<'a> = (
        usize,
        &'a str,
        &'a str,
        Instant,
        Duration,
        Option<(String, Outcome, upgrade::Agent)>,
        Instant,
    );
    let started = clock.now();
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
                started,
            )
        })
        .collect();
    let mut missed = Vec::new();
    let mut late = Vec::new();
    while !owed.is_empty() {
        let suspended = {
            let state = shared.lock();
            if state.shutting_down {
                return;
            }
            state.suspended
        };
        if suspended {
            clock.sleep(pause(RESTORED_FIRST_PAUSE));
            let now = clock.now();
            for tab in &mut owed {
                tab.6 = now;
            }
            continue;
        }
        let (out, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut owed)
            .into_iter()
            .partition(|tab| clock.now().saturating_duration_since(tab.6) >= bound);
        owed = kept;
        late.extend(out);
        if owed.is_empty() {
            break;
        }
        let at = owed
            .iter()
            .enumerate()
            .min_by_key(|(_, (order, _, _, due, _, _, _))| (*due, *order))
            .map(|(at, _)| at)
            .expect("owed was not empty");
        let now = clock.now();
        if owed[at].3 > now {
            clock.sleep((owed[at].3 - now).min(pause(RESTORED_FIRST_PAUSE)));
            continue;
        }
        let (order, sid, place, _, wait, _, _) = owed[at];
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
        let Some((step, agent)) = carry(sid, grace) else {
            owed.remove(at);
            continue;
        };
        let last = relaunch::outcome(&step);
        let supervised = step == CARRY_SUPERVISED;
        if supervised || restored_again(&step, &last) {
            if step == relaunch::WAIT_HAND_LIFTS {
                owed[at].6 = clock.now();
            }
            owed[at].3 = clock.now() + pause(wait);
            owed[at].4 = (wait * 2).min(RESTORED_MAX_PAUSE);
            owed[at].5 = (!supervised).then_some((step, last, agent));
            continue;
        }
        owed.remove(at);
        if matches!(last, Outcome::Cannot(_)) {
            aterm_log::warn!(
                "harness @{sid}: the restart the previous aterm left in flight {place} could \
                 not be carried on (last step: {step})"
            );
            missed.push(RestoredMiss {
                order,
                sid: sid.to_string(),
                place: place.to_string(),
                agent,
                outcome: last,
                said: 0,
            });
        }
    }
    for (order, sid, place, _, _, last, _) in late.into_iter().chain(owed) {
        let Some((step, last, agent)) = last else {
            continue;
        };
        aterm_log::warn!(
            "harness @{sid}: the restart the previous aterm left in flight {place} was not \
             carried on in time (last step: {step})"
        );
        missed.push(RestoredMiss {
            order,
            sid: sid.to_string(),
            place: place.to_string(),
            agent,
            outcome: last,
            said: 0,
        });
    }
    say_restored(shared, runs, missed);
}

/// One answer of [`Acts::carry_in_flight`] for a tab.
fn carry_in_flight_now(
    in_flight: impl FnOnce() -> Option<upgrade::Agent>,
    supervisable: impl FnOnce() -> bool,
    step: impl FnOnce() -> String,
) -> Option<(String, upgrade::Agent)> {
    let agent = in_flight()?;
    if supervisable() {
        return Some((CARRY_SUPERVISED.to_string(), agent));
    }
    Some((step(), agent))
}

/// How long the successor carries a restart left in flight ([`carry_in_flight_after_handoff`]).
#[cfg(any(unix, test))]
fn carry_bound(pause: &PauseFn) -> Duration {
    pause(Duration::from_secs(relaunch::STALE_S) + 2 * RESTORED_MAX_PAUSE)
}

/// A worker's reservation ends before it is marked done.
struct LoginEnd {
    latches: Arc<LoginLatches>,
    tab: String,
    owner: Arc<AtomicBool>,
}

impl Drop for LoginEnd {
    fn drop(&mut self) {
        self.latches.abandon_for(&self.tab, &self.owner);
    }
}

/// One worker's thread, running `main` ([`worker_main`], or [`exit_main`] for an exit no worker was handed).
fn spawn_worker(
    sid: &str,
    job: WorkerJob,
    done: Arc<AtomicBool>,
    hooks: &Hooks,
    main: fn(WorkerJob, &Hooks) -> WorkerExit,
) -> Option<JoinHandle<WorkerExit>> {
    let login_end = job.host.upgrade().map(|shared| LoginEnd {
        latches: Arc::clone(&shared.login_latches),
        tab: latch_tab(sid),
        owner: Arc::clone(&job.stop),
    });
    let hooks = hooks.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}{sid}"))
        .spawn(move || {
            let exit = main(job, &hooks);
            drop(login_end);
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
    followed: HashMap<String, FollowStamp>,
    settle: HashMap<String, Instant>,
    timer_at: Option<Instant>,
    full_epoch_seen: u64,
    phase_epoch_seen: u64,
    held: HashMap<String, u64>,
    forgiven_gen: u64,
    view: ViewLook,
    disk_at: Instant,
    holding: HashMap<String, Held>,
    watches: Watches,
    was_off: std::collections::HashSet<String>,
    startups: Startups,
}

impl HostState {
    /// The state a host thread starts with over `shared`.
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
            startups: Startups::held_after(held_deadline()),
        }
    }
}

/// A supervised agent as the roster last named it ([`HostState::holding`]).
struct Held {
    agent: Program,
    stamp: FollowStamp,
    look: Option<(Instant, Duration)>,
    kept: Option<(Instant, u64)>,
}

/// How long after a followed tab's screen moved the owner's view is looked at once more ([`look_at_upgrades`]).
pub(crate) const STATUS_AFTER_SCREEN: Duration = Duration::from_secs(2);

/// What one look at the owner's view was taken over ([`ViewLook::shown`]).
type ViewSeen = (u64, u64, Vec<String>, Vec<String>, Vec<String>, Vec<String>);

/// What the host last looked at the owner's view of the upgrades over ([`look_at_upgrades`]).
#[derive(Default)]
struct ViewLook {
    shown: Option<ViewSeen>,
    next: Option<Instant>,
    next_s: Option<u64>,
    follows: Vec<(String, u64)>,
    settle: Option<Instant>,
    watch: Vec<String>,
    claimed: Vec<String>,
    off: Vec<String>,
    agent_off: Vec<String>,
    unlisted: Option<Option<PathBuf>>,
    unreadable: Option<Vec<PathBuf>>,
}

/// The host thread's passes over `host` until shutdown.
fn host_loop(shared: &Arc<Shared>, host: &mut HostState, hooks: &Hooks) {
    let mut seen = bell_now();
    let mut timed_out = true;
    while let ControlFlow::Continue(until) = host_pass(shared, host, timed_out, hooks) {
        (seen, timed_out) = bell_wait(seen, until, host.view.next_s);
    }
}

/// A host that panicked starts again with what it held.
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

const RESTART_AGAIN: Duration = Duration::from_secs(1);

/// The host's restart after its run's `attempt`-th panic in a row.
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

/// The restart holds what it carries to the policy as it is.
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

/// The sessions Autopilot has Off, as one read of the sessions' own words ([`Hooks::autopilot`]) under the table's default (`[harness] autopilot`) says them.
#[derive(Debug, Default)]
struct OffSessions {
    words: HashMap<String, Autopilot>,
    default: Autopilot,
}

impl OffSessions {
    /// The sessions' words as `hooks` reads them now, under the table's default `default`.
    fn read(hooks: &Hooks, default: Autopilot) -> Self {
        Self {
            words: (hooks.autopilot)(),
            default,
        }
    }

    /// Whether session `sid` is Off.
    fn contains(&self, sid: &str) -> bool {
        self.mode(sid) == Autopilot::Off
    }

    /// Session `sid`'s Autopilot: its own word, else the table's default.
    fn mode(&self, sid: &str) -> Autopilot {
        self.words.get(sid).copied().unwrap_or(self.default)
    }
}

/// One pass of the host over the sessions as they are now (`timed_out`: no bell woke it, a look it set came due).
fn host_pass(
    shared: &Arc<Shared>,
    host: &mut HostState,
    timed_out: bool,
    hooks: &Hooks,
) -> ControlFlow<(), Option<Instant>> {
    let full_now = FULL_FOLLOW_EPOCH.load(Ordering::SeqCst);
    let phase_now = PHASE_FOLLOW_EPOCH.load(Ordering::SeqCst);
    let now = Instant::now();
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
    let activation_at = try_activation_start(shared, hooks, now);
    let armed_before = EXIT_WATCH_ARMINGS.load(Ordering::SeqCst);
    let steps = StepsSeen::read(&host.workers);
    let (mut wanted, stamps): (HashMap<String, Program>, HashMap<String, FollowStamp>) = if active {
        (hooks.roster)().into_iter().fold(
            (HashMap::new(), HashMap::new()),
            |(mut wanted, mut stamps), (sid, program, stamp)| {
                if !admits(program, stamp) {
                    return (wanted, stamps);
                }
                wanted.insert(sid.clone(), program);
                stamps.insert(sid, stamp);
                (wanted, stamps)
            },
        )
    } else {
        (HashMap::new(), HashMap::new())
    };
    let off = if active {
        OffSessions::read(hooks, cfg.autopilot)
    } else {
        OffSessions::default()
    };
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
    let named_off: HashMap<String, Program> = wanted
        .iter()
        .filter(|(_, agent)| !shared.switches.on_for(**agent))
        .map(|(sid, agent)| (sid.clone(), *agent))
        .collect();
    let switched_off: Vec<String> = named_off.keys().cloned().collect();
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
        end_exit_watches(shared, armed_before, |sid| wanted.contains_key(sid));
        end_exit_watches(shared, EVERY_WATCH, |sid| off.contains(sid));
        look
    } else {
        // A Commit's hold forgets nothing the roster last named: a pass that cannot name an
        // agent once the host supervises again is asked the same fact as any, so the exit the
        // Commit's stop left to the host ([`Kept::unwatched`]) is not handed for an agent that
        // still holds its tab. Supervision switched off forgets it all and watches no exit.
        if !suspended {
            host.holding.clear();
            end_exit_watches(shared, EVERY_WATCH, |_| true);
        }
        None
    };
    #[cfg(test)]
    assert!(
        !shared.passes_panic.load(Ordering::SeqCst),
        "every pass of the host panics (a test's)"
    );
    let kept_sids: std::collections::HashSet<String> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .keys()
        .cloned()
        .collect();
    shared
        .host_faults
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|sid, _| {
            wanted.contains_key(sid) || host.workers.0.contains_key(sid) || kept_sids.contains(sid)
        });
    note_agents_in_restored_tabs(shared, &wanted, &switched_off);
    host.held.retain(|sid, _| wanted.contains_key(sid));
    age_and_forgive(shared, host, cfg_gen, &wanted);
    forget_closed(shared, &host.workers, &wanted, active, hooks);
    let settle_at = follow_workers(shared, host, &wanted, &stamps, follow_all, now, hooks);
    end_lost_words(shared, hooks);
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
    publish_off(shared, &host.workers, &wanted, &off);
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
            shared.activations.any(),
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
    crate::disk_board::watching(would_supervise(&cfg, shared.headless));
    let disk_at = look_at_disk(
        &mut host.disk_at,
        Instant::now(),
        active,
        &disk_roster(&wanted, active, hooks),
        &off,
        hooks,
    );
    let restored: Vec<String> = shared
        .restored
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .attempts
        .iter()
        .map(|attempt| attempt.agent.sid.clone())
        .collect();
    host.startups.waits = HarnessWaits {
        grace: Duration::from_secs(u64::from(cfg.human_grace_s)),
        relaunching: relaunches_pending(&host.workers, restored.iter().map(String::as_str)),
    };
    let startup_at = look_at_startups(&mut host.startups, Instant::now(), active, &off, hooks);
    let fault_at = host_faults_due(shared, Instant::now());
    host.timer_at = [
        next_look,
        hold_look,
        view_at,
        watch_at,
        disk_at,
        startup_at,
        fault_at,
        activation_at,
    ]
    .into_iter()
    .flatten()
    .min();
    ControlFlow::Continue([host.timer_at, settle_at].into_iter().flatten().min())
}

/// Whether the timer that ended the host's wait was a settle's alone ([`HostState::settle`]).
fn settle_alone(host: &HostState, now: Instant) -> bool {
    host.settle.values().any(|at| *at <= now)
        && host.timer_at.is_none_or(|at| at > now)
        && host
            .view
            .next_s
            .is_none_or(|at| at > crate::upgrade_host::now_s())
}

/// Follow retained agent records once more after the bounded worker stop/join, before the quit layout reads them.
fn follow_at_shutdown(shared: &Shared, hooks: &Hooks) {
    let kept: Vec<(String, Arc<Mutex<Kept>>)> = shared
        .kept
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(sid, kept)| (sid.clone(), Arc::clone(kept)))
        .collect();
    for (sid, kept) in kept {
        let _ = host_caught(&sid, "the follow at shutdown", || {
            follow_snapshot(&sid, &kept, hooks)
        });
    }
}

/// A generic wake always owes a full follow.
const fn full_follow_due(
    timed_out: bool,
    full_before: u64,
    full_now: u64,
    phase_before: u64,
    phase_now: u64,
) -> bool {
    timed_out || full_before != full_now || phase_now < phase_before || phase_now - phase_before > 1
}

/// The disk watch's roster.
fn disk_roster(
    wanted: &HashMap<String, Program>,
    active: bool,
    hooks: &Hooks,
) -> HashMap<String, Program> {
    let mut roster = wanted.clone();
    if active {
        for sid in (hooks.codex_roster)() {
            roster.entry(sid).or_insert(Program::Codex);
        }
    }
    roster
}

/// The disk watch's timer.
fn look_at_disk(
    disk_at: &mut Instant,
    now: Instant,
    active: bool,
    wanted: &HashMap<String, Program>,
    off: &OffSessions,
    hooks: &Hooks,
) -> Option<Instant> {
    let mut sids: Vec<String> = if active {
        wanted
            .keys()
            .filter(|sid| !off.contains(sid))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    crate::disk_board::suspend(!active);
    if now >= *disk_at {
        sids.sort();
        *disk_at = now + DISK_TICK;
        (hooks.note)(HostNote::DiskLooked);
        let _ = caught("the disk watch", "it looks again at its next tick", || {
            (hooks.disk)(&sids);
        });
    }
    Some(*disk_at)
}

/// One session with a task record, as the deadlines read it ([`Acts::startups`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Startup {
    pub(crate) sid: String,
    pub(crate) state: StartupState,
    /// When the record entered its state (its `since_ms=`): where the deadline counts from.
    pub(crate) since: Instant,
    /// What its screen shows.
    pub(crate) seen: StartupSeen,
    /// What the held deadline reads of it ([`HeldRead`]).
    pub(crate) held: Option<HeldRead>,
    /// The supervisor's attention standing on it where that is a startup stall's own
    /// ([`STARTUP_HEAD`]): what [`restates`] compares the screen's hold with.
    pub(crate) standing: Option<String>,
    /// Its launcher is installing its program now (`task_launch::InstallingMark`), so no agent
    /// could have started yet.
    pub(crate) installing: bool,
}

/// A task record's state as the deadlines judge it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupState {
    /// `working` with no `why=`: the record as its starter left it, never moved since.
    Working,
    /// `working why=turn` or `why=moved`.
    Reopened,
    /// `stalled why=startup`, the deadline's own mark: its start is looked for, and marked `working` again.
    StalledAtStartup,
    /// `stalled why=held`, the held deadline's own mark.
    StalledHeld,
    /// Anything else: `done`, `exited`, the launcher's `stalled why=shell`.
    Other,
    /// A record no deadline's stall stands on whose session still carries a deadline's own attention ([`deadline_attention_stands`]).
    StallOver,
}

/// What a worker's screen says of its start ([`startup_seen`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartupSeen {
    /// The agent has taken its task up.
    Started,
    /// Not yet: what holds the screen instead.
    Holds(StartupHold),
    /// Not read (another thread held the screen, or the record is not one to judge).
    Unread,
}

/// What holds a worker's screen where its composer should be ([`startup_seen`]), named for a person in [`Self::attention`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StartupHold {
    /// The agent's folder-trust dialog, which no rule answered.
    Trust { agent: Program },
    /// Another box the agent drew: its title.
    Box { agent: Program, title: String },
    /// The agent with no composer and no box: a login or onboarding screen.
    NoComposer { agent: Program },
    /// No such agent at all: `program`, the tab's foreground program.
    NoAgent {
        agent: Program,
        program: Option<String>,
    },
    /// A codex-alab startup screen that waits for a person, as codex-alab's own attention says
    /// (`text`, owner [`crate::task_record::SELF_CLAIM_ATTENTION_OWNER`]: a sign-in, a folder
    /// trust its answer left, a forced model move, a hooks review), before any claim.
    Asked { text: String },
}

impl StartupHold {
    /// The supervisor's attention for a worker this holds past its deadline: for
    /// [`Self::Asked`], codex-alab's own words after [`STARTUP_HEAD`], cut to an entry's
    /// cap, since the supervisor's newer entry is the one aterm shows.
    #[must_use]
    pub(crate) fn attention(&self) -> String {
        let what = match self {
            Self::Trust { agent } => {
                format!(
                    "{}'s folder-trust dialog stands on its screen",
                    agent.product()
                )
            }
            Self::Box { agent, title } => {
                let title: String = title.trim().chars().take(60).collect();
                format!(
                    "a box {} drew stands on its screen ({title})",
                    agent.product()
                )
            }
            Self::NoComposer {
                agent: Program::Codex,
            } => "codex-alab never claimed its tab (a login or onboarding screen, or the `codex` \
                  it ran is not codex-alab)"
                .to_string(),
            Self::NoComposer { agent } => format!(
                "{} shows no composer (a login or onboarding screen)",
                agent.product()
            ),
            Self::NoAgent {
                agent,
                program: Some(p),
            } => {
                format!("no {} started; `{p}` holds the tab", agent.product())
            }
            Self::NoAgent {
                agent,
                program: None,
            } => format!("no {} started", agent.product()),
            Self::Asked { text } => {
                let head = format!("{STARTUP_HEAD}{ASKS}");
                let room = crate::session_timeline::META_ATTENTION_KEYED_MAX - head.len();
                return format!("{head}{}", cut_to(text, room));
            }
        };
        format!("{STARTUP_HEAD}{what} (its task is marked stalled why=startup)")
    }
}

/// How a startup stall's attention quoting codex-alab begins, after [`STARTUP_HEAD`] ([`StartupHold::Asked`]).
const ASKS: &str = "codex-alab asks: ";

/// Whether a startup stall's standing attention (`standing`) is to name `hold` instead: where
/// codex-alab's own words came or went (an [`StartupHold::Asked`] quote on either side, since
/// codex-alab takes its attention down once its screen's step is done while the stall stands)
/// and the words differ. Every other stall keeps the words it was raised with.
pub(crate) fn restates(standing: Option<&str>, hold: &StartupHold) -> bool {
    let Some(standing) = standing else {
        return false;
    };
    let quoted = standing
        .strip_prefix(STARTUP_HEAD)
        .is_some_and(|rest| rest.starts_with(ASKS));
    (quoted || matches!(hold, StartupHold::Asked { .. })) && standing != hold.attention()
}

/// `text` within `max` bytes: its head, ended by `…` where it is longer.
fn cut_to(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Everything that moves when a worker does, as the held deadline reads it ([`HeldRead`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Progress {
    /// How often a screen read found text changes (`AgentPublication::text_moves`), excluding colour-only or resize repaints.
    pub(crate) screen: u64,
    /// How often its agent verdict moved as the program's (`AgentPublication::verdict_moves`).
    pub(crate) verdict: u64,
    /// The input writes made to it (`OutputEchoTracker::input_sent`): a key, a paste, a `send`, a `turn`.
    pub(crate) input: u64,
    /// Its transcript's length as the footer's read last found it (`ClaudeExcerpts::transcript_len`).
    pub(crate) transcript: Option<u64>,
}

/// What the held deadline reads of a worker ([`look_at_startups`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HeldRead {
    /// Its agent verdict's word (`agent=`).
    pub(crate) word: String,
    /// The verdict's detail (`agent_detail=`), when it has one.
    pub(crate) detail: Option<String>,
    /// The tab's foreground program, for a screen no agent holds.
    pub(crate) program: Option<String>,
    /// [`Progress`].
    pub(crate) progress: Progress,
    /// The supervisor's attention stands on it and is no deadline's own.
    pub(crate) escalated: bool,
    /// The last row of its screen with words on it, trimmed.
    pub(crate) last_row: Option<String>,
    /// Claude Code's settings panel (`/usage`, `/status`, `/config`) stands on that screen (`aterm_phase::settings_panel`).
    pub(crate) panel: bool,
    /// The screen was not read at this look where the worker may be held (the engine's lock was busy, or its program's DEC 2026 frame unfinished).
    pub(crate) unread: bool,
    /// When a person last gave the session input (its person stamp, what `status human_ms=` reports; `None`: no person has in this process).
    pub(crate) person: Option<Instant>,
    /// A driver's hand stands on the session at this read ([`driver_holds`]: another holder's live drive lease, or a `turn` a named driver typed).
    pub(crate) driver: bool,
    /// A proven self-claim carries the session ([`crate::self_claim`]): its program
    /// answers for its own holds with its own attention, as codex-alab does.
    pub(crate) carried: bool,
}

impl HeldRead {
    /// Whether the worker is at a screen the held deadline judges.
    #[must_use]
    pub(crate) fn may_be_held(&self) -> bool {
        held_judges(&self.word) && !self.escalated && !self.carried
    }
}

/// The harness's own long waits on screens the held deadline judges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HarnessWaits {
    /// `[harness] human_grace_s`.
    pub(crate) grace: Duration,
    /// The sessions whose agent's exit a worker holds.
    pub(crate) relaunching: std::collections::HashSet<String>,
}

impl HarnessWaits {
    /// When the harness's hold for a hand on the session ends, on a screen it holds an act on for one ([`waits_out_a_hand`]).
    fn hands_off_until(&self, read: &HeldRead, hand: Option<Instant>) -> Option<Instant> {
        if !waits_out_a_hand(read) {
            return None;
        }
        read.person
            .max(hand)
            .and_then(|at| at.checked_add(self.grace))
    }
}

/// Whether the harness holds an act for a hand on the screen `read` shows ([`HarnessWaits::grace`]).
#[must_use]
fn waits_out_a_hand(read: &HeldRead) -> bool {
    read.panel || matches!(read.word.as_str(), "prompt" | "question" | "survey")
}

/// Whether a driver's hand stands on a session, from its lease (`SessionCtx::turn_lease`) at `now_us`.
#[must_use]
pub(crate) fn driver_holds(lease: Option<&crate::Lease>, now_us: u64, ours: &str) -> bool {
    match lease {
        Some(crate::Lease::Turn { driver, .. }) => driver.is_some(),
        Some(crate::Lease::Drive {
            holder, expires_us, ..
        }) => *expires_us > now_us && holder != ours,
        None => false,
    }
}

/// [`HarnessWaits::relaunching`] as the pass reads it.
fn relaunches_pending<'a>(
    workers: &Workers,
    restored: impl IntoIterator<Item = &'a str>,
) -> std::collections::HashSet<String> {
    workers
        .0
        .iter()
        .filter(|(_, w)| w.left.is(Word::Left))
        .map(|(sid, _)| sid.clone())
        .chain(restored.into_iter().map(str::to_string))
        .collect()
}

/// Whether a verdict is one the held deadline judges.
#[must_use]
pub(crate) fn held_judges(word: &str) -> bool {
    !(word == "busy" || word == "idle" || word.starts_with("wall"))
}

/// The supervisor's attention for a worker held `held_for` at what `read` shows.
#[must_use]
pub(crate) fn held_attention(read: &HeldRead, held_for: Duration) -> String {
    let secs = held_for.as_secs();
    let took = if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    };
    let at = match (read.word.as_str(), &read.program) {
        ("-", Some(program)) => format!("with no agent in front (`{program}` holds the tab)"),
        ("-", None) => "with no agent in front".to_string(),
        (word, _) => match &read.detail {
            Some(detail) => format!("at agent={word} ({detail})"),
            None => format!("at agent={word}"),
        },
    };
    let head = format!("{HELD_HEAD}nothing moved for {took} {at}");
    let tail = " (its task is marked stalled why=held)";
    let room = crate::session_timeline::META_ATTENTION_KEYED_MAX
        .saturating_sub(head.len() + tail.len() + ", its screen ending \"\"".len());
    let ends = read
        .last_row
        .as_deref()
        .map(|row| crate::session_timeline::sanitize_presentation_line(row, room))
        .filter(|row| !row.is_empty())
        .map(|row| format!(", its screen ending \"{row}\""))
        .unwrap_or_default();
    format!("{head}{ends}{tail}")
}

/// Whether the supervisor's attention standing on a worker (`standing`) is a deadline's own ([`STARTUP_HEAD`], [`HELD_HEAD`]).
#[must_use]
pub(crate) fn deadline_attention_stands(standing: Option<&str>) -> bool {
    standing.is_some_and(aterm_agent::supervise::deadline_attention)
}

/// The deadlines' marks ([`Acts::startup_mark`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupMark<'a> {
    /// `stalled why=startup`, with what holds the screen ([`StartupHold::attention`]) as the
    /// supervisor's attention.
    Stalled { hold: &'a StartupHold },
    /// `working` again, and the attention the startup stall set taken down.
    Started,
    /// `stalled why=held`, with what `read` shows after `held_for` with nothing moving
    /// ([`held_attention`]) as the supervisor's attention.
    Held {
        read: &'a HeldRead,
        held_for: Duration,
    },
    /// `working why=moved`, and the attention the held stall set taken down: the worker moved.
    Moved,
    /// No mark: the record has moved on from a deadline's stall by another writer's mark.
    Over,
    /// No mark: a startup stall still standing names `hold` now, its attention the startup
    /// stall's own still ([`restates`]).
    Restated { hold: &'a StartupHold },
}

/// The held deadline's clock for one worker ([`Startups::held`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HeldClock {
    progress: Progress,
    since: Instant,
    hand: Option<Instant>,
}

impl HeldClock {
    /// A clock started at `now` on what `read` shows.
    const fn at(read: &HeldRead, now: Instant) -> Self {
        Self {
            progress: read.progress,
            since: now,
            hand: None,
        }
    }
}

/// The deadlines' books ([`look_at_startups`]).
struct Startups {
    from: Option<Instant>,
    started: std::collections::HashSet<String>,
    off: HashMap<String, Instant>,
    hands: HashMap<String, Instant>,
    held: HashMap<String, HeldClock>,
    held_after: Duration,
    waits: HarnessWaits,
}

impl Default for Startups {
    fn default() -> Self {
        Self::held_after(HELD_DEADLINE)
    }
}

impl Startups {
    /// Empty books whose held deadline is `held_after`.
    fn held_after(held_after: Duration) -> Self {
        Self {
            from: None,
            started: std::collections::HashSet::new(),
            off: HashMap::new(),
            hands: HashMap::new(),
            held: HashMap::new(),
            held_after,
            waits: HarnessWaits::default(),
        }
    }
}

/// One look of the startup and held deadlines over every session with a task record: marks what
/// is due and returns when to look next.
fn look_at_startups(
    books: &mut Startups,
    now: Instant,
    active: bool,
    off: &OffSessions,
    hooks: &Hooks,
) -> Option<Instant> {
    if !active {
        books.from = None;
        return None;
    }
    let from = *books.from.get_or_insert(now);
    let reads = (hooks.acts.startups)(&books.started);
    books
        .started
        .retain(|sid| reads.iter().any(|w| w.sid == *sid));
    books
        .off
        .retain(|sid, _| reads.iter().any(|w| w.sid == *sid));
    books
        .hands
        .retain(|sid, _| reads.iter().any(|w| w.sid == *sid));
    books
        .held
        .retain(|sid, _| reads.iter().any(|w| w.sid == *sid && w.held.is_some()));
    let mut next: Option<Instant> = None;
    let mut soonest = |at: Instant| next = Some(next.map_or(at, |n| n.min(at)));
    for w in &reads {
        if off.contains(&w.sid) {
            books.off.insert(w.sid.clone(), now);
            books.hands.remove(&w.sid);
            books.held.remove(&w.sid);
            continue;
        }
        let back = books.off.get(&w.sid).copied().unwrap_or(from);
        match (w.state, &w.seen) {
            (StartupState::Other, _) => {}
            (StartupState::StallOver, _) => {
                (hooks.acts.startup_mark)(&w.sid, &StartupMark::Over);
                soonest(now + STARTUP_UNREAD_LOOK);
            }
            (StartupState::StalledHeld, _) => {
                if let Some(at) = look_at_held_stall(books, w, now, hooks) {
                    soonest(at);
                }
            }
            (StartupState::Reopened, _) => {
                if let Some(at) = judge_held(books, w, now, from.max(back), hooks) {
                    soonest(at);
                }
            }
            (StartupState::Working, _) if books.started.contains(&w.sid) => {
                if let Some(at) = judge_held(books, w, now, from.max(back), hooks) {
                    soonest(at);
                }
            }
            (_, StartupSeen::Started) => {
                books.started.insert(w.sid.clone());
                books.hands.remove(&w.sid);
                if w.state == StartupState::StalledAtStartup {
                    (hooks.acts.startup_mark)(&w.sid, &StartupMark::Started);
                } else if let Some(at) = judge_held(books, w, now, from.max(back), hooks) {
                    soonest(at);
                }
            }
            (_, StartupSeen::Unread) => soonest(now + STARTUP_UNREAD_LOOK),
            (StartupState::Working, StartupSeen::Holds(hold)) => {
                // The harness's own waits count no more here than at the held deadline.
                // An agent's exit in hand for a relaunch (whose shell a successor that
                // never saw the worker start reads as no agent) is not judged, and the
                // deadline falls a whole one after the last look that found it so. On a
                // screen the harness holds an act on for a hand, it falls no sooner than a
                // whole one after that hold ends, a driver's counted from the last look
                // that found it standing.
                // A launcher installing the worker's program has typed nothing yet: the deadline
                // falls a whole one after the last look that found it installing.
                if books.waits.relaunching.contains(&w.sid) || w.installing {
                    books.off.insert(w.sid.clone(), now);
                    soonest(now + HELD_RELOOK);
                    continue;
                }
                let driven = w.held.as_ref().is_some_and(|read| read.driver);
                if driven {
                    books.hands.insert(w.sid.clone(), now);
                }
                let hand = books.hands.get(&w.sid).copied();
                let start = w.since.max(from).max(back);
                let start = w
                    .held
                    .as_ref()
                    .and_then(|read| books.waits.hands_off_until(read, hand))
                    .map_or(start, |until| start.max(until));
                let due = start + STARTUP_DEADLINE;
                if now < due {
                    soonest(if driven {
                        due.min(now + HELD_RELOOK)
                    } else {
                        due
                    });
                    continue;
                }
                (hooks.acts.startup_mark)(&w.sid, &StartupMark::Stalled { hold });
            }
            (StartupState::StalledAtStartup, StartupSeen::Holds(hold)) => {
                if restates(w.standing.as_deref(), hold) {
                    (hooks.acts.startup_mark)(&w.sid, &StartupMark::Restated { hold });
                }
            }
        }
    }
    next
}

/// The held deadline's judgement of a started worker `w` ([`look_at_startups`]).
fn judge_held(
    books: &mut Startups,
    w: &Startup,
    now: Instant,
    floor: Instant,
    hooks: &Hooks,
) -> Option<Instant> {
    let read = w.held.as_ref()?;
    if !read.may_be_held() || books.waits.relaunching.contains(&w.sid) {
        books.held.remove(&w.sid);
        return None;
    }
    let clock = books
        .held
        .entry(w.sid.clone())
        .or_insert_with(|| HeldClock::at(read, now));
    if clock.progress != read.progress {
        clock.progress = read.progress;
        clock.since = now;
    }
    if read.driver {
        clock.hand = Some(now);
    }
    let from = clock.since.max(floor);
    let from = books
        .waits
        .hands_off_until(read, clock.hand)
        .map_or(from, |until| from.max(until));
    let due = from + books.held_after;
    if now < due {
        return Some(due.min(now + HELD_RELOOK));
    }
    if read.unread {
        return Some(now + STARTUP_UNREAD_LOOK);
    }
    let held_for = now.saturating_duration_since(clock.since);
    (hooks.acts.startup_mark)(&w.sid, &StartupMark::Held { read, held_for });
    Some(now + HELD_RELOOK)
}

/// A worker the held deadline stalled, looked at again ([`look_at_startups`]).
fn look_at_held_stall(
    books: &mut Startups,
    w: &Startup,
    now: Instant,
    hooks: &Hooks,
) -> Option<Instant> {
    let read = w.held.as_ref()?;
    let clock = books
        .held
        .entry(w.sid.clone())
        .or_insert_with(|| HeldClock::at(read, now));
    if read.may_be_held()
        && !books.waits.relaunching.contains(&w.sid)
        && clock.progress == read.progress
    {
        return Some(now + HELD_RELOOK);
    }
    books.held.remove(&w.sid);
    books.started.insert(w.sid.clone());
    (hooks.acts.startup_mark)(&w.sid, &StartupMark::Moved);
    None
}

/// What screen read `seen` says of a worker of kind `expected` that no proven claim carries.
/// A codex worker starts only when codex-alab claims its tab, so a codex screen that reads as
/// started is OpenAI's codex or a codex-alab that has not carried it
/// ([`StartupHold::NoComposer`]). While codex-alab may not claim the tab (`claimable` false: a
/// halt stands, or the session's Autopilot is off) it is not judged yet
/// ([`StartupSeen::Unread`]). Where codex-alab's own attention (`asked`, given only where no
/// other agent is in front) stands over a codex screen, that is what holds it
/// ([`StartupHold::Asked`]). Any other worker's read is the screen's.
pub(crate) fn unclaimed(
    seen: StartupSeen,
    expected: Option<Program>,
    claimable: bool,
    asked: Option<&str>,
) -> StartupSeen {
    match (expected, seen) {
        (
            Some(
                Program::Claude
                | Program::Agy
                | Program::Muse
                | Program::Grok
                | Program::Kimi
                | Program::Generic,
            )
            | None,
            seen,
        ) => seen,
        (Some(Program::Codex), _) if !claimable => StartupSeen::Unread,
        (
            Some(Program::Codex),
            StartupSeen::Started
            | StartupSeen::Holds(
                StartupHold::Trust {
                    agent: Program::Codex,
                }
                | StartupHold::Box {
                    agent: Program::Codex,
                    ..
                }
                | StartupHold::NoComposer {
                    agent: Program::Codex,
                },
            ),
        ) if let Some(text) = asked => StartupSeen::Holds(StartupHold::Asked {
            text: text.to_string(),
        }),
        (Some(Program::Codex), StartupSeen::Started) => {
            StartupSeen::Holds(StartupHold::NoComposer {
                agent: Program::Codex,
            })
        }
        (Some(Program::Codex), seen) => seen,
    }
}

/// What a worker's screen says of its start ([`look_at_startups`]).
pub(crate) fn startup_seen(
    rows: &[String],
    program: Option<&str>,
    agent: Option<Program>,
    expected: Option<Program>,
) -> StartupSeen {
    use aterm_phase::prompt::PromptKind;
    let reader = agent.or(expected).unwrap_or(Program::Claude);
    let read = aterm_phase::read(Some(reader.name()), rows, None);
    let prompt = match reader {
        Program::Claude => aterm_phase::parse_prompt_v2(rows),
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => read.prompt,
    };
    if let Some(p) = prompt {
        return match p.kind {
            PromptKind::Trust => StartupSeen::Holds(StartupHold::Trust { agent: reader }),
            PromptKind::Other => StartupSeen::Holds(StartupHold::Box {
                agent: reader,
                title: p.title,
            }),
            _ => StartupSeen::Started,
        };
    }
    if read.composer.is_some() {
        return StartupSeen::Started;
    }
    let its_own = agent.is_some() && !program.is_some_and(crate::quit_safety::is_shell_name);
    StartupSeen::Holds(if its_own {
        StartupHold::NoComposer { agent: reader }
    } else {
        StartupHold::NoAgent {
            agent: reader,
            program: program.map(str::to_string),
        }
    })
}

/// The tabs parked behind another supervisor's claim ([`ViewLook::claimed`]).
fn claimed_tabs(held: &HashMap<String, u64>, workers: &Workers) -> Vec<String> {
    let mut tabs: Vec<String> = held
        .keys()
        .filter(|sid| !workers.0.contains_key(*sid))
        .cloned()
        .collect();
    tabs.sort();
    tabs
}

/// The agent tabs the pass read Off ([`ViewLook::off`]).
fn off_tabs(wanted: &HashMap<String, Program>, off: &OffSessions) -> Vec<String> {
    let mut tabs: Vec<String> = wanted
        .keys()
        .filter(|sid| off.contains(sid))
        .cloned()
        .collect();
    tabs.sort();
    tabs
}

/// The owner's view, looked at when something it shows may have moved.
fn look_at_upgrades(
    view: &mut ViewLook,
    wanted: &HashMap<String, Program>,
    stamps: &HashMap<String, FollowStamp>,
    active: bool,
    (acts, activations): (u64, u64),
    hooks: &Hooks,
) -> Option<Instant> {
    if !active {
        let owed = view.shown.take().is_some() || view.next.is_some_and(|at| at <= Instant::now());
        let again = owed
            && caught(
                "the owner's view of the upgrades, standing down",
                "it is tried again in a while",
                || (hooks.upgrade_view)(false, &[], &[], &[]),
            )
            .is_none();
        if owed {
            view.next = again.then(|| Instant::now() + (hooks.pause)(WATCH_AGAIN));
        }
        if !again && view.unlisted.take().is_some_and(|told| told.is_some()) {
            (hooks.note)(HostNote::UpgradeRecords { unlisted: None });
        }
        if !again && view.unreadable.take().is_some_and(|told| !told.is_empty()) {
            (hooks.note)(HostNote::UpgradeRecordsUnreadable { files: Vec::new() });
        }
        view.next_s = None;
        view.follows.clear();
        view.settle = None;
        view.watch.clear();
        return view.next;
    }
    let mut tabs: Vec<String> = wanted.keys().cloned().collect();
    tabs.sort();
    let now = Instant::now();
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
        let Some(next) = caught(
            "the owner's view of the upgrades",
            "it is asked again once what it shows moves, or in a while",
            || (hooks.upgrade_view)(true, &view.claimed, &view.off, &view.agent_off),
        ) else {
            view.next = Some(now + (hooks.pause)(WATCH_AGAIN));
            view.next_s = None;
            view.follows.clear();
            view.settle = None;
            view.shown = seen;
            return view.next;
        };
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
        if view.unlisted.as_ref() != Some(&next.unlisted) {
            (hooks.note)(HostNote::UpgradeRecords {
                unlisted: next.unlisted.clone(),
            });
            view.unlisted = Some(next.unlisted);
        }
        if view.unreadable.as_ref() != Some(&next.unreadable) {
            (hooks.note)(HostNote::UpgradeRecordsUnreadable {
                files: next.unreadable.clone(),
            });
            view.unreadable = Some(next.unreadable);
        }
    }
    [view.next, view.settle].into_iter().flatten().min()
}

const WATCH_AGAIN: Duration = Duration::from_secs(120);

/// The watches the host fires ([`watch_upgrades`]).
#[derive(Default)]
struct Watches {
    told: BTreeSet<String>,
    fired: HashMap<String, Fired>,
}

/// One tab's last watch ([`Watches::fired`]).
struct Fired {
    running: Arc<AtomicBool>,
    at: Instant,
    again: Arc<AtomicBool>,
}

/// Whether a watch must be told again.
fn told_again(told: bool, watched: &upgrade_drive::Watched) -> bool {
    told && !matches!(
        watched.skipped.as_deref(),
        None | Some("no-record" | "not-due" | "moved" | "no-home")
    )
}

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

/// Start one watch of tab `sid` on a thread of its own at Background QoS, as the relaunch threads run.
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
    let spawned = crate::qos::spawn_lane_thread(
        format!("{THREAD_PREFIX}watch-{sid}"),
        crate::qos::Role::Background,
        move || {
            let watched = caught(
                &format!("@{tab}: the upgrade's watch"),
                "it is fired again as a watch that could not write",
                || watch_tab(&tab, told, grace, &points, &park, &look_at, &acts),
            );
            owed.store(
                watched.as_ref().map_or(told, |w| told_again(told, w)),
                Ordering::SeqCst,
            );
            flag.store(false, Ordering::SeqCst);
            ring_bell();
        },
    );
    match spawned {
        Ok(_) => Some((running, again)),
        Err(e) => {
            aterm_log::warn!("harness @{sid}: the upgrade's watch could not start: {e}");
            None
        }
    }
}

/// One watch of tab `sid`, the watch thread's whole body.
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

/// Reap the workers that ended (their last act set `done`, so the join waits on nothing but the thread's return).
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
            Some(Err(_)) => {
                aterm_log::warn!("harness @{sid}: the supervisor thread panicked; restarting it");
            }
        }
    }
}

/// A changed policy forgives every session's failures.
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

/// What is kept of a session for a relaunch lives as long as its tab (asked with the map's lock released).
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
            !wanted.contains_key(sid)
                && !workers.0.contains_key(sid)
                && tab_caught(
                    shared,
                    sid,
                    "the look at whether its tab is open",
                    hooks,
                    || (hooks.acts.open)(sid),
                ) == Some(false)
        })
        .collect();
    if closed.is_empty() {
        return;
    }
    for sid in &closed {
        crate::self_claim::forget(sid);
        shared.login_latches.gone(&latch_tab(sid));
    }
    let forgotten: Vec<(&String, Arc<Mutex<Kept>>)> = {
        let mut kept = shared.kept.lock().unwrap_or_else(PoisonError::into_inner);
        closed
            .iter()
            .filter_map(|sid| Some((sid, kept.remove(sid)?)))
            .collect()
    };
    for (sid, kept) in forgotten {
        let lost = with_kept_of(&kept, |k| {
            k.exit_watch = None;
            k.lost.take()
        });
        if let Some(lost) = lost {
            forget_lost(&lost.session, sid);
        }
    }
    if active {
        for sid in &closed {
            host_caught(sid, "the accounts recorder's forget", || {
                (hooks.acts.account_gone)(sid);
            });
        }
    }
}

/// Follow each worker whose publication moved, every worker after a generic wake, coalesced phase wakes or a timed-out pass.
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
            .iter()
            .filter(|(sid, w)| wanted.contains_key(*sid) && !w.unfollowed)
            .filter_map(|(sid, _)| kept.get(sid).map(|k| (sid.clone(), Arc::clone(k))))
            .collect()
    };
    host.followed
        .retain(|sid, _| host.workers.0.contains_key(sid));
    host.settle
        .retain(|sid, _| following.iter().any(|(followed, _)| followed == sid));
    let (settle_at, faulted) = follow_entries_caught(
        &following,
        stamps,
        (&mut host.followed, &mut host.settle),
        follow_all,
        now,
        hooks,
    );
    for sid in faulted {
        if let Some(w) = host.workers.0.get_mut(&sid) {
            w.unfollowed = true;
        }
    }
    let passive_at = repair_passive_snapshots(shared, host, wanted, now, hooks);
    [settle_at, passive_at].into_iter().flatten().min()
}

/// Failed passive reads are paced independently of screen frames; a new primary is read at once.
const PASSIVE_SNAPSHOT_RETRY: Duration = Duration::from_secs(2);

/// Passive Codex workers have no host idle step to repair an early missing or unnamed snapshot.
fn repair_passive_snapshots(
    shared: &Shared,
    host: &mut HostState,
    wanted: &HashMap<String, Program>,
    now: Instant,
    hooks: &Hooks,
) -> Option<Instant> {
    let mut next = None;
    for (sid, worker) in &mut host.workers.0 {
        if !watches_only(worker.agent)
            || !wanted.get(sid).copied().is_some_and(watches_only)
            || worker.unfollowed
            || worker.revoked.load(Ordering::SeqCst)
            || worker.stop.load(Ordering::SeqCst)
            || worker.done.load(Ordering::SeqCst)
        {
            worker.passive_read = None;
            continue;
        }
        let epoch = (hooks.claim_epoch)();
        let Some(primary) = PassivePrimary::current(sid, hooks) else {
            worker.passive_read = None;
            continue;
        };
        if published_snapshot(sid).is_some_and(|snap| primary.matches(sid, &snap)) {
            worker.passive_read = None;
            continue;
        }
        let at = match &worker.passive_read {
            Some((before, at)) if *before == primary && *at > now => *at,
            _ => {
                let owner = PassiveReadOwner {
                    primary: primary.clone(),
                    claim_epoch: epoch,
                    revoked: Arc::clone(&worker.revoked),
                    stop: Arc::clone(&worker.stop),
                    done: Arc::clone(&worker.done),
                    group: Arc::clone(&worker.group),
                    group_at: worker.group.load(Ordering::SeqCst),
                    switches: Arc::clone(&shared.switches),
                };
                start_point_read(sid, &worker.kept, hooks, Some(owner));
                let at = now + PASSIVE_SNAPSHOT_RETRY;
                worker.passive_read = Some((primary, at));
                at
            }
        };
        next = Some(next.map_or(at, |before: Instant| before.min(at)));
    }
    next
}

/// [`follow_entries_caught`]'s settle alone, for a test of what it follows.
#[cfg(test)]
fn follow_entries(
    following: &[(String, Arc<Mutex<Kept>>)],
    stamps: &HashMap<String, FollowStamp>,
    owed: (
        &mut HashMap<String, FollowStamp>,
        &mut HashMap<String, Instant>,
    ),
    follow_all: bool,
    now: Instant,
    hooks: &Hooks,
) -> Option<Instant> {
    follow_entries_caught(following, stamps, owed, follow_all, now, hooks).0
}

/// Select and follow from the roster-backed candidates.
fn follow_entries_caught(
    following: &[(String, Arc<Mutex<Kept>>)],
    stamps: &HashMap<String, FollowStamp>,
    (followed, settle): (
        &mut HashMap<String, FollowStamp>,
        &mut HashMap<String, Instant>,
    ),
    follow_all: bool,
    now: Instant,
    hooks: &Hooks,
) -> (Option<Instant>, Vec<String>) {
    let mut faulted = Vec::new();
    for (sid, kept) in following {
        let stamp = stamps.get(sid).copied();
        let before = followed.get(sid).copied();
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
        if settled {
            settle.remove(sid);
        }
        match follow_caught(sid, kept, hooks) {
            Some(true) => {}
            Some(false) => continue,
            None => {
                faulted.push(sid.clone());
                continue;
            }
        }
        if let Some(stamp) = stamp {
            followed.insert(sid.clone(), stamp);
        }
        if moved.is_some() || (before.is_none() && stamp.is_some_and(|stamp| stamp.screen > 0)) {
            settle.insert(sid.clone(), now + (hooks.pause)(STATUS_AFTER_SCREEN));
        }
    }
    (settle.values().min().copied(), faulted)
}

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
            || host_caught(sid, "the look at whether its agent holds the tab", || {
                (hooks.acts.holds)(sid, held.stamp.group)
            })
            .flatten()
                != Some(true)
        {
            return false;
        }
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

/// Each worker's step stamp as a pass reads the roster ([`WorkerIdle::stepped`]), read just before it.
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

    /// Whether worker `w` of session `sid` has been in a step at any time since the roster was read.
    fn stepping(&self, sid: &str, w: &Worker) -> bool {
        w.acting.load(Ordering::SeqCst)
            || self
                .0
                .get(sid)
                .is_none_or(|then| *then != w.stepped.load(Ordering::SeqCst))
    }
}

/// When the host looks again at an agent [`keep_holding`] kept, `look` the look it had set (`None`: kept just now).
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

const HOLDS_LOOK: Duration = relaunch::EXIT_SETTLE;

const HOLDS_LOOK_MAX: Duration = Duration::from_secs(2);

const HOLDS_KEEP_MAX: Duration = Duration::from_secs(30);

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
    let activations = shared.activations.read();
    let now = Instant::now();
    let mut next_look: Option<Instant> = None;
    for (sid, w) in &mut workers.0 {
        let look = tab_caught(shared, sid, "its worker's visit", hooks, || {
            let mut next: Option<Instant> = None;
            if !shared.switches.on_for(w.agent) {
                if !w.left.is(Word::Stopped) {
                    w.stop();
                }
                return None;
            }
            if off.contains(sid) {
                if !w.acting.load(Ordering::SeqCst) && !w.left.is(Word::Stopped) {
                    aterm_log::info!(
                        "harness @{sid}: autopilot is off; its supervisor stops, hands off the session"
                    );
                    w.stop();
                }
                return None;
            }
            if !wanted.contains_key(sid) {
                // An agent that left a tab that lives on (`keep_holding` kept every one
                // that still holds it) is the worker's to handle; a closed tab or a stopped
                // policy just stops it. Each is asked once: a stop rings the bell, so asking
                // at every pass would wake the host through the worker's whole wind-down. A
                // worker handed its exit, or whose agent came back, is still stopped when its
                // tab closes: the stop's word ends its relaunch's pause and says it is no exit.
                // A step that ran since the roster was read may be what the roster missed, so
                // the absence is judged by the pass the step's end rings (`StepsSeen`).
                if !steps.stepping(sid, w) {
                    if active && (hooks.acts.open)(sid) {
                        if !w.stop.load(Ordering::SeqCst) {
                            w.leave();
                        }
                    } else if !w.left.is(Word::Stopped) {
                        w.stop();
                    }
                }
                return None;
            }
            if !w.back(wanted[sid]) && w.cfg_gen != cfg_gen && !w.stop.load(Ordering::SeqCst) {
                w.hand_over();
            }
            if w.activations != activations.of(w.agent) {
                w.activations = activations.of(w.agent);
                w.park.store(true, Ordering::SeqCst);
                if shared.switches.upgrade() && upgrades(wanted[sid]) {
                    owe_note(&w.note, crate::upgrade_host::now_s());
                    told.insert(sid.clone());
                }
            }
            if shared.switches.upgrade()
                && let Some(at) = host_note(sid, &w.note, now, hooks)
            {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
            let mut look = w.look_at.lock().unwrap_or_else(PoisonError::into_inner);
            match *look {
                Some(at) if at <= now => {
                    *look = None;
                    w.park.store(true, Ordering::SeqCst);
                }
                Some(at) => next = Some(next.map_or(at, |n| n.min(at))),
                None => {}
            }
            next
        });
        if let Some(Some(at)) = look {
            next_look = Some(next_look.map_or(at, |n| n.min(at)));
        }
    }
    next_look
}

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
        activations: shared.activations.read(),
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
        let _ = tab_caught(shared, &sid, "its worker's start", hooks, || {
            start_worker(shared, host, &ctx, &sid, agent, None, watch);
        });
    }
    for (sid, _, exit) in exits {
        if !shared.switches.on_for(exit.agent) {
            continue;
        }
        aterm_log::info!(
            "harness @{sid}: no worker watched its agent (aterm's update held the harness, a \
             changed [harness] policy was handing it to a new worker, or a relaunch landed it \
             before any pass named it), and the roster names no agent in the tab now; reading \
             its exit"
        );
        let watch = state.claim_grace.runs_until(&sid, now);
        let _ = tab_caught(
            shared,
            &sid,
            "the start of its exit's reader",
            hooks,
            || {
                start_worker(shared, host, &ctx, &sid, exit.agent, Some(exit), watch);
            },
        );
    }
    publish_workers(&mut state, &host.workers);
}

/// The sessions the pass knows of (every agent it wants, every worker, every session it keeps anything of) that are Off.
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

/// The host's live workers as it leaves them ([`State::live`]).
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
    epoch: u64,
    activations: ActivationCounts,
    hooks: &'a Hooks,
}

/// The exits no worker was handed ([`Kept::unwatched`], or an exit watch's that hands itself, read: [`ExitWatched::hands`]).
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
            let gone = !wanted.contains_key(&sid) && !workers.0.contains_key(&sid);
            let exit = with_kept_of(&kept, |k| {
                if off {
                    k.unwatched = None;
                    k.exit_watch = None;
                    if gone {
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
        .filter(|(sid, ..)| {
            tab_caught(
                shared,
                sid,
                "the look at whether its tab is open",
                hooks,
                || (hooks.acts.open)(sid),
            ) == Some(true)
        })
        .collect();
    exits.sort_by(|a, b| a.0.cmp(&b.0));
    exits
}

/// An agent that left a tab while a switched-off agent's is forgotten, as one that ended under Off is ([`unwatched_exits`]).
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
        let _ = tab_caught(
            shared,
            &sid,
            "its forget while its agent's switch is off",
            hooks,
            || {
                let Some(snapshot) = with_kept_of(&kept, |k| {
                    k.snapshot.as_ref().filter(|_| !pending(k)).cloned()
                }) else {
                    return;
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
            },
        );
    }
}

/// Start session `sid`'s worker for `agent` and record it in `host.workers`.
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
    let activations = activations.of(agent);
    host.followed.remove(sid);
    host.held.remove(sid);
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
    let (relaunch_word, unset_mark) = with_kept_of(&kept, |k| {
        k.unwatched = None;
        k.commit = Committed::No;
        if exit.is_some() {
            return (false, false);
        }
        k.owed = None;
        let asks = k.relaunches.asks().is_some();
        let cleared = k.relaunches.running(Instant::now()) == Say::Clear;
        let lost = lost_marked(k);
        (cleared, !cleared && !asks && !lost)
    });
    let park = Arc::new(AtomicBool::new(false));
    let look_at: Arc<Mutex<Option<Instant>>> = Arc::default();
    let acting: Arc<AtomicBool> = Arc::default();
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
        finished: Mutex::default(),
        note: Arc::clone(&note),
        clock_hold: Mutex::default(),
        net: Arc::clone(&shared.net),
        route: Mutex::default(),
        memory: Mutex::default(),
        points: Arc::clone(&points),
        settle_later: AtomicBool::new(false),
        tap: Mutex::default(),
        login_latches: Arc::clone(&shared.login_latches),
        lifted_off,
        beside: Arc::clone(&beside),
        told: AtomicU8::new(0),
        repo_probe: Default::default(),
    });
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
        stop,
        interrupt: Arc::default(),
        handover: Arc::default(),
        clear_badge: relaunch_word,
        unset_mark,
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
            unfollowed: false,
            passive_read: None,
        },
    );
    if exit.is_none() && upgrades(agent) {
        host.watches.told.insert(sid.to_string());
    }
}

/// Build [`SuperviseOpts::hosted_with`] for a worker, including its approval journal and task result path.
fn hosted_opts(cfg: &SupervisorConfig, sid: &str, state: Option<&Path>) -> SuperviseOpts {
    use aterm_agent::supervise::approvals;
    let ledger = state.and_then(|root| approvals::path_under(root, Some(sid)));
    SuperviseOpts {
        journal: ledger.as_deref().map(approvals::journal_beside),
        ledger,
        tasks: state.map(crate::task_record::tasks_dir_under),
        ..SuperviseOpts::hosted_with(cfg)
    }
}

/// Stop every worker (flag and cut), then join them within [`WORKER_JOIN_TIMEOUT`].
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

/// The supervised agent a session's publication says it is.
pub(crate) fn agent_of(publication: &crate::session_timeline::AgentPublication) -> Option<Program> {
    identified_agent(publication).filter(|p| p.harness_table())
}

/// The agent that owns the foreground prompt, including one whose reader is intentionally read-only.
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
            Program::Claude
            | Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse => true,
            Program::Generic => false,
        })
}

/// Every live session whose server-published program is an agent, read in-process.
fn roster_of(store: &Store) -> Vec<(String, Program, FollowStamp)> {
    let handles = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .snapshot();
    let roster: Vec<(String, Program, FollowStamp)> = handles
        .iter()
        .filter(|h| h.state != SessionState::Exited)
        .filter_map(|h| {
            let (agent, stamp) = {
                let tl = h
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let publication = tl.agent();
                (
                    agent_of(publication)?,
                    FollowStamp {
                        group: publication.program_pgid,
                        reader: publication.reader,
                        rev: publication.rev,
                        screen: publication.screen_moves,
                        vendor: false,
                    },
                )
            };
            let vendor = !codex_alab_runs(&h.ctx, agent, stamp.group, Probe::Read);
            Some((
                h.sid.as_str().to_string(),
                agent,
                FollowStamp { vendor, ..stamp },
            ))
        })
        .collect();
    let mut images = ALAB_IMAGES.lock().unwrap_or_else(PoisonError::into_inner);
    if images.len() > ALAB_IMAGES_KEPT {
        images.retain(|sid, _| roster.iter().any(|(named, _, _)| named == sid));
    }
    drop(images);
    roster
}

/// Whether the host admits a worker for `agent` as the roster read it with `stamp`.
const fn admits(agent: Program, stamp: FollowStamp) -> bool {
    match agent {
        Program::Codex => !stamp.vendor,
        Program::Claude
        | Program::Agy
        | Program::Muse
        | Program::Grok
        | Program::Kimi
        | Program::Generic => true,
    }
}

/// Whether [`codex_alab_runs`] may read a process image it has not read for the group yet ([`alab_image`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    /// The host's roster pass: read it, once per group.
    Read,
    /// Any other reader (the window's frames, the control thread): only what the host's pass read.
    Cached,
}

static ALAB_IMAGES: Mutex<BTreeMap<String, AlabImage>> = Mutex::new(BTreeMap::new());

/// One tab's read of its group leader's image ([`ALAB_IMAGES`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct AlabImage {
    group: i32,
    exe: Option<std::path::PathBuf>,
    alab: bool,
}

const ALAB_IMAGES_KEPT: usize = 256;

/// Whether tab `sid`'s group `group` leads with an ALab build.
fn alab_image(sid: &str, group: i32, probe: Probe) -> bool {
    alab_image_by(sid, group, probe, &|pid| {
        i32::try_from(pid).ok().and_then(aterm_uds::sudo::exe_path)
    })
}

/// [`alab_image`] with the kernel's image path read by `exe_of`.
fn alab_image_by(
    sid: &str,
    group: i32,
    probe: Probe,
    exe_of: &dyn Fn(u32) -> Option<std::path::PathBuf>,
) -> bool {
    let Some(pid) = u32::try_from(group).ok().filter(|pid| *pid > 0) else {
        return false;
    };
    let cached = ALAB_IMAGES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(sid)
        .cloned()
        .filter(|read| read.group == group);
    match probe {
        Probe::Cached => cached.is_some_and(|read| read.alab),
        Probe::Read => {
            let exe = exe_of(pid);
            if let Some(read) = cached.filter(|read| read.exe == exe) {
                return read.alab;
            }
            let alab = exe
                .as_deref()
                .is_some_and(aterm_agent::harness::codex_alab::alab_bytes);
            ALAB_IMAGES
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(sid.to_string(), AlabImage { group, exe, alab });
            alab
        }
    }
}

/// Whether `ctx`'s `agent`, leading group `group`, is one the host admits as codex-alab.
fn codex_alab_runs(ctx: &crate::SessionCtx, agent: Program, group: i32, probe: Probe) -> bool {
    match agent {
        Program::Codex => {}
        Program::Claude
        | Program::Agy
        | Program::Muse
        | Program::Grok
        | Program::Kimi
        | Program::Generic => {
            return true;
        }
    }
    let sid = ctx.self_id.as_str();
    ctx.meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .supervisor_proven(crate::metrics::now_us())
        || crate::self_claim::copy_of_tab(sid)
            .is_some_and(|copy| i64::from(copy.pid) == i64::from(group))
        || alab_image(sid, group, probe)
}

/// The agent the host supervises or watches in `ctx`'s session.
pub(crate) fn admitted(ctx: &crate::SessionCtx) -> Option<Program> {
    let (agent, group) = {
        let tl = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
        (agent_of(tl.agent()), tl.agent().program_pgid)
    };
    admitted_as(ctx, agent, group)
}

/// [`admitted`] for an `agent` (and its `group`) the caller read under the timeline's guard and
/// has let go of.
///
/// A proven self-claim admits its Codex before the sweep names the tab's program, since the proof
/// read the claimant leading the foreground group on an ALab build. Waiting for the sweep would
/// leave `status` printing `autopilot=-` at the claim and the worker deciding under Cruise in a
/// Land session.
pub(crate) fn admitted_as(
    ctx: &crate::SessionCtx,
    agent: Option<Program>,
    group: i32,
) -> Option<Program> {
    let agent = agent.or_else(|| {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .supervisor_proven(crate::metrics::now_us())
            .then_some(Program::Codex)
    });
    agent.filter(|agent| codex_alab_runs(ctx, *agent, group, Probe::Cached))
}

/// Whether `agent`'s worker watches only from birth: aterm supervises no Codex.
const fn watches_only(agent: Program) -> bool {
    match agent {
        Program::Codex => true,
        Program::Claude
        | Program::Agy
        | Program::Muse
        | Program::Grok
        | Program::Kimi
        | Program::Generic => false,
    }
}

const UNCLAIMED_CODEX_ALAB: &str = "codex-alab";

static CODEX_REPORTS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Tell the host which program's own OSC 7501 report tab `sid` shows (`None` while it shows
/// none). Only a Codex report files the tab in [`CODEX_REPORTS`]: Grok Build's own report is
/// shown in its tab too, and must not make the disk watch take that tab for a Codex.
pub(crate) fn note_codex_report(
    sid: &str,
    shown: Option<aterm_core::terminal::program_state::ProgramApp>,
) {
    use aterm_core::terminal::program_state::ProgramApp;
    let codex = match shown {
        Some(ProgramApp::Codex) => true,
        Some(ProgramApp::GrokBuild) | None => false,
    };
    let mut reports = CODEX_REPORTS.lock().unwrap_or_else(PoisonError::into_inner);
    if codex {
        reports.insert(sid.to_string());
    } else {
        reports.remove(sid);
    }
}

const CODEX_REPORTS_KEPT: usize = 256;

/// Every live session whose agent is Codex ([`identified_agent`], supervised or not).
fn codex_roster_of(store: &Store) -> Vec<String> {
    let handles = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .snapshot();
    let now = crate::metrics::now_us();
    let roster: Vec<String> = handles
        .iter()
        .filter(|h| h.state != SessionState::Exited)
        .filter(|h| {
            let codex = {
                let tl = h
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                match identified_agent(tl.agent()) {
                    Some(Program::Codex) => true,
                    Some(
                        Program::Claude
                        | Program::Agy
                        | Program::Muse
                        | Program::Grok
                        | Program::Kimi
                        | Program::Generic,
                    )
                    | None => false,
                }
            };
            codex
                || CODEX_REPORTS
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .contains(h.sid.as_str())
                || h.ctx
                    .meta
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .live_claim(now)
                    .is_some_and(|claim| claim.proven.is_some())
        })
        .map(|h| h.sid.as_str().to_string())
        .collect();
    let mut reports = CODEX_REPORTS.lock().unwrap_or_else(PoisonError::into_inner);
    if reports.len() > CODEX_REPORTS_KEPT {
        reports.retain(|sid| {
            handles
                .iter()
                .any(|h| h.state != SessionState::Exited && h.sid.as_str() == sid)
        });
    }
    drop(reports);
    roster
}

/// Every live session's own Autopilot word ([`Hooks::autopilot`]), read in-process.
fn words_of(store: &Store) -> HashMap<String, Autopilot> {
    let handles = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .snapshot();
    handles
        .iter()
        .filter(|h| h.state != SessionState::Exited)
        .filter_map(|h| {
            let unapplied_no_harness = h
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .unapplied_leader_no_harness();
            let word = if unapplied_no_harness {
                Some(Autopilot::Off)
            } else {
                h.ctx
                    .meta
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .autopilot_word()
            }?;
            Some((h.sid.as_str().to_string(), word))
        })
        .collect()
}

/// The supervised sessions `sids` as the occupancy board keys them.
fn board_sessions(store: &Store, sids: &[String]) -> Vec<(u64, String)> {
    let guard = store.read().unwrap_or_else(PoisonError::into_inner);
    sids.iter()
        .filter_map(|sid| {
            let h = guard.by_sid(&aterm_session::SessionId::new(sid))?;
            Some((h.local_id, sid.clone()))
        })
        .collect()
}

/// The process group `sid`'s program is published for (`None`: no such session, or none resolved yet).
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

/// [`Acts::offered`] over tab `sid`'s live context.
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

/// [`Acts::self_idle`], live.
fn self_idle_now(store: &Store, sid: &str, pid: u32, start: &str) -> bool {
    let ctx = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .map(|h| Arc::clone(&h.ctx));
    ctx.is_some_and(|ctx| crate::self_claim::idle_now(&ctx, pid, start))
}

fn self_copy_now(store: &Store, sid: &str) -> Option<self_claimed::Copy> {
    let ctx = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .map(|h| Arc::clone(&h.ctx))?;
    crate::self_claim::live_copy(&ctx)
}

/// [`Acts::launch`], live.
fn launch_of(store: &Store, sid: &str) -> Option<i64> {
    let h = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .cloned()?;
    let started = h.ctx.launch.started()?;
    (h.ctx.human_input.seq() == started.seq)
        .then(|| i64::try_from(started.at_ms).ok())
        .flatten()
}

/// Whether `sid`'s agent still holds its tab ([`Acts::holds`]).
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

/// Whether session `sid` is still its worker's after a failed run ([`Hooks::still_wanted`]).
fn still_wanted(store: &Store, sid: &str, group: i32) -> bool {
    still_supervisable(store, sid) || holds(store, sid, group) == Some(true)
}

/// Whether `sid` is still a live session of an agent the host supervises or watches ([`admitted`]: never a vendor Codex).
fn still_supervisable(store: &Store, sid: &str) -> bool {
    let ctx = {
        let guard = store.read().unwrap_or_else(PoisonError::into_inner);
        let Some(h) = guard.by_sid(&aterm_session::SessionId::new(sid)) else {
            return false;
        };
        if h.state == SessionState::Exited {
            return false;
        }
        Arc::clone(&h.ctx)
    };
    admitted(&ctx).is_some()
}

/// One run of the loop over this instance's own socket.
fn hosted_body(sock: &str, job: &WorkerJob) -> BodyEnd {
    let mut ctl = HostCtl::as_harness(Endpoint::Socket(sock.to_string()));
    ctl.admission = Some((Arc::clone(&job.switches), job.agent));
    ctl.watch_only = job.watch_beside();
    ctl.fence = Some(holder());
    *job.interrupt.lock().unwrap_or_else(PoisonError::into_inner) = ctl.interrupter();
    if job.stop.load(Ordering::SeqCst) {
        return BodyEnd::Stopped;
    }
    let sel = format!("@{}", job.sid);
    if (job.clear_badge && !with_kept(job, |k| lost_marked(k))) || job.unset_mark {
        let owner = format!("owner={RELAUNCH_ATTENTION_OWNER}");
        if let Err(e) = ctl.call(&[&sel, "meta", "unset", "attention", &owner]) {
            return held_or_failed(e);
        }
    }
    let mut out = LogLines {
        sid: job.sid.clone(),
        buf: Vec::new(),
    };
    let mut session = Session::new(&mut ctl, Some(sel));
    session.set_supervisor_name(Some(holder()));
    session.type_under_hand(true);
    session.set_handover(Arc::clone(&job.handover));
    session.set_beside(job.watch_beside());
    if tape::seam_on(aterm_types::dev_seam!(tape::HOSTED_SEAM).as_deref()) {
        let setup = session.setup().clone();
        drop(session);
        let warn = Box::new(LogLines {
            sid: job.sid.clone(),
            buf: Vec::new(),
        });
        return body_end(tape::run_hosted_taped(
            &mut ctl,
            setup,
            &job.opts,
            Arc::clone(&job.stop),
            &mut out,
            warn,
            || stopped_by_commit(job),
        ));
    }
    body_end(session.run_hosted(&job.opts, Arc::clone(&job.stop), &mut out))
}

/// Whether the run of `job` that just returned was stopped by a seamless update's Commit
/// ([`HostHandle::suspend`] marks the worker so, and nothing has decided or rolled back since).
fn stopped_by_commit(job: &WorkerJob) -> bool {
    with_kept(job, |k| k.commit == Committed::Stopped)
}

/// What one run of the loop came to.
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

/// Set (`Some`) or clear the session's relaunch word, its `owner=relaunch` attention ([`RELAUNCH_ATTENTION_OWNER`]).
fn set_badge(sock: &str, sid: &str, text: Option<&str>) {
    let mut ctl = RelayCtl::new(Endpoint::Socket(sock.to_string()), None);
    let sel = format!("@{sid}");
    let owner = format!("owner={RELAUNCH_ATTENTION_OWNER}");
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

/// The token the window's drive presents on this instance's own socket, so what it types reads
/// `by=harness` ([`upgrade_drive::harness_client`]).
pub(crate) fn harness_drive_token() -> Option<upgrade_drive::HarnessToken> {
    crate::control_auth::harness_token().map(upgrade_drive::HarnessToken::new)
}

/// What every one of the window's sweeps over `home` and `state` is told: this instance's own
/// socket `sock` with the harness's token ([`harness_drive_token`]), and the window's own
/// `[harness]` switches. [`live_acts`], [`live_view`] and [`crate::upgrade_host::word_writer_at`]
/// each start from it, so none dials the socket without the token.
pub(crate) fn window_drive_opts(
    home: std::path::PathBuf,
    state: std::path::PathBuf,
    sock: Option<String>,
) -> upgrade_drive::Opts {
    window_drive_opts_with_tap(home, state, sock, false)
}

/// Capture the model-evidence policy and optional tap from one recovered roster.
/// Only the acting lane enables the tap; views and recorded requests leave it off.
fn window_drive_opts_with_tap(
    home: std::path::PathBuf,
    state: std::path::PathBuf,
    sock: Option<String>,
    tap_on: bool,
) -> upgrade_drive::Opts {
    let accounts = tap::LaunchPolicy::read(
        tap_on,
        tap::stable_exe,
        accounts::default_roster_path().as_deref(),
        &accounts::Store::at(&state),
        aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()),
    );
    upgrade_drive::Opts {
        home,
        state,
        sock,
        token: harness_drive_token(),
        only_sid: None,
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: aterm_types::dirs::state_dir(),
        ask: upgrade_ask(),
        several_logins: accounts.several_logins,
        usage_credits: usage_credits(),
        usage_tap: accounts.usage_tap,
        instances: upgrade_drive::Instances::Machine,
        agents_off: Vec::new(),
        land: false,
        timings: aterm_agent::harness::Timings::for_build(),
    }
}

/// The worker's acts over this instance's own socket.
fn live_acts(store: Store, sock: String) -> Acts {
    let task_sock = sock.clone();
    let startups_store = store.clone();
    let startup_store = store.clone();
    let fact_store = store.clone();
    let resume_sock = sock.clone();
    let disk_store = store.clone();
    let free_sock = sock.clone();
    let free_store = store.clone();
    let home = aterm_primer::home_dir();
    let place = home.clone().zip(harness_state());
    let exit_home = home.clone();
    let limit_home = home.clone();
    let account_home = home.clone();
    let upkeep = live_accounts_upkeep();
    let wall_upkeep = upkeep.clone();
    let usage_home = home.clone();
    let seat_home = home.clone();
    let cold_home = home.clone();
    let repo_home = home.clone();
    let limit_read: Mutex<HashMap<String, (Instant, Option<u64>)>> = Mutex::default();
    let fg_store = store.clone();
    let holds_store = store.clone();
    let launch_store = store.clone();
    let carry_store = store.clone();
    let offered_store = store.clone();
    let idle_store = store.clone();
    let snapshot_store = store.clone();
    let primary_store = store.clone();
    let row = live_row(store.clone());
    let [w1, w2, w3, w4, w5, w6, w7, w8, w9, w10] = std::array::from_fn(|_| Arc::clone(&row));
    let opts = move |sid: &str, grace: u32| {
        place.clone().map(|(home, state)| upgrade_drive::Opts {
            only_sid: Some(sid.to_string()),
            human_grace_s: grace,
            agents_off: AGENTS_OFF
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            ..window_drive_opts_with_tap(home, state, Some(sock.clone()), usage_tap_on())
        })
    };
    let opts = Arc::new(opts);
    let standing_opts = Arc::clone(&opts);
    let o_finished = Arc::clone(&opts);
    let o_earlier = Arc::clone(&opts);
    let o_adopted = Arc::clone(&opts);
    let o_in_flight = Arc::clone(&opts);
    let [o1, o2, o3, o4, o5, o6, o7, o8, o9, o10] = std::array::from_fn(|_| Arc::clone(&opts));
    let [o11, o12, o13, o14, o15, o16, o17, o18, o19, o20] =
        std::array::from_fn(|_| Arc::clone(&opts));
    let o_self = Arc::clone(&opts);
    let o_snap = Arc::clone(&opts);
    let o_settle = Arc::clone(&opts);
    Acts {
        open: Arc::new(move |sid| open(&store, sid)),
        holds: Arc::new(move |sid, group| holds(&holds_store, sid, group)),
        launch: Arc::new(move |sid| launch_of(&launch_store, sid)),
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
                w1(sid, "upgrade", &r);
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
                w2(sid, "upgrade-at-break", &r);
            }
            r.step
        }),
        owed: Arc::new(move |sid, only| o6(sid, 0).is_some_and(|o| relaunch::owed_of(&o, only))),
        disk_resume: Arc::new(move |sid, text| {
            disk_resumed(
                &disk_store,
                sid,
                aterm_agent::harness::disk::type_resume(
                    &resume_sock,
                    harness_drive_token().as_ref(),
                    sid,
                    text,
                ),
            )
        }),
        disk_free: Arc::new(move |sid, owed, unfit, held| {
            disk_ask::act(&free_store, &free_sock, sid, owed, unfit, held)
        }),
        disk_board: &crate::disk_board::SHELF,
        jobs_board: &crate::vitals_jobs::BOARD,
        leader_lives: crate::vitals_jobs::leader_lives,
        program_check: &crate::program_check_watch::BOARD,
        occupancy: Arc::new(crate::occupancy_host::reading_for),
        occupancy_pace: Arc::new(crate::occupancy_host::pace_for),
        task_fact: Arc::new(move |sid, word| task_fact_live(&fact_store, sid, word)),
        wakes: Arc::new(crate::occupancy_wakes::seen_for),
        wake_told: Arc::new(crate::occupancy_wakes::told_for),
        wake_claim: Arc::new(crate::occupancy_wakes::claim_for),
        wake_release: Arc::new(crate::occupancy_wakes::release_for),
        wakes_left: Arc::new(crate::occupancy_wakes::left),
        resume: Arc::new(move |sid, grace, carry| {
            let Some(o) = o7(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::resume_as(
                &upgrade_drive::Opts {
                    land: carry.land,
                    ..o
                },
                carry.done_under_land,
                carry.only,
            );
            if r.is_act() {
                w3(sid, "carry-on", &r);
            }
            r.step
        }),
        settle: Arc::new(move |sid| {
            let Some(o) = o_settle(sid, 0) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::settle(&o);
            if r.is_act() {
                w4(sid, "carry-on", &r);
            }
            r.step
        }),
        snapshot: Arc::new(move |sid| {
            let (read, took) = laps::within(|| {
                o3(sid, 0).map(|o| {
                    let copy = self_copy_now(&snapshot_store, sid);
                    relaunch::snapshot_with_claim(&o, copy.as_ref())
                })
            });
            if took.total() >= SLOW_STEP {
                aterm_log::info!(
                    "harness @{sid}: a relaunch snapshot read took {} ms ({took})",
                    took.total().as_millis()
                );
            }
            match read? {
                Ok(snap) => {
                    snapshot_refused(sid, None);
                    Some(snap)
                }
                Err(why) => {
                    snapshot_refused(sid, Some(why));
                    None
                }
            }
        }),
        point_read: POINT_READ_BOUND,
        follow: Arc::new(move |sid, snap| {
            let Some(fg) = foreground_of(&fg_store, sid) else {
                return Foreground::Shell;
            };
            let at = relaunch::foreground(snap, fg);
            if at == Foreground::Agent
                && let Some(home) = home.as_deref()
            {
                relaunch::follow(home, snap);
            }
            if at == Foreground::Agent {
                relaunch::follow_muse(snap);
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
        exit_look: Arc::new(move |sid, snap| match &snap.run {
            relaunch::AgentRun::Codex(_) => None,
            relaunch::AgentRun::Muse(_) => {
                o13(sid, 0).map(|o| relaunch::look_at_muse_exit(&o, snap))
            }
            relaunch::AgentRun::Agy(_) => o20(sid, 0).map(|o| relaunch::look_at_agy_exit(&o, snap)),
            relaunch::AgentRun::Claude => exit_home
                .as_deref()
                .map(|home| relaunch::look_at_exit(home, snap)),
        }),
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
        live_self_copy: Arc::new(move |sid| self_copy_now(&primary_store, sid)),
        self_idle: Arc::new(move |sid, pid, start| self_idle_now(&idle_store, sid, pid, start)),
        self_relaunch: Arc::new(move |sid, grace, snap, resume, asked| {
            let Some(o) = o_self(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = upgrade_drive::self_claimed_after_exit(&o, snap, resume, asked);
            w5(sid, "self-claimed-relaunch", &r);
            r.step
        }),
        self_snapshot: Arc::new(move |sid, copy, resume| {
            let o = o_snap(sid, 0)?;
            match relaunch::self_claimed_snapshot(&o, copy, resume) {
                Ok(snap) => Some(snap),
                Err(why) => {
                    aterm_log::info!(
                        "harness @{sid}: no shell holds the tab at the crashed codex-alab's exit \
                         ({why})"
                    );
                    None
                }
            }
        }),
        relaunch: Arc::new(move |sid, grace, snap, left, upgrade, asked| {
            let Some(o) = o5(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::after_exit(&o, snap, left, upgrade, asked);
            w6(sid, "relaunch", &r);
            r.step
        }),
        restarted: Arc::new(move |sid, agent, pid| {
            o15(sid, 0).is_some_and(|o| relaunch::restarted(&o, agent, pid))
        }),
        carry: Arc::new(move |sid, grace, agent, pid, through_hand| {
            let Some(o) = o16(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::carry_restart(&o, agent, pid, through_hand);
            if r.step != relaunch::WAIT_HAND_LIFTS {
                w7(sid, "carry-restart", &r);
            }
            r.step
        }),
        relaunch_restored: Arc::new(move |sid, snap, upgrade, was| {
            let Some(o) = o14(sid, 0) else {
                return "refused:no-home".to_string();
            };
            let r = relaunch::after_host_ended_from(&o, snap, upgrade, was);
            w8(sid, RESTORED_LINE, &r);
            r.step
        }),
        earlier_tab: Arc::new(move |sid, was| {
            if let Some(o) = o_earlier(sid, 0) {
                upgrade_drive::note_earlier_tab(&o, sid, was);
            }
        }),
        adopted_gone: Arc::new(move |sid, snap| {
            let o = o_adopted(sid, 0)?;
            relaunch::adopted_gone(&o, sid, snap.session.as_deref()?)
        }),
        in_flight_here: Arc::new(move |sid, snap| {
            o_in_flight(sid, 0).is_some_and(|o| {
                snap.session
                    .as_deref()
                    .is_some_and(|session| relaunch::in_flight_here(&o, sid, session))
            })
        }),
        carry_in_flight: Arc::new(move |sid, grace| {
            let o = o17(sid, grace)?;
            carry_in_flight_now(
                || upgrade_drive::in_flight_agent(&o),
                || still_supervisable(&carry_store, sid),
                || {
                    let r = upgrade_drive::step(&o);
                    w9(sid, "carry-on-after-update", &r);
                    r.step
                },
            )
        }),
        restart: Arc::new(move |sid, grace, why| {
            let Some(o) = o8(sid, grace) else {
                return "refused:no-home".to_string();
            };
            let r = match why {
                Restart::Account { .. } => {
                    let roster = match (harness_state(), aterm_time::unix_secs()) {
                        (Some(state), Some(now)) => match read_account_roster(
                            accounts::default_roster_path().as_deref(),
                            &accounts::Store::at(&state),
                            i64::try_from(now).unwrap_or(i64::MAX),
                        ) {
                            Ok(roster) => roster,
                            Err(_) => return "wait:account-state".to_string(),
                        },
                        _ => return "wait:account-state".to_string(),
                    };
                    relaunch::restart_under(&o, why, &roster)
                }
                // Every other restart re-applies the login its process had; the tap's logins
                // ride in `o.usage_tap` (`tap::Armed::lone_for`), as every relaunch line's do.
                _ => relaunch::restart_here(&o, why),
            };
            w10(sid, &format!("restart:{}", why.word()), &r);
            r.step
        }),
        tasked: Arc::new(move |sid, snap, memo| {
            let o = o10(sid, 0)?;
            memo.of_snapshot(&o.home, snap, &conversation_typed(&o, sid, snap))
        }),
        finished: Arc::new(move |sid, snap, memo| {
            let o = o_finished(sid, 0)?;
            let finished = memo.of_snapshot(&o.home, snap, &conversation_typed(&o, sid, snap));
            if let Some(dir) = memo.dir() {
                upgrade_drive::note_writes(&o, snap, dir);
            }
            finished
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
        repo_work: Arc::new(move |snap| {
            let tail = upgrade_drive::snapshot_transcript_tail(snap);
            aterm_agent::harness::repo_work::session_dir(&snap.cwd, tail.as_deref())
                .map_or_else(Default::default, |dir| {
                    session_repo_work(&dir, repo_home.as_deref())
                })
        }),
        transcript: Arc::new(upgrade_drive::snapshot_transcript),
        task_exited: Arc::new(move |sid| task_exited_live(&task_sock, sid)),
        startups: Arc::new(move |judged| startups_of(&startups_store, judged)),
        startup_mark: Arc::new(move |sid, mark| {
            startup_mark_live(&startup_store, sid, mark);
        }),
        standing_limit: Arc::new(move |sid, snap| {
            let tail = upgrade_drive::snapshot_transcript_tail(snap)?;
            relaunch::standing_limit(&standing_opts(sid, 0)?, snap, &tail)
        }),
        account_wall: Arc::new(move |sid, agent, settled| {
            account_wall_live(account_home.as_deref(), sid, agent, settled, &wall_upkeep)
        }),
        accounts_upkeep: upkeep,
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

/// The production [`Acts::account_wall`], read between the accounts' upkeep's sync and its renewal ([`walled_between_upkeeps`]).
fn account_wall_live(
    home: Option<&Path>,
    sid: &str,
    agent: &WalledAgent<'_>,
    settled: &dyn Fn() -> bool,
    upkeep: &AccountsUpkeep,
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
    walled_between_upkeeps(&|step| upkeep.make(step), || {
        account_wall_at(
            (home, &accounts::Store::at(&state), roster.as_deref()),
            sid,
            agent,
            settled,
            now,
        )
    })
}

/// A session's `wall`, recorded between the accounts' upkeep's sync and its renewal.
fn walled_between_upkeeps<T>(upkeep: &dyn Fn(UpkeepStep), wall: impl FnOnce() -> T) -> T {
    upkeep(UpkeepStep::Sync);
    let read = wall();
    upkeep(UpkeepStep::Renew);
    read
}

/// [`account_wall_live`] over the places it is given (`(home, account store, roster file)`,
/// the roster `None` where no config path is known) and the caller's clock.
fn account_wall_at(
    (home, store, roster): (&Path, &accounts::Store, Option<&Path>),
    sid: &str,
    agent: &WalledAgent<'_>,
    settled: &dyn Fn() -> bool,
    now: i64,
) -> (&'static str, Option<String>) {
    let roster = match read_account_roster(roster, store, now) {
        Ok(roster) => roster,
        Err(why) => return (why.recording_word(), None),
    };
    let roster = match roster {
        roster if roster.rows.is_empty() && agent.own => accounts::Roster::own(),
        roster if roster.rows.is_empty() => return ("not-listed", None),
        roster => roster,
    };
    let Some(args) = atpkg::caller_shell::process_args(agent.snap.pid) else {
        return ("no-env", None);
    };
    let who = accounts::ProcessLogin::of_env(args.env.iter().filter_map(|kv| kv.split_once('=')));
    let read = WallRead {
        env: &args.env,
        settled: accounts::judged_label(&roster, &who).is_some() && settled(),
        wall: upgrade_drive::named_standing_wall(home, agent.snap),
        started: process_started(&agent.snap.start),
    };
    account_wall_of(store, &roster, sid, &read, now)
}

/// The unix second a process started, from its kernel start as the snapshot keeps it ([`Snapshot::start`]: `ps -o lstart=` in UTC).
fn process_started(start: &str) -> Option<i64> {
    aterm_agent::harness::upgrade_models::parse_lstart(start).and_then(|s| i64::try_from(s).ok())
}

/// What [`account_wall_live`] read of a session's agent at its wall, for [`account_wall_of`].
struct WallRead<'a> {
    env: &'a [String],
    settled: bool,
    wall: Option<aterm_agent::harness::upgrade::StandingWall>,
    started: Option<i64>,
}

/// The recorder's call ([`account_wall_live`]) over what was read ([`WallRead`]).
fn account_wall_of(
    store: &accounts::Store,
    roster: &accounts::Roster,
    sid: &str,
    read: &WallRead<'_>,
    now: i64,
) -> (&'static str, Option<String>) {
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

/// What a session working in `dir` holds that no remote main has (`IdleHost::repo_work`, [`aterm_agent::harness::repo_work::probe`]).
pub(crate) fn session_repo_work(
    dir: &std::path::Path,
    home: Option<&std::path::Path>,
) -> aterm_agent::harness::repo_work::RepoRead {
    session_repo_work_under(dir, home, aterm_types::dirs::state_dir().as_deref())
}

/// [`session_repo_work`] under the state root `state`.
pub(crate) fn session_repo_work_under(
    dir: &std::path::Path,
    home: Option<&std::path::Path>,
    state: Option<&std::path::Path>,
) -> aterm_agent::harness::repo_work::RepoRead {
    use aterm_agent::harness::repo_work;
    if state.is_some_and(|state| crate::summarize::in_summary_folder_under(dir, state)) {
        return repo_work::RepoRead::Clean;
    }
    repo_work::probe(dir, home, repo_work::PROBE_LIMIT)
}

/// The product's [`Acts::task_exited`], over this instance's own socket ([`aterm_agent::supervise::mark_task_exited`]).
fn task_exited_live(sock: &str, sid: &str) {
    let mut ctl = RelayCtl::new(Endpoint::Socket(sock.to_string()), None);
    match aterm_agent::supervise::mark_task_exited(&mut ctl, &format!("@{sid}")) {
        Ok(Some(line)) => aterm_log::info!("harness @{sid}: task: {line}"),
        Ok(None) => {}
        Err(e) => aterm_log::warn!("harness @{sid}: task not marked exited: {e}"),
    }
}

/// The product's [`Acts::startups`]: every open session with a task record, read in-process.
fn startups_of(store: &Store, judged: &std::collections::HashSet<String>) -> Vec<Startup> {
    use crate::task_record::TaskState;
    let handles = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .snapshot();
    let ours = holder();
    handles
        .iter()
        .filter(|h| h.state != SessionState::Exited)
        .filter_map(|h| {
            let (standing, asked, carried, autopilot_off) = {
                let meta = h.ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
                (
                    meta.attention_owners
                        .get(ATTENTION_OWNER)
                        .map(str::to_string),
                    meta.attention_owners
                        .get(crate::task_record::SELF_CLAIM_ATTENTION_OWNER)
                        .map(str::to_string),
                    meta.supervisor_proven(crate::metrics::now_us()),
                    meta.autopilot_word() == Some(aterm_agent::supervise::Autopilot::Off),
                )
            };
            let claimable = !autopilot_off && h.ctx.fabric.hold().is_none();
            let (record, program, agent, mut held, installing) = {
                let tl = h
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let record = tl.task()?.clone();
                let installing = tl.worker_installing();
                let publication = tl.agent();
                let agent = agent_of(publication);
                let held = HeldRead {
                    word: publication.word.to_string(),
                    detail: publication.detail.clone(),
                    program: publication.program.clone(),
                    progress: Progress {
                        screen: publication.text_moves,
                        verdict: publication.verdict_moves,
                        input: 0,
                        transcript: tl.claude_excerpts().and_then(|e| e.transcript_len),
                    },
                    escalated: false,
                    last_row: None,
                    panel: false,
                    unread: false,
                    person: None,
                    driver: false,
                    carried,
                };
                (record, publication.program.clone(), agent, held, installing)
            };
            held.progress.input = h.ctx.output_echo.input_sent();
            held.person = h
                .ctx
                .human_input
                .ms_since(crate::metrics::now_us())
                .and_then(|ms| Instant::now().checked_sub(Duration::from_millis(ms)));
            held.driver = driver_holds(
                h.ctx
                    .turn_lease
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref(),
                crate::metrics::now_us(),
                &ours,
            );
            let deadlines = deadline_attention_stands(standing.as_deref());
            held.escalated = standing.is_some() && !deadlines;
            let sid = h.sid.as_str().to_string();
            let state = match (record.state, record.why) {
                (TaskState::Stalled, Some("startup")) => StartupState::StalledAtStartup,
                (TaskState::Stalled, Some("held")) => StartupState::StalledHeld,
                _ if deadlines => StartupState::StallOver,
                (TaskState::Working, None) => StartupState::Working,
                (TaskState::Working, Some("turn" | "moved")) => StartupState::Reopened,
                _ => StartupState::Other,
            };
            let rows_of = |t: &aterm_core::terminal::Terminal| -> Option<Vec<String>> {
                (!crate::frame_unfinished_now(t)).then(|| {
                    (0..t.rows() as usize)
                        .map(|r| crate::control::visible_row(t, r))
                        .collect()
                })
            };
            let for_start = state == StartupState::StalledAtStartup
                || (state == StartupState::Working && !judged.contains(&sid));
            let for_held = matches!(state, StartupState::Working | StartupState::Reopened)
                && held.may_be_held();
            // A proven self-claim is a start: its program read the tab and carries it.
            let rows = ((for_start && !carried) || for_held)
                .then(|| match h.term.try_lock() {
                    Ok(t) => rows_of(&t),
                    Err(std::sync::TryLockError::Poisoned(p)) => rows_of(&p.into_inner()),
                    Err(std::sync::TryLockError::WouldBlock) => None,
                })
                .flatten();
            let seen = match (&rows, for_start) {
                _ if for_start && carried => StartupSeen::Started,
                (Some(rows), true) => unclaimed(
                    startup_seen(rows, program.as_deref(), agent, record.agent),
                    record.agent,
                    claimable,
                    // codex-alab's words name only a screen codex holds: another agent in
                    // front (OpenAI's codex aside) is that agent's screen.
                    asked
                        .as_deref()
                        .filter(|_| agent.is_none_or(codex_alab_names_screen)),
                ),
                _ => StartupSeen::Unread,
            };
            if for_held {
                held.unread = rows.is_none();
                held.last_row = rows.as_ref().and_then(|rows| {
                    rows.iter()
                        .rev()
                        .map(|row| row.trim())
                        .find(|row| !row.is_empty())
                        .map(str::to_string)
                });
                held.panel = rows
                    .as_deref()
                    .is_some_and(|rows| aterm_phase::settings_panel(rows).is_some());
            }
            let held = matches!(
                state,
                StartupState::Working | StartupState::Reopened | StartupState::StalledHeld
            )
            .then_some(held);
            Some(Startup {
                sid,
                state,
                since: record.since,
                seen,
                held,
                standing: standing.filter(|s| s.starts_with(STARTUP_HEAD)),
                installing,
            })
        })
        .collect()
}

/// The product's [`Acts::startup_mark`].
fn startup_mark_live(store: &Store, sid: &str, mark: &StartupMark<'_>) {
    if let Some((local, marked)) = startup_mark_with(store, sid, mark)
        && marked.attention_moved
    {
        crate::presence::post_meta_changed(local);
    }
}

/// The task door's fact word on session `sid`'s record ([`Acts::task_fact`]): `NoTask` where no
/// session of this instance has that sid, else [`crate::task_record::mark_session_task_fact_if`]'s
/// outcome, waiting on a committing update's fence for at most the fabric's own wait. The
/// owner's mode is read again under the task lock immediately before the edit.
fn task_fact_live(
    store: &Store,
    sid: &str,
    word: Option<aterm_agent::supervise::occupancy::FactWord>,
) -> aterm_agent::supervise::occupancy::NoteOutcome {
    let Some(h) = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .cloned()
    else {
        return aterm_agent::supervise::occupancy::NoteOutcome::NoTask;
    };
    let fence = h.ctx.fabric.fence_wait();
    crate::task_record::mark_session_task_fact_if(
        &h.ctx,
        h.local_id,
        word,
        fence,
        crate::occupancy_host::prompt_now,
    )
}

/// The deadlines' marks, made in-process and conditional on the record still reading what the look
/// read (`task_record::mark_session_task_with`, read under the locks the mark takes).
pub(crate) fn startup_mark_with(
    store: &Store,
    sid: &str,
    mark: &StartupMark<'_>,
) -> Option<(u64, crate::task_record::MarkedWith)> {
    use crate::task_record::{
        AttentionEdit, TaskMark, TaskRecord, TaskState, mark_session_task_with,
    };
    let h = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .cloned()?;
    let said = match mark {
        StartupMark::Stalled { hold } | StartupMark::Restated { hold } => hold.attention(),
        StartupMark::Held { read, held_for } => held_attention(read, *held_for),
        StartupMark::Started => "started after all".to_string(),
        StartupMark::Moved => "it moved".to_string(),
        StartupMark::Over => "the stall is over: its attention taken down".to_string(),
    };
    let stalled = |why: &'static str| TaskMark {
        why: Some(why),
        ..TaskMark::new(TaskState::Stalled)
    };
    let take_down = AttentionEdit::TakeDown(deadline_attention_stands);
    type When = fn(&TaskRecord, Option<&str>) -> bool;
    let (next, edit, when): (Option<TaskMark>, AttentionEdit<'_>, When) = match mark {
        StartupMark::Stalled { .. } => (
            Some(stalled("startup")),
            AttentionEdit::Raise(&said),
            |r, _| r.state == TaskState::Working && r.why.is_none(),
        ),
        StartupMark::Held { .. } => (
            Some(stalled("held")),
            AttentionEdit::Raise(&said),
            |r, standing| {
                r.state == TaskState::Working
                    && (standing.is_none() || deadline_attention_stands(standing))
            },
        ),
        StartupMark::Started => (
            Some(TaskMark::new(TaskState::Working)),
            take_down,
            |r, _| r.state == TaskState::Stalled && r.why == Some("startup"),
        ),
        StartupMark::Moved => (
            Some(TaskMark {
                why: Some("moved"),
                ..TaskMark::new(TaskState::Working)
            }),
            take_down,
            |r, _| r.state == TaskState::Stalled && r.why == Some("held"),
        ),
        StartupMark::Over => (None, take_down, |r, _| {
            !(r.state == TaskState::Stalled && matches!(r.why, Some("startup" | "held")))
        }),
        StartupMark::Restated { .. } => (None, AttentionEdit::Raise(&said), |r, standing| {
            r.state == TaskState::Stalled
                && r.why == Some("startup")
                && standing.is_some_and(|s| s.starts_with(STARTUP_HEAD))
        }),
    };
    let fence = h.ctx.fabric.fence_wait();
    let marked = match mark_session_task_with(
        &h.ctx,
        h.local_id,
        next.as_ref(),
        edit,
        fence,
        |r, standing| r.is_some_and(|r| when(r, standing)),
    ) {
        Ok(marked) => marked,
        Err(_) => {
            aterm_log::warn!("harness @{sid}: task not marked: an update is committing");
            return None;
        }
    };
    match (&marked, mark) {
        (Some(m), StartupMark::Over) if m.attention_moved => {
            aterm_log::info!("harness @{sid}: task: {said}");
        }
        (Some(_), StartupMark::Over) | (None, StartupMark::Over | StartupMark::Restated { .. }) => {
            return None;
        }
        (Some(m), _) => {
            let (rev, state, why) = m
                .record
                .as_ref()
                .map_or((0, "-", None), |r| (r.rev, r.state.as_str(), r.why));
            let why = why.map_or_else(String::new, |w| format!(" why={w}"));
            let launch = match mark {
                StartupMark::Stalled { .. } => launch_words(&h.ctx),
                _ => String::new(),
            };
            aterm_log::info!("harness @{sid}: task: rev={rev} state={state}{why}: {said}{launch}");
        }
        (None, _) => {
            aterm_log::info!("harness @{sid}: task not marked: it moved on");
            return None;
        }
    }
    marked.map(|m| (h.local_id, m))
}

/// What a worker's launcher did with its line, for the startup stall's log line (`SessionTimeline::worker_launch`).
pub(crate) fn launch_words(ctx: &crate::SessionCtx) -> String {
    let tl = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
    match tl.worker_launch() {
        Some((at, words)) => format!(
            "; its launch line was {words}, {:.1} s before",
            at.elapsed().as_secs_f64()
        ),
        None => "; its launch line was never typed by this process (its launcher never \
                 reached the shell's first prompt here, or another process started it)"
            .to_string(),
    }
}

/// The production [`Acts::account_usage`].
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

/// What `seen` says, as true at `at` in the zone `offset` seconds east of UTC.
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

/// The roster a session's usage is recorded against ([`account_usage_live`]), or the word for why there is none.
fn usage_roster(
    path: Option<&Path>,
    store: &accounts::Store,
    (own, listed): (bool, bool),
    now: i64,
) -> Result<accounts::Roster, &'static str> {
    match read_account_roster(path, store, now).map_err(RosterUnread::recording_word)? {
        roster if !roster.rows.is_empty() => {
            if !listed {
                return Err("off");
            }
            Ok(roster)
        }
        _ if own => Ok(accounts::Roster::own()),
        _ => Err("not-listed"),
    }
}

/// What [`account_usage_live`] read of a session's agent and its screen.
struct UsageRead<'a> {
    env: &'a [String],
    settled: &'a dyn Fn() -> bool,
    answered: &'a dyn Fn(i64) -> Option<i64>,
    said: &'a dyn Fn(i64) -> accounts::UsageSaid,
}

/// The usage recorder's call, assembled ([`account_usage_live`]).
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

/// The production [`Acts::account_gone`].
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

/// Where the accounts live for this host, and its clock ([`Acts::account_seat`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountSeat {
    /// The roster's file, beside the `aterm.toml` the window loads.
    pub(crate) roster: Option<PathBuf>,
    /// The harness state root the accounts store is under (`accounts::Store::at`).
    pub(crate) state: PathBuf,
    /// The user's home: a session's standing wall and reset line are read from its transcript in its agent's own records.
    pub(crate) home: PathBuf,
    /// Now, unix seconds.
    pub(crate) now: i64,
    /// The bounded wait for the store's lock in every take this host makes of it ([`account_transact_of`], [`login_transact_of`]).
    pub(crate) ladder: &'static [std::time::Duration],
}

/// The production [`Acts::account_seat`].
fn account_seat_live(home: Option<&Path>) -> Option<AccountSeat> {
    Some(AccountSeat {
        roster: accounts::default_roster_path(),
        state: harness_state()?,
        home: home?.to_path_buf(),
        now: aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok())?,
        ladder: &accounts::LOCK_LADDER,
    })
}

/// Whether the process an account move's relaunch brought up runs under the target ([`WorkerIdle::account_applied`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Applied {
    /// Its own environment names the target.
    Yes,
    /// Its environment was read and names another login, or none of the roster's: the move was not applied.
    No,
    /// Nothing of it could be read: nobody knows, and nothing is taken back.
    Unread,
}

/// What the exec environment of process `pid` says of its login ([`Acts::account_env`], one read; `accounts::ProcessLogin::of_exec_env`).
fn exec_login(hooks: &Hooks, pid: u32) -> Option<accounts::ProcessLogin> {
    (hooks.acts.account_env)(pid)
        .as_deref()
        .and_then(accounts::ProcessLogin::of_exec_env)
}

/// Run one bounded transaction on the accounts state for session `sid` (`accounts::Store::transact_within`).
fn account_transact_of(
    sid: &str,
    hooks: &Hooks,
    seat: &AccountSeat,
    edit: impl FnOnce(&mut accounts::State, i64) -> Result<Vec<accounts::Event>, String>,
) -> Result<Vec<accounts::Event>, String> {
    let now = (hooks.acts.account_seat)().map_or(seat.now, |seat| seat.now);
    let mut note = None;
    accounts::Store::at(&seat.state).transact_within(seat.ladder, now, &mut note, |state| {
        Ok(edit(state, now)?
            .into_iter()
            .map(|event| match event.sid {
                Some(_) => event,
                None => event.with_sid(sid),
            })
            .collect())
    })
}

/// [`account_transact_of`] for a current login observation: the raw roster and login binding
/// are checked again under the state transaction's lock before its edit runs.
fn login_transact_of(
    sid: &str,
    hooks: &Hooks,
    seat: &AccountSeat,
    roster: &accounts::Roster,
    look: &AccountLook,
    label: &str,
    edit: impl FnOnce(
        &mut accounts::State,
        &mut accounts::Logins,
        i64,
    ) -> Result<Vec<accounts::Event>, String>,
) -> Result<Vec<accounts::Event>, String> {
    let now = (hooks.acts.account_seat)().map_or(seat.now, |seat| seat.now);
    let observation = accounts::LoginObservation {
        roster_path: seat.roster.as_deref().ok_or("account roster is missing")?,
        row: roster.row(label).ok_or("observed account is missing")?,
        who: &look.who,
    };
    accounts::Store::at(&seat.state).observe_login_within(
        seat.ladder,
        now,
        observation,
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

/// An account move whose relaunch nobody saw land, looked at again ([`AccountKept::flying`]).
fn account_land(sid: &str, kept: &Mutex<Kept>, hooks: &Hooks, landing: Landing) {
    let Some(flying) = with_kept_of(kept, |k| k.account.flying.clone()) else {
        return;
    };
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
        with_kept_of(kept, |k| k.account.flying = None);
        return;
    };
    let Some(who) = exec_login(hooks, snap.pid) else {
        return;
    };
    let on = who.me(&roster).account.map(str::to_owned);
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

/// How the session's loop learns of a move that landed after its ask was answered `wait:…` ([`account_land`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Landing {
    /// The host's carry landed it between two loops.
    Carried,
    /// It was read at an ask of a running loop (told `wait:account-unverified`, or one whose agent was relaunched under it unseen).
    Asked,
}

/// [`account_land`] where an agent that left its tab is back in it ([`relaunch_until`], [`Landing::Carried`]).
fn account_land_for(job: &WorkerJob, hooks: &Hooks) {
    if accounts_for(job.agent) && job.switches.accounts_live() {
        account_land(&job.sid, &job.kept, hooks, Landing::Carried);
    }
}

/// The production [`Acts::account_made`] under the aterm state root `root`.
fn account_made_at(root: &Path, sid: &str, to: &str, at: AccountAt) -> bool {
    use aterm_agent::supervise::approvals;
    use aterm_agent::supervise::policy::turn_end::RULE_ACCOUNT;
    let Some(path) = approvals::path_under(root, Some(sid)) else {
        return false;
    };
    let mut refused = Vec::new();
    let mut ledger = approvals::Ledger::open(Some(&path), Some(sid), &mut refused);
    ledger.write(
        aterm_agent::supervise::journal::unix_ms(),
        &approvals::Row {
            rule_id: RULE_ACCOUNT,
            outcome: approvals::Outcome::Typed,
            command: &format!("{}{to}", approvals::ACCOUNT_MOVE_COMMAND),
            reason: &approvals::account_switch_reason(to, at),
            box_seq: 0,
        },
        &mut refused,
    );
    refused.is_empty()
}

/// The login a relaunch with no process to read carries ([`Acts::account_cold`]).
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

/// One step of a cold relaunch under `login` ([`Acts::account_cold`]'s answer; `None`: `act` on the snapshot as the layout carried it).
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

/// The roster of `seat` where it lists an account ([`roster_read`]).
fn account_roster(seat: &AccountSeat) -> Option<accounts::Roster> {
    roster_read(seat).ok()
}

/// The roster a limit reset is judged against ([`roster_read`]).
fn account_roster_or_own(seat: &AccountSeat) -> Option<accounts::Roster> {
    match roster_read(seat) {
        Ok(roster) => Some(roster),
        Err(RosterUnread::Nothing) => Some(accounts::Roster::own()),
        Err(RosterUnread::Refused | RosterUnread::LoginsUnread) => None,
    }
}

/// One bounded read of `seat`'s roster file.
fn roster_read(seat: &AccountSeat) -> Result<accounts::Roster, RosterUnread> {
    let store = accounts::Store::at(&seat.state).bounded_by(seat.ladder);
    match read_account_roster(seat.roster.as_deref(), &store, seat.now) {
        Ok(roster) if roster.rows.is_empty() => Err(RosterUnread::Nothing),
        Ok(roster) => Ok(roster),
        Err(why) => Err(why),
    }
}

/// Every GUI account reader recovers admitted records before inspecting roster shape.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountGuiRead",
        action = "AnotherReader",
        reason = "Fixture selection chooses the restart, wall, usage, or normal UI consumer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountGuiRead",
        action = "AnotherShape",
        reason = "Fixture selection chooses an absent, empty, or populated roster"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountGuiRead",
        action = "AnotherFault",
        reason = "Fixture selection installs complete, pending, locked, conflicting, or damaged records"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountGuiRead",
        action = "Read",
        project = "harness_host::tests::account_read_tests::read_transition"
    )
)]
fn read_account_roster(
    path: Option<&Path>,
    store: &accounts::Store,
    now: i64,
) -> Result<accounts::Roster, RosterUnread> {
    match store.read_roster(path, now) {
        Ok(roster) => Ok(roster),
        Err(accounts::RosterReadError::Roster(_)) => Err(RosterUnread::Refused),
        Err(accounts::RosterReadError::Records(_)) => Err(RosterUnread::LoginsUnread),
    }
}

/// Why [`roster_read`] has no roster: the [`AccountsSaid`] it says, kept small.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RosterUnread {
    /// [`AccountsSaid::Nothing`]: no roster, or one that lists no account.
    Nothing,
    /// [`AccountsSaid::Refused`].
    Refused,
    /// [`AccountsSaid::LoginsUnread`].
    LoginsUnread,
}

impl RosterUnread {
    fn recording_word(self) -> &'static str {
        match self {
            Self::Nothing => "not-listed",
            Self::Refused => "roster-refused",
            Self::LoginsUnread => "account-state-refused",
        }
    }
}

impl From<RosterUnread> for AccountsSaid {
    fn from(unread: RosterUnread) -> Self {
        match unread {
            RosterUnread::Nothing => Self::Nothing,
            RosterUnread::Refused => Self::Refused,
            RosterUnread::LoginsUnread => Self::LoginsUnread,
        }
    }
}

/// The look at the accounts as a whole ([`AccountsSaid`]) under the rotation's switch (`rotation`: `[harness] accounts` not off).
fn accounts_said(
    rotation: bool,
    logins: bool,
    notes: bool,
    seat: &AccountSeat,
    carried: &BTreeSet<String>,
) -> Option<AccountsSaid> {
    if !rotation && !logins && !notes {
        return Some(AccountsSaid::Nothing);
    }
    let roster = match roster_read(seat) {
        Err(RosterUnread::Nothing) if notes => accounts::Roster::own(),
        _ if !rotation && !logins => return Some(AccountsSaid::Nothing),
        Ok(roster) if !rotation && !renewable_listed(&roster) => {
            return Some(AccountsSaid::Nothing);
        }
        Ok(roster) => roster,
        Err(unread) => return Some(unread.into()),
    };
    let store = accounts::Store::at(&seat.state);
    let (state, None) = store.load(seat.now).ok()? else {
        return None;
    };
    let (logins, None) = store.load_logins(seat.now).ok()? else {
        return None;
    };
    Some(AccountsSaid::listed(
        &roster, &state, &logins, seat.now, carried,
    ))
}

/// Tell the window what the accounts say, once per change ([`HostNote::Accounts`]).
fn accounts_tell(switches: &Switches, hooks: &Hooks, said: AccountsSaid, again: bool) {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    said.hash(&mut hasher);
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

/// What each other CLI's pool kept beside `seat`'s roster says ([`PoolSaid`]): a pool that
/// says nothing, or whose roster or logins cannot be read, is left out.
fn pools_said(seat: &AccountSeat) -> Vec<PoolSaid> {
    let Some(config) = seat
        .roster
        .as_deref()
        .and_then(Path::parent)
        .map(|dir| dir.join("aterm.toml"))
    else {
        return Vec::new();
    };
    aterm_agent::harness::accounts_sync::cli_pools(&config, &seat.state, &seat.home, seat.now)
        .into_iter()
        .filter_map(|pool| {
            let roster = pool.rows.ok()?;
            let store = accounts::Store::at_for(&seat.state, pool.cli, &seat.home);
            let (logins, _) = store.load_logins(seat.now).ok()?;
            let said = PoolSaid {
                cli: pool.cli,
                due: accounts::renew_due(&roster, &logins, &pool.state, &pool.enrolled, seat.now),
                wrong: accounts::wrong_accounts(&roster, &logins),
                sign_in: accounts::sign_in_tasks(&roster, &logins),
            };
            (!said.due.is_empty() || !said.wrong.is_empty() || !said.sign_in.is_empty())
                .then_some(said)
        })
        .collect()
}

/// Tell the window what the other pools say, once per change ([`HostNote::Pools`]); with
/// `again`, a standing word is told again. Nothing said, where nothing was, is no note.
fn pools_tell(switches: &Switches, hooks: &Hooks, said: Vec<PoolSaid>, again: bool) {
    use std::hash::{Hash as _, Hasher as _};
    let hash = if said.is_empty() {
        0
    } else {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        said.hash(&mut hasher);
        hasher.finish().max(1)
    };
    let told = switches.pools_said.swap(hash, Ordering::SeqCst) == hash;
    if told && !(again && hash != 0) {
        return;
    }
    (hooks.note)(HostNote::Pools(said));
}

/// The host's own look at the accounts, told ([`accounts_said`] under the mode read live, [`accounts_tell`] with `again`).
fn accounts_look(switches: &Switches, hooks: &Hooks, latches: &LoginLatches, again: bool) {
    if switches.mute() {
        return;
    }
    let pools = if switches.logins() {
        (hooks.acts.account_seat)()
            .map(|seat| pools_said(&seat))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    pools_tell(switches, hooks, pools, again);
    let rotation = switches.accounts() != accounts::Mode::Off;
    let notes = switches.notes();
    let said = if switches.accounts_live() || notes {
        (hooks.acts.account_seat)().and_then(|seat| {
            accounts_said(
                rotation,
                switches.logins(),
                notes,
                &seat,
                &latches.carried_rows(),
            )
        })
    } else {
        Some(AccountsSaid::Nothing)
    };
    if let Some(said) = said {
        accounts_tell(switches, hooks, said, again);
    }
}

/// What a worker read of its Claude Code for an account judgment ([`WorkerIdle::account_look`]).
struct AccountLook {
    who: accounts::ProcessLogin,
    label: Option<String>,
    snap: Snapshot,
}

impl AccountLook {
    /// The account the roster's judgments are about for this process (`accounts::judged_label`).
    fn judged<'a>(&self, roster: &'a accounts::Roster) -> Option<&'a str> {
        accounts::judged_label(roster, &self.who)
    }
}

/// [`WorkerIdle::roster_word`] for a process on the roster row `me` names.
fn roster_word_of(
    roster: &accounts::Roster,
    state: &accounts::State,
    enrolled: &BTreeSet<String>,
    me: &accounts::Me<'_>,
    at: &accounts::Point,
    mode: accounts::Mode,
    now: i64,
) -> RosterWord {
    let mine = me.label();
    let ready = accounts::candidates(
        roster,
        state,
        enrolled,
        mine,
        &accounts::Point::TurnEnd,
        now,
    )
    .into_iter()
    .find_map(|(label, verdict)| verdict.ok().map(|_| label))
    .map(|label| Ready { label });
    RosterWord::Row {
        next: accounts::sign_in_next(roster, state, enrolled, mine, now)
            .map(|(label, renew)| SignInNext { label, renew }),
        ready,
        held: held_of(roster, state, enrolled, me, at, mode, now),
    }
}

/// What holds a session at its usage limit even once another row is ready, as the roster's own advice says it at `at`.
fn held_of(
    roster: &accounts::Roster,
    state: &accounts::State,
    enrolled: &BTreeSet<String>,
    me: &accounts::Me<'_>,
    at: &accounts::Point,
    mode: accounts::Mode,
    now: i64,
) -> Option<HeldBy> {
    let me = accounts::Me {
        account: me.label(),
        left: None,
        ..*me
    };
    let mode = match mode {
        accounts::Mode::Off => accounts::Mode::Wall,
        mode => mode,
    };
    match accounts::advise_past_probe(roster, state, enrolled, &me, at, mode, now) {
        accounts::Advice::Stay(accounts::StayWhy::Pinned) => Some(HeldBy::Pin),
        accounts::Advice::Stay(accounts::StayWhy::OfferExpiring) => Some(HeldBy::Reset),
        accounts::Advice::Move { .. }
        | accounts::Advice::Wait { .. }
        | accounts::Advice::Resume { .. }
        | accounts::Advice::Stay(
            accounts::StayWhy::Breaker { .. }
            | accounts::StayWhy::Canary
            | accounts::StayWhy::NoCandidate
            | accounts::StayWhy::NoFresh,
        ) => None,
        accounts::Advice::Probe
        | accounts::Advice::Stay(
            accounts::StayWhy::NothingDue
            | accounts::StayWhy::Off
            | accounts::StayWhy::NoRoster
            | accounts::StayWhy::NotListed
            | accounts::StayWhy::OtherHome
            | accounts::StayWhy::NotAStoredLogin
            | accounts::StayWhy::Unmeasured
            | accounts::StayWhy::OtherSupervisor
            | accounts::StayWhy::KeyringStore
            | accounts::StayWhy::NoLine,
        ) => Some(HeldBy::Earlier),
    }
}

/// Whether `roster` lists a row the login lifecycle renews: one with an email.
fn renewable_listed(roster: &accounts::Roster) -> bool {
    roster.rows.iter().any(|row| row.email.is_some())
}

/// The renewable row whose own login the process holds.
fn renewable_own(roster: &accounts::Roster, look: &AccountLook) -> Option<String> {
    let me = look.who.me(roster);
    let label = me.account.filter(|_| me.default_home && me.stored_login)?;
    roster
        .row(label)
        .filter(|row| row.email.is_some())
        .map(|row| row.label.clone())
}

/// A usage wall that is this process's own ([`own_wall`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnWall {
    window: String,
    until: Option<i64>,
    at: i64,
}

/// The standing usage wall of `snap`'s own process (the recorder's rule, `accounts::record_wall`).
fn own_wall(seat: &AccountSeat, snap: &Snapshot) -> Option<OwnWall> {
    match standing_wall(seat, snap) {
        Standing::Own(wall) => Some(wall),
        Standing::Unwritten | Standing::NotOwn => None,
    }
}

/// What a session's transcript says of the usage limit its screen shows ([`standing_wall`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Standing {
    /// A quota rejection this process wrote stands last ([`own_wall`]).
    Own(OwnWall),
    /// No quota rejection stands last in a transcript that could be asked for one.
    Unwritten,
    /// Nothing looking again changes.
    NotOwn,
}

/// [`own_wall`]'s read, saying why there is none ([`Standing`]).
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

/// The session as the roster is asked about it at `point`.
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

/// Whether the evidence of the limit on a session's screen lags it.
fn evidence_lags(point: AccountPoint, judged: bool, unwritten: bool, owed: bool) -> bool {
    usage_limit(point) && judged && (unwritten || owed)
}

/// The loop's point as the roster is asked about it, with the wall it stands on: a turn end.
fn account_point(
    seat: &AccountSeat,
    look: &AccountLook,
    point: AccountPoint,
    seen: i64,
) -> (accounts::Point, Option<OwnWall>) {
    let (at, wall, _) = account_point_read(seat, look, point, seen);
    (at, wall)
}

/// [`account_point`], and whether the limit's row is not written yet ([`Standing::Unwritten`]).
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

/// The roster's advice as the policy takes it.
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

const ACCOUNT_ROW_STALE_S: i64 = 3600;

const ACCOUNT_CHANGED: &str = "account-changed";

/// Why a relaunch that was not made could not be, in the report's word (`accounts::SessionRow::held`).
fn held_word(step: &str) -> Option<&'static str> {
    match step {
        "wait:changed" => Some("jobs"),
        "wait:held" => Some("person"),
        step if step.starts_with("wait:signal-") => Some("person"),
        step if step.starts_with("refused:") => Some("line"),
        _ => None,
    }
}

/// The way back for a move that ended at the shell.
fn account_way_back(job: &WorkerJob, hooks: &Hooks, snap: &mut Snapshot) {
    let moved = with_kept(job, |k| {
        k.account
            .moved
            .take_if(|m| (m.pid, m.start.as_str()) == (snap.pid, snap.start.as_str()))
    });
    let Some(moved) = moved else {
        return;
    };
    if job.switches.accounts() == accounts::Mode::Off {
        return;
    }
    let seat = (hooks.acts.account_seat)();
    let roster = seat.as_ref().and_then(account_roster);
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
    let mut dead = (false, false);
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
        with_kept(job, |k| k.account.came_back = Some(moved.login.clone()));
        aterm_log::info!(
            "harness @{}: its move to account {} was not answered before its agent left; \
             relaunched under the login before it",
            job.sid,
            moved.to
        );
    }
}

/// Whether the store records `login` signed out ([`account_way_back`]).
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

/// The production [`Acts::memory`].
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

/// How long a tab's measure counts on the [`MemoryBoard`].
pub(crate) const BOARD_FRESH: Duration = Duration::from_secs(60);

/// The host's agents' footprints, one per tab, as each worker last measured its own ([`Acts::memory`]).
#[derive(Debug, Default)]
pub(crate) struct MemoryBoard {
    /// Per tab: when it was measured, and its footprint, MiB.
    seen: Mutex<HashMap<String, (Instant, u64)>>,
}

impl MemoryBoard {
    /// Record `sid`'s agent at `footprint_mib` at `now`, forget measures older than [`BOARD_FRESH`].
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
const fn pressure_word(
    level: aterm_sysprobe::MemoryLevel,
) -> aterm_agent::supervise::memory::Pressure {
    use aterm_agent::supervise::memory::Pressure;
    match level {
        aterm_sysprobe::MemoryLevel::Normal => Pressure::Normal,
        aterm_sysprobe::MemoryLevel::Warn => Pressure::Warn,
        aterm_sysprobe::MemoryLevel::Critical => Pressure::Critical,
    }
}

/// The machine's swap rate, KiB/s in plus out, for every agent's measure.
fn machine_swap_rate() -> Option<u32> {
    static METER: Mutex<aterm_sysprobe::SwapMeter> = Mutex::new(aterm_sysprobe::SwapMeter::new());
    let kib = aterm_sysprobe::swapped_kib();
    let at = Instant::now();
    METER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .observe(at, kib)
}

/// The product's [`Acts::watch`], as [`live_acts`] wires it.
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

/// The disk watch over the store ([`Hooks::disk`]).
fn live_disk(store: Store, note: Arc<NoteFn>) -> Arc<DiskFn> {
    static LOOKING: AtomicBool = AtomicBool::new(false);
    first_reading_off_thread(harness_state(), disk_first_reading);
    aterm_agent::harness::disk::beat::set_handler(Arc::new(crate::disk_board::beat));
    let now_store = Arc::clone(&store);
    let run: Arc<DiskLookFn> = Arc::new(move |sids, cwds, agents, seen, note| {
        disk_look(sids, cwds, agents, seen, &now_store, note);
    });
    let board_store = Arc::clone(&store);
    let watch = disk_watch(store, note, &LOOKING, run);
    let looked = Arc::new(move |sids: &[String], kind: crate::occupancy_host::Look| {
        *DISK_SIDS.lock().unwrap_or_else(PoisonError::into_inner) = sids.to_vec();
        let unix_ms = aterm_time::unix_millis()
            .and_then(|ms| i64::try_from(ms).ok())
            .unwrap_or(0);
        crate::occupancy_host::look(unix_ms, &board_sessions(&board_store, sids), kind);
        watch(sids);
    });
    let tick = Arc::clone(&looked);
    let look: Arc<DiskFn> =
        Arc::new(move |sids: &[String]| tick(sids, crate::occupancy_host::Look::Tick));
    let poked: Arc<DiskFn> =
        Arc::new(move |sids: &[String]| looked(sids, crate::occupancy_host::Look::Poke));
    aterm_agent::harness::disk::poke::set_handler(Arc::new(move |cause| {
        static POKE_PENDING: AtomicBool = AtomicBool::new(false);
        aterm_log::info!("harness disk watch: poked, {}", cause.describe());
        DISK_POKED.store(true, Ordering::SeqCst);
        if POKE_PENDING.swap(true, Ordering::SeqCst) {
            return;
        }
        let poked = Arc::clone(&poked);
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}disk-poke"))
            .spawn(move || {
                let sids = DISK_SIDS
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                disk_flight(&POKE_PENDING, "the disk watch's poke", || poked(&sids));
            });
        if spawned.is_err() {
            POKE_PENDING.store(false, Ordering::SeqCst);
        }
    }));
    look
}

/// What a disk look runs on its own thread over what the store gave it.
type DiskLookFn = dyn Fn(
        &[String],
        &[std::path::PathBuf],
        &[u32],
        &[(String, aterm_phase::disk_full::DiskFull)],
        &NoteFn,
    ) + Send
    + Sync;

/// The disk watch's look ([`live_disk`]'s, its poke's as well).
fn disk_watch(
    store: Store,
    note: Arc<NoteFn>,
    looking: &'static AtomicBool,
    run: Arc<DiskLookFn>,
) -> Arc<DiskFn> {
    let note: Arc<NoteFn> = {
        let said = Mutex::new(DiskEndsSaid::default());
        Arc::new(move |told: HostNote| {
            let owed = said
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .owed(&told, Instant::now());
            if owed {
                note(told);
            }
        })
    };
    Arc::new(move |sids: &[String]| {
        let cwds = working_dirs(&store, sids);
        let before = DISK_ALL_CWDS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (all, unknown, _) = all_session_dirs(&store, Some(&before));
        *DISK_ALL_CWDS.lock().unwrap_or_else(PoisonError::into_inner) = all;
        *DISK_CWD_UNKNOWN
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = unknown;
        let agents: Vec<u32> = sids
            .iter()
            .filter_map(|sid| foreground_of(&store, sid))
            .filter_map(|pgid| u32::try_from(pgid).ok())
            .collect();
        let seen: Vec<(String, aterm_phase::disk_full::DiskFull)> = sids
            .iter()
            .filter_map(|sid| {
                let rows = screen_rows(&store, sid)?;
                Some((sid.clone(), aterm_phase::disk_full::disk_full(&rows)?))
            })
            .collect();
        if looking.swap(true, Ordering::SeqCst) {
            return;
        }
        let note = Arc::clone(&note);
        let run = Arc::clone(&run);
        let sids = sids.to_vec();
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}disk"))
            .spawn(move || {
                let _quiet = aterm_sysprobe::quiet_errors();
                disk_flight(looking, "the disk watch's look", || {
                    run(&sids, &cwds, &agents, &seen, note.as_ref());
                });
            });
        if let Err(e) = spawned {
            looking.store(false, Ordering::SeqCst);
            aterm_log::warn!("harness disk watch could not start: {e}");
        }
    })
}

static DISK_SIDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static DISK_ALL_CWDS: Mutex<Vec<(u64, std::path::PathBuf)>> = Mutex::new(Vec::new());
static DISK_CWD_UNKNOWN: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static DISK_POKED: AtomicBool = AtomicBool::new(false);

/// The visible rows of tab `sid`'s screen, read in memory (the store's lock, then the terminal's, never waited on).
fn screen_rows(store: &Store, sid: &str) -> Option<Vec<String>> {
    let handle = {
        let g = store.read().unwrap_or_else(PoisonError::into_inner);
        g.by_sid(&aterm_session::SessionId::new(sid))?.clone()
    };
    let t = handle.term.try_lock().ok()?;
    let rows = usize::from(t.rows());
    Some(
        (0..rows)
            .map(|i| t.row_text(i).unwrap_or_default())
            .collect(),
    )
}

/// One look of the disk watch on its own thread ([`live_disk`]: a tick's look, or a poke's):
/// `look`, caught as `what`, then `looking` cleared.
fn disk_flight(looking: &AtomicBool, what: &str, look: impl FnOnce()) {
    let _ = caught(what, "it looks again at its next tick", look);
    looking.store(false, Ordering::SeqCst);
    crate::disk_board::beat();
}

/// The working directory every session of the store last reported, with its local id.
fn all_session_dirs(
    store: &Store,
    before: Option<&[(u64, std::path::PathBuf)]>,
) -> (Vec<(u64, std::path::PathBuf)>, Vec<u64>, Vec<u64>) {
    use crate::cwd_native::ReportedCwd as _;
    let handles: Vec<_> = {
        let g = store.read().unwrap_or_else(PoisonError::into_inner);
        g.live_handles().cloned().collect()
    };
    let mut unknown = Vec::new();
    let mut busy = Vec::new();
    let known = handles
        .iter()
        .filter_map(|h| {
            let read = match (h.term.try_lock(), before) {
                (Err(_), None) => {
                    busy.push(h.local_id);
                    return None;
                }
                (Err(_), Some(before)) => before.iter().find(|(id, _)| *id == h.local_id).cloned(),
                (Ok(t), _) => t
                    .native_working_directory()
                    .filter(|c| !c.is_empty())
                    .map(|c| std::path::PathBuf::from(c.into_owned()))
                    .filter(|cwd| cwd.is_absolute())
                    .map(|cwd| (h.local_id, cwd)),
            };
            if read.is_none() && cfg!(windows) {
                unknown.push(h.local_id);
            }
            read
        })
        .collect();
    (known, unknown, busy)
}

/// This instance's sessions as they stand now.
fn disk_sessions_now(store: &Store) -> aterm_agent::harness::disk::idle::Sessions {
    let before = DISK_ALL_CWDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    disk_sessions_with(store, &before)
}

/// [`disk_sessions_now`] over the look's copy `before`.
fn disk_sessions_with(
    store: &Store,
    before: &[(u64, std::path::PathBuf)],
) -> aterm_agent::harness::disk::idle::Sessions {
    let (all, unknown, busy) = all_session_dirs(store, (!cfg!(windows)).then_some(before));
    let why: Vec<String> = disk_cwd_unknown(&unknown)
        .into_iter()
        .chain(disk_tabs_busy(&busy))
        .collect();
    aterm_agent::harness::disk::idle::Sessions {
        cwds: all.into_iter().map(|(_, cwd)| cwd).collect(),
        unknown: (!why.is_empty()).then(|| why.join("; ")),
    }
}

/// Why tier 3 takes nothing at this removal.
fn disk_tabs_busy(busy: &[u64]) -> Option<String> {
    let (first, rest) = busy.split_first()?;
    Some(if rest.is_empty() {
        format!(
            "tab {first} was busy when the removal read the tabs again, so where it works now is \
             not known"
        )
    } else {
        format!(
            "{} tabs (tab {first} among them) were busy when the removal read the tabs again, so \
             where they work now is not known",
            busy.len()
        )
    })
}

/// The look's remover ([`disk_look`]'s one).
fn disk_remover(
    judge: aterm_agent::harness::disk::Judge,
    store: &Store,
) -> impl FnMut(&aterm_agent::harness::disk::Row) -> std::io::Result<aterm_agent::harness::disk::Removed>
+ '_ {
    aterm_agent::harness::disk::remover(judge, move || disk_sessions_now(store))
}

/// Why tier 3 takes nothing this look.
fn disk_cwd_unknown(unknown: &[u64]) -> Option<String> {
    let (first, rest) = unknown.split_first()?;
    let tabs = if rest.is_empty() {
        format!("tab {first}")
    } else {
        format!("{} tabs (tab {first} among them)", unknown.len())
    };
    Some(format!(
        "{tabs} reported no working directory this window can place, so any checkout may be \
         one's"
    ))
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

/// Where the disk watch searches for build directories (`aterm_agent::harness::disk::discover`).
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
    let (running, claude_unread) = match running {
        Ok(running) => (running, None),
        Err(why) => (discover::Running::default(), Some(why)),
    };
    discover::Roots {
        cwds: cwds.to_vec(),
        home: Some(home.to_path_buf()),
        claude_dirs: discover::claude_dirs(Some(home), running.dirs),
        claudes: running.claudes,
        claude_unread,
        agent_target_dirs,
        claude_tmp: Some(discover::claude_tmp_root(discover::own_uid())),
        device: discover::device_of(home),
        volume: aterm_agent::harness::disk::target::VolumeGuid::of_path_now(home),
        mounts: discover::mount_points(),
        skip_home_children: false,
    }
}

/// The disk watch's two runs of short looks ([`disk_look`]).
struct DiskRuns {
    passes: std::sync::atomic::AtomicU32,
    cleanup_off: std::sync::atomic::AtomicU32,
}

impl DiskRuns {
    const fn new() -> Self {
        Self {
            passes: std::sync::atomic::AtomicU32::new(0),
            cleanup_off: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// What a pass that ran tells the window ([`disk_pass_outcome`] on [`Self::passes`]).
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

static DISK_RUNS: DiskRuns = DiskRuns::new();
const DISK_SHORT_LOOKS: u32 = 2;
static DISK_NOTES_SAID: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
static DISK_PRIVATE_SAID: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The log line for a look below the warning line, or with an agent's demand waiting, in a
/// process that is not the owner's disk watch (`private`, from [`disk_private`]): the level and
/// the rule ([`DiskPrivate::why`]). `None` for any other look.
fn disk_private_line(
    private: Option<DiskPrivate>,
    below_warn: bool,
    demand: bool,
    level: aterm_agent::harness::disk::Level,
) -> Option<String> {
    let private = private.filter(|_| below_warn || demand)?;
    Some(format!(
        "harness disk watch: {}{}, and {}",
        level.as_str(),
        if demand {
            " with an agent's demand waiting"
        } else {
            ""
        },
        private.why()
    ))
}

/// The private reason, once a change, as [`DISK_NOTES_SAID`] says the config notes.
fn disk_private_said(said: &Mutex<Option<String>>, line: Option<String>) -> Option<String> {
    let mut said = said.lock().unwrap_or_else(PoisonError::into_inner);
    if *said == line {
        return None;
    }
    said.clone_from(&line);
    line
}

/// One look of the disk watch over `cwds` and `agents` (off the host thread): the ask's take-over
/// of what an ended host left of this process's sessions ([`disk_ask::before_pass`]), the pass,
/// then the ask's step over what it left ([`disk_ask::after_look`]).
fn disk_look(
    sids: &[String],
    cwds: &[std::path::PathBuf],
    agents: &[u32],
    seen: &[(String, aterm_phase::disk_full::DiskFull)],
    store: &Store,
    note: &NoteFn,
) {
    disk_ask::before_pass(sids, store);
    let mut looked = disk_ask::Looked::default();
    disk_look_pass(sids, cwds, agents, seen, store, note, &mut looked);
    disk_ask::after_look(&looked, sids, store, note);
}

/// What aterm itself freed at this look (`bytes`, ended now), for the ask's step to record
/// ([`disk_ask::Looked::reclaimed`]): no asked session's rise counts it.
fn disk_reclaimed(looked: &mut disk_ask::Looked, bytes: u64) {
    if bytes == 0 {
        return;
    }
    looked
        .reclaimed
        .push(aterm_agent::harness::disk::ask::Reclaimed {
            ended_ms: aterm_time::unix_millis()
                .and_then(|ms| i64::try_from(ms).ok())
                .unwrap_or(0),
            bytes,
        });
}

/// The look's pass ([`disk_look`]): what it read for the ask goes in `looked`.
fn disk_look_pass(
    sids: &[String],
    cwds: &[std::path::PathBuf],
    agents: &[u32],
    seen: &[(String, aterm_phase::disk_full::DiskFull)],
    store: &Store,
    note: &NoteFn,
    looked: &mut disk_ask::Looked,
) {
    let (Some(home), Some(state)) = (aterm_primer::home_dir(), harness_state()) else {
        return;
    };
    let board = &crate::disk_board::SHELF;
    crate::disk_board::beat();
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
    use aterm_agent::harness::disk::Level;
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    let table = aterm_sysprobe::volumes::table();
    let measure = || aterm_agent::harness::disk::volume_of(&home, &table).ok();
    let stats = measure();
    let free = stats.as_ref().map(|s| s.free);
    let total = stats.as_ref().map(|s| s.total);
    let room = stats.as_ref().map(|r| disk_room_after(&state, r, now));
    let thresholds = reading.thresholds_with(total, room);
    let level = thresholds.level(free, stats.as_ref().and_then(|s| s.files_free));
    let private = disk_private(&home);
    looked.thresholds = Some(thresholds);
    looked.private = private.is_some();
    disk_publish_look(
        &table,
        stats.as_ref().map(|r| crate::disk_board::VolumeRow {
            reading: r.clone(),
            thresholds,
            level,
            room,
            as_of: Instant::now(),
            as_of_unix: now,
            reclaimed_here: private.is_none(),
        }),
        &DISK_ALL_CWDS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
        &reading,
        &state,
        now,
    );
    let reserve = aterm_agent::harness::disk::reserve::path_for(&home);
    if private.is_none() {
        match aterm_agent::harness::cli::disk_reserve_look(
            &state,
            &reserve,
            level,
            free,
            &thresholds,
            now,
        ) {
            aterm_agent::harness::cli::ReserveMove::Kept
            | aterm_agent::harness::cli::ReserveMove::StillRefused => {}
            moved => {
                if let aterm_agent::harness::cli::ReserveMove::Released(bytes) = &moved {
                    disk_reclaimed(looked, *bytes);
                }
                aterm_log::info!(
                    "harness disk watch: reserve {moved:?} at {}",
                    reserve.display()
                );
            }
        }
    }
    disk_sightings(board, seen, level, free, &state, now);
    if level == Level::Unknown {
        return;
    }
    let reading = aterm_agent::harness::cli::DiskReading {
        config: reading.on_volume_with(total, room),
        floor_gib: reading.floor_gib.map(|floor| {
            aterm_agent::harness::disk::Config {
                auto_free_gib: floor,
                ..reading.config
            }
            .on_volume_with(total, reading.written, room)
            .auto_free_gib
        }),
        ..reading
    };
    let config = reading.config;
    let poked = DISK_POKED.swap(false, Ordering::SeqCst);
    let home_key = stats.as_ref().map(|r| r.key.clone());
    let adopted = home_key.as_ref().and_then(|k| {
        let b = crate::disk_board::SHELF.board_lock();
        if b.watched { b.adopt(k) } else { None }
    });
    let demand = adopted.as_ref().map(|(need, _)| *need);
    let line = disk_private_line(private, config.below_warn(free), demand.is_some(), level);
    if let Some(line) = disk_private_said(&DISK_PRIVATE_SAID, line) {
        aterm_log::info!("{line}");
    }
    if (!config.below_warn(free) && demand.is_none())
        || config.auto_free_gib == 0
        || private.is_some()
    {
        if let Some(told) = disk_look_without_pass(&reading, free, &DISK_RUNS) {
            note(told);
        }
        let cause = if let Some(private) = private {
            DiskFullCause::Private(private)
        } else if config.auto_free_gib == 0 {
            looked.done = Some(aterm_agent::harness::disk::ask::Done::CleanupOff);
            DiskFullCause::CleanupOff {
                fault: reading.off_by_fault,
            }
        } else {
            // Only a volume truly out of inodes: free bytes at or above a `warn` set below the
            // floor is no sign aterm is done.
            looked.done = stats
                .as_ref()
                .and_then(|s| s.files_free)
                .is_some_and(|f| f < thresholds.files_emergency)
                .then_some(aterm_agent::harness::disk::ask::Done::Inodes);
            DiskFullCause::Inodes
        };
        disk_alert(
            board,
            level,
            &thresholds,
            free,
            0,
            &[],
            disk_full_cause(&thresholds, free, cause),
            note,
        );
        disk_episode_after(board, level, 0, &[]);
        return;
    }
    let started = Instant::now();
    let due = demand.is_some()
        || disk_pass_due(
            &mut DISK_LAST_PASS
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            started,
            level,
            free,
            poked,
            room.unwrap_or(0),
        );
    if !due {
        // Back above the floor with no pass due: the run of short passes
        // ends, and with it the owner's low-disk line.
        if reading.at_or_above_floor(free)
            && let Some(told) = disk_look_without_pass(&reading, free, &DISK_RUNS)
        {
            note(told);
        }
        disk_episode_after(board, level, 0, &[]);
        if level != Level::Emergency {
            disk_alert(
                board,
                level,
                &thresholds,
                free,
                0,
                &[],
                DiskFullCause::Spent,
                note,
            );
        }
        return;
    }
    let lock_path = aterm_agent::harness::disk::reserve::pass_lock_for(&home);
    // Held until the ask's step records what this pass freed ([`disk_ask::after_look`]).
    looked.pass_lock = match aterm_agent::harness::disk::reserve::take_pass(&lock_path) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            aterm_log::info!(
                "harness disk watch: another instance's pass holds {}; this look runs none",
                lock_path.display()
            );
            looked.contended = true;
            disk_episode_after(board, level, 0, &[]);
            return;
        }
        Err(e) => {
            aterm_log::warn!(
                "harness disk watch: the pass lock {} could not be taken ({e}); the pass runs \
                 without it",
                lock_path.display()
            );
            None
        }
    };
    let short = config.below_auto_floor(free);
    let placing = aterm_agent::harness::disk::discover::placing_of(&home, &table);
    let demand_cwds: Vec<std::path::PathBuf> = adopted.map(|(_, cwds)| cwds).unwrap_or_default();
    let demand_only = sids.is_empty();
    if sids.is_empty() && demand.is_none() {
        let gone_after = if short {
            atpkg::build_cache::RECLAIM_GONE_UNDER_PRESSURE
        } else {
            atpkg::build_cache::RECLAIM_GONE
        };
        let journalled = if config.dead_build_dirs {
            disk_sweep_journalled(&state, now, board, |record| {
                atpkg::build_cache::reclaim_home_watched(gone_after, placing.as_ref(), record)
            })
        } else {
            SweepJournalled::default()
        };
        let SweepJournalled {
            swept,
            sized,
            unrecorded,
        } = journalled;
        let (freed, went) = disk_swept(&swept, &sized);
        disk_reclaimed(looked, freed);
        let after = measure();
        if !swept.is_empty() {
            aterm_log::info!(
                "harness disk watch: {} ({} GiB free against a {} GiB warning and a {} GiB \
                 floor), swept {} build cache{}, about {} back: {}",
                level.as_str(),
                free.map_or(0, |f| f >> 30),
                config.warn_free_gib,
                config.auto_free_gib,
                swept.len(),
                if swept.len() == 1 { "" } else { "s" },
                aterm_agent::harness::disk::human_bytes(freed),
                went.join("; "),
            );
            if short {
                let (bytes, went) = disk_episode_freed(board, freed, &went);
                note(HostNote::DiskFreed { bytes, went });
            }
        }
        let level_after = thresholds.level(
            after.as_ref().map(|s| s.free),
            after.as_ref().and_then(|s| s.files_free),
        );
        let free_after = after.as_ref().map(|s| s.free);
        disk_alert(
            board,
            level_after,
            &thresholds,
            free_after,
            freed,
            &[],
            disk_full_cause(
                &thresholds,
                free_after,
                if unrecorded {
                    DiskFullCause::Unrecorded
                } else {
                    DiskFullCause::Unsupervised {
                        whole: config.dead_build_dirs,
                    }
                },
            ),
            note,
        );
        disk_episode_after(board, level_after, freed, &went);
        disk_pass_ran(
            started,
            free,
            swept.is_empty()
                && (level_after == Level::Emergency
                    || (level_after == Level::Unknown && level == Level::Emergency)),
            level,
            !swept.is_empty(),
        );
        disk_pass_summary(
            home_key.as_ref(),
            after.as_ref().map(|s| s.free),
            (
                if swept.is_empty() {
                    Vec::new()
                } else {
                    vec![1]
                },
                false,
            ),
            freed,
            demand,
            &config,
            &[],
        );
        if level_after == Level::Unknown {
            return;
        }
        match DISK_RUNS.pass(!swept.is_empty(), after.as_ref().map(|s| s.free), config) {
            DiskOutcome::Plenty => note(HostNote::DiskPlenty),
            DiskOutcome::Short => {}
            DiskOutcome::Low { free_gib, .. } => note(HostNote::DiskLow {
                free_gib,
                floor_gib: config.auto_free_gib,
                hogs: Vec::new(),
                asked: disk_ask::owner_said(),
            }),
        }
        return;
    }
    let running = aterm_agent::harness::disk::discover::running_claude_census(Some(&home));
    let mut reach: Vec<std::path::PathBuf> = cwds.to_vec();
    for cwd in demand_cwds {
        if !reach.contains(&cwd) {
            reach.push(cwd);
        }
    }
    let mut roots = disk_roots(&reach, agents, &home, running);
    roots.skip_home_children = demand_only;
    let transcripts = home.join(".claude").join("projects");
    let cwd_unknown = disk_cwd_unknown(
        &DISK_CWD_UNKNOWN
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
    );
    let look = aterm_agent::harness::cli::DiskLook {
        state: &state,
        volume: &home,
        free,
        targets: &[],
        discover: Some(&roots),
        transcripts: Some(&transcripts),
        now,
        room,
        table: Some(&table),
        demand,
        session_cwds: &DISK_ALL_CWDS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, cwd)| cwd.clone())
            .collect::<Vec<_>>(),
        session_cwd_unknown: cwd_unknown.as_deref(),
        link_dirs: &aterm_agent::harness::cli::default_link_dirs(&home),
    };
    let judge = aterm_agent::harness::disk::Judge {
        now,
        threshold_days: config.target_stale_days,
        device: aterm_agent::harness::disk::target::dev_of(&home),
        volume: home_key
            .as_ref()
            .and_then(aterm_agent::harness::disk::target::VolumeGuid::of_key),
    };
    let tick = aterm_agent::harness::cli::disk_tick(
        &look,
        config,
        &mut disk_remover(judge, store),
        &mut || measure().map(|s| s.free),
    );
    board.tally_freed(&disk_pass_removals(&tick));
    let left: &[aterm_agent::harness::disk::Hog] = match &tick {
        aterm_agent::harness::cli::DiskTick::Reclaimed(_, hogs) => hogs,
        aterm_agent::harness::cli::DiskTick::Plenty => &[],
    };
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
        let went = done.went();
        aterm_log::info!(
            "harness disk watch: {} ({} GiB free against a {} GiB warning and a {} GiB floor), \
             freed about {} — {} build director{} of ended owners, whole, {} in part; {} idle \
             build director{} of live checkouts emptied (tier 3); and the incremental caches \
             of {} cargo profile{}; each is listed in {}",
            level.as_str(),
            free.map_or(0, |f| f >> 30),
            config.warn_free_gib,
            config.auto_free_gib,
            aterm_agent::harness::disk::human_bytes(done.freed_bytes),
            went.whole,
            if went.whole == 1 { "y" } else { "ies" },
            went.in_part,
            went.idle_whole,
            if went.idle_whole == 1 { "y" } else { "ies" },
            went.profiles,
            if went.profiles == 1 { "" } else { "s" },
            state.join(aterm_agent::harness::cli::DISK_LEDGER).display(),
        );
    }
    let took_whole = matches!(&tick, aterm_agent::harness::cli::DiskTick::Reclaimed(done, _)
        if done.went().idle_whole > 0);
    let after = free_after_pass(freed, free, || measure().map(|s| s.free));
    let freed_bytes = match &tick {
        aterm_agent::harness::cli::DiskTick::Reclaimed(done, _) => done.freed_bytes,
        aterm_agent::harness::cli::DiskTick::Plenty => 0,
    };
    disk_reclaimed(looked, freed_bytes);
    let went: Vec<String> = match &tick {
        aterm_agent::harness::cli::DiskTick::Reclaimed(done, _) => {
            let mut rows: Vec<(u64, String)> = done
                .removed
                .iter()
                .map(|(row, removed)| {
                    let line = format!(
                        "{} {}",
                        aterm_agent::harness::disk::human_bytes(removed.bytes),
                        row.path.display()
                    );
                    (
                        removed.bytes,
                        aterm_agent::harness::disk::shown(&line).into_owned(),
                    )
                })
                .collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r.0));
            rows.into_iter().map(|(_, line)| line).take(5).collect()
        }
        aterm_agent::harness::cli::DiskTick::Plenty => Vec::new(),
    };
    if freed && (short || took_whole) {
        let (bytes, went) = disk_episode_freed(board, freed_bytes, &went);
        note(HostNote::DiskFreed { bytes, went });
    }
    let measured_after = measure();
    let files_after = measured_after.as_ref().and_then(|s| s.files_free);
    let level_after = thresholds.level(after, files_after);
    if let Some(r) = measured_after {
        crate::disk_board::publish(|b| {
            b.measured(crate::disk_board::VolumeRow {
                level: thresholds.level(Some(r.free), r.files_free),
                thresholds,
                room,
                reading: r,
                as_of: Instant::now(),
                as_of_unix: now,
                reclaimed_here: private.is_none(),
            });
        });
    }
    disk_alert(
        board,
        level_after,
        &thresholds,
        after,
        freed_bytes,
        &glass,
        disk_full_cause(&thresholds, after, pass_cause(&tick)),
        note,
    );
    disk_episode_after(board, level_after, freed_bytes, &went);
    disk_pass_ran(
        started,
        free,
        !freed
            && (level_after == Level::Emergency
                || (level_after == Level::Unknown && level == Level::Emergency)),
        level,
        freed,
    );
    let tiers: Vec<u8> = match &tick {
        aterm_agent::harness::cli::DiskTick::Reclaimed(done, _) => {
            let mut t: Vec<u8> = done
                .removed
                .iter()
                .filter_map(|(row, _)| row.tier().map(aterm_agent::harness::disk::Tier::number))
                .chain(done.stops.iter().filter_map(|s| s.tier))
                .collect();
            t.sort_unstable();
            t.dedup();
            t
        }
        aterm_agent::harness::cli::DiskTick::Plenty => Vec::new(),
    };
    let ran_out = matches!(&tick, aterm_agent::harness::cli::DiskTick::Reclaimed(done, _)
        if done.ran_out());
    disk_pass_summary(
        home_key.as_ref(),
        after,
        (tiers, ran_out),
        freed_bytes,
        demand,
        &config,
        left,
    );
    if level_after == Level::Unknown {
        return;
    }
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
                asked: disk_ask::owner_said(),
            });
        }
    }
}

/// The board's rows of one look (`crate::disk_board`).
fn disk_publish_look(
    table: &aterm_sysprobe::volumes::Table,
    home: Option<crate::disk_board::VolumeRow>,
    sessions: &[(u64, std::path::PathBuf)],
    reading: &aterm_agent::harness::cli::DiskReading,
    state: &Path,
    now: i64,
) {
    let (mut rows, map) = disk_look_rows(table, home, sessions, reading, state, now);
    crate::disk_board::publish(|b| {
        for old in std::mem::take(&mut b.volumes) {
            if rows.len() >= crate::disk_board::MAX_VOLUMES {
                break;
            }
            if !rows.iter().any(|r| r.reading.key == old.reading.key) {
                rows.push(old);
            }
        }
        b.table = table.clone();
        b.volumes = rows;
        b.sessions = map;
    });
}

/// [`disk_publish_look`]'s rows and its map of each session's local id to its row, before they are published.
fn disk_look_rows(
    table: &aterm_sysprobe::volumes::Table,
    home: Option<crate::disk_board::VolumeRow>,
    sessions: &[(u64, std::path::PathBuf)],
    reading: &aterm_agent::harness::cli::DiskReading,
    state: &Path,
    now: i64,
) -> (Vec<crate::disk_board::VolumeRow>, BTreeMap<u64, usize>) {
    use aterm_sysprobe::volumes;
    let mut rows: Vec<crate::disk_board::VolumeRow> = home.into_iter().collect();
    let mut map: BTreeMap<u64, usize> = BTreeMap::new();
    for (id, cwd) in sessions {
        let cwd = table.long_spelling(cwd);
        let Some(spelled) = volumes::mount_of(&cwd, table) else {
            continue;
        };
        if !spelled.local || spelled.automounted {
            continue;
        }
        let placed = std::fs::canonicalize(&cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let Some(m) = volumes::mount_of(&placed, table) else {
            continue;
        };
        let same = |r: &crate::disk_board::VolumeRow| {
            r.reading.mount == m.on || m.key().is_some_and(|k| r.reading.key == k)
        };
        let at = match rows.iter().position(same) {
            Some(i) => i,
            None if rows.len() < crate::disk_board::MAX_VOLUMES => {
                let Ok(r) = volumes::read(m) else {
                    continue;
                };
                let room = disk_room_after(state, &r, now);
                let t = reading.thresholds_with(Some(r.total), Some(room));
                rows.push(crate::disk_board::VolumeRow {
                    level: t.level(Some(r.free), r.files_free),
                    thresholds: t,
                    room: Some(room),
                    reading: r,
                    as_of: Instant::now(),
                    as_of_unix: now,
                    reclaimed_here: false,
                });
                rows.len() - 1
            }
            None => continue,
        };
        map.insert(*id, at);
    }
    (rows, map)
}

/// Whether the working directory `cwd` is one the board cannot place for its 8.3 short names alone
/// (`C:\Users\ANDREW~1\x`: [`aterm_sysprobe::paths::short_spelled`]).
pub(crate) fn disk_short_spelled(cwd: &Path) -> bool {
    cfg!(windows) && aterm_sysprobe::paths::short_spelled(cwd).is_some()
}

/// A session's 8.3 directory spelled long, onto `shelf` (`crate::disk_board::Board::spelled`).
pub(crate) fn disk_spell_now(
    shelf: &crate::disk_board::Shelf,
    table: &aterm_sysprobe::volumes::Table,
    session: u64,
    cwd: &Path,
) -> bool {
    disk_spell_with(
        shelf,
        table,
        session,
        cwd,
        aterm_sysprobe::volumes::long_name,
    )
}

/// [`disk_spell_now`], `name` asked in place of [`aterm_sysprobe::volumes::long_name`] (a test's call that stalls).
fn disk_spell_with(
    shelf: &crate::disk_board::Shelf,
    table: &aterm_sysprobe::volumes::Table,
    session: u64,
    cwd: &Path,
    name: fn(&Path) -> Option<PathBuf>,
) -> bool {
    if !disk_short_spelled(cwd) {
        return false;
    }
    let spelled = table.long_spelling_by(cwd, |through| {
        disk_long_name_by(through, name, DISK_SPELL_DEADLINE)
    });
    let std::borrow::Cow::Owned(long) = spelled else {
        return false;
    };
    shelf.board_lock().spell(session, cwd.to_path_buf(), long);
    true
}

const DISK_SPELL_DEADLINE: Duration = Duration::from_secs(1);

static DISK_SPELL_STUCK: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The long spelling of `through` (an existing verbatim path through its last short name) as `name`
/// gives it (`GetLongPathNameW`: [`aterm_sysprobe::volumes::long_name`]).
fn disk_long_name_by(
    through: &Path,
    name: fn(&Path) -> Option<PathBuf>,
    deadline: Duration,
) -> Option<PathBuf> {
    let stuck = || {
        DISK_SPELL_STUCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    };
    if stuck().iter().any(|p| p == through) {
        return None;
    }
    let (answer, answered) = std::sync::mpsc::sync_channel(1);
    let abandoned = Arc::new(AtomicBool::new(false));
    let (asked, given_up) = (through.to_path_buf(), Arc::clone(&abandoned));
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}disk-long-name"))
        .spawn(move || {
            let long = {
                let _quiet = aterm_sysprobe::quiet_errors();
                name(&asked)
            };
            let mut stuck = DISK_SPELL_STUCK
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if given_up.load(Ordering::Relaxed) {
                if let Some(at) = stuck.iter().position(|p| *p == asked) {
                    stuck.swap_remove(at);
                }
            } else {
                let _ = answer.send(long);
            }
        });
    if spawned.is_err() {
        return None;
    }
    match answered.recv_timeout(deadline) {
        Ok(long) => long,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let mut marked = stuck();
            if let Ok(long) = answered.try_recv() {
                return long;
            }
            abandoned.store(true, Ordering::Relaxed);
            marked.push(through.to_path_buf());
            None
        }
    }
}

/// The 8.3 reports the speller has yet to spell ([`disk_cwd_reported`]).
type SpellQueue = Mutex<Vec<(&'static crate::disk_board::Shelf, u64, PathBuf)>>;
static DISK_SPELL_QUEUE: SpellQueue = Mutex::new(Vec::new());
static DISK_SPELLING: AtomicBool = AtomicBool::new(false);

/// A tab reported a new working directory `cwd` (the window's cwd-change observer, `App::flush_title_drift`, on the UI thread).
pub(crate) fn disk_cwd_reported(session: u64, cwd: Option<&str>) {
    disk_cwd_reported_onto(&crate::disk_board::SHELF, session, cwd);
}

/// [`disk_cwd_reported`] onto `shelf` (a test's own board).
fn disk_cwd_reported_onto(
    shelf: &'static crate::disk_board::Shelf,
    session: u64,
    cwd: Option<&str>,
) {
    let Some(cwd) = cwd
        .filter(|c| disk_short_spelled(Path::new(c)))
        .map(PathBuf::from)
    else {
        return;
    };
    {
        let mut queue = DISK_SPELL_QUEUE
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.retain(|(s, id, _)| !(std::ptr::eq(*s, shelf) && *id == session));
        if queue.len() >= crate::disk_board::MAX_SPELLED {
            queue.remove(0);
        }
        queue.push((shelf, session, cwd));
    }
    if DISK_SPELLING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}disk-spell"))
        .spawn(disk_speller);
    if let Err(e) = spawned {
        DISK_SPELLING.store(false, Ordering::SeqCst);
        aterm_log::warn!("harness disk watch: no thread to spell a tab's 8.3 directory long ({e})");
    }
}

/// The speller's thread ([`disk_cwd_reported`]).
fn disk_speller() {
    let _quiet = aterm_sysprobe::quiet_errors();
    loop {
        let queued = std::mem::take(
            &mut *DISK_SPELL_QUEUE
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        if queued.is_empty() {
            DISK_SPELLING.store(false, Ordering::SeqCst);
            let more = !DISK_SPELL_QUEUE
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty();
            if !more || DISK_SPELLING.swap(true, Ordering::SeqCst) {
                return;
            }
            continue;
        }
        let table = aterm_sysprobe::volumes::table();
        for (shelf, session, cwd) in queued {
            disk_spell_now(shelf, &table, session, &cwd);
        }
    }
}

/// Why this process is not the owner's disk watch ([`disk_private`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiskPrivate {
    /// It runs under a private state root (a development build's `ATERM_STATE_HOME`).
    StateRoot,
    /// The account's home as the OS knows it could not be read: nothing proves this is the owner's.
    NoAccountHome,
    /// Its home's device cannot be named (a volume that keeps no file ids, fat or exFAT, or a home that cannot be opened).
    DeviceUnnamed,
    /// Its home is not the account's and lies on the account home's volume, or on one that cannot
    /// be told apart from it (the account home's own device cannot be named).
    ScratchHome,
}

impl DiskPrivate {
    /// Why the watch removes nothing, whole: `this instance <rule>: it measures and removes nothing`.
    pub(crate) const fn why(self) -> &'static str {
        match self {
            Self::StateRoot => {
                "this instance runs under a private state root (ATERM_STATE_HOME): it measures \
                 and removes nothing"
            }
            Self::NoAccountHome => {
                "this instance cannot read the account's home, so nothing proves it is the \
                 owner's watch: it measures and removes nothing"
            }
            Self::DeviceUnnamed => {
                "this instance cannot name its home's device (a volume that keeps no file ids, or \
                 a home it cannot open): it measures and removes nothing"
            }
            Self::ScratchHome => {
                "this instance's home is not the account's and lies on the account home's volume \
                 (a scratch home): it measures and removes nothing"
            }
        }
    }
}

/// Whether, and why ([`DiskPrivate`]), this process is not the owner's disk watch.
fn disk_private(home: &Path) -> Option<DiskPrivate> {
    disk_private_with(
        disk_private_root(
            aterm_types::dirs::runs_under_private_state_root(),
            crate::headless_seam_admits(
                aterm_types::dev_seam!(aterm_agent::harness::disk::WATCH_SEAM).is_some(),
            ),
        ),
        atpkg::platform::account_home().as_deref(),
        home,
        &aterm_agent::harness::disk::target::dev_of,
    )
}

/// Whether the private state root (`private`) keeps this process from the owner's disk watch
/// ([`DiskPrivate::StateRoot`]): it does unless the watch's development seam was admitted
/// (`watch_seam`, [`aterm_agent::harness::disk::WATCH_SEAM`], only under a private state root
/// and a scratch home), which leaves the home's own rules to judge it.
const fn disk_private_root(private: bool, watch_seam: bool) -> bool {
    private && !watch_seam
}

/// [`disk_private`] over its facts.
fn disk_private_with(
    private_root: bool,
    account: Option<&Path>,
    home: &Path,
    dev: &dyn Fn(&Path) -> Option<u64>,
) -> Option<DiskPrivate> {
    if private_root {
        return Some(DiskPrivate::StateRoot);
    }
    let Some(account) = account else {
        return Some(DiskPrivate::NoAccountHome);
    };
    let Some(here) = dev(home) else {
        return Some(DiskPrivate::DeviceUnnamed);
    };
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if canon(account) == canon(home) {
        return None;
    }
    dev(account)
        .is_none_or(|owner| here == owner)
        .then_some(DiskPrivate::ScratchHome)
}

/// The first reading's thread.
fn first_reading_off_thread(state: Option<PathBuf>, read: fn(Option<&Path>)) -> bool {
    let theirs = state.clone();
    match std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}disk-first"))
        .spawn(move || {
            let _quiet = aterm_sysprobe::quiet_errors();
            read(theirs.as_deref());
        }) {
        Ok(_) => true,
        Err(e) => {
            aterm_log::warn!(
                "harness disk watch: no thread for the first reading ({e}); reading inline"
            );
            let _quiet = aterm_sysprobe::quiet_errors();
            read(state.as_deref());
            false
        }
    }
}

/// The first reading, at host start (no pass).
fn disk_first_reading(state: Option<&Path>) {
    let Some(home) = aterm_primer::home_dir() else {
        return;
    };
    let table = aterm_sysprobe::volumes::table();
    let Ok(r) = aterm_agent::harness::disk::volume_of(&home, &table) else {
        crate::disk_board::publish(|b| b.table = table);
        return;
    };
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    let reading =
        aterm_agent::harness::cli::disk_config_read(crate::app_config::config_path().as_deref());
    let room = state.map(|state| disk_room_after(state, &r, now));
    let t = reading.thresholds_with(Some(r.total), room);
    let row = crate::disk_board::VolumeRow {
        level: t.level(Some(r.free), r.files_free),
        thresholds: t,
        room,
        reading: r,
        as_of: Instant::now(),
        as_of_unix: now,
        reclaimed_here: disk_private(&home).is_none(),
    };
    crate::disk_board::publish(|b| {
        b.table = table;
        b.volumes = vec![row];
    });
}

static DISK_ROOMS: Mutex<Option<aterm_agent::harness::disk::room::Rooms>> = Mutex::new(None);

/// Feed one look's `reading` of a volume to its room meter at `now` and answer the volume's room.
fn disk_room_after(state: &Path, reading: &aterm_sysprobe::volumes::Reading, now: i64) -> u64 {
    use aterm_agent::harness::disk::room::Rooms;
    let (room, save) = {
        let mut kept = DISK_ROOMS.lock().unwrap_or_else(PoisonError::into_inner);
        let rooms = kept.get_or_insert_with(|| Rooms::load(state));
        let key = reading.key.spelled();
        if let Some(meter) = rooms.meter(&key) {
            meter.observe(now, reading.free);
        }
        let room = rooms.room(&key, now, reading.total);
        (room, rooms.save_due(now).then(|| rooms.clone()))
    };
    if let Some(rooms) = save
        && let Err(e) = rooms.save(state)
    {
        aterm_log::warn!("harness disk watch: the room file could not be written: {e}");
    }
    room
}

const DISK_PASS_EVERY: Duration = Duration::from_secs(10 * 60);
const DISK_EMERGENCY_DROP: u64 = 512 << 20;
const DISK_LOW_EVERY: Duration = Duration::from_secs(3600);
const DISK_LOW_EVERY_MAX: Duration = Duration::from_secs(8 * 3600);

/// The last pass ([`disk_pass_due`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiskPass {
    at: Instant,
    free: Option<u64>,
    fruitless: bool,
    level: aterm_agent::harness::disk::Level,
    low_every: Duration,
}

static DISK_LAST_PASS: Mutex<Option<DiskPass>> = Mutex::new(None);

/// Whether a look at `now` (the volume at `level`, `free` bytes free, `poked` since the last look, `room` bytes its room) runs a pass.
fn disk_pass_due(
    last: &mut Option<DiskPass>,
    now: Instant,
    level: aterm_agent::harness::disk::Level,
    free: Option<u64>,
    poked: bool,
    room: u64,
) -> bool {
    use aterm_agent::harness::disk::Level;
    if matches!(level, Level::Unknown | Level::Ok) {
        return false;
    }
    let Some(pass) = last.as_mut() else {
        return true;
    };
    if level != Level::Emergency {
        pass.fruitless = false;
    }
    let since = now.saturating_duration_since(pass.at);
    if level == Level::Low {
        let fell = matches!((pass.free, free), (Some(was), Some(is))
            if is.saturating_add(room / 4) < was);
        return poked || pass.level != Level::Low || fell || since >= pass.low_every;
    }
    if since >= DISK_PASS_EVERY {
        return true;
    }
    if pass.fruitless {
        return matches!((pass.free, free), (Some(was), Some(is))
            if is.saturating_add(DISK_EMERGENCY_DROP) < was);
    }
    poked || level == Level::Emergency || pass.level < Level::Short
}

/// The cadence after a pass at `level` that `freed` something or not, the last one's `before` ([`DiskPass::low_every`]).
fn disk_low_every(
    level: aterm_agent::harness::disk::Level,
    freed: bool,
    before: Option<&DiskPass>,
) -> Duration {
    use aterm_agent::harness::disk::Level;
    match before {
        Some(last) if level == Level::Low && !freed && last.level == Level::Low => {
            last.low_every.saturating_mul(2).min(DISK_LOW_EVERY_MAX)
        }
        _ => DISK_LOW_EVERY,
    }
}

/// A pass that started at `at` in a look that measured `free` at `level` ran.
fn disk_pass_ran(
    at: Instant,
    free: Option<u64>,
    fruitless: bool,
    level: aterm_agent::harness::disk::Level,
    freed: bool,
) {
    let mut last = DISK_LAST_PASS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let low_every = disk_low_every(level, freed, last.as_ref());
    *last = Some(DiskPass {
        at,
        free,
        fruitless,
        level,
        low_every,
    });
}

/// What a pass did, on the board for `await sys` to answer from (`crate::disk_board::PassSummary`).
fn disk_pass_summary(
    key: Option<&aterm_sysprobe::volumes::VolumeKey>,
    free_after: Option<u64>,
    (tiers, ran_out): (Vec<u8>, bool),
    freed: u64,
    demand: Option<u64>,
    config: &aterm_agent::harness::disk::Config,
    left: &[aterm_agent::harness::disk::Hog],
) {
    let Some(key) = key else {
        return;
    };
    let spent = crate::disk_board::spent(demand, free_after, ran_out);
    let summary = crate::disk_board::PassSummary {
        key: key.clone(),
        ended_at: Instant::now(),
        free_after,
        tiers,
        freed,
        demand,
        ran_out,
        spent,
        held_back: spent && !config.idle_whole_on(),
        hogs: left.len(),
        largest: left.iter().map(|h| h.bytes).max().unwrap_or(0),
        left: left.to_vec(),
        ended_at_unix: crate::disk_board::unix_now(),
    };
    crate::disk_board::publish(|b| b.last_pass = Some(summary));
}

/// The short episode's freed figures with this pass's `freed` bytes and `went` lines added (the episode adds them itself at [`disk_episode_after`]).
fn disk_episode_freed(board: &Shelf, freed: u64, went: &[String]) -> (u64, Vec<String>) {
    let ep = disk_episode(board);
    let mut all = ep.went.clone();
    for w in went {
        if all.len() < 5 && !all.contains(w) {
            all.push(w.clone());
        }
    }
    (ep.freed.saturating_add(freed), all)
}

/// What the no-session sweep did, journalled ([`disk_sweep_journalled`]).
#[derive(Debug, Default)]
struct SweepJournalled {
    swept: Vec<(PathBuf, atpkg::build_cache::Unused)>,
    sized: Vec<(PathBuf, atpkg::build_cache::Unused, u64)>,
    unrecorded: bool,
}

/// The no-session sweep, journalled.
fn disk_sweep_journalled(
    state: &Path,
    now: i64,
    board: &Shelf,
    sweep: impl FnOnce(
        &mut dyn FnMut(&Path, atpkg::build_cache::Unused) -> bool,
    ) -> Vec<(PathBuf, atpkg::build_cache::Unused)>,
) -> SweepJournalled {
    let mut sized: Vec<(PathBuf, atpkg::build_cache::Unused, u64)> = Vec::new();
    let mut unrecorded = false;
    let swept = sweep(&mut |path, why| {
        let (bytes, _) = aterm_agent::harness::disk::dir_bytes(path);
        let on_record =
            aterm_agent::harness::cli::disk_sweep_journal(state, path, bytes, why, false, now);
        if on_record {
            sized.push((path.to_path_buf(), why, bytes));
        } else {
            unrecorded = true;
        }
        on_record
    });
    let removals = disk_sweep_removals(&swept, &sized);
    for ((path, why), (bytes, _)) in swept.iter().zip(&removals) {
        aterm_agent::harness::cli::disk_sweep_journal(state, path, *bytes, *why, true, now);
    }
    board.tally_freed(&removals);
    SweepJournalled {
        swept,
        sized,
        unrecorded,
    }
}

/// What the no-session sweep freed.
fn disk_swept(
    swept: &[(PathBuf, atpkg::build_cache::Unused)],
    sized: &[(PathBuf, atpkg::build_cache::Unused, u64)],
) -> (u64, Vec<String>) {
    let mut freed = 0u64;
    let went = swept
        .iter()
        .map(|(p, why)| {
            let bytes = disk_sized(sized, p, *why);
            freed = freed.saturating_add(bytes);
            format!(
                "{} {} ({})",
                aterm_agent::harness::disk::human_bytes(bytes),
                p.display(),
                aterm_agent::harness::disk::sweep_reason(*why)
            )
        })
        .collect();
    (freed, went)
}

/// The size `path` was given before its removal (`sized`), or nothing for one not sized.
fn disk_sized(
    sized: &[(PathBuf, atpkg::build_cache::Unused, u64)],
    path: &Path,
    why: atpkg::build_cache::Unused,
) -> u64 {
    sized
        .iter()
        .find(|(p, w, _)| p == path && *w == why)
        .map_or(0, |(_, _, b)| *b)
}

/// The no-session sweep's removals as today's freed tally counts them ([`crate::disk_board::Shelf::tally_freed`]).
fn disk_sweep_removals(
    swept: &[(PathBuf, atpkg::build_cache::Unused)],
    sized: &[(PathBuf, atpkg::build_cache::Unused, u64)],
) -> Vec<(u64, &'static str)> {
    swept
        .iter()
        .map(|(p, why)| (disk_sized(sized, p, *why), crate::disk_board::BUILD_CACHES))
        .collect()
}

/// A supervised pass's removals as today's freed tally counts them.
fn disk_pass_removals(tick: &aterm_agent::harness::cli::DiskTick) -> Vec<(u64, &'static str)> {
    match tick {
        aterm_agent::harness::cli::DiskTick::Reclaimed(done, _) => done
            .removed
            .iter()
            .map(|(row, removed)| (removed.bytes, crate::disk_board::freed_what(row.class)))
            .collect(),
        aterm_agent::harness::cli::DiskTick::Plenty => Vec::new(),
    }
}

use crate::disk_board::{DiskEpisode, Shelf};

const DISK_RESUME_TRIES: u8 = 3;

/// The disk episode state, on `board` under its leaf lock.
fn disk_episode(board: &Shelf) -> EpisodeGuard<'_> {
    EpisodeGuard(board.board_lock())
}

struct EpisodeGuard<'a>(std::sync::MutexGuard<'a, crate::disk_board::Board>);

impl std::ops::Deref for EpisodeGuard<'_> {
    type Target = DiskEpisode;
    fn deref(&self) -> &DiskEpisode {
        &self.0.episode
    }
}

impl std::ops::DerefMut for EpisodeGuard<'_> {
    fn deref_mut(&mut self) -> &mut DiskEpisode {
        &mut self.0.episode
    }
}

/// What the supervised screens said this look (`seen`), against the level measured.
fn disk_sightings(
    board: &Shelf,
    seen: &[(String, aterm_phase::disk_full::DiskFull)],
    level: aterm_agent::harness::disk::Level,
    free: Option<u64>,
    state: &Path,
    now: i64,
) {
    use aterm_agent::harness::disk::{Level, human_bytes, poke::Cause};
    use aterm_phase::disk_full::Source;
    {
        let mut ep = disk_episode(board);
        ep.said.retain(|sid| seen.iter().any(|(s, _)| s == sid));
    }
    for (sid, d) in seen {
        let code = d.code.as_str().to_string();
        let fresh = {
            let mut ep = disk_episode(board);
            if level == Level::Emergency && d.source != Source::ToolOutput {
                ep.stuck.insert(sid.clone(), code.clone());
            }
            ep.said.insert(sid.clone())
        };
        if !fresh {
            continue;
        }
        DISK_POKED.store(true, Ordering::SeqCst);
        let measured = free.map_or_else(|| "unknown".to_string(), human_bytes);
        if level == Level::Emergency {
            aterm_log::warn!(
                "harness @{sid}: its Claude Code says the disk is full ({code}, {:?}); {measured} \
                 free measured: no continuation is typed until space is back",
                d.source
            );
        } else {
            aterm_log::info!(
                "harness @{sid}: its Claude Code says the disk is full ({code}, {:?}), but {measured} \
                 is free on the home volume: reported, not acted on",
                d.source
            );
        }
        aterm_agent::harness::cli::disk_poke_journal(
            state,
            &Cause::Screen {
                sid: sid.clone(),
                code,
            },
            free,
            level,
            now,
        );
    }
}

/// After a look: the episode's level, and every stuck session owed the resume once the volume is out of its emergency.
fn disk_episode_after(
    board: &Shelf,
    level: aterm_agent::harness::disk::Level,
    freed: u64,
    went: &[String],
) {
    use aterm_agent::harness::disk::{Level, resume_text};
    let resumed = {
        let mut ep = disk_episode(board);
        ep.freed = ep.freed.saturating_add(freed);
        for w in went {
            if ep.went.len() < 5 {
                ep.went.push(w.clone());
            }
        }
        if level == Level::Unknown {
            return;
        }
        ep.level = Some(level);
        let mut resumed = Vec::new();
        if level != Level::Emergency && !ep.stuck.is_empty() {
            let stuck = std::mem::take(&mut ep.stuck);
            for (sid, code) in stuck {
                let text = resume_text(&code, ep.freed, &ep.went);
                ep.owed.insert(sid.clone(), (text, DISK_RESUME_TRIES));
                resumed.push(sid);
            }
        }
        if matches!(level, Level::Ok | Level::Low) && ep.owed.is_empty() && ep.stuck.is_empty() {
            ep.freed = 0;
            ep.went.clear();
        }
        resumed
    };
    for sid in resumed {
        aterm_log::info!(
            "harness @{sid}: space is back ({}); the disk resume is owed at its next idle point",
            level.as_str()
        );
    }
}

/// What a supervised pass says holds a full volume up.
fn pass_cause(tick: &aterm_agent::harness::cli::DiskTick) -> DiskFullCause {
    match tick {
        aterm_agent::harness::cli::DiskTick::Reclaimed(done, _)
            if done
                .denials
                .iter()
                .any(|r| matches!(r, aterm_agent::harness::disk::Refusal::Unjournalled(_))) =>
        {
            DiskFullCause::Unrecorded
        }
        _ => DiskFullCause::Spent,
    }
}

/// What holds a full volume up ([`HostNote::DiskFull`]).
fn disk_full_cause(
    thresholds: &aterm_agent::harness::disk::Thresholds,
    free: Option<u64>,
    cause: DiskFullCause,
) -> DiskFullCause {
    if free.is_some_and(|f| f < thresholds.emergency) {
        cause
    } else {
        DiskFullCause::Inodes
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the board whose episode keeps the alert, the look's reading and the alert's \
              own words: a struct for them would be built at each call and read once"
)]
fn disk_alert(
    board: &Shelf,
    level: aterm_agent::harness::disk::Level,
    thresholds: &aterm_agent::harness::disk::Thresholds,
    free: Option<u64>,
    freed: u64,
    hogs: &[String],
    cause: DiskFullCause,
    note: &NoteFn,
) {
    if level == aterm_agent::harness::disk::Level::Unknown {
        return;
    }
    let emergency = level == aterm_agent::harness::disk::Level::Emergency;
    let (was, total_freed) = {
        let mut ep = disk_episode(board);
        (
            std::mem::replace(&mut ep.alerted, emergency),
            ep.freed.saturating_add(freed),
        )
    };
    if emergency {
        if !was {
            aterm_log::error!(
                "harness disk watch: the home volume is full ({} free) and {}",
                free.map_or_else(
                    || "unknown".to_string(),
                    aterm_agent::harness::disk::human_bytes
                ),
                cause.log_words()
            );
        }
        note(HostNote::DiskFull {
            free,
            freed: total_freed,
            hogs: hogs.to_vec(),
            cause,
            thresholds: *thresholds,
            asked: disk_ask::owner_said(),
        });
    } else if was {
        note(HostNote::DiskFullOver);
    }
}

/// Whether tab `sid`'s continuations are withheld.
fn disk_withholds(board: &Shelf, sid: &str) -> bool {
    let ep = disk_episode(board);
    ep.stuck.contains_key(sid) && ep.level == Some(aterm_agent::harness::disk::Level::Emergency)
}

/// The resume owed tab `sid`, if one is.
fn disk_owed(board: &Shelf, sid: &str) -> Option<String> {
    disk_episode(board)
        .owed
        .get(sid)
        .map(|(text, _)| text.clone())
}

/// The `disk-free@v1` line owed tab `sid`, if one is.
fn disk_ask_owed(board: &Shelf, sid: &str) -> Option<crate::disk_board::AskOwed> {
    board.board_lock().asks.get(sid).cloned()
}

/// Why tab `sid` may not be asked to free space as of the turn break it rests at (nothing: it
/// may), for the look's seats.
fn disk_ask_fit(board: &Shelf, sid: &str, found: crate::disk_board::Unfit) {
    let mut b = board.board_lock();
    if found == crate::disk_board::Unfit::default() {
        b.unfit.remove(sid);
    } else {
        b.unfit.insert(sid.to_owned(), found);
    }
}

/// Tab `sid` works again (a busy read): what its worker found of the session itself at its last
/// turn break no longer holds, as it was given something to do ([`crate::disk_board::Unfit::own`]);
/// whose it is holds on. The loop's lasting withholds stand as its loop told them
/// ([`IdleHost::lasting_hold`]): a break of the agent's own background work reads busy too and
/// works past no wall it stands at, so the loop alone says when a wall's hold is over.
fn disk_ask_busy(board: &Shelf, sid: &str) {
    let mut b = board.board_lock();
    let Some(found) = b.unfit.get_mut(sid) else {
        return;
    };
    found.own = None;
    if found.theirs.is_none() {
        b.unfit.remove(sid);
    }
}

/// Tab `sid`'s loop opened a limit episode at a usage-limit wall (`Some`) or closed it (`None`):
/// the ask to free space holds it out of its seats while the wall holds
/// ([`crate::disk_board::Board::walls`]).
fn disk_ask_walled(board: &Shelf, sid: &str, wall: Option<crate::disk_board::Wall>) {
    let mut b = board.board_lock();
    match wall {
        Some(wall) => {
            b.walls.insert(sid.to_owned(), wall);
        }
        None => {
            b.walls.remove(sid);
        }
    }
}

/// Tab `sid`'s loop decided its last turn break held by a lasting withhold (`Some`) or none
/// (`None`): the ask to free space holds it out of its seats while it holds
/// ([`crate::disk_board::Board::withholds`]).
fn disk_ask_withheld(board: &Shelf, sid: &str, withhold: Option<crate::disk_board::Withhold>) {
    let mut b = board.board_lock();
    match withhold {
        Some(withhold) => {
            b.withholds.insert(sid.to_owned(), withhold);
        }
        None => {
            b.withholds.remove(sid);
        }
    }
}

/// The ask's step at tab `sid` ended other than in a wait: owed no more until the lease names it
/// again.
fn disk_ask_stepped(board: &Shelf, sid: &str) {
    board.board_lock().asks.remove(sid);
}

/// The resume's step at tab `sid` ended `typed` (or tries ran out): owed no more.
fn disk_resume_stepped(board: &Shelf, sid: &str, typed: bool) {
    let mut ep = disk_episode(board);
    let spent = match ep.owed.get_mut(sid) {
        Some((_, tries)) => {
            *tries = tries.saturating_sub(1);
            typed || *tries == 0
        }
        None => false,
    };
    if spent {
        ep.owed.remove(sid);
    }
}

const DISK_END_RESTATE: Duration = Duration::from_secs(6 * 3600);

/// The ends the disk watch has told the window.
#[derive(Debug, Default)]
struct DiskEndsSaid {
    settings_read: Option<Instant>,
    plenty: Option<Instant>,
}

impl DiskEndsSaid {
    /// Whether `told` goes to the window at `now` (and, for an end, that it went).
    fn owed(&mut self, told: &HostNote, now: Instant) -> bool {
        let said = match told {
            HostNote::DiskSettingsRead => &mut self.settings_read,
            HostNote::DiskPlenty => &mut self.plenty,
            HostNote::DiskLow { .. }
            | HostNote::DiskLowCleanupOff { .. }
            | HostNote::DiskSettingsOff { .. } => {
                *self = Self::default();
                return true;
            }
            _ => return true,
        };
        if said.is_some_and(|at| now.saturating_duration_since(at) < DISK_END_RESTATE) {
            return false;
        }
        *said = Some(now);
        true
    }
}

/// What a reading of the `[disk]` settings tells the window of itself.
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

/// What a look that runs no pass (the volume at or above the reading's floor, its figure unknown, or the floor `0`) tells the window.
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
                asked: disk_ask::owner_said(),
            })
        }
    }
}

/// The free figure a pass left the volume at.
fn free_after_pass(
    freed: bool,
    free: Option<u64>,
    remeasure: impl FnOnce() -> Option<u64>,
) -> Option<u64> {
    remeasure().or(free.filter(|_| !freed))
}

/// What one look of a run ([`DiskRuns`]) tells the window ([`disk_pass_outcome`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiskOutcome {
    /// At or above the floor: the run ends, and any low-disk line with it.
    Plenty,
    /// Below the floor, not yet [`DISK_SHORT_LOOKS`] looks of the run.
    Short,
    Low {
        looks: u32,
        free_gib: u64,
    },
}

/// The owner is told when the volume stays short, not only when aterm freed nothing.
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

/// The harness state directory the upgrade and the relaunch file their records under (`<aterm state>/harness/<HARNESS>`), or `None` without one.
pub(crate) fn harness_state() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(dir) = STATE_FOR_TEST.with(|dir| dir.borrow().clone()) {
        return Some(dir);
    }
    aterm_types::dirs::state_dir().map(|root| {
        root.join("harness")
            .join(aterm_agent::harness::cli::HARNESS)
    })
}

#[cfg(test)]
thread_local! {
    static STATE_FOR_TEST: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// `f` with [`harness_state`] answering `dir` on this thread.
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

/// The owner's view over this instance's own socket ([`Hooks::upgrade_view`], [`upgrade_drive::View`]).
fn live_view(
    sock: String,
    proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>,
) -> Arc<ViewFn> {
    let place = aterm_primer::home_dir().zip(harness_state());
    let view = Mutex::new(upgrade_drive::View::default());
    Arc::new(move |look, claimed, off, agent_off| {
        let mut view = view.lock().unwrap_or_else(PoisonError::into_inner);
        let mut hand = |rows: &[upgrade_drive::Row], kept: &[String]| {
            let _ = proxy.send_event(crate::Wake::AgentUpgrade {
                rows: rows.to_vec(),
                kept: kept.to_vec(),
            });
        };
        if !look {
            view.stand_down(&mut |rows| hand(rows, &[]));
            return ViewNext::default();
        }
        let Some((home, state)) = place.clone() else {
            return ViewNext::default();
        };
        view.set_claimed(claimed);
        view.set_off(off);
        view.set_agent_off(agent_off);
        let at = view.refresh_keeping(
            &window_drive_opts(home, state, Some(sock.clone())),
            &mut hand,
        );
        ViewNext {
            at,
            follows: view.follows().to_vec(),
            watch: view.watch_due().to_vec(),
            unlisted: view.unlisted().map(Path::to_path_buf),
            unreadable: view.unreadable().to_vec(),
        }
    })
}

const WAKE_FALLBACK: Duration = Duration::from_secs(60);

/// What the roster's file held at a look of the activation thread ([`activation_loop`]): the hash
/// of its bounded read (`accounts::roster_text`: its text, no file, or one print for a file that
/// does not read, a FIFO, a device or one too large among them), or `None` where the host's seat
/// names no roster path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RosterPrint(Option<u64>);

impl RosterPrint {
    /// The roster file of the host's account seat ([`Acts::account_seat`]) as it reads now, or
    /// `None` while the host reads no roster: muted, or with the accounts, the logins and the
    /// usage notes all off ([`accounts_look`]'s own condition), when nothing is opened at all.
    fn of(host: &HostHandle) -> Option<Self> {
        use std::hash::{Hash as _, Hasher as _};
        let switches = &host.shared.switches;
        if switches.mute() || !(switches.accounts_live() || switches.notes()) {
            return None;
        }
        let read = (host.hooks.acts.account_seat)()
            .and_then(|seat| seat.roster)
            .map(|path| accounts::roster_text(&path).ok());
        Some(Self(read.map(|read| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            read.hash(&mut hasher);
            hasher.finish()
        })))
    }
}

/// The activation thread's loop over `wait` until the host shuts down. `wait` is the parked
/// [`ActivationWake::wait`]: `true` for a newer build, `false` for the fallback or a pulled
/// trigger. A roster file that reads otherwise after a wait than at the last look that read it
/// (a row named, changed or dropped there: by hand or by `aterm harness accounts add`) is
/// news of the accounts ([`Switches::accounts_news`]), which no store transaction wakes the
/// host for: a session's loop then decides again what it holds, as the dialog of its own
/// `/login` over a login a row now names, which it backs out of. While the host reads no roster
/// ([`RosterPrint::of`]) the file is not opened, and the last look that read it stands.
fn activation_loop(host: &HostHandle, mut wait: impl FnMut() -> bool) {
    let mut roster = RosterPrint::of(host);
    accounts_look(
        &host.shared.switches,
        &host.hooks,
        &host.shared.login_latches,
        false,
    );
    while !host.shared.lock().shutting_down {
        let newer = wait();
        if host.shared.lock().shutting_down {
            break;
        }
        if newer {
            aterm_log::info!(
                "harness: an activation notice — a newer agent build may be installed (atpkg \
                 landed one, or Claude's own updater moved its link); every supervised session \
                 looks at its next idle point"
            );
            host.note_activation();
        }
        if let Some(read) = RosterPrint::of(host) {
            if roster.is_some_and(|was| was != read) {
                aterm_log::info!("harness: the accounts' roster changed: news of the accounts");
                host.shared
                    .switches
                    .accounts_news
                    .fetch_add(1, Ordering::SeqCst);
            }
            roster = Some(read);
        }
        accounts_look(
            &host.shared.switches,
            &host.hooks,
            &host.shared.login_latches,
            true,
        );
    }
}

/// The agents whose twins atpkg wakes the host for with a marker of their own (`atpkg::activation_notice::marker_name`).
#[cfg(not(target_os = "macos"))]
fn own_marker_agents() -> Vec<Program> {
    Vec::new()
}

/// [`own_marker_agents`] where atpkg publishes its markers.
#[cfg(target_os = "macos")]
fn own_marker_agents() -> Vec<Program> {
    Program::ALL
        .into_iter()
        .filter(|agent| {
            atpkg::vendor_direct::spec(agent.name()).is_some_and(|row| row.live_upgrade)
                && atpkg::activation_notice::marker_name(agent.name())
                    != atpkg::activation_notice::CLAUDE_MARKER
        })
        .collect()
}

/// What the activation wake calls when atpkg replaces the marker of `agent`'s own twin ([`own_marker_agents`], [`ActivationWake::with_agent`]).
fn agent_landing_told(host: HostHandle, agent: Program) -> Box<dyn FnMut() + Send> {
    Box::new(move || {
        aterm_log::info!(
            "harness: an activation notice for {} — a newer build of it may be installed \
             (atpkg landed one); every supervised {} session looks at its next idle point",
            agent.product(),
            agent.product()
        );
        host.note_agent_activation(agent);
    })
}

/// The accounts' upkeep a host makes ([`Acts::accounts_upkeep`]).
#[derive(Clone)]
pub(crate) enum AccountsUpkeep {
    /// Nothing at all, whatever is configured.
    Inert,
    /// [`maintain_accounts_live`]: the person's own instance's.
    Live,
    /// A test's own, handed each step it is made for.
    #[cfg(test)]
    Made(Arc<dyn Fn(UpkeepStep) + Send + Sync>),
}

impl AccountsUpkeep {
    /// Make the upkeep's `step`.
    pub(crate) fn make(&self, step: UpkeepStep) {
        match self {
            Self::Inert => {}
            Self::Live => maintain_accounts_live(step),
            #[cfg(test)]
            Self::Made(upkeep) => upkeep(step),
        }
    }
}

/// [`live_acts`]' accounts' upkeep for this process, and the fact menu's (`crate::fact_menu`).
pub(crate) fn live_accounts_upkeep() -> AccountsUpkeep {
    let not_own = aterm_types::dirs::not_own_account();
    let seam = aterm_types::dirs::test_root().is_none()
        && crate::headless_seam_admits(
            aterm_types::dev_seam!(aterm_agent::harness::accounts_sync::UPKEEP_SEAM).is_some(),
        );
    let arm = accounts_upkeep_arm(not_own, seam);
    if let (AccountsUpkeep::Inert, Some(why)) = (&arm, not_own) {
        aterm_log::info!(
            "harness: accounts-upkeep=inert: {why}, so this instance keeps no fleet accounts \
             (no roster is fetched and no sign-in is started, whatever aterm.toml says)"
        );
    }
    arm
}

/// The live upkeep where the instance runs as its account's own (`not_own` is `None`) or the
/// development seam `accounts_sync::UPKEEP_SEAM` was admitted (`seam`), else the inert one.
const fn accounts_upkeep_arm(not_own: Option<&str>, seam: bool) -> AccountsUpkeep {
    if not_own.is_none() || seam {
        AccountsUpkeep::Live
    } else {
        AccountsUpkeep::Inert
    }
}

/// The accounts' upkeep, live ([`AccountsUpkeep::Live`]).
fn maintain_accounts_live(step: UpkeepStep) {
    use aterm_agent::harness::accounts_sync::{Upkept, upkeep_live};
    static SAID: Mutex<Option<Upkept>> = Mutex::new(None);
    let Some(home) = aterm_primer::home_dir() else {
        return;
    };
    let Some(state) = harness_state() else {
        return;
    };
    let Some(roster) = accounts::default_roster_path() else {
        return;
    };
    let Some(now) = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()) else {
        return;
    };
    aterm_agent::harness::accounts_sync::log_sign_ins_to(|line| {
        aterm_log::info!("harness: accounts-upkeep: {line}");
    });
    let upkept = upkeep_live(&home, &state, &roster, now, step);
    if let Upkept::Configured { renewal: true, .. } | Upkept::Unconfigured { renewal: true, .. } =
        upkept
    {
        aterm_log::info!(
            "harness: accounts-upkeep: a sign-in started or a saved account choice completed"
        );
    }
    let found = match upkept {
        Upkept::Configured { kind, roster, .. } => Upkept::Configured {
            kind,
            roster,
            renewal: false,
        },
        Upkept::Unconfigured {
            fleet_file_ignored, ..
        } => Upkept::Unconfigured {
            fleet_file_ignored,
            renewal: false,
        },
    };
    let mut said = SAID
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *said == Some(found) {
        return;
    }
    *said = Some(found);
    match found {
        Upkept::Unconfigured {
            fleet_file_ignored, ..
        } => aterm_log::info!(
            "harness: accounts-upkeep=unconfigured{}: no fleet accounts source is configured \
             (`[fleet] accounts_source`, or a fleet-accounts.toml a sync handed its source \
             wrote), so no roster is fetched; the rows of a roster that stands are still signed \
             in automatically",
            if fleet_file_ignored {
                " fleet-file=ignored"
            } else {
                ""
            }
        ),
        Upkept::Configured { kind, roster, .. } => {
            aterm_log::info!("harness: accounts-upkeep=configured source={kind} roster={roster}")
        }
    }
}

/// The accounts' upkeep on a thread of its own, kicked by the activation thread at its arming and after every wait ([`upkept`]).
struct Upkeep {
    kick: std::sync::mpsc::SyncSender<()>,
}

impl Upkeep {
    /// The upkeep's thread for `host`, making `upkeep` at each kick.
    fn spawn(host: HostHandle, upkeep: AccountsUpkeep) -> Option<Self> {
        let (kick, kicked) = std::sync::mpsc::sync_channel::<()>(1);
        let spawned = std::thread::Builder::new()
            .name(format!("{THREAD_PREFIX}upkeep"))
            .spawn(move || {
                while kicked.recv().is_ok() {
                    upkeep.make(UpkeepStep::Both);
                    accounts_look(
                        &host.shared.switches,
                        &host.hooks,
                        &host.shared.login_latches,
                        false,
                    );
                }
            });
        match spawned {
            Ok(_) => Some(Self { kick }),
            Err(e) => {
                aterm_log::warn!(
                    "harness: the accounts' upkeep thread could not start: {e}; the activation \
                     thread makes the upkeep itself"
                );
                None
            }
        }
    }

    /// One more upkeep once the one in hand is over.
    fn kick(&self) {
        let _ = self.kick.try_send(());
    }
}

/// The activation thread's `wait` with the accounts' upkeep beside it ([`Upkeep`]).
fn upkept(
    host: &HostHandle,
    wait: impl FnMut() -> bool + Send + 'static,
    upkeep: AccountsUpkeep,
) -> Box<dyn FnMut() -> bool + Send> {
    upkept_on(Upkeep::spawn(host.clone(), upkeep.clone()), wait, upkeep)
}

/// [`upkept`] over the upkeep's `thread` as it started.
fn upkept_on(
    thread: Option<Upkeep>,
    mut wait: impl FnMut() -> bool + Send + 'static,
    upkeep: AccountsUpkeep,
) -> Box<dyn FnMut() -> bool + Send> {
    let Some(thread) = thread else {
        upkeep.make(UpkeepStep::Both);
        return Box::new(move || {
            let newer = wait();
            upkeep.make(UpkeepStep::Both);
            newer
        });
    };
    thread.kick();
    Box::new(move || {
        let newer = wait();
        thread.kick();
        newer
    })
}

/// Start the activation thread, parked on the markers that say a newer agent build is installed
/// ([`activation_wake`]) with the accounts' upkeep beside it.
fn spawn_activation_wake(host: HostHandle) {
    spawn_activation_wake_with(host, |host| {
        let mut wake = activation_wake(host);
        let trigger = wake.trigger();
        (
            Some(trigger),
            host_upkept(host, move || wake.wait(WAKE_FALLBACK)),
        )
    });
}

/// The activation thread's `wait` with the host's own accounts' upkeep beside it ([`upkept`] over [`Acts::accounts_upkeep`]).
fn host_upkept(
    host: &HostHandle,
    wait: impl FnMut() -> bool + Send + 'static,
) -> Box<dyn FnMut() -> bool + Send> {
    upkept(host, wait, host.hooks.acts.accounts_upkeep.clone())
}

/// What [`spawn_activation_wake_with`]'s thread runs on.
type ArmedWake = (Option<WakeTrigger>, Box<dyn FnMut() -> bool + Send>);

type ActivationArm = Box<dyn FnOnce(&HostHandle) -> ArmedWake + Send>;
type ActivationTask = Box<dyn FnOnce() + Send>;
type ActivationSpawn = Arc<dyn Fn(ActivationTask) -> std::io::Result<()> + Send + Sync>;

/// An activation start not yet made: a failed spawn leaves its one-shot arming closure here for
/// the retry.
struct PendingActivation {
    at: Instant,
    arm: Arc<Mutex<Option<ActivationArm>>>,
    spawn: ActivationSpawn,
    thread: std::sync::Weak<Mutex<Option<JoinHandle<()>>>>,
}

#[derive(Default)]
enum ActivationStart {
    #[default]
    Absent,
    Pending(PendingActivation),
    Starting,
    Started,
}

impl ActivationStart {
    const fn next(&self) -> Option<Instant> {
        match self {
            Self::Pending(pending) => Some(pending.at),
            _ => None,
        }
    }
}

/// One due start, claimed under the host lock and attempted after releasing it.
fn try_activation_start(shared: &Arc<Shared>, hooks: &Hooks, now: Instant) -> Option<Instant> {
    let mut pending = {
        let mut state = shared.lock();
        if state.shutting_down {
            return None;
        }
        let at = state.activation.next()?;
        if at > now {
            return Some(at);
        }
        let ActivationStart::Pending(pending) =
            std::mem::replace(&mut state.activation, ActivationStart::Starting)
        else {
            unreachable!("only a pending activation has a deadline")
        };
        pending
    };
    let Some(thread) = pending.thread.upgrade() else {
        shared.lock().activation = ActivationStart::Absent;
        return None;
    };
    let host = HostHandle {
        shared: Arc::clone(shared),
        thread,
        hooks: hooks.clone(),
    };
    let arm = Arc::clone(&pending.arm);
    let spawned = (pending.spawn)(Box::new(move || {
        if host.shared.lock().shutting_down {
            return;
        }
        let arm = arm.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(arm) = arm else { return };
        let (trigger, wait) = arm(&host);
        if let Some(trigger) = trigger {
            let _ = host.shared.wake.set(trigger);
        }
        if host.shared.lock().shutting_down {
            return;
        }
        aterm_log::info!(
            "harness: the activation wake is armed; every session supervised before it \
             looks at its next idle point"
        );
        host.note_activation();
        activation_loop(&host, wait);
    }));
    let mut state = shared.lock();
    if state.shutting_down {
        drop(state);
        return None;
    }
    match spawned {
        Ok(()) => {
            state.activation = ActivationStart::Started;
            None
        }
        Err(e) => {
            pending.at = now + WAKE_FALLBACK;
            let at = pending.at;
            state.activation = ActivationStart::Pending(pending);
            drop(state);
            aterm_log::warn!(
                "harness: the activation wake could not start: {e}; its start is retried \
                 at the host's next activation deadline"
            );
            Some(at)
        }
    }
}

/// [`spawn_activation_wake`] with the wake's arming injected, so a test sees which thread arms it.
fn spawn_activation_wake_with(
    host: HostHandle,
    arm: impl FnOnce(&HostHandle) -> ArmedWake + Send + 'static,
) {
    spawn_activation_wake_on(
        host,
        arm,
        Arc::new(|task| {
            std::thread::Builder::new()
                .name(format!("{THREAD_PREFIX}activation"))
                .spawn(task)
                .map(|_| ())
        }),
    );
}

/// [`spawn_activation_wake_with`] with the thread's spawn injected: an absent start is queued as
/// [`ActivationStart::Pending`] and tried at once.
fn spawn_activation_wake_on(
    host: HostHandle,
    arm: impl FnOnce(&HostHandle) -> ArmedWake + Send + 'static,
    spawn: ActivationSpawn,
) {
    {
        let mut state = host.shared.lock();
        if state.shutting_down || !matches!(state.activation, ActivationStart::Absent) {
            return;
        }
        state.activation = ActivationStart::Pending(PendingActivation {
            at: Instant::now(),
            arm: Arc::new(Mutex::new(Some(Box::new(arm)))),
            spawn,
            thread: Arc::downgrade(&host.thread),
        });
    }
    let _ = try_activation_start(&host.shared, &host.hooks, Instant::now());
    host.ensure_thread();
    ring();
}

/// The activation wake at atpkg's markers and the native link as they stand now ([`spawn_activation_wake`]).
fn activation_wake(host: &HostHandle) -> ActivationWake {
    let home = aterm_primer::home_dir();
    let news = host.clone();
    let state_dir = harness_state();
    let wake_state = state_dir.clone();
    let mut wake = ActivationWake::new(home.as_deref())
        .with_word(
            state_dir
                .as_ref()
                .map(|state| upgrade_drive::word_marker(state)),
        )
        .with_side(
            state_dir
                .as_ref()
                .map(|state| accounts::Store::at(state).wake_path()),
            Box::new(move || {
                news.note_accounts();
                let now = aterm_time::unix_secs()
                    .and_then(|n| i64::try_from(n).ok())
                    .unwrap_or(0);
                if wake_state.as_deref().is_some_and(|dir| {
                    aterm_agent::harness::accounts_sync::any_pending_pin(dir, now)
                }) && let Some(trigger) = news.shared.wake.get()
                {
                    trigger.pull();
                }
            }),
        );
    for secret_mark in [
        aterm_types::dirs::aterm_config_dir().map(|d| d.join("secrets/state.toml")),
        home.as_ref().map(|h| {
            h.join(".secrets")
                .join(aterm_agent::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE)
        }),
        home.as_ref().map(|h| {
            h.join(".secrets")
                .join(aterm_agent::harness::headless_oauth::MAILBOX_TOML_FILE)
        }),
        aterm_types::dirs::aterm_config_dir()
            .map(|d| d.join(aterm_agent::harness::headless_oauth::MAILBOX_TOML_FILE)),
    ] {
        let h = host.clone();
        wake = wake.with_side(
            secret_mark,
            Box::new(move || {
                if let Some(trigger) = h.shared.wake.get() {
                    trigger.pull();
                }
            }),
        );
    }
    for agent in own_marker_agents() {
        wake = wake.with_agent(agent.name(), agent_landing_told(host.clone(), agent));
    }
    wake
}

/// The hook that carries the loop's next try to the window.
fn waiting_hook(send: impl Fn(crate::Wake) + Send + Sync + 'static) -> Arc<WaitingFn> {
    Arc::new(move |sid, plan| {
        send(crate::Wake::HarnessWait {
            sid: sid.to_string(),
            plan,
        });
    })
}

/// The hook that carries the loop's acts and its task's face to the window.
fn fact_hook(send: impl Fn(crate::Wake) + Send + Sync + 'static) -> Arc<FactFn> {
    Arc::new(move |sid, fact| {
        send(crate::Wake::HarnessFact {
            sid: sid.to_string(),
            fact,
        });
    })
}

/// The production host: this process's store, its own control socket, atpkg's activation notice and the owner's word.
pub(crate) fn start_default(
    store: Store,
    sock: String,
    cfg: SupervisorConfig,
    headless: bool,
    suspended: bool,
    proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>,
) -> HostHandle {
    let words = store.clone();
    let (s1, s2, s3, s4) = (store.clone(), store.clone(), store.clone(), store.clone());
    let s6 = store.clone();
    let s7 = store.clone();
    let s8 = store.clone();
    let s5 = store;
    let (k1, k2, k3, k4) = (sock.clone(), sock.clone(), sock.clone(), sock);
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
            needs: Arc::new(move |sid, needs, entry| store_needs(&s7, sid, needs, entry)),
            hold: Arc::new(move |sid, held| store_land_held(&s8, sid, held)),
            claim_epoch: Arc::new(|| CLAIM_EPOCH.load(Ordering::SeqCst)),
            acts: live_acts(s3, k3),
            backoff: Arc::new(|n| aterm_gui_timing::current().of(restart_backoff(n))),
            pause: Arc::new(|pause| aterm_gui_timing::current().of(pause)),
            carry_clock: Arc::new(MonotonicClock),
            upgrade_view: live_view(k4, proxy),
            disk: live_disk(s4, Arc::clone(&note)),
            codex_roster: Arc::new(move || codex_roster_of(&s5)),
            note,
            log_usage: Arc::new(move |sid, h5, d7| log_usage(&s6, sid, h5, d7)),
        },
    );
    spawn_activation_wake(host.clone());
    host
}

/// [`Hooks::needs`], live.
pub(crate) fn store_needs(store: &Store, sid: &str, needs: &[String], entry: &str) -> bool {
    let told = crate::needs::HarnessNeeds::told(needs, entry).map(Arc::new);
    let found = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .map(|h| Arc::clone(&h.ctx));
    let Some(ctx) = found else {
        return false;
    };
    let standing = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .attention_owners
        .get(NEEDS_ATTENTION_OWNER)
        .map(str::to_string);
    ctx.timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .set_harness_needs(told, standing.as_deref())
}

/// [`Hooks::hold`], live: tab `sid`'s Land hold as its loop told it.
pub(crate) fn store_land_held(
    store: &Store,
    sid: &str,
    held: Option<aterm_agent::harness::acts::LandHeld>,
) -> bool {
    let found = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .map(|h| Arc::clone(&h.ctx.timeline));
    let Some(timeline) = found else {
        return false;
    };
    timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .set_land_held(held)
}

/// Live [`Hooks::log_usage`] implementation recording `mark - usage h5=<pct> d7=<pct>` on tab `sid`.
pub(crate) fn log_usage(store: &Store, sid: &str, h5: Option<u32>, d7: Option<u32>) {
    let found = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .map(|h| Arc::clone(&h.ctx.timeline));
    let Some(timeline) = found else {
        return;
    };
    let mut g = timeline.lock().unwrap_or_else(PoisonError::into_inner);
    let at = g.slog().now();
    g.slog_mut().observe_usage(h5, d7, at);
    g.slog_take_wake();
}

/// The live acts' one row writer.
fn live_row(store: Store) -> Arc<RowFn> {
    Arc::new(move |sid, kind, r| {
        aterm_log::info!("harness @{sid}: {}", r.line_as(kind));
        log_act(&store, sid, kind, r);
    })
}

/// The hosted lanes say they relaunched, and the harness labels its restart.
pub(crate) fn log_act(store: &Store, sid: &str, kind: &str, r: &upgrade_drive::Report) {
    let step = r.step.as_str();
    let Some(rule) = aterm_agent::harness::acts::relaunch_act(kind, step) else {
        return;
    };
    let label = aterm_agent::harness::acts::relaunch_label(kind, step, &r.to);
    let Some(timeline) = timeline_of(store, sid) else {
        return;
    };
    let mut g = timeline.lock().unwrap_or_else(PoisonError::into_inner);
    let at = g.slog().now();
    g.slog_mut()
        .observe_relaunch_act(&rule, upgrade_drive::started(step), label.as_deref(), at);
    g.slog_take_wake();
}

/// The disk lane's resume came back `step`: once its carry-on is typed, the harness labels the
/// moment (`resuming after disk`).
pub(crate) fn disk_resumed(store: &Store, sid: &str, step: String) -> String {
    if step == "typed" {
        log_label(store, sid, aterm_agent::harness::acts::DISK_RESUME_LABEL);
    }
    step
}

/// The harness labels a moment of its own in session `sid`'s log, `words` its closed template
/// ([`crate::session_log::SessionLog::harness_label`]).
pub(crate) fn log_label(store: &Store, sid: &str, words: &str) {
    let Some(timeline) = timeline_of(store, sid) else {
        return;
    };
    let mut g = timeline.lock().unwrap_or_else(PoisonError::into_inner);
    let at = g.slog().now();
    g.slog_mut().harness_label(words, at);
    g.slog_take_wake();
}

fn timeline_of(
    store: &Store,
    sid: &str,
) -> Option<Arc<Mutex<crate::session_timeline::SessionTimeline>>> {
    store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_sid(&aterm_session::SessionId::new(sid))
        .map(|h| Arc::clone(&h.ctx.timeline))
}

/// Remove the hooks an older aterm wrote into the user's Claude Code settings, off the winit thread.
pub(crate) fn sweep_legacy_hooks(auto_prime: bool) {
    let Some(home) = aterm_primer::home_dir() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name(format!("{THREAD_PREFIX}hook-sweep"))
        .spawn(move || sweep_claude_settings_in(&home, auto_prime));
}

/// The sweep's two passes over `<home>/.claude/settings.json`, in order.
pub(crate) fn sweep_claude_settings_in(home: &std::path::Path, auto_prime: bool) {
    match aterm_primer::remove_aterm_hooks_in(home) {
        Ok(aterm_primer::HookRemoval::Nothing) => {}
        Ok(removal) => {
            aterm_log::info!("harness: legacy Claude hooks removed: {removal:?}");
        }
        Err(e) => aterm_log::warn!("harness: legacy Claude hooks left in place: {e}"),
    }
    if !auto_prime {
        aterm_log::debug!("harness: Claude Code defaults left alone (agents_auto_prime = false)");
        return;
    }
    match aterm_primer::add_claude_defaults_in(home) {
        Ok(aterm_primer::DefaultsWrite::Nothing) => {}
        Ok(added) => aterm_log::info!("harness: Claude Code defaults added: {added:?}"),
        Err(e) => aterm_log::warn!("harness: Claude Code defaults not added: {e}"),
    }
}

#[cfg(any(test, feature = "bench-support"))]
impl Acts {
    /// Acts over no real session.
    pub(crate) fn inert() -> Self {
        Acts {
            open: Arc::new(|_| false),
            holds: Arc::new(|_, _| None),
            launch: Arc::new(|_| None),
            due: Arc::new(|_| Due::No),
            behind: Arc::new(|_, _| Behind::Nothing),
            step: Arc::new(|_, _| "current".to_string()),
            notice: Arc::new(|_, _| "current".to_string()),
            owed: Arc::new(|_, _| false),
            resume: Arc::new(|_, _, _| "current".to_string()),
            settle: Arc::new(|_| "current".to_string()),
            disk_resume: Arc::new(|_, _| "wait:inert".to_string()),
            disk_free: Arc::new(|_, _, _, _| "wait:inert".to_string()),
            disk_board: {
                static INERT: Shelf = Shelf::new();
                &INERT
            },
            jobs_board: {
                static INERT: crate::vitals_jobs::JobsBoard = crate::vitals_jobs::JobsBoard::new();
                &INERT
            },
            // Nothing is proved alive: no call is told of.
            leader_lives: |_, _| false,
            program_check: {
                static INERT: Mutex<crate::program_check_watch::Board> =
                    Mutex::new(crate::program_check_watch::Board::new());
                &INERT
            },
            occupancy: Arc::new(|_| aterm_agent::supervise::occupancy::OccupancyReading::default()),
            occupancy_pace: Arc::new(|_, _, _| aterm_agent::supervise::occupancy::PaceGrant::Local),
            task_fact: Arc::new(|_, _| aterm_agent::supervise::occupancy::NoteOutcome::NoTask),
            wakes: Arc::new(|_| aterm_agent::supervise::occupancy::WakesSeen::default()),
            wake_told: Arc::new(|_, _, _, _| {}),
            wake_claim: Arc::new(|_, _, _, _, _| {
                aterm_agent::supervise::occupancy::WakeClaim::Gone
            }),
            wake_release: Arc::new(|_, _, _, _| {}),
            wakes_left: Arc::new(|_| {}),
            snapshot: Arc::new(|_| None),
            point_read: INERT_POINT_READ,
            follow: Arc::new(|_, _| Foreground::Shell),
            status: Arc::new(|_| None),
            exit_look: Arc::new(|_, _| None),
            exited: Arc::new(|_, _| true),
            asked: Arc::new(|_, _| None),
            offered: Arc::new(|_| None),
            relaunch: Arc::new(|_, _, _, _, _, _| "refused:inert".to_string()),
            self_copy: Arc::new(|_| None),
            live_self_copy: Arc::new(|_| None),
            self_idle: Arc::new(|_, _, _| false),
            self_relaunch: Arc::new(|_, _, _, _, _| "refused:inert".to_string()),
            self_snapshot: Arc::new(|_, _, _| None),
            restarted: Arc::new(|_, _, _| false),
            carry: Arc::new(|_, _, _, _, _| "refused:inert".to_string()),
            relaunch_restored: Arc::new(|_, _, _, _| "refused:inert".to_string()),
            earlier_tab: Arc::new(|_, _| {}),
            adopted_gone: Arc::new(|_, _| None),
            in_flight_here: Arc::new(|_, _| false),
            carry_in_flight: Arc::new(|_, _| None),
            restart: Arc::new(|_, _, _| "refused:inert".to_string()),
            tasked: Arc::new(|_, _, _| None),
            finished: Arc::new(|_, _, _| None),
            limit_reset: Arc::new(|_| None),
            transcript_tail: Arc::new(|_| None),
            repo_work: Arc::new(|_| Default::default()),
            transcript: Arc::new(|_| None),
            task_exited: Arc::new(|_| {}),
            startups: Arc::new(|_| Vec::new()),
            startup_mark: Arc::new(|_, _| {}),
            standing_limit: Arc::new(|_, _| None),
            account_wall: Arc::new(|_, _, _| ("inert", None)),
            accounts_upkeep: AccountsUpkeep::Inert,
            account_usage: Arc::new(|_, _, _, _, _| "inert"),
            account_gone: Arc::new(|_| {}),
            account_seat: Arc::new(|| None),
            account_env: Arc::new(|_| None),
            account_settings: Arc::new(|_, _, _| true),
            tap_born: Arc::new(|_| None),
            account_restart: Arc::new(|_, _| None),
            account_made: Arc::new(|_, _, _| {}),
            account_cold: Arc::new(|_| None),
            hold: Arc::new(|_, _| true),
            route: Arc::new(|_, _, _| Route::Custom("inert".to_string())),
            memory: Arc::new(|_, _| None),
            watch: Arc::new(|_, _, _| upgrade_drive::Watched::default()),
        }
    }
}

#[cfg(any(test, feature = "bench-support"))]
impl HostHandle {
    /// A host that supervises `sids`, each a Claude Code tab, with `body` as every worker's loop and every act inert.
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
            needs: Arc::new(|_, _, _| false),
            hold: Arc::new(|_, _| false),
            claim_epoch: Arc::new(|| 0),
            acts,
            backoff: Arc::new(|_| Duration::from_millis(10)),
            pause: Arc::new(|pause| aterm_gui_timing::Timing::FAST.of(pause)),
            carry_clock: Arc::new(MonotonicClock),
            upgrade_view: Arc::new(|_, _, _, _| ViewNext::default()),
            disk: Arc::new(|_| {}),
            codex_roster: Arc::new(Vec::new),
            note: Arc::new(|_| {}),
            log_usage: Arc::new(|_, _, _| {}),
        };
        Self::start(SupervisorConfig::default(), false, false, hooks)
    }
}

#[cfg(all(test, unix))]
const SUCCESSOR_TEST_LANDED: u32 = 5151;

#[cfg(test)]
impl HostHandle {
    /// A host over no real session (nothing supervised, every act inert) whose restored relaunch is `act`.
    pub(crate) fn for_restored_test(act: Arc<RestoredFn>) -> Self {
        Self::restored_test_host(act, Arc::new(|_| false))
    }

    /// [`Self::for_restored_test`] whose every tab is open.
    #[cfg(unix)]
    pub(crate) fn for_restored_test_with_open_tabs(act: Arc<RestoredFn>) -> Self {
        Self::restored_test_host(act, Arc::new(|_| true))
    }

    /// [`Self::for_restored_test`] whose note of a restored agent's earlier tab ([`Acts::earlier_tab`]) is recorded in `noted`.
    pub(crate) fn for_restored_test_noting(
        act: Arc<RestoredFn>,
        noted: Arc<Mutex<Vec<(String, String)>>>,
    ) -> Self {
        let mut acts = Acts::inert();
        acts.earlier_tab = Arc::new(move |sid, was| {
            noted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((sid.to_string(), was.to_string()));
        });
        Self::restored_test_host_with(act, Arc::new(|_| false), acts)
    }

    /// A seamless update's successor whose tabs are all open with no agent named, started suspended.
    #[cfg(unix)]
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
                    profile: None,
                    tab: sid.to_string(),
                    pid: SUCCESSOR_TEST_LANDED,
                    start: "Wed Oct 15 01:02:03 2026".to_string(),
                    shell: 0,
                    program: PathBuf::from("/opt/claude"),
                    argv: vec!["/opt/claude".to_string()],
                    session: None,
                    cwd: "/".to_string(),
                    version: None,
                    run: aterm_agent::harness::relaunch::AgentRun::Claude,
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
                refused: false,
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
            needs: Arc::new(|_, _, _| false),
            hold: Arc::new(|_, _| false),
            claim_epoch: Arc::new(|| 0),
            acts,
            backoff: Arc::new(|_| Duration::from_millis(10)),
            pause: Arc::new(|pause| aterm_gui_timing::Timing::FAST.of(pause)),
            carry_clock: Arc::new(MonotonicClock),
            upgrade_view: Arc::new(|_, _, _, _| ViewNext::default()),
            disk: Arc::new(|_| {}),
            codex_roster: Arc::new(Vec::new),
            note: Arc::new(|_| {}),
            log_usage: Arc::new(|_, _, _| {}),
        };
        Self::start(SupervisorConfig::default(), false, true, hooks)
    }

    fn restored_test_host(act: Arc<RestoredFn>, open: Arc<WantedFn>) -> Self {
        Self::restored_test_host_with(act, open, Acts::inert())
    }

    /// [`Self::restored_test_host`] over `acts` (every other act of theirs kept).
    fn restored_test_host_with(act: Arc<RestoredFn>, open: Arc<WantedFn>, mut acts: Acts) -> Self {
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
            needs: Arc::new(|_, _, _| false),
            hold: Arc::new(|_, _| false),
            claim_epoch: Arc::new(|| 0),
            acts,
            backoff: Arc::new(|_| Duration::from_millis(10)),
            pause: Arc::new(|pause| pause),
            carry_clock: Arc::new(MonotonicClock),
            upgrade_view: Arc::new(|_, _, _, _| ViewNext::default()),
            disk: Arc::new(|_| {}),
            codex_roster: Arc::new(Vec::new),
            note: Arc::new(|_| {}),
            log_usage: Arc::new(|_, _, _| {}),
        };
        Self::start(SupervisorConfig::default(), false, false, hooks)
    }

    /// Tab `sid`'s relaunch on exit is owed on `snap` (`None`: owed no more), as a worker's exit handling leaves [`Kept::owed`].
    pub(crate) fn owe_for_test(&self, sid: &str, snap: Option<Snapshot>) {
        self.owe_asked_for_test(sid, snap, None);
    }

    /// [`Self::owe_for_test`] with the explicit restart's `asked` cause ([`OwedRelaunch::asked`]).
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

    /// The restored queue's worker has ended (or never ran).
    pub(crate) fn restored_worker_ended(&self) -> bool {
        !self
            .shared
            .restored
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
    }
}

#[path = "harness_disk_ask.rs"]
mod disk_ask;

#[cfg(test)]
#[path = "harness_host_tests.rs"]
mod tests;

#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "IsAlab",
    reason = "That the agent is codex-alab is the host's admission (a proven self-claim, a \
              copy, or an ALab image: harness_host::codex_alab_runs), read by its roster pass; \
              a_codex_alab_end_with_no_copy_is_nobodys hands that admission to the real host as \
              its test roster's stamp (the vendor fact withheld), and the image read itself, \
              self_claim::runs_alab_build, is driven against a real process by \
              control_self_claim_tests' a_claim_is_proven_by_its_connections_peer_leading_the_foreground."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "ReleaseHandsToLane",
    reason = "Negative control (Buggy=1): the reader before the admission flip of 2026-10-16, \
              which read a codex-alab's end with no copy through the Codex lane; \
              a_codex_alab_end_with_no_copy_is_nobodys projects what the real reader did (the \
              lane's look or relaunch, waited for as the decision is) and checks that state \
              against the model: the state only this mutant leaves, that end read by the lane."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "Copied",
    reason = "aterm's copy of the self-claimed agent's record is read by the control verbs               while the proven claim stands (self_claim::refresh, at the claim and each               meta-change); a_self_supervised_agents_exit_is_decided_by_its_copy_alone puts               one in place for the reader."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "HandOn",
    reason = "Another hand landing on the session (a hold, a lease, a named turn, another \
              supervisor's claim) is the fabric's and the control verbs'; \
              the_restart_acts_kill_waits_out_another_hand drives a real fleet hold."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "ClaimOn",
    reason = "Another supervisor's claim landing on the session is the control verbs' \
              (`meta set supervisor`); \
              a_stopped_programs_end_under_another_supervisors_claim_is_that_supervisors \
              drives one through the reader's status, and \
              an_acts_end_read_under_a_hold_is_relaunched_once_it_lifts (its `claim` case) one \
              beside a hold."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "SelfClaimOn",
    reason = "The agent's own PROVEN self-claim landing is the control verbs' (`meta set \
              supervisor codex-alab@<pid>`, proven by the kernel's peer pid and the foreground \
              group); an_acts_end_under_a_dead_proven_self_claim_is_relaunched puts the real one \
              on the session's meta and drives the real act under it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "SelfClaimOff",
    reason = "The proven self-claim given back, or cleared once its process left the front, is \
              the control verbs' and the status sweep's (session_timeline::release_proven_if_gone)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "CommitDropsTheCopy",
    reason = "Negative control (Buggy=1): the crash rule's relaunch owed in the outgoing \
              instance's memory alone, which a Commit lost; \
              a_self_claimed_relaunch_owed_at_a_commit_carries_its_validated_line observes the \
              real host carry it on its line (Kept::owed) and take it up."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "DeadSelfClaimTakesTheExit",
    reason = "Negative control (Buggy=1): a reader that read a proven self-claim whose process \
              ended as a live supervisor's and left it the act's end; \
              an_acts_end_under_a_dead_proven_self_claim_is_relaunched observes the real reader \
              relaunch it, the state only this mutant leaves being that end left to nobody."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "HandOff",
    reason = "That hand lifted: the same test lifts the real fleet hold and observes the \
              act's KILL follow."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "LeftToTheHolder",
    reason = "Negative control (Buggy=1): the reader before ruling 20 of 2026-10-14, which \
              left an act's end read under a hold, a lease or a named turn to that holder; \
              an_acts_end_read_under_a_hold_is_relaunched_once_it_lifts observes the real \
              reader carry it through a fleet hold, a state only this mutant reaches otherwise."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "CarriedIntoAClaim",
    reason = "Negative control (Buggy=1): ruling 20's first reader, which asked another \
              supervisor's claim only at an exit's first read, so a claim that landed while \
              the end was carried through a hold was asked at no later look; \
              an_acts_end_read_under_a_hold_is_relaunched_once_it_lifts (its `claim` case) \
              observes the real \
              reader leave that end to the claim, the state only this mutant leaves being a \
              relaunch typed into the claimed tab."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "DecidedWhileRunning",
    reason = "Negative control (Buggy=1): 1a733ed60's reader, which settled an exit whose \
              program still ran (stopped away from its tab) as left; on_agent_left decides \
              only once it ended, which \
              a_stamped_program_stopped_under_a_hand_that_came_and_went_is_relaunched drives."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "KillUnderHand",
    reason = "Negative control (Buggy=1): 1a733ed60's restart_act::kill, which re-read only \
              the leader; the shipped kill reads restart_act::another_hand before each send."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "Hears",
    reason = "The program's own SIGTERM handler: the hearing stand-in removes its record and \
              exits 0, which this Tier-1 observes by its real exit status."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "EndsOtherwise",
    reason = "Anyone else ending the program (a person's /exit, a crash, a shell's kill): the \
              negative control drives it with a bare `signal term`."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "Commit",
    reason = "A seamless update's Commit is the handoff's (its own models); here it is the \
              stamp a cut act left on disk with no thread, which the carrier finishes."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "SignalBeforeStamp",
    reason = "Negative control (Buggy=1): no shipped code signals before restart_act::begin \
              stamps; the bare `signal term` control shows such an exit read as the person's."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "CommitForgetsStamp",
    reason = "Negative control (Buggy=1): round fourteen's RemedySent, kept in the instance's \
              memory; the stamp is a file and no Commit touches it."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "TermOnly",
    reason = "Negative control (Buggy=1): an act with no escalation; the deaf case shows the \
              real act's SIGKILL."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "StampAfterEnd",
    reason = "Negative control (Buggy=1): the act stamps only a leader that leads its tab, \
              and takes the stamp back when it sent nothing (restart_act::term's re-read)."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "TakeBackAlways",
    reason = "Negative control (Buggy=1): 3eaa8f83c's second act, whose take-back removed the \
              first act's stamp; restart_act::take_back keeps one it joined, which \
              a_second_restart_that_sends_nothing_leaves_the_first_acts_stamp drives."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "HarnessRestartStamp",
    action = "WatchPerAct",
    reason = "Negative control (Buggy=1): 5838fac39's Stamped::signal, which started a watch \
              per act; restart_act::watch keeps one per process, which \
              many_restarts_of_one_program_run_one_watch observes as the model's esc."
)]
/// The real act's observed state as the model's (`aterm_models_harness::harness_restart_stamp_model`).
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn harness_restart_stamp_waivers() {}
