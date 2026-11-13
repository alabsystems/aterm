// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor loop driving worker sessions over [`Ctl`] (`aterm-ctl` or relay).
//!
//! Waits for a worker's turn to end ([`Session::await_turn_from`]), evaluates approval
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
use super::phase::{
    Phase, busy_seen, busy_signal, last_said_index, last_said_line, last_said_row, status_row,
};
use super::policy::approval::{Decision, footer_mode, readings_read_only};
use super::policy::exception::Exception;
use super::policy::reset::{
    KeepWhy, ResetAsk, ResetEnd, ResetGo, ResetNo, ResetOffer, ResetOutcome, ResetWord,
};
use super::policy::sleeps::{FirstLook, SleepBook, SleepStamp};
use super::policy::turn_end::{
    AccountAdvice, AccountBad, AccountPoint, AgentResumed, Composer, FirstRead, GaveUp,
    ModelSwitch, PERSON_TYPING, ProbeGate, RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY,
    RULE_LIMIT_RESUME, RULE_OUTAGE_RESUME, Reach, TurnEndAction, TurnEndReading, TurnEndState,
    TurnEndTiming, WaitKind, is_continue_rule, said_mark,
};
use super::prompt::{
    CancelEffect, Prompt, PromptKind, PromptV2, parse_prompt, prompt_box_first_row, prompt_box_span,
};
use super::screen::{COMPOSER_COL, HumanInput, Screen, cursor_home, parse_text_json};
use crate::harness::agent_image::Act;
use crate::harness::contract::Scope;
use aterm_phase::{Program, ScreenReader};
pub use duty::BuiltinAuthority;

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
        self.stdout
            .split_whitespace()
            .find_map(|w| w.strip_prefix("seq="))
            .and_then(crate::parse_uint)
    }
    /// Returns `true` if a guarded write matched no row (`OK skipped`).
    pub fn skipped(&self) -> bool {
        self.ok() && self.stdout.trim_start().starts_with("OK skipped")
    }
    /// Returns `true` if a generation-fenced press was skipped because the screen changed.
    pub fn skipped_changed(&self) -> bool {
        self.skipped()
            && self
                .stdout
                .split_whitespace()
                .any(|w| w == "reason=changed")
    }
    /// Returns `true` if a human-count-fenced press was skipped due to recent user input.
    pub fn skipped_person(&self) -> bool {
        self.skipped() && self.stdout.split_whitespace().any(|w| w == "reason=person")
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
                        .and_then(crate::parse_uint::<u32>)
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
            .and_then(crate::parse_uint)
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
    /// Publish the loop's actual continuation decision without granting an effect.
    fn continuation_activity(&mut self, _reason: &str, _wait_ms: Option<u64>) {}
    /// Executes `args` against the target session after any `@sid` selector.
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String>;

    /// Execute a semantic duty's request on a private hosted adapter. `None` means this is
    /// an ordinary adapter and the caller must retain its legacy claim checks. A refused
    /// typed request is `Some(Ok(refusal))`, never permission to retry without its duty.
    fn call_for_duty(
        &mut self,
        _duty: Scope,
        _revision: u64,
        _args: &[&str],
    ) -> Option<Result<CtlReply, String>> {
        None
    }

    /// Observe the private hosted adapter's current duty ownership. This observation is
    /// recorded by a tape and never re-read from the live host during replay.
    fn builtin_authority(&mut self, _sid: &str) -> BuiltinAuthority {
        BuiltinAuthority::Unsupported
    }

    /// What the host lets the harness do in `sid`'s session: [`Act::On`] only while the kernel
    /// confirms its foreground agent by its image ([`crate::harness::agent_image`]). Read only by
    /// a loop whose host confirms agents ([`Session::set_confirms_agent`]); recorded by a tape and
    /// never re-read from the live host during replay. An ordinary adapter confirms nothing and
    /// answers [`Act::On`]: its loop is not the window's harness.
    fn agent_act(&mut self, _sid: &str) -> Act {
        Act::On
    }

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
    /// Whether the last `status` printed the session log's head (`log_seq= log_ep= … turn=`);
    /// `None` until a `status` is read this session or since a reconnect.
    pub log: Option<bool>,
    /// `Some(false)` once the host refused the loop's `await log` (an older build answers
    /// `ERR usage`): the loop waits on the screen until a reconnect.
    pub await_log: Option<bool>,
    /// Whether `await gone|agent|seq|idle` take `autopilot=<word>`: its waits then wake on a switch
    /// of the session's word (v6 §14.10). An older host reads only the arguments it knows and
    /// answers such a wait as it always did, so there a switch is seen at the loop's next wake; a
    /// host that refused the token as a usage error would get each wait bare.
    pub await_autopilot: Option<bool>,
    /// `Some(false)` once a host refused `autopilot=<word>` on the loop's `await log` as a usage
    /// error and answered the wait bare (a build from before the log wait took it): the loop's log
    /// waits go bare until a reconnect, and there a switch made while the agent works is seen at
    /// the turn's point.
    pub await_log_word: Option<bool>,
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

/// The word a host step's [`HostStep::line`] names its step by (`<what> step=<word>`, as the
/// window's host spells it).
fn host_step_word(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix("step="))
}

/// Turn-end policy context offered to [`IdleHost::at_idle_on`] at an idle point.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct IdlePoint {
    /// Whether the task has landed and been defended under Autopilot Land (`Nothing left:`).
    pub done_under_land: bool,
    /// What holds a fact the host would type at this point of its own (`HostTurn::Fact`: the ask
    /// to free disk space), by the occupancy door's own guards
    /// ([`crate::supervise::occupancy::Guards::held_host_fact`]): a person typed within
    /// `[harness] human_grace_s` or another driver's hand is on the session ([`Guard::Person`]),
    /// the turn was the person's own exchange ([`Guard::Exchange`]), or a manager awaits the
    /// session ([`Guard::Managed`]). The host types no such fact here; its carry-on and its
    /// upgrade keep their own gates.
    pub held: Option<Guard>,
    /// Of those, the first that holds the session for someone else for as long as they keep it
    /// ([`crate::supervise::occupancy::Guards::theirs`]): the person's own exchange
    /// ([`Guard::Exchange`]) or a manager's wait ([`Guard::Managed`]). A fact [`Self::held`] holds
    /// for a person's grace alone stays owed for a later point; one for a session that is theirs
    /// is given up, as no later point of theirs would take it.
    pub theirs: Option<Guard>,
    /// The point reads as a question, and rests for the host only because a relaunch's carry-on is
    /// owed ([`IdleHost::carry_owed`]): a conversation that ended on a question comes back on it.
    /// The carry-on steps here and nothing else of the host's does.
    pub carry_only: bool,
    /// The stop for replies that repeat with no work holds the loop's typing here
    /// ([`super::policy::turn_end::TurnEndState::replies_stop`], as the decision at this point read
    /// it), so a carry-on owed here types nothing either: a relaunch is no one else's turn.
    pub replies_held: bool,
    /// A line the loop wrote stands held in custody (ATERM-KI-1101-3), and what the loop has judged
    /// of it ([`LineHeld`]): `None` where no line is held. The loop says it at an idle point and at
    /// a break of the agent's own background work while the line stands, and offers no break's
    /// fact at all then ([`IdleHost::at_background_fact`]).
    pub line_held: Option<LineHeld>,
}

pub use super::custody::LineHeld;

/// A tape spells a point as it did before [`IdlePoint::held`], [`IdlePoint::theirs`],
/// [`IdlePoint::carry_only`], [`IdlePoint::replies_held`] and [`IdlePoint::line_held`] were there
/// whenever none of them says anything, so a tape recorded before them still replays.
impl std::fmt::Debug for IdlePoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("IdlePoint");
        d.field("done_under_land", &self.done_under_land);
        if let Some(held) = &self.held {
            d.field("held", held);
        }
        if let Some(theirs) = &self.theirs {
            d.field("theirs", theirs);
        }
        if self.carry_only {
            d.field("carry_only", &true);
        }
        if self.replies_held {
            d.field("replies_held", &true);
        }
        if let Some(held) = &self.line_held {
            d.field("line_held", held);
        }
        d.finish()
    }
}

/// A withhold of the loop's at a turn break that lasts past any grace, told to the host as the
/// loop decides each break ([`IdleHost::lasting_hold`]): nothing the host would type there goes
/// until it is over, so a host that offers the session work it would type at a break (the
/// `disk-free@v1` seat rule's) holds it out meanwhile, as it does at a usage-limit wall
/// ([`IdleHost::limit_wall`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lasting {
    /// The credits hold ([`Guard::Credits`]: `[harness] usage_credits = false`, and usage credits
    /// would carry the next turn): it lifts at `lifts_unix` (unix seconds, the reset plus the
    /// loop's grace, or the hold's bound), `None` where the loop knows no such time.
    Credits { lifts_unix: Option<i64> },
    /// A wall on screen that is no usage limit ([`Guard::Wall`]), by its kind's wire word
    /// ([`aterm_phase::WallKind::wire`]): `wall:auth`, a sign-in only a person can do, or another
    /// the loop waits out.
    Wall { wire: &'static str },
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
    /// Manager awaits the session (`TurnEndState::managed`).
    Managed,
    /// Host withholds or owns the session's turn ends (`IdleHost::withholds_turn_end` or `IdleHost::owns_turn_end`).
    HostWithheld,
    /// Upgrade notice is due (`IdleHost::upgrade_due`).
    UpgradeDue,
    /// Host line was already sent during this break.
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
    /// A person's command standing in the composer held the point's act (v6 §14.17.5.8): said
    /// once while it holds, naming its form and never its words.
    Command(aterm_phase::DraftCommand),
    NoHostStep,
    Held(Guard),
}

