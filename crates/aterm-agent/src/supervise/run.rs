// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor loop driving worker sessions over [`Ctl`] (`aterm-ctl` or relay).
//!
//! Waits for a worker's turn to end ([`Session::await_turn`]), evaluates approval
//! boxes via [`super::policy::approval::decide`], executes turn-end continuations
//! and limit/login/error waits via [`super::policy::turn_end::decide_turn_end`],
//! rides out transient server outages, and records decisions to the ledger and journal.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::approvals::{self, Ledger};
use super::classify::DEFAULT_PYTHON_ALLOW;
use super::config::Approve;
use super::config::{Autopilot, SupervisorConfig};
use super::fx::Fx;
use super::journal::{CLEARED_MEMORY, CLEARED_TURN_END, Journal};
use super::limit::{self, ResetSpec};
use super::phase::{Phase, busy_signal, last_said_index, last_said_row, status_row};
use super::policy::approval::{Decision, footer_mode, readings_read_only};
use super::policy::exception::Exception;
use super::policy::reset::{
    KeepWhy, ResetAsk, ResetEnd, ResetGo, ResetNo, ResetOffer, ResetOutcome, ResetWord,
};
use super::policy::sleeps::{FirstLook, SleepBook, SleepStamp};
use super::policy::turn_end::{
    AccountAdvice, AccountBad, AccountPoint, FirstRead, ModelSwitch, ProbeGate, RULE_API_BACK,
    RULE_API_CUTOFF, RULE_API_RETRY, RULE_LIMIT_RESUME, Reach, TurnEndAction, TurnEndReading,
    TurnEndState, TurnEndTiming, WaitKind, is_continue_rule, said_mark,
};
use super::prompt::{
    CancelEffect, Prompt, PromptKind, PromptV2, parse_prompt, prompt_box_first_row, prompt_box_span,
};
use super::screen::{HumanInput, Screen, parse_text_json};
use aterm_phase::{Program, ScreenReader};

/// Exit code, stdout, and stderr from a single `aterm-ctl` request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CtlReply {
    /// Client exit code (`0` ok, `124` timeout, `1` error, `75` unserved).
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CtlReply {
    pub const fn ok(&self) -> bool {
        self.code == 0
    }
    pub const fn timed_out(&self) -> bool {
        self.code == 124
    }
    /// Returns `true` if the server rejected the verb form as a usage error.
    pub fn usage_error(&self) -> bool {
        self.stderr.contains("ERR usage")
    }
    /// Returns the `ERR ...` text stripped of any leading `aterm-ctl: ` prefix.
    pub fn err_text(&self) -> &str {
        let line = self.stderr.trim();
        line.strip_prefix("aterm-ctl:")
            .map(str::trim)
            .unwrap_or(line)
    }
    /// Returns `true` if the reply is a bare `ERR` with no trailing message.
    pub fn bare_err(&self) -> bool {
        !self.ok() && self.err_text() == "ERR"
    }
    /// Returns `true` if the server rejected the verb form with a usage error or bare `ERR`.
    pub fn unknown_form(&self) -> bool {
        self.usage_error() || self.bare_err()
    }
    /// Returns `true` if the `OK` reply carries `warn=input-unread`.
    pub fn warned_input_unread(&self) -> bool {
        let warned = |l: &str| l.split_whitespace().any(|w| w == "warn=input-unread");
        self.ok()
            && match self.turn_verdict() {
                Some(verdict) => warned(verdict),
                None => self.stdout.lines().next().is_some_and(warned),
            }
    }
    /// Returns `true` if the reply error starts with `ERR <what>`.
    pub fn is_err(&self, what: &str) -> bool {
        !self.ok() && self.err_text().starts_with(&format!("ERR {what}"))
    }
    /// Parses the `seq=<n>` content baseline stamped on an input verb reply.
    pub fn seq(&self) -> Option<u64> {
        let rest = &self.stdout[self.stdout.find("seq=")? + "seq=".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }
    /// Returns `true` if a guarded write matched no row (`OK skipped`).
    pub fn skipped(&self) -> bool {
        self.ok() && self.stdout.trim_start().starts_with("OK skipped")
    }
    /// Returns `true` if a generation-fenced press was skipped because the screen changed.
    pub fn skipped_changed(&self) -> bool {
        self.skipped() && self.stdout.contains("reason=changed")
    }
    /// Returns `true` if a human-count-fenced press was skipped due to recent user input.
    pub fn skipped_person(&self) -> bool {
        self.skipped() && self.stdout.contains("reason=person")
    }
    /// Extracts the `turn submitted=<0|1> ...` verdict line from stderr or stdout.
    pub fn turn_verdict(&self) -> Option<&str> {
        self.stderr
            .lines()
            .chain(self.stdout.lines().take(1))
            .find(|l| l.contains("submitted="))
    }
    /// Returns `true` if the `turn` verdict reports `submitted=1`.
    pub fn submitted(&self) -> bool {
        self.turn_verdict()
            .is_some_and(|v| v.split_whitespace().any(|w| w == "submitted=1"))
    }
    /// Returns `true` if `turn` pressed Enter (`pressed>=1`) without confirming submission (`submitted=0`).
    pub fn pressed_unverified(&self) -> bool {
        self.turn_verdict().is_some_and(|v| {
            let words = || v.split_whitespace();
            words().any(|w| w == "submitted=0")
                && words().any(|w| {
                    w.strip_prefix("pressed=")
                        .and_then(|n| n.parse::<u32>().ok())
                        .is_some_and(|n| n >= 1)
                })
        })
    }
    /// Parses `seq=<n>` from the `turn` verdict line.
    #[cfg(all(test, unix))]
    pub(crate) fn turn_seq(&self) -> Option<u64> {
        let v = self.turn_verdict()?;
        v.split_whitespace()
            .find_map(|w| w.strip_prefix("seq="))
            .and_then(|n| n.parse().ok())
    }
    /// Returns `true` if the request was not served due to a transport or server disconnect.
    pub fn lost(&self) -> bool {
        if self.ok() {
            return false;
        }
        if self.code == i32::from(aterm_ctl::EXIT_REPLACED) {
            return true;
        }
        let lines = self.client_lines();
        match lines.iter().find(|l| l.starts_with("ERR")) {
            Some(err) => turned_away(err),
            None => lines.iter().any(|l| unsent(l) || cut(l)),
        }
    }

    /// Returns `true` if an unserved request was guaranteed not to have been executed.
    pub fn untouched(&self) -> bool {
        if self.ok() || self.code == i32::from(aterm_ctl::EXIT_REPLACED) {
            return false;
        }
        let lines = self.client_lines();
        match lines.iter().find(|l| l.starts_with("ERR")) {
            Some(err) => turned_away(err),
            None => lines.iter().any(|l| unsent(l)) && !lines.iter().any(|l| cut(l)),
        }
    }

    fn client_lines(&self) -> Vec<&str> {
        self.stderr
            .lines()
            .map(|l| {
                let l = l.trim();
                l.strip_prefix("aterm-ctl:").map_or(l, str::trim)
            })
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// Returns `true` if the request was unserved (including `no such session` during an active outage).
    pub fn unserved(&self, in_outage: bool) -> bool {
        self.lost() || (in_outage && self.is_err("no such session"))
    }
}

const CUT: &[&str] = &[
    "server closed the connection without responding",
    "server hung up before the complete",
    "server hung up mid-body",
    "Connection reset",
    "forcibly closed",
    "Broken pipe",
    "Resource temporarily unavailable",
    "timed out",
];
const UNSENT: &[&str] = &[
    "Connection refused",
    "actively refused",
    "the instance took no request",
];
const LOST_SOCKET: &[&str] = &["No such file or directory", "cannot find the file"];

fn unsent(line: &str) -> bool {
    UNSENT.iter().any(|m| line.contains(m))
        || (line.starts_with("connect ") && LOST_SOCKET.iter().any(|m| line.contains(m)))
}

fn cut(line: &str) -> bool {
    CUT.iter().any(|m| line.contains(m))
}
const TURNED_AWAY: &[&str] = &[
    "ERR control server busy",
    "ERR auth",
    "ERR main thread stalled",
];

const MAIN_THREAD_STALLED: &str = "main thread stalled";

fn turned_away(err: &str) -> bool {
    TURNED_AWAY.iter().any(|&phrase| leads_with(err, phrase))
        || err.strip_prefix("ERR ").is_some_and(|body| {
            body.match_indices(": ")
                .any(|(at, sep)| leads_with(&body[at + sep.len()..], MAIN_THREAD_STALLED))
        })
}

fn leads_with(line: &str, phrase: &str) -> bool {
    line.strip_prefix(phrase)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', ';']))
}

/// Transport seam for executing control requests against a target session.
pub trait Ctl {
    /// Executes `args` against the target session after any `@sid` selector.
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String>;

    /// Executes a host-level request (`args`) against the instance hosting `sid`.
    fn call_at_host_of(&mut self, sid: &str, args: &[&str]) -> Result<CtlReply, String> {
        let _ = sid;
        self.call(args)
    }

    /// Returns an optional handle that aborts any in-flight and subsequent requests from another thread.
    fn interrupter(&self) -> Option<Interrupter> {
        None
    }
}

/// Thread-safe callback handle returned by [`Ctl::interrupter`].
pub type Interrupter = Box<dyn Fn() + Send + Sync>;

impl Ctl for crate::CtlClient {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.run_raw(args)
    }
}

#[derive(Debug, Default)]
pub(super) struct ActMarks {
    pending: Vec<String>,
    off: bool,
}

pub(super) const ACT_MARKS_MAX: usize = 32;

impl ActMarks {
    pub(super) fn queue(&mut self, mark: String) {
        if self.off {
            return;
        }
        if self.pending.len() >= ACT_MARKS_MAX {
            self.pending.remove(0);
        }
        self.pending.push(mark);
    }
}

pub(super) fn act_mark_latches(reply: &CtlReply) -> bool {
    reply.is_err("unknown verb") || reply.is_err("usage: mark") || reply.is_err("mark:")
}

/// Server control capabilities probed lazily per supervisor session.
#[derive(Debug, Default, Clone, Copy)]
pub struct Caps {
    /// Whether `key if-gen=<epoch>.<seq>` is supported.
    pub gen_fence: Option<bool>,
    /// Whether `key act=approve|deny|answer` is supported.
    pub act_word: Option<bool>,
    /// Whether `turn submit=guarded:<re>` is supported.
    pub guarded_submit: Option<bool>,
    /// Whether `send if-gen=<epoch>.<seq>` is supported.
    pub send_gen: Option<bool>,
    /// Whether `await agent <word>` is supported.
    pub await_agent: Option<bool>,
    /// Whether `status` reports a measured `input=` age.
    pub input: Option<bool>,
}

/// Observed turn-end state comprising the classified [`Phase`] and [`Screen`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub phase: Phase,
    pub screen: Screen,
    /// Whether the wait budget expired while the session remained busy.
    pub timed_out: bool,
}

/// Outcome of an [`IdleHost::at_idle`] step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostStep {
    /// Journal description line (`HOST seq=<n> <line>`).
    pub line: String,
    /// Whether the step submitted input to or terminated the agent.
    pub moved: bool,
    /// Optional host turn typed into the agent whose response is awaited next.
    pub typed: Option<super::policy::turn_end::HostTurn>,
    /// Whether this step repeats the previous wait at the same point.
    pub repeat: bool,
}

/// Turn-end policy context offered to [`IdleHost::at_idle_on`] at an idle point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdlePoint {
    /// Whether the task has landed and been defended under Autopilot Land (`Nothing left:`).
    pub done_under_land: bool,
}

/// Reason a busy break or idle point was withheld from [`IdleHost`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// Break offered only on a busy read.
    NotBusy,
    /// Busy read includes foreground work beyond background tasks.
    NoBackgroundWait,
    /// Usage-limit, login, or API-error wall is on screen.
    Wall,
    /// Background break has not yet settled for [`background_settle`].
    Settle,
    /// Usage-limit episode is currently open.
    LimitEpisode,
    /// Loop is held for a server-published stall.
    Stall,
    /// Previous supervisor action has not yet been taken by the agent.
    ActUntaken,
    /// Supervisor action is currently in flight at an idle point.
    ActInFlight,
    /// Point is not idle (question, approval box, or busy turn).
    NotIdle,
    /// Screen reader does not vouch for an authoritative idle reading.
    NotAuthoritative,
    /// Approval prompt box is on screen.
    Prompt,
    /// System recently woke from sleep and is within the post-wake settle window.
    WakeSettle,
    /// Session is running on usage credits with `[harness] usage_credits = false`.
    Credits,
    /// User typed recently or another driver holds the session.
    Person,
    /// Session is halted (`hold=1`).
    Halted,
    /// Task is finished and left to the user's own exchange.
    Exchange,
    /// Session autopilot mode does not permit this continuation.
    Autopilot,
    /// Agent memory ceiling exceeded under host memory pressure.
    Memory,
    /// Agent route to its API was measured down.
    Reach,
    /// User draft text is present in the composer.
    Draft,
    /// Launch card is on screen with no user task beneath it.
    Taskless,
    /// A manager waits on the session (`TurnEndState::managed`): the occupancy door types it
    /// nothing until a manager's own note can carry the fact.
    Managed,
    /// The host withholds the session's continuations (`IdleHost::withholds_turn_end`: a full
    /// disk) or owns its turn ends (`IdleHost::owns_turn_end`: the upgrade's wind-down).
    HostWithheld,
    /// The upgrade's notice is due (`IdleHost::upgrade_due`): a nudge waits for it.
    UpgradeDue,
    /// This break's one line went already: the host's (the upgrade's notice).
    HostLine,
}

impl Guard {
    /// All guards in [`Self::code`] order.
    pub const ALL: [Guard; 25] = [
        Guard::NotBusy,
        Guard::NoBackgroundWait,
        Guard::Wall,
        Guard::Settle,
        Guard::LimitEpisode,
        Guard::Stall,
        Guard::ActUntaken,
        Guard::ActInFlight,
        Guard::NotIdle,
        Guard::NotAuthoritative,
        Guard::Prompt,
        Guard::WakeSettle,
        Guard::Credits,
        Guard::Person,
        Guard::Halted,
        Guard::Exchange,
        Guard::Autopilot,
        Guard::Memory,
        Guard::Reach,
        Guard::Draft,
        Guard::Taskless,
        Guard::Managed,
        Guard::HostWithheld,
        Guard::UpgradeDue,
        Guard::HostLine,
    ];

    /// Returns the short token (`guard=`) for this guard.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Guard::NotBusy => "not-busy",
            Guard::NoBackgroundWait => "no-background-wait",
            Guard::Wall => "wall",
            Guard::Settle => "settle",
            Guard::LimitEpisode => "limit-episode",
            Guard::Stall => "stall",
            Guard::ActUntaken => "act-untaken",
            Guard::ActInFlight => "act-in-flight",
            Guard::NotIdle => "not-idle",
            Guard::NotAuthoritative => "not-authoritative",
            Guard::Prompt => "prompt",
            Guard::WakeSettle => "wake-settle",
            Guard::Credits => "credits",
            Guard::Person => "person",
            Guard::Halted => "halted",
            Guard::Exchange => "exchange",
            Guard::Autopilot => "autopilot",
            Guard::Memory => "memory",
            Guard::Reach => "reach",
            Guard::Draft => "draft",
            Guard::Taskless => "taskless",
            Guard::Managed => "managed",
            Guard::HostWithheld => "host-withheld",
            Guard::UpgradeDue => "upgrade-due",
            Guard::HostLine => "host-line",
        }
    }

    /// Returns the non-zero numeric code (`1..=25`) for this guard.
    #[must_use]
    pub fn code(self) -> u8 {
        Guard::ALL
            .iter()
            .position(|g| *g == self)
            .and_then(|i| u8::try_from(i + 1).ok())
            .unwrap_or(0)
    }

    /// Decodes a [`Guard`] from its [`Self::code`] value.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Guard> {
        Guard::ALL.get(usize::from(code).checked_sub(1)?).copied()
    }

    /// The [`Guard`] whose [`Self::word`] is `word`, or `None`.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Guard> {
        Guard::ALL.into_iter().find(|g| g.word() == word)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HoldSaid {
    Point(&'static str),
    NoHostStep,
    Held(Guard),
}

impl HoldSaid {
    fn words(self) -> String {
        match self {
            HoldSaid::Point(why) => format!("point nothing why={why}"),
            HoldSaid::NoHostStep => "background no-host-step".to_string(),
            HoldSaid::Held(guard) => format!("background held why={}", guard.word()),
        }
    }
}

/// Scheduled retry or back-off wait reported via [`IdleHost::waiting`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitPlan {
    /// Unix timestamp (seconds) when the point will be re-evaluated.
    pub at_unix: i64,
    /// Policy reason string for the wait.
    pub why: String,
    /// Wait classification ([`WaitKind::Retry`] or [`WaitKind::BackOff`]).
    pub kind: WaitKind,
}

#[cfg(test)]
thread_local! {
    pub(super) static TELL_EVERY_STEP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ToldWait {
    at: Instant,
    unix: i64,
    kind: WaitKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Due {
    pub(super) at: Instant,
    pub(super) unix_ms: i64,
    pub(super) why: String,
    pub(super) wait: WaitKind,
}

const ACCOUNT_NEWS_EVERY: Duration = Duration::from_secs(60);

const WAIT_SKEW_S: u64 = 2;

/// Account usage notices observed on a screen read ([`IdleHost::usage_seen`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageSeen {
    pub credits: Option<aterm_phase::credits::Credits>,
    pub warning: Option<aterm_phase::credits::Warning>,
}

impl UsageSeen {
    /// Returns `true` if either a credits notice or usage warning is present.
    #[must_use]
    pub const fn says(&self) -> bool {
        self.credits.is_some() || self.warning.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CreditsKept {
    pub(super) reset: Option<CreditsReset>,
    pub(super) since: Instant,
    pub(super) model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CreditsReset {
    pub(super) text: String,
    pub(super) unix: Option<i64>,
}

/// Reason a claimed `/usage` probe did not record a sample ([`IdleHost::account_probed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMiss {
    /// `/usage` was not submitted (skipped by fence, refused, or empty composer guard).
    NotWritten,
    /// `/usage` was submitted but no Settings panel opened within the bound.
    NotOpened,
    /// Usage panel did not settle with complete figures within the bound.
    Unsettled,
    /// Usage panel extends below the grid fold.
    Fold,
    /// User keyed the session before or during the panel read.
    Person,
    /// Opened view is not the fullscreen renderer's Usage tab.
    Renderer,
    /// Usage panel closed before settling.
    Closed,
    /// Transport lost the reply during the probe.
    Lost,
}

impl ProbeMiss {
    /// Returns the short token for this probe miss.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ProbeMiss::NotWritten => "not-written",
            ProbeMiss::NotOpened => "not-opened",
            ProbeMiss::Unsettled => "unsettled",
            ProbeMiss::Fold => "fold",
            ProbeMiss::Person => "person",
            ProbeMiss::Renderer => "renderer",
            ProbeMiss::Closed => "closed",
            ProbeMiss::Lost => "lost",
        }
    }
}

/// Reason a host declined to claim an account `/usage` probe ([`IdleHost::account_probe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeNo {
    /// Host does not track accounts.
    NoAccounts,
    /// `[harness] accounts` mode is not `early`.
    Mode,
    /// No account roster exists or the foreground process is not a readable Claude Code session.
    Unread,
    /// Foreground process is not using a listed login in the default home directory.
    NotJudged,
    /// Account is not due for a fresh probe under the store lock.
    NotDue,
    /// Probe claim is already held for this account or globally.
    Claimed,
    /// Account store lock remained held past the bounded wait.
    LockHeld,
    /// Account state file could not be read or written.
    State,
}

impl ProbeNo {
    /// All probe refusal reasons in [`Self::word`] order.
    pub const ALL: [ProbeNo; 8] = [
        ProbeNo::NoAccounts,
        ProbeNo::Mode,
        ProbeNo::Unread,
        ProbeNo::NotJudged,
        ProbeNo::NotDue,
        ProbeNo::Claimed,
        ProbeNo::LockHeld,
        ProbeNo::State,
    ];

    /// Returns the short token for this probe refusal.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ProbeNo::NoAccounts => "no-accounts",
            ProbeNo::Mode => "not-early",
            ProbeNo::Unread => "unread",
            ProbeNo::NotJudged => "not-a-stored-login",
            ProbeNo::NotDue => "not-due",
            ProbeNo::Claimed => "claimed",
            ProbeNo::LockHeld => "lock-held",
            ProbeNo::State => "state",
        }
    }

    /// Parses a [`ProbeNo`] from its [`Self::word`] token.
    #[must_use]
    pub fn of_word(word: &str) -> Option<ProbeNo> {
        ProbeNo::ALL.into_iter().find(|no| no.word() == word)
    }
}

/// Reason a rating survey card could not be dismissed ([`IdleHost::survey_stuck`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StuckSurvey {
    /// Survey shape is unrecognized or its question row cannot be fenced.
    Withheld,
    /// Survey remained open after exhausting dismissal attempts.
    Unclosed,
}

/// Stall condition reported by the stall canary ([`IdleHost::stall_unread`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StallSeen {
    /// Unclassified screen with Claude Code resting on `✳` for `minutes`.
    Unread { minutes: u64 },
    /// Agent's own background work running with no turn progress for `minutes`.
    OwnWork { minutes: u64 },
    /// Background child process `pid` making no progress for `minutes`.
    NoProgress { minutes: u64, pid: u32 },
}

/// Configuration options for [`Session::run_hosted`], [`Session::watch`], and [`Session::supervise`].
#[derive(Debug, Clone, Default)]
pub struct SuperviseOpts {
    /// Maximum wall-clock budget for the supervisor loop.
    pub max: Duration,
    /// Allowed `python3 <script>` globs treated as read-only.
    pub python_allow: Vec<String>,
    /// Remaining context percentage threshold at or below which `EVENT context` is emitted (`0` disables).
    pub context_warn: u8,
    /// Optional JSONL journal path recording supervisor events and decisions.
    pub journal: Option<PathBuf>,
    /// Optional JSONL approval ledger path for hosted loops.
    pub ledger: Option<PathBuf>,
    /// `[harness]` supervisor policy configuration.
    pub policy: SupervisorConfig,
    /// Whether to exit with [`CLAIM_HELD`] instead of watching read-only when another holder owns the claim.
    pub yield_when_held: bool,
    /// Optional host callback interface invoked at idle points and background breaks.
    pub idle_host: Option<Arc<dyn IdleHost>>,
    /// Optional directory where tasked session results are snapshotted at completion.
    pub tasks: Option<PathBuf>,
}

impl SuperviseOpts {
    /// Returns default options for an unbounded in-GUI hosted supervisor loop.
    pub fn hosted() -> Self {
        Self::hosted_with(&SupervisorConfig::default())
    }

    /// Returns [`Self::hosted`] options configured with `cfg`.
    pub fn hosted_with(cfg: &SupervisorConfig) -> Self {
        Self {
            max: UNBOUNDED,
            context_warn: DEFAULT_CONTEXT_WARN,
            policy: cfg.clone(),
            yield_when_held: true,
            ..Self::default()
        }
    }

    /// Returns the configured `python3` read-allow globs, or the default list when empty.
    pub(crate) fn allow(&self) -> Vec<String> {
        if self.python_allow.is_empty() {
            DEFAULT_PYTHON_ALLOW.iter().map(|s| s.to_string()).collect()
        } else {
            self.python_allow.clone()
        }
    }
}

const STOPPED_WRITE: &str = "the supervisor was stopped: nothing is written after the stop";
const STOPPED_WAIT: &str = "the supervisor was stopped: no wait begins after the stop";

/// Infinite duration budget (`--max-s 0`, [`SuperviseOpts::hosted`]).
pub const UNBOUNDED: Duration = Duration::MAX;
const DEFAULT_CONTEXT_WARN: u8 = 10;
const TAIL_ROWS: usize = 40;

type ScreenKey = (u64, usize, Vec<String>);

#[derive(Clone, Copy, Debug)]
enum CellsAt {
    Grid(usize),
    Abs(u64),
}

struct Shaped {
    screen: Screen,
    read_by: Option<&'static dyn ScreenReader>,
    above: Option<Vec<String>>,
}

fn tail_misses_the_live_rows(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    screen.first > 0
        && (screen.cursor_row < screen.first
            || screen.rows.iter().all(|r| r.trim().is_empty())
            || reader.prompt(&screen.rows).is_some_and(|p| {
                p.head_off_screen || super::policy::box_call::reads_the_screen_above(&p)
            }))
}

fn drawn_above_tail(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    if screen.first == 0 || screen.rows.last().is_none_or(|r| !r.trim().is_empty()) {
        return false;
    }
    let read = reader.read_at(&screen.rows, screen.cursor_index(), None);
    !read.phase_authoritative || read.phase == Phase::Idle
}

fn gap_above_tail(screen: &Screen) -> bool {
    screen.first > 0 && aterm_phase::live_zone_reaches_above(&screen.rows, TAIL_ROWS)
}

const WAIT_STEP: Duration = Duration::from_secs(20);
const HOST_FOLLOW_UPS: u8 = 3;
fn next_wait_step() -> Duration {
    WAIT_STEP.min(Duration::from_secs(crate::harness::upgrade::quiet_s()))
}
const _: () = assert!(
    WAIT_STEP.as_nanos() <= Duration::from_secs(crate::harness::upgrade::QUIET_S).as_nanos()
);
/// Settle duration required before offering a background-work break to the host.
pub(crate) fn background_settle() -> Duration {
    Duration::from_secs(crate::harness::upgrade::quiet_s())
}
const BOX_SETTLE: Duration = Duration::from_millis(300);
const _: () = assert!(
    BOX_SETTLE.as_millis() > 250,
    "the box settle outlasts the limit reset's 250 ms refusal"
);

const SPENT_CALLS: usize = 16;

const AUTOCOMPACT_SETTLE: Duration = Duration::from_millis(1200);
const AGENT_WORDS_BUT_IDLE: &str = "busy,prompt,question,survey,wall,unknown,-";
const AGENT_WORDS_BUT_QUESTION: &str = "busy,prompt,idle,survey,wall,unknown,-";
const AGENT_RECHECK: Duration = Duration::from_secs(5);

const HANDBACK_POLL: Duration = Duration::from_secs(2);
const OFF_STEP: Duration = Duration::from_secs(5);
const ANSWERED_PAST_LIMIT: &str = "the worker answered past the adopted limit's notice";
const IDLE_MS: &str = "2000";
const SETTLE_MS: &str = "500";
const SETTLE_CAP: Duration = Duration::from_millis(1500);
const FRAME_WAIT: Duration = Duration::from_secs(61);
const FRAME_REREADS: u32 = 8;
const WRITING_CAP: Duration = Duration::from_secs(3);
/// How long an idle read its reader does not vouch for must stand, every read reading it,
/// before it is a point: over twice the longest such read measured inside a turn (0.385 s).
const NO_EVIDENCE_HOLD: Duration = Duration::from_secs(1);
/// A hang detector for an idle read held while the program's own title says its run goes
/// on: never a latency budget (the longest end-of-turn review measured took 4.4 s).
const TITLE_HOLD: Duration = Duration::from_secs(60);
const UNSCOPED_CAP: Duration = WRITING_CAP;
const BOXED_BUSY_CAP: Duration = Duration::from_secs(2);
const COMPACTED_RISE: u8 = 30;
const STRAY_SETTLE: Duration = Duration::from_millis(2000);
const BUSY_SINK_RETRIES: u32 = 3;
const BUSY_SINK_BACKOFF: Duration = Duration::from_millis(250);
const MAX_APPROVALS_OF_ONE_COMMAND: usize = 2;
#[cfg(test)]
const RESULT_ROWS: usize = 28;
/// Maximum character length for a command, speech snippet, or notice in a single log line.
pub(crate) const LINE_CHARS: usize = 160;
const KEY_ROWS: usize = 8;
const ABOVE_LETTERS: usize = 64;