impl HoldSaid {
    fn words(self) -> String {
        match self {
            HoldSaid::Point(why) => format!("point nothing why={why}"),
            HoldSaid::Command(form) => format!(
                "point nothing why=command a person's {} {}",
                form.words(),
                super::policy::turn_end::COMMAND_STANDS
            ),
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
    /// The restart (its [`Restart::word`](super::policy::turn_end::Restart::word)) whose host's
    /// word this due waits out ([`Session::restart_later`]); any other due the loop stands
    /// replaces it, whatever the restart's run counters still hold.
    pub(super) put_off: Option<&'static str>,
}

const ACCOUNT_NEWS_EVERY: Duration = Duration::from_secs(60);

const WAIT_SKEW_S: u64 = 2;

/// Account usage notices observed on a screen read ([`IdleHost::usage_seen`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageSeen {
    pub credits: Option<aterm_phase::credits::Credits>,
    pub warning: Option<aterm_phase::credits::Warning>,
    /// A model's week, its wall or its warning ([`aterm_phase::ScreenReader::model_week`]).
    pub week: Option<aterm_phase::ModelWeek>,
}

impl UsageSeen {
    /// Returns `true` if a credits notice, a usage warning or a model's week is present.
    #[must_use]
    pub const fn says(&self) -> bool {
        self.credits.is_some() || self.warning.is_some() || self.week.is_some()
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
    /// No turn ran on this session since its last read was claimed: time alone makes no read due.
    NotUsed,
    /// Probe claim is already held for this account or globally.
    Claimed,
    /// Account store lock remained held past the bounded wait.
    LockHeld,
    /// Account state file could not be read or written.
    State,
}

impl ProbeNo {
    /// All probe refusal reasons in [`Self::word`] order.
    pub const ALL: [ProbeNo; 9] = [
        ProbeNo::NoAccounts,
        ProbeNo::Mode,
        ProbeNo::Unread,
        ProbeNo::NotJudged,
        ProbeNo::NotDue,
        ProbeNo::NotUsed,
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
            ProbeNo::NotUsed => "not-used",
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
}

/// Configuration options for [`Session::run_hosted`], `Session::watch`, and `Session::supervise`.
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

/// Whether `rows` say the context indicator `reader` reads ([`ScreenReader::context`]) has gone,
/// as after `/compact`: Claude Code's footer rule ([`crate::harness::footer::context_gone`]), the
/// one the window drops its context fact on. A screen without a reading is not enough: the `/` and
/// `@` menus, the shortcuts and Claude Code's own notices stand in the indicator's place while
/// they are open, and a box takes the composer away. No other program's reading is read as gone.
fn indicator_gone(reader: &dyn ScreenReader, rows: &[String]) -> bool {
    match reader.program() {
        Program::Claude => crate::harness::footer::context_gone(rows),
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => false,
    }
}
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
/// How long a switch of the session's Autopilot word stands before the loop decides on it (v6
/// §14.10): a chip click passing through Land on its way to Off types nothing.
const WORD_SETTLE: Duration = Duration::from_secs(1);
/// The most switches of the session's word one wait inside the agent's turn notes before it ends
/// as timed out ([`Session::busy_wait`]); each wakes it once, its next try carrying the word.
const BUSY_WORD_WAKES: usize = 4;
const ANSWERED_PAST_LIMIT: &str = "the worker answered past the adopted limit's notice";
const IDLE_MS: &str = "2000";
const SETTLE_MS: &str = "500";
const SETTLE_CAP: Duration = Duration::from_millis(1500);
const FRAME_WAIT: Duration = Duration::from_secs(61);
const FRAME_REREADS: u32 = 8;
const WRITING_CAP: Duration = Duration::from_secs(3);
/// Duration an unvouched idle read must persist before being treated as a point.
const NO_EVIDENCE_HOLD: Duration = Duration::from_secs(1);
/// Hang detector for a read held while the program's window title still reports a running turn: a
/// minute of its reply unchanged ([`ScreenReader::said_tail`]), not a latency budget. A status line
/// that refreshes, or a footer timer, moves the screen and not the reply.
const TITLE_HOLD: Duration = Duration::from_secs(60);
/// Hang detector for a read held while the agent has drawn nothing of its own
/// ([`Session::first_frame_owed`]): a minute of unmoved screen, not a latency budget, since a
/// starved program may draw its first frame long after it started.
const FIRST_FRAME_HANG: Duration = Duration::from_secs(60);
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
    /// `OK autopilot <word>`: the session's stored word (`None` unset) is not the one a wait at
    /// rest carried ([`Session::park`]).
    Word(Option<Autopilot>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Next {
    Gone(BusyWait),
    Settle,
    Idle,
    /// Past the content seq of a read the agent had drawn nothing on.
    Moved(u64),
    /// A turn the program's title runs and its screen does not show (a Claude Code reply drawn
    /// under a status line, `aterm_phase::ScreenReader::title_live`): the log wakes the wait for
    /// its end where the host keeps one, and the short settle does elsewhere. No busy row stands
    /// to wait gone, and one that never shows answers `await gone` at once.
    Run,
}

/// `screen`'s phase as its reader reads the rows beside what the window title read with them
/// says of the program's run (`screen.title`, sent with the rows; a host that sends none says
/// nothing): a turn the title runs and the screen does not show reads Busy
/// ([`ScreenReader::phase_seen`]).
fn phase_of(reader: &dyn ScreenReader, screen: &Screen) -> Phase {
    reader.phase_seen(&screen.rows, reader.runs(screen.title.as_deref()))
}

/// Whether the program's title, read with `screen`'s rows, runs a turn the rows do not show
/// ([`ScreenReader::title_live`]).
fn title_run(reader: &dyn ScreenReader, screen: &Screen) -> bool {
    reader.title_live(&screen.rows, reader.runs(screen.title.as_deref()))
}

/// The agent's first frame, as this loop has read it ([`Session::first_frame_owed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FirstFrame {
    /// No read since the loop began showed anything the agent drew.
    Owed {
        /// A read was held as the agent still starting: this loop saw it before its first frame.
        held: bool,
    },
    /// A hold on an unmoved screen ran out ([`FIRST_FRAME_HANG`]) and no read since showed
    /// anything the agent drew: reads are no longer held, and each point read before the agent
    /// draws is its launch's, no turn of its own ([`Session::launch_unmet`]). A starved agent can
    /// take longer than the hang detector to draw its first frame.
    RanOut {
        /// Whether a read was held before the hold ran out ([`Self::Owed`]'s `held`).
        held: bool,
    },
    /// A read showed it.
    Drawn {
        /// Whether a read was held before it ([`Self::Owed`]'s `held`).
        held: bool,
    },
}

impl FirstFrame {
    /// Whether this loop held a read as the agent still starting, so the agent started under it.
    pub(super) const fn saw_start(self) -> bool {
        match self {
            Self::Owed { held } | Self::RanOut { held } | Self::Drawn { held } => held,
        }
    }
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
        Program::Codex | Program::Grok | Program::Kimi | Program::Agy | Program::Muse => false,
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
    /// A wait at rest woke on a switch of the session's word ([`Wait::Word`]).
    Word(Option<Autopilot>),
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
    let ago =
        |ms: i64| Duration::from_millis(u64::try_from(now_ms.saturating_sub(ms)).unwrap_or(0));
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
    let ago =
        |ms: i64| Duration::from_millis(u64::try_from(now_ms.saturating_sub(ms)).unwrap_or(0));
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

/// THE END A LOOP BEFORE THIS ONE NOTED, read back from its journal (newest first): the mark of its
/// words, when it was first noted, whether the task's own wake kept it, the receipt journaled with
/// it (`RECEIPT` with the `DONE` note's seq, §14.9 slice 9b; a receipt of another seq is not read),
/// and, for a worker's failed conclusion (a `FAILED` note, which rests as a landing does), how it
/// ended.
type EndedBefore = (
    Option<u64>,
    i64,
    bool,
    Option<crate::harness::acts::LandReceipt>,
    Option<GaveUp>,
);

fn ended_before(journal: &str, sid: Option<&str>, last_ms: Option<i64>) -> Option<EndedBefore> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let land = format!("rule={}", super::policy::turn_end::RULE_LAND);
    let receipt_line = format!("\"line\":\"{} ", crate::harness::land_receipt::RECEIPT);
    let mut later: Vec<super::journal::JournalRecord> = Vec::new();
    let mut note: Option<Option<u64>> = None;
    let mut noted_ms = 0;
    let mut by_wake = false;
    let mut after: Option<(u64, crate::harness::acts::LandReceipt)> = None;
    let mut receipt = None;
    let mut failed = None;
    let records = journal.lines().rev().filter_map(|text| {
        let end = text.contains("\"line\":\"DONE ")
            || text.contains("\"line\":\"LEFT ")
            || text.contains("\"line\":\"FAILED ")
            || text.contains(&receipt_line);
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
        let ends = word == "DONE" || word == "LEFT" || word == "FAILED";
        let Some(seq) = note else {
            if !ends {
                if said_a_point(&rec) {
                    later.push(rec);
                } else if word == crate::harness::land_receipt::RECEIPT && after.is_none() {
                    after = crate::harness::acts::LandReceipt::of_line(
                        &rec.line,
                        rec.ended.unwrap_or(rec.t),
                    );
                }
                continue;
            }
            let seq = words
                .next()
                .and_then(|w| w.strip_prefix("seq="))
                .and_then(crate::parse_uint::<u64>);
            let rule = words.next().unwrap_or_default();
            if !matches!(word, "DONE" | "FAILED")
                || rule != land
                || last_ms.is_some_and(|t| t > rec.t)
            {
                return None;
            }
            if word == "FAILED" {
                failed = Some(GaveUp::of_journal_line(&rec.line)?);
            }
            noted_ms = rec.ended.map_or(rec.t, |first| first.min(rec.t));
            by_wake = rec.line.contains(super::policy::turn_end::WAKE_KEPT);
            receipt = after
                .take()
                .filter(|(at, _)| Some(*at) == seq)
                .map(|(_, r)| r);
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
            .then_some((landing, noted_ms, by_wake, receipt, failed));
    }
    let seq = note?;
    later
        .iter()
        .all(|p| !is_box(p) && p.seq.is_some() && p.seq == seq)
        .then_some((None, noted_ms, by_wake, receipt, failed))
}

#[cfg(test)]
fn landed_before(journal: &str, sid: Option<&str>, last_ms: Option<i64>) -> Option<Option<u64>> {
    ended_before(journal, sid, last_ms)
        .filter(|(_, _, _, _, failed)| failed.is_none())
        .map(|(words, _, _, _, _)| words)
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
                        let until = crate::harness::unix_time::parse_utc(until)?;
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
                // An act typed at a point read since the last wait: no wait came before it there.
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

/// THE BOOKS WITH AN OFF RECORD SPLICED AT EACH PARK THE JOURNAL SAYS (`PARKED seq=<n>
/// autopilot=off`), before the first row stamped at or after it: a build through 0.105.0 kept
/// only that note. A park the ledger records itself (an [`approvals::RULE_OFF_STOOD`] row of
/// the session's with the park's seq, stamped at or after it) is read from that row alone, which
/// says what the park kept: a splice before it would end the Land episode an Off no person set
/// keeps over Land's rest on a need ([`approvals::OFF_STOOD_RESTING`], the second review of the
/// post-landing fixes, 2026-11-01).
fn with_parks_journaled<'b>(
    body: &'b str,
    journal: Option<&str>,
    sid: Option<&str>,
) -> std::borrow::Cow<'b, str> {
    let bare = sid.map(|s| s.trim_start_matches('@'));
    let recorded: Vec<(i64, u64)> = body
        .lines()
        .filter(|line| line.contains(approvals::RULE_OFF_STOOD))
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|v| {
            v.get("rule_id").and_then(aterm_json::Value::as_str) == Some(approvals::RULE_OFF_STOOD)
                && v.get("sid")
                    .and_then(aterm_json::Value::as_str)
                    .map(|s| s.trim_start_matches('@'))
                    == bare
        })
        .filter_map(|v| Some((v.get("ts")?.as_i64()?, v.get("box_seq")?.as_u64()?)))
        .collect();
    let parks: Vec<(i64, u64)> = journal
        .unwrap_or_default()
        .lines()
        .filter(|text| text.contains("PARKED seq="))
        .filter_map(|text| super::journal::JournalRecord::from_json(text).ok())
        .filter(|rec| rec.sid.as_deref() == bare)
        .filter(|rec| {
            lifecycle::parked_off_seq(&rec.line)
                .is_some_and(|seq| !recorded.iter().any(|&(ts, own)| own == seq && ts >= rec.t))
        })
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
    let w: i64 = crate::parse_uint(w)?;
    let ms = match unit {
        "s" => w.checked_mul(1000)?,
        "min" => w.checked_mul(60_000)?,
        _ => return None,
    };
    Some((crate::parse_uint(k)?, ms))
}

fn retry_held(hold: &Hold, timing: &TurnEndTiming) -> Option<approvals::RetryMark> {
    use aterm_phase::{ApiCause, WallKind};
    let (wall, n) = hold.why.rsplit_once(" retry ")?;
    let n: usize = crate::parse_uint(n).filter(|n| *n > 0)?;
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
        down: false,
        witnessed: false,
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

/// What the reads of a turn saw, kept by the looks that end before it is judged
/// ([`Looking::moved`]).
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
    /// A live read saw the agent's own turn since the wall ([`Session::limit_on_busy`]).
    worked: bool,
    /// The wall's notice, which its badge says ([`Session::limit_retimed`]).
    message: String,
}

/// THE TURN THAT RESUMED PAST A LIMIT EPISODE `seq` CLOSED ([`Session::limit_on_busy`], or an Off
/// park's start, `Session::lower_badges`): the wall again within that turn is the same episode
/// ([`Session::open_episode`]). It lasts as long as that turn: an unattended read of a point of
/// another phase than the limit's (an Off park's included), or a settled break of the agent's
/// background work the turn ends at ([`Session::limit_at_break`]), ends it, so a wall after
/// either opens an episode of its own; so does a relaunch under another account.
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
    fn say(&mut self, line: &str) -> Result<(), String> {
        super::ledger::assert_marked(line);
        Ok(())
    }
    fn note(&mut self, line: &str) {
        super::ledger::assert_marked(line);
    }
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
    duty: duty::DutyContext,
    builtin_authority: BuiltinAuthority,
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
    /// The local zone's offset and the wall second it was read at ([`Self::local_offset_s`]).
    local_offset: Option<(i64, i64)>,
    local_offset_fixed: Option<i64>,
    zone_offset: fn(&str, i64) -> Option<i64>,
    turn_end: TurnEndState,
    turn_end_due: Option<Due>,
    clock_marks: Option<(Instant, i64)>,
    /// The wall (unix ms) [`Self::clock_marks`] held as the loop's last screen read returned
    /// ([`Self::screen`]): a live turn's read hears an answer by it (`Session::wakes_heard`), never
    /// by the mark a later call left.
    read_wall: Option<i64>,
    sleeps: SleepBook,
    sleep_count: Option<SleepStamp>,
    sleep_look: Option<FirstLook>,
    sleep_unknown: Option<&'static str>,
    sleep_unsaid: Option<&'static str>,
    slept_at: Option<i64>,
    /// The last sleep the loop measured while the worker's turn ran, until that turn's point
    /// ([`woken_turn`]).
    woken: Option<woken_turn::WokenTurn>,
    /// Claude Code's no-response retry row the busy reads show, as drawn, until a busy read shows
    /// another row or none, or the point ([`woken_turn`]).
    retry_seen: Option<String>,
    wake_settle: Option<(Due, bool)>,
    /// How many times running a wait's resume met a dark wake since an act was last let through
    /// ([`turn_end_loop::dark_wake_step`]).
    dark_gates: u32,
    /// How long those gates have held it, the Mac's sleep counted
    /// ([`turn_end_loop::DARK_WAKE_LONGEST`]).
    dark_held: Duration,
    /// How the Mac has spent the last few minutes, by the loop's own clocks ([`Self::watch_sleep`]).
    sleep_window: turn_end_loop::SleepWindow,
    restart_waits: u32,
    /// The loop's restart run ([`turn_end_loop::RestartRun`]): the kind and second its asks
    /// carry (`policy::turn_end::RestartAsk::of`), and the second the live claim of the
    /// conversation's record it holds was taken (`policy::turn_end::RestartClaim`).
    restart_run: Option<turn_end_loop::RestartRun>,
    /// THE RUN SET ASIDE, with its back-off step ([`Self::restart_run`], [`Self::restart_waits`]),
    /// of a restart beside a move onto another account
    /// ([`super::policy::turn_end::Restart::moves_account`]): the one the move's point goes on
    /// with where the host puts the move off, or one standing when the move's run took the slot
    /// (`turn_end_loop::Session::aside_for`), holding the claim it held. Between points the move's
    /// run stands in the slot and this one here; while that restart acts at the move's point, its
    /// run is back in the slot and the move's is held by the act
    /// (`turn_end_loop::Session::set_aside_run`). Each, asked again at a later point, is so the
    /// same ask on its own back-off (a run minted afresh at every point would hold the point's wait
    /// at the ladder's first step, and the move's with it), and the one set aside, asked on its
    /// own, takes the slot. It ends with the move's run where that is made or another restart's
    /// run takes its place, and at an act that is no restart unless it outlives that act by what it
    /// is and holds (`turn_end_loop::Session::outlives_act`); a refused move leaves it to the
    /// restart its point goes on with.
    restart_aside: Option<(turn_end_loop::RestartRun, u32)>,
    /// The last Unix second a restart run of this loop was minted at or restored to
    /// ([`Self::restart_run`]), seeded from the newest run's second the journal beside the ledger
    /// shows asked, which its note names by the run's ask
    /// ([`turn_end_loop::journaled_restart_floor`]): a new run is minted past it, so no two runs
    /// of this loop, or of this loop and one its journal shows, share an ask
    /// (`policy::turn_end::RestartAsk::of`).
    restart_floor: i64,
    look_at: Option<Instant>,
    /// When the last switch of the session's word a wait at rest woke on has stood
    /// [`Self::word_settle`] (v6 §14.10): the loop reads the word then and decides on it.
    word_at: Option<Instant>,
    /// [`WORD_SETTLE`], a test's own.
    word_settle: Duration,
    survey_first_seen: Option<Instant>,
    survey_zero: Option<Instant>,
    survey_told: Option<StuckSurvey>,
    survey_absent_told: bool,
    survey_hold: Option<Instant>,
    survey_kind: Option<aterm_phase::SurveyKind>,
    running: super::policy::turn_end::RunningTurn,
    wall_reset: Option<(String, Option<i64>)>,
    /// The usage tap's read of the walled process and the reset the wall waits for, from it and
    /// the notice together ([`turn_end_loop::WallTap`]); dropped with the wall.
    wall_tap: Option<turn_end_loop::WallTap>,
    /// The tap's disagreement with the notice this loop's rows wrote down last, folded from
    /// each row it writes as a loop that takes the session over folds the ledger's
    /// ([`approvals::fold_tap_said`]); kept apart from [`Self::wall_tap`], which a wall met
    /// again reads afresh.
    tap_said: Option<approvals::TapSaid>,
    /// The account the standing limit was read under ([`turn_end_loop::WallLogin`]); dropped with the wall.
    wall_login: Option<turn_end_loop::WallLogin>,
    /// Whether `host.limit_until()` observed the `agy` quota pool blocked (`until > now`) during
    /// the current wall wait, so a subsequent `Pick::Keep` (`until <= now`) proves an early pool
    /// refill rather than an unrelated pool reading.
    agy_pool_blocked: bool,
    /// When the profile was last read for a moved login during a wait ([`Session::login_edge`]).
    login_looked_at: Option<Instant>,
    stuck_asked: Option<Instant>,
    stuck_rung: wall_memory::StuckRung,
    stuck_raised: Option<(Instant, super::policy::outlook::ExceptionKind)>,
    stuck_dialog: Option<((String, wall_memory::StuckOn), Instant)>,
    stuck_still: Option<(String, Instant)>,
    reprint_jumps: u32,
    wall_answered: Option<(Instant, String)>,
    wall_memory: Option<wall_memory::WallMemory>,
    stale_open_wall: Option<approvals::OpenWall>,
    wall_verdict: Option<wall_confirm::Verdict>,
    wall_conversation: Option<(u64, super::policy::turn_end::Conversation)>,
    wall_refused_said: Option<String>,
    left_read: Option<wall_confirm::LeftRead>,
    limit_notice_row: Option<String>,
    transcript_wait_seq: Option<u64>,
    transcript_over_seq: Option<u64>,
    /// The seq of a read whose title-only run outlasted [`TITLE_HOLD`] with its reply unchanged:
    /// that read is taken as its screen alone shows it ([`Session::turn_of`]).
    title_spent_seq: Option<u64>,
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
    pub(super) first_frame: FirstFrame,
    pub(super) left_keys: Vec<String>,
    pub(super) repo_seen: Option<(Option<u64>, Instant, crate::harness::repo_work::RepoRead)>,
    /// The agent's left rows the host last read for the same done words, as [`Self::repo_seen`]
    /// keeps the repository's ([`Session::left_read`]).
    pub(super) left_seen: Option<(Option<u64>, Instant, crate::harness::leftover::LeftRead)>,
    /// A fresh repository probe read [`crate::harness::repo_work::RepoRead::Unread`], not yet journaled.
    pub(super) repo_unread_unsaid: bool,
    pub(super) left_at_wall_seq: Option<u64>,
    box_reason: Option<(String, String)>,
    changed_streak: u32,
    press_retry: Option<approval_loop::PressRetry>,
    driver: bool,
    question: approval_loop::QuestionMemory,
    decline: approval_loop::DeclineMemory,
    question_held: bool,
    question_handed_back: bool,
    /// The accounts' news came while the box of the `/login` this loop typed stood handed over
    /// (its method picker): the box is decided again, as one the roster may now name the row of
    /// ([`super::policy::login::PickerGate::Roster`]).
    login_box_news: bool,
    /// The box (its review key) the policy escalated while a look at the disk went unanswered
    /// ([`super::policy::probe::PolicyProbe::unanswered`]): handed over, it is decided again once
    /// that look ends, so a disk that was slow, not dead, has the box judged on what it says.
    box_left_out: Option<String>,
    /// The look [`Self::box_left_out`] waited on ended: the box is decided again.
    look_answered: bool,
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
    /// The person's last key the window measured ([`Self::key_ago`]): the last `status`'s
    /// `human_typed_ms` (its `human_ms` from a window that reports none), moved back by a sleep
    /// measured since ([`Self::person_slept`]).
    key: Option<Instant>,
    /// The last `status` read that saw a hand no window host holds on the tab
    /// ([`crate::harness::relaunch::outside_hand`]): another driver's key, which no later read
    /// takes back (2026-10-30 review 9), so its hour outlives the hand ([`Self::key_ago`]); a
    /// sleep measured since moves it back ([`Self::person_slept`]).
    hand_key: Option<Instant>,
    launch_status: approval_loop::LaunchStatus,
    /// The content seq of the screen whose acts wait for the worker to read input no write of
    /// this loop left, and when `status` last read it unread ([`Self::input_unread`]).
    input_unread_at: Option<(u64, Instant)>,
    /// The line this loop wrote and gave up on undrawn, held until it settles
    /// ([`turn_end_loop::Custody`]): one per session, kept past turns it outlasts and new points.
    custody: Option<turn_end_loop::Custody>,
    /// The restart in place asked for a line held in custody since the last act submitted
    /// ([`super::policy::turn_end::Restart::InputLost`]), once the host made or refused it (or
    /// answered none): a loss after it waits ([`super::custody::Restarts`]). A host's deferral
    /// (`wait:…`), and its success word while the process the line was written to is named after
    /// it ([`super::custody::Restarts::answered`]), answer nothing: the restart stands asked, so
    /// the next loss of the line still held asks again, on the restart's back-off, and a line that
    /// settles meanwhile spends it ([`super::custody::Restarts::settled`]).
    input_lost: super::custody::Restarts,
    /// How many lines this loop took into custody ([`turn_end_loop::Custody`]): the next one's
    /// count in its [`super::custody::LineAsk`].
    lines_held: u32,
    /// What a write's held-text note last read of the input (`(seq, at, word)`: the content seq
    /// the write left, when, and `status`'s `input=` word), for the custody its echo's give-up may
    /// take it into ([`Self::note_text_held`]).
    held_note: Option<(u64, Instant, String)>,
    /// When this process last took or renewed the host's hard hand for a
    /// typed act ([`Self::under_hand`]), while it holds it.
    hand_taken: Option<Instant>,
    launch_used: Option<(u64, String)>,
    /// THE COMPOSER'S TEXT, THE CURSOR'S ROW AND COLUMN, AND WHETHER A DRAFT STOOD, as the host's guards
    /// last had a word of its cell for them ([`Self::composer_for_guards`]).
    guards_told: Option<(String, usize, usize, bool)>,
    /// THE DRAFT the loop's reads found in the composer, when it first stood so, and what its glyph's
    /// cell has told of it ([`Self::see_draft`], [`Self::cell_saw`]). Whether it is a person's is
    /// [`Self::person_at`]'s to say: the loop's own words are none.
    draft_seen: Option<(String, Instant, turn_end_loop::DraftCell)>,
    /// WHAT THE PROMPT-HISTORY SEARCH SHOWED IN THE COMPOSER, read by read ([`Self::see_draft`]):
    /// while it stands, and the old prompt it left there unedited once it went, which no person
    /// typed (`Diversion::Recall`, [`Self::left_composer`]).
    searched: Option<turn_end_loop::Searched>,
    held: bool,
    rejudge: bool,
    host_held: bool,
    /// An edge (`word_edge`, `reach_edge`, `witness_edge`, …) called [`Self::turn_end_now`] while
    /// no `Due` had yet run out and [`Self::turn_end_again`] read a one-read frame of another
    /// point (`Again::Moved`): if the next look finds the handed point back on screen, decide it
    /// then without waiting on a later or absent `Due`.
    pub(super) edge_moved: bool,
    /// The point's decision was made but not carried out: the host's step at a new point comes
    /// before [`Self::turn_end_execute`] ([`Self::host_steps_here`]).
    decision_owed: bool,
    /// The last decision read the stop for repeating replies as holding the loop's typing
    /// ([`IdlePoint::replies_held`]).
    replies_stopped: bool,
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
    bound_off: bool,
    #[cfg(test)]
    last_box: Option<String>,
    #[cfg(test)]
    read_clock: Option<(Instant, u32)>,
    /// The `text` read (by [`Self::read_clock`]'s count) that took this long to answer, so every
    /// read after it is asked for that much later ([`Self::set_read_lag`]).
    #[cfg(test)]
    read_lag: Option<(u32, Duration)>,
    stepper: approval_loop::StepperTrack,
    box_proved: Option<String>,
    calls_spent: Vec<String>,
    agent_woke: Option<String>,
    /// A COMPOSER THAT HOLDS THE POINT WITH NOTHING DUE ([`Self::draft_look`]): as the decision read
    /// it. One whose read could not say ([`Composer::Unknown`]) is looked at again at each wait step
    /// too ([`Self::wait_for_next`]): a cell unread moves no content seq when it comes back.
    draft_held: Option<Composer>,
    /// WHETHER THE PERSON'S COMMAND IS WHY THE POINT IS HELD (v6 §14.17.5.8): the decision that
    /// did nothing over a command standing ([`Composer::Command`]) sends the same draft read as a
    /// prompt, so the hold's journal line names the command ([`HoldSaid::Command`]); a hold for
    /// anything else (a read that cannot say, a rest, the same reply) is said as its own.
    command_why: bool,
    /// THE TEXT A DECISION HELD THE POINT ON ([`Self::hold_on`]): that hold's, as its last look read
    /// it, or a draft a person's grace waits on, and what its glyph's cell had told of it. None where
    /// it is the loop's own line ([`Self::own_line_shown`]), which no person's grace waits on.
    held_text: Option<(String, turn_end_loop::DraftCell)>,
    /// WHEN A READ FOUND THAT TEXT GONE ([`Self::see_draft`]), which no read tells from a redraw
    /// stalled before its text: a person's mark until the next turn ([`Self::person_at`]), so nothing
    /// is typed there for a whole grace from it.
    draft_left: Option<Instant>,
    unauthoritative_held: bool,
    /// A line this loop wrote and did not submit ([`Self::left_line_look`]). Gone at the loop's next
    /// write, at another point, or once the worker works.
    left_line: Option<turn_end_loop::LeftLine>,
    /// That line's text and who wrote it ([`turn_end_loop::OwnLine`]), kept past the look that
    /// decides it again and past another point: no person's draft, and its Enter its writer's.
    /// Gone at the loop's next write, once the worker works, or (the accounts probe's `/usage`)
    /// once the probe ends.
    own_line: Option<turn_end_loop::OwnLine>,
    /// The accounts probe's write the ledger books as owed at this process's start
    /// ([`approvals::probe_wrote`]): the proof a `/usage` found standing is still that write's,
    /// weighed once ([`Self::probe_seed_claims`]).
    probe_seed: Option<approvals::ProbeWrote>,
    /// The model the handed point's footer named ([`named_model`]), or the first a read of that
    /// point named where its own read named none: the one baseline every read of the point is
    /// weighed against at a limit wall ([`Self::point_model_moved`]).
    point_model: Option<String>,
    /// A [`turn_end_loop::LeftLine::went`] wait (`(point_key, human_seq, until)`): holds writes on
    /// that point until `until`, a turn, or a person's key.
    left_went: Option<(String, Option<u64>, Instant)>,
    claim: claim::Claim,
    // Once lease support was confirmed, never downgrade retained custody to
    // the initial unsupported-host metadata path after a refusal or release.
    claim_managed: bool,
    claim_said: Vec<String>,
    claimed_at_naming: bool,
    attention_ours: Option<String>,
    attention_desired: Option<escalate::AttentionTarget>,
    /// The semantic origin retained by delayed publication and cleanup of this entry.
    attention_duty: Option<Scope>,
    attention_pending: bool,
    attention_retry_at: Option<Instant>,
    metadata_context: claim::MetadataContext,
    /// Fresh mode: unknown, recognized global override, or recognized session mode.
    metadata_mode: Option<Option<Autopilot>>,
    /// Global mode used when a fresh reply explicitly unsets the session override.
    metadata_global: Autopilot,
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
    /// The defense the land check was typed over, as [`Self::point_said`] read it there: a
    /// screen-read landing's result where the check's `LANDED` lands it.
    land_defense_said: Option<(aterm_phase::Program, String)>,
    /// What aterm checked at the landing that stands (§14.9, slice 9b): built at a fresh
    /// landing's fold, or read from the journal with the landing a loop before this one noted;
    /// gone once the face moves to open or theirs.
    land_receipt: Option<crate::harness::acts::LandReceipt>,
    /// Whether the host was last told a receipt ([`IdleHost::land_receipt`]), so a move off the
    /// landing tells it `None` once.
    receipt_told: bool,
    /// The landing's `RECEIPT` line, said right after its `DONE` note, and the landing's time.
    receipt_note: Option<(String, i64)>,
    /// The wall a fresh landing was read at, read for its receipt before the face and taken by
    /// the landing's `DONE` note ([`Self::task_edge_with`]), which read it there before.
    landing_read_ms: Option<i64>,
    task_seeded: bool,
    /// THE TASK RECORD'S READ AHEAD OF THE POINT IN HAND WAS LOST ([`Self::task_ahead`]): the
    /// point was judged as a session with no record is, and one it rests is decided again soon,
    /// reading the record again ([`turn_end_loop::RECORD_UNREAD`]).
    record_unread: bool,
    /// THE POINT IN HAND IS THIS LOOP'S FIRST ([`Self::turn_end_at_point`]), the one a decision
    /// made again at it ([`Self::turn_end_now`]) judges too: where the host's record of the
    /// conversation can trail a worker's task ([`Self::record_trails_task`]).
    point_first: bool,
    /// THE HOST'S RECORD OF THE CONVERSATION READ THE POINT IN HAND AS ONE NOBODY ASKED
    /// ([`IdleHost::taskless`]) WHILE ITS SCREEN SHOWS A CONVERSATION it does not read fresh: that
    /// record can trail the screen, so the point, decided to do nothing, is decided again once,
    /// soon ([`turn_end_loop::RECORD_TRAILS`]).
    record_contradicted: bool,
    /// Whether the point in hand was decided again for [`Self::record_contradicted`], once per point.
    record_looked_again: bool,
    /// Whether this loop tracks task records ([`SuperviseOpts::tasks`]), as its start set it.
    tasks_tracked: bool,
    /// Whether a line's write read the task record before the loop's first fold did
    /// ([`Self::managed_at_write`]).
    managed_read: bool,
    needs_told: Option<u64>,
    needs_told_text: String,
    /// A needs write lacks acknowledgement; its entry may need restoring or clearing.
    needs_pending: bool,
    /// The next metadata-only attempt at an unacknowledged needs write.
    needs_retry_at: Option<Instant>,
    needs_heard: Option<(u64, String)>,
    reach_seen: Option<Reach>,
    /// The witness on the session's route the loop asked at its last outage wall
    /// (`IdleHost::witness`, v6 §14.17.5.4), `Some` while that point's wait stands
    /// ([`Self::witness_edge`]); `None` where none was asked.
    witness_seen: Option<Option<Instant>>,
    /// The point's account advice and route move ([`IdleHost::account`],
    /// [`IdleHost::route_move`]), as last read and at which screen.
    account_seen: Option<(
        u64,
        AccountPoint,
        Option<AccountAdvice>,
        Option<super::policy::turn_end::RouteTo>,
    )>,
    roster_seen: Option<(u64, Option<super::policy::turn_end::RosterWord>)>,
    nothing_decided: bool,
    decided_withheld: bool,
    login_notice_told: Option<u32>,
    /// What the host was last told of the session's own login this turn
    /// ([`super::policy::turn_end::LoginSeen`]); a turn that runs forgets it.
    login_fact_told: Option<super::policy::turn_end::LoginSeen>,
    account_stamp: Option<u64>,
    account_news_at: Option<Instant>,
    login_seen: Option<u64>,
    /// The host was asked to discover the lost login's row before any `/login` was typed into it
    /// ([`IdleHost::login_discover`]): once per lost login.
    login_discovered: bool,
    probe: UsageProbe,
    /// What the host was last told of the probe's own panel ([`IdleHost::own_panel`]): `true`
    /// while it was told one stands.
    probe_panel_told: bool,
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
    /// The `what` and key of each sighting this loop kept a capture of
    /// ([`Session::screen_capture`]), at most `CAPTURES_EACH` of each `what`.
    captured: Vec<(String, String)>,
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
    /// The lasting withhold last told the host ([`IdleHost::lasting_hold`]); `None` before this
    /// loop told any, so its first tells what the host holds from a loop before it.
    lasting_told: Option<Option<Lasting>>,
    hold_said: Option<HoldSaid>,
    hold: occupancy_loop::HoldWatch,
    facts: facts_loop::FactsWatch,
    /// Cruise's carries at breaks of the agent's own background work ([`Self::cruise_carry`]).
    cruise_carry: cruise_carry_loop::CarryWatch,
    stall_suspect: bool,
    /// A turn-end act at the handed point was withheld for [`Self::stall_suspect`] before
    /// [`Self::hold_for_stall`] held it: if the next probe finds no stall, re-execute it.
    stall_withheld: bool,
    /// An ask badge withheld for [`Self::stall_suspect`] before [`Self::hold_for_stall`] held it.
    withheld_ask: Option<escalate::WithheldAsk>,
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
    /// Constructs a [`Session`] targeting `sid` (`None`: no selector, so the instance's active tab) with default [`SessionSetup`].
    pub fn new(ctl: &'a mut C, sid: Option<String>) -> Self {
        Self::from_setup(ctl, SessionSetup::new(sid))
    }