/// Exit code returned when a supervisor wait or loop exhausts its time budget.
pub const EXIT_TIMEOUT: u8 = 124;

const RESET_GRACE: Duration = Duration::from_secs(10 * 60);

const SLEPT_THROUGH_S: i64 = 60;

pub(super) const fn slept_through(wall_delta_s: i64, mono_delta_s: i64) -> bool {
    wall_delta_s - mono_delta_s > SLEPT_THROUGH_S
}
const RESET_HORIZON: Duration = Duration::from_secs(8 * 86_400);
const TURN_IDLE: &str = "idle=600";
const TURN_TIMEOUT: &str = "timeout=2500";
/// Keyed attention owner token (`owner=supervisor`) for supervisor escalations.
pub const ATTENTION_OWNER: &str = "supervisor";

/// Keyed attention owner token (`owner=relaunch`) for unrecoverable agent relaunch failures.
pub const RELAUNCH_ATTENTION_OWNER: &str = "relaunch";

/// Keyed attention owner token (`owner=needs`) for worker `NEEDS YOU:` items.
pub const NEEDS_ATTENTION_OWNER: &str = "needs";

const ATTENTION_BYTES: usize = aterm_types::control_verbs::META_ATTENTION_KEYED_MAX;
/// Prefix for usage-limit episode attention badges (`limited:`).
pub const ATTENTION_PREFIX: &str = "limited:";
const APPROVAL_LINE_CELLS: usize = 64;

const QUESTION_FIRST_KEY_IDLE: &str = "1000";
const QUESTION_FIRST_KEY_CAP: Duration = Duration::from_millis(3000);

const MAX_CHANGED_PRESSES: u32 = 5;
const REFUSAL_PAUSE: Duration = Duration::from_millis(250);
const REFUSAL_PAUSE_MAX: Duration = Duration::from_secs(8);
const REFUSAL_QUIET: Duration = Duration::from_secs(10 * 60);

/// Default duration to ride out a server outage before exiting (`180` s).
pub const DEFAULT_RECONNECT: Duration = Duration::from_secs(180);
pub(super) const RECONNECT_PAUSE: Duration = Duration::from_millis(500);
pub(super) const RECONNECT_PAUSE_MAX: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fail {
    Lost(String),
    Hard(String),
}

impl From<String> for Fail {
    fn from(e: String) -> Self {
        Fail::Hard(e)
    }
}

impl From<Fail> for String {
    fn from(f: Fail) -> Self {
        match f {
            Fail::Lost(e) | Fail::Hard(e) => e,
        }
    }
}

enum Wait {
    Latched,
    TimedOut,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Next {
    Gone(BusyWait),
    Settle,
    Idle,
}

fn box_anchor(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    let (header, esc) = reader.prompt(rows)?.span;
    let row = rows
        .get(header + 1..esc)?
        .iter()
        .find(|r| !r.trim().is_empty())?;
    Some(super::policy::row_guard(row))
}

type BusyMatch = Box<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BusyWait {
    guard: String,
    span: Option<(usize, usize)>,
    unscoped: bool,
    boxed: bool,
}

impl BusyWait {
    fn step(&self, step: Duration) -> Duration {
        let step = if self.boxed {
            step.min(BOXED_BUSY_CAP)
        } else {
            step
        };
        if self.unscoped {
            step.min(UNSCOPED_CAP)
        } else {
            step
        }
    }
}

fn live_rows(reader: &dyn ScreenReader, screen: &Screen) -> (usize, Option<(usize, usize)>) {
    let n = screen.rows.len();
    let at = reader.busy_zone_start(&screen.rows).min(n);
    let span = (at < n && screen.first + at > 0).then(|| (screen.first + at, screen.first + n - 1));
    (at, span)
}

/// Returns `true` if usage-limit reset times for `p` are anchored to when the notice was printed.
pub(crate) const fn places_resets_on_print(p: Program) -> bool {
    match p {
        Program::Claude | Program::Generic => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini => false,
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "one Past lives on the stack per wait and is matched at once; boxing the \
              screen would put a heap allocation on every wait's read to save nothing"
)]
enum Past {
    Moved,
    Still(Option<Screen>),
}

enum Rode {
    Back { fresh: bool },
    Spent,
    Stopped,
}

#[derive(Debug)]
struct Outage {
    since: Instant,
    kind: String,
    told: bool,
    back: bool,
    pause: Duration,
}

enum End {
    Stopped(
        #[cfg_attr(
            not(test),
            expect(dead_code, reason = "rendered by the tests' supervise")
        )]
        Turn,
    ),
    Timeout(
        #[cfg_attr(
            not(test),
            expect(dead_code, reason = "rendered by the tests' supervise")
        )]
        Turn,
    ),
    Quit,
}

fn session_gone(end: &Result<End, String>) -> bool {
    matches!(end, Err(e) if e.contains("ERR exited") || e.contains("no such session"))
}

fn watch_end_line(end: Result<End, String>) -> (String, u8) {
    match end {
        Ok(End::Timeout(_)) => ("TIMEOUT".to_string(), EXIT_TIMEOUT),
        Ok(End::Stopped(..)) => ("EXIT stopped at a review point".to_string(), 1),
        Ok(End::Quit) => ("EXIT stopped".to_string(), 0),
        Err(e) => (format!("EXIT {}", exit_reason(&e)), 1),
    }
}

fn seed_usage_act_in(
    state: &mut TurnEndState,
    body: &str,
    sid: Option<&str>,
    older: &OlderReads,
    now_ms: i64,
    now: Instant,
) {
    let ago = |ms: i64| Duration::from_millis(u64::try_from(now_ms - ms).unwrap_or(0));
    if let Some(t) = approvals::awaited_try_in(body, sid)
        && now_ms >= t.at_ms
    {
        let read = match t.read_ms {
            Some(ms) if !t.ended => FirstRead::Read(ago(ms)),
            _ if t.unread => FirstRead::Unread,
            _ => FirstRead::Unknown,
        };
        let awaited = state.seed_awaited(RULE_LIMIT_RESUME, ago(t.at_ms), read, true, t.met, now);
        if !awaited && t.ended && !t.met && older.judged_ms.is_none_or(|j| j < t.at_ms) {
            state.seed_try_taken();
        }
    }
    let taken = |at: i64, end: i64| {
        older
            .takes
            .iter()
            .any(|&(act, judged)| act.abs_diff(at) <= 5_000 && end <= judged)
    };
    let Some(at) = approvals::usage_attempt_taken_in(body, sid, &taken) else {
        return;
    };
    let take_ms = i64::try_from(state.timing.take_within.as_millis()).unwrap_or(0);
    let appearance = match at.met_ms {
        Some(m) if at.read_ms == Some(m) => m.saturating_add(take_ms),
        Some(m) => m,
        None => at.at_ms,
    };
    if at.met_ms.is_some() {
        state.seed_met_attempt(ago(appearance), at.limit, now);
    } else {
        state.seed_usage_attempt(ago(appearance), at.limit, now);
    }
    state.seed_limit_conversation(at.conversation, at.program);
    state.seed_account_resumed(at.at_ms.div_euclid(1000));
}

#[cfg(test)]
fn seed_usage_act(
    state: &mut TurnEndState,
    path: &Path,
    sid: Option<&str>,
    now_ms: i64,
    now: Instant,
) {
    let body = std::fs::read_to_string(path).unwrap_or_default();
    seed_usage_act_in(state, &body, sid, &OlderReads::default(), now_ms, now);
}

fn seed_done_check(
    state: &mut TurnEndState,
    body: &str,
    sid: Option<&str>,
    older: &OlderReads,
    now_ms: i64,
    now: Instant,
) {
    let Some(mut owed) = approvals::done_check_owed_in(body, sid) else {
        // Its answer judged at the point recorded last: a wall there, read again, cut it.
        if let Some(rule) = approvals::check_point_last_in(body, sid) {
            state.seed_check_point(rule);
        }
        return;
    };
    if owed.compact {
        state.seed_compact_check(owed.rule);
        return;
    }
    if let Some((at_ms, _)) = owed.check.filter(|_| !owed.cut) {
        let after: Vec<bool> = older
            .points
            .iter()
            .filter(|(t, _)| *t > at_ms)
            .map(|(_, done)| *done)
            .collect();
        if !after.is_empty() {
            owed.answered |= after.iter().any(|done| *done);
            owed.check = None;
            owed.read_ms = None;
            if after.len() > 1 || !owed.answered {
                return;
            }
        }
    }
    let ago = |ms: i64| Duration::from_millis(u64::try_from(now_ms - ms).unwrap_or(0));
    let awaited = owed.check.is_some_and(|(at_ms, in_place)| {
        now_ms >= at_ms
            && state.seed_awaited(
                owed.rule,
                ago(at_ms),
                owed.read_ms.map_or(
                    if owed.unread {
                        FirstRead::Unread
                    } else {
                        FirstRead::Unknown
                    },
                    |ms| FirstRead::Read(ago(ms)),
                ),
                in_place,
                owed.cut,
                now,
            )
    });
    state.seed_done_check(owed.rule, owed.cut && !awaited, owed.answered);
}

fn ended_before(
    journal: &str,
    sid: Option<&str>,
    last_ms: Option<i64>,
) -> Option<(Option<u64>, i64, bool)> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let land = format!("rule={}", super::policy::turn_end::RULE_LAND);
    let mut later: Vec<super::journal::JournalRecord> = Vec::new();
    let mut note: Option<Option<u64>> = None;
    let mut noted_ms = 0;
    let mut by_wake = false;
    let records = journal.lines().rev().filter_map(|text| {
        let end = text.contains("\"line\":\"DONE ") || text.contains("\"line\":\"LEFT ");
        if !end && !text.contains("\"kind\":\"event\"") && !text.contains("\"kind\":\"approved\"") {
            return None;
        }
        super::journal::JournalRecord::from_json(text)
            .ok()
            .filter(|rec| rec.sid.as_deref() == sid)
    });
    for rec in records {
        let mut words = rec.line.split(' ');
        let word = words.next().unwrap_or_default();
        let ends = word == "DONE" || word == "LEFT";
        let Some(seq) = note else {
            if !ends {
                if said_a_point(&rec) {
                    later.push(rec);
                }
                continue;
            }
            let seq = words
                .next()
                .and_then(|w| w.strip_prefix("seq="))
                .and_then(|v| v.parse::<u64>().ok());
            let rule = words.next().unwrap_or_default();
            if word != "DONE" || rule != land || last_ms.is_some_and(|t| t > rec.t) {
                return None;
            }
            noted_ms = rec.ended.map_or(rec.t, |first| first.min(rec.t));
            by_wake = rec.line.contains(super::policy::turn_end::WAKE_KEPT);
            note = Some(seq);
            continue;
        };
        if !ends && !said_a_point(&rec) {
            continue;
        }
        if ends || is_box(&rec) {
            break;
        }
        let landing = rec.mark;
        let own = |p: &super::journal::JournalRecord| match landing {
            Some(_) => !is_box(p) && p.mark == landing,
            None => !is_box(p) && p.seq.is_some() && p.seq == seq,
        };
        return later
            .iter()
            .all(own)
            .then_some((landing, noted_ms, by_wake));
    }
    let seq = note?;
    later
        .iter()
        .all(|p| !is_box(p) && p.seq.is_some() && p.seq == seq)
        .then_some((None, noted_ms, by_wake))
}

#[cfg(test)]
fn landed_before(journal: &str, sid: Option<&str>, last_ms: Option<i64>) -> Option<Option<u64>> {
    ended_before(journal, sid, last_ms).map(|(words, _, _)| words)
}

fn is_box(rec: &super::journal::JournalRecord) -> bool {
    rec.kind == "approved" || rec.phase == "prompt"
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hold {
    why: String,
    until_ms: i64,
    acted: bool,
    said: Option<String>,
    at_ms: i64,
    short: Option<u32>,
}

fn journaled_hold(journal: &str, sid: Option<&str>, last_ms: Option<i64>) -> Option<Hold> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let mut hold: Option<(i64, Hold)> = None;
    let mut act: Option<i64> = None;
    let mut said: Option<String> = None;
    let mut streak: Option<u32> = None;
    let mut continued = false;
    let mut acts = 0_u32;
    let mut point_read = false;
    let reset =
        |hold: &mut Option<(i64, Hold)>, act: &mut Option<i64>, streak: &mut Option<u32>| {
            *hold = None;
            *act = None;
            *streak = None;
        };
    for rec in journal
        .lines()
        .filter_map(|text| super::journal::JournalRecord::from_json(text).ok())
        .filter(|rec| rec.sid.as_deref() == sid)
    {
        match rec.kind.as_str() {
            "waiting" if rec.summary.contains(" the worker to take the ") => point_read = false,
            "waiting" => {
                point_read = false;
                act = None;
                let why = rec.summary.split_once(' ').map_or("", |(_, why)| why);
                streak = match short_held(why) {
                    Some((k, _)) => Some(k),
                    None => streak.map(|k| k.saturating_add(u32::from(continued))),
                };
                continued = false;
                acts = 0;
                hold = rec
                    .summary
                    .strip_prefix("until=")
                    .and_then(|s| s.split_once(' '))
                    .filter(|_| rec.phase == "-")
                    .and_then(|(until, why)| {
                        let until = crate::harness::upgrade_models::parse_utc(until)?;
                        let until_ms = i64::try_from(until).ok()?.checked_mul(1000)?;
                        Some((
                            rec.t,
                            Hold {
                                why: why.to_string(),
                                until_ms,
                                acted: false,
                                said: said.clone(),
                                at_ms: rec.t,
                                short: streak,
                            },
                        ))
                    });
            }
            "continued" | "typed" => {
                // An act typed at a point read since the last wait: no
                // wait came before it there.
                if std::mem::take(&mut point_read) {
                    reset(&mut hold, &mut act, &mut streak);
                }
                let continues = is_continue_rule(&rec.phase);
                if continues && (acts > 0 || hold.is_none()) {
                    streak = Some(0);
                    continued = false;
                    hold = Some((
                        rec.t,
                        Hold {
                            why: String::new(),
                            until_ms: rec.t,
                            acted: false,
                            said: said.clone(),
                            at_ms: rec.t,
                            short: streak,
                        },
                    ));
                }
                act = hold.is_some().then_some(rec.t);
                if let Some((_, h)) = hold.as_mut() {
                    h.said = Some(rec.summary.clone());
                }
                said = Some(rec.summary);
                continued |= continues;
                acts += 1;
            }
            _ if rec.line.starts_with("DONE ") => {
                point_read = false;
                act = None;
                streak = Some(streak.unwrap_or(0).saturating_add(u32::from(continued)));
                continued = false;
                acts = 0;
                hold = Some((
                    rec.t,
                    Hold {
                        why: String::new(),
                        until_ms: rec.t,
                        acted: false,
                        said: said.clone(),
                        at_ms: rec.t,
                        short: streak,
                    },
                ));
            }
            "restarted" | "escalated" | "skipped" => {
                point_read = false;
                reset(&mut hold, &mut act, &mut streak);
            }
            _ if is_box(&rec) || rec.line.starts_with("LEFT ") => {
                point_read = false;
                reset(&mut hold, &mut act, &mut streak);
            }
            _ if said_a_point(&rec) => {
                if point_read {
                    reset(&mut hold, &mut act, &mut streak);
                }
                point_read = true;
            }
            _ => {}
        }
    }
    let (t, hold) = hold?;
    match act {
        Some(at) => last_ms
            .is_some_and(|a| a.abs_diff(at) <= 5_000)
            .then_some(Hold {
                acted: true,
                ..hold
            }),
        None => last_ms.is_none_or(|a| a < t).then_some(hold),
    }
}

fn journaled_left(journal: &str, sid: Option<&str>) -> Option<String> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let mut left = None;
    for rec in journal
        .lines()
        .filter_map(|text| super::journal::JournalRecord::from_json(text).ok())
        .filter(|rec| rec.sid.as_deref() == sid)
    {
        match rec.kind.as_str() {
            "continued" | "typed" => left = None,
            _ if rec.line.starts_with("LEFT ") => left = Some(rec.t),
            _ if rec.line.starts_with("DONE ") => left = None,
            _ => {}
        }
    }
    left.map(approvals::left_before_key)
}

fn with_parks_journaled<'b>(
    body: &'b str,
    journal: Option<&str>,
    sid: Option<&str>,
) -> std::borrow::Cow<'b, str> {
    let bare = sid.map(|s| s.trim_start_matches('@'));
    let parks: Vec<(i64, u64)> = journal
        .unwrap_or_default()
        .lines()
        .filter(|text| text.contains("PARKED seq="))
        .filter_map(|text| super::journal::JournalRecord::from_json(text).ok())
        .filter(|rec| rec.sid.as_deref() == bare && lifecycle::is_parked_off(&rec.line))
        .map(|rec| (rec.t, rec.seq.unwrap_or(0)))
        .collect();
    if parks.is_empty() {
        return std::borrow::Cow::Borrowed(body);
    }
    let record = |&(t, seq): &(i64, u64)| {
        approvals::Row {
            rule_id: approvals::RULE_OFF_STOOD,
            outcome: approvals::Outcome::Waiting,
            command: "",
            reason: approvals::OFF_STOOD,
            box_seq: seq,
        }
        .to_json(t, sid)
    };
    let mut parks = parks.iter().peekable();
    let mut out = String::with_capacity(body.len() + 256);
    for line in body.lines() {
        let ts = line.strip_prefix("{\"ts\":").and_then(|rest| {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            rest[..digits].parse::<i64>().ok()
        });
        while let Some(park) = parks.next_if(|(t, _)| ts.is_some_and(|ts| ts >= *t)) {
            out.push_str(&record(park));
            out.push('\n');
        }
        out.push_str(line);
        out.push('\n');
    }
    for park in parks {
        out.push_str(&record(park));
        out.push('\n');
    }
    std::borrow::Cow::Owned(out)
}

#[derive(Debug, Default)]
struct OlderReads {
    takes: Vec<(i64, i64)>,
    points: Vec<(i64, bool)>,
    judged_ms: Option<i64>,
}

fn journaled_takes(journal: &str, sid: Option<&str>) -> Vec<(i64, i64)> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let mut takes = Vec::new();
    let mut act: Option<(i64, bool)> = None;
    for rec in journal
        .lines()
        .filter_map(|text| super::journal::JournalRecord::from_json(text).ok())
        .filter(|rec| rec.sid.as_deref() == sid)
    {
        match rec.kind.as_str() {
            "continued" | "typed" => {
                if let Some((t, true)) = act {
                    takes.push((t, i64::MAX));
                }
                act = Some((rec.t, false));
            }
            "waiting" if rec.summary.contains(" the worker to take the ") => {
                if let Some((_, window)) = act.as_mut() {
                    *window = true;
                }
            }
            "waiting" => {
                if let Some((t, true)) = act.take() {
                    takes.push((t, rec.t));
                }
            }
            _ => {}
        }
    }
    if let Some((t, true)) = act {
        takes.push((t, i64::MAX));
    }
    takes
}

fn journaled_points(journal: &str, sid: Option<&str>) -> Vec<(i64, bool)> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    journal
        .lines()
        .filter_map(|text| super::journal::JournalRecord::from_json(text).ok())
        .filter(|rec| rec.sid.as_deref() == sid)
        .filter_map(|rec| match rec.kind.as_str() {
            "waiting" if !rec.summary.contains(" the worker to take the ") => Some((rec.t, false)),
            _ if rec.line.starts_with("DONE ") => Some((rec.t, true)),
            _ => None,
        })
        .collect()
}

fn short_held(why: &str) -> Option<(u32, i64)> {
    let (k, rest) = why.split_once(" short turn(s) in a row: ")?;
    let (w, unit) = rest.strip_suffix(" back-off")?.split_once(' ')?;
    let w: i64 = w.parse().ok()?;
    let ms = match unit {
        "s" => w.checked_mul(1000)?,
        "min" => w.checked_mul(60_000)?,
        _ => return None,
    };
    Some((k.parse().ok()?, ms))
}

fn retry_held(hold: &Hold, timing: &TurnEndTiming) -> Option<approvals::RetryMark> {
    use aterm_phase::{ApiCause, WallKind};
    let (wall, n) = hold.why.rsplit_once(" retry ")?;
    let n: usize = n.parse().ok().filter(|n| *n > 0)?;
    let api = |cause: ApiCause| WallKind::ApiError {
        code: None,
        retryable: cause != ApiCause::Request,
        cause,
    };
    let (kind, ladder, at_once) = match wall {
        "overloaded" => (WallKind::Overloaded, &timing.retry_backoff, false),
        "api-error"
            if hold.said.as_deref().is_some_and(|said| {
                said.contains("reported an API error")
                    && !said.contains("(overloaded)")
                    && !said.contains("; trying again.")
            }) =>
        {
            (api(ApiCause::Server), &timing.retry_backoff, false)
        }
        "api-error" => (api(ApiCause::Unreachable), &timing.net_backoff, false),
        "api-error refused:" => (api(ApiCause::Request), &timing.refused_backoff, false),
        "api-error cut off:" => (api(ApiCause::CutOff), &timing.net_backoff, true),
        "api-error overlong:" => (api(ApiCause::Overlong), &timing.retry_backoff, true),
        _ => return None,
    };
    let rung = if at_once {
        n.checked_sub(2)
    } else {
        n.checked_sub(1)
    };
    let wait = rung.map_or(Some(Duration::ZERO), |r| {
        crate::supervise::ladder::Ladder(ladder).at(r)
    })?;
    let wait_ms = i64::try_from(wait.as_millis()).ok()?;
    Some(approvals::RetryMark {
        kind,
        attempts: if hold.acted { n } else { n - 1 },
        ups: 0,
        since_ms: hold.until_ms + 500 - wait_ms,
    })
}

fn said_a_point(rec: &super::journal::JournalRecord) -> bool {
    match rec.kind.as_str() {
        "event" => matches!(
            rec.phase.as_str(),
            "idle" | "idle-no-report" | "turn" | "question" | "limited" | "prompt"
        ),
        "approved" => true,
        _ => false,
    }
}

fn deadline_after(now: Instant, max: Duration) -> Instant {
    now.checked_add(max)
        .or_else(|| now.checked_add(Duration::from_secs(100 * 365 * 86_400)))
        .unwrap_or(now)
}

struct Sink<'w> {
    out: &'w mut (dyn Write + Send + 'w),
    journal: Journal,
    warn: Option<&'w mut (dyn Write + Send + 'w)>,
    fx: Fx,
}

impl Sink<'_> {
    fn record(&mut self, line: &str) {
        self.record_marked(line, None);
    }

    fn record_marked(&mut self, line: &str, mark: Option<u64>) {
        let t_ms = self.fx.stamp_ms();
        let row = match self.warn.as_deref_mut() {
            Some(w) => self.journal.record_marked(t_ms, line, mark, w),
            None => self.journal.record_marked(t_ms, line, mark, self.out),
        };
        if let Some(row) = row {
            self.fx
                .emit(super::tape::doors::JOURNAL, || row.into_bytes());
        }
    }

    fn record_ended(&mut self, line: &str, ended: Option<i64>) {
        let t_ms = self.fx.stamp_ms();
        let row = match self.warn.as_deref_mut() {
            Some(w) => self.journal.record_ended(t_ms, line, ended, w),
            None => self.journal.record_ended(t_ms, line, ended, self.out),
        };
        if let Some(row) = row {
            self.fx
                .emit(super::tape::doors::JOURNAL, || row.into_bytes());
        }
    }

    fn line(&mut self, line: &str) -> Result<(), String> {
        self.line_marked(line, None)
    }

    fn line_marked(&mut self, line: &str, mark: Option<u64>) -> Result<(), String> {
        self.record_marked(line, mark);
        let out = &mut *self.out;
        self.fx.exchange(
            super::tape::doors::OUT,
            || line.as_bytes().to_vec(),
            || emit(out, line),
        )
    }
}

struct Looking {
    approved: Vec<approval_loop::Approved>,
    gone_first: bool,
    handed: Option<String>,
    handed_seq: Option<u64>,
    handed_submit: Option<(u64, u64)>,
    moved: Saw,
    survey: Survey,
    read_back: Option<Screen>,
    face_owed: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Saw {
    busy: bool,
    live: bool,
}

struct BoxAsk {
    key: String,
    memory: bool,
}

struct SurveyDuty<'a> {
    opts: &'a SuperviseOpts,
    survey: &'a mut Survey,
}

struct SurveyCtx<'a> {
    block: Option<&'a aterm_phase::SurveyBlock>,
    busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Survey {
    Closed,
    Said,
    Tried { pressed: Option<u64>, tries: u32 },
    Retry { tries: u32, at: Instant },
}

const SURVEY_RETRY: [Duration; SURVEY_TRIES] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];
const SURVEY_BADGE_AT: u32 = 2;
const SURVEY_TRIES: usize = 4;
const SURVEY_SETTLE: Duration = Duration::from_millis(800);
const SURVEY_LAND: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageProbe {
    Idle,
    Owed {
        own: bool,
        since: Instant,
    },
    Closing {
        own: bool,
        since: Instant,
        tries: u32,
        at: Instant,
        told: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probed {
    No,
    Again,
    Done,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProbeWaits {
    open: Duration,
    settle: Duration,
    step: Duration,
    close: Duration,
}

const PROBE_WAITS: ProbeWaits = ProbeWaits {
    open: Duration::from_secs(5),
    settle: Duration::from_secs(10),
    step: Duration::from_millis(250),
    close: Duration::from_secs(2),
};

const PROBE_STUCK_AT: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResetStep {
    Idle,
    Owed(Box<ResetOwed>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResetOwed {
    go: Option<ResetGo>,
    since: Instant,
    keyed: Instant,
    entered: bool,
    enters: u32,
    kept: Option<KeepWhy>,
    tries: u32,
    at: Instant,
    told: bool,
    seen: Option<Instant>,
    offer: Option<ResetOffer>,
    hold: Option<Instant>,
    shown: Option<(ResetShown, Instant)>,
    yes_said: bool,
    skips: u32,
    agains: u8,
    stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetShown {
    Checking,
    Resetting,
    Wall,
    Confirm,
    Other,
    Unread,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    Armed,
    Warned { last: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Episode {
    seq: u64,
    resumes_itself: bool,
    reset_unix: Option<i64>,
    reset_text: String,
    extended: bool,
    raised: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Retry {
    seq: u64,
}

enum Step {
    Again { settle: bool },
    Review(Turn),
    Decide(Screen),
}

trait Review {
    fn approved(&mut self, seq: u64, command: &str) -> Result<(), String>;
    fn review(&mut self, turn: &Turn) -> Result<bool, String>;
    fn review_marked(&mut self, turn: &Turn, mark: Option<u64>) -> Result<bool, String> {
        let _ = mark;
        self.review(turn)
    }
    fn say(&mut self, line: &str) -> Result<(), String>;
    fn note(&mut self, line: &str);
    fn note_ended(&mut self, line: &str, ended: Option<i64>) {
        let _ = ended;
        self.note(line);
    }
    fn unattended(&self) -> bool;
}

#[cfg(test)]
struct Alone;

#[cfg(test)]
impl Review for Alone {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
        Ok(false)
    }
    fn say(&mut self, _line: &str) -> Result<(), String> {
        Ok(())
    }
    fn note(&mut self, _line: &str) {}
    fn unattended(&self) -> bool {
        false
    }
}

#[cfg(test)]
struct StopAtReview<'l, 'w> {
    sink: &'l Mutex<Sink<'w>>,
    allow: &'l [String],
}

#[cfg(test)]
impl Review for StopAtReview<'_, '_> {
    fn approved(&mut self, seq: u64, command: &str) -> Result<(), String> {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(&approved_line(seq, command));
        Ok(())
    }
    fn review(&mut self, turn: &Turn) -> Result<bool, String> {
        self.review_marked(turn, None)
    }
    fn review_marked(&mut self, turn: &Turn, mark: Option<u64>) -> Result<bool, String> {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record_marked(&event_line(turn, self.allow), mark);
        Ok(false)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        sink.record(line);
        log_line(sink.out, line);
        Ok(())
    }
    fn note(&mut self, line: &str) {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(line);
    }
    fn note_ended(&mut self, line: &str, ended: Option<i64>) {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record_ended(line, ended);
    }
    fn unattended(&self) -> bool {
        false
    }
}

struct Lines<'w, 's> {
    sink: &'w Mutex<Sink<'s>>,
    allow: &'w [String],
}

impl Review for Lines<'_, '_> {
    fn approved(&mut self, seq: u64, command: &str) -> Result<(), String> {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line(&approved_line(seq, command))
    }
    fn review(&mut self, turn: &Turn) -> Result<bool, String> {
        self.review_marked(turn, None)
    }
    fn review_marked(&mut self, turn: &Turn, mark: Option<u64>) -> Result<bool, String> {
        let line = event_line(turn, self.allow);
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line_marked(&line, mark)?;
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line(line)
    }
    fn note(&mut self, line: &str) {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(line);
    }
    fn note_ended(&mut self, line: &str, ended: Option<i64>) {
        self.sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record_ended(line, ended);
    }
    fn unattended(&self) -> bool {
        true
    }
}

fn approved_line(seq: u64, command: &str) -> String {
    format!("APPROVED seq={seq} {}", clip(command))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    Pressed { seq: u64 },
    Skipped { seq: u64 },
    Changed { seq: u64 },
    Unseen { seq: u64 },
    Yielded { seq: u64 },
    Typed { seq: u64 },
}

#[derive(Debug)]
enum Pressing {
    Done(Press),
    Lost { why: String },
    Halted { why: String },
    Refused { why: String },
    Unconfirmed { why: String, retry: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Dismiss {
    Pressed { seq: u64 },
    Skipped,
    Yielded,
    Refused { halted: bool, why: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Backspaced {
    Done,
    Refused { halted: bool, why: String },
}

/// Supervisor session state machine driving a target session over [`Ctl`].
pub struct Session<'a, C: Ctl> {
    ctl: &'a mut C,
    setup: SessionSetup,
    caps: Caps,
    outage: Option<Outage>,
    probing: bool,
    last: Option<Screen>,
    busy_match: Option<(String, BusyMatch)>,
    survey_gone: bool,
    context_warn: u8,
    context: Context,
    limit: Option<Episode>,
    retry: Option<Retry>,
    box_ask: Option<BoxAsk>,
    fx: Fx,
    local_offset: Option<i64>,
    local_offset_fixed: Option<i64>,
    zone_offset: fn(&str, i64) -> Option<i64>,
    turn_end: TurnEndState,
    turn_end_due: Option<Due>,
    clock_marks: Option<(Instant, i64)>,
    sleeps: SleepBook,
    sleep_count: Option<SleepStamp>,
    sleep_look: Option<FirstLook>,
    sleep_unknown: Option<&'static str>,
    sleep_unsaid: Option<&'static str>,
    slept_at: Option<i64>,
    wake_settle: Option<(Due, bool)>,
    restart_waits: u32,
    restart_run: Option<(&'static str, i64)>,
    look_at: Option<Instant>,
    survey_first_seen: Option<Instant>,
    survey_zero: Option<Instant>,
    survey_told: Option<StuckSurvey>,
    survey_absent_told: bool,
    survey_hold: Option<Instant>,
    survey_kind: Option<aterm_phase::SurveyKind>,
    running: super::policy::turn_end::RunningTurn,
    wall_reset: Option<(String, Option<i64>)>,
    /// What the usage tap read of the walled process, and the reset the wall is waited to by it and the notice together ([`turn_end_loop::WallTap`]). Dropped with the wall.
    wall_tap: Option<turn_end_loop::WallTap>,
    stuck_asked: Option<Instant>,
    stuck_rung: wall_memory::StuckRung,
    stuck_raised: Option<(Instant, super::policy::outlook::ExceptionKind)>,
    stuck_told: Option<(Instant, Option<u32>)>,
    stuck_dialog: Option<(String, Instant)>,
    stuck_still: Option<(String, Instant)>,
    reprint_jumps: u32,
    wall_answered: Option<(Instant, String)>,
    wall_memory: Option<wall_memory::WallMemory>,
    wall_verdict: Option<wall_confirm::Verdict>,
    wall_conversation: Option<(u64, super::policy::turn_end::Conversation)>,
    wall_refused_said: Option<String>,
    left_read: Option<wall_confirm::LeftRead>,
    limit_notice_row: Option<String>,
    transcript_wait_seq: Option<u64>,
    transcript_over_seq: Option<u64>,
    standing_read: wall_confirm::StandingRead,
    limit_cut: Option<(ScreenKey, Option<usize>)>,
    message_fold: Option<(ScreenKey, Vec<usize>)>,
    cwd: Option<PathBuf>,
    trusted_cwd: Option<PathBuf>,
    worker_agent: Option<aterm_phase::Program>,
    footer: Option<crate::harness::lights::Mode>,
    busy_read: bool,
    ledger: Ledger,
    turn_end_seeded: bool,
    first_books: bool,
    books_last_ms: Option<i64>,
    pub(super) left_keys: Vec<String>,
    pub(super) repo_seen: Option<(
        Option<u64>,
        Instant,
        Option<crate::harness::repo_work::RepoWork>,
    )>,
    pub(super) left_at_wall_seq: Option<u64>,
    box_reason: Option<(String, String)>,
    changed_streak: u32,
    press_retry: Option<approval_loop::PressRetry>,
    limit_retry: Option<approval_loop::LimitRetry>,
    /// Gemini CLI's usage-limit dialog as the last read that drew it showed
    /// it, kept till a point that is neither the dialog nor the error its
    /// `Stop` leaves, or a read of another box
    /// ([`approval_loop::LimitDialogRead`]).
    limit_dialog_read: Option<approval_loop::LimitDialogRead>,
    /// The key of the point the limit episode's badge was carried to that is
    /// not its dialog nor its `Stop`'s error ([`Self::limit_badge_carry`]):
    /// it goes once a point's fold ends the episode.
    limit_badge_away: Option<String>,
    driver: bool,
    question: approval_loop::QuestionMemory,
    decline: approval_loop::DeclineMemory,
    question_held: bool,
    question_handed_back: bool,
    tell_own: bool,
    read_back: Option<(Screen, Duration)>,
    refusal_pause: Duration,
    refusal_said: Option<(String, Instant)>,
    stop: Option<Arc<AtomicBool>>,
    adopted_limit: bool,
    adopted_box: bool,
    program: Option<String>,
    status_sid: Option<String>,
    person: Option<Instant>,
    launch_status: approval_loop::LaunchStatus,
    /// The content seq and loop time of the screen a `status` last said had unread input
    /// ([`Self::input_unread`]).
    input_unread_at: Option<(u64, Instant)>,
    /// When this process last took or renewed the host's hard hand for a
    /// typed act ([`Self::under_hand`]), while it holds it.
    hand_taken: Option<Instant>,
    launch_used: Option<(u64, String)>,
    draft_seen: Option<(String, Instant)>,
    held: bool,
    rejudge: bool,
    host_held: bool,
    host_passed: Option<String>,
    host_follows: Option<(String, u8, bool)>,
    box_first_seen: Option<(String, Instant)>,
    countdown: Option<approval_loop::Countdown>,
    episode: Option<approval_loop::Episode>,
    /// The last read's person count (`Some(None)`: it carried none), the next episode's baseline.
    last_read_count: Option<Option<u64>>,
    #[cfg(test)]
    deny_cap_off: bool,
    #[cfg(test)]
    touch_off: bool,
    #[cfg(test)]
    episode_per_box: bool,
    #[cfg(test)]
    last_box: Option<String>,
    #[cfg(test)]
    read_clock: Option<(Instant, u32)>,
    stepper: approval_loop::StepperTrack,
    box_proved: Option<String>,
    calls_spent: Vec<String>,
    agent_woke: Option<String>,
    draft_held: Option<super::policy::turn_end::Composer>,
    claim: claim::Claim,
    claim_said: Vec<String>,
    claimed_at_naming: bool,
    attention_ours: Option<String>,
    turn_end_badge: bool,
    badge_parked: Option<String>,
    exception_now: Option<Exception>,
    exception_open: Option<(Exception, Instant)>,
    task_done_said: bool,
    landed_noted_ms: Option<i64>,
    task_note: Option<(String, Option<i64>)>,
    task_point: Option<u64>,
    point_said: Option<(aterm_phase::Program, String)>,
    land_report: Option<(aterm_phase::Program, String)>,
    task_seeded: bool,
    needs_told: Option<u64>,
    needs_told_text: String,
    needs_heard: Option<(u64, String)>,
    reach_seen: Option<Reach>,
    account_seen: Option<(u64, AccountPoint, Option<AccountAdvice>)>,
    roster_seen: Option<(u64, Option<super::policy::turn_end::RosterWord>)>,
    nothing_decided: bool,
    decided_withheld: bool,
    login_notice_told: Option<u32>,
    account_stamp: Option<u64>,
    account_news_at: Option<Instant>,
    login_seen: Option<u64>,
    probe: UsageProbe,
    probe_fold: Option<(Option<u16>, Option<usize>)>,
    probe_off: bool,
    turn_live: bool,
    probe_said: Option<String>,
    probe_hold: Option<Instant>,
    reset: ResetStep,
    reset_taken: Option<(ResetGo, Instant)>,
    reset_kinds: Vec<aterm_phase::ResetKind>,
    reset_passed: Option<String>,
    reset_again: u8,
    reset_fallback: Option<Decision>,
    decided_on: Option<super::policy::box_facts::BoxFacts>,
    focus_key_tried: bool,
    halted: bool,
    autopilot: approval_loop::AutopilotWord,
    local_offset_at: fn(i64) -> Option<i64>,
    memory_seen: Option<super::memory::MemoryOver>,
    memory_wall: bool,
    memory_over_wall: bool,
    memory_told: Option<super::memory::Verdict>,
    memory_refused: bool,
    beside_memory_ended: Option<u32>,
    beside_idle_at: Option<(u32, u64, Instant)>,
    wait_told: Option<Option<ToldWait>>,
    /// Reason a test seeded runtime-learned state that cannot be replayed from a setup tape.
    #[cfg(test)]
    pub(crate) unreplayable: Option<&'static str>,
    stalled: Option<stall::WorkerStall>,
    stall_host: Option<Arc<dyn IdleHost>>,
    pub(super) act_marks: ActMarks,
    usage_told: Option<(UsageSeen, Instant)>,
    pub(super) credits_kept: Option<CreditsKept>,
    withheld: Option<Guard>,
    hold_said: Option<HoldSaid>,
    hold: occupancy_loop::HoldWatch,
    stall_suspect: bool,
    stall_termed: bool,
    stall_grace_s: u32,
    stall_term_after_s: u32,
    resume_limits: bool,
    stall_term: bool,
}

/// Process environment inputs (`$HOME`, real uid, `$TMPDIR`, and worker source) for approval policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalEnv {
    pub home: Option<PathBuf>,
    pub uid: u32,
    pub tmpdir: Option<PathBuf>,
    /// Source used to read the foreground worker's environment.
    pub worker: WorkerSource,
}

/// Source for [`ApprovalEnv::worker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerSource {
    /// Read live from the target session's foreground process.
    Session,
    /// Fixed worker environment (or error) injected by tests.
    Fixed(Result<super::policy::WorkerEnv, String>),
}

impl ApprovalEnv {
    /// Captures [`ApprovalEnv`] from the current process (using a hermetic worker in unit tests).
    pub fn of_process() -> Self {
        #[cfg(unix)]
        // SAFETY: getuid(2) cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        #[cfg(not(unix))]
        let uid = 0;
        #[cfg(test)]
        let worker = WorkerSource::Fixed(Ok(super::policy::WorkerEnv::hermetic(None)));
        #[cfg(not(test))]
        let worker = WorkerSource::Session;
        Self {
            home: aterm_types::dirs::home_dir(),
            uid,
            tmpdir: std::env::var_os("TMPDIR")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
            worker,
        }
    }
}

impl<'a, C: Ctl> Session<'a, C> {
    /// Constructs a [`Session`] targeting `sid` (`None` for `@self`) with default [`SessionSetup`].
    pub fn new(ctl: &'a mut C, sid: Option<String>) -> Self {
        Self::from_setup(ctl, SessionSetup::new(sid))
    }

    /// Constructs a [`Session`] from an explicit [`SessionSetup`].
    pub fn from_setup(ctl: &'a mut C, setup: SessionSetup) -> Self {
        Self {
            ctl,
            turn_end: TurnEndState::new(setup.turn_end_timing.clone()),
            setup,
            caps: Caps::default(),
            outage: None,
            probing: false,
            last: None,
            busy_match: None,
            survey_gone: false,
            context_warn: 0,
            context: Context::Armed,
            limit: None,
            retry: None,
            box_ask: None,
            fx: Fx::os(),
            local_offset: None,
            local_offset_fixed: None,
            zone_offset: limit::zone_offset_at,
            turn_end_due: None,
            clock_marks: None,
            sleeps: SleepBook::default(),
            sleep_count: None,
            sleep_look: None,
            sleep_unknown: None,
            sleep_unsaid: None,
            slept_at: None,
            wake_settle: None,
            restart_waits: 0,
            restart_run: None,
            look_at: None,
            survey_first_seen: None,
            survey_zero: None,
            survey_told: None,
            survey_absent_told: false,
            survey_hold: None,
            survey_kind: None,
            running: super::policy::turn_end::RunningTurn::default(),
            wall_reset: None,
            wall_tap: None,
            reprint_jumps: 0,
            wall_answered: None,
            stuck_asked: None,
            stuck_rung: wall_memory::StuckRung::Quiet,
            stuck_raised: None,
            stuck_told: None,
            stuck_dialog: None,
            stuck_still: None,
            wall_memory: None,
            wall_verdict: None,
            wall_conversation: None,
            wall_refused_said: None,
            left_read: None,
            limit_notice_row: None,
            transcript_wait_seq: None,
            transcript_over_seq: None,
            standing_read: wall_confirm::StandingRead::default(),
            limit_cut: None,
            message_fold: None,
            cwd: None,
            trusted_cwd: None,
            worker_agent: None,
            footer: None,
            busy_read: false,
            ledger: Ledger::off(),
            turn_end_seeded: false,
            first_books: false,
            books_last_ms: None,
            left_keys: Vec::new(),
            repo_seen: None,
            left_at_wall_seq: None,
            box_reason: None,
            changed_streak: 0,
            press_retry: None,
            limit_retry: None,
            limit_dialog_read: None,
            limit_badge_away: None,
            driver: false,
            question: approval_loop::QuestionMemory::default(),
            decline: approval_loop::DeclineMemory::default(),
            question_held: false,
            question_handed_back: false,
            tell_own: false,
            read_back: None,
            refusal_pause: REFUSAL_PAUSE,
            refusal_said: None,
            stop: None,
            adopted_limit: false,
            adopted_box: false,
            program: None,
            status_sid: None,
            person: None,
            launch_status: approval_loop::LaunchStatus::default(),
            input_unread_at: None,
            hand_taken: None,
            launch_used: None,
            draft_seen: None,
            held: false,
            rejudge: false,
            host_held: false,
            host_passed: None,
            host_follows: None,
            agent_woke: None,
            draft_held: None,
            box_first_seen: None,
            countdown: None,
            episode: None,
            last_read_count: None,
            #[cfg(test)]
            deny_cap_off: false,
            #[cfg(test)]
            touch_off: false,
            #[cfg(test)]
            episode_per_box: false,
            #[cfg(test)]
            last_box: None,
            #[cfg(test)]
            read_clock: None,
            stepper: approval_loop::StepperTrack::default(),
            box_proved: None,
            calls_spent: Vec::new(),
            claim: claim::Claim::Off,
            claim_said: Vec::new(),
            claimed_at_naming: false,
            attention_ours: None,
            turn_end_badge: false,
            badge_parked: None,
            exception_now: None,
            exception_open: None,
            task_done_said: false,
            landed_noted_ms: None,
            task_note: None,
            task_point: None,
            point_said: None,
            land_report: None,
            task_seeded: false,
            needs_told: None,
            needs_told_text: String::new(),
            needs_heard: None,
            reach_seen: None,
            account_seen: None,
            roster_seen: None,
            nothing_decided: false,
            decided_withheld: false,
            login_notice_told: None,
            account_stamp: None,
            account_news_at: None,
            login_seen: None,
            probe: UsageProbe::Idle,
            probe_fold: None,
            probe_off: false,
            turn_live: false,
            probe_said: None,
            probe_hold: None,
            reset: ResetStep::Idle,
            reset_taken: None,
            reset_kinds: Vec::new(),
            reset_passed: None,
            reset_again: 0,
            reset_fallback: None,
            decided_on: None,
            focus_key_tried: false,
            halted: false,
            autopilot: approval_loop::AutopilotWord::default(),
            local_offset_at: limit::local_offset_at,
            memory_seen: None,
            memory_refused: false,
            beside_memory_ended: None,
            beside_idle_at: None,
            memory_wall: false,
            memory_over_wall: false,
            memory_told: None,
            wait_told: None,
            #[cfg(test)]
            unreplayable: None,
            stalled: None,
            stall_host: None,
            act_marks: ActMarks::default(),
            usage_told: None,
            credits_kept: None,
            withheld: None,
            hold_said: None,
            hold: occupancy_loop::HoldWatch::default(),
            stall_suspect: false,
            stall_termed: false,
            stall_grace_s: SupervisorConfig::default().human_grace_s,
            stall_term_after_s: SupervisorConfig::default().stall_term_after_s,
            stall_term: SupervisorConfig::default().stall_term,
            resume_limits: SupervisorConfig::default().resume_limits,
        }
    }

    /// Sets the supervisor claim holder name (`aterm-supervise:<pid>` when `None`).
    pub fn set_supervisor_name(&mut self, name: Option<String>) {
        self.setup.holder = name.unwrap_or_else(claim::default_holder);
    }

    /// Configures whether turn-end continuations may type while a user hand is active.
    pub const fn type_under_hand(&mut self, on: bool) {
        self.setup.types_under_hand = on;
    }

    /// Configures read-only watching alongside a self-supervising agent holding `holder`.
    pub fn set_beside(&mut self, holder: Option<String>) {
        if self.setup.beside != holder {
            self.beside_idle_at = None;
        }
        self.setup.beside = holder;
    }

    /// Sets the handover flag preserving badges and claims when a new loop takes over.
    pub fn set_handover(&mut self, flag: Arc<AtomicBool>) {
        self.setup.handover = setup::Handover(Some(flag));
    }

    /// Overrides supervisor claim renewal interval and lease TTL for tests.
    #[cfg(test)]
    pub(crate) fn set_claim_timing(&mut self, renew: Duration, ttl_ms: u64) {
        self.setup.claim_renew = renew;
        self.setup.claim_ttl_ms = ttl_ms;
    }

    /// Sets the approval ledger file path (`None` disables ledger writing).
    pub fn set_approval_ledger(&mut self, path: Option<PathBuf>) {
        self.setup.ledger_path = path;
    }

    /// Returns the configured approval ledger path, if any.
    #[must_use]
    pub fn approval_ledger(&self) -> Option<&Path> {
        self.setup.ledger_path.as_deref()
    }

    /// Returns the [`SessionSetup`] for this session.
    #[must_use]
    pub const fn setup(&self) -> &SessionSetup {
        &self.setup
    }

    /// Overrides the [`ApprovalEnv`] for tests.
    #[cfg(test)]
    pub(crate) fn set_approval_env(&mut self, env: ApprovalEnv) {
        self.setup.approval_env = env;
    }

    /// Sets a fixed wall clock (unix seconds) and local timezone offset for tests.
    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, now: i64, local_offset_s: i64) {
        self.set_clock_at_ms(now.saturating_mul(1000), local_offset_s);
    }

    /// Sets a fixed wall clock (unix milliseconds) and local timezone offset for tests.
    #[cfg(test)]
    pub(crate) fn set_clock_at_ms(&mut self, now_ms: i64, local_offset_s: i64) {
        self.test_clock().set_at_ms(now_ms);
        self.local_offset_fixed = Some(local_offset_s);
        self.local_offset = None;
        self.local_offset_at = |_| None;
    }

    /// Overrides the local timezone offset lookup function for tests.
    #[cfg(test)]
    pub(crate) fn set_local_lookup(&mut self, lookup: fn(i64) -> Option<i64>) {
        self.local_offset_at = lookup;
    }

    /// Configures the scripted clock to jump forward by `jump` seconds after its next read.
    #[cfg(test)]
    pub(crate) fn set_clock_jump(&mut self, jump: i64) {
        self.test_clock().set_jump(jump);
    }

    /// Enables sleep accounting on the scripted clock starting at `total`.
    #[cfg(test)]
    pub(crate) fn count_sleeps(&mut self, total: Duration) {
        self.test_clock().count_sleeps(total);
    }

    /// Records `secs` of prior machine sleep on the scripted clock.
    #[cfg(test)]
    pub(crate) fn count_slept(&mut self, secs: i64) {
        self.test_clock().count_slept(secs);
    }

    /// Sets the simulated boot UUID on the scripted clock.
    #[cfg(test)]
    pub(crate) fn set_boot(&mut self, boot: [u8; 16]) {
        self.test_clock().set_boot(boot);
    }

    /// Sets the simulated last sleep/wake record on the scripted clock.
    #[cfg(test)]
    pub(crate) fn set_last_sleep(&mut self, last: Option<(i64, i64)>) {
        self.test_clock().set_last_sleep(last);
    }

    /// Steps the fixed wall clock forward by `jump` seconds without recording a sleep.
    #[cfg(test)]
    pub(crate) fn set_clock_step(&mut self, jump: i64) {
        self.test_clock().set_step(jump);
    }

    /// Returns the session's sleep tracking state for tests.
    #[cfg(test)]
    pub(crate) fn sleep_book(&self) -> &SleepBook {
        &self.sleeps
    }

    /// Returns a [`ClockJump`] handle to advance the scripted clock during a loop run.
    #[cfg(test)]
    pub(crate) fn clock_jump_handle(&mut self) -> ClockJump {
        ClockJump(
            self.fx
                .scripted_handle()
                .expect("a fixed clock: the session's door reads no virtual clock"),
        )
    }

    /// Configures the scripted clock to advance with real elapsed time.
    #[cfg(test)]
    pub(crate) fn set_clock_running(&mut self) {
        self.test_clock().set_running();
    }

    /// Freezes the monotonic clock alongside the fixed wall clock for tests.
    #[cfg(test)]
    pub(crate) fn set_mono_standing(&mut self) {
        self.test_clock().stand_mono();
    }

    #[cfg(test)]
    fn test_clock(&mut self) -> &super::fx::ScriptedClock {
        self.fx
            .scripted()
            .expect("a fixed clock is staged over the system's, never over a virtual one")
    }

    /// Replaces the session's external effect seam ([`super::fx::Fx`]).
    pub(crate) fn set_fx(&mut self, fx: Fx) {
        self.fx = fx;
    }

    /// Overrides the re-check interval while held for a person in tests.
    #[cfg(test)]
    pub(crate) fn set_person_hold_wait(&mut self, wait: Duration) {
        self.setup.person_hold_wait = Some(wait);
    }

    /// Lets a hold on a box outlast its countdown, the deny cap's negative control.
    #[cfg(test)]
    pub(crate) fn set_deny_cap_off(&mut self) {
        self.deny_cap_off = true;
        self.seeds_learned_state("the loop before the deny cap");
    }

    /// Reads no box episode as touched, the touch rule's negative control.
    #[cfg(test)]
    pub(crate) fn set_touch_off(&mut self) {
        self.touch_off = true;
        self.seeds_learned_state("the loop that reads no episode as touched");
    }

    /// Starts a box episode afresh at every changed box, the episode's negative control.
    #[cfg(test)]
    pub(crate) fn set_episode_per_box(&mut self) {
        self.episode_per_box = true;
        self.seeds_learned_state("the loop that starts an episode at every box");
    }

    /// Places each `text` read a second after the one before, the clock its countdowns tick on.
    #[cfg(test)]
    pub(crate) fn set_read_clock(&mut self) {
        self.read_clock = Some((self.fx.mono(), 0));
        self.seeds_learned_state("the reads' own clock (`set_read_clock`)");
    }

    #[cfg(test)]
    fn read_clock_now(&self) -> Option<Instant> {
        let (set, reads) = self.read_clock?;
        Some(set + Duration::from_secs(u64::from(reads.saturating_sub(1))))
    }

    /// Advances any active post-wake settle deadline to the current instant for tests.
    #[cfg(test)]
    pub(crate) fn pass_wake_settle(&mut self) {
        let now = self.fx.mono();
        let unix_ms = self.unix_ms_of(now);
        if let Some((settle, _)) = &mut self.wake_settle {
            settle.at = now;
            settle.unix_ms = unix_ms;
        }
        if self
            .turn_end_due
            .as_ref()
            .is_some_and(|d| d.why == turn_end_loop::WOKE)
        {
            self.set_due(now, turn_end_loop::WOKE);
        }
    }

    /// Marks this session as having test-seeded runtime state (`why`), skipping tape replay.
    #[cfg(test)]
    pub(crate) fn seeds_learned_state(&mut self, why: &'static str) {
        self.unreplayable = Some(why);
    }

    /// Overrides the named timezone offset lookup function for tests.
    #[cfg(test)]
    pub(crate) fn set_zone_lookup(&mut self, lookup: fn(&str, i64) -> Option<i64>) {
        self.zone_offset = lookup;
    }

    /// Sets the bounded wait timings ([`crate::harness::Timings`]) for the loop.
    pub fn set_timings(&mut self, timings: crate::harness::Timings) {
        self.setup.claim_ttl_ms = claim::ttl_ms(&timings);
        self.setup.redecide_after_skip = timings.redecide_after_skip;
    }

    /// Sets the turn-end policy timings and resets turn-end policy state.
    pub fn set_turn_end_timing(&mut self, timing: TurnEndTiming) {
        self.turn_end = TurnEndState::new(timing.clone());
        self.setup.turn_end_timing = timing;
    }

    /// Sets the maximum outage ride-out duration (`--reconnect-s`).
    pub const fn set_reconnect(&mut self, window: Duration) {
        self.setup.reconnect = window;
    }

    /// Overrides the duration of missed presses before raising an attention badge.
    #[cfg(test)]
    pub(crate) fn set_press_badge_after(&mut self, after: Duration) {
        self.setup.press_badge_after = after;
    }

    /// Overrides rating survey dismissal retry pauses for tests.
    #[cfg(test)]
    pub(crate) fn set_survey_retry(&mut self, pauses: [Duration; SURVEY_TRIES]) {
        self.setup.survey_retry = pauses;
    }

    /// Overrides the rating survey settle delay before pressing `0`.
    #[cfg(test)]
    pub(crate) fn set_survey_settle(&mut self, settle: Duration) {
        self.setup.survey_settle = settle;
    }

    /// Overrides the grace window for a survey `0` press to take effect.
    #[cfg(test)]
    pub(crate) fn set_survey_land(&mut self, land: Duration) {
        self.setup.survey_land = land;
    }

    /// Overrides `/usage` probe wait bounds for tests.
    #[cfg(test)]
    pub(crate) fn set_probe_waits(
        &mut self,
        open: Duration,
        settle: Duration,
        step: Duration,
        close: Duration,
    ) {
        self.setup.probe_waits = ProbeWaits {
            open,
            settle,
            step,
            close,
        };
    }

    /// Overrides usage-limit reset wait bounds for tests.
    #[cfg(test)]
    pub(crate) fn set_reset_waits(&mut self, waits: reset_loop::ResetWaits) {
        self.setup.reset_waits = waits;
    }

    /// Overrides the settle delay required before pressing an approval box.
    #[cfg(test)]
    pub(crate) fn set_box_settle(&mut self, settle: Duration) {
        self.setup.box_settle = settle;
    }

    /// Overrides the transcript poll interval when resolving an approval box call.
    #[cfg(test)]
    pub(crate) fn set_box_call_step(&mut self, step: Duration) {
        self.setup.box_call_step = step;
    }

    /// Overrides the settle delay before accepting an autocompact prompt.
    #[cfg(test)]
    pub(crate) fn set_autocompact_settle(&mut self, settle: Duration) {
        self.setup.autocompact_settle = settle;
    }

    /// Overrides the background-work settle duration before offering a host break.
    #[cfg(test)]
    pub(crate) fn set_background_settle(&mut self, settle: Duration) {
        self.setup.background_settle = settle;
    }

    /// Returns the probed server capabilities.
    #[cfg(test)]
    pub(crate) fn caps(&self) -> Caps {
        self.caps
    }

    pub(super) fn call(&mut self, args: &[&str]) -> Result<CtlReply, Fail> {
        // Nor does a stopped loop begin a wait: some steps wait twice with no stop check
        // between, and every wait goes through here, so a stop ends the run within one wait.
        if self.stopped() {
            if claim::is_input(args) {
                return Err(Fail::Hard(STOPPED_WRITE.to_string()));
            }
            if args.iter().find(|a| !a.starts_with('@')) == Some(&"await") {
                return Err(Fail::Hard(STOPPED_WAIT.to_string()));
            }
        }
        self.renew_claim_if_due();
        if let Some(refused) = self.claim_refuses(args) {
            return Ok(refused);
        }
        #[cfg(test)]
        if args.first() == Some(&"text")
            && let Some((_, reads)) = self.read_clock.as_mut()
        {
            *reads = reads.saturating_add(1);
        }
        let mut full: Vec<&str> = Vec::with_capacity(args.len() + 1);
        if let Some(sid) = &self.setup.sid {
            full.push(sid.as_str());
        }
        full.extend_from_slice(args);
        let r = self.ctl.call(&full);
        self.watch_sleep();
        let r = r.map_err(Fail::Hard)?;
        self.note_status(args, &r);
        if claim::is_input(args) && !(r.is_err("halted") || r.is_err("busy") || r.is_err("rate")) {
            self.refusal_said = None;
        }
        if claim::is_input(args) && r.warned_input_unread() {
            self.stall_suspect = true;
        }
        let kind = || args[..args.len().min(2)].join(" ");
        if self.unserved(&r) {
            if self.outage.is_none() {
                let kind = kind();
                self.outage = Some(Outage {
                    since: self.fx.mono(),
                    kind,
                    told: false,
                    back: false,
                    pause: self.setup.pause,
                });
            }
        } else if !self.probing
            && self.outage.as_ref().is_some_and(|o| {
                let kind = kind();
                o.kind == kind || (kind == "await seq" && r.ok())
            })
        {
            self.outage = None;
        }
        Ok(r)
    }

    pub(super) fn unserved(&self, r: &CtlReply) -> bool {
        r.unserved(self.outage.is_some())
    }

    pub(super) fn fault(&self, r: &CtlReply, what: String) -> Fail {
        if self.unserved(r) {
            Fail::Lost(what)
        } else {
            Fail::Hard(what)
        }
    }

    /// Reads the current terminal screen via `text --json`.
    pub fn read_screen(&mut self) -> Result<Screen, String> {
        self.screen().map_err(String::from)
    }

    /// Reads the current [`Turn`] and its associated [`ScreenReader`].
    pub fn phase_now(&mut self) -> Result<(Turn, &'static dyn ScreenReader), String> {
        let screen = self.read_screen()?;
        let reader = self.program_reader(&screen.rows)?;
        Ok((self.turn_of(screen), reader))
    }

    /// Resolves the session's program reader and waits up to `timeout` for the turn to end.
    #[cfg(test)]
    pub(crate) fn await_phase(
        &mut self,
        timeout: Duration,
    ) -> Result<(Turn, &'static dyn ScreenReader), String> {
        let _ = self.program_reader(&[]);
        let turn = self.await_turn(timeout)?;
        let reader = self.reader(&turn.screen.rows);
        Ok((turn, reader))
    }

    /// Resolves the [`ScreenReader`] from `status` or falls back to screen identification.
    pub(crate) fn program_reader(
        &mut self,
        rows: &[String],
    ) -> Result<&'static dyn ScreenReader, String> {
        self.program = self.foreground_program().map_err(String::from)?;
        Ok(self.reader(rows))
    }

    pub(super) fn reader(&self, rows: &[String]) -> &'static dyn ScreenReader {
        let by_screen = aterm_phase::identify(self.program.as_deref(), rows);
        let no_grammar = match by_screen.program() {
            Program::Generic => true,
            Program::Claude | Program::Codex | Program::Agy | Program::Muse | Program::Gemini => {
                false
            }
        };
        if self.program.is_none()
            && no_grammar
            && aterm_phase::parse_prompt_v2(rows).is_some_and(|p| p.head_off_screen)
        {
            return &aterm_phase::ClaudeReader;
        }
        by_screen
    }

    fn busy_up(&mut self, guard: &str, rows: &[String]) -> bool {
        if self.busy_match.as_ref().is_none_or(|(g, _)| g != guard) {
            self.busy_match = aterm_observe::row_matcher(guard).ok().map(|m| {
                let up: BusyMatch = Box::new(move |r: &str| !m.clears(r));
                (guard.to_string(), up)
            });
        }
        self.busy_match
            .as_ref()
            .is_some_and(|(_, m)| rows.iter().any(|r| m(r)))
    }

    pub(super) fn turn_of(&self, screen: Screen) -> Turn {
        let reader = self.reader(&screen.rows);
        let mut phase = reader.phase(&screen.rows);
        let carried = phase == Phase::Idle && self.transcript_wait_seq == Some(screen.seq);
        if carried {
            phase = Phase::Busy;
        } else if phase == Phase::Busy && self.transcript_over_seq == Some(screen.seq) {
            phase = Phase::Idle;
        }
        if phase == Phase::Busy
            && !carried
            && let Some(w) = self.remembered_wall(reader, &screen.rows)
        {
            phase = Phase::Limited {
                message: w.message,
                reset: w.reset,
            };
        }
        Turn {
            phase,
            screen,
            timed_out: false,
        }
    }

    pub(super) fn point_of(&mut self, screen: Screen) -> Turn {
        let reader = self.reader(&screen.rows);
        match reader.phase(&screen.rows) {
            Phase::Idle => {
                self.transcript_wait(reader, &screen);
            }
            Phase::Busy => {
                self.transcript_wait_over(reader, &screen);
            }
            _ => {}
        }
        self.turn_of(screen)
    }

    pub(super) fn review_key(&self, turn: &Turn) -> String {
        point_key(self.reader(&turn.screen.rows), turn)
    }

    fn file_request(&self, turn: &Turn) -> Option<String> {
        file_request_key(self.reader(&turn.screen.rows), turn)
    }

    fn first_busy_wait(&self) -> Option<BusyWait> {
        let Some(screen) = &self.last else {
            let named = self.reader(&[]);
            let guard = named.busy_guard().or_else(|| {
                self.program
                    .is_none()
                    .then(|| aterm_phase::Program::Claude.reader().busy_guard())
                    .flatten()
            })?;
            return Some(BusyWait {
                guard,
                span: None,
                unscoped: true,
                boxed: named.busy_row_outlives_boxes(),
            });
        };
        let reader = self.reader(&screen.rows);
        let guard = reader.busy_guard()?;
        let (_, span) = live_rows(reader, screen);
        Some(BusyWait {
            guard,
            span,
            unscoped: false,
            boxed: reader.busy_row_outlives_boxes(),
        })
    }

    fn screen(&mut self) -> Result<Screen, Fail> {
        #[cfg(test)]
        if super::fx::planted_clock_read() {
            let _ = self.fx.mono();
        }
        let asked = self.fx.mono();
        let (screen, read_by) = self.read_once()?;
        self.fx.screen_read(&screen);
        // Tests may put the reads on a clock of their own
        // ([`Self::set_read_clock`]).
        #[cfg(test)]
        let asked = self.read_clock_now().unwrap_or(asked);
        // The box's auto-deny countdown, placed at the instant the read was
        // asked for: the vendor's deny, give or take a tick, believed once a
        // later read sees it tick ([`approval_loop::Countdown`]); and the box
        // episode, from the first read that shows a box to the first that
        // shows none, its baseline the person count of the read before it
        // ([`approval_loop::Episode`]).
        self.keep_countdown(&screen.rows, asked);
        self.keep_episode(&screen, self.last_read_count);
        self.last_read_count = Some(screen.human_seq);
        if self.caps.await_agent.is_none() {
            self.caps.await_agent = Some(screen.generation.is_some());
        }
        let reader = read_by.unwrap_or_else(|| self.reader(&screen.rows));
        let open = reader.survey(&screen.rows);
        self.survey_gone |= !open;
        if !open {
            self.survey_first_seen = None;
        }
        if let Some(mode) = footer_mode(&screen.rows) {
            self.footer = Some(mode);
        } else if self.footer.is_none() && !self.busy_read {
            self.busy_read = reader.phase(&screen.rows) == Phase::Busy;
        }
        // Gemini CLI's usage-limit dialog read, a point or not, and the
        // worker working past it ([`Self::note_limit_dialog`]).
        let busy = self.limit_dialog_read.is_some() && reader.phase(&screen.rows) == Phase::Busy;
        self.note_limit_dialog(reader.program(), &screen.rows, busy);
        let credits = reader.credits(&screen.rows);
        self.note_credits(reader, credits.as_ref(), &screen.rows);
        self.note_usage(reader, credits, &screen.rows);
        let draft = reader
            .composer(&screen.rows)
            .filter(|c| typed_draft_in(Some(c), reader.placeholder(), &screen))
            .map(|(_, lines)| lines.join("\n"));
        self.draft_seen = match (draft, self.draft_seen.take()) {
            (Some(d), Some((seen, at))) if d == seen => Some((seen, at)),
            (Some(d), _) => Some((d, self.fx.mono())),
            (None, _) => None,
        };
        let mut rows = self.last.take().map(|l| l.rows).unwrap_or_default();
        rows.clone_from(&screen.rows);
        self.last = Some(Screen {
            rows,
            generation: screen.generation.clone(),
            launch: screen.launch.clone(),
            ..screen
        });
        Ok(screen)
    }

    fn note_usage(
        &mut self,
        reader: &dyn ScreenReader,
        credits: Option<aterm_phase::credits::Credits>,
        rows: &[String],
    ) {
        let Some(host) = self.stall_host.clone() else {
            return;
        };
        let seen = UsageSeen {
            credits,
            warning: reader.usage_warning(rows),
        };
        let now = self.fx.mono();
        let retell = Duration::from_secs(
            u64::try_from(crate::harness::accounts::CREDITS_RETELL_S).unwrap_or(u64::MAX),
        );
        let due = match &self.usage_told {
            Some((told, at)) => *told != seen || (seen.says() && now >= *at + retell),
            None => true,
        };
        if due {
            host.usage_seen(&seen);
            self.usage_told = Some((seen, now));
        }
    }

    fn note_credits(
        &mut self,
        reader: &dyn ScreenReader,
        credits: Option<&aterm_phase::credits::Credits>,
        rows: &[String],
    ) {
        let started = self
            .stall_host
            .as_ref()
            .and_then(|h| h.agent_started())
            .map(|unix| self.instant_of(unix));
        if self
            .credits_kept
            .as_ref()
            .is_some_and(|kept| kept.model.is_some() && started.is_some_and(|at| at > kept.since))
        {
            self.credits_kept = None;
        }
        let Some(credits) = credits else {
            let now = self.now_unix();
            let mono = self.fx.mono();
            let stands = self.credits_kept.as_ref().is_some_and(|kept| {
                kept.model.is_some()
                    || match kept.reset.as_ref().and_then(|r| r.unix) {
                        Some(unix) => unix > now,
                        None => {
                            mono.saturating_duration_since(kept.since)
                                < self.turn_end.timing.credits_bound
                        }
                    }
            });
            if reader.composer(rows).is_some() && !stands {
                self.credits_kept = None;
            }
            return;
        };
        let now = self.fx.mono();
        let kept = self.credits_kept.get_or_insert(CreditsKept {
            reset: None,
            since: now,
            model: None,
        });
        if let Some(model) = credits
            .model
            .as_deref()
            .and_then(super::policy::turn_end::credits_model)
        {
            kept.model = Some(model);
        }
        let Some(begun) = &credits.started else {
            return;
        };
        let Some(text) = begun.reset.as_deref() else {
            return;
        };
        if kept.reset.as_ref().is_some_and(|r| r.text == text) {
            return;
        }
        let session = begun.window == Some(aterm_phase::credits::Window::Session);
        let unix = self.reset_unix_for(Some(text), session, true, false, rows);
        if let Some(kept) = &mut self.credits_kept {
            kept.reset = Some(CreditsReset {
                text: text.to_string(),
                unix,
            });
        }
    }

    fn read_once(&mut self) -> Result<(Screen, Option<&'static dyn ScreenReader>), Fail> {
        let read = self.read_shaped()?;
        let (screen, read_by) =
            self.limit_words_read_above(read.screen, read.read_by, read.above)?;
        Ok((self.message_bounded_by_cells(screen, read_by), read_by))
    }

    fn message_bounded_by_cells(
        &mut self,
        mut screen: Screen,
        read_by: Option<&'static dyn ScreenReader>,
    ) -> Screen {
        let reader = read_by.unwrap_or_else(|| self.reader(&screen.rows));
        let Some((caret, blanks)) = reader.message_blanks(&screen.rows) else {
            return screen;
        };
        let key = (screen.seq, screen.first, screen.rows.clone());
        let inner = match self.message_fold.take() {
            Some((seen, inner)) if seen == key => inner,
            _ => self.message_cells(CellsAt::Grid(screen.first), caret, &blanks),
        };
        if !inner.is_empty() {
            screen.rows = reader.fold_message(&screen.rows, caret, &inner);
        }
        self.message_fold = Some((key, inner));
        screen
    }

    pub(super) fn rows_as_read(&self, screen: &Screen) -> Vec<String> {
        if let Some(((seq, first, rows), inner)) = &self.message_fold
            && (*seq, *first) == (screen.seq, screen.first)
            && !inner.is_empty()
        {
            let reader = self.reader(rows);
            let folded = reader
                .message_blanks(rows)
                .map(|(caret, _)| reader.fold_message(rows, caret, inner));
            if folded.as_ref() == Some(&screen.rows) {
                return rows.clone();
            }
        }
        screen.rows.clone()
    }

    pub(super) fn rows_bounded_by_cells(
        &mut self,
        rows: Vec<String>,
        reader: &'static dyn ScreenReader,
        abs: Option<u64>,
    ) -> Vec<String> {
        let (Some(abs), Some((caret, blanks))) = (abs, reader.message_blanks(&rows)) else {
            return rows;
        };
        let inner = self.message_cells(CellsAt::Abs(abs), caret, &blanks);
        if inner.is_empty() {
            return rows;
        }
        reader.fold_message(&rows, caret, &inner)
    }

    fn message_cells(&mut self, at: CellsAt, caret: usize, blanks: &[usize]) -> Vec<usize> {
        let mut drawn = |row: usize| {
            let at = match at {
                CellsAt::Grid(first) => (first + row).to_string(),
                CellsAt::Abs(abs) => format!("abs={}", abs.saturating_add(row as u64)),
            };
            match self.call(&["cell", &at, "0"]) {
                Ok(r) if r.ok() => crate::supervise::screen::cell_drawn(&r.stdout),
                _ => None,
            }
        };
        let own = match drawn(caret) {
            Some((glyph, style)) if glyph == ">" => style,
            _ => return Vec::new(),
        };
        let mut inner = Vec::new();
        for &row in blanks {
            match drawn(row) {
                Some((glyph, style)) if glyph == "%20" || glyph.is_empty() => {
                    if style != own {
                        return inner;
                    }
                    inner.push(row);
                }
                _ => return Vec::new(),
            }
        }
        Vec::new()
    }

    pub(super) fn limit_words_read_above(
        &mut self,
        mut screen: Screen,
        read_by: Option<&'static dyn ScreenReader>,
        above: Option<Vec<String>>,
    ) -> Result<(Screen, Option<&'static dyn ScreenReader>), Fail> {
        let claude = match self.reader(&screen.rows).program() {
            Program::Claude => true,
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
                false
            }
        };
        let Some(words) = claude
            .then(|| aterm_phase::limit_words_above(&screen.rows))
            .flatten()
        else {
            return Ok((screen, read_by));
        };
        let key = (screen.seq, screen.first, screen.rows.clone());
        let cut = match self.limit_cut.take() {
            Some((seen, cut)) if seen == key => cut,
            _ => self.programs_words_above(&screen, words, above)?,
        };
        self.limit_cut = Some((key, cut));
        let Some(end) = cut else {
            return Ok((screen, read_by));
        };
        screen.rows.drain(..=end);
        screen.first += end + 1;
        Ok((screen, None))
    }

    fn programs_words_above(
        &mut self,
        screen: &Screen,
        words: aterm_phase::WordsAbove,
        above: Option<Vec<String>>,
    ) -> Result<Option<usize>, Fail> {
        use aterm_phase::phase::{WordsOf, words_of};
        let grid = if let Some(mut grid) = above.filter(|a| a.len() == screen.first) {
            grid.extend(screen.rows.iter().cloned());
            grid
        } else {
            let whole = self.grid()?;
            let same = whole
                .rows
                .get(screen.first..)
                .is_some_and(|rows| wall_confirm::same_rows(rows, &screen.rows));
            if !same {
                return Ok(None);
            }
            whole.rows
        };
        let at = screen.first + words.row;
        let judged = match words_of(&grid, at) {
            WordsOf::Above => {
                let Some(mut joined) = self.rows_up_to_owner(&grid)? else {
                    return Ok(None);
                };
                let above = joined.len();
                joined.extend(grid);
                words_of(&joined, above + at)
            }
            judged => judged,
        };
        Ok((judged == WordsOf::Program).then_some(words.end))
    }

    fn read_shaped(&mut self) -> Result<Shaped, Fail> {
        let tail = format!("tail={TAIL_ROWS}");
        let screen = self.text_json(&["text", "--json", &tail])?;
        let reader = self.reader(&screen.rows);
        let gap = gap_above_tail(&screen);
        if !tail_misses_the_live_rows(reader, &screen) && !drawn_above_tail(reader, &screen) && !gap
        {
            return Ok(Shaped {
                screen,
                read_by: Some(reader),
                above: None,
            });
        }
        let zone = gap || screen.rows.last().is_some_and(|r| r.trim().is_empty());
        let whole = self.text_json(&["text", "--json"])?;
        let (screen, above) = if zone {
            self.live_zone(whole)
        } else {
            (whole, Vec::new())
        };
        Ok(Shaped {
            screen,
            read_by: None,
            above: Some(above),
        })
    }

    fn live_zone(&self, mut whole: Screen) -> (Screen, Vec<String>) {
        let first = aterm_phase::live_zone_start(&whole.rows, TAIL_ROWS);
        let mut above: Vec<String> = whole.rows.drain(..first).collect();
        let whole_first = std::mem::replace(&mut whole.first, first);
        if tail_misses_the_live_rows(self.reader(&whole.rows), &whole) {
            above.append(&mut whole.rows);
            whole.rows = above;
            whole.first = whole_first;
            return (whole, Vec::new());
        }
        (whole, above)
    }

    pub(super) fn grid(&mut self) -> Result<Screen, Fail> {
        let screen = self.text_json(&["text", "--json"])?;
        self.fx.screen_read(&screen);
        Ok(screen)
    }

    pub(super) fn text_json(&mut self, args: &[&str]) -> Result<Screen, Fail> {
        self.whole_frame(|s| {
            let r = s.call(args)?;
            if !r.ok() {
                return Err(s.fault(
                    &r,
                    format!("{} failed: {}", args.join(" "), r.stderr.trim()),
                ));
            }
            let screen = parse_text_json(&r.stdout).map_err(Fail::Hard)?;
            let open = screen.frame_open;
            Ok((screen, open))
        })
    }

    pub(super) fn whole_frame<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<(T, bool), Fail>,
    ) -> Result<T, Fail> {
        let mut rereads = 0;
        loop {
            let (got, open) = read(self)?;
            if !open || rereads == FRAME_REREADS {
                return Ok(got);
            }
            rereads += 1;
            if !matches!(self.wait(&["frame"], FRAME_WAIT)?, Wait::Latched) {
                return Ok(got);
            }
        }
    }

    /// Whether the program's own window title says its run goes on: one `title` read (the
    /// OSC 0/2 title the program set, whatever is pinned over it, which `status`'s `subject=`
    /// shows instead). `false` where the reader reads no titles (no read is made) and where
    /// the host refuses the read.
    fn title_says_running(&mut self, reader: &dyn ScreenReader) -> Result<bool, Fail> {
        if reader.title_runs("").is_none() {
            return Ok(false);
        }
        let r = self.call(&["title"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("title failed: {}", r.stderr.trim())));
        }
        if !r.ok() {
            return Ok(false);
        }
        let title = r.stdout.trim_end_matches(['\r', '\n']);
        let title = title.strip_prefix("OK").unwrap_or(title);
        let title = title.strip_prefix(' ').unwrap_or(title);
        Ok(reader.title_runs(title) == Some(true))
    }

    fn wait(&mut self, cond: &[&str], step: Duration) -> Result<Wait, Fail> {
        self.wait_at(cond, step).map(|(wait, _)| wait)
    }

    /// [`Self::wait`], with the content seq the host says the condition
    /// latched at (`OK <kind> <seq>`): `None` when it did not latch or the
    /// host said no seq.
    fn wait_at(&mut self, cond: &[&str], step: Duration) -> Result<(Wait, Option<u64>), Fail> {
        let ms = step.as_nanos().div_ceil(1_000_000).to_string();
        let mut args = vec!["await"];
        args.extend_from_slice(cond);
        args.push("timeout");
        args.push(&ms);
        let r = self.call(&args)?;
        if r.ok() {
            let latched = r
                .stdout
                .split_whitespace()
                .nth(2)
                .and_then(|seq| seq.parse().ok());
            Ok((Wait::Latched, latched))
        } else if self.unserved(&r) {
            Err(Fail::Lost(format!(
                "await {} failed: {}",
                cond.join(" "),
                r.stderr.trim()
            )))
        } else if r.timed_out() {
            Ok((Wait::TimedOut, None))
        } else if r.usage_error() {
            Ok((Wait::Unsupported, None))
        } else {
            Err(Fail::Hard(format!(
                "await {} failed: {}",
                cond.join(" "),
                r.stderr.trim()
            )))
        }
    }

    fn wait_gone(&mut self, busy: &BusyWait, step: Duration) -> Result<(), Fail> {
        let span = busy.span.map(|(a, b)| (a.to_string(), b.to_string()));
        let mut cond = vec!["gone", busy.guard.as_str()];
        if let Some((a, b)) = &span {
            cond.extend(["rows", a.as_str(), b.as_str()]);
        }
        match self.await_gone(&cond, step) {
            Err(Fail::Hard(e)) if busy.span.is_some() && e.contains("ERR bad rows") => Ok(()),
            other => other.map(drop),
        }
    }

    fn await_gone(&mut self, cond: &[&str], step: Duration) -> Result<bool, Fail> {
        match self.wait(cond, step)? {
            Wait::Latched => Ok(true),
            Wait::TimedOut => Ok(false),
            Wait::Unsupported => Err(Fail::Hard(format!(
                "await {} is not known to this host",
                cond.join(" ")
            ))),
        }
    }

    fn spent_turn(&self) -> Turn {
        match &self.last {
            Some(screen) => Turn {
                phase: self.reader(&screen.rows).phase(&screen.rows),
                screen: screen.clone(),
                timed_out: true,
            },
            None => Turn {
                phase: Phase::Busy,
                screen: Screen::default(),
                timed_out: true,
            },
        }
    }

    /// Waits up to `timeout` for the worker's turn to reach a non-busy phase.
    #[cfg(test)]
    pub(crate) fn await_turn(&mut self, timeout: Duration) -> Result<Turn, String> {
        self.await_turn_to(timeout, &mut std::io::stderr())
    }

    #[cfg(test)]
    fn await_turn_to(&mut self, timeout: Duration, log: &mut dyn Write) -> Result<Turn, String> {
        let deadline = self.fx.mono() + timeout;
        let mut gone_first = true;
        let mut moved = Saw::default();
        loop {
            let remaining = deadline.saturating_duration_since(self.fx.mono());
            match self.await_turn_from(remaining, gone_first, &mut moved, None, None, &mut Alone) {
                Ok(turn) => return Ok(turn),
                Err(Fail::Lost(why)) => {
                    let rode = self.ride_out(why, deadline, &mut |line| {
                        log_line(&mut *log, line);
                        Ok(())
                    })?;
                    match rode {
                        Rode::Spent => return Ok(self.spent_turn()),
                        Rode::Stopped => return Err("stopped".to_string()),
                        Rode::Back { .. } => {}
                    }
                    gone_first = false;
                }
                Err(Fail::Hard(e)) => return Err(e),
            }
        }
    }

    fn await_turn_from(
        &mut self,
        timeout: Duration,
        gone_first: bool,
        saw: &mut Saw,
        mut duty: Option<&mut SurveyDuty<'_>>,
        opts: Option<&SuperviseOpts>,
        review: &mut dyn Review,
    ) -> Result<Turn, Fail> {
        let deadline = self.fx.mono() + timeout;
        let mut next = match gone_first.then(|| self.first_busy_wait()).flatten() {
            Some(busy) => Next::Gone(busy),
            None => Next::Settle,
        };
        let mut background_since: Option<Instant> = None;
        let mut footer_seen = false;
        // Since when every read has been idle (or a question) under the program's own title
        // saying its run goes on ([`TITLE_HOLD`]); a busy read ends it.
        let mut title_held: Option<Instant> = None;
        // Since when every read has been idle with no evidence ([`NO_EVIDENCE_HOLD`]).
        let mut unvouched_since: Option<Instant> = None;
        // The next short settle's cap while that hold runs: its read lands as the hold ends.
        let mut settle_cap = SETTLE_CAP;
        loop {
            // The hosted loop's stop ends a turn that may run for hours:
            // the look it returns to says so ([`Self::look`]). Checked here
            // and before each of the turn's other waits (before the wait
            // for the next change), so a stop
            // in any wait of the turn ends the turn, spent, rather than as
            // the refusal of its next wait ([`Self::call`]).
            if self.stopped() {
                return Ok(self.spent_turn());
            }
            let step = deadline
                .saturating_duration_since(self.fx.mono())
                .min(self.setup.wait_step);
            let mut unsettled = false;
            let mut settled = false;
            match &next {
                Next::Gone(busy) => {
                    self.wait_gone(busy, busy.step(step))?;
                    unsettled = true;
                }
                Next::Settle => {
                    let cap = std::mem::replace(&mut settle_cap, SETTLE_CAP);
                    self.wait(&["idle", SETTLE_MS], step.min(cap))?;
                    unsettled = true;
                }
                Next::Idle => {
                    settled = matches!(
                        self.wait(&["idle", IDLE_MS], step.min(WRITING_CAP))?,
                        Wait::Latched
                    );
                }
            }
            let mut screen = self.screen()?;
            let reader = self.reader(&screen.rows);
            if self.fx.mono() < deadline {
                self.watch_context(&screen, reader, &mut |line| review.say(line))?;
            }
            let mut phase = reader.phase(&screen.rows);
            if phase != Phase::Prompt {
                self.box_gone();
                self.offer_gone(aterm_phase::phase::has_composer_frame(&screen.rows));
            }
            let writing = screen.frame_open
                || (!settled
                    && !matches!(phase, Phase::Busy | Phase::Prompt)
                    && reader.composer(&screen.rows).is_none()
                    && self.probe == UsageProbe::Idle
                    && self.reset == ResetStep::Idle);
            if writing {
                self.dialog_note(&screen, review);
            }
            let carried = phase == Phase::Idle && !writing && self.transcript_wait(reader, &screen);
            if carried {
                phase = Phase::Busy;
            }
            let mut transcript_idle = false;
            if phase == Phase::Busy && !carried && self.transcript_wait_over(reader, &screen) {
                phase = Phase::Idle;
                transcript_idle = true;
            }
            if phase != Phase::Busy && !writing {
                // A READ TORN MID-REPAINT IS NO POINT. The footer this wait saw leave latches
                // at the first batch it is off the screen, and a reply growing under the window
                // is repainted row by row with its footer last: a read there shows the composer
                // frame and no busy footer, idle to every reader, while the reply still streams
                // (Claude Code 2.1.307, `r7-both` L6: `- check 65 passed` of 80 judged a point,
                // the reply's `NEEDS YOU:` above it never read). An idle read straight after
                // that latch whose frame its reader judges not whole
                // ([`ScreenReader::frame_torn`]: by default the composer frame's geometry,
                // [`aterm_phase::phase::composer_frame_torn`]) is read once more after the
                // short settle, as a read with no composer yet is (`writing`), and that read
                // decides. A frame the program bracketed in DEC 2026 never gets this far
                // unfinished: the server holds `await gone` until it closes, and the read
                // waits for it ([`Self::text_json`]); one it did not bracket, the server's
                // `gone` settles on ([`aterm_core::terminal::GONE_SETTLE`]).
                if phase == Phase::Idle
                    && footer_seen
                    && matches!(next, Next::Gone(_))
                    && reader.frame_torn(&screen.rows)
                    && self.fx.mono() < deadline
                {
                    next = Next::Settle;
                    footer_seen = false;
                    continue;
                }
                // An idle read its reader does not vouch for (a row nobody knows, a first
                // frame, a menu) is a point only once it has stood NO_EVIDENCE_HOLD, every read
                // between reading it too, or after the idle window held still (`settled`). Each
                // settle in the hold is capped to land its read as the hold ends, so a standing
                // one is told NO_EVIDENCE_HOLD after its first read, still or animated. Not for a
                // read the transcript calls idle, nor while the `/usage` panel or a limit reset's
                // step is owed (their screens are the loop's).
                if phase == Phase::Idle
                    && !settled
                    && !transcript_idle
                    && self.probe == UsageProbe::Idle
                    && self.reset == ResetStep::Idle
                    && !reader.phase_authoritative(&screen.rows, &phase)
                    && self.fx.mono() < deadline
                {
                    let since = *unvouched_since.get_or_insert(self.fx.mono());
                    let stood = self.fx.since(since);
                    if stood < NO_EVIDENCE_HOLD {
                        settle_cap = NO_EVIDENCE_HOLD.saturating_sub(stood).min(SETTLE_CAP);
                        next = Next::Settle;
                        footer_seen = false;
                        continue;
                    }
                } else {
                    unvouched_since = None;
                }
                // An idle or question read under the program's own running title (Muse Code's
                // end-of-turn review: the reply whole, the screen still, only the title's
                // spinner says the run goes on) is held, read again after the short settle,
                // until the title rests, or until the hold has run TITLE_HOLD without a busy
                // read, when it is taken as it stands. Nothing is typed into the review.
                if matches!(phase, Phase::Idle | Phase::Question) && self.fx.mono() < deadline {
                    match title_held {
                        Some(at) if self.fx.since(at) >= TITLE_HOLD => {
                            review.note(&format!(
                                "NOTE seq={} the agent's title still said its run goes on {} s \
                                 into the hold: its end read as the screen shows it",
                                screen.seq,
                                TITLE_HOLD.as_secs()
                            ));
                        }
                        _ if self.title_says_running(reader)? => {
                            title_held.get_or_insert(self.fx.mono());
                            next = Next::Settle;
                            footer_seen = false;
                            continue;
                        }
                        _ => {}
                    }
                }
                return Ok(Turn {
                    phase,
                    screen,
                    timed_out: false,
                });
            }
            // A busy read, or one still writing: neither hold stood.
            unvouched_since = None;
            title_held = None;
            if writing {
                phase = Phase::Busy;
            }
            let may_act = !writing
                && ((self.stall_host.is_some()
                    && (carried || reader.background_wait(&screen.rows).is_some()))
                    || duty.as_deref().is_some_and(|d| {
                        reader.survey(&screen.rows) || *d.survey != Survey::Closed
                    }));
            let off = if review.unattended()
                && let Some(opts) = opts
            {
                if may_act {
                    !self.autopilot_allows_busy(opts)?
                } else {
                    self.autopilot(opts) == Autopilot::Off
                }
            } else {
                false
            };
            let signing_in = super::policy::login::dialog_stands(&screen.rows);
            if phase == Phase::Busy && !writing && !carried {
                if let Some(w) = self.remembered_wall(reader, &screen.rows) {
                    return Ok(Turn {
                        phase: Phase::Limited {
                            message: w.message,
                            reset: w.reset,
                        },
                        screen,
                        timed_out: false,
                    });
                }
                if !signing_in {
                    self.end_memory_on_live_turn(reader, &screen.rows, screen.seq);
                }
            }
            if !writing && !carried && !signing_in && reader.background_wait(&screen.rows).is_none()
            {
                self.took(screen.seq, opts.map(|o| &o.policy), review);
                self.tell_face();
                self.wall_refusal_over();
            }
            if !writing {
                if !off {
                    self.host_steps_in_background(
                        &screen,
                        &mut background_since,
                        opts.map(|o| &o.policy),
                        review,
                    )?;
                }
                self.stuck_busy_note(&screen, background_since, review);
            }
            if !(writing && unsettled) {
                saw.busy = true;
                saw.live |= !writing && !signing_in;
                let try_in_flight = !writing && self.limit_retry_busy();
                if !signing_in {
                    if let Some(opts) = opts {
                        self.end_told_wait(opts);
                    }
                    self.running.busy(self.fx.mono());
                }
                if review.unattended() {
                    if !signing_in {
                        self.limit_on_busy(screen.seq, review)?;
                    }
                    if self.wall_memory.is_none() && !self.turn_end.compacting() && !signing_in {
                        self.exception_ended(screen.seq, Some("the agent working"), review);
                    }
                    if self.box_ask.is_some() && !writing && !try_in_flight {
                        self.close_box(screen.seq, review)?;
                    }
                    if !writing && !self.question.answered.is_empty() {
                        self.question_done(screen.seq, review)?;
                    }
                    if !writing && let Some(opts) = opts {
                        let live = !carried && reader.background_wait(&screen.rows).is_none();
                        self.reset_at_work(&screen, live, opts, review)?;
                    }
                }
                if !writing
                    && let Some(opts) = opts
                    && let Some(over) = self.memory_measure(opts)
                {
                    self.memory_warn(&screen, over, opts, review);
                }
            }
            if self.fx.mono() >= deadline {
                return Ok(Turn {
                    phase,
                    screen,
                    timed_out: true,
                });
            }
            if !writing
                && !off
                && let Some(duty) = duty.as_deref_mut()
                && (reader.survey(&screen.rows) || *duty.survey != Survey::Closed)
            {
                let turn = Turn {
                    phase,
                    screen,
                    timed_out: false,
                };
                let again = self.survey(&turn, duty.opts, duty.survey, true, review)?;
                let Turn {
                    phase: p,
                    screen: sc,
                    ..
                } = turn;
                phase = p;
                screen = sc;
                if again {
                    next = Next::Settle;
                    continue;
                }
            }
            if writing && unsettled {
                next = if footer_seen && matches!(next, Next::Gone(_)) {
                    Next::Settle
                } else {
                    Next::Idle
                };
                footer_seen = false;
                continue;
            }
            let (live, span) = live_rows(reader, &screen);
            if let Some(guard) = reader
                .busy_guard()
                .filter(|busy| !writing && self.busy_up(busy, &screen.rows[live..]))
            {
                next = Next::Gone(BusyWait {
                    guard,
                    span,
                    unscoped: false,
                    boxed: reader.busy_row_outlives_boxes(),
                });
                footer_seen = true;
                continue;
            }
            if self.stopped() {
                return Ok(self.spent_turn());
            }
            let seq = screen.seq.to_string();
            self.wait(&["seq", &seq], step)?;
            next = if writing || reader.composer(&screen.rows).is_none() {
                Next::Idle
            } else {
                Next::Settle
            };
            footer_seen = false;
        }
    }

    fn watch_context(
        &mut self,
        screen: &Screen,
        reader: &dyn ScreenReader,
        say: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        let warn = self.context_warn;
        if warn == 0 {
            return Ok(());
        }
        let rows = &screen.rows;
        let left = reader.context(rows);
        match self.context {
            Context::Armed => match left {
                Some(left) if left <= warn => {
                    self.context = Context::Warned { last: left };
                    say(&format!(
                        "EVENT context seq={} {left}% until auto-compact",
                        screen.seq
                    ))
                }
                _ => Ok(()),
            },
            Context::Warned { last } => {
                let compacted = match left {
                    Some(left) => left >= last.saturating_add(COMPACTED_RISE),
                    None => reader.composer(rows).is_some() && reader.prompt(rows).is_none(),
                };
                if compacted {
                    self.context = Context::Armed;
                    return say(&format!("EVENT compacted seq={}", screen.seq));
                }
                if let Some(left) = left {
                    self.context = Context::Warned { last: left };
                }
                Ok(())
            }
        }
    }

    /// Runs the supervisor loop until its first unhandled review point and returns the formatted phase.
    #[cfg(test)]
    pub(crate) fn supervise(&mut self, opts: &SuperviseOpts) -> Result<(String, u8), String> {
        self.supervise_to(opts, &mut std::io::stderr())
    }

    #[cfg(test)]
    fn supervise_to(
        &mut self,
        opts: &SuperviseOpts,
        log: &mut (dyn Write + Send),
    ) -> Result<(String, u8), String> {
        let allow = opts.allow();
        let journal = self
            .fx
            .open_journal(opts.journal.as_deref(), self.setup.sid.as_deref(), log);
        let sink = Mutex::new(Sink {
            out: log,
            journal,
            warn: None,
            fx: self.fx.clone(),
        });
        let end = self.drive(
            opts,
            &mut StopAtReview {
                sink: &sink,
                allow: &allow,
            },
        );
        match end {
            Ok(End::Stopped(turn)) => {
                let reader = self.reader(&turn.screen.rows);
                Ok((render_result(&turn, reader, &allow), 0))
            }
            Ok(End::Timeout(turn)) => {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .record("TIMEOUT");
                let reader = self.reader(&turn.screen.rows);
                Ok((
                    format!("TIMEOUT\n{}", render_result(&turn, reader, &allow)),
                    EXIT_TIMEOUT,
                ))
            }
            Ok(End::Quit) => Err("stopped".to_string()),
            Err(e) => {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .record(&format!("EXIT {}", exit_reason(&e)));
                Err(e)
            }
        }
    }

    /// Runs the continuous watching supervisor loop, writing event lines to `out`.
    #[cfg(test)]
    pub(crate) fn watch(&mut self, opts: &SuperviseOpts, out: &mut (dyn Write + Send)) -> u8 {
        self.watch_to(opts, out, &mut std::io::stderr())
    }

    #[cfg(test)]
    fn watch_to(
        &mut self,
        opts: &SuperviseOpts,
        out: &mut (dyn Write + Send),
        warn: &mut (dyn Write + Send),
    ) -> u8 {
        let allow = opts.allow();
        let journal =
            self.fx
                .open_journal(opts.journal.as_deref(), self.setup.sid.as_deref(), warn);
        let sink = Mutex::new(Sink {
            out,
            journal,
            warn: Some(warn),
            fx: self.fx.clone(),
        });
        let mut review = Lines {
            sink: &sink,
            allow: &allow,
        };
        let end = self.drive(opts, &mut review);
        if !session_gone(&end) {
            self.release_attention(&mut review);
        }
        let (line, code) = watch_end_line(end);
        let _ = sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .line(&line);
        code
    }

    #[cfg(test)]
    pub(super) fn seed_from_ledger(
        &mut self,
        path: &Path,
        now_ms: i64,
        lifted_off: bool,
    ) -> Option<approvals::OpenWall> {
        let body = self.fx.read_text(path).unwrap_or_default();
        self.seed_from_ledger_text(path, &body, now_ms, lifted_off)
    }

    pub(super) fn seed_from_ledger_text(
        &mut self,
        path: &Path,
        body: &str,
        now_ms: i64,
        lifted_off: bool,
    ) -> Option<approvals::OpenWall> {
        let fx = self.fx.clone();
        let read = |p: &Path| fx.read_text(p);
        let sid = self.setup.sid.clone();
        let earlier =
            crate::harness::upgrade_drive::earlier_ledgers_in(path, sid.as_deref(), &read);
        let ledgers: Vec<(String, String)> = earlier
            .iter()
            .filter_map(|(t, p)| Some((t.clone(), read(p)?)))
            .collect();
        self.first_books =
            !lifted_off && earlier.is_empty() && body.lines().all(|l| l.trim().is_empty());
        self.books_last_ms = std::iter::once(body)
            .chain(ledgers.iter().map(|(_, b)| b.as_str()))
            .filter_map(approvals::last_row_ms)
            .max();
        let own_journal = self.fx.read_text(&approvals::journal_beside(path));
        let mut left_keys = crate::harness::upgrade_drive::lefts_of(body, sid.as_deref(), &ledgers);
        let carried = sid.as_deref().filter(|s| {
            approvals::takes_up_earlier(body, Some(s))
                && ledgers.iter().any(|(t, b)| approvals::holds_rows_of(b, t))
        });
        let (books, journal) = match carried {
            None => (None, own_journal),
            Some(sid) => {
                let own = if approvals::took_up_earlier(body, sid) {
                    body.to_string()
                } else {
                    self.ledger_row(
                        approvals::RULE_EARLIER_BOOKS,
                        approvals::Outcome::Waiting,
                        "",
                        approvals::EARLIER_BOOKS,
                        0,
                    );
                    read(path).unwrap_or_else(|| body.to_string())
                };
                let journals: Vec<(String, String)> = earlier
                    .iter()
                    .filter_map(|(t, p)| Some((t.clone(), read(&approvals::journal_beside(p))?)))
                    .collect();
                let journal = approvals::books_through(
                    own_journal.as_deref().unwrap_or_default(),
                    Some(sid),
                    &journals,
                );
                (
                    Some(approvals::books_through(&own, Some(sid), &ledgers)),
                    Some(journal),
                )
            }
        };
        let body = books.as_deref().unwrap_or(body);
        self.seed_sleep(body, now_ms);
        let typed = approvals::typed_at_in(body, self.setup.sid.as_deref());
        let ages: Vec<Duration> = typed
            .iter()
            .filter_map(|&ts| u64::try_from(now_ms - ts).ok())
            .map(Duration::from_millis)
            .collect();
        self.turn_end.seed_acts(&ages, self.fx.mono());
        if let Some(ts) = approvals::login_typed_last_in(body, self.setup.sid.as_deref()) {
            let ago = Duration::from_millis(u64::try_from(now_ms - ts).unwrap_or(0));
            self.turn_end.seed_login(ago, self.fx.mono());
        }
        if approvals::carry_ended_in(body, self.setup.sid.as_deref()) {
            self.turn_end.seed_carry_over();
        }
        let streak = approvals::short_streak_in(body, self.setup.sid.as_deref());
        let since = streak
            .as_ref()
            .filter(|s| s.recorded && s.point)
            .map(|s| if s.ran { i64::MAX } else { s.at_ms });
        if let Some((words, noted_ms, by_wake)) = journal.as_deref().and_then(|journal| {
            ended_before(
                journal,
                self.setup.sid.as_deref(),
                typed.last().copied().max(since),
            )
        }) {
            self.turn_end.seed_landed(words);
            self.turn_end.seed_noted_at(noted_ms);
            if by_wake {
                self.turn_end.seed_landed_by_wake();
            }
        }
        if let Some(seed) = &streak {
            self.turn_end.seed_short(seed.short);
            if seed.point && !seed.ran {
                let ago = Duration::from_millis(u64::try_from(now_ms - seed.at_ms).unwrap_or(0));
                self.turn_end.seed_point(ago, self.fx.mono());
                if seed.esc {
                    self.turn_end.seed_esc();
                }
            } else if !seed.point && seed.recorded {
                self.turn_end.seed_yield(&seed.rule);
            }
        }
        let older = journal
            .as_deref()
            .filter(|_| !streak.as_ref().is_some_and(|s| s.recorded));
        let hold = older.and_then(|journal| {
            journaled_hold(journal, self.setup.sid.as_deref(), typed.last().copied())
        });
        let older_reads = older.map_or_else(OlderReads::default, |journal| OlderReads {
            takes: journaled_takes(journal, self.setup.sid.as_deref()),
            points: journaled_points(journal, self.setup.sid.as_deref()),
            judged_ms: hold.as_ref().filter(|h| !h.acted).map(|h| h.at_ms),
        });
        if let Some(hold) = &hold
            && let Some(short) = hold.short
        {
            self.turn_end.seed_short(short);
            if hold.acted {
                if let Some(seed) = &streak {
                    self.turn_end.seed_yield(&seed.rule);
                }
            } else {
                let point_ms = short_held(&hold.why)
                    .map_or(hold.at_ms, |(_, wait_ms)| hold.until_ms + 500 - wait_ms);
                let ago = Duration::from_millis(u64::try_from(now_ms - point_ms).unwrap_or(0));
                self.turn_end.seed_point(ago, self.fx.mono());
            }
        }
        let marked = match streak.as_ref().and_then(|s| s.retry) {
            Some(mark) => Some((mark, streak.as_ref().filter(|s| !s.point))),
            None => hold.as_ref().and_then(|h| {
                let mark = retry_held(h, &self.turn_end.timing)?;
                Some((mark, streak.as_ref().filter(|_| h.acted)))
            }),
        };
        if let Some((mark, last)) = marked {
            let ago = |ms: i64| {
                Duration::from_millis(u64::try_from(now_ms.saturating_sub(ms)).unwrap_or(0))
            };
            let retry = [RULE_API_RETRY, RULE_API_BACK, RULE_API_CUTOFF]
                .into_iter()
                .find(|r| last.is_some_and(|s| *r == s.rule));
            let awaited = retry.is_some_and(|rule| {
                last.is_some_and(|s| {
                    now_ms >= s.at_ms
                        && self.turn_end.seed_awaited(
                            rule,
                            ago(s.at_ms),
                            FirstRead::Unknown,
                            true,
                            false,
                            self.fx.mono(),
                        )
                })
            });
            self.turn_end.seed_retry(
                mark.kind,
                mark.attempts,
                mark.ups,
                ago(mark.since_ms),
                retry.is_some() && !awaited,
                self.fx.mono(),
            );
        }
        if let Some(key) = older.and_then(|j| journaled_left(j, self.setup.sid.as_deref()))
            && !left_keys.contains(&key)
        {
            left_keys.push(key);
        }
        self.left_keys = left_keys;
        seed_usage_act_in(
            &mut self.turn_end,
            body,
            self.setup.sid.as_deref(),
            &older_reads,
            now_ms,
            self.fx.mono(),
        );
        let parked = with_parks_journaled(body, journal.as_deref(), self.setup.sid.as_deref());
        seed_done_check(
            &mut self.turn_end,
            &parked,
            self.setup.sid.as_deref(),
            &older_reads,
            now_ms,
            self.fx.mono(),
        );
        if let Some(mut land) = approvals::land_episode_in(&parked, self.setup.sid.as_deref()) {
            // An older build's books: the points its journal says it judged
            // after Land's last act, as [`approvals::land_episode_in`] reads
            // a newer one's records ([`OlderReads::points`]).
            if let Some(last) = typed.last().filter(|_| land.landing && !land.cut) {
                let after: Vec<bool> = older_reads
                    .points
                    .iter()
                    .filter(|(t, _)| t > last)
                    .map(|(_, done)| *done)
                    .collect();
                if let Some(&first) = after.first() {
                    land.answered |= after.contains(&true);
                    land.landing = first && after.len() == 1;
                }
            }
            self.turn_end
                .seed_land(land.nudges, land.landing, land.cut, land.answered);
            self.turn_end.seed_land_hold(land.hold);
            if let Some(act) = land.act.filter(|_| land.landing && !land.cut) {
                let ago = Duration::from_millis(u64::try_from(now_ms - act.at_ms).unwrap_or(0));
                self.turn_end
                    .seed_land_act(act.said, act.lead, ago, self.fx.mono());
            }
            if let Some(rule) = land.owed {
                self.turn_end.seed_land_owed(rule);
            }
            if land.compact {
                self.turn_end.seed_compact_land();
            } else if land.judged {
                self.turn_end.seed_land_point();
            }
        }
        if let Some(seed) = streak.as_ref().filter(|s| s.point && s.ran) {
            self.turn_end.seed_ran();
            if let Some(ran_ms) = seed.ran_ms {
                let ago = |ms: i64| Duration::from_millis(u64::try_from(now_ms - ms).unwrap_or(0));
                self.turn_end
                    .seed_ran_turn(ago(ran_ms), ago(seed.at_ms), self.fx.mono());
            }
        }
        if lifted_off {
            self.turn_end.start_lifted_off();
            self.ledger_row(
                approvals::RULE_OFF_STOOD,
                approvals::Outcome::Waiting,
                "",
                approvals::OFF_STOOD,
                0,
            );
        }
        let open_wall = approvals::open_wall_in(body, self.setup.sid.as_deref());
        self.seed_model_switch_in(body, older);
        let moves = approvals::account_moves_in(body, self.setup.sid.as_deref());
        let ago = |ts: i64| u64::try_from(now_ms - ts).ok().map(Duration::from_millis);
        let made: Vec<Duration> = moves.made_ms.iter().filter_map(|&ts| ago(ts)).collect();
        if !made.is_empty() {
            self.turn_end.seed_account_moves(&made, self.fx.mono());
        }
        if let Some(unmade) = moves.unmade {
            self.turn_end.seed_account_unmade(unmade, moves.unmade_ms);
        }
        if let Some(open) = moves.open
            && let Some(ago) = ago(open.at_ms)
        {
            self.turn_end.seed_account(
                open.to,
                open.at,
                ago,
                open.asked,
                open.told,
                open.told_closes,
                self.fx.mono(),
            );
        }
        open_wall
    }

    fn drive(&mut self, opts: &SuperviseOpts, review: &mut dyn Review) -> Result<End, String> {
        // The window's own host, untaped, for an unwind to tell (none on a replay: its host is
        // the tape's).
        let live = opts.idle_host.clone().filter(|_| !self.fx.replaying());
        let mut guard = occupancy_loop::UntoldOnUnwind::over(self, live);
        guard.session().drive_to_end(opts, review)
    }

    /// [`Self::drive`]'s body, under its unwind's guard.
    fn drive_to_end(
        &mut self,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<End, String> {
        let hosted;
        let opts = match opts.idle_host.as_ref() {
            Some(host) if !self.fx.is_live() => {
                hosted = SuperviseOpts {
                    idle_host: Some(self.fx.host_door(host)),
                    ..opts.clone()
                };
                &hosted
            }
            _ => opts,
        };
        let end = self.drive_loop(opts, review);
        self.stuck_over(0, "the loop ended", review);
        self.exception_ended(0, Some("the loop ended"), review);
        if !session_gone(&end) {
            self.hold_left(Some(&opts.policy), review);
        }
        self.occupancy_untold();
        self.send_act_marks();
        end
    }

    pub(super) fn send_act_marks(&mut self) {
        for mark in std::mem::take(&mut self.act_marks.pending) {
            if self.act_marks.off {
                break;
            }
            let mut args = vec!["mark", "-"];
            args.extend(mark.split(' '));
            match self.call(&args) {
                Ok(reply) if act_mark_latches(&reply) => self.act_marks.off = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    fn drive_loop(&mut self, opts: &SuperviseOpts, review: &mut dyn Review) -> Result<End, String> {
        let start = self.fx.mono();
        let mut deadline = deadline_after(start, opts.max);
        let allow = opts.allow();
        self.stall_host.clone_from(&opts.idle_host);
        self.stall_grace_s = opts.policy.human_grace_s;
        self.stall_term_after_s = opts.policy.stall_term_after_s;
        self.stall_term = opts.policy.stall_term;
        self.resume_limits = opts.policy.resume_limits;
        self.context_warn = opts.context_warn;
        self.context = Context::Armed;
        if !self.turn_end_seeded
            && let Some(path) = self.setup.ledger_path.as_deref()
        {
            self.fx.bound(path);
        }
        self.ledger = self.fx.open_ledger(
            self.setup.ledger_path.as_deref(),
            self.setup.sid.as_deref(),
            &mut std::io::stderr(),
        );
        let mut open_wall = None;
        if !std::mem::replace(&mut self.turn_end_seeded, true)
            && let Some(path) = self.setup.ledger_path.clone()
        {
            let path = path.as_path();
            let body = self.fx.read_text(path).unwrap_or_default();
            let now_ms = self.fx.stamp_ms();
            let lifted_off = opts.idle_host.as_ref().is_some_and(|h| h.lifted_off());
            open_wall = self.seed_from_ledger_text(path, &body, now_ms, lifted_off);
            self.hold_seed_from(&body, start, now_ms, lifted_off, &opts.policy, review);
            if approvals::probe_owed(&body, self.setup.sid.as_deref()) {
                self.probe = UsageProbe::Owed {
                    own: false,
                    since: self.fx.mono(),
                };
            }
            if review.unattended()
                && let Some(open) = approvals::reset_owed(&body, self.setup.sid.as_deref())
            {
                self.reset_take_up(&open, review);
            }
        }
        let seeded_landing = self.turn_end.landed_seed_armed();
        if !seeded_landing {
            self.tell_start_face();
        }
        self.seed_wall_memory(open_wall);
        if opts.idle_host.is_some() {
            self.require_seeded_limit_conversation();
        }
        let mut state = Looking {
            approved: Vec::new(),
            gone_first: true,
            handed: None,
            handed_seq: None,
            handed_submit: None,
            moved: Saw::default(),
            survey: Survey::Closed,
            read_back: None,
            face_owed: false,
        };
        if review.unattended() {
            if std::mem::take(&mut self.claimed_at_naming) {
                // Taken before the first read: what it found is said here,
                // where the start's take said it.
                self.claim_said(review);
            } else {
                self.take_claim(review);
            }
            if let Some(held) = self.yield_to_holder(opts) {
                return Err(held);
            }
            match self.reconcile(review) {
                Ok(()) | Err(Fail::Lost(_)) => {}
                Err(Fail::Hard(e)) => return Err(e),
            }
        }
        if seeded_landing {
            if self.autopilot.answered() {
                let mode = self.autopilot(opts);
                self.turn_end.start_under(mode);
            }
            if self.autopilot.answered() || !review.unattended() {
                self.tell_start_face();
            } else {
                state.face_owed = true;
            }
        }
        loop {
            if self.stopped() {
                return Ok(End::Quit);
            }
            if let Some(held) = self.yield_to_holder(opts) {
                return Err(held);
            }
            let looked = self.look(opts, &allow, &mut state, &mut deadline, review);
            if looked.is_ok() {
                self.send_act_marks();
            }
            match looked {
                Ok(Some(end)) => return Ok(end),
                Ok(None) => self.outage = None,
                Err(Fail::Lost(why)) => {
                    match self.ride_out(why, deadline, &mut |line| review.say(line))? {
                        Rode::Stopped => return Ok(End::Quit),
                        Rode::Spent => return Ok(End::Timeout(self.spent_turn())),
                        Rode::Back { fresh: true } => state.handed = None,
                        Rode::Back { fresh: false } => {}
                    }
                    state.gone_first = false;
                }
                Err(Fail::Hard(e)) => return Err(e),
            }
        }
    }

    fn look(
        &mut self,
        opts: &SuperviseOpts,
        allow: &[String],
        state: &mut Looking,
        deadline: &mut Instant,
        review: &mut dyn Review,
    ) -> Result<Option<End>, Fail> {
        // THE WORD NO HOST HAS ANSWERED YET (the loop's start-up read was
        // lost: [`Self::drive`]) is read first, before anything of this look
        // is decided — the survey's duty and the Off park read it — and a
        // seeded landing still armed is decided under it, as the start would
        // have, and its face told. Lost again, it is this look's outage, and
        // the look after it reads again.
        if review.unattended() && !self.autopilot.answered() {
            self.autopilot.begin();
            let mode = self.autopilot_read(opts)?;
            if self.turn_end.landed_seed_armed() {
                self.turn_end.start_under(mode);
            }
            if std::mem::take(&mut state.face_owed) {
                self.tell_start_face();
            }
        }
        let remaining = deadline.saturating_duration_since(self.fx.mono());
        let turn = match state.read_back.take() {
            Some(screen) => self.turn_of(screen),
            None => {
                let off = review.unattended() && self.autopilot(opts) == Autopilot::Off;
                let mut duty = (opts.policy.dismiss_surveys && !off).then_some(SurveyDuty {
                    opts,
                    survey: &mut state.survey,
                });
                self.await_turn_from(
                    remaining,
                    state.gone_first,
                    &mut state.moved,
                    duty.as_mut(),
                    Some(opts),
                    review,
                )?
            }
        };
        self.claim_said(review);
        if self.stopped() {
            return Ok(Some(End::Quit));
        }
        let off = self.autopilot(opts) == Autopilot::Off;
        if review.unattended()
            && !turn.timed_out
            && self.fx.mono() < *deadline
            && self.usage_panel_look(&turn, state, *deadline, off, opts, review)?
        {
            state.gone_first = false;
            return Ok(None);
        }
        if review.unattended() && !turn.timed_out && self.fx.mono() < *deadline && off {
            self.park_while_off(turn.screen.seq, *deadline, opts, review)?;
            state.gone_first = false;
            return Ok(None);
        }
        if review.unattended()
            && !turn.timed_out
            && self.fx.mono() < *deadline
            && self.reset_look(&turn, state, *deadline, opts, review)?
        {
            state.gone_first = false;
            return Ok(None);
        }
        if std::mem::take(&mut self.rejudge) {
            state.handed = None;
        }
        self.held = false;
        let mut turn_key: Option<String> = None;
        let saw = std::mem::take(&mut state.moved);
        let worked = saw.busy;
        if turn.phase == Phase::Prompt {
            self.box_up(turn.screen.seq);
        }
        self.turn_live = saw.live;
        if worked {
            self.probe_said = None;
            let redrawn = !saw.live
                && turn.phase == Phase::Prompt
                && state.handed.as_deref()
                    == Some(
                        turn_key
                            .get_or_insert_with(|| self.review_key(&turn))
                            .as_str(),
                    );
            if !redrawn {
                state.handed = None;
            }
            if let Some(host) = opts.idle_host.as_ref() {
                host.turn_ran();
            }
            self.decline = approval_loop::DeclineMemory::default();
            self.question_done(turn.screen.seq, review)?;
        } else if !matches!(turn.phase, Phase::Prompt | Phase::Busy)
            && self
                .reader(&turn.screen.rows)
                .composer(&turn.screen.rows)
                .is_some()
        {
            // No read saw it busy, and yet the worker sits at its composer
            // with no dialog up: the dialog in hand is done, however it ended
            // (a dialog between its tabs, or at its review, is still a box:
            // `Phase::Prompt`). One the loop answered in a reply short enough
            // to start and end inside the settle after its Enter is told
            // here. One that went with no answer of the loop's — a person's
            // Esc after the loop's text — is forgotten here, its text among
            // it: remembered, the same question asked again would read as
            // the text "gone" from a new dialog's empty field, and be handed
            // over (F4). The composer itself is the evidence
            // (a box replaces it): a frame that reads neither a dialog nor
            // the composer — a dialog the reader misses, an exit to a shell —
            // keeps the memory, whose worst is a hand-over; forgotten there,
            // the loop's text in a dialog still up would read as a person's,
            // and their act or a key in flight would be lost.
            self.question_done(turn.screen.seq, review)?;
        }
        if worked {
            // A badge the turn-end policy raised with no point to carry it
            // (`/login` typed): the worker works again, so it is done.
            if std::mem::take(&mut self.turn_end_badge) {
                let said = self.unset_if_ours(|_| true)?;
                review.note(&format!(
                    "CLEARED seq={} {CLEARED_TURN_END} attention={said}",
                    turn.screen.seq
                ));
            }
        }
        if self
            .box_ask
            .as_ref()
            .is_some_and(|b| b.key != *turn_key.get_or_insert_with(|| self.review_key(&turn)))
        {
            let key = turn_key
                .get_or_insert_with(|| self.review_key(&turn))
                .clone();
            if !self.limit_badge_carry(&turn, &key) {
                self.close_box(turn.screen.seq, review)?;
            }
        }
        if turn.timed_out || self.fx.mono() >= *deadline {
            return Ok(Some(End::Timeout(turn)));
        }
        if review.unattended()
            && self.limit.is_some()
            && !matches!(turn.phase, Phase::Limited { .. })
            && (worked || turn.phase == Phase::Prompt)
        {
            let why = if worked {
                "answered after a busy spell"
            } else {
                "a box is up: it is working"
            };
            self.close_episode(turn.screen.seq, why, review)?;
        }
        if review.unattended() && !matches!(turn.phase, Phase::Limited { .. }) {
            self.retry = None;
            self.adopted_limit_over(turn.screen.seq, ANSWERED_PAST_LIMIT, review)?;
        }
        if review.unattended() && self.stall_look(turn.screen.seq, state, review)? {
            state.gone_first = false;
            return self.wait_for_next(turn, opts, allow, deadline, review);
        }
        if self.survey(&turn, opts, &mut state.survey, false, review)? {
            state.gone_first = false;
            return Ok(None);
        }
        let handed_back = std::mem::take(&mut self.question_handed_back);
        let seen = if !handed_back
            && state.handed.as_deref().is_some_and(|h| {
                h == turn_key
                    .get_or_insert_with(|| self.review_key(&turn))
                    .as_str()
            })
            && !submitted_over(state.handed_submit, &turn.screen)
        {
            // The point already handed over, still showing: the screen
            // moved (a footer tick, a banner) and nothing the point is made
            // of did. Nothing is pressed or reported, and what the decision
            // read of it is the new screen's too (the decision review's
            // RR-DEC-2: the host's gate read a repaint's raw wall, and the
            // quiet upgrade restarted the agent at a refused request).
            if let Some(old) = state.handed_seq.replace(turn.screen.seq) {
                self.carry_point(old, turn.screen.seq);
            }
            if review.unattended() && self.draft_look(&turn, opts, allow, review)? {
                state.gone_first = false;
                return Ok(None);
            }
            turn
        } else {
            let mut point =
                match self.auto_read(turn, opts, allow, &mut state.approved, *deadline, review)? {
                    Step::Again { settle } => {
                        state.handed = None;
                        state.gone_first = !settle;
                        return Ok(None);
                    }
                    Step::Decide(screen) => {
                        state.handed = None;
                        state.read_back = Some(screen);
                        state.gone_first = false;
                        return Ok(None);
                    }
                    Step::Review(point) => point,
                };
            if self.fx.mono() >= *deadline {
                return Ok(Some(End::Timeout(point)));
            }
            let busy_for = self.running.point(self.fx.mono());
            let turn_end = if review.unattended() {
                let action = self.turn_end_at_point(&point, busy_for, opts)?;
                let folded = self.task_edge_at_fold(&point.screen, opts, review);
                self.said_before_fail(&point, folded, review)?;
                action
            } else {
                TurnEndAction::Nothing
            };
            // The limit episode's badge carried to a point off its screen,
            // its episode ended by this point's fold: it goes now.
            if self.limit_badge_off_ended() {
                self.close_box(point.screen.seq, review)?;
            }
            if review.unattended()
                && self.limit.is_some()
                && self.turn_end.limit_ended()
                && !matches!(point.phase, Phase::Limited { .. })
            {
                let closed =
                    self.close_episode(point.screen.seq, "answered since the limit", review);
                self.said_before_fail(&point, closed, review)?;
            }
            if review.unattended() && self.autopilot(opts) == Autopilot::Off {
                self.park_while_off(point.screen.seq, *deadline, opts, review)?;
                state.gone_first = false;
                return Ok(None);
            }
            self.point_as_decided(&mut point);
            let point_escalated = review.unattended()
                && matches!(turn_end, TurnEndAction::Escalate { .. })
                && self.escalates_here(&point.phase);
            if review.unattended() {
                match (&point.phase, &turn_end) {
                    (Phase::Prompt, _) if !self.held => {
                        let raised = self.escalate_point(&point, None, review);
                        self.said_before_fail(&point, raised, review)?;
                    }
                    (_, TurnEndAction::Escalate { reason }) if point_escalated => {
                        let raised = self.escalate_point(&point, Some(reason), review);
                        self.said_before_fail(&point, raised, review)?;
                        if self.memory_wall {
                            self.mark_memory_badge();
                        }
                    }
                    _ => {}
                }
                // GEMINI CLI'S USAGE LIMIT THAT NAMES NO RESET, ITS DIALOG
                // ANSWERED `Stop` with no badge of the dialog's carried here
                // (no point decided it): raised now, while its resume is
                // typed on the ladder ([`Self::limit_stopped_badge`]).
                if !point_escalated && self.limit_retry.is_some() {
                    let key = self.review_key(&point);
                    if let Some(reason) = self.limit_stopped_badge(&point, &key) {
                        let raised = self.escalate_point(&point, Some(&reason), review);
                        self.said_before_fail(&point, raised, review)?;
                        self.limit_badge_raised(&key);
                    }
                }
            }
            if !self.say_point(&point, review)? {
                return Ok(Some(End::Stopped(point)));
            }
            state.approved.clear();
            state.handed = Some(self.review_key(&point));
            state.handed_seq = Some(point.screen.seq);
            state.handed_submit = submit_mark(&point.screen);
            if review.unattended() {
                let handled = point_escalated
                    || !matches!(
                        turn_end,
                        TurnEndAction::Nothing | TurnEndAction::Escalate { .. }
                    );
                self.limit_after(&point, deadline, handled, review)?;
            }
            if self.host_steps_here(&point, opts, &mut false, review)? {
                self.probe_shut(&point, ProbeGate::HostWants, None, review)?;
                state.gone_first = false;
                return Ok(None);
            }
            if review.unattended() {
                match self.usage_probe(&point, &turn_end, opts, review)? {
                    Probed::No => self.turn_end_execute(&point, turn_end, opts, allow, review)?,
                    Probed::Again => {}
                    Probed::Done => self.after_probe(&point, opts, allow, review)?,
                    Probed::Open => {
                        state.gone_first = false;
                        return Ok(None);
                    }
                }
            }
            point
        };
        if let Some(end) = self.wait_for_next(seen, opts, allow, deadline, review)? {
            return Ok(Some(end));
        }
        state.gone_first = false;
        Ok(None)
    }

    fn survey(
        &mut self,
        turn: &Turn,
        opts: &SuperviseOpts,
        survey: &mut Survey,
        busy: bool,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let reader = self.reader(&turn.screen.rows);
        let block = reader.survey_block(&turn.screen.rows);
        let open = block.is_some();
        let ctx = SurveyCtx {
            block: block.as_ref(),
            busy,
        };
        let left = std::mem::take(&mut self.survey_gone);
        let lone_zero = stray_digit(reader, &turn.screen, "0");
        if !lone_zero
            && turn.phase != Phase::Prompt
            && self
                .survey_zero
                .is_some_and(|at| self.fx.since(at) >= self.setup.survey_land)
        {
            self.survey_zero = None;
        }
        if let (Some(block), Some(pressed)) = (&block, self.survey_zero) {
            let wait = self
                .setup
                .survey_land
                .saturating_sub(self.fx.since(pressed));
            if !wait.is_zero() {
                self.survey_gone |= left;
                self.survey_pending(block, turn.screen.seq, wait)?;
                return Ok(true);
            }
        }
        if self.survey_zero.is_some()
            && lone_zero
            && !self.survey_person_holds(
                busy,
                turn.screen.seq,
                "a stray 0's backspace",
                opts,
                review,
            )?
        {
            match self.backspace() {
                Ok(Backspaced::Done) => {}
                Ok(Backspaced::Refused { halted, why }) => {
                    self.survey_gone |= left;
                    self.refused(halted, &why, turn.screen.seq, review)?;
                    return Ok(true);
                }
                Err(e) => {
                    self.survey_gone |= left;
                    return Err(e);
                }
            }
            self.survey_zero = None;
            if let Survey::Tried { tries, .. } = *survey {
                *survey = Survey::Closed;
                if open {
                    self.retry_survey(turn, &ctx, survey, review, tries)?;
                }
            }
            return Ok(true);
        }
        let composer = reader.composer(&turn.screen.rows);
        let draft = if turn.phase == Phase::Prompt {
            Some(false)
        } else {
            self.composer_draft(
                composer.as_ref(),
                reader.placeholder(),
                reader.placeholder_fg(),
                &turn.screen,
            )
        };
        let waits = turn.phase == Phase::Prompt || draft != Some(false);
        let mut tries = 0;
        match *survey {
            Survey::Tried { pressed, .. } if left => {
                self.survey_zero = None;
                let kind = self.survey_kind.take();
                match pressed {
                    Some(seq) if draft == Some(true) && heads_with_zero(composer.as_ref()) => {
                        review.note(&format!(
                            "ABANDONED seq={seq} the session survey left under typed text; a 0 of \
                             ours may head it"
                        ));
                    }
                    Some(seq) => {
                        review.say(&survey_dismissed_line(seq, kind))?;
                    }
                    None => {}
                }
                *survey = Survey::Closed;
                self.survey_ended(turn.screen.seq, review);
            }
            Survey::Tried { .. } if waits => return Ok(false),
            Survey::Tried { tries, .. } => {
                self.retry_survey(turn, &ctx, survey, review, tries)?;
                return Ok(false);
            }
            Survey::Retry { .. } if left => {
                *survey = Survey::Closed;
                self.survey_ended(turn.screen.seq, review);
            }
            Survey::Retry { at, .. } if self.fx.mono() < at => {
                // The idle look re-decides at `at`; a busy read has no point to look at
                // again, and its next read comes with the screen.
                if !busy {
                    self.look_at = Some(self.look_at.map_or(at, |t| t.min(at)));
                }
                return Ok(false);
            }
            Survey::Retry { .. } if busy => return Ok(false),
            Survey::Retry { tries: n, .. } => tries = n,
            Survey::Said if left => {
                *survey = Survey::Closed;
                self.survey_ended(turn.screen.seq, review);
            }
            Survey::Said | Survey::Closed => {}
        }
        if *survey == Survey::Said
            && open
            && !waits
            && !busy
            && let Some(why) = block.as_ref().and_then(aterm_phase::SurveyBlock::withheld)
        {
            // Said on a busy read (which badges nothing: the worker is at work), the card is
            // still open at the idle look: the badge goes there, once.
            self.tell_withheld_survey(turn, why, review)?;
        }
        if !open {
            self.survey_absent(review);
        }
        if !open || waits || *survey == Survey::Said {
            return Ok(false);
        }
        let Some(block) = block else {
            return Ok(false);
        };
        let withheld = block.withheld();
        if opts.policy.dismiss_surveys
            && self.claim.watching_behind().is_none()
            && withheld.is_none()
        {
            // Claude Code drops a digit sent within the first 600 ms of the card
            // ([`SURVEY_SETTLE`]): wait out what is left of it, on the screen's next
            // move, and look again from a fresh read.
            if let Some(wait) = self.survey_young() {
                self.wait(&["seq", &turn.screen.seq.to_string()], wait)?;
                return Ok(true);
            }
            if self.survey_person_holds(busy, turn.screen.seq, "the survey's 0", opts, review)? {
                return Ok(false);
            }
            self.survey_kind = Some(block.kind);
            match self.dismiss_survey(turn.screen.seq, &block.guard, turn.screen.human_seq)? {
                Dismiss::Pressed { seq } => {
                    *survey = Survey::Tried {
                        pressed: Some(seq),
                        tries,
                    };
                    self.survey_zero = Some(self.fx.mono());
                    return Ok(true);
                }
                Dismiss::Skipped => {
                    *survey = Survey::Tried {
                        pressed: None,
                        tries,
                    };
                    return Ok(true);
                }
                Dismiss::Yielded => {
                    self.survey_gone |= left;
                    return Ok(true);
                }
                Dismiss::Refused { halted, why } => {
                    self.survey_gone |= left;
                    self.refused(halted, &why, turn.screen.seq, review)?;
                    return Ok(true);
                }
            }
        }
        *survey = Survey::Said;
        review.say(&survey_event_line(
            turn.screen.seq,
            self.setup.sid.as_deref(),
            &block,
        ))?;
        if let Some(why) = withheld
            && !busy
        {
            self.tell_withheld_survey(turn, why, review)?;
        }
        Ok(false)
    }

    fn tell_withheld_survey(
        &mut self,
        turn: &Turn,
        why: &str,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let why = format!("a rating card aterm does not answer is open ({why})");
        self.survey_stuck(turn.screen.seq, StuckSurvey::Withheld, &why, review);
        Ok(())
    }

    fn survey_stuck(&mut self, seq: u64, kind: StuckSurvey, text: &str, review: &mut dyn Review) {
        if self.survey_told == Some(kind) || !review.unattended() {
            return;
        }
        review.note(&format!("WAITING seq={seq} {text}"));
        if let Some(host) = &self.stall_host {
            host.survey_stuck(Some((kind, text)));
        }
        self.survey_told = Some(kind);
    }

    fn survey_absent(&mut self, review: &mut dyn Review) {
        if self.survey_absent_told || self.survey_told.is_some() || !review.unattended() {
            return;
        }
        self.survey_absent_told = true;
        if let Some(host) = &self.stall_host {
            host.survey_stuck(None);
        }
    }

    fn survey_ended(&mut self, seq: u64, review: &mut dyn Review) {
        if self.survey_told.take().is_none() {
            return;
        }
        self.survey_absent_told = true;
        if let Some(host) = &self.stall_host {
            host.survey_stuck(None);
        }
        review.note(&format!("CLEARED seq={seq} survey"));
    }

    fn survey_pending(
        &mut self,
        block: &aterm_phase::SurveyBlock,
        seq: u64,
        wait: Duration,
    ) -> Result<(), Fail> {
        self.wait_row_gone(&block.guard, seq, wait)
    }

    pub(super) fn wait_row_gone(
        &mut self,
        guard: &str,
        seq: u64,
        wait: Duration,
    ) -> Result<(), Fail> {
        if guard.is_empty() {
            self.wait(&["seq", &seq.to_string()], wait)?;
        } else {
            self.await_gone(&["gone", guard], wait)?;
        }
        Ok(())
    }

    fn survey_young(&mut self) -> Option<Duration> {
        let first = *self.survey_first_seen.get_or_insert_with(|| self.fx.mono());
        let wait = self
            .setup
            .survey_settle
            .saturating_sub(self.fx.since(first));
        (!wait.is_zero()).then_some(wait)
    }

    fn survey_person_holds(
        &mut self,
        busy: bool,
        seq: u64,
        act: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if busy {
            self.survey_held_busy(seq, act, opts, review)
        } else {
            self.held_for_person(seq, act, true, approval_loop::DenyClock::None, opts, review)
        }
    }

    fn survey_held_busy(
        &mut self,
        seq: u64,
        act: &str,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if self.survey_hold.is_some_and(|until| until > self.fx.mono()) {
            return Ok(true);
        }
        self.program = self.foreground_program()?;
        let Some(until) = self.hands_off_until(opts) else {
            self.survey_hold = None;
            return Ok(false);
        };
        self.survey_hold = Some(until);
        review.note(&self.held_line(seq, until, act));
        Ok(true)
    }

    pub(super) fn held_line(&self, seq: u64, until: Instant, act: &str) -> String {
        let who = if self.driver {
            "another driver's hand is on the session"
        } else {
            "a person is typing"
        };
        format!(
            "HELD seq={seq} {who}: hands off until {} ({act})",
            utc_stamp(
                u64::try_from(self.now_unix()).unwrap_or(0)
                    + until.saturating_duration_since(self.fx.mono()).as_secs()
            )
        )
    }

    fn retry_survey(
        &mut self,
        turn: &Turn,
        ctx: &SurveyCtx<'_>,
        survey: &mut Survey,
        review: &mut dyn Review,
        failed: u32,
    ) -> Result<(), Fail> {
        let tries = failed.saturating_add(1);
        let pause = super::ladder::Ladder(&self.setup.survey_retry)
            .step(usize::try_from(tries - 1).unwrap_or(usize::MAX));
        let at = self.fx.mono() + pause;
        *survey = Survey::Retry { tries, at };
        if !ctx.busy {
            self.look_at = Some(self.look_at.map_or(at, |t| t.min(at)));
        }
        if tries == 1
            && let Some(block) = ctx.block
        {
            review.say(&survey_event_line(
                turn.screen.seq,
                self.setup.sid.as_deref(),
                block,
            ))?;
        }
        review.note(&format!(
            "WAITING seq={} the session survey did not take its 0 ({tries} tries): again in {} s",
            turn.screen.seq,
            pause.as_secs()
        ));
        if tries == SURVEY_BADGE_AT && !ctx.busy {
            let why = format!("the session survey did not close ({tries} tries); still trying");
            self.survey_stuck(turn.screen.seq, StuckSurvey::Unclosed, &why, review);
        }
        Ok(())
    }

    fn ride_out(
        &mut self,
        why: String,
        deadline: Instant,
        say: &mut dyn FnMut(&str) -> Result<(), String>,
    ) -> Result<Rode, String> {
        let now = self.fx.mono();
        if now >= deadline {
            return Ok(Rode::Spent);
        }
        if self.setup.reconnect.is_zero() {
            return Err(why);
        }
        let first_pause = self.setup.pause;
        let outage = self.outage.get_or_insert_with(|| Outage {
            since: now,
            kind: String::new(),
            told: false,
            back: false,
            pause: first_pause,
        });
        let since = outage.since;
        if !std::mem::replace(&mut outage.told, true) {
            say(&format!("RECONNECT {}", clip(&one_line(&why))))?;
        }
        let end = since
            .checked_add(self.setup.reconnect)
            .map_or(deadline, |end| end.min(deadline));
        self.caps = Caps::default();
        let mut last = why;
        loop {
            if self.stopped() {
                return Ok(Rode::Stopped);
            }
            let now = self.fx.mono();
            if now >= end {
                return if now >= deadline {
                    Ok(Rode::Spent)
                } else {
                    Err(format!("reconnect window lapsed: {last}"))
                };
            }
            let pause = self.outage.as_ref().map_or(first_pause, |o| o.pause);
            self.fx.sleep(pause.min(end - now));
            if let Some(o) = self.outage.as_mut() {
                o.pause = pause.saturating_mul(2).min(self.setup.pause_max);
            }
            self.probing = true;
            let probe = self.screen();
            self.probing = false;
            match probe {
                Ok(_) => {
                    let fresh = self
                        .outage
                        .as_mut()
                        .is_some_and(|o| !std::mem::replace(&mut o.back, true));
                    if fresh {
                        let ms = self.fx.since(since).as_millis();
                        say(&format!("RECONNECTED after {ms} ms"))?;
                    }
                    return Ok(Rode::Back { fresh });
                }
                Err(Fail::Lost(e)) => last = e,
                Err(Fail::Hard(e)) => return Err(e),
            }
        }
    }

    fn moved_past(
        &mut self,
        seq: u64,
        deadline: Instant,
        step_cap: Duration,
    ) -> Result<Past, Fail> {
        let step = deadline
            .saturating_duration_since(self.fx.mono())
            .min(step_cap);
        match self.wait(&["seq", &seq.to_string()], step)? {
            Wait::Latched => Ok(Past::Moved),
            Wait::TimedOut if self.fx.mono() >= deadline => Ok(Past::Still(None)),
            Wait::TimedOut => {
                let now = self.screen()?;
                Ok(if now.seq < seq {
                    Past::Moved
                } else {
                    Past::Still(Some(now))
                })
            }
            Wait::Unsupported => Err(Fail::Hard(
                "await seq is not known to this host".to_string(),
            )),
        }
    }
}

/// Test handle for advancing a session's [`super::fx::ScriptedClock`] mid-run.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct ClockJump(Arc<super::fx::ScriptedClock>);

#[cfg(test)]
impl ClockJump {
    /// Advances the scripted wall clock by `secs` to simulate a machine sleep.
    pub(crate) fn slept(&self, secs: i64) {
        self.0.slept(secs);
    }
}

impl<C: Ctl> Session<'_, C> {
    fn now_unix(&self) -> i64 {
        self.fx.wall_s()
    }

    fn now_unix_ms(&self) -> i64 {
        self.fx.wall_ms()
    }

    fn local_offset_s(&mut self) -> i64 {
        if let Some(o) = self.local_offset {
            return o;
        }
        let o = self.fx.tz_offset(self.local_offset_fixed);
        self.local_offset = Some(o);
        o
    }

    fn zone_lookup(&self) -> impl Fn(&str, i64) -> Option<i64> + '_ {
        let live = self.zone_offset;
        move |zone, unix| self.fx.zone_offset(live, zone, unix)
    }

    pub(super) fn local_offset_then(&self, unix: i64) -> Option<i64> {
        self.fx.local_offset_at(self.local_offset_at, unix)
    }

    fn reset_unix(&mut self, reset: Option<&str>) -> Option<i64> {
        let text = reset.unwrap_or("-");
        let placed = self
            .wall_reset
            .as_ref()
            .filter(|(key, _)| key.ends_with(&format!("|{text}")))
            .and_then(|_| self.wall_reset_unix());
        if placed.is_some() {
            return placed;
        }
        let text = reset?;
        let now = self.now_unix();
        let spec: ResetSpec = limit::parse_reset(text)?;
        let local = self.local_offset_s();
        let at = limit::reset_at_with(&spec, now, local, &self.zone_lookup());
        Some(self.transcript_day(&spec, at))
    }

    fn transcript_day(&self, spec: &ResetSpec, at: i64) -> i64 {
        if !matches!(spec, ResetSpec::At { date: None, .. }) {
            return at;
        }
        self.stall_host
            .as_ref()
            .and_then(|h| i64::try_from(h.limit_until()?).ok())
            .filter(|&named| {
                let before = at.saturating_sub(named);
                let days = before.saturating_add(43_200).div_euclid(86_400);
                days >= 1 && (before - days * 86_400).abs() < 60
            })
            .unwrap_or(at)
    }

    fn reset_unix_for(
        &mut self,
        reset: Option<&str>,
        session_window: bool,
        live: bool,
        answered: bool,
        rows: &[String],
    ) -> Option<i64> {
        let text = reset?;
        let now = self.now_unix();
        let spec: ResetSpec = limit::parse_reset(text)?;
        let local = self.local_offset_s();
        let at = if places_resets_on_print(self.reader(rows).program()) {
            let printed = limit::printed_by_the_screen(rows, now, local, live)
                .max(answered.then_some(now - limit::PRINT_LAG_S - limit::CLOCK_SLACK_S));
            let zone = &self.zone_lookup();
            limit::place_reset_with(&spec, session_window, now, printed, local, zone)
        } else {
            limit::reset_at_with(&spec, now, local, &self.zone_lookup())
        };
        Some(self.transcript_day(&spec, at))
    }

    fn refresh_reset(&mut self, reset: Option<&str>) {
        let text = reset.unwrap_or("-");
        if self.limit.as_ref().is_none_or(|ep| ep.reset_text == text) {
            return;
        }
        let at = self.reset_unix(reset);
        if let Some(ep) = self.limit.as_mut()
            && let Some(at) = at
            && ep.reset_unix.is_none_or(|old| at > old)
        {
            ep.reset_unix = Some(at);
            ep.reset_text = text.to_string();
            ep.extended = false;
        }
    }

    fn limit_after(
        &mut self,
        point: &Turn,
        deadline: &mut Instant,
        handled: bool,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        if let Phase::Limited { message, reset } = &point.phase {
            if let Some(raised) = self.limit.as_ref().map(|ep| ep.raised) {
                self.refresh_reset(reset.as_deref());
                if !handled && !raised {
                    let text = format!("{ATTENTION_PREFIX} {}", one_line(message));
                    let reset_unix = self.limit.as_ref().and_then(|ep| ep.reset_unix);
                    let waits = self.waits_out(message, reset_unix);
                    self.escalate(
                        point.screen.seq,
                        &text,
                        None,
                        escalate::Escalation::Limit { waits },
                        review,
                    )?;
                    if let Some(ep) = self.limit.as_mut() {
                        ep.raised = true;
                    }
                }
            } else {
                self.open_episode(point.screen.seq, message, reset.as_deref(), handled, review)?;
            }
            return self.extend(deadline, review);
        }
        Ok(())
    }

    fn open_episode(
        &mut self,
        seq: u64,
        message: &str,
        reset: Option<&str>,
        handled: bool,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let reset_text = reset.map_or_else(|| "-".to_string(), str::to_string);
        let reset_unix = self.reset_unix(reset);
        let retry = self.retry.take();
        let resumes_itself = limit::resumes_by_itself(message);
        if handled {
            review.note(&format!("LIMITED seq={seq} handled: {}", clip(message)));
        } else {
            let text = format!("{ATTENTION_PREFIX} {}", one_line(message));
            let waits = self.waits_out(message, reset_unix);
            self.escalate(
                seq,
                &text,
                retry.as_ref().map(|r| r.seq),
                escalate::Escalation::Limit { waits },
                review,
            )?;
        }
        self.limit = Some(Episode {
            seq: retry.map_or(seq, |r| r.seq),
            resumes_itself,
            reset_unix,
            reset_text,
            extended: false,
            raised: !handled,
        });
        if let Some(host) = &self.stall_host {
            host.limited(true);
        }
        Ok(())
    }

    pub(super) fn waits_out(&self, message: &str, reset_unix: Option<i64>) -> bool {
        let horizon = i64::try_from(RESET_HORIZON.as_secs()).unwrap_or(i64::MAX);
        limit::resumes_by_itself(message)
            || (self.resume_limits && reset_unix.is_some_and(|at| at - self.now_unix() <= horizon))
    }

    fn limit_on_busy(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        let Some(ep) = self.limit.as_ref().filter(|ep| ep.resumes_itself) else {
            return Ok(());
        };
        let retry = Retry { seq: ep.seq };
        self.close_episode(
            seq,
            "the worker works again: the notice said it would continue by itself",
            review,
        )?;
        self.retry = Some(retry);
        Ok(())
    }

    fn extend(&mut self, deadline: &mut Instant, review: &mut dyn Review) -> Result<(), Fail> {
        let now_unix = self.now_unix();
        let Some(ep) = self.limit.as_mut() else {
            return Ok(());
        };
        let Some(reset) = ep.reset_unix else {
            return Ok(());
        };
        if ep.extended {
            return Ok(());
        }
        ep.extended = true;
        if reset - now_unix > i64::try_from(RESET_HORIZON.as_secs()).unwrap_or(i64::MAX) {
            return Ok(());
        }
        let until_unix = reset + i64::try_from(RESET_GRACE.as_secs()).unwrap_or(0);
        let until =
            self.fx.mono() + Duration::from_secs(u64::try_from(until_unix - now_unix).unwrap_or(0));
        if until <= *deadline {
            return Ok(());
        }
        *deadline = until;
        let line = format!(
            "EXTEND until={} reset={}",
            utc_stamp(u64::try_from(until_unix).unwrap_or(0)),
            ep.reset_text
        );
        review.say(&line)?;
        Ok(())
    }

    fn wait_for_next(
        &mut self,
        mut seen: Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        deadline: &mut Instant,
        review: &mut dyn Review,
    ) -> Result<Option<End>, Fail> {
        let anchor = (seen.phase == Phase::Prompt)
            .then(|| box_anchor(self.reader(&seen.screen.rows), &seen.screen.rows))
            .flatten();
        self.stall_before_wait(seen.screen.seq, review)?;
        let held = self.question_held
            && aterm_phase::parse_prompt_v2(&seen.screen.rows)
                .is_some_and(|p| p.kind == aterm_phase::PromptKind::Question);
        let step_cap = if held {
            HANDBACK_POLL
        } else {
            self.setup.wait_step
        };
        self.watch_sleep();
        let mut seen_key: Option<String> = None;
        let mut verdict_seq = seen.screen.seq;
        let request = anchor
            .as_ref()
            .filter(|_| !held)
            .and_then(|_| self.file_request(&seen));
        let mut asked = false;
        // Every pass after the first is a wake: a step of the wait ran.
        let mut woke = false;
        loop {
            if self.stopped() {
                return Ok(None);
            }
            // THE CARET READ AGAIN AT EACH WAKE while it alone withholds the
            // host's point ([`Self::caret_withholds`]). Claude Code paints its
            // frame, then moves the terminal's cursor into the composer; a
            // read between the two finds the composer drawn and the cursor
            // under it, and the cursor's move bumps no content seq, so no wait
            // on the content wakes for it. Measured (host_live, 8 suites
            // beside 14 CPU hogs): a relaunched agent's first point read 34 ms
            // before its caret, its owed carry-on withheld `not-authoritative`
            // at every wake for good. Content that moved since ends the wait
            // (the loop looks again); the same content takes the fresh cursor.
            if std::mem::replace(&mut woke, true) && self.caret_withholds(&seen, opts) {
                let now = self.screen()?;
                if !seen.screen.take_cursor_of(&now) {
                    return Ok(None);
                }
            }
            self.check_slept(seen.screen.seq, review);
            self.dialog_note(&seen.screen, review);
            if self.host_steps_here(&seen, opts, &mut asked, review)? {
                return Ok(None);
            }
            if self.host_held
                && !opts
                    .idle_host
                    .as_ref()
                    .is_some_and(|h| h.owns_turn_end() || h.withholds_turn_end())
            {
                self.host_held = false;
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.reach_edge(opts) {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.memory_edge(&seen, opts, review)? {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.account_edge(opts) {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            // The usage tap moved the wall's reset: decided again now, not at the back-off's end.
            if self.tap_edge(opts, seen.screen.seq) {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.look_at.is_some_and(|at| self.fx.mono() >= at) {
                self.look_at = None;
                return Ok(None);
            }
            self.tell_wait(opts);
            let due = self.due_at().into_iter().chain(self.look_at).min();
            let until = due.map_or(*deadline, |t| t.min(*deadline));
            let now = self.fx.mono();
            let step_cap = opts
                .idle_host
                .as_ref()
                .and_then(|h| h.wants_at())
                .filter(|at| *at > now)
                .map_or(step_cap, |at| step_cap.min(at - now));
            let step_cap = self.settling().map_or(step_cap, |at| {
                step_cap.min(at.saturating_duration_since(now))
            });
            if now < until {
                let step = until.saturating_duration_since(now).min(step_cap);
                self.host_passed = None;
                self.host_follows = None;
                if let Some(anchor) = anchor.as_deref() {
                    if self.await_gone(&["gone", anchor], step)? {
                        return Ok(None);
                    }
                    if let Some(request) = request.as_deref() {
                        let screen = self.screen()?;
                        let now = self.turn_of(screen);
                        if self.file_request(&now).as_deref() != Some(request) {
                            return Ok(None);
                        }
                    }
                } else if let Some(words) = self.agent_wake(&seen, &mut seen_key) {
                    let recheck = step >= AGENT_RECHECK.min(self.setup.wait_step);
                    match self.wait(&["agent", words], step.min(AGENT_RECHECK))? {
                        Wait::Latched => {
                            self.agent_woke =
                                Some(seen_key.take().unwrap_or_else(|| self.review_key(&seen)));
                            return Ok(None);
                        }
                        Wait::TimedOut => {
                            if recheck
                                && self.moved_under_verdict(
                                    &seen,
                                    &mut seen_key,
                                    &mut verdict_seq,
                                )?
                            {
                                return Ok(None);
                            }
                        }
                        Wait::Unsupported => self.caps.await_agent = Some(false),
                    }
                } else if let Past::Moved = self.moved_past(seen.screen.seq, until, step_cap)? {
                    return Ok(None);
                }
                if held && self.question_policy_answers(opts)? {
                    self.question_held = false;
                    self.question_handed_back = true;
                    return Ok(None);
                }
                if self.stall_step(seen.screen.seq, review)? {
                    return Ok(None);
                }
                if self.fx.mono() < until || self.stalled.is_some() {
                    continue;
                }
            }
            if self.fx.mono() >= *deadline {
                return Ok(Some(End::Timeout(seen)));
            }
            if self.look_at.is_some_and(|at| self.fx.mono() >= at) {
                self.look_at = None;
                return Ok(None);
            }
            self.turn_end_now(&seen, opts, allow, review)?;
            return Ok(None);
        }
    }

    fn tell_wait(&mut self, opts: &SuperviseOpts) {
        #[cfg(test)]
        if TELL_EVERY_STEP.with(std::cell::Cell::get) {
            self.wait_told = None;
        }
        let due = self.turn_end_due.as_ref().filter(|d| d.wait.told());
        let unix = |d: &Due| turn_end_loop::nearest_s(d.unix_ms, 0);
        let told = match (self.wait_told, due) {
            (Some(None), None) => return,
            (Some(Some(t)), Some(d))
                if t.at == d.at && t.kind == d.wait && unix(d).abs_diff(t.unix) <= WAIT_SKEW_S =>
            {
                return;
            }
            _ => due.map(|d| ToldWait {
                at: d.at,
                unix: unix(d),
                kind: d.wait,
            }),
        };
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        let plan = due.zip(told).map(|(d, t)| WaitPlan {
            at_unix: t.unix,
            why: d.why.clone(),
            kind: t.kind,
        });
        self.wait_told = Some(told);
        host.waiting(plan);
    }

    fn watch_sleep(&mut self) {
        let (mono_now, wall_now) = (self.fx.mono(), self.now_unix_ms());
        let Some((mono_then, wall_then)) = self.clock_marks.replace((mono_now, wall_now)) else {
            self.look_at_sleep(wall_now);
            return;
        };
        let mono_ms =
            i64::try_from(mono_now.duration_since(mono_then).as_millis()).unwrap_or(i64::MAX);
        let wall_ms = wall_now.saturating_sub(wall_then);
        if !slept_through(wall_ms.div_euclid(1000), mono_ms.div_euclid(1000)) {
            return;
        }
        let ahead =
            Duration::from_millis(u64::try_from(wall_ms.saturating_sub(mono_ms)).unwrap_or(0));
        let by = self
            .recount_sleep()
            .map_or(ahead, |counted| counted.min(ahead));
        let secs = i64::try_from(by.as_secs()).unwrap_or(i64::MAX);
        if !slept_through(secs, 0) {
            return;
        }
        self.sleeps.add(by, wall_then, wall_now);
        self.turn_end.slept(by);
        let woke_at = wall_now.div_euclid(1000);
        self.settle_wake(woke_at, mono_now);
        self.slept_at = Some(woke_at);
    }

    fn recount_sleep(&mut self) -> Option<Duration> {
        let now = self.fx.asleep();
        let counted = self
            .sleep_count
            .as_ref()
            .zip(now.as_ref())
            .map(|(then, now)| now.asleep.saturating_sub(then.asleep));
        self.sleep_count = now;
        counted
    }

    fn look_at_sleep(&mut self, now_ms: i64) {
        if self.sleep_look.is_some() {
            return;
        }
        let count = self.fx.asleep();
        let last = self.fx.last_sleep();
        let reach = turn_end_loop::WAKE_SETTLE + turn_end_loop::WAKE_SPREAD;
        if self.sleeps.place_last(now_ms, last)
            && let Some((_, woke_ms)) = last
            && Duration::from_millis(u64::try_from(now_ms - woke_ms).unwrap_or(0)) < reach
        {
            let woke = self.instant_of_ms(woke_ms);
            self.settle_wake(woke_ms.div_euclid(1000), woke);
            let _ = self.settling();
        }
        self.sleep_count.clone_from(&count);
        self.sleep_look = Some(FirstLook {
            at_ms: now_ms,
            count,
            last,
        });
    }

    fn seed_sleep(&mut self, body: &str, now_ms: i64) {
        self.look_at_sleep(now_ms);
        let Some(FirstLook {
            at_ms: at,
            count,
            last,
        }) = self.sleep_look.clone()
        else {
            return;
        };
        let act = approvals::last_act_count_in(body, self.setup.sid.as_deref());
        let since = self.sleeps.count_since_act(
            at,
            count.as_ref(),
            act.as_ref().map(|(ts, c)| (*ts, c.as_ref())),
            last,
        );
        self.sleep_unknown = since.unknown();
    }

    fn say_sleep_unknown(&mut self, seq: u64, review: &mut dyn Review) {
        if let Some(why) = self.sleep_unsaid.take() {
            review.note(&format!(
                "SLEEP seq={seq} not counted before this loop's first look ({why}): the turn's \
                 done row is read whole of it, but for the system's record of its last sleep"
            ));
        }
    }

    fn policy_now(&mut self) -> Instant {
        self.watch_sleep();
        self.fx.mono()
    }

    fn check_slept(&mut self, seq: u64, review: &mut dyn Review) {
        self.watch_sleep();
        if let Some(wall_now) = self.slept_at.take() {
            self.woke(seq, wall_now, review);
        }
    }

    pub(super) fn end_told_wait(&mut self, opts: &SuperviseOpts) {
        if !matches!(self.wait_told, Some(Some(_))) {
            return;
        }
        self.wait_told = Some(None);
        if let Some(host) = opts.idle_host.as_ref() {
            host.waiting(None);
        }
    }

    fn reach_edge(&mut self, opts: &SuperviseOpts) -> bool {
        let Some(seen) = self.reach_seen else {
            return false;
        };
        if self.turn_end_due.is_none() {
            return false;
        }
        let Some(host) = opts.idle_host.as_ref() else {
            return false;
        };
        let now = host.reach();
        if now == seen {
            return false;
        }
        self.reach_seen = Some(now);
        true
    }

    pub(super) fn account_edge(&mut self, opts: &SuperviseOpts) -> bool {
        if opts.policy.accounts == crate::harness::accounts::Mode::Off && !opts.policy.logins {
            return false;
        }
        if self.renewal_made(opts) {
            return true;
        }
        let latched = self.turn_end.account_refused();
        let stands =
            self.turn_end_due.is_some() || (self.nothing_decided && self.roster_seen.is_some());
        if (self.account_seen.is_none() && self.roster_seen.is_none() && !latched) || !stands {
            return false;
        }
        let Some(host) = opts.idle_host.as_ref() else {
            return false;
        };
        if latched
            && self
                .account_news_at
                .is_some_and(|at| self.fx.since(at) < ACCOUNT_NEWS_EVERY)
        {
            return false;
        }
        if !self.account_news(host.as_ref()) {
            return false;
        }
        if latched {
            self.account_news_at = Some(self.fx.mono());
        }
        true
    }

    fn renewal_made(&mut self, opts: &SuperviseOpts) -> bool {
        let (Some(seen), Some(host)) = (self.login_seen, opts.idle_host.as_ref()) else {
            return false;
        };
        let stamp = host.account_stamp();
        if stamp == seen {
            return false;
        }
        if host.login_superseded() {
            self.login_seen = None;
            return true;
        }
        self.login_seen = Some(stamp);
        false
    }

    fn memory_edge(
        &mut self,
        seen: &Turn,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let over = self.memory_measure(opts);
        if let Some(over) = over {
            self.memory_warn(&seen.screen, over, opts, review);
        }
        if self.beside_self_claim() {
            self.beside_memory_remedy(over, seen.screen.seq, opts, review)?;
            return Ok(false);
        }
        let at_point = matches!(seen.phase, Phase::Idle | Phase::Limited { .. });
        if over.is_some_and(|o| o.acts()) {
            // An idle point — a wall's own among them — and a limit notice
            // (waited out for hours, or days): the reading takes the measure
            // as the memory wall there ([`Self::turn_end_reading`]).
            return Ok(at_point && self.memory_seen.is_none());
        }
        if !at_point {
            return Ok(false);
        }
        let wall = std::mem::take(&mut self.memory_over_wall);
        let key = self.review_key(seen);
        let badge = self
            .box_ask
            .as_ref()
            .is_some_and(|b| b.memory && b.key == key);
        if badge {
            self.box_ask = None;
            self.adopted_box = false;
            let said = self.unset_if_ours(|t| !t.starts_with(ATTENTION_PREFIX))?;
            review.note(&format!(
                "CLEARED seq={} {CLEARED_MEMORY} attention={said}: the measure is no longer \
                 acted on",
                seen.screen.seq
            ));
        }
        Ok(wall || badge)
    }

    pub(super) const fn mark_memory_badge(&mut self) {
        if let Some(b) = self.box_ask.as_mut() {
            b.memory = true;
        }
    }

    fn memory_measure(&mut self, opts: &SuperviseOpts) -> Option<super::memory::MemoryOver> {
        let cfg = opts.policy.memory_ceiling;
        let m = opts.idle_host.as_ref().and_then(|h| h.memory());
        if super::memory::episode_over(cfg, m.as_ref()) {
            self.memory_told = None;
        }
        m.and_then(|m| super::memory::judge(cfg, &m))
    }

    fn memory_warn(
        &mut self,
        screen: &Screen,
        over: super::memory::MemoryOver,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) {
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        let makes = if self.beside_self_claim() {
            host.relaunches_after_stall()
        } else {
            host.can_restart()
        };
        let restarts = makes && opts.policy.relaunch && !self.memory_refused;
        self.memory_say(screen, over, restarts, opts, review);
    }

    fn memory_say(
        &mut self,
        screen: &Screen,
        over: super::memory::MemoryOver,
        restarts: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) {
        let Some(host) = opts.idle_host.as_ref() else {
            return;
        };
        let verdict = over.verdict(restarts);
        if self.memory_told == Some(verdict) {
            return;
        }
        self.memory_told = Some(verdict);
        let agent = match self.reader(&screen.rows).program() {
            p @ (Program::Claude
            | Program::Codex
            | Program::Agy
            | Program::Muse
            | Program::Gemini) => p.product(),
            Program::Generic => "The agent",
        };
        let resume = host.resume_command();
        let text = over.warning(agent, restarts, resume.as_deref());
        review.note(&format!("MEMORY seq={} {text}", screen.seq));
        host.warn(&text);
    }

    fn moved_under_verdict(
        &mut self,
        seen: &Turn,
        seen_key: &mut Option<String>,
        seq: &mut u64,
    ) -> Result<bool, Fail> {
        if let Some(last) = self.last.clone().filter(|l| l.seq > *seq) {
            *seq = last.seq;
            let then = self.turn_of(last);
            let key = seen_key.get_or_insert_with(|| self.review_key(seen));
            if point_key(self.reader(&then.screen.rows), &then) != *key
                || submitted_over(submit_mark(&seen.screen), &then.screen)
            {
                return Ok(true);
            }
        }
        match self.wait(&["seq", &seq.to_string()], Duration::ZERO)? {
            Wait::Latched => {}
            Wait::TimedOut => return Ok(false),
            Wait::Unsupported => {
                return Err(Fail::Hard(
                    "await seq is not known to this host".to_string(),
                ));
            }
        }
        let screen = self.screen()?;
        *seq = screen.seq;
        let now = self.turn_of(screen);
        let key = seen_key.get_or_insert_with(|| self.review_key(seen));
        Ok(point_key(self.reader(&now.screen.rows), &now) != *key
            || submitted_over(submit_mark(&seen.screen), &now.screen))
    }

    fn agent_wake(&self, seen: &Turn, seen_key: &mut Option<String>) -> Option<&'static str> {
        if self.caps.await_agent != Some(true)
            || self.turn_end.act_in_flight()
            || self.turn_end.waits_on_named_reset()
            || self.draft_held.is_some()
        {
            return None;
        }
        if self.footer.is_none()
            && !self.busy_read
            && match self.reader(&seen.screen.rows).program() {
                Program::Claude | Program::Generic => true,
                Program::Codex | Program::Agy | Program::Muse | Program::Gemini => false,
            }
        {
            return None;
        }
        let words = match seen.phase {
            Phase::Idle => AGENT_WORDS_BUT_IDLE,
            Phase::Question => AGENT_WORDS_BUT_QUESTION,
            _ => return None,
        };
        let reading = aterm_phase::read(self.program.as_deref(), &seen.screen.rows, None);
        if reading.wall.is_some()
            || !reading.phase_authoritative
            || reading.phase != seen.phase
            || self.agent_woke.as_deref().is_some_and(|w| {
                w == seen_key
                    .get_or_insert_with(|| self.review_key(seen))
                    .as_str()
            })
        {
            return None;
        }
        Some(words)
    }

    /// A BUSY READ OF THE AGENT'S OWN BACKGROUND WORK: the break offered to the host's step,
    /// or withheld, and the hold's clock and the occupancy door fed. Under `prompt` alone the
    /// door's write is the break's last step (§2.1: the brakes, then the host's line, then the
    /// door), and only its reads and its write can fail here.
    fn host_steps_in_background(
        &mut self,
        screen: &Screen,
        since: &mut Option<Instant>,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let reader = self.reader(&screen.rows);
        let carried = self.transcript_wait_seq == Some(screen.seq);
        let unfit = if carried {
            None
        } else if reader.phase(&screen.rows) != Phase::Busy {
            Some(Guard::NotBusy)
        } else if reader.background_wait(&screen.rows).is_none() {
            Some(Guard::NoBackgroundWait)
        } else {
            None
        };
        let wall = if unfit.is_none() {
            reader.wall(&screen.rows)
        } else {
            None
        };
        let notice = wall
            .as_ref()
            .filter(|w| w.kind.reads_limited())
            .map(|w| w.reset.as_deref());
        let unfit = unfit.or_else(|| {
            wall.as_ref()
                .is_some_and(|w| {
                    !(w.kind.reads_limited()
                        && self.limit_spent(self.stall_host.as_deref(), notice))
                })
                .then_some(Guard::Wall)
        });
        let host = self.stall_host.clone();
        if let Some(guard) = unfit {
            *since = None;
            self.withhold(host.as_ref(), guard);
            let hold = (guard == Guard::Wall).then_some(HoldSaid::Held(guard));
            self.say_hold(screen.seq, hold, review);
            return Ok(());
        }
        // A break read after the door's line was taken: its answer ended here.
        self.turn_end.fact_answered();
        let at = *since.get_or_insert_with(|| self.fx.mono());
        let Some(host) = host.filter(|h| h.wants()) else {
            self.withheld = None;
            let waited = self.fx.since(at);
            let settled = waited >= self.setup.background_settle;
            if settled {
                self.say_hold(screen.seq, Some(HoldSaid::NoHostStep), review);
            }
            self.hold_background(screen, at, waited, settled, cfg, review);
            if self.occupancy_write(screen, None, cfg, review)? {
                *since = None;
            }
            return Ok(());
        };
        let waited = self.fx.since(at);
        let settled = waited >= self.setup.background_settle;
        self.hold_background(screen, at, waited, settled, cfg, review);
        let held = if !settled {
            Some(Guard::Settle)
        } else if self.limit.is_some() && !self.limit_spent(Some(host.as_ref()), notice)
            || self.policy_limit_holds()
            || self.limit_retry_is_away()
        {
            Some(Guard::LimitEpisode)
        } else if self.stalled.is_some() {
            Some(Guard::Stall)
        } else if self.turn_end.act_untaken() {
            Some(Guard::ActUntaken)
        } else if self.settling().is_some() {
            // A busy read's request is what sees the sleep here ([`Self::call`]).
            Some(Guard::WakeSettle)
        } else if cfg.is_some_and(|cfg| self.credits_hold_here(cfg, &screen.rows)) {
            // Nor onto usage credits the key keeps the session off.
            Some(Guard::Credits)
        } else {
            None
        };
        if let Some(guard) = held {
            self.withhold(Some(&host), guard);
            if guard != Guard::Settle {
                self.say_hold(screen.seq, Some(HoldSaid::Held(guard)), review);
            }
            self.occupancy_write(screen, Some(guard), cfg, review)?;
            return Ok(());
        }
        self.withheld = None;
        self.hold_said = None;
        let mut line_gone = None;
        if let Some((line, kind)) = host.at_background() {
            review.note(&format!("HOST seq={} background {line}", screen.seq));
            self.host_turn(kind, screen.seq);
            self.hold_line(screen.seq, cfg, review);
            *since = None;
            // The break's one line went: the host's (§2.1, the upgrade's notice first).
            line_gone = Some(Guard::HostLine);
        }
        if self.occupancy_write(screen, line_gone, cfg, review)? {
            *since = None;
        }
        Ok(())
    }

    fn host_steps_here(
        &mut self,
        point: &Turn,
        opts: &SuperviseOpts,
        asked: &mut bool,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let Some(host) = opts.idle_host.as_ref().filter(|h| h.wants()) else {
            return Ok(false);
        };
        self.program = self.foreground_program()?;
        let mut reading = aterm_phase::read_at(
            self.program.as_deref(),
            &point.screen.rows,
            point.screen.cursor_index(),
            None,
        );
        if reading.phase == Phase::Busy && point.phase == Phase::Idle {
            let reader = self.reader(&point.screen.rows);
            if self.transcript_wait_over(reader, &point.screen) {
                reading.phase = Phase::Idle;
                let read = aterm_phase::identify(self.program.as_deref(), &point.screen.rows);
                reading.phase_authoritative = read
                    .phase_authoritative(&point.screen.rows, &Phase::Idle)
                    && read.cursor_vouches(
                        &point.screen.rows,
                        &reading,
                        point.screen.cursor_index(),
                    );
            }
        }
        self.wall_as_decided(&mut reading, &point.screen);
        let limit_wall = matches!(&reading.wall, Some(w) if w.kind.reads_limited());
        let notice = match (&point.phase, &reading.phase, &reading.wall) {
            (Phase::Limited { reset, .. }, _, _) | (_, Phase::Limited { reset, .. }, _) => {
                Some(reset.as_deref())
            }
            (_, _, Some(w)) if w.kind.reads_limited() => Some(w.reset.as_deref()),
            _ => None,
        };
        let spent = (limit_wall || self.limit.is_some())
            && (self.turn_end.account_resume_owed()
                || self.limit_spent(Some(host.as_ref()), notice));
        let rests = |p: &Phase| *p == Phase::Idle || (spent && matches!(p, Phase::Limited { .. }));
        let guard = if !rests(&point.phase) || !rests(&reading.phase) {
            Some(Guard::NotIdle)
        } else if !reading.phase_authoritative {
            Some(Guard::NotAuthoritative)
        } else if reading.wall.is_some() && !(spent && limit_wall) {
            Some(Guard::Wall)
        } else if reading.wall.is_none() && self.turn_end.holds_left_retry() {
            // A retry's wall whose notice left the screen with no work since
            // (a person's `/status`, a scroll): the policy holds its rung
            // there, so the host's turn would meet the outage too (RR-DEC-3).
            Some(Guard::Wall)
        } else if reading.prompt.is_some() {
            Some(Guard::Prompt)
        } else if self.limit.is_some() && !spent {
            Some(Guard::LimitEpisode)
        } else if self.policy_limit_holds() || self.limit_retry_is_away() {
            // A limit the turn-end policy still waits on, met under a
            // screen-alone agent: through its relaunch, where the episode
            // closed ([`TurnEndState::limit_holds_host`]); or Gemini CLI's
            // dialog held when the agent left the tab, before its relaunch's
            // start-up is read as that limit ([`Self::limit_retry_away`]).
            Some(Guard::LimitEpisode)
        } else if self.turn_end.act_in_flight() {
            Some(Guard::ActInFlight)
        } else if self.settling().is_some() {
            // The wait offers the point again once the settle falls ([`Self::wait_for_next`]).
            Some(Guard::WakeSettle)
        } else if self.credits_hold_here(&opts.policy, &point.screen.rows) {
            // Usage credits carry the session, and the key keeps it on its
            // subscription: the step's turn would be billed to them.
            Some(Guard::Credits)
        } else if self.limit.is_none() && !limit_wall && self.standing_holds(host.as_ref()) {
            // A LIMIT ONLY THE TRANSCRIPT SHOWS, asked last, of a step about
            // to be taken: the host's step goes before the decision that
            // would read it as the wall ([`Self::read_standing_limit`]), and
            // a relaunch's carry-on was typed into a weekly limit the
            // resumed screen no longer drew (KI-1015-1 LIM-1). The host
            // names none the harness moved the conversation off.
            Some(Guard::LimitEpisode)
        } else {
            None
        };
        let idle = guard.is_none();
        match guard {
            Some(guard) => self.withhold(Some(host), guard),
            None => self.withheld = None,
        }
        let rests_here = |p: &Phase| matches!(p, Phase::Idle | Phase::Limited { .. });
        if !idle
            && rests_here(&point.phase)
            && rests_here(&reading.phase)
            && reading.phase_authoritative
            && let Some(line) = host.at_withheld_point(std::mem::replace(asked, true))
        {
            review.note(&format!("HOST seq={} {line}", point.screen.seq));
        }
        if idle {
            // The point its last step passed over while it still wanted one
            // is offered again only after a step of the wait
            // ([`Self::host_passed`]): never at once.
            if self.host_passed.is_some() && self.host_passed == Some(self.review_key(point)) {
                return Ok(false);
            }
            let at = IdlePoint {
                done_under_land: self.autopilot(opts) == Autopilot::Land && self.turn_end.landed(),
            };
            let step = host.at_idle_on(at);
            if let Some(step) = &step {
                if !step.repeat {
                    review.note(&format!("HOST seq={} {}", point.screen.seq, step.line));
                }
                if step.moved {
                    self.turn_end.host_moved();
                }
                if let Some(kind) = step.typed {
                    self.host_turn(kind, point.screen.seq);
                }
                if step.typed.is_some() || step.moved {
                    self.hold_point(&point.screen, true, Some(&opts.policy), review);
                }
            }
            let stood = !step.as_ref().is_some_and(|s| s.moved);
            self.host_held = stood;
            let key = host.wants().then(|| self.review_key(point));
            let (had, moved_before) = self
                .host_follows
                .as_ref()
                .filter(|(at, ..)| key.as_ref() == Some(at))
                .map_or((0, false), |(_, n, moved)| (*n, *moved));
            let typed = step.as_ref().is_some_and(|s| s.typed.is_some());
            self.host_follows = if stood {
                key.clone()
                    .filter(|_| step.is_some() && !host.owns_turn_end() && had < HOST_FOLLOW_UPS)
                    .map(|key| (key, had + 1, false))
            } else if moved_before {
                key.clone()
                    .filter(|_| !typed && had < HOST_FOLLOW_UPS)
                    .map(|key| (key, had + 1, true))
            } else {
                key.clone().filter(|_| !typed).map(|key| (key, had, true))
            };
            self.host_passed = key.filter(|_| (stood || !typed) && self.host_follows.is_none());
        }
        Ok(idle)
    }

    /// [`TurnEndState::limit_holds_host`] now, with the grace a limit's notice
    /// gets past its reset ([`crate::harness::upgrade_drive::LIMIT_GRACE_S`]).
    fn policy_limit_holds(&self) -> bool {
        let grace = Duration::from_secs(crate::harness::upgrade_drive::LIMIT_GRACE_S);
        self.turn_end.limit_holds_host(self.fx.mono(), grace)
    }

    fn standing_holds(&self, host: &dyn IdleHost) -> bool {
        let grace = crate::harness::upgrade_drive::LIMIT_GRACE_S;
        host.standing_limit().is_some_and(|limit| {
            limit.until.is_none_or(|reset| {
                i64::try_from(reset.saturating_add(grace)).is_ok_and(|due| self.now_unix() < due)
            })
        })
    }

    fn limit_spent(&self, host: Option<&dyn IdleHost>, notice: Option<Option<&str>>) -> bool {
        let grace = crate::harness::upgrade_drive::LIMIT_GRACE_S;
        self.limit
            .as_ref()
            .filter(|ep| notice.is_none_or(|reset| ep.reset_text == reset.unwrap_or("-")))
            .and_then(|ep| ep.reset_unix)
            .or_else(|| host.and_then(|h| h.limit_until()?.try_into().ok()))
            .is_some_and(|reset| {
                self.now_unix() >= reset.saturating_add(i64::try_from(grace).unwrap_or(i64::MAX))
            })
    }
}

impl<C: Ctl> Session<'_, C> {
    fn withhold(&mut self, host: Option<&Arc<dyn IdleHost>>, guard: Guard) {
        let Some(host) = host.filter(|h| h.wants()) else {
            self.withheld = None;
            return;
        };
        if self.withheld != Some(guard) {
            self.withheld = Some(guard);
            host.withheld(guard);
        }
    }

    /// Whether the host's point `seen` stands withheld on its caret alone; [`Self::wait_for_next`]
    /// reads such a point's cursor again at each wake.
    fn caret_withholds(&self, seen: &Turn, opts: &SuperviseOpts) -> bool {
        self.withheld == Some(Guard::NotAuthoritative)
            && opts.idle_host.as_ref().is_some_and(|h| h.wants())
            && aterm_phase::read(self.program.as_deref(), &seen.screen.rows, None)
                .phase_authoritative
    }

    fn say_hold(&mut self, seq: u64, hold: Option<HoldSaid>, review: &mut dyn Review) {
        let Some(hold) = hold else {
            self.hold_said = None;
            return;
        };
        if self.hold_said != Some(hold) {
            self.hold_said = Some(hold);
            review.note(&format!("HOLD seq={seq} {}", hold.words()));
        }
    }
}

fn approval_line(rows: &[String]) -> String {
    let first = prompt_box_first_row(rows).map_or("", |i| rows[i].as_str());
    cut_cells(first, APPROVAL_LINE_CELLS).0
}

fn cut_cells(text: &str, cap: usize) -> (String, bool) {
    let clean: String = text
        .chars()
        .map(|c| if limit::breaks_a_line(c) { ' ' } else { c })
        .collect();
    let mut cells = 0;
    let mut out = String::new();
    for c in clean.trim().chars() {
        let w = aterm_grapheme::char_width(c);
        if cells + w > cap {
            return (out, true);
        }
        cells += w;
        out.push(c);
    }
    (out, false)
}

fn fit_bytes(s: &str, n: usize) -> String {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Extracts and decodes the `attention=` value from a `meta` reply, or `None` if unset.
pub(crate) fn standing_attention(stdout: &str) -> Option<String> {
    let line = stdout.lines().find(|l| crate::is_ok_reply(l))?;
    let raw = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("attention="))?;
    if raw == "-" || raw.is_empty() {
        return None;
    }
    Some(super::report::pct_decode(raw)).filter(|t| !t.trim().is_empty())
}

fn reply_word(r: &CtlReply) -> String {
    let text = if r.ok() {
        r.stdout.lines().next().unwrap_or("OK")
    } else {
        r.err_text()
    };
    clip(&one_line(text))
}

fn stray_digit(reader: &dyn ScreenReader, screen: &Screen, digit: &str) -> bool {
    if reader.prompt(&screen.rows).is_some() {
        return false;
    }
    let composer = reader.composer(&screen.rows);
    typed_draft_in(composer.as_ref(), reader.placeholder(), screen)
        && composer.is_some_and(|(_, lines)| lines.len() == 1 && lines[0] == digit)
}

fn typed_draft(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    typed_draft_in(
        reader.composer(&screen.rows).as_ref(),
        reader.placeholder(),
        screen,
    )
}

fn typed_draft_in(
    composer: Option<&(usize, Vec<String>)>,
    placeholder: bool,
    screen: &Screen,
) -> bool {
    let Some((caret, lines)) = composer else {
        return false;
    };
    if lines.iter().skip(1).any(|l| !l.is_empty()) {
        return true;
    }
    if lines.first().is_none_or(|l| l.is_empty()) {
        return false;
    }
    let on_caret_row = screen.cursor_index().is_none_or(|i| i == *caret);
    !(placeholder && on_caret_row && screen.cursor_col == 2)
}

fn heads_with_zero(composer: Option<&(usize, Vec<String>)>) -> bool {
    composer.is_some_and(|(_, lines)| lines.first().is_some_and(|l| l.starts_with('0')))
}

fn survey_event_line(seq: u64, sid: Option<&str>, block: &aterm_phase::SurveyBlock) -> String {
    let target = sid.map(|s| format!(" {s}")).unwrap_or_default();
    let kind = survey_kind_word(block.kind);
    match block.withheld() {
        None => format!(
            "EVENT survey seq={seq}{kind} dismiss with: aterm ctl{target} key 'if={}' 0",
            block.guard
        ),
        Some(why) => format!(
            "EVENT survey seq={seq}{kind} not pressed ({why}); read it with: aterm ctl{target} text"
        ),
    }
}

fn survey_kind_word(kind: aterm_phase::SurveyKind) -> String {
    if kind == aterm_phase::SurveyKind::Session {
        String::new()
    } else {
        format!(" kind={}", kind.name())
    }
}

fn survey_dismissed_line(seq: u64, kind: Option<aterm_phase::SurveyKind>) -> String {
    format!(
        "DISMISSED survey seq={seq}{}",
        kind.map(survey_kind_word).unwrap_or_default()
    )
}

/// Formats [`render_phase`] output plus any open rating survey or autocompact banner line.
pub fn render_phase_and_survey(
    turn: &Turn,
    reader: &dyn ScreenReader,
    python_allow: &[String],
) -> String {
    let rows = &turn.screen.rows;
    let mut out = render_phase(turn, reader, python_allow);
    if let Some(card) = reader.survey_block(rows) {
        match card.withheld() {
            Some(why) => out.push_str(&format!("survey withheld ({why})\n")),
            None => out.push_str("survey 0\n"),
        }
    }
    if let Some(left) = reader.context(rows) {
        out.push_str(&format!("context {left}%\n"));
    }
    out
}

/// Formats the multi-line `aterm drive phase` summary for `turn`.
pub fn render_phase(turn: &Turn, reader: &dyn ScreenReader, python_allow: &[String]) -> String {
    let rows = &turn.screen.rows;
    let mut out = format!("{}\n", turn.phase.name());
    match &turn.phase {
        Phase::Prompt => {
            if let Some((p, esc)) = phase_prompt(reader, rows) {
                let verdict = reader
                    .prompt(rows)
                    .filter(|v2| v2.kind == PromptKind::Bash)
                    .map(|v2| readings_read_only(&v2.readings(), python_allow));
                out.push_str(&render_prompt(&p, esc, verdict));
            }
            if let Some(token) = reader.prompt(rows).as_ref().and_then(prompt_box_token) {
                out.push_str(&format!("box {token}\n"));
            }
        }
        Phase::Busy => match busy_rule(reader, rows) {
            Some(b) => out.push_str(&format!("reason {b}\n")),
            None if turn.screen.rows.is_empty() => {
                out.push_str("reason no screen: no read answered before the timeout\n")
            }
            None => out.push_str("reason whole screen, no composer frame: output still changing\n"),
        },
        Phase::Limited { message, reset } => {
            out.push_str(&format!(
                "message {message}\nreset {}\n",
                reset.as_deref().unwrap_or("-")
            ));
        }
        Phase::Idle | Phase::Question => {}
    }
    out
}

fn phase_prompt(
    reader: &dyn ScreenReader,
    rows: &[String],
) -> Option<(Prompt, Option<&'static str>)> {
    match reader.program() {
        Program::Claude => parse_prompt(rows).map(|p| {
            let esc = reader
                .prompt(rows)
                .map_or(Some(CancelEffect::Back), |v2| reader.esc(&v2));
            (p, esc_line(esc))
        }),
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
            reader.prompt(rows).map(|p| {
                let esc = esc_line(reader.esc(&p));
                let prompt = Prompt {
                    kind: p.kind,
                    command: p.command,
                    description: p.description,
                    notes: p.notes,
                    options: p
                        .options
                        .iter()
                        .filter_map(|o| o.n.map(|n| (n, o.label.clone())))
                        .collect(),
                };
                (prompt, esc)
            })
        }
    }
}

fn esc_line(effect: Option<CancelEffect>) -> Option<&'static str> {
    match effect? {
        CancelEffect::Reject | CancelEffect::Back => Some("cancel esc"),
        CancelEffect::Exit => Some("exit esc"),
        CancelEffect::Interrupt => Some("interrupt esc"),
    }
}

fn busy_rule(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    match reader.program() {
        Program::Claude | Program::Generic => busy_signal(rows).map(|b| b.to_string()),
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini => {
            let status_row = reader.busy_guard().is_some_and(|guard| {
                aterm_observe::row_matcher(&guard).is_ok_and(|m| rows.iter().any(|r| m.matches(r)))
            });
            if status_row {
                return Some("status row: esc to interrupt".to_string());
            }
            if reader.phase(rows) != Phase::Busy {
                return None;
            }
            Some(match reader.background_wait(rows) {
                Some(work) => format!("its own background work: {work}"),
                None => "its reader reads it busy".to_string(),
            })
        }
    }
}

/// Formats the single-line `EVENT <phase> seq=<n> <summary>` record for `turn`.
pub fn event_line(turn: &Turn, python_allow: &[String]) -> String {
    format!(
        "EVENT {} seq={} {}",
        turn.phase.name(),
        turn.screen.seq,
        event_summary(turn, python_allow)
    )
}

fn event_summary(turn: &Turn, python_allow: &[String]) -> String {
    let rows = &turn.screen.rows;
    let reader = aterm_phase::identify(None, rows);
    let Some(p) = (turn.phase == Phase::Prompt)
        .then(|| reader.prompt(rows))
        .flatten()
    else {
        return said_summary(reader, turn);
    };
    let classify = if p.kind == PromptKind::Bash {
        match readings_read_only(&p.readings(), python_allow) {
            Ok(()) => "read-only".to_string(),
            Err(why) => format!("not-read-only:{why}"),
        }
    } else {
        "-".to_string()
    };
    let what = if p.command.is_empty() && p.kind == PromptKind::Workflow {
        p.description.as_str()
    } else {
        p.command.as_str()
    };
    format!(
        "kind={} classify={classify} command={}",
        p.kind.name(),
        or_dash(&clip(what))
    )
}

fn said_summary(reader: &dyn ScreenReader, turn: &Turn) -> String {
    let rows = &turn.screen.rows;
    let width = turn.screen.cols.map(usize::from);
    match &turn.phase {
        Phase::Prompt => "kind=other classify=- command=-".to_string(),
        Phase::Limited { message, reset } => format!(
            "message={} reset={}",
            clip(message),
            reset.as_deref().map_or_else(|| "-".to_string(), clip)
        ),
        Phase::Busy | Phase::Idle | Phase::Question => match reader.program() {
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini => {
                or_dash(&clip_tail(&said_line(reader, rows, width)))
            }
            Program::Claude => {
                let line = last_said_index(rows).map_or_else(String::new, |last| {
                    let first = width.map_or(last, |w| aterm_phase::line_start(rows, last, w));
                    rows[first..=last]
                        .iter()
                        .map(|r| r.trim())
                        .collect::<Vec<_>>()
                        .join(" ")
                });
                or_dash(&clip_tail(&line))
            }
            Program::Generic => or_dash(&clip(last_said_row(rows).unwrap_or("").trim())),
        },
    }
}

fn said_line(reader: &dyn ScreenReader, rows: &[String], width: Option<usize>) -> String {
    let Some(tail) = reader.said_tail(rows) else {
        return String::new();
    };
    let lines: Vec<&str> = tail
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let Some(&last) = lines.last() else {
        return String::new();
    };
    let wrapped = width
        .and_then(|w| {
            let end = reader.transcript_end(rows).min(rows.len());
            let at = (0..end).rev().find(|&i| rows[i].trim().ends_with(last))?;
            Some(at + 1 - aterm_phase::line_start(rows, at, w))
        })
        .unwrap_or(1)
        .clamp(1, lines.len());
    lines[lines.len() - wrapped..].join(" ")
}

fn submit_mark(screen: &Screen) -> Option<(u64, u64)> {
    let rows = &screen.rows;
    let upto = last_said_index(rows).map_or(0, |k| k + 1);
    screen.submits.map(|n| {
        (
            n,
            super::policy::turn_end::screen_mark(&rows[..upto.min(rows.len())]),
        )
    })
}

fn submitted_over(then: Option<(u64, u64)>, now: &Screen) -> bool {
    matches!((then, submit_mark(now)), (Some((n0, m0)), Some((n, m))) if n > n0 && m != m0)
}

fn point_key(reader: &dyn ScreenReader, turn: &Turn) -> String {
    let rows = &turn.screen.rows;
    if turn.phase == Phase::Prompt
        && let Some(p) = reader.prompt(rows)
    {
        return box_key(&p, rows);
    }
    let mut key = format!(
        "{} {}\n{}\n",
        turn.phase.name(),
        said_summary(reader, turn),
        status_row(rows).unwrap_or("")
    );
    let span = (turn.phase == Phase::Prompt)
        .then(|| prompt_box_span(rows))
        .flatten();
    let upto = match span {
        Some((a, _)) => a,
        None => last_said_index(rows).map_or(0, |k| k + 1),
    };
    push_words_above(&mut key, &rows[..upto]);
    if let Some((a, b)) = span {
        for r in &rows[a..=b] {
            key.push_str(r.trim_end());
            key.push('\n');
        }
    }
    key
}

fn push_words_above(key: &mut String, above: &[String]) {
    let said: Vec<&String> = above.iter().filter(|r| !r.trim().is_empty()).collect();
    for r in &said[said.len().saturating_sub(KEY_ROWS)..] {
        key.extend(r.chars().filter(|c| c.is_alphabetic()));
        key.push('\n');
    }
}

fn box_key(p: &aterm_phase::PromptV2, rows: &[String]) -> String {
    use aterm_phase::QuestionForm;
    let form = p.question_dialog.as_ref().map(|d| match d.form {
        QuestionForm::Single { .. } => "single",
        QuestionForm::Multi { .. } => "multi",
        QuestionForm::Preview { .. } => "preview",
        QuestionForm::Review { .. } => "review",
        QuestionForm::Unknown { .. } => "unknown",
    });
    let whole = !p.head_off_screen && form != Some("unknown");
    let mut key = format!(
        "prompt {} {}",
        p.kind.name(),
        if whole { "whole" } else { "cut" }
    );
    if let Some(form) = form {
        key.push_str(" form=");
        key.push_str(form);
    }
    key.push('\n');
    let numbers: Vec<String> = p
        .options
        .iter()
        .map(|o| o.n.map_or_else(|| "-".to_string(), |n| n.to_string()))
        .collect();
    key.push_str(&numbers.join(" "));
    key.push('\n');
    let focused = p.focused().map(|o| o.row);
    for (i, row) in approval_loop::box_rows(p, rows) {
        let row = if Some(i) == focused {
            row.trim_start_matches(['›', '❯'])
        } else {
            row
        };
        key.extend(row.chars().filter(|&c| !is_layout(c)));
    }
    key.push('\n');
    let above = &rows[..p.span.0.min(rows.len())];
    if whole {
        let letters: Vec<char> = before_the_echo(above)
            .iter()
            .flat_map(|r| r.chars())
            .filter(|c| c.is_alphabetic())
            .collect();
        key.extend(&letters[letters.len().saturating_sub(ABOVE_LETTERS)..]);
        key.push('\n');
    } else {
        push_words_above(&mut key, above);
    }
    key
}

fn before_the_echo(above: &[String]) -> &[String] {
    let gap = |r: &String| r.chars().all(is_layout);
    let Some(last) = above.iter().rposition(|r| !gap(r)) else {
        return above;
    };
    match above[..last].iter().rposition(gap) {
        Some(sep) => &above[..sep],
        None => above,
    }
}

fn is_layout(c: char) -> bool {
    c.is_whitespace() || ('\u{2500}'..='\u{257F}').contains(&c)
}

fn file_request_key(reader: &dyn ScreenReader, turn: &Turn) -> Option<String> {
    if turn.phase != Phase::Prompt {
        return None;
    }
    let rows = &turn.screen.rows;
    let p = reader.prompt(rows)?;
    let file = matches!(
        p.kind,
        PromptKind::Edit | PromptKind::Write | PromptKind::Overwrite | PromptKind::Read
    );
    if !file || p.head_off_screen {
        return None;
    }
    let options = p.options.iter().map(|o| o.row).min().unwrap_or(usize::MAX);
    let mut key = format!("{}\n", p.kind.name());
    for (_, row) in approval_loop::box_rows(&p, rows)
        .into_iter()
        .take_while(|(i, _)| *i < options)
    {
        key.extend(row.chars().filter(|&c| !is_layout(c)));
    }
    Some(key)
}

fn clip(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(LINE_CHARS)
        .collect()
}

fn clip_tail(s: &str) -> String {
    let n = s.chars().count();
    if n <= LINE_CHARS {
        return clip(s);
    }
    let rest: String = s.chars().skip(n + 1 - LINE_CHARS).collect();
    format!("…{}", clip(&rest))
}

fn or_dash(s: &str) -> String {
    if s.is_empty() {
        "-".to_string()
    } else {
        s.to_string()
    }
}

/// Formats a single-line `EXIT` reason from a fatal control error string.
pub fn exit_reason(err: &str) -> String {
    let err = one_line(err);
    if err.contains("ERR exited") || err.contains("no such session") {
        format!("session gone ({err})")
    } else {
        err
    }
}

fn one_line(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
fn log_line(log: &mut dyn Write, line: &str) {
    let _ = emit(log, line);
}

fn emit(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|e| format!("cannot write a line to stdout: {e}"))
}

/// A prompt's lines ([`render_phase`]). `verdict` is the read-only rule's
/// verdict on a Bash box's command alone ([`readings_read_only`] over every
/// reading of its rows), said as `classify read-only` or `classify
/// not-read-only <reason>`; `None` on a Bash box is one whose rows were not
/// read whole, which is no proof.
fn render_prompt(p: &Prompt, esc: Option<&str>, verdict: Option<Result<(), String>>) -> String {
    let mut out = format!("kind {}\n", p.kind.name());
    if !p.command.is_empty() {
        out.push_str(&format!("command {}\n", p.command));
    }
    if !p.description.is_empty() && p.kind != PromptKind::Other {
        out.push_str(&format!("description {}\n", p.description));
    }
    for note in &p.notes {
        out.push_str(&format!("note {note}\n"));
    }
    if p.kind == PromptKind::Bash {
        match verdict {
            Some(Ok(())) => out.push_str("classify read-only\n"),
            Some(Err(why)) => out.push_str(&format!("classify not-read-only {why}\n")),
            None => out.push_str("classify not-read-only the box's command was not read whole\n"),
        }
    }
    for (n, text) in &p.options {
        out.push_str(&format!("option {n} {text}\n"));
    }
    if let Some(esc) = esc {
        out.push_str(esc);
        out.push('\n');
    }
    out
}

/// Formats the compact result returned by [`Session::supervise`].
#[cfg(test)]
pub(crate) fn render_result(
    turn: &Turn,
    reader: &dyn ScreenReader,
    python_allow: &[String],
) -> String {
    let mut out = render_phase(turn, reader, python_allow);
    out.push_str(&render_result_rows(turn));
    out
}

#[cfg(test)]
fn render_result_rows(turn: &Turn) -> String {
    let mut out = String::from("--\n");
    let rows = &turn.screen.rows;
    let span = (turn.phase == Phase::Prompt)
        .then(|| prompt_box_span(rows))
        .flatten();
    match span {
        Some((a, b)) => {
            for r in &rows[a..=b] {
                out.push_str(r);
                out.push('\n');
            }
        }
        None => {
            let kept: Vec<&String> = rows.iter().filter(|r| !r.trim().is_empty()).collect();
            let start = kept.len().saturating_sub(RESULT_ROWS);
            for r in &kept[start..] {
                out.push_str(r);
                out.push('\n');
            }
        }
    }
    out
}

/// Formats a Unix timestamp in seconds as `YYYY-MM-DDTHH:MM:SSZ`.
#[must_use]
pub fn utc_stamp(secs: u64) -> String {
    aterm_types::rfc3339::format_rfc3339(secs)
}

#[path = "answer.rs"]
mod answer;
pub use answer::{
    AnswerOpts, EXIT_NO_BOX, EXIT_NOT_SERVED, EXIT_REFUSED, prompt_box_token, question_box_token,
};
#[path = "approval_loop.rs"]
mod approval_loop;
#[path = "beside.rs"]
mod beside;
/// Re-exports whole-box approval decision evaluation for tape explanation.
pub(crate) use approval_loop::whole_box_decision;
#[path = "box_head.rs"]
mod box_head;
/// Re-exports approval box header search helpers for tape replay.
pub(crate) use box_head::{SEARCH as HEAD_SEARCH, head_row, head_rows, joined_box, same_box};
#[path = "claim.rs"]
mod claim;
/// Re-exports the write-verb classifier for harness tape accounting.
pub(crate) use claim::is_write;
pub use claim::{CLAIM_HELD, CLAIM_RENEW};
#[path = "escalate.rs"]
mod escalate;
#[path = "host_door.rs"]
pub(crate) mod host_door;
pub use host_door::IdleHost;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "occupancy_loop.rs"]
mod occupancy_loop;
#[path = "press.rs"]
mod press;
#[path = "reset_loop.rs"]
mod reset_loop;
#[path = "setup.rs"]
mod setup;
pub use setup::SessionSetup;
#[path = "stall.rs"]
mod stall;
#[path = "task_mark.rs"]
mod task_mark;
pub use task_mark::{HELD_HEAD, STARTUP_HEAD, deadline_attention, mark_task_exited};
#[path = "turn_end_loop.rs"]
mod turn_end_loop;
#[path = "wall_confirm.rs"]
pub(crate) mod wall_confirm;
pub(super) use wall_confirm::words_kind;
#[path = "wall_memory.rs"]
mod wall_memory;
pub use turn_end_loop::standing_rules;
/// Re-exports composer cursor-row guard constructors.
pub(crate) use turn_end_loop::{composer_guard, composer_guard_at};
/// Re-exports turn-text folding and standing-rules helpers.
#[cfg(test)]
pub(crate) use turn_end_loop::{one_turn, standing_rules_in};

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "run_frame_tests.rs"]
mod frame_tests;