    /// Constructs a [`Session`] from an explicit [`SessionSetup`].
    pub fn from_setup(ctl: &'a mut C, setup: SessionSetup) -> Self {
        let fx = setup.live_fx();
        Self {
            ctl,
            duty: duty::DutyContext::default(),
            builtin_authority: BuiltinAuthority::Unsupported,
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
            fx,
            local_offset: None,
            local_offset_fixed: None,
            zone_offset: limit::zone_offset_at,
            turn_end_due: None,
            clock_marks: None,
            read_wall: None,
            sleeps: SleepBook::default(),
            sleep_count: None,
            sleep_look: None,
            sleep_unknown: None,
            sleep_unsaid: None,
            slept_at: None,
            woken: None,
            retry_seen: None,
            wake_settle: None,
            dark_gates: 0,
            dark_held: Duration::ZERO,
            sleep_window: turn_end_loop::SleepWindow::default(),
            restart_waits: 0,
            restart_run: None,
            restart_aside: None,
            restart_floor: 0,
            look_at: None,
            word_at: None,
            word_settle: WORD_SETTLE,
            survey_first_seen: None,
            survey_zero: None,
            survey_told: None,
            survey_absent_told: false,
            survey_hold: None,
            survey_kind: None,
            running: super::policy::turn_end::RunningTurn::default(),
            wall_reset: None,
            wall_tap: None,
            tap_said: None,
            wall_login: None,
            agy_pool_blocked: false,
            login_looked_at: None,
            reprint_jumps: 0,
            wall_answered: None,
            stuck_asked: None,
            stuck_rung: wall_memory::StuckRung::Quiet,
            stuck_raised: None,
            stuck_dialog: None,
            stuck_still: None,
            wall_memory: None,
            stale_open_wall: None,
            wall_verdict: None,
            wall_conversation: None,
            wall_refused_said: None,
            left_read: None,
            limit_notice_row: None,
            transcript_wait_seq: None,
            transcript_over_seq: None,
            title_spent_seq: None,
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
            first_frame: FirstFrame::Owed { held: false },
            left_keys: Vec::new(),
            repo_seen: None,
            left_seen: None,
            repo_unread_unsaid: false,
            left_at_wall_seq: None,
            box_reason: None,
            changed_streak: 0,
            press_retry: None,
            driver: false,
            question: approval_loop::QuestionMemory::default(),
            decline: approval_loop::DeclineMemory::default(),
            question_held: false,
            question_handed_back: false,
            login_box_news: false,
            box_left_out: None,
            look_answered: false,
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
            key: None,
            hand_key: None,
            launch_status: approval_loop::LaunchStatus::default(),
            input_unread_at: None,
            custody: None,
            input_lost: super::custody::Restarts::default(),
            lines_held: 0,
            held_note: None,
            hand_taken: None,
            launch_used: None,
            guards_told: None,
            draft_seen: None,
            searched: None,
            held: false,
            rejudge: false,
            host_held: false,
            edge_moved: false,
            decision_owed: false,
            replies_stopped: false,
            host_passed: None,
            host_follows: None,
            agent_woke: None,
            draft_held: None,
            command_why: false,
            held_text: None,
            draft_left: None,
            unauthoritative_held: false,
            left_line: None,
            own_line: None,
            probe_seed: None,
            point_model: None,
            left_went: None,
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
            bound_off: false,
            #[cfg(test)]
            last_box: None,
            #[cfg(test)]
            read_clock: None,
            #[cfg(test)]
            read_lag: None,
            stepper: approval_loop::StepperTrack::default(),
            box_proved: None,
            calls_spent: Vec::new(),
            claim: claim::Claim::Off,
            claim_managed: false,
            claim_said: Vec::new(),
            claimed_at_naming: false,
            attention_ours: None,
            attention_desired: None,
            attention_duty: None,
            attention_pending: false,
            attention_retry_at: None,
            metadata_context: claim::MetadataContext::Normal,
            metadata_mode: None,
            metadata_global: Autopilot::default(),
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
            land_defense_said: None,
            land_receipt: None,
            receipt_told: false,
            receipt_note: None,
            landing_read_ms: None,
            task_seeded: false,
            record_unread: false,
            point_first: false,
            record_contradicted: false,
            record_looked_again: false,
            tasks_tracked: false,
            managed_read: false,
            needs_told: None,
            needs_told_text: String::new(),
            needs_pending: false,
            needs_retry_at: None,
            needs_heard: None,
            reach_seen: None,
            witness_seen: None,
            account_seen: None,
            roster_seen: None,
            nothing_decided: false,
            decided_withheld: false,
            login_notice_told: None,
            login_fact_told: None,
            account_stamp: None,
            account_news_at: None,
            login_seen: None,
            login_discovered: false,
            probe: UsageProbe::Idle,
            probe_panel_told: false,
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
            captured: Vec::new(),
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
            lasting_told: None,
            hold_said: None,
            hold: occupancy_loop::HoldWatch::default(),
            facts: facts_loop::FactsWatch::default(),
            cruise_carry: cruise_carry_loop::CarryWatch::default(),
            stall_suspect: false,
            stall_withheld: false,
            withheld_ask: None,
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

    /// Enable the private built-in adapter's recorded scoped-duty protocol. Ordinary control
    /// adapters retain their legacy all-or-nothing claim behavior unless explicitly enabled.
    pub const fn set_builtin_duties(&mut self, enabled: bool) {
        self.setup.builtin_duties = enabled;
    }

    /// The window's host confirms the session's agent by its image ([`Ctl::agent_act`]): the
    /// loop types only while the host says [`Act::On`], and observes otherwise.
    pub const fn set_confirms_agent(&mut self, on: bool) {
        self.setup.confirms_agent = on;
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

    /// Overrides [`WORD_SETTLE`] for tests (no tape's replay knows another).
    #[cfg(test)]
    pub(crate) fn set_word_settle(&mut self, settle: Duration) {
        self.word_settle = settle;
        if settle != WORD_SETTLE {
            self.seeds_learned_state("a test's own word settle");
        }
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

    /// Reads the clocks of a host that never sleeps from now (`fx::AwakeClock`): a test's loop
    /// over a real instance then counts none of its machine's sleep, which on a Mac with its lid
    /// closed would put a wake to settle into every wait. The setup keeps it
    /// (`SessionSetup::live_fx`), so a loop rebuilt from it, a hosted run's recording one among
    /// them, reads the same.
    #[cfg(any(test, debug_assertions))]
    pub fn set_host_awake(&mut self) {
        self.setup.host_awake = true;
        self.fx.awake();
    }

    /// Overrides the re-check interval while held for a person in tests.
    #[cfg(test)]
    pub(crate) fn set_person_hold_wait(&mut self, wait: Duration) {
        self.setup.person_hold_wait = Some(wait);
    }

    /// Overrides how many looks a due line of the occupancy door stands untold before it is said
    /// ready-idle, in tests.
    #[cfg(test)]
    pub(crate) fn set_occupancy_k_looks(&mut self, k: i64) {
        self.setup.occupancy_k_looks = k;
    }

    /// Sets whether the occupancy door's `wait-nudge` names `wake`, as a build before it named it
    /// recorded, in tests.
    #[cfg(test)]
    pub(crate) fn set_occupancy_nudge_wake(&mut self, named: bool) {
        self.setup.occupancy_nudge_wake = named;
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

    /// Holds an untouched permission box for a person's whole grace, as before its bound: the
    /// bound's negative control.
    #[cfg(test)]
    pub(crate) fn set_bound_off(&mut self) {
        self.bound_off = true;
        self.seeds_learned_state("the loop before the untouched box's bound");
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
        let lag = self
            .read_lag
            .filter(|(slow, _)| reads > *slow)
            .map_or(Duration::ZERO, |(_, lag)| lag);
        Some(set + Duration::from_secs(u64::from(reads.saturating_sub(1))) + lag)
    }

    /// On [`Self::set_read_clock`]'s clock, the `text` read numbered `read` (from 1) takes `lag`
    /// to answer: every read after it is asked for `lag` later.
    #[cfg(test)]
    pub(crate) fn set_read_lag(&mut self, read: u32, lag: Duration) {
        self.read_lag = Some((read, lag));
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

    /// Overrides the settle delay before answering an autocompact or auto-mode setup offer.
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
        if let Some(operation) = claim::attention_operation(args)
            && (self.handing_over() || (operation == "set" && self.stopped()))
        {
            return Err(Fail::Hard(
                "the watcher stopped before the attention write".to_string(),
            ));
        }
        if self.metadata_context != claim::MetadataContext::Normal
            && (claim::is_input(args)
                || self.handing_over()
                || (self.stopped() && self.metadata_context == claim::MetadataContext::Retry))
        {
            return Err(Fail::Lost(
                "the metadata-only attempt cannot send this request".to_string(),
            ));
        }
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
        if self.metadata_context == claim::MetadataContext::Normal {
            if self.setup.builtin_duties && claim::is_write(args) {
                self.refresh_builtin_authority();
            }
            self.renew_claim_if_due();
        }
        if let Some(refused) = self.claim_refuses(args) {
            return Ok(refused);
        }
        if let Some(refused) = self.observe_refuses(args) {
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
        let r = match (self.duty.get(), self.duty.revision()) {
            (Some(duty), Some(revision)) if self.setup.builtin_duties && claim::is_write(args) => {
                self.ctl.call_for_duty(duty, revision, &full).unwrap_or_else(|| Ok(CtlReply {
                    code: 1, stdout: String::new(),
                    stderr: "aterm-ctl: ERR denied supervisor input authority (typed adapter unavailable)\n".to_string(),
                }))
            }
            _ => self.ctl.call(&full),
        };
        self.watch_sleep();
        let r = r.map_err(Fail::Hard)?;
        self.note_status(args, &r);
        self.cruise_carry.wrote(args, &r);
        if claim::is_input(args) && !(r.is_err("halted") || r.is_err("busy") || r.is_err("rate")) {
            self.refusal_said = None;
        }
        if claim::is_input(args) && r.warned_input_unread() {
            self.stall_suspect = true;
        }
        self.note_exchange(args, Some(&r));
        Ok(r)
    }

    /// A READ THAT ONLY OBSERVES: `args` (a read, never a write) sent to the session as
    /// [`Self::call`] sends it, but nothing the loop decides by learns from its reply: no stall
    /// suspicion or input capability ([`Self::note_status`]), no outage opened or cleared
    /// ([`Self::note_exchange`]). The held text's `NOTE` reads `status` so (`note_text_held`), and a
    /// write's echo goes on to its hang exactly as it would without the read.
    pub(super) fn call_observed(&mut self, args: &[&str]) -> Result<CtlReply, Fail> {
        debug_assert!(
            !claim::is_write(args),
            "an observed read writes nothing: {args:?}"
        );
        let mut full: Vec<&str> = Vec::with_capacity(args.len() + 1);
        if let Some(sid) = &self.setup.sid {
            full.push(sid.as_str());
        }
        full.extend_from_slice(args);
        let r = self.ctl.call(&full);
        self.watch_sleep();
        r.map_err(Fail::Hard)
    }

    /// Tracks the failed request's outage window, including for direct metadata reads that bypass
    /// ordinary claim renewal. `None` is a transport loss.
    pub(super) fn note_exchange(&mut self, args: &[&str], reply: Option<&CtlReply>) {
        let kind = || args[..args.len().min(2)].join(" ");
        if reply.is_none_or(|r| self.unserved(r)) {
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
                o.kind == kind || (kind == "await seq" && reply.is_some_and(CtlReply::ok))
            })
        {
            self.outage = None;
        }
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
            Program::Claude
            | Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse => false,
        };
        if self.program.is_none()
            && no_grammar
            && aterm_phase::parse_prompt_v2(rows).is_some_and(|p| p.head_off_screen)
        {
            return &aterm_phase::ClaudeReader;
        }
        by_screen
    }

    /// Whether the agent's first frame is still owed on `screen` for a hosted loop: no read since
    /// the loop began showed anything the agent drew, the screen could be the shell's alone
    /// ([`aterm_phase::shell_alone`]), and the agent's reader finds nothing of its own on it
    /// ([`aterm_phase::ScreenReader::program_drawn`]). The agent is the one `status` last named,
    /// else the task's `worker=`, else the one the screen names; when nothing names one (an
    /// agent run by `node`), every reader that knows its program's frame is asked. Once a
    /// read shows the agent's frame it stays drawn. A loop with no host owes none. Once the hold
    /// ran out ([`FirstFrame::RanOut`]) nothing is owed, and a read the agent drew on is drawn.
    fn first_frame_owed(&mut self, screen: &Screen, hosted: bool) -> bool {
        if matches!(self.first_frame, FirstFrame::Drawn { .. }) {
            return false;
        }
        let drawn = self.agent_drew(&screen.rows, hosted);
        if drawn {
            self.first_frame = FirstFrame::Drawn {
                held: self.first_frame.saw_start(),
            };
        }
        !drawn && matches!(self.first_frame, FirstFrame::Owed { .. })
    }

    /// Whether the point read on `rows` is the agent's launch, no turn of its own: the hold for
    /// its first frame ran out on an unmoved screen ([`FirstFrame::RanOut`]), no read since showed
    /// the agent's frame ([`Self::first_frame_owed`]), and `rows` show nothing the agent drew.
    pub(super) fn launch_unmet(&self, rows: &[String], hosted: bool) -> bool {
        matches!(self.first_frame, FirstFrame::RanOut { .. }) && !self.agent_drew(rows, hosted)
    }

    /// Whether `rows` show anything the agent drew, for [`Self::first_frame_owed`].
    fn agent_drew(&self, rows: &[String], hosted: bool) -> bool {
        let named = self
            .program
            .as_deref()
            .and_then(aterm_phase::program_of)
            .or(self.worker_agent);
        !hosted
            || !aterm_phase::shell_alone(rows)
            || match named.unwrap_or_else(|| aterm_phase::identify(None, rows).program()) {
                Program::Generic => Program::ALL.iter().any(|p| {
                    let knows_its_frame = match p {
                        Program::Claude | Program::Agy | Program::Muse => true,
                        Program::Codex | Program::Grok | Program::Kimi | Program::Generic => false,
                    };
                    knows_its_frame && p.reader().program_drawn(rows)
                }),
                agent => agent.reader().program_drawn(rows),
            }
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
        let mut phase = phase_of(reader, &screen);
        if phase == Phase::Busy
            && self.title_spent_seq == Some(screen.seq)
            && title_run(reader, &screen)
        {
            phase = reader.phase(&screen.rows);
        }
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
        match phase_of(reader, &screen) {
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
        self.read_wall = self.clock_marks.map(|(_, wall_ms)| wall_ms);
        self.fx.screen_read(&screen);
        #[cfg(test)]
        let asked = self.read_clock_now().unwrap_or(asked);
        self.keep_countdown(&screen.rows, asked);
        self.keep_episode(&screen, self.last_read_count, asked);
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
            self.busy_read = phase_of(reader, &screen) == Phase::Busy;
        }
        let credits = reader.credits(&screen.rows);
        self.note_credits(reader, credits.as_ref(), &screen.rows);
        self.note_usage(reader, credits, &screen.rows);
        self.see_draft(reader, &screen);
        let mut rows = self.last.take().map(|l| l.rows).unwrap_or_default();
        rows.clone_from(&screen.rows);
        self.last = Some(Screen {
            rows,
            generation: screen.generation.clone(),
            launch: screen.launch.clone(),
            title: screen.title.clone(),
            ..screen
        });
        Ok(screen)
    }

    /// THE DRAFT `screen`'S ROWS SHOW ([`Self::draft_seen`]), by its rows and the cursor alone
    /// ([`draft_read`]), no cell read: what every read keeps until a read that matters asks the
    /// glyph's cell, a decision's or a step of the host's ([`Self::composer_typed`]). The same text
    /// keeps its first sight whichever way it is read: a caret that went home is told from a
    /// suggestion first read with the cursor past it only by that cell. Other text never does. It is
    /// another draft, as a person's edit of its whitespace is, and a window's reflow, which no read
    /// tells from one; a draft that comes back after it has a grace of its own, which only stretches
    /// the hold.
    ///
    /// Text a decision held the point on ([`Self::held_text`]), at this point or the last, read gone
    /// (the composer empty, or other text the rows do not call typed), leaves a person's mark
    /// ([`Self::draft_left`]): that may be a frame read before the text was drawn again, and nothing
    /// is typed there for a whole grace. Words that begin with text no read has said typed say it was
    /// a suggestion all along, drawn in part or read before its cursor went home, so they leave none;
    /// text a read has said typed was no suggestion, whatever words follow it. Nor does the loop's own
    /// line going, which no decision holds the point on. A frame with no composer drawn neither
    /// marks nor forgets: the read after it says.
    ///
    /// So too what a prompt-history search showed there ([`Self::searched`]): the text its row last
    /// showed while it stood, kept once it went while the composer still shows it, unless the
    /// composer held that text before it opened (a query that matched nothing over a draft leaves
    /// the draft).
    fn see_draft(&mut self, reader: &dyn ScreenReader, screen: &Screen) {
        let composer = reader.composer(&screen.rows);
        let typed = draft_read(composer.as_ref(), reader.placeholder(), screen);
        let draft = composer.as_ref().map(|(_, lines)| lines.join("\n"));
        if let (Some((caret, _)), Some(shown)) = (composer.as_ref(), draft.as_deref()) {
            // What the composer held before: the last read's text (`draft_seen`, not yet moved on
            // by this one) where a read said it typed, by its glyph's cell or, no cell asked, by
            // the rows and the cursor (`DraftCell::Unasked`): no grey suggestion read with the
            // cursor home. A suggestion read with the cursor past it, as a frame read before its
            // program placed the cursor could show it, would pass; no measured frame shows one
            // (docs/measured/draft-hold-findings-2026-11-04.md, §2).
            let typed_before = || {
                self.draft_seen
                    .as_ref()
                    .filter(|(.., cell)| *cell != turn_end_loop::DraftCell::Unsaid)
                    .map(|(text, ..)| text.clone())
            };
            let was = self.searched.take();
            let open = matches!(was, Some(turn_end_loop::Searched::Open { .. }));
            // The search opens where its own row stands (the reader's `history_search`) with the
            // cursor under the composer, not on the cursor alone: a read inside Muse Code's
            // unbracketed redraw finds a whole composer with the cursor still on the footer
            // (docs/measured/half-drawn-frame-2026-10-22.md). Once open, the cursor keeps it so.
            let under = screen.cursor_under(*caret);
            let opens = under && !open && reader.history_search(&screen.rows);
            let before = opens.then(typed_before).flatten();
            self.searched =
                turn_end_loop::Searched::seen(was, shown, under && (open || opens), before);
        }
        let seen = self.draft_seen.take();
        self.draft_seen = match (typed, draft) {
            (_, Some(shown)) if seen.as_ref().is_some_and(|(text, ..)| *text == shown) => seen,
            (Some(true), Some(shown)) => {
                Some((shown, self.fx.mono(), turn_end_loop::DraftCell::Unasked))
            }
            (_, Some(shown)) => {
                if self.held_text.take().is_some_and(|(text, cell)| {
                    cell == turn_end_loop::DraftCell::Typed || !shown.starts_with(&text)
                }) {
                    self.draft_left = Some(self.fx.mono());
                }
                None
            }
            (_, None) => None,
        };
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
            week: reader.model_week(rows),
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
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
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

    /// What the program's OSC 0/2 window title says of its turn: `Some(true)` running,
    /// `Some(false)` at rest, `None` where its reader reads no titles, the host refuses `title`, or
    /// the program set none (Claude Code under `CLAUDE_CODE_DISABLE_TERMINAL_TITLE`), so the
    /// title tells nothing.
    /// The title read with `screen`'s rows (`text --json`'s `"title"`) is the one asked; a host
    /// that sends none (an older build) is asked with `title`.
    fn title_reading(
        &mut self,
        reader: &dyn ScreenReader,
        screen: &Screen,
    ) -> Result<Option<bool>, Fail> {
        if reader.title_runs("").is_none() {
            return Ok(None);
        }
        if let Some(title) = screen.title.as_deref() {
            return Ok(reader.runs(Some(title)));
        }
        let r = self.call(&["title"])?;
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("title failed: {}", r.stderr.trim())));
        }
        if !r.ok() {
            return Ok(None);
        }
        let title = r.stdout.trim_end_matches(['\r', '\n']);
        let title = title.strip_prefix("OK").unwrap_or(title);
        let title = title.strip_prefix(' ').unwrap_or(title);
        if title.is_empty() {
            return Ok(None);
        }
        Ok(reader.title_runs(title))
    }

    fn wait(&mut self, cond: &[&str], step: Duration) -> Result<Wait, Fail> {
        self.wait_at(cond, step).map(|(wait, _)| wait)
    }

    /// Executes `await` for `cond` and returns the wait outcome along with any latched content sequence.
    fn wait_at(&mut self, cond: &[&str], step: Duration) -> Result<(Wait, Option<u64>), Fail> {
        self.wait_on(cond, step, None)
    }

    /// A wait at rest (THE WORD EDGE, v6 §14.10): where `latch` holds and the host takes it, `cond`
    /// carries the session's stored word ([`Self::word_latch`]), so a switch of it wakes the wait
    /// as [`Wait::Word`]; an act's own waits never carry it.
    fn park(&mut self, cond: &[&str], step: Duration, latch: bool) -> Result<Wait, Fail> {
        let word = if latch { self.word_latch()? } else { None };
        self.wait_on(cond, step, word.as_deref())
            .map(|(wait, _)| wait)
    }

    /// The `autopilot=<word>` a wait at rest carries: the stored word last read or reported (`-`
    /// unset), never the table's, which would latch a session with none on every wait. `None`
    /// where `meta` names no word or the host refused the token (an older host, [`Self::wait_on`]).
    fn word_latch(&mut self) -> Result<Option<String>, Fail> {
        let Some(word) = self.autopilot.latch() else {
            return Ok(None);
        };
        Ok((self.caps.await_autopilot != Some(false)).then(|| format!("autopilot={word}")))
    }

    /// A WAIT INSIDE THE AGENT'S TURN carries the session's word as a wait at rest does
    /// ([`Self::park`]), so a switch made while the agent works is noted at once
    /// ([`Self::word_moved`]) and the point the turn ends on is decided on the word read then
    /// ([`Self::decide_to_act_now`]), never on the one the turn began under. Nothing is typed into
    /// the turn: the wait goes on, carrying the word reported, for what is left of `step`.
    fn busy_wait(
        &mut self,
        cond: &[&str],
        step: Duration,
        review: &mut dyn Review,
    ) -> Result<Wait, Fail> {
        let until = self.fx.mono() + step;
        for _ in 0..BUSY_WORD_WAKES {
            let left = until.saturating_duration_since(self.fx.mono());
            match self.park(cond, left, true)? {
                Wait::Word(word) => {
                    let seq = self.last.as_ref().map_or(0, |s| s.seq);
                    self.word_moved(seq, word, review);
                }
                wait => return Ok(wait),
            }
        }
        Ok(Wait::TimedOut)
    }

    /// [`Self::wait_at`], `word` the `autopilot=` token appended after the timeout ([`Self::park`]).
    fn wait_on(
        &mut self,
        cond: &[&str],
        step: Duration,
        word: Option<&str>,
    ) -> Result<(Wait, Option<u64>), Fail> {
        let ms = step.as_nanos().div_ceil(1_000_000).to_string();
        let mut args = vec!["await"];
        args.extend_from_slice(cond);
        args.push("timeout");
        args.push(&ms);
        args.extend(word);
        let mut r = self.call(&args)?;
        // THE FIRST WAIT THAT CARRIES THE TOKEN ASKS WHETHER THE HOST TAKES IT, no read of its own
        // (a recorded tape answers no read it never made). A shipped host before slice 10a reads
        // only the arguments it knows and answers as ever, so the token rides on harmlessly; one
        // that refused it as a usage error gets the same wait bare, and no wait carries it again.
        // A wait refused bare too was refused for something else, and is answered as it always
        // was.
        let mut word = word;
        if word.is_some() && self.caps.await_autopilot.is_none() {
            if r.usage_error() {
                args.pop();
                let bare = self.call(&args)?;
                if !bare.usage_error() {
                    self.caps.await_autopilot = Some(false);
                }
                (r, word) = (bare, None);
            } else if !self.unserved(&r) {
                self.caps.await_autopilot = Some(true);
            }
        }
        let mut reply = r.stdout.split_whitespace().skip(1);
        if r.ok() && word.is_some() && reply.next() == Some("autopilot") {
            let stored = reply.next().unwrap_or("-");
            let read = Autopilot::ALL.into_iter().find(|a| a.word() == stored);
            if read.is_none() && stored != "-" {
                // A word this build cannot name would wake every wait carrying the old one.
                self.caps.await_autopilot = Some(false);
                return Ok((Wait::Latched, None));
            }
            return Ok((Wait::Word(read), None));
        }
        if r.ok() {
            let latched = r
                .stdout
                .split_whitespace()
                .nth(2)
                .and_then(crate::parse_uint);
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

    fn wait_gone(
        &mut self,
        busy: &BusyWait,
        step: Duration,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let span = busy.span.map(|(a, b)| (a.to_string(), b.to_string()));
        let mut cond = vec!["gone", busy.guard.as_str()];
        if let Some((a, b)) = &span {
            cond.extend(["rows", a.as_str(), b.as_str()]);
        }
        let waited = match self.busy_wait(&cond, step, review) {
            Ok(Wait::Unsupported) => Err(Fail::Hard(format!(
                "await {} is not known to this host",
                cond.join(" ")
            ))),
            other => other.map(drop),
        };
        match waited {
            Err(Fail::Hard(e)) if busy.span.is_some() && e.contains("ERR bad rows") => Ok(()),
            other => other.map(drop),
        }
    }

    fn await_gone(&mut self, cond: &[&str], step: Duration) -> Result<bool, Fail> {
        match self.wait(cond, step)? {
            Wait::Latched | Wait::Word(_) => Ok(true),
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
                phase: phase_of(self.reader(&screen.rows), screen),
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
        // An idle read held under a running title: what its reply said
        // ([`ScreenReader::said_tail`]), and since when it has said it. A reply still drawing
        // restarts it; a status line or a footer timer moving under the same reply does not, so
        // [`TITLE_HOLD`] of the reply unchanged ends the hold.
        let mut title_held: Option<(Option<String>, Instant)> = None;
        // A turn only the title runs (`Next::Run`): what its reply said
        // ([`ScreenReader::said_tail`]), and since when it has said it. A reply still drawing
        // restarts it, so it is never cut short; a status line or a footer timer moving under the
        // same reply does not, so [`TITLE_HOLD`] of the reply unchanged ends it, and the read is
        // taken as its screen shows it.
        let mut title_run_since: Option<(Option<String>, Instant)> = None;
        // A title read in this wait found the program at rest, with no busy read after it.
        let mut title_rested = false;
        let mut unvouched_since: Option<Instant> = None;
        // The last read held as still starting (its seq), and since when ([`FIRST_FRAME_HANG`]).
        let mut first_frame_still: Option<(u64, Instant)> = None;
        let mut settle_cap = SETTLE_CAP;
        // The session log's cursor a timed-out log wait left standing ([`Self::log_wake`]).
        let mut log_at: Option<log_wake::LogHead> = None;
        loop {
            // A stop ends the turn, however long it has run, as spent ([`Self::spent_turn`]). It
            // is checked here and before each of the turn's other waits, so a stop in any wait
            // ends the turn rather than surfacing as the refusal of its next wait ([`Self::call`]).
            if self.stopped() {
                return Ok(self.spent_turn());
            }
            let step = deadline
                .saturating_duration_since(self.fx.mono())
                .min(self.setup.wait_step);
            let mut unsettled = false;
            let mut settled = false;
            // Once a read showed the turn running (`footer_seen`), the log may wake the wait for
            // its end ([`log_wake`]); the read after it still decides. A cursor stands only from
            // one such wait to the next.
            let on_the_log = footer_seen && matches!(next, Next::Gone(_) | Next::Run);
            if !on_the_log {
                log_at = None;
            }
            match &next {
                Next::Gone(busy) => {
                    let woke = if on_the_log {
                        self.log_wake(&mut log_at, busy.step(step))?
                    } else {
                        log_wake::LogWoke::Screen
                    };
                    match woke {
                        log_wake::LogWoke::Screen => {
                            self.wait_gone(busy, busy.step(step), review)?;
                        }
                        log_wake::LogWoke::Word(word) => {
                            let seq = self.last.as_ref().map_or(0, |s| s.seq);
                            self.word_moved(seq, word, review);
                        }
                        log_wake::LogWoke::Answered | log_wake::LogWoke::TimedOut => {}
                    }
                    unsettled = true;
                }
                Next::Settle => {
                    let cap = std::mem::replace(&mut settle_cap, SETTLE_CAP);
                    self.busy_wait(&["idle", SETTLE_MS], step.min(cap), review)?;
                    unsettled = true;
                }
                Next::Idle => {
                    settled = matches!(
                        self.busy_wait(&["idle", IDLE_MS], step.min(WRITING_CAP), review)?,
                        Wait::Latched
                    );
                }
                Next::Moved(seq) => {
                    self.busy_wait(&["seq", &seq.to_string()], step, review)?;
                    unsettled = true;
                }
                Next::Run => {
                    let woke = if on_the_log {
                        self.log_wake(&mut log_at, step)?
                    } else {
                        log_wake::LogWoke::Screen
                    };
                    match woke {
                        log_wake::LogWoke::Screen => {
                            self.busy_wait(&["idle", SETTLE_MS], step.min(SETTLE_CAP), review)?;
                        }
                        log_wake::LogWoke::Word(word) => {
                            let seq = self.last.as_ref().map_or(0, |s| s.seq);
                            self.word_moved(seq, word, review);
                        }
                        log_wake::LogWoke::Answered | log_wake::LogWoke::TimedOut => {}
                    }
                    unsettled = true;
                }
            }
            let mut screen = self.screen()?;
            let reader = self.reader(&screen.rows);
            if self.fx.mono() < deadline {
                self.watch_context(&screen, reader, &mut |line| review.say(line))?;
            }
            let mut phase = phase_of(reader, &screen);
            let runs = reader.runs(screen.title.as_deref());
            // A turn only the title runs holds while its reply changes, and TITLE_HOLD of the
            // reply unchanged it is read as the screen shows it, the hold's NOTE said below.
            let title_only = phase == Phase::Busy && title_run(reader, &screen);
            if title_only {
                let said = reader.said_tail(&screen.rows);
                let since = match &title_run_since {
                    Some((held, at)) if *held == said => *at,
                    _ => self.fx.mono(),
                };
                if self.fx.since(since) >= TITLE_HOLD {
                    phase = reader.phase(&screen.rows);
                    self.title_spent_seq = Some(screen.seq);
                    title_held = Some((said.clone(), since));
                }
                title_run_since = Some((said, since));
            } else {
                title_run_since = None;
            }
            // A program still starting is neither idle nor busy. Until the agent has drawn
            // something of its own ([`Self::first_frame_owed`]), the loop waits for the screen
            // to move and reads again. A screen unmoved for FIRST_FRAME_HANG ends the hold, and
            // until the agent draws, a point is its launch's ([`Self::launch_unmet`]). A box or a
            // wall skips the hold: the agent drew it.
            if !matches!(phase, Phase::Prompt | Phase::Limited { .. })
                && self.fx.mono() < deadline
                && self.first_frame_owed(&screen, opts.is_some_and(|o| o.idle_host.is_some()))
            {
                let since = match first_frame_still {
                    Some((seq, since)) if seq == screen.seq => since,
                    _ => self.fx.mono(),
                };
                if self.fx.since(since) < FIRST_FRAME_HANG {
                    first_frame_still = Some((screen.seq, since));
                    self.first_frame = FirstFrame::Owed { held: true };
                    next = Next::Moved(screen.seq);
                    footer_seen = false;
                    continue;
                }
                self.first_frame = FirstFrame::RanOut {
                    held: self.first_frame.saw_start(),
                };
                review.note(&format!(
                    "NOTE seq={} the agent drew nothing of its own on a screen that stood {} s \
                     unmoved: read as it stands",
                    screen.seq,
                    FIRST_FRAME_HANG.as_secs()
                ));
            }
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
                // A read torn mid-repaint is no point. A reply growing under the window is
                // repainted row by row with its footer last, so a read just after the busy
                // footer left can show the composer frame and no footer while the reply still
                // streams. Such an idle read whose frame its reader judges torn
                // ([`ScreenReader::frame_torn`]) is read once more after the short settle, and
                // that read decides. A frame bracketed in DEC 2026 never gets here unfinished,
                // since the read waits for it to close ([`Self::text_json`]); for an unbracketed
                // one the server's `gone` waits out [`aterm_core::terminal::GONE_SETTLE`].
                if phase == Phase::Idle
                    && footer_seen
                    && matches!(next, Next::Gone(_) | Next::Run)
                    && reader.frame_torn(&screen.rows)
                    && self.fx.mono() < deadline
                {
                    next = Next::Settle;
                    footer_seen = false;
                    continue;
                }
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
                // The program's own title vouches for an end its screen cannot: under a status
                // line Claude Code draws its reply with no busy sign (no `esc to interrupt`, its
                // spinner row gone once the reply's first line is drawn), and Muse Code shows its
                // last reply before its run ends. A read under a running title is held and read
                // again after the short settle. A title at rest vouches for an end only on a read
                // after it: Claude Code rests its title about 23 ms before the frame that draws
                // the reply's last line, so a screen read just before that flip under a title read
                // just after it is a reply short of its last line. That holds for a wait's first
                // read too (one taken up on a reply already drawing, or after an outage), and the
                // screen cannot say whether a status line hides the busy sign, since Claude Code
                // drops `esc to interrupt` for a configured status line even where its command
                // draws no row. So the first read at rest under a title at rest takes the short
                // settle and the read after it decides. An end the program drew (Claude Code's
                // done row, which comes with that last line or after it, or the interrupt a
                // stopped turn ends on, as `title_live` reads it), a wall, and an end the
                // transcript told decide as read: the title also runs while the agent's background
                // agents or workflows do, its turn over. A health wall (Claude Code's
                // critical-memory banner) is no such wall: the program draws it over a live turn
                // too, and the restart it asks for waits for the reply's last line like any other
                // end. A program with no reading of its titles, or a title never set, says nothing
                // and pays nothing.
                if matches!(phase, Phase::Idle | Phase::Question)
                    && !transcript_idle
                    && !reader.end_drawn(&screen.rows)
                    && !reader.interrupted(&screen.rows)
                    && reader.wall(&screen.rows) == reader.health_wall(&screen.rows)
                    && self.fx.mono() < deadline
                {
                    let said = reader.said_tail(&screen.rows);
                    match &title_held {
                        Some((held, at)) if *held == said && self.fx.since(*at) >= TITLE_HOLD => {
                            review.note(&format!(
                                "NOTE seq={} the agent's title still said its run goes on, its \
                                 reply unchanged {} s: its end read as the screen shows it",
                                screen.seq,
                                TITLE_HOLD.as_secs()
                            ));
                        }
                        _ => match self.title_reading(reader, &screen)? {
                            Some(true) => {
                                if title_held.as_ref().is_none_or(|(held, _)| *held != said) {
                                    title_held = Some((said, self.fx.mono()));
                                }
                                title_rested = false;
                                next = Next::Settle;
                                footer_seen = false;
                                continue;
                            }
                            Some(false) if !title_rested => {
                                title_rested = true;
                                next = Next::Settle;
                                footer_seen = false;
                                continue;
                            }
                            Some(false) | None => {}
                        },
                    }
                }
                return Ok(Turn {
                    phase,
                    screen,
                    timed_out: false,
                });
            }
            unvouched_since = None;
            if !title_only {
                title_held = None;
            }
            title_rested = false;
            if writing {
                phase = Phase::Busy;
            }
            let may_act = !writing
                && ((self.stall_host.is_some()
                    && (carried || reader.background_wait_seen(&screen.rows, runs).is_some()))
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
            if !writing
                && !carried
                && !signing_in
                && reader.background_wait_seen(&screen.rows, runs).is_none()
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
                self.note_retry_row(&screen);
                if !signing_in {
                    if let Some(opts) = opts {
                        self.end_told_wait(opts);
                    }
                    self.running.busy(self.fx.mono());
                }
                if review.unattended() {
                    if !signing_in {
                        let live = !writing
                            && !carried
                            && reader.background_wait_seen(&screen.rows, runs).is_none();
                        self.limit_on_busy(screen.seq, live, review)?;
                    }
                    if !writing
                        && !carried
                        && !signing_in
                        && !off
                        && let Some(opts) = opts
                    {
                        self.unstick_woken_turn(&screen, reader, opts, review)?;
                    }
                    // The agent working ends what it was told for, but a refused go-ahead
                    // stands through the breaks of its own background work, where it is told.
                    let at_break = reader.background_wait_seen(&screen.rows, runs).is_some();
                    if self.wall_memory.is_none()
                        && !self.turn_end.compacting()
                        && !signing_in
                        && self
                            .exception_open
                            .as_ref()
                            .is_none_or(|(open, _)| open.ends_at_work(at_break))
                    {
                        self.exception_ended(screen.seq, Some("the agent working"), review);
                    }
                    if self.box_ask.is_some() && !writing {
                        self.close_box(screen.seq, review)?;
                    }
                    if !writing && !self.question.answered.is_empty() {
                        self.question_done(screen.seq, review)?;
                    }
                    if !writing && let Some(opts) = opts {
                        let live =
                            !carried && reader.background_wait_seen(&screen.rows, runs).is_none();
                        self.reset_at_work(&screen, live, opts, review)?;
                    }
                }
                if !writing
                    && let Some(opts) = opts
                    && let Some(over) = self.memory_measure(opts)
                {
                    self.memory_warn(&screen, over, opts, review);
                }
                if phase == Phase::Busy
                    && !writing
                    && !signing_in
                    && let Some(host) = opts.and_then(|o| o.idle_host.as_ref())
                {
                    host.busy_tap();
                    // A wall's lasting withhold is over at a live read, where the session works
                    // past it ([`IdleHost::lasting_hold`]), and told so: the next break at a wall
                    // tells it again. A break of the agent's own background work reads busy too,
                    // and is no work past the wall it stands at: there the wall's hold stands, as
                    // that break told it, or each read would publish it and take it down again.
                    if !carried
                        && reader.background_wait_seen(&screen.rows, runs).is_none()
                        && matches!(self.lasting_told, Some(Some(Lasting::Wall { .. })))
                    {
                        self.tell_lasting(host.as_ref(), None);
                    }
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
                next = if footer_seen && matches!(next, Next::Gone(_) | Next::Run) {
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
            // A turn only the title runs shows no busy row to wait gone: the log wakes the wait
            // for its end (`footer_seen` arms it), and the short settle does where no log can.
            if title_only
                && !writing
                && phase == Phase::Busy
                && self.title_spent_seq != Some(screen.seq)
            {
                next = Next::Run;
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
                    None => indicator_gone(reader, rows),
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
        // Cruise's ladder at background breaks, replayed where it is still a ladder: carries
        // before the agent's launch are another program's, and a lift off Off starts it again.
        let mut carries = approvals::background_carries_in(body, self.setup.sid.as_deref());
        if !carries.is_empty()
            && let Some(launch) = self.stall_host.as_ref().and_then(|host| host.launch())
        {
            carries.retain(|&(ts, _)| ts >= launch);
        }
        if !lifted_off
            && let Some((last_ms, streak)) =
                super::policy::cruise_carry::BackgroundCarries::replayed(
                    &self.turn_end.timing,
                    &carries,
                    now_ms,
                )
        {
            let ago =
                Duration::from_millis(u64::try_from(now_ms.saturating_sub(last_ms)).unwrap_or(0));
            self.cruise_carry.seed(ago, streak, self.fx.mono());
        }
        let typed = approvals::typed_at_in(body, self.setup.sid.as_deref());
        let ages: Vec<Duration> = typed
            .iter()
            .filter_map(|&ts| {
                (now_ms >= ts)
                    .then(|| u64::try_from(now_ms.saturating_sub(ts)).ok())
                    .flatten()
            })
            .map(Duration::from_millis)
            .collect();
        self.turn_end.seed_acts(&ages, self.fx.mono());
        // The person's hour from a turn's end, which the window's clock does not keep.
        if let Some(from_ms) = approvals::go_ahead_hour_in(body, self.setup.sid.as_deref()) {
            let ago =
                Duration::from_millis(u64::try_from(now_ms.saturating_sub(from_ms)).unwrap_or(0));
            self.turn_end.seed_go_ahead_hour(ago, self.fx.mono());
        }
        if let Some(ts) = approvals::login_typed_last_in(body, self.setup.sid.as_deref()) {
            let ago = Duration::from_millis(u64::try_from(now_ms.saturating_sub(ts)).unwrap_or(0));
            self.turn_end.seed_login(ago, self.fx.mono());
        }
        if approvals::carry_ended_in(body, self.setup.sid.as_deref()) {
            self.turn_end.seed_carry_over();
        }
        let streak = approvals::short_streak_in(body, self.setup.sid.as_deref());
        if streak.as_ref().is_some_and(|s| s.recorded) || !typed.is_empty() {
            // The session worked under a loop before this one, a point it recorded or an act it
            // typed (a turn-ran row alone is a turn no point judged yet, such as the opening one
            // a restart cut): a person's message is not the one that opens it
            // ([`TurnEndState::seed_worked`]).
            self.turn_end.seed_worked();
        }
        if approvals::off_stood_in(body, self.setup.sid.as_deref()) {
            // Or under Off, driven by the person ([`TurnEndState::seed_off_stood`]).
            self.turn_end.seed_off_stood();
        }
        let since = streak
            .as_ref()
            .filter(|s| s.recorded && s.point)
            .map(|s| if s.ran { i64::MAX } else { s.at_ms });
        let ended = journal.as_deref().and_then(|journal| {
            ended_before(
                journal,
                self.setup.sid.as_deref(),
                typed.last().copied().max(since),
            )
        });
        if let Some((words, _, _, _, Some(ending))) = ended {
            self.turn_end.seed_failed(ending, words);
        } else if let Some((words, noted_ms, by_wake, receipt, None)) = ended {
            self.turn_end.seed_landed(words);
            self.turn_end.seed_noted_at(noted_ms);
            self.land_receipt = receipt;
            if by_wake {
                self.turn_end.seed_landed_by_wake();
            }
        }
        if let Some(seed) = &streak {
            self.turn_end.seed_short(seed.short);
            self.turn_end.seed_wall_pass(seed.pass);
            if let Some(program) = seed.program {
                self.turn_end.seed_short_program(program);
            }
            let ago = Duration::from_millis(
                u64::try_from(now_ms.saturating_sub(seed.at_ms)).unwrap_or(0),
            );
            self.turn_end.seed_replies(seed.said, ago, self.fx.mono());
            self.turn_end.seed_switch_spent(seed.switch_spent);
            self.turn_end.seed_checks(seed.checks);
            self.turn_end.seed_needs_acked(seed.needs_acked);
            let reset = seed.said_reset_ms.map(|ms| self.instant_of_ms(ms));
            self.turn_end.seed_replies_reset(reset);
            if seed.point && !seed.ran {
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
                let point_ms = short_held(&hold.why).map_or(hold.at_ms, |(_, wait_ms)| {
                    hold.until_ms.saturating_add(500).saturating_sub(wait_ms)
                });
                let ago = Duration::from_millis(
                    u64::try_from(now_ms.saturating_sub(point_ms)).unwrap_or(0),
                );
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
            let retry = [
                RULE_API_RETRY,
                RULE_API_BACK,
                RULE_API_CUTOFF,
                RULE_OUTAGE_RESUME,
            ]
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
                mark.down,
                self.fx.mono(),
            );
            if mark.witnessed {
                self.turn_end.seed_retry_witnessed();
            }
        }
        if let Some(key) = older.and_then(|j| journaled_left(j, self.setup.sid.as_deref()))
            && !left_keys.contains(&key)
        {
            // An older journal's: work before it is an act typed before it, or a point judged
            // before the one it left (two points by its time).
            let at = approvals::parse_left_before(&key).and_then(|t| i64::try_from(t).ok());
            if at.is_some_and(|at| {
                typed.iter().any(|&ts| ts < at)
                    || older_reads.points.iter().filter(|(t, _)| *t <= at).count() >= 2
            }) {
                self.turn_end.seed_left_after_work();
            }
            left_keys.push(key);
        }
        if approvals::left_after_work_in(body, self.setup.sid.as_deref()) {
            self.turn_end.seed_left_after_work();
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
            // The land check's books the last row kept: no defense is checked twice across loops.
            if let Some(seed) = &streak {
                self.turn_end.seed_land_books(seed.land);
            }
            if land.checked {
                self.turn_end.seed_land_checked();
            }
            self.turn_end.seed_land_hold(land.hold);
            if let Some(act) = land.act.filter(|_| land.landing && !land.cut) {
                let ago = Duration::from_millis(
                    u64::try_from(now_ms.saturating_sub(act.at_ms)).unwrap_or(0),
                );
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
                let ago = |ms: i64| {
                    Duration::from_millis(u64::try_from(now_ms.saturating_sub(ms)).unwrap_or(0))
                };
                self.turn_end
                    .seed_ran_turn(ago(ran_ms), ago(seed.at_ms), self.fx.mono());
            }
        }
        if lifted_off {
            // An Off no person set keeps the replies that hold past its lift, and Land's rest on
            // a need, as its park did.
            let kept = approvals::off_kept_in(body, self.setup.sid.as_deref());
            self.turn_end.start_lifted_off(kept);
            self.ledger_row(
                approvals::RULE_OFF_STOOD,
                approvals::Outcome::Waiting,
                "",
                kept.reason(),
                0,
            );
        }
        let open_wall = approvals::open_wall_in(body, self.setup.sid.as_deref());
        self.tap_said = approvals::tap_said_in(body, self.setup.sid.as_deref());
        if let Some(newest) = journal
            .as_deref()
            .and_then(|j| turn_end_loop::journaled_restart_floor(j, self.setup.sid.as_deref()))
        {
            self.restart_floor = self.restart_floor.max(newest);
        }
        self.seed_model_switch_in(body, older);
        if let Some(journal) = journal.as_deref() {
            self.seed_open_move_in(journal);
        }
        let moves = approvals::account_moves_in(body, self.setup.sid.as_deref());
        let ago = |ts: i64| {
            (now_ms >= ts)
                .then(|| u64::try_from(now_ms.saturating_sub(ts)).ok())
                .flatten()
                .map(Duration::from_millis)
        };
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
        let live = opts.idle_host.clone().filter(|_| !self.fx.replaying());
        let mut guard = occupancy_loop::UntoldOnUnwind::over(self, live);
        guard.session().drive_to_end(opts, review)
    }

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
        self.loop_ended(&end, opts, review);
        end
    }

    /// What the loop's end `end` leaves booked: the stuck episode and the exception closed, a
    /// line left in the composer released where the session went (its composer went with it, a
    /// wake's claim given back), the hold's end row where it lives on, the untold lines and the
    /// act marks.
    ///
    /// Where the session lives on, a line left stays left, a wake's claim kept: the loop makes
    /// no read at its end, and the line may still stand or may have gone, so releasing it unseen
    /// could have a successor, or the agent's own `await wake`, tell it a second time. The
    /// successor that seeds it (`FactsState::seed_left`) settles it at its first look that finds
    /// the composer clear (told, since it cannot weigh this loop's count) or the session gone
    /// (released). Until a loop looks the claim stands, and an `await wake` hears nothing of that
    /// wake (the slice-2 plan's 2.9b, decided under its safety lens: one tell lost at most, never
    /// a second).
    fn loop_ended(
        &mut self,
        end: &Result<End, String>,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) {
        self.stuck_over(0, "the loop ended", review);
        self.exception_ended(0, Some("the loop ended"), review);
        if session_gone(end) {
            let gone = super::facts::LeftLook::Gone;
            self.left_look(gone, None, 0, Some(&opts.policy), Some(&mut *review));
        } else {
            self.hold_left(Some(&opts.policy), review);
        }
        self.occupancy_untold();
        self.send_act_marks();
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
        self.metadata_global = opts.policy.autopilot;
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
        self.tasks_tracked = opts.tasks.is_some();
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
            if let Some(wrote) = approvals::probe_wrote(&body, self.setup.sid.as_deref()) {
                self.probe = UsageProbe::Owed {
                    own: false,
                    since: self.fx.mono(),
                };
                self.probe_seed = Some(wrote);
                self.tell_own_panel();
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
        // The Autopilot word no host has answered yet (the start-up read was lost,
        // [`Self::drive`]) is read before anything of this look is decided, since the survey's
        // duty and the Off park read it; a seeded landing still armed is decided under it and
        // its face told. Lost again, it is this look's outage, and the next look reads again.
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
        self.screen_capture(&turn.screen, opts, review);
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
            self.question_done(turn.screen.seq, review)?;
        }
        if worked {
            // The worker worked: the lines the loop left are gone, and a line held in custody is
            // looked at on the point's read, after them, so a draw hands it back to its writer.
            self.left_line = None;
            self.own_line = None;
            self.custody_after_work(&turn, review)?;
            self.left_went = None;
            self.unauthoritative_held = false;
            self.stall_withheld = false;
            self.withheld_ask = None;
            // A badge the turn-end policy raised with no point to carry it
            // (`/login` typed): the worker works again, so it is done.
            if std::mem::take(&mut self.turn_end_badge) {
                let said = self.unset_if_ours(|_| true)?;
                self.note_attention_clear(
                    turn.screen.seq,
                    &format!("{CLEARED_TURN_END} "),
                    &said,
                    review,
                );
            }
        }
        if self
            .box_ask
            .as_ref()
            .is_some_and(|b| b.key != *turn_key.get_or_insert_with(|| self.review_key(&turn)))
        {
            self.close_box(turn.screen.seq, review)?;
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
        if review.unattended() && self.stall_look(&turn, state, review)? {
            state.gone_first = false;
            return self.wait_for_next(turn, opts, allow, deadline, review);
        }
        if self.survey(&turn, opts, &mut state.survey, false, review)? {
            state.gone_first = false;
            return Ok(None);
        }
        let handed_back = std::mem::take(&mut self.question_handed_back)
            | std::mem::take(&mut self.login_box_news)
            | std::mem::take(&mut self.look_answered);
        let seen = if !handed_back
            && state.handed.as_deref().is_some_and(|h| {
                h == turn_key
                    .get_or_insert_with(|| self.review_key(&turn))
                    .as_str()
            })
            && !submitted_over(
                state.handed_submit,
                self.reader(&turn.screen.rows),
                &turn.screen,
            )
            && !self.point_model_moved(&turn)
        {
            // The point already handed over, still showing: the screen moved (a footer tick, a
            // banner) but nothing the point is made of did. Nothing is pressed or reported, and
            // the decision's read carries to the new screen, so the host's gate never acts on a
            // repaint's raw wall. A point handed on a read naming no model takes the first model
            // one of its reads names (a narrow agy pane names it only beside a draft).
            if self.point_model.is_none() {
                self.point_model = named_model(self.reader(&turn.screen.rows), &turn.screen.rows);
            }
            if let Some(old) = state.handed_seq.replace(turn.screen.seq) {
                self.carry_point(old, turn.screen.seq);
            }
            if review.unattended() {
                self.custody_look(&turn, review)?;
            }
            if review.unattended()
                && (self.left_line_look(&turn, opts, allow, review)?
                    || self.draft_look(&turn, opts, allow, review)?
                    || self.unauthoritative_look(&turn, opts, allow, review)?)
            {
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
            self.woken = None;
            self.retry_seen = None;
            let turn_end = if review.unattended() {
                let action = self.turn_end_looked(&point, busy_for, opts, review)?;
                let folded = self.task_edge_at_fold(&point.screen, opts, review);
                self.said_before_fail(&point, folded, review)?;
                action
            } else {
                TurnEndAction::Nothing
            };
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
            }
            if !self.say_point(&point, review)? {
                return Ok(Some(End::Stopped(point)));
            }
            state.approved.clear();
            state.handed = Some(self.review_key(&point));
            state.handed_seq = Some(point.screen.seq);
            state.handed_submit = submit_mark(self.reader(&point.screen.rows), &point.screen);
            self.point_model = named_model(self.reader(&point.screen.rows), &point.screen.rows);
            if review.unattended() {
                // A person told now to close their own `/login` over the limit
                // ([`Exception::tells_login_over_limit`]) is the limit's answer: no second notice.
                let handled = point_escalated
                    || !matches!(
                        turn_end,
                        TurnEndAction::Nothing | TurnEndAction::Escalate { .. }
                    )
                    || self
                        .exception_now
                        .as_ref()
                        .is_some_and(Exception::tells_login_over_limit);
                self.limit_after(&point, deadline, handled, review)?;
            }
            self.decision_owed = true;
            self.tell_break(opts, &point);
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
        let _duty = self.duty_guard(Scope::Questions);
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
        // With no survey drawn and none in hand, nothing below reads the draft.
        let draft = if turn.phase == Phase::Prompt || (!open && *survey == Survey::Closed) {
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
            // A line the loop gave up on undrawn, held in custody, holds the 0 as it holds the
            // other writers that ask it (ATERM-KI-1101-3): queued behind a line the worker has yet
            // to read, or read as a line it read draws, the 0 may land in the composer beside it.
            // The card is looked at again once the line settles.
            if self.custody.is_some() {
                return Ok(false);
            }
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
            PERSON_TYPING
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
                    // The host back may be another process, counting its submits from 0: no
                    // line left before keeps a count a look may weigh.
                    self.facts.state.recount();
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

    /// `latch`: a wait at rest, which a switch of the session's word wakes ([`Self::park`]).
    fn moved_past(
        &mut self,
        seq: u64,
        deadline: Instant,
        step_cap: Duration,
        latch: bool,
    ) -> Result<Past, Fail> {
        let step = deadline
            .saturating_duration_since(self.fx.mono())
            .min(step_cap);
        match self.park(&["seq", &seq.to_string()], step, latch)? {
            Wait::Latched => Ok(Past::Moved),
            Wait::Word(word) => Ok(Past::Word(word)),
            // With a composer that could not say held, the wait's own step reads it next
            // (`Self::wait_for_next`), so this read would be its twin.
            Wait::TimedOut
                if self.fx.mono() >= deadline || self.draft_held == Some(Composer::Unknown) =>
            {
                Ok(Past::Still(None))
            }
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

/// How long, in seconds, a read of the local zone's offset stands before the next look reads it
/// again ([`Session::local_offset_s`]): each read is a record on the session's tape.
const LOCAL_OFFSET_STANDS_S: i64 = 60;

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

    /// The local zone's offset as last read, read again when `now`, a wall clock the caller read,
    /// is [`LOCAL_OFFSET_STANDS_S`] past that read or behind it: a session can outlive a daylight
    /// change or the Mac's move to another zone. The choice follows the caller's read, which the
    /// tape holds, so a replay reads the offset where its recording did.
    pub(super) fn local_offset_s(&mut self, now: i64) -> i64 {
        if let Some((offset, read_at)) = self.local_offset
            && (0..LOCAL_OFFSET_STANDS_S).contains(&now.saturating_sub(read_at))
        {
            return offset;
        }
        let offset = self.fx.tz_offset(self.local_offset_fixed);
        self.local_offset = Some((offset, now));
        offset
    }

    /// The local zone's offset at an instant, for a clock that names no zone: asked of the zone
    /// where it answers, else `local`, the offset as last read.
    pub(super) fn local_at(&self, local: i64) -> impl Fn(i64) -> Option<i64> + '_ {
        move |unix| Some(self.local_offset_then(unix).unwrap_or(local))
    }

    /// When the turn on `rows`' done row ended ([`limit::done_row_ended_local`]).
    pub(super) fn done_row_ended(&mut self, rows: &[String], now: i64) -> Option<i64> {
        let local = self.local_offset_s(now);
        limit::done_row_ended_local(rows, now, &self.local_at(local))
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
        let local = self.local_offset_s(now);
        let at = limit::reset_at_local(&spec, now, &self.local_at(local), &self.zone_lookup());
        let at = self.transcript_day(&spec, at, local);
        Some(self.on_reset_clock(at))
    }

    /// `at`, the reset a wall's words name (unix seconds), where the loop waits to it
    /// ([`TurnEndTiming::reset_placed`]): as named, unless a development build scaled the time to
    /// it (`turn_end::RESET_SPAN_SCALE_SEAM`), when the wall clock is read again, to the
    /// millisecond, to place it from. Only a scaled timing reads that clock, so a run under the
    /// shipped one reads, and tapes, what it did before.
    pub(super) fn on_reset_clock(&self, at: i64) -> i64 {
        let timing = &self.turn_end.timing;
        if timing.reset_as_named() {
            return at;
        }
        timing.reset_placed(at, self.now_unix_ms())
    }

    fn transcript_day(&self, spec: &ResetSpec, at: i64, local: i64) -> i64 {
        if !matches!(spec, ResetSpec::At { date: None, .. }) {
            return at;
        }
        self.stall_host
            .as_ref()
            .and_then(|h| i64::try_from(h.limit_until()?).ok())
            .filter(|&named| {
                limit::is_a_later_day_of(
                    spec,
                    at,
                    named,
                    &self.local_at(local),
                    &self.zone_lookup(),
                )
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
        let local = self.local_offset_s(now);
        let at = if places_resets_on_print(self.reader(rows).program()) {
            let ended = self.done_row_ended(rows, now);
            let printed = limit::printed_from(ended, now, live)
                .max(answered.then_some(now - limit::PRINT_LAG_S - limit::CLOCK_SLACK_S));
            let zone = &self.zone_lookup();
            limit::place_reset_local(
                &spec,
                session_window,
                now,
                printed,
                &self.local_at(local),
                zone,
            )
        } else {
            limit::reset_at_local(&spec, now, &self.local_at(local), &self.zone_lookup())
        };
        Some(self.transcript_day(&spec, at, local))
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
            self.tell_wall(Some(at));
        }
    }

    /// Tell the host the reset the open limit episode now holds its wall to
    /// ([`IdleHost::limit_wall`]), so what it publishes of the wall is spent where
    /// [`Self::limit_spent`] takes it so. A replay of a tape whose recording build had no such door
    /// tells nothing, as it recorded.
    fn tell_wall(&self, reset_unix: Option<i64>) {
        if let Some(host) = &self.stall_host
            && !self.fx.replay_lacks_host_door("host.limit_wall")
        {
            host.limit_wall(reset_unix);
        }
    }

    /// The usage tap times the standing wall ([`Self::tap_unix`]) at another reset than the open
    /// limit episode holds it to, a cut wall's first one included, however the loop came to read
    /// it: [`Self::tap_edge`]'s poll during the wait, a new point's decision ([`Self::limit_after`])
    /// or a decision an edge made again of the point it waits at (looked at as the next wait
    /// starts, [`Self::wait_for_next`]). The episode holds its wall to the tap's reset, earlier or
    /// later, since the loop waits to that one; the host is told, the budget is extended to it
    /// ([`Self::extend`]), and a badge raised for the wall is raised again when the reset changes
    /// whether the harness waits it out ([`Self::waits_out`]).
    fn limit_retimed(
        &mut self,
        seq: u64,
        deadline: &mut Instant,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let Some(at) = self.tap_unix() else {
            return Ok(());
        };
        let Some(ep) = self.limit.as_mut().filter(|ep| ep.reset_unix != Some(at)) else {
            return Ok(());
        };
        let at = Some(at);
        let was = std::mem::replace(&mut ep.reset_unix, at);
        ep.extended = false;
        let (raised, message) = (ep.raised, ep.message.clone());
        self.tell_wall(at);
        let waits = self.waits_out(&message, at);
        if raised && waits != self.waits_out(&message, was) {
            let text = format!("{ATTENTION_PREFIX} {}", one_line(&message));
            self.escalate(
                seq,
                &text,
                None,
                escalate::Escalation::Limit { waits },
                review,
            )?;
        }
        self.extend(deadline, review)
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
                self.limit_retimed(point.screen.seq, deadline, review)?;
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
        self.limit_retimed(point.screen.seq, deadline, review)
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
            worked: false,
            message: message.to_string(),
        });
        if let Some(host) = &self.stall_host {
            host.limited(true);
        }
        self.tell_wall(reset_unix);
        Ok(())
    }

    pub(super) fn waits_out(&self, message: &str, reset_unix: Option<i64>) -> bool {
        let horizon = i64::try_from(RESET_HORIZON.as_secs()).unwrap_or(i64::MAX);
        limit::resumes_by_itself(message)
            || (self.resume_limits && reset_unix.is_some_and(|at| at - self.now_unix() <= horizon))
    }

    /// A busy read closes the episode of a notice that resumes by itself, and a `live` read (the
    /// agent's own turn, no break of its background work) one whose reset and grace have passed:
    /// the agent works past it, and a turn of it may end at such a break, where no point closes
    /// it. The wall again within that turn is the same episode ([`Retry`]), until the point or the
    /// settled break the turn ends at ([`Self::limit_at_break`]). Any other live read marks the
    /// episode worked, for the break that turn ends at.
    fn limit_on_busy(&mut self, seq: u64, live: bool, review: &mut dyn Review) -> Result<(), Fail> {
        let Some(ep) = self.limit.as_ref() else {
            return Ok(());
        };
        let why = if ep.resumes_itself {
            "the worker works again: the notice said it would continue by itself"
        } else if live && self.limit_spent(self.stall_host.as_deref(), None) {
            "the worker works again past the limit's reset"
        } else {
            if let Some(ep) = self.limit.as_mut() {
                ep.worked |= live;
            }
            return Ok(());
        };
        let retry = Retry { seq: ep.seq };
        self.close_episode(seq, why, review)?;
        self.retry = Some(retry);
        Ok(())
    }

    /// A settled break of the agent's background work that its live turn reached since the wall
    /// closes the episode, as a point it reached working does (`answered after a busy spell`),
    /// reset or no reset. A turn that met the wall again ended at a Limited point, and a break
    /// after it reads as that wall ([`Self::remembered_wall`]), so it reaches no break. The break
    /// ends the turn that resumed past a closed episode, too, so a wall after it, hours on or
    /// minutes, opens an episode of its own, escalated as new, never the old one's retry
    /// ([`Retry`]). The clear reads no condition: only an unattended run sets a retry
    /// ([`Self::limit_on_busy`], an Off park's start), so an attended run has none to clear.
    fn limit_at_break(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        self.retry = None;
        if !self.limit.as_ref().is_some_and(|ep| ep.worked) {
            return Ok(());
        }
        self.close_episode(
            seq,
            "the worker worked past the limit to a break of its background work",
            review,
        )
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
        if reset.saturating_sub(now_unix)
            > i64::try_from(RESET_HORIZON.as_secs()).unwrap_or(i64::MAX)
        {
            return Ok(());
        }
        let until_unix = reset.saturating_add(i64::try_from(RESET_GRACE.as_secs()).unwrap_or(0));
        let wait_secs = u64::try_from(until_unix.saturating_sub(now_unix)).unwrap_or(0);
        let mono = self.fx.mono();
        let until = mono
            .checked_add(Duration::from_secs(wait_secs))
            .unwrap_or_else(|| mono + RESET_HORIZON + RESET_GRACE);
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
        // The acts this look queued are the log's before it waits (fix E3, E2E-3): a box
        // handed to the person blocks the wait until it leaves, so `drive_loop`'s flush after
        // the look would write the escalate act at the answer, not when the harness acted.
        self.send_act_marks();
        self.limit_retimed(seen.screen.seq, deadline, review)?;
        let anchor = (seen.phase == Phase::Prompt)
            .then(|| box_anchor(self.reader(&seen.screen.rows), &seen.screen.rows))
            .flatten();
        self.stall_before_wait(&seen, review)?;
        let held = self.question_held
            && aterm_phase::parse_prompt_v2(&seen.screen.rows)
                .is_some_and(|p| p.kind == aterm_phase::PromptKind::Question);
        // A box escalated for a look still out is looked at again as often as a held question.
        let left_out = self.box_left_out.is_some()
            && self.box_left_out.as_deref() == Some(self.review_key(&seen).as_str());
        let step_cap = if held || left_out {
            HANDBACK_POLL
        } else {
            self.setup.wait_step
        };
        self.watch_sleep();
        let mut seen_key: Option<String> = None;
        let mut verdict_seq = seen.screen.seq;
        let mut since_recheck = Duration::ZERO;
        let request = anchor
            .as_ref()
            .filter(|_| !held)
            .and_then(|_| self.file_request(&seen));
        let mut asked = false;
        let mut woke = false;
        let rest = review.unattended();
        loop {
            if self.stopped() {
                return Ok(None);
            }
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
            if self.witness_edge(opts) {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.memory_edge(&seen, opts, review)? {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.account_edge(opts) {
                if seen.phase == Phase::Prompt && self.turn_end.login_typed_at().is_some() {
                    self.login_box_news = true;
                    return Ok(None);
                }
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.tap_edge(opts, seen.screen.seq) {
                self.limit_retimed(seen.screen.seq, deadline, review)?;
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.login_edge(opts) {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.word_edge(&seen, anchor.is_some(), opts, allow, review)? {
                return Ok(None);
            }
            if std::mem::take(&mut self.edge_moved) {
                self.turn_end_now(&seen, opts, allow, review)?;
                return Ok(None);
            }
            if self.look_at.is_some_and(|at| self.fx.mono() >= at) {
                self.look_at = None;
                return Ok(None);
            }
            self.tell_wait(opts);
            let due = self
                .due_at()
                .into_iter()
                .chain(self.look_at)
                .chain(self.word_at)
                .chain(self.needs_retry_at)
                .chain(self.attention_retry_at)
                .min();
            let until = due.map_or(*deadline, |t| t.min(*deadline));
            let now = self.fx.mono();
            let step_cap = opts
                .idle_host
                .as_ref()
                .and_then(|h| h.wants_at())
                .filter(|at| *at > now)
                .map_or(step_cap, |at| step_cap.min(at - now));
            let step_cap = self
                .reach_wake(opts)
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
                    match self.park(&["gone", anchor], step, rest)? {
                        Wait::Latched => return Ok(None),
                        Wait::Word(word) => {
                            self.word_moved(seen.screen.seq, word, review);
                            continue;
                        }
                        Wait::TimedOut => {}
                        Wait::Unsupported => {
                            return Err(Fail::Hard(format!(
                                "await gone {anchor} is not known to this host"
                            )));
                        }
                    }
                    if let Some(request) = request.as_deref() {
                        let screen = self.screen()?;
                        let now = self.turn_of(screen);
                        if self.file_request(&now).as_deref() != Some(request) {
                            return Ok(None);
                        }
                    }
                } else if let Some(words) = self.agent_wake(&seen, &mut seen_key) {
                    let waited = step.min(AGENT_RECHECK);
                    match self.park(&["agent", words], waited, rest)? {
                        Wait::Latched => {
                            self.agent_woke =
                                Some(seen_key.take().unwrap_or_else(|| self.review_key(&seen)));
                            return Ok(None);
                        }
                        Wait::Word(word) => {
                            self.word_moved(seen.screen.seq, word, review);
                            continue;
                        }
                        Wait::TimedOut => {
                            // A TURN TOO SHORT TO MOVE THE WORD IS FOUND HERE, so the look comes by
                            // what has been waited: a retry due every 2 s (a take-down still owed)
                            // cuts every step short, and must not hold back this read.
                            since_recheck += waited;
                            if since_recheck >= AGENT_RECHECK.min(self.setup.wait_step) {
                                since_recheck = Duration::ZERO;
                                if self.moved_under_verdict(
                                    &seen,
                                    &mut seen_key,
                                    &mut verdict_seq,
                                    review,
                                )? {
                                    return Ok(None);
                                }
                            }
                        }
                        Wait::Unsupported => self.caps.await_agent = Some(false),
                    }
                } else {
                    match self.moved_past(seen.screen.seq, until, step_cap, rest)? {
                        Past::Moved => return Ok(None),
                        Past::Word(word) => {
                            self.word_moved(seen.screen.seq, word, review);
                            continue;
                        }
                        Past::Still(_) => {}
                    }
                }
                if held && self.question_policy_answers(opts)? {
                    self.question_held = false;
                    self.question_handed_back = true;
                    return Ok(None);
                }
                if left_out && !self.fx.policy_probe().look_out() {
                    self.box_left_out = None;
                    self.look_answered = true;
                    return Ok(None);
                }
                if self.stall_step(&seen, review)? {
                    return Ok(None);
                }
                // A composer whose read could not say (its glyph's cell refused, read inside a frame,
                // or grey with the cursor off it) is looked at again at the wait's pace: a cell coming
                // back or a cursor going home moves no content seq, and nothing else would ever
                // decide the point. The look's own read does it (`Self::draft_look`), as a moved
                // screen's would.
                if self.draft_held == Some(Composer::Unknown) {
                    return Ok(None);
                }
                if (self.fx.mono() < until && !self.due_at().is_some_and(|at| self.fx.mono() >= at))
                    || self.stalled.is_some()
                {
                    continue;
                }
            }
            if self.fx.mono() >= *deadline {
                return Ok(Some(End::Timeout(seen)));
            }
            let now = self.fx.mono();
            if self.word_at.is_some_and(|at| now >= at) {
                continue;
            }
            let policy_due = self.due_at().is_some_and(|at| now >= at)
                || self.look_at.is_some_and(|at| now >= at);
            if !policy_due && self.needs_retry_at.is_some_and(|at| now >= at) {
                self.retry_needs(&seen.screen, opts, review)?;
                if self.autopilot(opts) == Autopilot::Off {
                    return Ok(None);
                }
                continue;
            }
            if !policy_due && self.attention_retry_at.is_some_and(|at| now >= at) {
                self.retry_supervisor_attention(&seen.screen, opts, review)?;
                if self.autopilot(opts) == Autopilot::Off {
                    return Ok(None);
                }
                continue;
            }
            if self.look_at.is_some_and(|at| self.fx.mono() >= at) {
                self.look_at = None;
                return Ok(None);
            }
            self.turn_end_now(&seen, opts, allow, review)?;
            return Ok(None);
        }
    }

    /// A wait at rest woke on a switch of the session's word to `word` ([`Wait::Word`]): the loop
    /// decides on the word that stands [`Self::word_settle`] from now ([`Self::word_edge`]), and
    /// its next waits carry `word`, so they wake only on a switch after it. A switch inside the
    /// settle starts it again.
    fn word_moved(&mut self, seq: u64, word: Option<Autopilot>, review: &mut dyn Review) {
        self.autopilot.reported(word);
        self.word_at = Some(self.fx.mono() + self.word_settle);
        review.note(&format!(
            "NOTE seq={seq} autopilot={} the session's Autopilot word moved: decided after a {} ms \
             settle",
            word.map_or("-", Autopilot::word),
            self.word_settle.as_millis()
        ));
    }

    /// THE WORD EDGE (v6 §14.10): once a switch has stood its settle ([`Self::word_moved`]), the
    /// word read then decides the point `seen`; Off leaves it to [`Self::look`]'s park, and a box
    /// (`boxed`) is left to the look, which presses or leaves it as before.
    fn word_edge(
        &mut self,
        seen: &Turn,
        boxed: bool,
        opts: &SuperviseOpts,
        allow: &[String],
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if !self.word_at.is_some_and(|at| self.fx.mono() >= at) {
            return Ok(false);
        }
        // The edge stays armed until its read comes back: a read lost to a cut is made again at
        // the next wait, after the loop finds its host again.
        self.autopilot.begin();
        let word = self.autopilot_read(opts)?;
        self.word_at = None;
        if word != Autopilot::Off && !boxed {
            self.turn_end_now(seen, opts, allow, review)?;
        }
        Ok(true)
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
        let ahead =
            Duration::from_millis(u64::try_from(wall_ms.saturating_sub(mono_ms)).unwrap_or(0));
        let awake = Duration::from_millis(u64::try_from(mono_ms).unwrap_or(0));
        self.sleep_window.add(awake, ahead);
        if self.dark_gates > 0 {
            self.dark_held = self.dark_held.saturating_add(awake.saturating_add(ahead));
        }
        if !slept_through(wall_ms.div_euclid(1000), mono_ms.div_euclid(1000)) {
            return;
        }
        let by = self
            .recount_sleep()
            .map_or(ahead, |counted| counted.min(ahead));
        let secs = i64::try_from(by.as_secs()).unwrap_or(i64::MAX);
        if !slept_through(secs, 0) {
            return;
        }
        self.sleeps.add(by, wall_then, wall_now);
        self.turn_end.slept(by);
        self.person_slept(by);
        let woke_at = wall_now.div_euclid(1000);
        self.settle_wake(woke_at, mono_now);
        self.slept_at = Some(woke_at);
        let carried = self.retry_seen.clone();
        self.woken = self
            .running
            .runs()
            .then(|| woken_turn::WokenTurn::measured(wall_then, wall_now, mono_now, carried));
    }

    /// THE LOOP'S OWN STAMPS OF THE PERSON COUNT THE SLEEP (2026-10-30 review 9, second pass):
    /// their last gesture and key as the last `status` timed them (`Session::person`,
    /// `Session::key`), the last hand a read saw (`Session::hand_key`), the draft the loop saw and
    /// the read that found one gone (`Session::draft_left`) are `Instant`s, which stood still while
    /// the Mac slept `by`. Each moves back by it, as the
    /// window's ages count it and [`TurnEndState::slept`] moves the policy's, so none reads as
    /// minutes old at the wake: [`Self::key_ago`] takes the youngest, and one left on the awake
    /// clock read as a new key past the moved-back one, which started the hour again. A stamp the
    /// clock cannot hold that far back is dropped, and the window's age stands for it.
    fn person_slept(&mut self, by: Duration) {
        let back = |t: Option<Instant>| t.and_then(|t| t.checked_sub(by));
        self.person = back(self.person);
        self.key = back(self.key);
        self.hand_key = back(self.hand_key);
        self.draft_left = back(self.draft_left);
        self.draft_seen = self
            .draft_seen
            .take()
            .and_then(|(draft, at, cell)| Some((draft, at.checked_sub(by)?, cell)));
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
            && Duration::from_millis(u64::try_from(now_ms.saturating_sub(woke_ms)).unwrap_or(0))
                < reach
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

    /// Journals a fresh repository probe that could not read the repository
    /// ([`Self::repo_unread_unsaid`]).
    fn say_repo_unread(&mut self, seq: u64, review: &mut dyn Review) {
        if std::mem::take(&mut self.repo_unread_unsaid) {
            review.note(&format!(
                "REPO UNREAD seq={seq} a read of the repository did not end within {}s or could \
                 not be made: what the repository holds is unknown, and no landing is judged \
                 clean on it",
                crate::harness::repo_work::PROBE_LIMIT.as_secs()
            ));
        }
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

    /// Whether the host's peek at the API's reach ([`IdleHost::reach_peek`], which takes no
    /// lease) says Down now; never in a replay whose recording build had no `host.reach_peek`
    /// door, which peeks nothing.
    pub(super) fn peeks_down(&self, host: &dyn IdleHost) -> bool {
        !self.fx.replay_lacks_host_door("host.reach_peek")
            && matches!(host.reach_peek(), Reach::Down { .. })
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

    /// A NEW WITNESS ON THE ROUTE OF AN OUTAGE WAIT (v6 §14.17.5.4, slice 12c): the witness was
    /// asked at this point (an outage wall's, [`Self::witness_seen`]) and a turn end is due, and
    /// the host's newest answer of another session on the route moved since; the point is decided
    /// again, which resumes it where the answer came after the wall's last try. Read at each step
    /// of the wait (at most [`WAIT_STEP`] apart), so a witness is acted on within one step.
    fn witness_edge(&mut self, opts: &SuperviseOpts) -> bool {
        let Some(seen) = self.witness_seen else {
            return false;
        };
        if self.turn_end_due.is_none() {
            return false;
        }
        let Some(host) = opts.idle_host.as_ref() else {
            return false;
        };
        let now = host.witness();
        if now == seen {
            return false;
        }
        self.witness_seen = Some(now);
        true
    }

    /// When the host's next measure of the reach can land ([`IdleHost::reach_at`]), while
    /// [`Self::reach_edge`] would act on it: the reach was asked at this point and a turn end is
    /// due. The wait's step ends by then, so a measured Up is met within one step of landing,
    /// not at the next [`WAIT_STEP`]. A replay whose recording build had no `host.reach_at`
    /// door asks nothing here, so its steps stay as recorded.
    fn reach_wake(&self, opts: &SuperviseOpts) -> Option<Instant> {
        if self.reach_seen.is_none()
            || self.turn_end_due.is_none()
            || self.fx.replay_lacks_host_door("host.reach_at")
        {
            return None;
        }
        opts.idle_host.as_ref()?.reach_at()
    }

    pub(super) fn account_edge(&mut self, opts: &SuperviseOpts) -> bool {
        if opts.policy.accounts == crate::harness::accounts::Mode::Off && !opts.policy.logins {
            return false;
        }
        if self.renewal_made(opts) {
            return true;
        }
        let latched = self.turn_end.account_refused();
        // The `/login` this loop typed still stands (its dialog, or its opening): the roster may
        // name the login's row while the dialog sits unchanged, and the dialog is then backed out
        // of ([`super::policy::login::backs_out`]), so the accounts' news decides it again.
        let own_login = opts.policy.logins && self.turn_end.login_typed_at().is_some();
        let stands = self.turn_end_due.is_some()
            || (self.nothing_decided && self.roster_seen.is_some())
            || own_login;
        if (self.account_seen.is_none() && self.roster_seen.is_none() && !latched && !own_login)
            || !stands
        {
            return false;
        }
        // A TURN END'S WAIT UNDER THE MOVE ITS SESSION YIELDS ITS ACCOUNT BY (F19's `yields`), OR
        // UNDER A MOVE ITS HOST'S WORD PUTS OFF (`Self::restart_later`'s due), ENDS AT ITS
        // INSTANT: what keeps that move unmade (a live job, a person's hand, a missing socket, the
        // hour's moves) is no news of the accounts, but the host's strike of the row the unmade
        // move took is (`Kind::Struck` wakes), and news would ask the move again at once, its
        // strike news again: 371 asks in 288 s in `claude-rotation-proof`'s R6 (rp1103a,
        // 2026-11-03), the back-off never reached.
        let waits_out = self.account_restart_waits()
            || (self.turn_end_due.is_some()
                && matches!(
                    &self.account_seen,
                    Some((_, AccountPoint::TurnEnd, Some(advice), _))
                        if matches!(
                            advice.past_probe(),
                            Some(AccountAdvice::Move { yields: Some(_), .. })
                        )
                ));
        if waits_out && !own_login {
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

    /// The standing due waits out a host's word on a move onto another account
    /// ([`Self::restart_later`]).
    fn account_restart_waits(&self) -> bool {
        self.turn_end_due
            .as_ref()
            .and_then(|due| due.put_off)
            .is_some_and(super::policy::turn_end::Restart::moves_account)
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
            if self.attention_pending {
                self.note_attention_clear(
                    seen.screen.seq,
                    &format!("{CLEARED_MEMORY} "),
                    &said,
                    review,
                );
            } else {
                review.note(&format!(
                    "CLEARED seq={} {CLEARED_MEMORY} attention={said}: the measure is no longer \
                     acted on",
                    seen.screen.seq
                ));
            }
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
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse) => p.product(),
            Program::Generic => "The agent",
        };
        let resume = host.resume_command();
        let text = over.warning(agent, restarts, resume.as_deref());
        review.note(&format!("MEMORY seq={} {text}", screen.seq));
        host.warn(&text);
    }

    /// Whether the screen moved past the verdict's point while the loop waits at it. The screen it
    /// reads is also the context watch's ([`Self::watch_context`]): at rest is where a person's
    /// `/compact`, the `/` and `@` menus and the shortcuts show.
    fn moved_under_verdict(
        &mut self,
        seen: &Turn,
        seen_key: &mut Option<String>,
        seq: &mut u64,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        if let Some(last) = self.last.clone().filter(|l| l.seq > *seq) {
            *seq = last.seq;
            self.watch_context(&last, self.reader(&last.rows), &mut |line| review.say(line))?;
            let then = self.turn_of(last);
            let key = seen_key.get_or_insert_with(|| self.review_key(seen));
            if self.moved_from(seen, key, &then) {
                return Ok(true);
            }
        }
        match self.wait(&["seq", &seq.to_string()], Duration::ZERO)? {
            Wait::Latched | Wait::Word(_) => {}
            Wait::TimedOut => return Ok(false),
            Wait::Unsupported => {
                return Err(Fail::Hard(
                    "await seq is not known to this host".to_string(),
                ));
            }
        }
        let screen = self.screen()?;
        *seq = screen.seq;
        self.watch_context(&screen, self.reader(&screen.rows), &mut |line| {
            review.say(line)
        })?;
        let now = self.turn_of(screen);
        let key = seen_key.get_or_insert_with(|| self.review_key(seen));
        Ok(self.moved_from(seen, key, &now))
    }

    /// WHETHER `now` IS ANOTHER POINT THAN `seen`, whose key is `key` ([`Self::review_key`]): the key
    /// moved, or a submission counted since drew its turn ([`submitted_over`]). The model a limit
    /// wall binds to ([`Self::point_model_moved`]) is no part of it: a wake is awaited only at an
    /// idle or question point with no wall ([`Self::agent_wake`]), and a wall drawn since moves the
    /// key's phase.
    fn moved_from(&self, seen: &Turn, key: &str, now: &Turn) -> bool {
        let (was, is) = (
            self.reader(&seen.screen.rows),
            self.reader(&now.screen.rows),
        );
        point_key(is, now) != key || submitted_over(submit_mark(was, &seen.screen), is, &now.screen)
    }

    /// WHETHER `now` NAMES ANOTHER MODEL THAN THE POINT'S, AT A LIMIT WALL ([`Self::point_model`],
    /// [`model_moved`]): only there does the model bind (agy's quota is a model's,
    /// [`Seat::moved`](super::policy::turn_end::Seat::moved)'s `per_model`), so a footer read cut
    /// short mid-repaint anywhere else moves no point. One baseline for every read of the point:
    /// its hand-off's, then the first model a read of it named.
    pub(super) fn point_model_moved(&self, now: &Turn) -> bool {
        matches!(now.phase, Phase::Limited { .. })
            && model_moved(
                self.point_model.as_deref(),
                named_model(self.reader(&now.screen.rows), &now.screen.rows).as_deref(),
            )
    }

    fn agent_wake(&self, seen: &Turn, seen_key: &mut Option<String>) -> Option<&'static str> {
        if self.caps.await_agent != Some(true)
            || self.turn_end.act_in_flight()
            || self.turn_end.waits_on_named_reset()
            || self.draft_held.is_some()
            || self.unauthoritative_held
            || self.left_line.is_some()
            || self.left_went.is_some()
            || self.custody.is_some()
        {
            return None;
        }
        if self.footer.is_none()
            && !self.busy_read
            && match self.reader(&seen.screen.rows).program() {
                Program::Claude | Program::Generic => true,
                Program::Codex | Program::Grok | Program::Kimi | Program::Agy | Program::Muse => {
                    false
                }
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

    /// Offers a settled background-work break to [`IdleHost::at_background`] and updates occupancy
    /// state, then writes the stuck fact's line (`[harness] stuck_break_note`) and the door's, in
    /// the occupancy design's §2.1 order.
    fn host_steps_in_background(
        &mut self,
        screen: &Screen,
        since: &mut Option<Instant>,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let reader = self.reader(&screen.rows);
        self.carry_read(reader, screen);
        let carried = self.transcript_wait_seq == Some(screen.seq);
        let runs = reader.runs(screen.title.as_deref());
        let unfit = if carried {
            None
        } else if phase_of(reader, screen) != Phase::Busy {
            Some(Guard::NotBusy)
        } else if reader.background_wait_seen(&screen.rows, runs).is_none() {
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
        if let Some(host) = host.as_ref() {
            // The lasting withhold this break holds the session at: a wall that is no usage
            // limit, or the credits hold; a break that is no break of the agent's own background
            // work tells nothing.
            let lasting = match unfit {
                Some(Guard::Wall) => Some(wall.as_ref().filter(|w| !w.kind.reads_limited()).map(
                    |w| Lasting::Wall {
                        wire: w.kind.wire(),
                    },
                )),
                Some(_) => None,
                None => Some(
                    cfg.and_then(|cfg| self.credits_hold_lifts_here(cfg, &screen.rows))
                        .map(|lifts_unix| Lasting::Credits { lifts_unix }),
                ),
            };
            if let Some(lasting) = lasting {
                self.tell_lasting(host.as_ref(), lasting);
            }
        }
        if let Some(guard) = unfit {
            *since = None;
            self.withhold(host.as_ref(), guard);
            let hold = (guard == Guard::Wall).then_some(HoldSaid::Held(guard));
            self.say_hold(screen.seq, hold, review);
            return Ok(());
        }
        self.turn_end.fact_answered();
        // No decision reads this composer, and the break's guards hold what it shows.
        let reading = reader.read(&screen.rows, Some(screen.cursor_col));
        self.composer_for_guards(&reading, screen);
        // A line held in custody is looked at on the break's read first: what settles it at a
        // point settles it at a break (`Session::custody_at_break`).
        self.custody_at_break(screen, review)?;
        // The break's read is a look at any line left in the composer, before any door's line.
        self.left_look_at(
            screen,
            occupancy_loop::LookAt::Break,
            cfg,
            Some(&mut *review),
        );
        let at = *since.get_or_insert_with(|| self.fx.mono());
        let Some(host) = host.filter(|h| h.wants()) else {
            self.withheld = None;
            let waited = self.fx.since(at);
            let settled = waited >= self.setup.background_settle;
            // A limit episode the agent worked past to this break closes first: the break's judgment
            // of its turn (the person's key read, the carry or the go-ahead after it) reads it closed.
            if settled {
                self.limit_at_break(screen.seq, review)?;
            }
            self.break_judged(screen, at, settled);
            if settled {
                self.say_hold(screen.seq, Some(HoldSaid::NoHostStep), review);
            }
            self.hold_background(screen, at, waited, settled, cfg, review);
            // The needs the break's report names, read before any line the break types and told
            // after its lines, whatever they returned (`Session::break_needs`).
            let shown = self.break_fold(screen, (at, waited), cfg, review);
            let lines = self.break_lines_hostless(screen, since, (at, waited), cfg, review);
            let told = self.break_needs(screen, shown, cfg, review);
            return lines.and(told);
        };
        let waited = self.fx.since(at);
        let settled = waited >= self.setup.background_settle;
        // A limit episode the agent worked past to this break closes first: the break's judgment
        // of its turn (the person's key read, the carry or the go-ahead after it) reads it closed.
        if settled {
            self.limit_at_break(screen.seq, review)?;
        }
        self.break_judged(screen, at, settled);
        self.hold_background(screen, at, waited, settled, cfg, review);
        // The needs the break's report names, read before any line the break types and told after
        // its lines, whatever they returned (`Session::break_needs`).
        let shown = self.break_fold(screen, (at, waited), cfg, review);
        let lines = self.break_lines(screen, since, (at, waited), (&host, notice), cfg, review);
        let told = self.break_needs(screen, shown, cfg, review);
        lines.and(told)
    }

    /// The lines a break of the agent's own background work types where no window host wants
    /// steps of its own ([`Self::host_steps_in_background`]): a due wake's, the stuck fact's, the
    /// door's, else Cruise's carry.
    fn break_lines_hostless(
        &mut self,
        screen: &Screen,
        since: &mut Option<Instant>,
        (at, waited): (Instant, Duration),
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        // A due wake's line (§2.1 rank 2) first: typed, it is the break's one line.
        if self.wake_first(screen, cfg, review)? {
            *since = None;
            return Ok(());
        }
        // The stuck fact's own line (the occupancy design's §2.1 rank 4), before the nudges.
        let stuck = self.stuck_break(screen, (at, waited), false, cfg, review)?;
        if stuck {
            *since = None;
        }
        let line_gone = stuck.then_some(Guard::HostLine);
        let wrote = self.occupancy_write(screen, line_gone, cfg, review)?;
        if wrote {
            *since = None;
        }
        // Cruise's carry where no line went: Cruise never parks at such a break.
        if self.cruise_carry(screen, (at, waited), stuck || wrote, cfg, review)? {
            *since = None;
        }
        Ok(())
    }

    /// The lines a break of the agent's own background work types where the window's `host`
    /// wants its own steps ([`Self::host_steps_in_background`]; `notice`, the usage limit's reset
    /// the screen draws): held behind a brake, else a due wake's, the host's step or fact, the
    /// stuck fact's, the door's, else Cruise's carry.
    fn break_lines(
        &mut self,
        screen: &Screen,
        since: &mut Option<Instant>,
        (at, waited): (Instant, Duration),
        (host, notice): (&Arc<dyn IdleHost>, Option<Option<&str>>),
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let settled = waited >= self.setup.background_settle;
        let held = if !settled {
            Some(Guard::Settle)
        } else if self.limit.is_some() && !self.limit_spent(Some(host.as_ref()), notice)
            || self.policy_limit_holds()
        {
            Some(Guard::LimitEpisode)
        } else if self.stalled.is_some() {
            Some(Guard::Stall)
        } else if self.turn_end.act_untaken() {
            Some(Guard::ActUntaken)
        } else if self.settling().is_some() {
            Some(Guard::WakeSettle)
        } else if cfg.is_some_and(|cfg| self.credits_hold_here(cfg, &screen.rows)) {
            Some(Guard::Credits)
        } else {
            None
        };
        if let Some(guard) = held {
            self.withhold(Some(host), guard);
            if guard != Guard::Settle {
                // A hold types no go-ahead: the lines the read held back for it are back
                // (DESIGN-harness-autopilot §15.6).
                self.turn_end.go_ahead_unwritten();
                self.say_hold(screen.seq, Some(HoldSaid::Held(guard)), review);
            }
            self.occupancy_write(screen, Some(guard), cfg, review)?;
            // A `/compact`'s continuation owed is Cruise's line here, the host's step held
            // behind it: no point comes while the agent's background work runs.
            if guard == Guard::ActUntaken
                && self.turn_end.owed_at_break().is_some()
                && self.cruise_carry(screen, (at, waited), false, cfg, review)?
            {
                *since = None;
            }
            return Ok(());
        }
        self.withheld = None;
        self.hold_said = None;
        // A due wake's line (§2.1 rank 2) before the host's own step (rank 3, the upgrade's
        // notice): typed, it is the break's one line, and the host's step waits a break.
        if self.wake_first(screen, cfg, review)? {
            *since = None;
            return Ok(());
        }
        let mut line_gone = None;
        // A tape a build without the guarded door recorded replays the door it had.
        let stepped = if self.fx.replay_lacks_host_door("host.at_background_on") {
            host.at_background()
        } else {
            // A manager's wait the loop has not read yet (its first fold not reached since it
            // started) is read now, as a line's write reads it; a read that says nothing holds.
            let unread = self.managed_at_write()?;
            let mut point = self.break_point(self.fx.mono(), cfg, &screen.rows);
            point.held = point.held.or(unread);
            host.at_background_on(point)
        };
        if let Some((line, kind)) = stepped {
            review.note(&format!("HOST seq={} background {line}", screen.seq));
            self.host_turn(kind, screen.seq);
            self.hold_line(screen.seq, cfg, review);
            *since = None;
            line_gone = Some(Guard::HostLine);
        } else if let Some(step) = self.background_fact(screen, cfg, host.as_ref(), review)? {
            // The host's own fact at the break (the `disk-free@v1` ask's line), where the upgrade
            // typed nothing: what a turn of the session's that ends with its own background work
            // still running would otherwise never take.
            review.note(&format!("HOST seq={} background {}", screen.seq, step.line));
            if step.moved {
                self.turn_end.host_moved();
            }
            if let Some(kind) = step.typed {
                // Kept in a record of its own, as the occupancy door's line at such a break is
                // (`TurnEndState::fact_typed`): an act the agent took whose answer is still
                // awaited (a continuation, a land check) keeps its record and judges that answer,
                // and what the loop owes past it (a compact, a reset's resume) stays owed.
                match kind {
                    super::policy::turn_end::HostTurn::Fact => {
                        self.cruise_carry.host_typed();
                        self.turn_end.fact_typed();
                    }
                    kind => self.host_turn(kind, screen.seq),
                }
                self.hold_line(screen.seq, cfg, review);
                *since = None;
                line_gone = Some(Guard::HostLine);
            }
        }
        // The stuck fact's own line (the occupancy design's §2.1 rank 4), after the host's line
        // and before the nudges.
        if self.stuck_break(screen, (at, waited), line_gone.is_some(), cfg, review)? {
            *since = None;
            line_gone = Some(Guard::HostLine);
        }
        let wrote = self.occupancy_write(screen, line_gone, cfg, review)?;
        if wrote {
            *since = None;
        }
        // Cruise's carry where no line went: Cruise never parks at such a break.
        if self.cruise_carry(
            screen,
            (at, waited),
            line_gone.is_some() || wrote,
            cfg,
            review,
        )? {
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
        let agy_rotation_due = match reading.program {
            Program::Agy => {
                (limit_wall || self.limit.is_some())
                    && !self.fx.replay_lacks_host_door("host.upgrade_due")
                    && host.upgrade_due()
            }
            Program::Claude
            | Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Muse
            | Program::Generic => false,
        };
        let spent = (limit_wall || self.limit.is_some())
            && (self.turn_end.account_resume_owed()
                || self.limit_spent(Some(host.as_ref()), notice)
                || agy_rotation_due);
        // A RELAUNCH'S CARRY-ON RESTS AT THE QUESTION ITS RESUME DRAWS: a conversation that ended
        // on a question comes back on it. Nothing else of the host's steps there.
        let carry_only = (point.phase == Phase::Question || reading.phase == Phase::Question)
            && !self.fx.replay_lacks_host_door("host.carry_owed")
            && host.carry_owed();
        let rests = |p: &Phase| {
            *p == Phase::Idle
                || (spent && matches!(p, Phase::Limited { .. }))
                || (carry_only && *p == Phase::Question)
        };
        let guard = if !rests(&point.phase) || !rests(&reading.phase) {
            Some(Guard::NotIdle)
        } else if !reading.phase_authoritative {
            Some(Guard::NotAuthoritative)
        } else if reading.wall.is_some() && !(spent && limit_wall) {
            Some(Guard::Wall)
        } else if reading.wall.is_none() && self.turn_end.holds_left_retry() {
            // Retry wall still active off-screen.
            Some(Guard::Wall)
        } else if reading.prompt.is_some() {
            Some(Guard::Prompt)
        } else if self.limit.is_some() && !spent {
            Some(Guard::LimitEpisode)
        } else if !spent && self.policy_limit_holds() {
            // Turn-end policy or dialog limit still active.
            Some(Guard::LimitEpisode)
        } else if self.turn_end.act_in_flight() {
            Some(Guard::ActInFlight)
        } else if self.settling().is_some() {
            Some(Guard::WakeSettle)
        } else if self.credits_hold_here(&opts.policy, &point.screen.rows) {
            Some(Guard::Credits)
        } else if self.limit.is_none() && !limit_wall && self.standing_holds(host.as_ref()) {
            // Transcript standing limit still active.
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
            // The point the host's last step passed over is offered again only after a step of
            // the wait ([`Self::host_passed`]), never at once.
            if self.host_passed.is_some() && self.host_passed == Some(self.review_key(point)) {
                return Ok(false);
            }
            // No decision need read this composer at a point with nothing due, and its rows alone
            // tell neither a suggestion its cursor has since gone home on nor a draft whose caret is.
            // A hold's look has read it (`Self::draft_look`).
            if self.draft_held.is_none() {
                self.composer_for_guards(&reading, &point.screen);
            }
            // The status this step read first ([`Self::foreground_program`]) is the person's and
            // the driver's word, as at a break's own write.
            let guards = self.occupancy_guards(self.fx.mono(), &opts.policy, &point.screen.rows);
            let line_held = self.custody_holds_host(point.screen.seq, review);
            let at = IdlePoint {
                done_under_land: self.autopilot(opts) == Autopilot::Land
                    && self.turn_end.concluded(),
                held: guards.held_host_fact(),
                theirs: guards.theirs(),
                carry_only,
                replies_held: !self.fx.replay_lacks_host_door("host.carry_owed")
                    && self.replies_stopped,
                line_held,
            };
            let step = host.at_idle_on(at);
            if let Some(step) = &step {
                if !step.repeat {
                    review.note(&format!("HOST seq={} {}", point.screen.seq, step.line));
                }
                if step.moved {
                    self.turn_end.host_moved();
                }
                if host_step_word(&step.line).is_some_and(crate::harness::upgrade_drive::started) {
                    self.turn_end.harness_started();
                }
                if let Some(kind) = step.typed {
                    self.host_turn(kind, point.screen.seq);
                }
                if step.typed.is_some() || step.moved {
                    self.hold_point(&point.screen, true, Some(&opts.policy), review);
                }
            }
            let stood = !step.as_ref().is_some_and(|s| s.moved);
            if !stood {
                self.decision_owed = false;
                self.host_held = false;
            } else if step.is_some() || self.decision_owed {
                self.host_held = true;
            }
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

    /// The turn break `point` rests at, just decided, told to the host before it is asked
    /// whether it wants a step there ([`IdleHost::turn_break`]), so what the host finds of the
    /// session there (the `disk-free@v1` seat rule's finding, which every host's disk look reads)
    /// is made at every decided turn break, not only at one where the host owes a step. It is
    /// told once per decision: the wait's later looks at the same point find nothing new, and the
    /// host's finding reads the session's records. A point mid-turn (a box) is no turn break.
    /// What lasting withhold holds the session at it is told as well ([`Self::tell_lasting`]).
    fn tell_break(&mut self, opts: &SuperviseOpts, point: &Turn) {
        if let Some(host) = opts.idle_host.as_ref()
            && matches!(point.phase, Phase::Idle | Phase::Limited { .. })
        {
            host.turn_break(self.occupancy_theirs());
            let lasting = self.lasting_at(&point.screen, &opts.policy);
            self.tell_lasting(host.as_ref(), lasting);
        }
    }

    /// The lasting withhold that holds the session at the turn break `screen` draws, as the
    /// loop's own guards would read it there, without a read of its own: a wall on screen that is
    /// no usage limit ([`Guard::Wall`]; a usage limit's wall is the limit episode's,
    /// [`IdleHost::limit_wall`]), else the credits hold ([`Guard::Credits`]), else none.
    fn lasting_at(&self, screen: &Screen, cfg: &SupervisorConfig) -> Option<Lasting> {
        let mut reading = aterm_phase::read_at(
            self.program.as_deref(),
            &screen.rows,
            screen.cursor_index(),
            None,
        );
        self.wall_as_decided(&mut reading, screen);
        if let Some(wall) = reading.wall.as_ref().filter(|w| !w.kind.reads_limited()) {
            return Some(Lasting::Wall {
                wire: wall.kind.wire(),
            });
        }
        self.credits_hold_lifts_here(cfg, &screen.rows)
            .map(|lifts_unix| Lasting::Credits { lifts_unix })
    }

    /// Tell the host the lasting withhold the break just decided holds the session at (`hold`),
    /// once per change ([`IdleHost::lasting_hold`]). A replay of a tape whose recording build had
    /// no such door tells nothing, as it recorded.
    fn tell_lasting(&mut self, host: &dyn IdleHost, hold: Option<Lasting>) {
        if self.lasting_told == Some(hold) || self.fx.replay_lacks_host_door("host.lasting_hold") {
            return;
        }
        self.lasting_told = Some(hold);
        host.lasting_hold(hold);
    }

    /// The host's own fact ([`IdleHost::at_background_fact`]) at a settled break of the agent's
    /// own background work where the turn is over and only a shell or a monitor it left runs on
    /// ([`aterm_phase::turn_over_background`]): asked only where the host owes one
    /// ([`IdleHost::fact_owed`]), and offered where no draft stands in the composer and no hold
    /// is on the session, by the guards an idle point's fact is ([`IdlePoint`]), as the
    /// occupancy door's line at such a break. Those guards are judged on a `status` read made
    /// here, as every line at a break is ([`Self::write_guards`]), and as an idle point's step
    /// reads `status` first: the loop's last read was made at an idle point, which a session at
    /// background breaks for hours left long before, so a person who typed since (their own
    /// message, answered within seconds) would be inside their grace unseen. A read that says
    /// nothing holds the fact unoffered. A replay of a tape whose recording build had no such
    /// door offers nothing, as it recorded.
    fn background_fact(
        &mut self,
        screen: &Screen,
        cfg: Option<&SupervisorConfig>,
        host: &dyn IdleHost,
        review: &mut dyn Review,
    ) -> Result<Option<HostStep>, Fail> {
        let Some(cfg) = cfg else {
            return Ok(None);
        };
        if self.fx.replay_lacks_host_door("host.at_background_fact")
            || !aterm_phase::turn_over_background(&screen.rows)
            || !host.fact_owed()
        {
            return Ok(None);
        }
        let guards = match self.write_guards(screen, self.fx.mono(), cfg, host)? {
            Ok(guards) if guards.halted => Err(Guard::Halted),
            Ok(guards) if guards.draft => Err(Guard::Draft),
            read => read,
        };
        let guards = match guards {
            Ok(guards) => guards,
            Err(guard) => {
                review.note(&format!(
                    "HOST seq={} background fact held: {}",
                    screen.seq,
                    guard.word()
                ));
                return Ok(None);
            }
        };
        Ok(host.at_background_fact(IdlePoint {
            done_under_land: false,
            held: guards.held_host_fact(),
            theirs: guards.theirs(),
            ..IdlePoint::default()
        }))
    }

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
        let program = aterm_phase::identify(self.program.as_deref(), &[]).program();
        let ep_reset = self
            .limit
            .as_ref()
            .filter(|ep| notice.is_none_or(|reset| ep.reset_text == reset.unwrap_or("-")))
            .and_then(|ep| ep.reset_unix);
        let reset = match program {
            Program::Agy => {
                let now = self.now_unix();
                let host_reset: Option<i64> = (!self.fx.replay_lacks_host_door("host.limit_until"))
                    .then(|| host?.limit_until()?.try_into().ok())
                    .flatten()
                    .filter(|&u| u > now || self.agy_pool_blocked);
                match (host_reset, ep_reset) {
                    (Some(h), Some(e)) => Some(h.min(e)),
                    (h, e) => h.or(e),
                }
            }
            Program::Claude
            | Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Muse
            | Program::Generic => {
                ep_reset.or_else(|| host.and_then(|h| h.limit_until()?.try_into().ok()))
            }
        };
        reset.is_some_and(|reset| {
            self.now_unix() >= reset.saturating_add(i64::try_from(grace).unwrap_or(i64::MAX))
        })
    }
}

impl<C: Ctl> Session<'_, C> {
    fn withhold(&mut self, host: Option<&Arc<dyn IdleHost>>, guard: Guard) {
        let wait_ms = self.turn_end_due.as_ref().map(|due| {
            u64::try_from(due.at.saturating_duration_since(self.fx.mono()).as_millis())
                .unwrap_or(u64::MAX)
        });
        self.ctl.continuation_activity(guard.word(), wait_ms);
        let Some(host) = host.filter(|h| h.wants()) else {
            self.withheld = None;
            return;
        };
        if self.withheld != Some(guard) {
            self.withheld = Some(guard);
            host.withheld(guard);
        }
    }

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

/// Whether the composer may hold typed text, by its rows and the cursor alone: all but where
/// [`draft_read`] reads none. Its own line's echo ([`turn_end_loop::echo_of`]), a stray digit and
/// the probe's `/usage` are told by their words, which no grey suggestion is.
fn typed_draft_in(
    composer: Option<&(usize, Vec<String>)>,
    placeholder: bool,
    screen: &Screen,
) -> bool {
    draft_read(composer, placeholder, screen) != Some(false)
}

/// WHAT THE COMPOSER'S ROWS AND THE CURSOR ALONE SAY OF ITS TEXT: typed (`Some(true)`), none
/// (`Some(false)`), or nothing (`None`). Any text where the program draws no grey suggestion is typed.
/// Where it does (`placeholder`), the cursor tells, on the composer's rows. Past column 2 on the
/// caret's row it stands after typed text, and on a later row in a draft of more than one row (a
/// Shift+Enter). At column 2 of the caret's row, on a lone first line, it stands on a suggestion's
/// first glyph; on text of more than one row it says nothing, which keeps grey text on more rows than
/// one, never measured (Claude Code 2.1.318 cuts a suggestion to one row and Muse Code 1.4.14 clips
/// its tip to one, `docs/measured/composer-cells-2026-11-03.md`), from being called either. Off the
/// composer's rows, or off the rows read, it says nothing: a program that places
/// its cursor last after drawing its rows (Muse Code's ~2 KB bottom pane ending `ESC[48;3H`,
/// `docs/measured/half-drawn-frame-2026-10-22.md`, and Claude Code's unbracketed launch frame) is read
/// there between the two, which is no person's typing (`fabric_world`'s `174640090a`). None of this is
/// a decision's read or a host's step, which ask the glyph's cell wherever the cursor stands
/// (`Session::composer_typed`): a frame read before its program placed the cursor can leave it past a
/// suggestion or under one too.
fn draft_read(
    composer: Option<&(usize, Vec<String>)>,
    placeholder: bool,
    screen: &Screen,
) -> Option<bool> {
    let Some((caret, lines)) = composer else {
        return Some(false);
    };
    let later = lines.iter().skip(1).any(|l| !l.is_empty());
    if lines.first().is_none_or(|l| l.is_empty()) {
        return Some(later);
    }
    if !placeholder {
        return Some(true);
    }
    let row = screen
        .cursor_index()
        .filter(|row| (*caret..caret + lines.len()).contains(row))?;
    if row != *caret || screen.cursor_col != COMPOSER_COL {
        return Some(true);
    }
    (!later).then_some(false)
}

/// [`cursor_home`] of `screen`'s cursor.
fn screen_cursor_home(caret: usize, lines: &[String], screen: &Screen) -> bool {
    let cursor = screen.cursor_index().map(|row| (row, screen.cursor_col));
    cursor_home(cursor, caret, lines)
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
        Phase::Busy => {
            let title = turn.screen.title.as_deref();
            match busy_rule(reader, rows, title) {
                Some(b) => out.push_str(&format!("reason {b}\n")),
                None if turn.screen.rows.is_empty() => {
                    out.push_str("reason no screen: no read answered before the timeout\n")
                }
                None => {
                    out.push_str("reason whole screen, no composer frame: output still changing\n")
                }
            }
            // A turn over under the agent's own background work: the phase it rests at, which
            // the server publishes as `agent=` and the session log closes the turn at. A turn
            // the title runs rests at none ([`ScreenReader::background_rest_seen`]).
            if let Some(rest) = reader.background_rest_seen(rows, reader.runs(title)) {
                out.push_str(&format!("rest {}\n", rest.name()));
            }
        }
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
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => reader.prompt(rows).map(|p| {
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
        }),
    }
}

fn esc_line(effect: Option<CancelEffect>) -> Option<&'static str> {
    match effect? {
        CancelEffect::Reject | CancelEffect::Back => Some("cancel esc"),
        CancelEffect::Exit => Some("exit esc"),
        CancelEffect::Interrupt => Some("interrupt esc"),
    }
}

/// Why `rows` read busy, beside what `title` (the window title read with them) says of the run:
/// a turn only the title runs is `title: running`.
fn busy_rule(reader: &dyn ScreenReader, rows: &[String], title: Option<&str>) -> Option<String> {
    let runs = reader.runs(title);
    match reader.program() {
        Program::Claude => busy_seen(rows, runs).map(|b| b.to_string()),
        Program::Generic => busy_signal(rows).map(|b| b.to_string()),
        Program::Codex | Program::Grok | Program::Kimi | Program::Agy | Program::Muse => {
            let status_row = reader.busy_guard().is_some_and(|guard| {
                aterm_observe::row_matcher(&guard).is_ok_and(|m| rows.iter().any(|r| m.matches(r)))
            });
            if status_row {
                return Some("status row: esc to interrupt".to_string());
            }
            if reader.phase_seen(rows, runs) != Phase::Busy {
                return None;
            }
            if reader.phase(rows) != Phase::Busy {
                return Some("title: running".to_string());
            }
            Some(match reader.background_wait_seen(rows, runs) {
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

/// The `EVENT` line's words for `turn`: a box's kind, its `classify=` verdict and command, or the
/// last line the agent said. `classify=` is the read-only rule on the screen's readings alone. A
/// Kimi Code box passes its command's control characters through raw, so its drawn command may
/// not be the one Kimi runs (D23 of `docs/DESIGN-harness-repos-2026-10-24.md`); only the
/// presence verdict also reads Kimi's own record (`docs/DESIGN-kimi-wire-verdict-2026-11-07.md`),
/// and nothing presses on Kimi.
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
            Program::Codex | Program::Grok | Program::Kimi | Program::Agy | Program::Muse => {
                or_dash(&clip_tail(&said_line(reader, rows, width)))
            }
            Program::Claude => {
                or_dash(&clip_tail(&last_said_line(rows, width).unwrap_or_default()))
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
            let end = own_turn_end(reader, rows)
                .unwrap_or_else(|| reader.transcript_end(rows))
                .min(rows.len());
            let at = (0..end).rev().find(|&i| rows[i].trim().ends_with(last))?;
            Some(at + 1 - aterm_phase::line_start(rows, at, w))
        })
        .unwrap_or(1)
        .clamp(1, lines.len());
    lines[lines.len() - wrapped..].join(" ")
}

/// The session's submit count on `screen` and the mark of the turn's rows above it ([`point_upto`],
/// the point key's own cut, so a draft typed after a submission never moves it), each row as the
/// screen's transcript reads it ([`ScreenReader::transcript`]: beside Claude Code's side panel,
/// its words up to the panel's rule, so the panel's text moves no mark).
fn submit_mark(reader: &dyn ScreenReader, screen: &Screen) -> Option<(u64, u64)> {
    let rows = &screen.rows;
    let upto = point_upto(reader, rows);
    screen.submits.map(|n| {
        let transcript = reader.transcript(rows);
        (
            n,
            super::policy::turn_end::screen_mark(&transcript.rows()[..upto.min(rows.len())]),
        )
    })
}

/// Whether a submission counted since `then` ([`submit_mark`]) drew its turn on `now`: the count up
/// and the turn's rows moved.
fn submitted_over(then: Option<(u64, u64)>, reader: &dyn ScreenReader, now: &Screen) -> bool {
    matches!(
        (then, submit_mark(reader, now)),
        (Some((n0, m0)), Some((n, m))) if n > n0 && m != m0
    )
}

/// WHERE A POINT'S TURN ROWS END (ATERM-KI-1101-2): above the program's own composer where it draws
/// one ([`own_turn_end`]), else under the last said row ([`last_said_index`]), Claude Code's rule.
fn point_upto(reader: &dyn ScreenReader, rows: &[String]) -> usize {
    own_turn_end(reader, rows).unwrap_or_else(|| last_said_index(rows).map_or(0, |k| k + 1))
}

/// WHERE A PROGRAM'S OWN TURN ENDS ON ITS SCREEN, so no draft typed, pasted or cleared in its
/// composer, and nothing its footer draws, moves a point ([`point_key`], [`submit_mark`]): agy's
/// transcript, which ends at a running turn's spinner row or above its composer's frame, its notice
/// cards and its feedback survey ([`aterm_phase::agy::transcript_end`]), Kimi Code's turn above
/// its composer's top row ([`aterm_phase::kimi::composer_rows`]), and Grok Build's above its
/// composer's box ([`aterm_phase::grok::composer_rows`]: its draft's words are never read, a
/// suggestion the empty composer offers being one a row cannot tell from a draft). `None` where
/// the point keeps Claude Code's rule: Claude Code's and Muse Code's frames are its own, and it
/// stops above them and the status rows over them, which their frame's top would not; Codex has
/// no composer its reader finds; and Kimi Code's or Grok Build's screen with no composer drawn.
fn own_turn_end(reader: &dyn ScreenReader, rows: &[String]) -> Option<usize> {
    match reader.program() {
        Program::Agy => Some(reader.transcript_end(rows).min(rows.len())),
        Program::Kimi => aterm_phase::kimi::composer_rows(rows).map(|(top, _, _)| top),
        Program::Grok => aterm_phase::grok::composer_rows(rows).map(|(top, _, _)| top),
        Program::Claude | Program::Muse | Program::Codex | Program::Generic => None,
    }
}

/// THE MODEL A PROGRAM'S FOOTER NAMES, which a limit met under it binds to: agy's
/// ([`aterm_phase::agy::footer_model`], [`Seat::of`](super::policy::turn_end::Seat::of)'s model wherever the footer names one, and no
/// more of the seat). `None` for every other program, and for a frame whose footer names none: agy
/// at a pane under about 40 columns draws `? for shortcuts` alone at idle and the model only beside
/// a draft (`aterm_phase::agy::fixtures::lab::NARROW_36_END`, `NARROW_36_DRAFT`). The launch card,
/// which [`Seat::of`](super::policy::turn_end::Seat::of) falls back on, names the model the session started on, so after a `/model`
/// at such a pane a draft typed and cleared would move it each time.
fn named_model(reader: &dyn ScreenReader, rows: &[String]) -> Option<String> {
    match reader.program() {
        Program::Agy => aterm_phase::agy::footer_model(rows),
        Program::Claude
        | Program::Muse
        | Program::Kimi
        | Program::Codex
        | Program::Grok
        | Program::Generic => None,
    }
}

/// WHETHER THE MODEL MOVED UNDER ONE POINT ([`named_model`]): both reads name one, and another, as
/// [`Seat::moved`](super::policy::turn_end::Seat::moved) counts, so a read naming none moves nothing. `/model` under a standing limit
/// wall draws nothing above agy's composer, and the point it leaves is weighed again
/// ([`Session::point_model_moved`], which weighs it only there); the key itself ([`point_key`])
/// cannot carry the model, which a narrow pane draws only beside a draft.
fn model_moved(then: Option<&str>, now: Option<&str>) -> bool {
    matches!((then, now), (Some(a), Some(b)) if a != b)
}

/// A POINT'S KEY: its phase, the agent's last words, the status row and the letters of the turn's
/// last rows ([`push_words_above`]), each row of those as the screen's transcript reads it
/// ([`ScreenReader::transcript`]: beside Claude Code's side panel, its words up to the panel's
/// rule, so the panel's text moves no point); a box's own key where one stands ([`box_key`]).
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
        None => point_upto(reader, rows),
    };
    push_words_above(&mut key, &reader.transcript(rows).rows()[..upto]);
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

/// Formats prompt details and read-only classification (`verdict`) for [`render_phase`].
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
#[path = "capture_loop.rs"]
mod capture_loop;
#[cfg(test)]
pub(crate) use approval_loop::reask_pause;
/// Re-exports whole-box approval decision evaluation for tape explanation.
pub(crate) use approval_loop::whole_box_decision;
#[path = "box_head.rs"]
mod box_head;
/// Re-exports approval box header search helpers for tape replay.
pub(crate) use box_head::{SEARCH as HEAD_SEARCH, head_row, head_rows, joined_box, same_box};
#[path = "claim.rs"]
mod claim;
#[path = "duty.rs"]
pub(crate) mod duty;
/// Re-exports the write-verb classifier for harness tape accounting.
pub(crate) use claim::is_write;
pub use claim::{CLAIM_HELD, CLAIM_RENEW};
#[path = "cruise_carry_loop.rs"]
mod cruise_carry_loop;
#[path = "escalate.rs"]
mod escalate;
#[path = "host_door.rs"]
pub(crate) mod host_door;
pub use host_door::IdleHost;
#[path = "facts_loop.rs"]
mod facts_loop;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "log_wake.rs"]
mod log_wake;
#[path = "observe.rs"]
mod observe;
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
pub use task_mark::{
    COULD_NOT_FINISH_WHY, HELD_HEAD, REPEATED_WHY, STARTUP_HEAD, USAGE_LIMIT_WHY,
    deadline_attention, mark_task_exited,
};
#[path = "turn_end_loop.rs"]
mod turn_end_loop;
#[path = "wall_confirm.rs"]
pub(crate) mod wall_confirm;
pub(super) use wall_confirm::words_kind;
#[path = "wall_memory.rs"]
mod wall_memory;
#[path = "woken_turn.rs"]
mod woken_turn;
/// The dark wake's cap, which a line held in custody waits to at most
/// ([`super::custody::CUSTODY_CAP`]).
pub(crate) use turn_end_loop::DARK_WAKE_CAP;
/// Re-exports composer cursor-row guard constructors.
pub(crate) use turn_end_loop::composer_guard;
pub use turn_end_loop::standing_rules;
/// The loop's two wake gates, for the outage replay (`policy::turn_end`'s `sysmsg_replay` tests).
#[cfg(test)]
pub(crate) use turn_end_loop::{DARK_WAKE_STEP, SleepWindow, WAKE_SETTLE, dark_wake_holds};
/// Re-exports turn-text folding and standing-rules helpers.
#[cfg(test)]
pub(crate) use turn_end_loop::{one_turn, standing_rules_in};

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "run_frame_tests.rs"]
mod frame_tests;
